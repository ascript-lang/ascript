# VM Plan V2 — Sync core: literals/strings, print+output, locals, globals, full arithmetic/compare/logic, statements

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development.

**Goal:** Make the VM execute the full **synchronous, single-frame** language: every literal (incl. strings/templates/decimals), `print` with output capture, `let`/local read+write (`GET/SET_LOCAL`), global/builtin reads (`GET_GLOBAL`), assignment, all arithmetic/comparison/equality, short-circuit `&&`/`||`/`??` (via jumps), `ExprStmt`, `Block` scoping, and the `;`-separated statement list — all byte-identical to the tree-walker. Widen the differential gate to whole-program stdout over the sync subset of `examples/`.

**Architecture:** Extend `src/compile/` (statement + expression emit arms driven by the resolver's `Local`/`Global` resolutions) and `src/vm/run.rs` (exec arms). The VM gains an output path: route `print` through the borrowed `Interp`'s `push_output` (reusing `OutputSink::Capture`/`Live`) so VM and tree-walker share the exact sink. No new `Value` variant.

**Spec:** bytecode-vm-design §Compiler, §Instruction set (stack/consts, locals, globals, arithmetic/logic). **Depends on V1.**

---

## Surveyed ground truth
- `print`/builtins are `Value::Builtin(name)` dispatched by `Interp::call_builtin(name,args,span)` (`src/interp.rs:2815`); `print` writes via `push_output` (`src/interp.rs:463`). The VM reuses this — a `CALL` to a `Builtin` delegates to `call_builtin` (full `CALL` is V4; this plan special-cases the builtin call shape needed for `print` and `len`/`type` via `GET_GLOBAL`+`CALL`).
- Resolver: a `NameRef` use → `result.uses[range]`: `Local(slot)`→`GET_LOCAL slot`; `Global(name)`→`GET_GLOBAL <name-const>`; `Upvalue` is V5. A `let` binding's slot comes from the enclosing frame's `Binding.slot` (look up via `bindings`/`uses`).
- Short-circuit: the tree-walker evaluates `a && b` as `if a then b else a` (returns operands, JS-like) — confirm exact semantics in `src/interp.rs` BinOp::And/Or/Coalesce and mirror via `JUMP_IF_FALSE`/`JUMP_IF_TRUE`/`DUP`/`POP`.
- String `+`, numeric coercions, `Decimal` arithmetic, `==` deep vs identity, comparison of mixed types — read the tree-walker's `eval_binary`/`apply_op` and replicate EXACTLY (the differential gate enforces it).

---

## File Structure
- `src/compile/mod.rs` — statement emit (`compile_stmt`), expression emit (`compile_expr`) full sync coverage; a `Compiler` struct carrying the `ResolveResult` + current `Chunk` builder + scope/slot bookkeeping from the resolver.
- `src/vm/run.rs` — exec arms for all sync ops.
- `src/lib.rs` — `vm_run_source(src) -> Result<(String, Option<i32>), AsError>` (capture output like `run_source_exit`) for the harness.
- `tests/vm_differential.rs` — widen to stdout comparison over a curated sync example set.

---

## Task 1: Output path + `print` + the run-to-output entry
- [ ] `Vm` holds `interp: Rc<Interp>`; the VM's top-level driver uses `Interp::new()` (Capture) for tests / `new_live()` for real runs. Add `vm_run_source(src)` mirroring `run_source_exit`: compile → run top-level closure → return `(interp.output(), exit_code)`. A `print(x)` lowers to `GET_GLOBAL "print"` + arg + `CALL 1`; the `CALL` arm, when the callee is `Value::Builtin("print")`, calls `interp.call_builtin("print", &args, span).await`. Test: `vm_run_source("print(1+2)")` → `"3\n"` (match tree-walker exactly, incl. trailing newline + number formatting).
- [ ] Gate + commit `feat(vm): output path + print via shared sink`.

## Task 2: Full literals + strings + templates
- [ ] `Literal` for number/string/bool/nil/decimal → const pool (parse token text once: numbers incl. hex/bin/scientific exactly as the lexer/tree-walker do; string unescape; decimal). `TemplateExpr` → compile each part (string chunk → CONST; `${expr}` → compile expr) then `TEMPLATE n` op: pop n parts, coerce each to string with the SAME coercion the tree-walker uses, concat, push. Test arithmetic-free literals + `` `a${1+2}b` `` against tree-walker.
- [ ] Gate + commit `feat(vm): literals, strings, template interpolation`.

## Task 3: Locals — `let`, reads, assignment, blocks
- [ ] `LetStmt` (incl. `const`): compile initializer, `SET_LOCAL slot` (slot from resolver `Binding`). `NameRef`→`Local(slot)` → `GET_LOCAL`. `AssignExpr` to a local → compile value, `SET_LOCAL`, leave value on stack (assignment is an expression). `Block` → compile statements; block-scoped locals already have distinct slots from the resolver (no runtime scope push needed — slots are frame-flat). `ExprStmt` → compile expr, `POP` (unless it's the trailing program value). Destructuring `let` is V10. Tests: `let x=1\nlet y=x+1\nprint(y)` → `2`; shadowing in a block; reassignment.
- [ ] Gate + commit `feat(vm): locals (let/const/assign) + block scoping`.

## Task 4: Globals + builtins
- [ ] `Global(name)` → `GET_GLOBAL <name-const>`: the exec arm resolves via the borrowed `Interp`'s global env / `BUILTIN_NAMES` → push `Value::Builtin(name)` for builtins, or the global binding. `SET_GLOBAL` for top-level assignment if the tree-walker allows it (confirm). Stdlib module member access (`math.abs`) is a `GET_PROP`/member on an imported namespace — defer the qualified-call detail to V4's `CALL` (this plan: bare builtins `print`/`len`/`type`/`range`/`assert`). Test `print(len([1,2,3]))`→`3`, `print(type(1))`.
- [ ] Gate + commit `feat(vm): globals + bare builtins`.

## Task 5: Complete arithmetic / comparison / equality / unary
- [ ] Implement exec arms for ADD/SUB/MUL/DIV/MOD/POW/NEG, EQ/NE/LT/LE/GT/GE, NOT — each delegating to a shared helper that mirrors the tree-walker's `apply_binop`/`apply_unop` (numbers `f64`, `Decimal` promotion rules, string `+` concat, `==` semantics for containers = identity per value.rs, comparison type errors → span panic). Reuse the tree-walker's helper functions directly if they're callable as free fns; else replicate and cover with differential tests. Test each operator + the error cases (e.g. `1 < "x"` panic message identical).
- [ ] Gate + commit `feat(vm): full arithmetic/comparison/equality semantics`.

## Task 6: Short-circuit `&&` / `||` / `??`
- [ ] `a && b`: compile a; `DUP`; `JUMP_IF_FALSE end`; `POP`; compile b; `end:`. `a || b`: `DUP`; `JUMP_IF_TRUE end`; `POP`; compile b. `a ?? b` (Coalesce): `DUP`; `JUMP_IF_NOT_NIL end` (add a small `JUMP_IF_NIL`/test, or `DUP`+is-nil+`JUMP_IF_FALSE`)`; POP; compile b`. Match the tree-walker's exact return-the-operand semantics (truthiness rules!). Verify AScript truthiness (what's falsy: `false`, `nil`, others?) from the tree-walker and replicate in `JUMP_IF_FALSE`'s truthiness test. Tests: `false && x` doesn't evaluate `x` (side-effect probe via `print`), `nil ?? 5`→`5`, `0 || 7` (depends on truthiness — match tree-walker).
- [ ] Gate + commit `feat(vm): short-circuit && || ?? via jumps`.

## Task 7: Widen the differential gate to sync examples
- [ ] In `tests/vm_differential.rs`, add a corpus runner: for a curated list of `examples/*.as` that use ONLY sync features implemented so far (e.g. `hello.as`, `numbers.as`, `strings.as`, `ranges.as` if sync, parts of `core_types.as`), run both tree-walker (`run_source_exit`) and `vm_run_source`, assert byte-identical stdout + exit code. Maintain an explicit allow-list (constructs not yet supported are excluded with a comment); the list GROWS each slice until V10 enables the whole corpus. NEVER weaken the byte-identical assertion.
- [ ] Full suite + clippy both configs. Commit `test(vm): differential stdout gate over sync example subset`.

## Done criteria (V2)
- [ ] VM runs all sync single-frame programs identically to the tree-walker (literals, strings, templates, locals, globals, bare builtins incl. `print`, all operators, short-circuit).
- [ ] Output goes through the shared `OutputSink`; byte-identical stdout on the sync example subset.
- [ ] `cargo test` green; clippy clean both configs; binary still tree-walker-driven.

**Next:** V3 — control flow (`if`/`while`/`for`/range/`for..of` sync, `break`/`continue`, jump patching, nested blocks).
