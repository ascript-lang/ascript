# VM Plan V5 — Closures & upvalues: cells, capture-by-value, GET/SET_UPVALUE, CLOSE_UPVALUE

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development.

**Goal:** Implement AScript's JS-like closure capture on the VM, resolver-driven: captured-and-mutated locals become shared `Rc<RefCell<Value>>` **cells** (mutation visible through all capturing closures); captured-but-never-reassigned locals are **captured by value** (Luau-style, no cell). `CLOSURE` wires each upvalue per the resolver's plan; `GET/SET_UPVALUE` access them; `CLOSE_UPVALUE` finalizes a cell when its slot leaves scope.

**Architecture:** The resolver already computes per-frame `cell_slots: Vec<u32>` (slots needing a cell) and `upvalues: Vec<UpvalueDescriptor>` (ParentLocal/ParentUpvalue). The VM represents a cell slot as the slot holding a `Value` that is logically a cell; implement cells as `Rc<RefCell<Value>>` stored OUT-OF-BAND (a parallel per-frame `Vec<Option<Rc<RefCell<Value>>>>` indexed by slot) so non-cell slots stay plain `Value` in the stack (fast path). `Closure.upvalues: Vec<Rc<RefCell<Value>>>` (cells) for mutable captures; for by-value captures, store the captured `Value` in a cell too OR a separate by-value vec — choose the representation that keeps `GET_UPVALUE` a single index. **Depends on V4.**

---

## Ground truth
- Resolver: `FrameInfo.cell_slots` = slots that are `captured && mutated` (need a `RefCell` cell); `FrameInfo.upvalues` = the closure's capture plan (`ParentLocal(slot)` = capture the parent frame's slot; `ParentUpvalue(idx)` = capture the parent closure's upvalue idx). `Binding.captured`/`.mutated` drive `cell_slots` (already implemented + tested, C2 added `bindings`).
- Tree-walker capture semantics: closures capture the `Environment` by reference; reassigning a captured `let` is visible to all closures (JS-like). The cell approach reproduces this; the by-value optimization is SOUND only when the resolver proves no reassignment after capture (`!mutated` for captured vars) — match that exactly so behavior is identical.
- Capture-by-value vs cell is an OPTIMIZATION that must be behavior-identical: a never-reassigned captured var has the same observable value whether shared or copied. The differential gate enforces no divergence.

---

## Tasks
- [ ] **T1 — cell-slot representation.** Extend `CallFrame`/`Fiber` so a frame can mark certain slots as cells. On frame entry, for each slot in `proto.chunk.cell_slots`, allocate `Rc<RefCell<Value>>` (init `Nil`) in a per-frame `cells: Vec<Option<Rc<RefCell<Value>>>>` sized to slot_count. `GET_LOCAL`/`SET_LOCAL` for a cell slot read/write `*cell.borrow()`; for a non-cell slot, the plain stack slot (fast path unchanged). The compiler already knows (from the resolver) which slots are cells — emit `GET_LOCAL`/`SET_LOCAL` unchanged; the VM consults `cell_slots` (or the compiler emits distinct `GET_LOCAL_CELL`/`SET_LOCAL_CELL` opcodes for a branch-free fast path — prefer distinct opcodes to avoid a per-access check). Decide and document; distinct opcodes recommended. Tests: a mutated captured var. Commit.
- [ ] **T2 — CLOSURE wiring.** `CLOSURE proto-idx`: build `Closure { proto, upvalues }` by reading the proto's `upvalues` descriptors: `ParentLocal(slot)` → clone the parent frame's cell `Rc` (for a cell slot) OR capture-by-value (clone the `Value`, wrap in a fresh `Rc<RefCell<Value>>` so the upvalue vec is uniform) when the descriptor marks by-value; `ParentUpvalue(idx)` → clone `parent_closure.upvalues[idx]`. The resolver's descriptor must distinguish cell vs by-value capture — if `UpvalueDescriptor` lacks that bit, add it (resolver change: mark each upvalue as `by_value: bool` derived from the source binding's `mutated`). Keep value.rs/the legacy interp untouched. Tests: counter closure (mutable capture shared), and an immutable capture. Commit.
- [ ] **T3 — GET/SET_UPVALUE.** `GET_UPVALUE idx` → push `*closure.upvalues[idx].borrow()`; `SET_UPVALUE idx` → `*borrow_mut() = value` (only valid for mutable/cell captures; by-value captures are never SET — the resolver guarantees it). Compiler emits these for `Resolution::Upvalue(idx)` uses/assignments. Tests: nested closures reading/writing an enclosing var; the classic `makeCounter` returning increment/get closures sharing state. Commit.
- [ ] **T4 — CLOSE_UPVALUE.** When a cell slot leaves scope (block/loop iteration end) and may have been captured, emit `CLOSE_UPVALUE slot` to detach it (so the next iteration's binding is a FRESH cell — critical for `for`-loop closures capturing the loop var, JS `let` semantics). Confirm the tree-walker's per-iteration binding freshness and mirror it: each loop iteration that declares a captured `let` gets a new cell. Compiler emits `CLOSE_UPVALUE` (or re-allocates the cell) at iteration boundaries per the resolver. Tests: `for`/`while` body that captures the loop var into an array of closures → each closure sees its own iteration's value (match tree-walker exactly). Commit.
- [ ] **T5 — widen differential gate.** Add closure-heavy examples (`functions.as` closures, `generators.as` excluded until V8, parts of `stdlib.as`). Byte-identical. Full suite + clippy both configs. Commit.

## Done criteria (V5)
- [ ] Mutable captures shared via cells; immutable captures by value; both behavior-identical to the tree-walker (incl. per-iteration loop-var freshness).
- [ ] `CLOSURE`/`GET_UPVALUE`/`SET_UPVALUE`/`CLOSE_UPVALUE` correct; resolver upvalue plan consumed.
- [ ] Differential gate widened; `cargo test` green; clippy clean both configs.

**Next:** V6 — error model: `?` (PROPAGATE), `!` (UNWRAP), `recover` (native over `call_value`), VM frame-stack unwinding, Tier-1/Tier-2 + diagnostics parity.
