# VAL — Compact Value Representation & Allocation Discipline — Implementation Plan

> REQUIRED SUB-SKILL: superpowers:subagent-driven-development (fresh implementer + independent reviewer
> per task; the reviewer RUNS the commands and probes edges). Steps use `- [ ]`. VAL is **pure
> performance** — behavior byte-identical across tree-walker == specialized-VM == generic-VM == `.aso`,
> both feature configs, is the hard gate (`goal-brief.md` Gate 1). Every microbench measures BOTH
> specialized AND `--no-specialize` generic mode (Gate 12). No surface change ⇒ Gates 9/11/13 are **N/A**
> (justified in §6 of the spec and stated per-task below).

**Spec:** `superpowers/specs/2026-06-08-compact-value-design.md`. **Branch:** `feat/compact-value` off
`main`. **Depends on:** **NUM** spec-locked, plan-written, and **merged** first (`goal-brief.md` execution
order) — VAL is written against the post-NUM world (`Value::Int(i64)`, `Value::Float(f64)`,
`MapKey::Int(i64)`, `ArithKind::Int`, the `Int` `.aso` constant kind, the `Int` worker wire tag). Rebase
onto NUM's `main` before starting. **Depended on by:** the deferred JIT; benefits SRV. **Breaking: NO** —
zero observable-behavior change is the gate; no surface syntax, no new `Value` kind, no new `type_name`.

**Architecture:** shrink `Value` from its true **32 bytes** (set by the two 24-byte two-field method
bindings, NOT by `Decimal`/`Str`) toward 8, in four independently-shippable, independently-four-mode-tested
**stages**:
1. **Box the two fat variants** (`ClassMethod(Rc<Class>, Rc<str>)` → `Rc<ClassMethodData>`;
   `GeneratorMethod(Rc<GeneratorHandle>, &'static str)` → `Rc<GeneratorMethodData>`) + move `Decimal` (16 B)
   behind a pointer + inline scalars (`Nil`/`Bool`/`Int`/`Float`) + SMI fast paths in VM arith. This is the
   §3.3 **niche-optimized fallback** — derived `Clone`/`Drop` over `Cc`/`Rc`, **no new ownership `unsafe`**.
   Ships **regardless** of the Stage-2 verdict (32→16 B; ≤8 only with optional thin-`Str`).
2. **Pointer NaN-box** to 8 B — **GATED** on (a) upstreaming gcmodule `Cc::into_raw`/`from_raw` AND (b) the
   GC-soundness suite green on the tagged layout. Introduces the hand-written tag-dispatched
   `unsafe` `Clone`/`Drop`/`trace`. **If either gate fails, STOP at Stage 1's 16-byte enum** — an
   owner-noted, recorded stopping point (`goal-brief.md` Gate 6). An explicit decision task gates this.
3. **Small-string inlining** (≤6 B under NaN-box, or a small-string-optimized `Str` under the fallback).
4. **Compiler escape-analysis** stack allocation (`src/compile/`) with the **identity-preservation
   soundness obligation** (§3.4 of the spec) and a `--no-escape-analysis` kill switch (a fifth differential
   axis).

**Tech stack:** Rust; `src/value.rs`, `src/gc.rs`, `src/vm/{run,adapt,ic,shape,aso,verify}.rs`,
`src/compile/mod.rs`, `src/worker/serialize.rs`; `static_assertions` (new dev/normal dep, per
`goal-brief.md` cross-spec decisions). The AST/grammar half of CLAUDE.md's "Touching syntax" is **N/A**.

---

## Shared API Contract (pinned to current code — pre-NUM line numbers; NUM-renamed names noted)

**Existing (verified TODAY, pre-NUM):**
- `Value` enum `value.rs:623`; **size_of == 32** (set by the two 24-byte two-field variants).
- The two FAT variants: `GeneratorMethod(Rc<crate::coro::GeneratorHandle>, &'static str)` `value.rs:675`;
  `ClassMethod(Rc<Class>, Rc<str>)` `value.rs:681`. Their arms: `PartialEq` `value.rs:730,733`; `Debug`
  `value.rs:778,779`; `Display` `value.rs:892,893`.
- `Value::Decimal(Decimal)` (16 B, `Copy`) `value.rs:630`; `Value::Str(Rc<str>)` `value.rs:631`.
- Container `Cc` variants: `Closure` `value.rs:639`, `Array` `:640`, `Object` `:641`, `Map` `:644`,
  `Set` `:648`, `Instance` `:662`. `Rc` no-op-`Trace` variants: `Bytes` `:650`, `Native` `:656`,
  `Enum`/`EnumVariant` `:659-660`, `Class` `:661`, etc.
- `is_truthy` `value.rs:687` (POST-NUM: NUM rewrites the falsy set — VAL must preserve whatever NUM lands).
- `type_name` `value.rs:483` (+ `interp.rs:5392`).
- `impl PartialEq for Value` `value.rs:692`; identity via `crate::gc::cc_ptr_eq` `value.rs:707-723`,
  `gc.rs:142`.
- `impl Trace for Value` `gc.rs:176-206` (container arms `:183-192`, native/immutable catch-all `_ => {}`
  `:204`); container `Trace` impls `gc.rs:218-309` (untouched). `cc_ptr_eq` `gc.rs:142`.
- gcmodule-0.3.3: `Cc<T>` = `NonNull<RawCcBox<T,O>>` `src/cc.rs:79`; registry-driven collector
  `src/collect.rs:58-114`. **No public `into_raw`/`from_raw`** (the Stage-2 gate).
- VM arith site `run.rs:3778` (`apply_binop` fallthrough), specialized guards `run.rs:3779-3805`
  (`ArithKind::Number` arm `:3779`), warmup observe `:3808-3819`. `ArithKind` enum `adapt.rs:49`
  (POST-NUM: NUM adds `ArithKind::Int`). `cache.specialized()`/`observe()`/`deopt()` in `adapt.rs`.
- `Vm.specialize: bool` `run.rs:104`; `Vm::with_specialize` `run.rs:165`; `Vm::new_generic` `run.rs:169`.
- ICs `vm/ic.rs`; shapes `vm/shape.rs` (key on KEYS, not value encoding).
- `ASO_FORMAT_VERSION` `aso.rs:105` (currently 18; **POST-NUM ≥19** — read-and-+1, never hardcode).
- Worker wire tags `serialize.rs:80-89` (`TAG_NIL=0` `:80`, `TAG_NUMBER=2` `:82`, `TAG_DECIMAL=3` `:83`);
  `encode_value` `:380`, `decode_value` dispatch `:525`. POST-NUM: NUM adds an `Int` tag; `TAG_NUMBER`
  becomes the `Float` tag.
- Four-mode differential helper `tests/vm_differential.rs:7190-7254` (tree-walker / specialized /
  `vm_run_source_generic` / `.aso` via `build_and_run_aso`). VM test entry points `vm_run_source` /
  `vm_run_source_generic` (`lib.rs`).
- `src/compile/` is a single `mod.rs` (Stage 4 adds the escape-analysis pass here).
- `bench/` dir + `src/stdlib/bench.rs` (`bench.measure`/`bench.compare`, `:50`/`:105`).

**New names (introduce; do not collide with NUM):** `ClassMethodData { class: Rc<Class>, name: Rc<str> }`,
`GeneratorMethodData { handle: Rc<GeneratorHandle>, name: &'static str }` (Stage 1); `Value::int`,
`Value::float`, `Value::object(..)`, `as_int`/`as_float`/`as_object`/… accessor helpers (insulation layer);
Stage-2-only `ValueTag` + `Cc::into_raw`/`from_raw` (gated); Stage-4 `--no-escape-analysis` flag +
`Vm`/compiler `escape_analysis: bool` paralleling `specialize`.

## Conventions (every task)
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- `cargo test` AND `cargo test --no-default-features` green; `cargo clippy --all-targets` AND
  `--no-default-features --all-targets` clean.
- **Four-mode byte-identity** (`tests/vm_differential.rs`, both feature configs) re-runs per stage — VAL
  adds **zero new goldens** (no behavior change) but re-runs every existing one on each new layout. Fix the
  encoding/decode, never the assertion.
- **No `await` across a `RefCell`/resource borrow**; native handles stay GC-opaque (`Value::trace` decodes
  then does nothing for them — §3.4, Gate 4). The V13-T6 native-`Drop` gate re-runs unchanged per stage.
- **Insulate via accessors.** After Stage 1's helpers land, the rest of the tree reads scalars/containers
  through `Value::int`/`as_int`/`Value::object`/… so later stages change the encoding in `value.rs` only
  (mirrors NUM's mechanical-rename discipline).
- **Gate 9/11/13 N/A, stated:** no grammar, no tree-sitter, no `examples/**` additions, no
  `docs/content/**`/`NAV` additions — a pure-representation change. Only internal doc touch-points
  (`CLAUDE.md` "Values", the design-spec perf section, `bench/`, `roadmap.md`) — Task 13.
- **Bench every wall-clock workload in BOTH VM modes** (specialized + `vm_run_source_generic` /
  `--no-specialize`). A generic-mode regression is a VAL **bug**, not a trade (Gate 12).

---

## Task 0 — Foundation: `static_assertions`, the `Value: Send` lock, size tripwire, accessor scaffold
**Files:** `Cargo.toml`, `src/value.rs`. **Tests:** `value.rs` (`#[test]`), compile-time asserts.
- [ ] Failing/red: add a `#[test] fn value_size_is_documented()` asserting `size_of::<Value>() == 32`
  (the verified pre-shrink baseline — this test is the **moving tripwire** that each stage updates: 32 → 16
  → 8); and a no-op accessor round-trip test (`Value::int(5).as_int()==Some(5)`, `Value::float`/`as_float`,
  `Value::object`) that currently fails because the helpers don't exist.
- [ ] Add `static_assertions` to `Cargo.toml` (dev+normal as needed; `goal-brief.md` cross-spec decision).
  Add `assert_not_impl_any!(Value: Send, Sync);` next to the `Value` definition (`value.rs:623`) — locks the
  `#[tokio::main(flavor = "current_thread")]` + `LocalSet` invariant; a future `Send` decision must delete
  this assert, surfacing the choice (spec §6). Add the `Value::int`/`float`/`object`/`as_int`/`as_float`/
  `as_object` (and any other hot kind) thin accessors over the CURRENT enum encoding so the codebase can be
  migrated onto them ahead of the encoding change.
- [ ] Green both configs; clippy both. **Independent review:** confirm the assert fails to compile if a
  `Send` field is hypothetically added (reviewer probes by a throwaway edit); confirm accessors are
  zero-cost over the current enum. Commit.

---

## Stage 1 — Box the fat variants, move Decimal behind a pointer, inline scalars + SMI (the floor; ships regardless of Stage 2)

## Task 1 — Box `ClassMethod`/`GeneratorMethod` into single `Rc` payloads (the load-bearing 32→24 shrink)
**Files:** `src/value.rs` (+ every exhaustive `match Value` the compiler flushes: `interp.rs`, `fmt.rs`,
`gc.rs`, `worker/serialize.rs`, `vm/*`). **Tests:** `value.rs`.
- [ ] Failing test: `size_of::<Value>()` drops below 32 once these two variants are 1-word (assert it is
  now ≤24, since `Decimal`(16) is still the next-widest until Task 2); construct + dispatch a `ClassMethod`
  (`User.from`, a user static) and a `GeneratorMethod` (`gen.next`) and assert byte-identical behavior to
  pre-change (display `<class method User.x>`, `<generator method next>`; `PartialEq`).
- [ ] Introduce `struct ClassMethodData { class: Rc<Class>, name: Rc<str> }` and
  `struct GeneratorMethodData { handle: Rc<GeneratorHandle>, name: &'static str }`; change the variants to
  `ClassMethod(Rc<ClassMethodData>)` / `GeneratorMethod(Rc<GeneratorMethodData>)`. Update the `PartialEq`
  `value.rs:730,733`, `Debug` `:778,779`, `Display` `:892,893` arms, the `Trace` catch-all (still no-op —
  these are acyclic `Rc`), and every construction/dispatch site the compiler flags (`call_value`,
  `call_generator_method`, the typed-parser `User.from` path). Behavior IDENTICAL — only the payload is now
  one indirection.
- [ ] Green both configs; clippy. **Four-mode differential green** (re-run; no new goldens). **Review:**
  grep for any residual two-field construction; confirm `User.from`, user statics, and `gen.next`/`for await`
  are byte-identical on all four modes; confirm `size_of` dropped. Commit.

## Task 2 — Move `Decimal` behind a pointer; shrink the enum to the niche-fallback 16 bytes
**Files:** `src/value.rs` (+ compiler-flushed arms in `interp.rs`/`vm/run.rs` decimal arith, `fmt.rs`,
`worker/serialize.rs` `TAG_DECIMAL`, `aso.rs` decimal constant). **Tests:** `value.rs`, decimal tests.
- [ ] Failing test: `size_of::<Value>() == 16` (the §3.3 niche-fallback floor — now that the two fat
  variants AND `Decimal` are ≤8-byte payloads and scalars fit a word, Rust's niche optimization over
  `Cc`/`Rc`'s non-null guarantee fills the discriminant); decimal arithmetic / `type_name=="decimal"` /
  exact equality / Map-key fold all byte-identical (an extra indirection, zero behavior change).
- [ ] Change `Value::Decimal(Decimal)` → `Value::Decimal(Rc<Decimal>)` (or a pooled box). Update the
  `decimal_fast` VM arm `run.rs:~3787`, the `apply_binop` decimal arm in `interp.rs`, `Display`/`Debug`,
  `MapKey` (still folds by value), the worker `TAG_DECIMAL` encode/decode (still the SAME logical tag —
  serialize the decoded decimal, **not** the pointer), and the `.aso` decimal constant (still logical — no
  `ASO_FORMAT_VERSION` bump if the serialized form is unchanged; if the box changes the in-pool layout, read
  `ASO_FORMAT_VERSION` and +1 per `goal-brief.md`).
- [ ] Update Task 0's size tripwire to 16. Green both configs; clippy. **Four-mode differential green.**
  **Review:** confirm decimal exactness preserved (the boxing is invisible), `.aso` round-trip green,
  worker round-trip of a `Decimal` byte-identical. Commit.

## Task 3 — Inline-scalar SMI fast path in VM arithmetic + adaptive `ArithKind::Int` operand load/store
**Files:** `src/vm/run.rs` (arith site `:3778-3819`), `src/vm/adapt.rs`. **Tests:** `vm_differential.rs`,
`value.rs` boundary test.
- [ ] Failing tests (BOTH engines, four-mode): the SMI↔boxed spill **boundary** unit test of spec §7.2 —
  `2^47 − 1`, `2^47`, `−2^47`, `−2^47 − 1`, plus `2^53`, `i64::MAX`, `i64::MIN` — asserting for each:
  round-trip (`decode(encode(n))==n`), arithmetic that carries across the boundary yields the right
  kind+value, comparison across an SMI and a boxed operand of equal/adjacent value, and `MapKey` fold (an
  SMI and a boxed `Int` of equal value are the SAME Map key) are byte-identical on tree-walker ==
  specialized-VM == generic-VM. **NOTE:** under the Stage-1 *niche fallback* `Int` is a full inline `i64` —
  no SMI, no spill — so in Stage 1 this test exercises the inline-`i64` path and the SMI/spill assertions
  become live only at Stage 2 (write the test now; it must pass trivially under the full-`i64` Stage-1 layout
  and again under the NaN-box if Stage 2 lands).
- [ ] Add the `ArithKind::Int` guarded fast path in the arith site (NUM already added the `ArithKind::Int`
  variant + checked-int semantics — Stage 1 makes its operand load/store read the inline scalar word with no
  heap touch, then run the EXACT same checked-int computation NUM defined). Specialized and generic VM
  byte-identical (incl. which inputs panic). No new opcode.
- [ ] Green both configs; clippy. **Four-mode differential green** (re-run scalar-heavy goldens).
  **Review:** confirm the fast path is "guard the kind, then the exact generic op" (`run.rs:3779-3805`
  pattern); confirm a mixed Int/Float site deopts identically in both VM modes; confirm no panic-input
  divergence. Commit.

## Task 4 — Stage-1 benchmarks (both VM modes) + size report + cold-path check
**Files:** `bench/` (new harness writing a markdown report sibling to `bench/PROFILING_RESULTS.md`),
reuse `src/stdlib/bench.rs`. **Tests:** the bench harness runs; no CI perf gate beyond Gate-12 no-regress.
- [ ] Add `bench/compact_value_bench.as` + a runner; report **`size_of::<Value>()` 32→16**, and wall-clock
  on scalar-heavy loops (int sum, array-index walk, Fibonacci/Mandelbrot over NUM ints), allocation/refcount
  churn (a `Cc`/`Rc` clone counter or `dhat`), and a cache-density proxy (large `Vec<Value>` / large
  `IndexMap` traversal) — **each in BOTH specialized AND `--no-specialize` generic mode** side by side
  (Gate 12). Add the **cold-path check**: a `ClassMethod`/`GeneratorMethod` construct+dispatch microbench
  confirming Task 1's boxing adds no measurable regression on those rare bindings.
- [ ] Honest framing in the report: state the measured geomean PER MODE and flag any regression (the extra
  `Decimal` indirection, the boxed-method-binding indirection). **No speedup is claimed** — the number is
  whatever `bench/` reports. **Gate 12: no regression in EITHER mode.**
- [ ] **Review:** reviewer RE-RUNS the bench in both modes and confirms the report's numbers + the
  no-either-mode-regression floor. Commit. **← Stage 1 is a green merge-able point on its own.**

---

## Stage 2 — Pointer NaN-box to 8 bytes (GATED)

## Task 5 — GATE DECISION: gcmodule `Cc::into_raw`/`from_raw` + GC-soundness verdict
**Files:** (decision doc inline in the plan/PR; an upstream gcmodule PR if pursued); no core change yet.
**Tests:** the existing `src/gc.rs` V13-T4 (cycle reclamation) + V13-T6 (native-`Drop` determinism) suites
as the soundness baseline.
- [ ] Determine whether `Cc::into_raw`/`from_raw` can be **upstreamed** to gcmodule (the supported path —
  a small PR exposing the `NonNull<RawCcBox<T,O>>` round-trip; `src/cc.rs:79`). The layout-coupling
  `transmute` over the private `#[repr(C)] RawCcBox` (`src/cc.rs:38-79`) is **REJECTED as fragile** (spec
  §8) — do not take it.
- [ ] **Record the verdict (owner-noted, never silent — Gate 6):** if (a) `into_raw`/`from_raw` is available
  upstream AND (b) the §3.3 GC-soundness design (decode-in-`Value::trace` + correct tag-dispatched
  `Clone`/`Drop`) is judged to pass the V13-T4/T6 gates on the tagged layout → **proceed to Task 6**. Else
  → **STOP at Stage 1's 16-byte enum**, mark Stage 2 deferred in `roadmap.md` + the design spec with the
  reason, and **skip Tasks 6–8** (jump to Stage 3 over the niche-fallback enum, which still benefits).
- [ ] **Review:** an independent reviewer confirms the verdict's evidence (the upstream PR state / the
  soundness argument) and signs off on proceed-vs-stop. Commit the recorded decision.

> **Tasks 6–8 are conditional on Task 5 = PROCEED. If STOP, Stage 2 is deferred and Stage 3 runs over the
> 16-byte niche-fallback enum.**

## Task 6 — NaN-box encoding: `Float` verbatim, `Int`-SMI, `Nil`/`Bool` singletons, tagged heap pointers
**Files:** `src/value.rs` (the encoding + hand-written `PartialEq`/`Hash`/`Debug`/`is_truthy`/`type_name`
decode-then-match; accessor helpers from Task 0 now wrap the bits). **Tests:** `value.rs` (the §7.2
boundary test from Task 3 now exercises the LIVE SMI/spill path), encoding round-trip property test.
- [ ] Failing tests: `size_of::<Value>() == 8` (update the Task 0 tripwire); the SMI bit budget — **3-bit
  tag + 48-bit payload**, `Int`-SMI range **[−2^47, 2^47 − 1]** as two's-complement i48, anything beyond
  **spills to a pooled `Rc<i64>` box**; an SMI and a boxed-i64 of equal value byte-identical for
  `type_name`/arith/compare/print/`MapKey` fold; encoding round-trip `∀v: decode(encode(v))==v` across
  SMI/boxed-`Int`, every heap kind, `Nil`/`Bool`/`Float` (incl. the canonical-quiet-NaN carve-out so a real
  `Float(NaN)` is not mistaken for a tag).
- [ ] Implement the NaN-box per §3.2: a non-NaN-tagged word IS an `f64` (read verbatim); the NaN space
  carries the 3-bit tag (`Nil`/`Bool`/`Int`-SMI/heap-pointer-family) + 48-bit payload; heap kinds are the
  same `Rc`/`Cc` pointer with unused high bits tagged. `Decimal` is a heap kind (already boxed in Task 2).
  Hand-write the decode-then-match `PartialEq`/`Hash`/`Debug`/`is_truthy`/`type_name`. The Task-0 accessors
  now decode the bits — the rest of the tree is textually insulated.
- [ ] Green both configs; clippy. **Review:** confirm the canonical-quiet-NaN carve-out is exact (a
  property test over `f64::NAN` and FPU-produced NaNs); confirm SMI spill threshold is `|n| ≤ 2^47−1`
  inlines, `n == −2^47` inlines, beyond spills. Commit (NOT yet four-mode-green — Clone/Drop/trace land next).

## Task 7 — Hand-written tag-dispatched `unsafe` `Clone`/`Drop`/`Value::trace` ownership layer
**Files:** `src/value.rs` (the `unsafe` `Clone`/`Drop`), `src/gc.rs` (`Value::trace` decode-then-recurse,
`:176-206`). **Tests:** `gc.rs` V13-T4/T6 + a new `Cc`-roundtrip leak/double-free property test.
- [ ] Failing tests: a `Cc`-roundtrip property test — encode→`Value::trace`→decode→`Drop` reclaims
  correctly with **no leak and no double-free** under a V13-T5-style soak (clone/drop a tagged container N
  times; assert refcount returns to baseline and the cycle collector reclaims); the V13-T4 (every cycle
  class reclaimed) and **V13-T6 (native-`Drop` fd determinism)** suites pass UNCHANGED on the tagged layout.
- [ ] Implement the hand-written `Clone` (tag-dispatch: reconstruct the right `Cc<T>`/`Rc<T>` via the
  upstreamed `from_raw`, `clone`, re-tag) and `Drop` (reconstruct + drop the strong ref) — a missed
  decrement leaks, an extra is UAF, so this is the load-bearing `unsafe`. `Value::trace` (`gc.rs:177`)
  **decodes the tag then recurses** into the container `Cc`s exactly as the current container arms
  (`:183-192`) and does **NOTHING** for the native/immutable kinds (the `_ => {}` catch-all `:204` — §3.4,
  Gate 4 preserved). Container `Trace` impls (`gc.rs:218-309`) UNTOUCHED. **Cross-spec note (record in the
  PR for SRV):** the tag space mixes `!Send` `Rc`/`Cc` (non-atomic) ownership; a future SRV
  `Value::Shared(Arc<…>)` tag must dispatch to ATOMIC `Arc` ops here — this does not make `Value: Send` (the
  Task-0 assert still holds).
- [ ] Green both configs; clippy (note: `await_holding_refcell_ref` unaffected — no new awaits). **Four-mode
  differential green** (the whole corpus re-runs on the 8-byte layout). **GC soundness suite green.**
  **Review:** the reviewer RUNS the leak/soak test under valgrind/ASan-or-`dhat` if available and audits
  every tag arm for a missing/extra refcount op. Commit. **← Stage 2 is a green merge-able point.**

## Task 8 — Stage-2 size report + both-mode bench delta + `.aso`/worker re-audit
**Files:** `bench/` (extend Task 4's report), `src/vm/aso.rs`/`verify.rs` + `src/worker/serialize.rs`
(audit only). **Tests:** bench harness; `.aso` + worker round-trip tests.
- [ ] Extend the report: `size_of::<Value>()` **16→8**; re-run every Task-4 workload in BOTH modes; flag the
  SMI-spill branch cost on huge-int workloads (honest framing). **Gate 12: no regression in either mode.**
- [ ] **`.aso` audit:** the constant pool serializes LOGICAL values (`aso.rs:440`-era) — confirm Stage 2
  changed nothing on-disk (recommended: serialize the decoded value, not the in-memory word) ⇒ **no
  `ASO_FORMAT_VERSION` bump**; if any serialized form changed, read `ASO_FORMAT_VERSION` and +1 (never
  hardcode) + update `verify.rs`. **Worker audit:** wire tags are LOGICAL (`serialize.rs:80-89`) — add a
  round-trip test that every kind (incl. SMI vs boxed-`Int` → same `Int` tag) survives, byte-identical.
- [ ] Green both configs. **Review:** reviewer re-runs bench (both modes) + `.aso`/worker round-trips.
  Commit.

---

## Stage 3 — Small-string inlining

## Task 9 — Inline small strings in the value word (NaN-box) or SSO `Str` (fallback)
**Files:** `src/value.rs` (the `Str` encoding + decode-then-operate Display/concat/equality/`MapKey::Str`),
`src/vm/run.rs` (the `ConcatStr` arith arm `:3793`). **Tests:** `value.rs` property test, `vm_differential.rs`.
- [ ] Failing tests: an inline `"hi"` and a heap `"hi"` are byte-identical for Display, concat (`+`),
  equality, hash, JSON, and `MapKey::Str` fold (a property test mirroring NUM's Map-key test); strings up to
  N bytes (≤6 under NaN-box; an SSO `Str` under the fallback) allocate **no** `Rc<str>`; the single-codepoint
  strings NUM's `string.from_codepoints`/`code_at` produce are inline. `size_of::<Value>()` unchanged.
- [ ] Implement inline-string storage (tag + inline bytes under NaN-box; a small-string-optimized `Str`
  representation under the fallback). The `ConcatStr` VM fast path (`run.rs:3793`) and every `Value::Str`
  consumer go through the Task-0 accessors / decode-first ops, so an inline and heap string are
  indistinguishable. `MapKey::Str` folds by the decoded `&str`.
- [ ] Green both configs; clippy. **Four-mode differential green.** **Review:** confirm zero alloc for short
  keys/identifiers (a `dhat` spot-check); confirm shape ICs don't thrash on an inline/heap-`Str` mix (add a
  cache-non-thrash test per spec §6). Commit. **← Stage 3 is a green merge-able point.**

---

## Stage 4 — Escape-analysis stack allocation (compiler)

## Task 10 — Escape-analysis pass + identity-preservation soundness obligation + `--no-escape-analysis`
**Files:** `src/compile/mod.rs` (the new pass), `src/vm/run.rs` (frame-slot allocation of stack values),
`src/lib.rs` + the CLI (`--no-escape-analysis` flag), `src/vm/run.rs` (`escape_analysis: bool` paralleling
`specialize` `:104`). **Tests:** `vm_differential.rs` (a FIFTH axis), `tests/check.rs`/compile tests.
- [ ] Failing tests: the **identity-preservation soundness obligation** (spec §3.4) — a constructed
  `Array`/`Object`/`Map`/`Set`/`Instance` is classified **non-escaping ONLY IF** the analysis proves the
  value is never (a) stored to a heap field / returned / captured / sent to a worker, AND (b) an operand of
  `==`/`!=`/`is` (identity via `cc_ptr_eq`, `value.rs:707-723`), AND (c) inserted into an identity-keyed
  `Map`/`Set` — within its frame. Tests that each identity observation FORCES heap allocation (a
  stack-replaced value would lose shared-pointer identity and change an observable `==`/`is`/Map-key
  result). Any uncertainty ⇒ heap-allocate (the sound default).
- [ ] Implement the conservative escape-analysis + scalar-replacement pass in `src/compile/`; frame-slot
  allocation for proven-non-escaping constructions (never GC-tracked, reclaimed by frame pop). Add the
  `--no-escape-analysis` kill switch + `Vm.escape_analysis` flag so the optimized and un-optimized paths are
  independently differential-tested.
- [ ] Green both configs; clippy. **Five-mode differential green:** the four existing modes PLUS
  `--escape-analysis` vs `--no-escape-analysis` proving stack- and heap-allocated objects are
  observationally identical (`tests/vm_differential.rs`). **Review:** the reviewer constructs adversarial
  identity-leak cases (`let a={}; let b=a; a==b`; `{} as a Set member`; an object returned then compared)
  and confirms each is classified escaping. Commit. **← Stage 4 is a green merge-able point.**

## Task 11 — Stage-4 bench (both modes) + escape-analysis allocation-elimination report
**Files:** `bench/` (extend the report). **Tests:** bench harness.
- [ ] Report allocation/refcount churn eliminated on an object-heavy workload (short-lived `{x,y}` returned
  or stored once) with escape-analysis ON vs OFF, in BOTH VM modes; confirm Gate-12 no-regression in either
  mode and no regression vs `--no-escape-analysis`. Honest framing; no speedup claimed.
- [ ] **Review:** reviewer re-runs both modes + the on/off delta. Commit.

---

## Task 12 — Tooling parity smoke tests (fmt / repl / lsp / check) under the new layout
**Files:** tests in `tests/` / inline. **Tests:** `tests/lsp.rs`, fmt idempotence, repl regression.
- [ ] These consume `Value`/AST through the public API and never render a `Value`'s encoding ⇒ **expected
  ZERO behavior change** (Gate 9/11/13 N/A — stated). Add a regression smoke test in each: formatter
  idempotence, REPL cross-line persistence (state survives on the persistent `Vm`/`Interp`), LSP hover
  round-trips identically under the final landed layout (Stage 1, and Stage 2 if it landed).
- [ ] Green both configs. **Review:** confirm no tooling output changed. Commit.

## Task 13 — Internal docs (CLAUDE.md, design spec, roadmap, bench report) — NO surface docs
**Files:** `CLAUDE.md` ("Values" paragraph), the main design spec (perf section),
`superpowers/roadmap.md`, `bench/` report. **Tests:** none (doc-only).
- [ ] **State the Gate 9/11/13 N/A justification explicitly** (spec §6): VAL adds **zero** `examples/**`,
  zero grammar/tree-sitter, zero `docs/content/**` pages, zero `NAV` entries — a reviewer must read the
  ABSENCE as correct for a pure-representation change, not a gap. Update only the internal touch-points:
  `CLAUDE.md` "Values" (the compact encoding + the **decode-then-operate** invariant + the native-handle
  reinforcement + the `assert_not_impl_any!(Value: Send)` lock + which stages landed/deferred); the design
  spec's perf section with the `bench/` numbers; the roadmap entry (incl. the Task-5 Stage-2 proceed/stop
  verdict). Append-only; **NAV unchanged**.
- [ ] **Review:** reviewer confirms the deferral (if any) is owner-noted, not silent (Gate 6), and that no
  surface doc was added. Commit.

---

## Done when
Stage 1 is merged green on its own (32→16, scalars inline, two fat variants boxed, `Decimal` behind a
pointer, no new ownership `unsafe`); the Task-5 gate decision is **recorded** (owner-noted) and either
Stage 2 (→8 B, NaN-box, hand-written `unsafe` ownership layer, GC-soundness suite green) is merged or it is
a documented stopping point at 16 B; Stages 3–4 land if pursued, each a green merge-able point. The
**four-mode byte-identity** (tree-walker == specialized-VM == generic-VM == `.aso`) holds in BOTH feature
configs **per stage** with **zero new goldens** (no behavior change); the §7.2 SMI↔boxed boundary test and
the encoding round-trip property test pass on both engines; the V13-T4/T6 GC-soundness gates re-run green on
the landed layout; `static_assertions` locks `!Send`/`!Sync`; clippy + tests green both configs; the
`bench/` report substantiates `size_of::<Value>()` 32→16(→8) and **no regression in EITHER VM mode**
(Gate 12). Gates 9/11/13 are N/A by design and that absence is documented. Merge `--no-ff` to `main` per
stage (each stage is an independent merge).
