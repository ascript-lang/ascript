# VM Plan V11 — Shapes, inline caches, PEP-659 adaptive specialization (performance layer)

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development.
> **Built AFTER the base VM reaches whole-corpus parity (V10).** Semantics-preserving (guards + deopt). Gated by a three-way differential (generic-VM == specialized-VM == tree-walker) + the `std/bench` perf gate. A kill switch (`--no-specialize`) runs pure-generic.

**Goal:** The dynamic-language performance layer, in-spec (not deferred): object/instance **shapes** (`shape_id` + per-VM `ShapeRegistry`), **polymorphic inline caches** at `GET_PROP`/`SET_PROP`/`CALL_METHOD` (mono→poly≤4→megamorphic), and **PEP-659 adaptive specialization** for arithmetic (`ADD_NUMBER`/`ADD_DECIMAL`/`CONCAT_STR`) and `GET_GLOBAL_CACHED`. Target ≥2× on compute-bound code, no regression on any benchmark.

**Architecture:** Add a `shape_id` to `Object`/`Instance` (the ONE remaining value-model change). A per-VM `ShapeRegistry` assigns ids to key-layouts via a transition tree. Since `Object` is already an insertion-ordered `IndexMap` with O(1) `get_index_of`, the IC caches `(shape_id, index)` and the fast path is `obj.shape_id == cached → values.get_index(cached_index)` — no value-model rewrite. ICs live in a parallel `Vec<InlineCache>` on the `Chunk` behind `Cell`/`RefCell` (`!Send`, no atomics). Adaptive opcodes use a warmup counter + quickening (rewrite the opcode byte in place) + deopt on guard miss. **Depends on V10.**

---

## Ground truth
- `Object(Rc<RefCell<IndexMap<String,Value>>>)`, `Instance(Rc<RefCell<Instance>>)` with `fields: IndexMap<String,Value>` — `IndexMap` gives stable indices + O(1) `get_index_of`/`get_index`. Adding/removing a key changes the layout → must transition shape. Reassigning an existing key keeps the layout/shape (IC stays valid). A class gives instances a base shape from declared fields.
- `--no-specialize` kill switch: pure-generic execution (no IC fast paths, no adaptive opcodes) for the three-way differential.
- AScript-semantic guards: schema-value receivers and `?.` nil-receivers NEVER take a cached fast path (guard excludes → generic).

---

## Tasks
- [ ] **T1 — `shape_id` + ShapeRegistry.** Add `shape_id: Cell<u32>` to the `Object`/`Instance` representation (minimal: a field on the `RefCell`'d inner, or a parallel cell). Implement a per-VM `ShapeRegistry { transitions: HashMap<(u32, Box<str>), u32>, ... }` assigning ids to key-layouts via a transition tree (empty shape → add key → child shape). Objects/instances get a shape on creation (`NEW_OBJECT`, instance init, class base shape). Key add/remove transitions; reassign keeps. Tests: shape identity for same-layout objects; transition on key add; class instances share a base shape. Commit `feat(vm): shapes + per-VM ShapeRegistry`.
- [ ] **T2 — polymorphic IC at GET_PROP/SET_PROP.** Add an `InlineCache` parallel array on `Chunk` (one slot per specializable op, indexed by the op's reserved `u16` IC field). `GET_PROP`: mono cache `(shape_id, index)` → fast `values.get_index(index)`; on miss, fall to generic lookup + record up to 4 shapes (polymorphic cascade); >4 → megamorphic (generic always). `SET_PROP` similar (existing-key set keeps shape). Guard: schema-value receiver → generic. Tests: monomorphic hit, polymorphic (≤4 shapes), megamorphic fallback, mutation correctness (add-key invalidates), schema-receiver bypass. Commit.
- [ ] **T3 — CALL_METHOD IC.** Cache method lookup by `(class shape/id, method)` → the resolved method/proto; deopt on shape change. Tests: monomorphic method dispatch hit; polymorphic; subclass override correctness. Commit.
- [ ] **T4 — PEP-659 adaptive arithmetic + GET_GLOBAL_CACHED.** Adaptive families: `ADD` warms up (counter in the IC slot); after N hits with Number operands, quicken to `ADD_NUMBER` (rewrite the opcode byte in place); on a guard miss (non-number operand) deopt back to generic `ADD` (and re-adapt). Same for `ADD_DECIMAL`, `CONCAT_STR`, and `GET_GLOBAL`→`GET_GLOBAL_CACHED` (cache the global slot/value, guard on global table version). Tests: a hot numeric loop quickens; a polymorphic add deopts and stays correct; global cache invalidation. Commit.
- [ ] **T5 — `--no-specialize` kill switch + THREE-WAY differential.** Add a VM flag (and a `--no-specialize` CLI/test hook) that disables all IC fast paths + adaptive opcodes (pure generic). Add a three-way differential test: for the whole corpus + test suite, assert `generic-VM stdout == specialized-VM stdout == tree-walker stdout`, byte-identical. A guard bug surfaces instantly. Commit.
- [ ] **T6 — `std/bench` + perf gate.** Create `src/stdlib/bench.rs` (`std/bench`: timing harness — `bench(name, fn)`, iterations, ns/op; the survey confirms it does NOT exist yet) + a benchmark suite (deep recursion, tight loops, property access, string building, method dispatch). Gate: specialized-VM achieves a REAL speed-up (target ≥2× on compute-bound) vs the tree-walker, with NO regression on any benchmark. Record numbers in the plan/commit. If short of target, this is where to tune ICs/specialization BEFORE cutover. Commit.
- [ ] **T7 — full suite + clippy both configs.** Commit.

## Done criteria (V11)
- [ ] Shapes + polymorphic ICs (GET/SET_PROP, CALL_METHOD) + PEP-659 adaptive arithmetic/globals, all semantics-preserving (guards + deopt).
- [ ] Three-way differential green (generic == specialized == tree-walker, byte-identical, whole corpus + suite).
- [ ] `std/bench` perf gate: ≥2× compute-bound target, no regression on any benchmark.
- [ ] `--no-specialize` kill switch works; `cargo test` green; clippy clean both configs.

**Next:** V12 — `.aso` bytecode persistence + import (Chunk serialization with version header, verifier on load, `ascript build`, `import` resolving `.aso`). Specialization is runtime-only and does NOT serialize (the `.aso` holds the generic chunk).
