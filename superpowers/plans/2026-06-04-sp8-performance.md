# SP8 — Performance: global-access fast path + capture-by-value — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Recover the geomean regression (`~2.92×` V11-T6 → `~2.69×` measured on this branch)
introduced when module top-level `let/const/fn/class` became late-bound USER-GLOBALS, and land the
deferred #136 capture-by-value closure optimization. Both are pure speed changes: the whole-corpus
three-way differential (tree-walker == specialized-VM == generic-VM) must stay **byte-identical**.

**Architecture:** Two independent phases (A = global-access fast path, B = capture-by-value) plus a
closing holistic+measurement phase (C). Each phase is TDD, ends green on both feature configs +
clippy + the whole-corpus three-way differential, and ends with a perf-measurement gate before the
next. The tree-walker and the generic VM are the byte-identical oracles; never weaken them.

**Tech Stack:** Rust. CST front-end → resolver (`src/syntax/resolve`) → compiler (`src/compile/mod.rs`)
→ `Chunk` → VM (`src/vm/*`, run with `specialize` true/false). Legacy front-end → tree-walker
(`src/interp.rs`, untouched). gcmodule GC. `.aso` versioned bytecode (`src/vm/aso.rs`).

**Spec:** `docs/superpowers/specs/2026-06-04-sp8-performance-design.md`.

**Branch:** `feat/sp1-engine-parity` (current) — confirm with the owner whether SP8 lands on a fresh
`feat/sp8-performance` branch off `main` or stacks here; this plan is branch-agnostic.

---

## Conventions for every task

- **Differential test harness:** `tests/vm_differential.rs` compares `ascript::vm_run_source(src)`
  (specialized VM), `ascript::vm_run_source_generic(src)` (generic VM), and
  `ascript::run_source_exit(src)` (tree-walker). "Byte-identical" = identical stdout + exit on all
  three. Read a few neighboring cases first and match the file's actual helper name/pattern.
- **Perf harness:** `cargo test --release --test vm_bench -- --ignored --nocapture` prints
  per-bench tw/gen/spec medians, spec/tw (the GATE metric), spec/gen (no-regression), and the
  geomean. `#[ignore]`d; release-only; numbers are machine-dependent (re-run to compare on the SAME
  machine — capture a baseline first, in Task A0).
- **Per-engine manual smoke:** `cargo build` then `target/debug/ascript run X.as` (VM) vs
  `target/debug/ascript run --tree-walker X.as`.
- **Gate after each phase (paste tails):** `cargo test --test vm_differential 2>&1 | tail`;
  `cargo test 2>&1` (0 failures all binaries); `cargo test --no-default-features 2>&1` (0 failures);
  `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets` (clean);
  `grep await_holding_refcell_ref Cargo.toml` (still `deny`).
- **Commit trailer:** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Never** edit a passing tree-walker test, weaken a differential assertion, or relax the `≥2×`
  compute-bound gate to make a number pass. A divergence on valid code = fix the root cause; a perf
  shortfall = improve the design, not the gate.

---

## Phase A — Global-access fast path (index-stable + structural generation)

**Files:** `src/vm/adapt.rs` (`GlobalCache::IndexBound`), `src/vm/run.rs` (`struct_gen` Cell + bump
on define; `Op::GetGlobal`/`Op::SetGlobal` fast paths; helper accessors), `src/vm/chunk.rs` (reuse
the `global_caches` side-table; add a parallel set-site cache if `SET_GLOBAL` needs its own offset
slot). Tests `tests/vm_differential.rs`, `src/vm/adapt.rs` unit tests, `tests/vm_bench.rs`.

### Task A0: baseline + read the code

- [ ] **Step 1 — Record the baseline** on THIS machine: `cargo test --release --test vm_bench -- --ignored --nocapture 2>&1 | tail -20`. Save the table (spec/tw per bench + geomean) into the task notes — this is the before-number every later measurement compares against. (Reference: V11-T6 recorded 2.92× geomean; this branch measured ~2.69× with `while loop` 3.26× / `numeric loop` 2.67× / `sum recursion` 5.14× the regressed benches.)
- [ ] **Step 2 — Read** the current global machinery end-to-end: `src/vm/run.rs:627-685` (`Op::GetGlobal`, esp. the `:661-668` user-global no-cache comment), `:719-773` (`Op::SetGlobal`), `:3003-3060` (`global_version`/`bump_global_version`/`get_user_global`/`user_global_mutable`/`update_user_global`/`define_user_global`), `src/vm/adapt.rs:155-193` (`GlobalCache`), `src/vm/chunk.rs:203,438-448` (`global_caches` storage + `global_cache`/`set_global_cache`). Confirm: user-globals are only ever `insert`ed (never removed), so an entry's `IndexMap` index is stable for the `Vm`'s life.

### Task A1: failing differential tests (correctness baseline — these PASS today and must STAY passing)

- [ ] **Step 3 — Write differential cases** in `tests/vm_differential.rs` (match the file's helper) asserting spec == generic == tree-walker, byte-identical. These are REGRESSION GUARDS for the fast path (they pass today; they must still pass after A2):

```rust
// hot reassigned top-level let in a loop (the regression target; index cache must hit every iter)
diff_case("global_reassign_loop",
    "let sum = 0\nfor (i in 0..1000) { sum = sum + i }\nprint(sum)\n");
// two globals read+written each iteration (the while-loop shape)
diff_case("global_while_two",
    "let i = 0\nlet s = 0\nwhile (i < 1000) { s = s + i\n i = i + 1 }\nprint(s)\n");
// forward/late read of a top-level let from a function defined earlier
diff_case("global_forward_ref",
    "fn get() { return later }\nlet later = 42\nprint(get())\n");
// user-global shadows a builtin (resolution order: user-global wins)
diff_case("global_shadows_builtin",
    "let print2 = 7\nlet len = 99\nprint(len)\n");
// immutable global reassignment -> same-chunk compile-time immutable error
diff_case("global_const_reassign",
    "const k = 1\nk = 2\nprint(k)\n");
// redeclaration -> 'already defined in this scope'
diff_case("global_redeclare",
    "let x = 1\nlet x = 2\nprint(x)\n");
// reference to a not-yet-defined global from a CALL before its define -> undefined variable
diff_case("global_use_before_define",
    "fn get() { return nope }\nprint(get())\n");
```
(Use the file's actual helper name; `diff_case` is illustrative.)

- [ ] **Step 4 — Run, verify GREEN today:** `cargo test --test vm_differential global_ 2>&1 | tail`. (They pass on the current string-keyed path; A2 must keep them green on the index path.)

### Task A2: implement the index-stable global cache

- [ ] **Step 5 — `src/vm/adapt.rs`:** add a third `GlobalCache` variant `IndexBound { idx: usize, struct_gen: u64 }`. Add an accessor `get_index(&self, struct_gen: u64) -> Option<usize>` returning `Some(idx)` iff the variant is `IndexBound` AND its `struct_gen` matches; and `pub fn index_bound(idx: usize, struct_gen: u64) -> GlobalCache`. Keep `get`/`set` (builtins) unchanged. Add a unit test mirroring `global_cache_version_guard`: an `IndexBound` hits at the matching `struct_gen`, misses at a stale one.
- [ ] **Step 6 — `src/vm/run.rs` structural generation:** add `struct_gen: std::cell::Cell<u64>` beside `global_version` (`:136`), init `0` (`:170`). Add `fn struct_gen(&self) -> u64` and bump it ONLY in `define_user_global` (`:3052-3060`) — NOT in `update_user_global` (`:3040-3047`). (Keep `global_version`/`bump_global_version` for the builtin cache.) Add `fn get_user_global_full(&self, name: &str) -> Option<(usize, Value)>` using `IndexMap::get_full` (index + value), and index-keyed accessors `fn user_global_value_at(&self, idx: usize) -> Value` and `fn set_user_global_at(&self, idx: usize, value: Value) -> Option<bool>` (returns the slot's `mutable` for the SET mutability check; uses `get_index`/`get_index_mut`). Keep `update_user_global`'s `class_env` sync (`:3044-3046`) on the index path too.
- [ ] **Step 7 — `Op::GetGlobal` fast path (`src/vm/run.rs:627-685`):** before the existing resolution, when `self.specialize`, consult the site cache: if `IndexBound { idx, struct_gen }` with `struct_gen == self.struct_gen()`, push `self.user_global_value_at(idx)`. Else keep the existing order: try the builtin `Cached` path; then `get_user_global_full(&name)` → on a hit push the value AND (if `self.specialize`) `set_global_cache(fault_ip, GlobalCache::index_bound(idx, self.struct_gen()))`; then the builtin-name branch (records the existing `Cached`); else the `undefined variable` panic. **Critically:** user-globals are resolved BEFORE builtins (unchanged order) so shadowing still works.
- [ ] **Step 8 — `Op::SetGlobal` fast path (`src/vm/run.rs:719-773`):** cache the same `IndexBound` at the SET site. If `SET_GLOBAL` needs a distinct offset cache slot from `GET_GLOBAL` (they are different bytecode offsets, so the existing offset-keyed `global_caches` already disambiguates — confirm), reuse `global_caches`. On a `struct_gen` hit, `set_user_global_at(idx, v)`: `Some(true)` → done; `Some(false)` → immutable error (same message/span as `:758-763`); `None` is impossible on a cached index (defensive: fall through to re-resolve). On a miss, fall through to the existing name-keyed path (`user_global_mutable` + `update_user_global`) AND record `IndexBound` for next time. Do NOT bump `struct_gen` (a SET is not a define).
- [ ] **Step 9 — KILL SWITCH:** gate every consult AND record of `IndexBound` behind `if self.specialize` (mirroring the builtin cache at `:659`). With specialization off, both ops do the name-keyed lookup. Verify no `RefCell` borrow of `user_globals` is held across an `.await` (these ops are synchronous — confirm).
- [ ] **Step 10 — Run** the Task-A1 cases: `cargo test --test vm_differential global_ 2>&1 | tail` → all GREEN (byte-identical). Manual smoke the 7 programs (VM vs `--tree-walker`).
- [ ] **Step 11 — Phase-A correctness gate** (full gate set from Conventions). Three-way differential whole-corpus must be byte-identical.

### Task A3: PERF MEASUREMENT GATE (Phase A)

- [ ] **Step 12 — Re-measure:** `cargo test --release --test vm_bench -- --ignored --nocapture 2>&1 | tail -20` on the SAME machine as A0. Compare against the A0 baseline. **Acceptance:** the regressed benches (`while loop`, `numeric loop`, `sum recursion`, `property r/w`) recover toward their V11-T6 figures; geomean moves toward ~2.9×; `≥2×` holds on EVERY compute-bound bench; NO spec-vs-generic regression on ANY bench. Record the achieved geomean + per-bench spec/tw in the commit message and update the recorded table comment in `tests/vm_bench.rs` (the `GATE RESULT` block) with a dated SP8 line. If the geomean does not improve, the index cache is not hitting (debug: is `struct_gen` bumping inside the loop? is the site cache being consulted? add a temporary counter) — fix the design, do not relax the gate.
- [ ] **Step 13 — Commit:** `perf(vm): index-stable global-access cache (recover the user-global regression)`.

---

## Phase B — #136 capture-by-value closure optimization (additive fast path)

**Files:** `src/syntax/resolve/types.rs` (`UpvalueDescriptor::ParentLocal { by_value }`,
`FrameInfo.value_capture_slots`), `src/syntax/resolve/mod.rs` (`cell_slots` = `captured && mutated`;
populate the new fields; `resolve_upvalue` sets `by_value`), `src/compile/mod.rs` (consume the
narrowed cell set), `src/vm/run.rs` (`Op::Closure` by-value upvalue build). Conditionally
`src/vm/aso.rs`. Tests `tests/vm_differential.rs`, `tests/vm_bench.rs`.

### Task B0: read + decide the `.aso` question FIRST

- [ ] **Step 1 — Read** the closure-capture model: `src/syntax/resolve/mod.rs:519-538` (`cell_slots` = every captured) and `:784-799` (field-default frame), `:334-391` (`resolve_upvalue`/`add_upvalue`/`mark_captured`), `:851-862` (`mark_mutated_target` → `Binding.mutated`); `src/syntax/resolve/types.rs:30-67` (`Binding`, `UpvalueDescriptor`, `FrameInfo`); `src/compile/mod.rs:826-855` (`cur_cells` → `GET_LOCAL_CELL` choice); `src/vm/run.rs:1559-1618` (`Op::Closure`, `GetLocalCell`/`SetLocalCell`/`FreshCell`), `:2013-2021` (`GET/SET_UPVALUE`); `src/vm/fiber.rs:24-60` (`cells`, `alloc_cells`); `src/vm/value_ext.rs:16-40` (`Closure.upvalues: Vec<Cc<RefCell<Value>>>`).
- [ ] **Step 2 — Check `.aso`:** does `src/vm/aso.rs` serialize a `FnProto`/`Chunk`'s `upvalues` descriptors (which would gain a `by_value` bit) and/or `cell_slots`? Grep `aso.rs` for `upvalue`/`cell_slot`/`FrameInfo`/`Chunk`. If YES → the descriptor layout changes, so bump `ASO_FORMAT_VERSION` and round-trip (Task B4). If NO (protos recompiled from source on load) → no bump; note it. Record the finding in the task notes BEFORE implementing.

### Task B1: failing differential tests (correctness — must stay byte-identical)

- [ ] **Step 3 — Write differential cases** in `tests/vm_differential.rs` (spec == generic == tree-walker), covering BOTH the optimized (never-reassigned) and the unchanged (reassigned) capture paths:

```rust
// captured-but-never-reassigned constant: by-value eligible (must be byte-identical to by-ref)
diff_case("capture_const",
    "fn make() { let k = 10\n return fn() { return k } }\nlet f = make()\nprint(f())\n");
// captured AND reassigned (counter): stays a shared cell — mutation visible
diff_case("capture_counter",
    "fn make() { let n = 0\n return fn() { n = n + 1\n return n } }\nlet c = make()\nprint(c())\nprint(c())\nprint(c())\n");
// per-iteration capture freshness: each closure captures its own iteration's value
diff_case("capture_loop_fresh",
    "let fns = []\nfor (i in 0..3) { let v = i * 10\n fns.push(fn() { return v }) }\nfor (g in fns) { print(g()) }\n");
// transitive capture (closure over closure) of a never-reassigned binding
diff_case("capture_transitive",
    "fn a() { let k = 5\n return fn() { return fn() { return k } } }\nprint(a()()())\n");
// mixed: one captured-constant + one captured-counter in the same closure
diff_case("capture_mixed",
    "fn make() { let base = 100\n let n = 0\n return fn() { n = n + 1\n return base + n } }\nlet c = make()\nprint(c())\nprint(c())\n");
```

- [ ] **Step 4 — Run, verify GREEN today** (current by-ref path is correct): `cargo test --test vm_differential capture_ 2>&1 | tail`. They must STAY green through B2/B3.

### Task B2: resolver — narrow cells to `captured && mutated`, mark by-value upvalues

- [ ] **Step 5 — `src/syntax/resolve/types.rs`:** change `UpvalueDescriptor::ParentLocal(u32)` → `ParentLocal { slot: u32, by_value: bool }` (a transitive `ParentUpvalue` keeps its source's kind — no new field). Add `pub value_capture_slots: Vec<u32>` to `FrameInfo`.
- [ ] **Step 6 — `src/syntax/resolve/mod.rs`:** in `resolve_file` (`:524-529`) and the field-default frame (`:787-791`), split the captured bindings: `cell_slots` = `captured && mutated`; `value_capture_slots` = `captured && !mutated`. In `resolve_upvalue` (`:334-345`), when capturing a `ParentLocal`, set `by_value` from the source binding's `mutated` (a never-reassigned source → `by_value: true`). Be careful: `Binding.mutated` is only fully known after the WHOLE frame is resolved (an assignment can appear after the capture textually) — confirm `resolve_upvalue` runs after the source binding's frame is fully walked, OR defer the `by_value` decision to frame-finalization (recompute each `ParentLocal` descriptor's `by_value` from the now-final `mutated` flag when popping the source frame). **This ordering is the single subtlest point — write a resolver unit test for "capture precedes a later reassignment" (must be by-REF / cell).**
- [ ] **Step 7 — Resolver unit tests** (`src/syntax/resolve/mod.rs` `#[cfg(test)]`, near the existing `mutated` test at `:1354`): (a) a captured-never-reassigned local is in `value_capture_slots`, NOT `cell_slots`, and its upvalue descriptor is `by_value: true`; (b) a captured-then-reassigned local is in `cell_slots`, `by_value: false`; (c) capture textually BEFORE a later reassignment is still by-REF (cell). Run → green.
- [ ] **Step 8 — Commit:** `feat(resolve): capture-by-value eligibility (captured && !mutated)`.

### Task B3: compiler + VM by-value capture

- [ ] **Step 9 — `src/compile/mod.rs`:** `cur_cells` is built from the frame's `cell_slots`; now that `cell_slots` excludes `captured && !mutated`, `emit_get_local`/`emit_set_local` (`:839-855`) automatically emit plain `GET_LOCAL`/`SET_LOCAL` for a by-value slot (a never-reassigned binding's single store is its declaration → one `SET_LOCAL`; reads → `GET_LOCAL`). Confirm `loop_fresh_cells` / `FreshCell` emission (`:857+`, `src/vm/run.rs:1610-1618`) is keyed on `cell_slots` so a now-plain slot gets NO `FreshCell`. Adjust the `Op::Closure` upvalue emission to carry the `by_value` bit per descriptor (the descriptor already holds it — the compiler just writes the proto's upvalue table; verify nothing in the compiler assumed the old `ParentLocal(u32)` tuple shape).
- [ ] **Step 10 — `src/vm/run.rs` `Op::Closure` (`:1574-1593`):** for `ParentLocal { slot, by_value }`: `by_value == false` → existing path (clone the parent frame's cell `Cc`, panic if `cells[slot]` is `None`). `by_value == true` → read the parent frame's PLAIN slot value (`fiber.local(slot).clone()` — the stack slot at `slot_base + slot`, NOT `cells`) and wrap a FRESH `Cc::new(RefCell::new(v))` into the upvalue (representation-uniform; recommended approach (a) from the spec — zero `value.rs`/`Closure` change). `ParentUpvalue` unchanged.
- [ ] **Step 11 — Run** the Task-B1 cases: `cargo test --test vm_differential capture_ 2>&1 | tail` → all GREEN (byte-identical). Manual smoke (VM vs `--tree-walker`).
- [ ] **Step 12 — Phase-B correctness gate** (full gate set). Whole-corpus three-way byte-identical.

### Task B4: `.aso` (conditional on B0)

- [ ] **Step 13 — IF B0 found upvalue descriptors are serialized:** update `src/vm/aso.rs` to (de)serialize the `by_value` bit + `value_capture_slots`; bump `ASO_FORMAT_VERSION`; verifier validates; add a `tests/aso.rs` build+run round-trip for a closure capturing a constant (built `.aso` == tree-walker) and confirm an old `.aso` is rejected with the version-mismatch message. IF B0 found they are NOT serialized → skip; note "no `.aso` change (protos recompiled on load)".
- [ ] **Step 14 — Commit:** `feat(vm): capture-by-value upvalues (#136) — additive fast path`.

### Task B5: PERF MEASUREMENT GATE (Phase B)

- [ ] **Step 15 — Add a closure-capture-heavy bench** to `tests/vm_bench.rs::benches()` (match the `Bench` struct, deterministic, ends with a `print`), e.g.:

```rust
Bench {
    name: "closure capture (1e6)",
    compute_bound: true,
    src: r#"
fn make(base) {
  let k = base + 1
  return fn() { return k }
}
let total = 0
for (i in 0..1000000) {
  let f = make(i)
  total = total + f()
}
print(total)
"#,
},
```
(This builds a closure capturing a never-reassigned `k` each iteration — by-value eligible — vs the
old per-iteration cell allocation.)

- [ ] **Step 16 — Re-measure:** `cargo test --release --test vm_bench -- --ignored --nocapture 2>&1 | tail -25` on the SAME machine. **Acceptance:** the new closure bench shows a measurable spec/tw + spec/gen win vs a quick before-snapshot (stash the resolver/VM change, measure, unstash — or compare against the Phase-A baseline run of the same bench added first); NO regression on any existing bench (the counter/mutated captures still use cells, unchanged). Record the number in the commit message + the `tests/vm_bench.rs` `GATE RESULT` comment.
- [ ] **Step 17 — Commit:** `perf(vm): measure capture-by-value win + closure-capture bench`.

---

## Phase C — Holistic gate, perf record, review

**Files:** `tests/vm_bench.rs` (final recorded table), `docs/superpowers/specs/2026-06-02-bytecode-vm-design.md` (mark capture-by-value as landed, not deferred), the resolver comment `src/syntax/resolve/mod.rs:519-523` (update "FUTURE optimization (V5)" wording).

### Task C1: final measurement + records

- [ ] **Step 1 — Full perf run** on the reference machine: record the final geomean spec/tw, every per-bench spec/tw + spec/gen, into the `tests/vm_bench.rs` `GATE RESULT` comment block with a dated SP8 entry. Confirm geomean recovered toward ~2.9× and `≥2×` holds on every compute-bound bench with no spec-vs-generic regression.
- [ ] **Step 2 — Update stale comments/docs:** `src/syntax/resolve/mod.rs:519-523` (capture-by-value is no longer "a FUTURE optimization (V5)" — it is implemented for `captured && !mutated`); the VM spec `docs/superpowers/specs/2026-06-02-bytecode-vm-design.md:147,327` (capture-by-value is landed). The `src/vm/run.rs:661-668` user-global "we do NOT cache it" comment must be updated to describe the index-stable cache.

### Task C2: holistic gate + independent review

- [ ] **Step 3 — Full gate set** both feature configs + clippy both + `grep await_holding_refcell_ref Cargo.toml` (deny) + whole-corpus three-way differential green + the perf gate.
- [ ] **Step 4 — Independent review** (re-read spec, re-run gates, adversarial divergence hunt over the new surface): global shadowing/redeclaration/forward-ref/immutable-cross-chunk under the index cache; closures capturing constants vs counters, per-iteration freshness, transitive captures, the "capture-before-later-reassignment stays by-ref" subtlety. Fix any divergence at the root.
- [ ] **Step 5 — Final commit** if review surfaced fixes; otherwise the sub-project is complete.

---

## Self-review (author)

**Spec coverage:** §1 global-access fast path → Phase A (A2 implement, A3 perf gate); §2
capture-by-value → Phase B (B2 resolver, B3 compiler+VM, B4 `.aso`-conditional, B5 perf gate); the
spec's testing bar + measurement gates → A3/B5/C1; stale-comment cleanup → C2. All covered.

**Placeholder scan:** No "TBD/handle edge cases". Test programs are concrete AScript; the one deferral
to the implementer is exact Rust signatures (the spec/plan give the change sites with line numbers).
Differential helper name (`diff_case`) is illustrative — the implementer uses the file's actual
helper. The `.aso` bump is correctly CONDITIONAL on B0's finding (the plan does not assume it).

**Type consistency:** `GlobalCache::IndexBound { idx: usize, struct_gen: u64 }`;
`struct_gen: Cell<u64>` on `Vm` (distinct from `global_version`, which keeps serving the builtin
cache); `UpvalueDescriptor::ParentLocal { slot: u32, by_value: bool }`;
`FrameInfo.value_capture_slots: Vec<u32>`; `Closure.upvalues` stays `Vec<Cc<RefCell<Value>>>`
(approach (a), no `value.rs` change). `ASO_FORMAT_VERSION` bumped AT MOST once (B4, only if upvalue
descriptors are serialized). Consistent across spec + plan.

**Invariant adherence:** byte-identical three-way differential is the gate on every phase; both
feature configs; clippy both; `await_holding_refcell_ref` stays deny (global ops are synchronous);
`≥2×` compute-bound gate never relaxed; per-task commit trailer present. The tree-walker and
`src/value.rs` are untouched.

**Subtlest risk flagged:** B2 Step 6 — the `by_value` decision depends on the FINAL `mutated` flag,
which may be set by an assignment textually AFTER the capture; the plan requires finalizing `by_value`
at frame-pop (when `mutated` is complete) and a dedicated unit test for capture-before-later-reassign.
This is the one place a subtle bug could make a reassigned binding wrongly captured by value (a
behavior divergence) — the differential `capture_counter`/`capture_mixed` cases + the resolver unit
test guard it.
