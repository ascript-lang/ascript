# NANB — Compact Boxed `Value` (Seam → 16-Byte Two-Word → A/B Verdict) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges (code quality + spec adherence) before
> acceptance. At the end of each phase, a **holistic per-phase review subagent** reviews the
> phase's combined changes before the next phase starts. A task/phase is closed only when every
> box under it is ticked.

**Goal:** Make the `Value` representation swappable behind a sealed API seam (`ValueKind` view +
constructors + private repr — Phase 1, merges unconditionally), implement the 16-byte two-word
layout behind it (`ThinStr` thins the two fat `Rc<str>` payloads; full inline i64; raw f64; no
ownership `unsafe` — Phase 2, bring-up feature `value16`), prove it with the cross-binary
differential + deep fuzz campaign + Miri + property suite (Phase 3), and **ship ONLY on the spec
§8.1 measured win** — otherwise record the rejection in `bench/NANB_RESULTS.md` + `goal-perf.md`
exactly as VAL did (Phase 4, both outcomes are first-class). The 8-byte NaN-box stays a designed,
double-gated follow-up (spec §3.2) — NOT implemented here.

**Architecture:** `pub struct Value(ValueRepr)` with `ValueRepr` private to `src/value.rs`;
consumers read via `#[inline(always)] fn kind(&self) -> ValueKind<'_>` (borrowed view, variant
set mirrors the repr 1:1) / `fn into_kind(self) -> OwnedKind` (consuming matches) and construct
via total constructor coverage (`Value::int/float/str/...`). `pub type AStr` aliases the string
payload (`Rc<str>` → `ThinStr` under `value16`); `MapKey::Str(AStr)` tracks it (semantics
pinned by property test). `impl Trace for Value` moves beside the repr into `value.rs`; container
`Trace` impls, `cc_addr` identity, the `!Send` asserts, `SharedNode`, serializer wire tags, and
`ASO_FORMAT_VERSION` (27 at drafting — asserted unchanged vs the merge-base, never a literal;
DEFER bumps to 28 in a parallel branch) are all untouched. Spec:
`superpowers/specs/2026-06-12-nan-boxing-design.md` — read it FIRST; every mechanism, candidate
layout, invariant, and rejection is there.

**Tech stack:** Rust, single binary `ascript`; `src/value.rs` (the seam + repr + `ThinStr`),
`src/gc.rs` (Trace relocation), the 103-file mechanical migration (`src/interp.rs`,
`src/vm/run.rs`, `src/stdlib/**`, `src/worker/serialize.rs`, `src/compile/`, `src/vm/aso.rs`);
`static_assertions` (kept); proptest (`tests/property.rs`); `cargo +nightly miri` (the `ThinStr`
unsafe core); the four-mode differential (`tests/vm_differential.rs`) + the cross-binary
old-vs-new-repr corpus diff; `tests/vm_bench.rs` (Gate 12/17 + `dbg_zero_cost_gate`);
`bench/run_compact_value_bench.sh` protocol → `bench/run_nanb_bench.sh` + `bench/NANB_RESULTS.md`
(Gates 16/18); `FUZZ_STRESS_N` stress + `fuzz/fuzz_targets/*` (the FUZZ ~284k-case precedent).

**Binding execution standards (production-grade mandate, `goal.md` Gates 1–14 + `goal-perf.md`
Gates 15–18):** TDD per task (failing test → minimal code → green → commit, house trailer
`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`); any bug found en route —
ours or pre-existing — is fixed in-branch with a failing-test-first regression guard, never
deferred; clippy clean + tests green in BOTH feature configs at every phase close (and in the
`value16` matrix during Phases 2–3); the tree-walker is never relaxed; no placeholder/TODO on a
reachable path. Branches: **Phase 1 on `feat/value-seam` off `main`, merged `--no-ff` on its own
gates**; Phases 2–4 on `feat/value16` off the post-seam `main`, merged ONLY on the Phase-4 SHIP
verdict (on REJECT the branch is left flagged, the VAL `c1571ec` precedent).

---

## File structure

**New files:**
- `src/value/thin_str.rs` (or a `thin_str` module inside `value.rs`) — the single-allocation
  thin string (Phase 2; the only new `unsafe`).
- `bench/run_nanb_bench.sh` — interleaved same-session A/B runner (mirrors
  `bench/run_compact_value_bench.sh`).
- `bench/nanb_bench.as` — extends `bench/compact_value_bench.as` with the LANE Task-0
  functional/call workloads if not already covered there.
- `bench/NANB_RESULTS.md` — the A/B + RSS/alloc report AND the recorded verdict (ship or
  reject — written in both outcomes).
- `scripts/nanb-cross-repr-diff.sh` — corpus runner diffing baseline-repr vs `value16` binaries
  byte-for-byte (Phase 3; may live under `bench/`).

**Modified files:**
- `src/value.rs` — `ValueKind`/`OwnedKind`/constructors/`AStr`; the seal
  (`pub struct Value(ValueRepr)`); the relocated `impl Trace for Value`; the staged size
  assertion; Phase 2: the `value16` repr + `MapKey::Str(AStr)`.
- `src/gc.rs` — Trace-for-Value moves out (cross-reference comment stays); container impls
  untouched.
- The migration surface (mechanical, Phase 1): `src/interp.rs` (675 `Value::` refs),
  `src/vm/run.rs` (435), `src/stdlib/schema.rs` (408), `src/stdlib/*` (the rest),
  `src/worker/serialize.rs` (138), `src/compile/mod.rs` (90), `src/vm/{aso,verify,adapt,ic,fiber}.rs`,
  `src/{coro,task,env,repl,lsp/…}.rs` — every file `rg -l 'Value::'` reports (103 today).
- `Cargo.toml` — the `value16` bring-up feature (Phase 2; REMOVED at Phase 4 either way).
- `tests/property.rs` — repr round-trip/identity/NaN/ThinStr/MapKey properties + saboteur.
- `tests/vm_differential.rs` — no relaxation; the corpus is the proof at every step.
- `tests/aso.rs` (or sibling) — negative-space guard (`ASO_FORMAT_VERSION` unchanged vs
  merge-base, golden byte-compare vs the merge-base).
- `CLAUDE.md` (the "Values" paragraph), `superpowers/roadmap.md`, `goal-perf.md` (status +
  verdict), the spec itself (the Phase-4 verdict is appended to it, the VAL §3.3 precedent).

---

## Phase 1 — the API seam (pure refactor, zero behavior change, merges on its own)

> Blast radius is measured (spec §4.1): 5,437 `Value::` tokens / 103 files / 893 pattern-arm
> lines / 106 `if let` / 155 `matches!` / ~3,405 scalar constructions. The phase is sequenced so
> the compiler does the bookkeeping: introduce the view (1.1), migrate area by area with the full
> differential after every task (1.3–1.6), then SEAL the repr (1.7) so the compiler proves zero
> stragglers. Zero behavior change is the gate at every step.

### Task 1.1: `ValueKind<'_>` + `OwnedKind` + total constructors + `AStr` (repr unchanged)

**Files:**
- Modify: `src/value.rs`
- Test: inline `#[test]`s in `src/value.rs`

- [x] **Step 1: Write the failing tests** (view totality + round-trip + zero-divergence with
  direct matching):

```rust
#[test]
fn value_kind_view_is_total_and_faithful() {
    // One value of EVERY variant; kind() must report the same logical kind and
    // borrow the same payload (pointer-identical for handles, bit-equal for scalars).
    let arr = Value::array(vec![Value::int(1)]);
    match arr.kind() {
        // The view BORROWS the same handle: pointer-identical to the accessor's clone.
        ValueKind::Array(a) => {
            let cell = arr.as_array().expect("array accessor");
            assert!(crate::gc::cc_ptr_eq(a, &cell));
        }
        other => panic!("wrong kind: {other:?}"),
    }
    assert!(matches!(Value::int(7).kind(), ValueKind::Int(7)));
    assert!(matches!(Value::float(2.5).kind(), ValueKind::Float(f) if f == 2.5));
    assert!(matches!(Value::str("hi").kind(), ValueKind::Str(s) if &**s == "hi"));
    assert!(matches!(Value::nil().kind(), ValueKind::Nil));
    // NaN bit pattern preserved through construct→kind (the §7.5 seed property).
    let weird_nan = f64::from_bits(0x7FF0_0000_0000_0001);
    assert!(matches!(Value::float(weird_nan).kind(),
        ValueKind::Float(f) if f.to_bits() == weird_nan.to_bits()));
}

#[test]
fn owned_kind_moves_without_refcount_change() {
    let v = Value::str("payload");
    let before = /* Rc strong count via a test-only probe */ v.str_strong_count();
    match v.into_kind() {
        OwnedKind::Str(s) => assert_eq!(&*s, "payload"),
        _ => unreachable!(),
    } // moved, not cloned: count returned to `before` after drop
    let _ = before;
}
```

- [x] **Step 2: Run — expect FAIL** (`kind`/`into_kind`/constructors don't exist):
  `cargo test -p ascript value_kind_view`
- [x] **Step 3: Implement** in `src/value.rs`:
  - `pub type AStr = Rc<str>;` with a doc comment naming it the string-payload seam (spec §3.1.2).
  - `pub enum ValueKind<'a>` exactly as spec §4.2 (29 variants, `Regex` cfg-gated, scalars
    by-value `Copy`, handles `&'a`), `#[derive(Debug)]` where payloads allow (hand-write `Debug`
    otherwise — `Value`'s own Debug exists at `:1487` as the model).
  - `pub enum OwnedKind` — the by-value mirror (payloads moved).
  - `#[inline(always)] pub fn kind(&self) -> ValueKind<'_>` and
    `#[inline(always)] pub fn into_kind(self) -> OwnedKind` — today, trivial re-projections of
    the enum.
  - **Total constructor coverage**, extending the VAL Task-0 set (`value.rs:1283-1326`):
    `nil() bool_(b) int(i) float(f) decimal(d) str(impl Into<AStr>) builtin(...) function(rc)
    closure(cc) array(Vec<Value>) object(IndexMap) map(...) set(...) bytes(...) regex(...)
    native(...) native_method(...) enum_(...) enum_variant(...) class(...) interface(...)
    instance(...) bound_method(...) super_(...) future(...) generator(...) generator_method(...)
    class_method(...) shared(arc)` — each `#[inline]`, each a one-line wrap. Plus the borrowed
    extractors the migration needs (`as_str() -> Option<&str>`, `as_array()`, `as_bytes()`, …)
    where they don't already exist.
- [x] **Step 4: Run — expect PASS**; `cargo test -p ascript` (lib tests) green; clippy clean.
- [x] **Step 5: Commit** — `git commit -m "feat(value): ValueKind/OwnedKind view + total constructors + AStr seam (repr unchanged)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 1.2: relocate `impl Trace for Value` into `value.rs` (zero behavior)

**Files:** `src/gc.rs` (`:193-246` moves out), `src/value.rs` (receives it)

- [x] **Step 1:** Move the impl verbatim (typed arms: containers recurse, `EnumVariant` deref-
  traces per ADT `gc.rs:210-217`, `Shared` explicit no-op, catch-all no-op). Leave a
  cross-reference comment at the old site: container `Trace` impls + `cc_addr`/`cc_ptr_eq` STAY
  in `gc.rs` (they take `&self`, never a `Value` word — spec §4.2). Rationale comment: the impl
  must see the private repr after Task 1.7's seal.
- [x] **Step 2:** The existing GC suites are the test: `cargo test -p ascript gc` + the V13
  soundness/soak/Drop gates green, both feature configs.
- [x] **Step 3: Commit** — `git commit -m "refactor(gc): move Trace-for-Value beside the (soon-private) repr in value.rs" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 1.3: migrate the VM (`src/vm/**`) to the view

**Files:** `src/vm/run.rs` (435 refs — the hot one), `src/vm/{adapt,ic,fiber,aso,verify}.rs`

- [x] **Step 1:** Mechanical migration by the spec §4.3 rules. The two load-bearing shapes:
  - the adaptive guard (`run.rs:5318`):
    `match (kind, &a, &b) { (ArithKind::Int, Value::Int(x), Value::Int(y)) => int_binop(op, *x, *y, span), … }`
    → `match (kind, a.kind(), b.kind()) { (ArithKind::Int, ValueKind::Int(x), ValueKind::Int(y)) => int_binop(op, x, y, span), … }`
  - consuming pops (`fiber.pop()` then `match v {` taking payload ownership) → `v.into_kind()`
    with `OwnedKind::` arms — NO new clones on any path that didn't clone before (reviewer
    audits this specifically).
  - `aso.rs` `write_value`/`read_value` (`:789/:871`): pattern arms → `kind()`, constructions →
    constructors. Wire bytes untouched by construction.
- [x] **Step 2:** Full `cargo test --test vm_differential` (both feature configs) — byte-identical;
  `cargo test --test aso` green (round-trip bytes unchanged).
- [x] **Step 3: Commit** — `git commit -m "refactor(vm): route Value matches through ValueKind/OwnedKind (mechanical, behavior-identical)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 1.4: migrate the tree-walker (`src/interp.rs`, 675 refs)

- [x] **Step 1:** Same rules; the big tuple-match `apply_binop` (`interp.rs:6924` area) and
  `type_name` (`:7763`) migrate to `kind()` pairs. The oracle's SEMANTICS must not move: the
  diff is mechanically `Value::X(p)` → `ValueKind::X(p)` + scrutinee `.kind()` + constructor
  renames — the reviewer reads the diff for any non-mechanical line.
- [x] **Step 2:** `cargo test --test vm_differential` both configs; `cargo test` default config.
- [x] **Step 3: Commit** — `git commit -m "refactor(interp): ValueKind migration (oracle semantics untouched)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 1.5: migrate the stdlib + serializers (`src/stdlib/**`, `src/worker/serialize.rs`)

- [x] **Step 1:** File-by-file (schema.rs 408 → ffi.rs 163 → net_http/object/fs/math/json/… ),
  then `src/worker/serialize.rs` (`encode_value`'s kind-tag walk + `non_sendable`'s
  classification match `:124-172`). The wire TAG_* constants and byte layout are untouched — the
  worker round-trip property suite is the proof.
- [x] **Step 2:** `cargo test` AND `cargo test --no-default-features` green (the cfg-gated
  modules — regex/tui/ai/telemetry arms — compile in both).
- [x] **Step 3: Commit** — `git commit -m "refactor(stdlib,worker): ValueKind migration (wire bytes byte-identical)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 1.6: migrate the remainder + sweep to zero

**Files:** `src/compile/mod.rs`, `src/coro.rs`, `src/task.rs`, `src/env.rs`, `src/repl.rs`,
`src/lsp/**`, `src/fuzzgen/**`, `src/det.rs` (expected no-op — it is `Value`-free), tests with
direct `Value::` construction.

- [x] **Step 1:** Migrate everything remaining; finish with the sweep:
  `rg -n 'Value::[A-Z]' src/ --glob '!src/value.rs'` → **zero hits** (constructors are lowercase;
  any uppercase variant path outside `value.rs` is a straggler).
- [x] **Step 2:** Full suite both configs green.
- [x] **Step 3: Commit** — `git commit -m "refactor(core): complete the ValueKind migration — zero variant paths outside value.rs" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 1.7: SEAL the representation — `pub struct Value(ValueRepr)`

**Files:** `src/value.rs`

- [x] **Step 1: Write the failing probe** — a `compile_fail` doc-test (or `trybuild`-style
  comment test) pinning that `Value::Int` is not nameable outside `value.rs`; plus keep
  `value_size_is_documented` asserting **24** (the seal must not change layout: a newtype over
  the enum is layout-identical).
- [x] **Step 2: Implement:** rename the enum to `enum ValueRepr` (private, NOT `pub(crate)` —
  module-private is the compiler-enforced seal, spec §4.2), wrap as
  `#[derive(Clone)] pub struct Value(ValueRepr);`. Repr-private impls (`PartialEq`, `Debug`,
  `Display`, `is_truthy`, `as_f64`/`as_int_exact`, `MapKey::from_value`, `frozen_kind`/
  `freeze_value`/`is_frozen_value`, `Trace`) match `self.0`. The `static_assertions::
  assert_not_impl_any!(Value: Send, Sync)` (`:1203`) stays verbatim.
- [x] **Step 3:** Build — **the compiler now finds every straggler the sweep missed**; fix all
  (mechanically, same rules). Full suite + clippy, BOTH configs.
- [x] **Step 4: Commit** — `git commit -m "feat(value): seal the representation — Value is a struct over a private ValueRepr" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 1.8: the Phase-1 zero-cost proof (Gate 12/17 + DBG)

**Files:** none (measurement) / `tests/vm_bench.rs` re-run

- [x] **Step 1: Same-session sanity A/B** — build `main` and `feat/value-seam`
  (`--profile profiling`), run the `bench/compact_value_bench.as` HOT set + `bench/profiling/`
  workloads interleaved, 5 reps, both VM modes: **geomean must be ≈1.00× (within ±1%)**. The
  enum-view-inlines-away claim is verified here, not assumed (spec §8.2). If it regresses: the
  view/inlining is wrong — fix (`#[inline(always)]` audit, check a non-inlined `kind()` in the
  arith path via the profiler), never accept.
- [x] **Step 2:** `cargo test --test vm_bench` — spec/tw geomean ≥2× (Gate 17) AND
  `dbg_zero_cost_gate` green (dispatch-arm text was touched → the DBG re-run rule applies).
- [x] **Step 3:** Record the numbers in the commit body (they also seed `bench/NANB_RESULTS.md`'s
  Phase-1 section). **Commit** — `git commit -m "bench(value): Phase-1 seam zero-cost proof (geomean ~1.00x both modes; dbg gate green)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 1.9: Phase 1 holistic review + independent merge

- [x] **Step 1:** Holistic subagent over the ENTIRE seam diff: zero behavior change (full
  four-mode differential both configs); zero non-mechanical lines in `interp.rs`/`run.rs` (read
  the diff); no new clone on a previously clone-free path (spot-audit `into_kind` call sites);
  the seal genuinely seals (`rg 'ValueRepr' src/ --glob '!src/value.rs'` → zero); `Trace`
  relocation preserved every arm; both clippy configs clean; the 1.8 numbers honest.
- [x] **Step 2:** Findings fixed before close. Merge `feat/value-seam` → `main` `--no-ff`
  (message: the seam is hygiene that stands regardless of the Phase-4 verdict — spec §0). Phase 2
  branches from the post-merge `main`.

---

## Phase 2 — the 16-byte repr behind the seam (branch `feat/value16`, feature `value16` default-off)

### Task 2.1: `ThinStr` — the unsafe core, TDD + Miri

**Files:**
- Create: `src/value/thin_str.rs`
- Test: inline `#[test]`s + `tests/property.rs` additions

- [ ] **Step 1: Write the failing tests FIRST** (they define the contract):

```rust
#[test]
fn thin_str_is_one_word_and_not_send() {
    assert_eq!(std::mem::size_of::<ThinStr>(), 8);
    assert_eq!(std::mem::size_of::<Option<ThinStr>>(), 8); // NonNull niche
    static_assertions::assert_not_impl_any!(ThinStr: Send, Sync);
}

#[test]
fn thin_str_round_trip_clone_drop_balance() {
    for s in ["", "a", "hé", "x".repeat(4096).as_str()] {
        let t = ThinStr::from(s);
        assert_eq!(&*t, s);
        assert_eq!(t.len(), s.len());
        let c = t.clone();
        assert_eq!(t.strong_count(), 2);
        drop(c);
        assert_eq!(t.strong_count(), 1);
        // Hash/Eq/Ord agree with str content (the Rc<str> parity contract).
        assert_eq!(hash_of(&t), hash_of(&s));
    }
}
```

  proptest (in `tests/property.rs`): `∀ s: String` — `ThinStr::from(&s)` derefs to `s`,
  clone/drop N times leaves count balanced, `Display`/`Debug`/`Eq`/`Ord`/`Hash` agree with
  `Rc<str>::from(&s)`.
- [ ] **Step 2: Run — expect FAIL**; **implement** per spec §3.1.1: `#[repr(C)] StrHeader
  { strong: Cell<usize>, len: usize }` + bytes in ONE `std::alloc` allocation
  (`Layout::new::<StrHeader>().extend(Layout::array::<u8>(len))`, padded-to-align dealloc with
  the SAME layout); `Clone` bumps with the `Rc`-style `isize::MAX` abort guard; `Drop` decrements
  and deallocates at zero; `Deref<Target = str>` via `slice::from_raw_parts` +
  `str::from_utf8_unchecked` (UTF-8 by construction — the only constructors take `&str`/`String`);
  `PhantomData<Rc<()>>` for `!Send`/`!Sync`; `From<&str>`, `From<String>`, `From<Rc<str>>`,
  `Display`, `Debug`, `Eq/Ord/Hash` by content. Doc-comment the layout and every safety
  obligation per `unsafe` block (the DBG `Code`-newtype documentation standard).
- [ ] **Step 3: Miri (the stated bar for new unsafe):**
  `cargo +nightly miri test -p ascript thin_str` — clean (alloc/dealloc pairing, no UB in deref,
  zero-len edge, clone/drop balance). Record the command + output summary in the commit body
  (the `src/vm/chunk.rs:1020` precedent).
- [ ] **Step 4:** `cargo test -p ascript thin_str` + the proptest battery green.
- [ ] **Step 5: Commit** — `git commit -m "feat(value): ThinStr — single-allocation header-length thin string (Miri-clean)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 2.2: flip the repr under `value16` — `AStr = ThinStr`, `MapKey::Str(AStr)`, size 16

**Files:** `src/value.rs`, `Cargo.toml` (feature `value16 = []`), `src/worker/serialize.rs`
(conversion sites the compiler surfaces), boundary files per audit

- [ ] **Step 1: Write the failing test** — the staged size assertion (the VAL tripwire style,
  `value.rs:1791`):

```rust
#[test]
fn value_size_is_documented() {
    // NANB: 24 (fat Rc<str> payloads) → 16 (ThinStr; round_up(8) + 8-byte tag).
    // 8 bytes is the gated NaN-box follow-up (spec §3.2) — NOT this layout.
    #[cfg(feature = "value16")]
    assert_eq!(std::mem::size_of::<Value>(), 16);
    #[cfg(not(feature = "value16"))]
    assert_eq!(std::mem::size_of::<Value>(), 24);
}
```

- [ ] **Step 2: Implement:**
  `#[cfg(feature = "value16")] pub type AStr = ThinStr;` / `#[cfg(not(...))] pub type AStr = Rc<str>;`
  — flip `ValueRepr::Str(AStr)` / `Builtin(AStr)` and `MapKey::Str(AStr)` (`MapKey::from_value`
  `:236` stays `s.clone()` — now a count bump in BOTH configs; `SharedKey::from_map_key`/
  `to_map_key` `:925-946` already copy text, only their source type changes). Let the compiler
  surface every `Rc<str>` ↔ `AStr` boundary; route each through `AStr::from`/`Rc::from(&*s)`.
- [ ] **Step 3: The boundary-conversion audit (spec §3.1.2 deliverable):** list every surfaced
  conversion site, classify hot/cold (anything in `run.rs` dispatch, `apply_binop`, member-read,
  or a stdlib per-element loop is hot), and ELIMINATE hot ones (carry `AStr` through, or take
  `&str`). Record the audit table in the commit body. A hot-path copy left standing is a blocker,
  not a note.
- [ ] **Step 4:** Green in the **4-way matrix**: `cargo test` / `cargo test --features value16` /
  `cargo test --no-default-features` / `cargo test --no-default-features --features value16`;
  clippy clean in all four.
- [ ] **Step 5: Commit** — `git commit -m "feat(value): 16-byte two-word Value behind cfg(value16) — ThinStr payloads, MapKey aligned" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 2.3: the representation property suite + saboteur

**Files:** `tests/property.rs`, `tests/vm_differential.rs` (focused battery), `tests/aso.rs`

- [ ] **Step 1:** Write (run under BOTH reprs in CI):
  - **Round-trip totality:** for a generator covering every kind (incl. nested
    containers/EnumVariant payloads/Shared) — `construct → kind() → reconstruct` is logically
    equal, `type_name` identical, identity (`cc_addr`) stable through clone, refcounts balanced
    after drop.
  - **Float bits verbatim:** ∀ u64 bit patterns (proptest + the explicit set: ±0.0, ±inf,
    quiet/signaling NaN payloads, `0x7FF8…0001`, denormals) — `Value::float(f).kind()` returns
    bit-identical `f64` (the two-word layout's no-NaN-games guarantee, spec §3.1); Display/`==`/
    `MapKey` canonicalization unchanged (NaN → one key, `value.rs:228-233`).
  - **Int extremes:** `i64::MIN/MAX` + the SMI-boundary battery values (`value.rs:1830` — must
    stay trivially green; they become live under a future Candidate A).
  - **`MapKey` cross-payload pin:** the same string content keys identically under `Rc<str>` and
    `ThinStr` payloads (hash/eq/fold).
  - **Serializer byte-stability:** a corpus of values `encode_value`s (worker wire) and
    `write_value`s (`.aso` pool) to **byte-identical** output under both reprs.
  - **Saboteur self-test** (the FUZZ precedent): a deliberately-wrong `ThinStr` clone (count not
    bumped, behind `#[cfg(test)]` injection) must make the balance property FAIL — proving the
    harness can catch the bug class.
- [ ] **Step 2:** Negative space: assert `ASO_FORMAT_VERSION` equals a `const ASO_AT_MERGE_BASE`
  recorded from this branch's merge-base at branch time (27 at drafting — never a literal
  campaign-wide pin; NANB runs PARALLEL to DEFER, which bumps to 28; re-record on rebase) +
  golden `.aso` byte-compare
  of a representative multi-feature compile against the merge-base (the SHAPE Task-5.3 precedent).
- [ ] **Step 3:** All green in the 4-way matrix. **Commit** —
  `git commit -m "test(value): repr property suite — round-trip/NaN-verbatim/identity/MapKey/serializer-bytes + saboteur" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 2.4: Phase 2 holistic review

- [ ] **Step 1:** Holistic subagent: the `value16` diff outside `value.rs`/`thin_str.rs` is ONLY
  compiler-forced conversion sites (the seam held — anything else means Phase 1 missed a
  straggler: fix the seam, not the site); the audit table is complete and hot-path-clean; Miri
  output attached; the 4-way matrix green; `assert_not_impl_any!(Value: Send, Sync)` and the
  `SharedNode: Send+Sync` assert both still compile-time-enforced; no borrow-across-await
  introduced (clippy `await_holding_refcell_ref` clean).
- [ ] **Step 2:** Findings fixed before Phase 3.

---

## Phase 3 — the evidence: cross-repr differential, deep fuzz, the same-session A/B

### Task 3.1: cross-binary old-vs-new repr differential

**Files:** create `scripts/nanb-cross-repr-diff.sh`

- [ ] **Step 1:** Script: build `target/release-baseline/ascript` (no feature) and
  `target/release-value16/ascript` (`--features value16`) from the SAME commit; run every
  `examples/**` program (minus `EXAMPLE_SKIPS`) + the goldens on both; `diff` stdout/stderr/exit
  code **byte-for-byte**. This is the oracle the within-binary differential cannot provide (both
  engines share the repr — spec §0 "Engines").
- [ ] **Step 2:** Run — zero diffs. Also run the full `cargo test --test vm_differential` under
  `--features value16` AND `--no-default-features --features value16` (four-mode, new repr).
- [ ] **Step 3: Commit** — `git commit -m "test(nanb): cross-binary old-vs-new repr corpus differential — byte-identical" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 3.2: the deep fuzz campaign (the FUZZ ~284k-case bar)

- [ ] **Step 1:** The stress differential under the new repr (exact command, recorded with its
  output tail in the commit body):

```bash
FUZZ_STRESS_N=300000 cargo test --features value16 --test property \
  stress_differential_many_seeds -- --ignored --nocapture
```

  Zero divergences required (the bar set by FUZZ merge `9b202eb`: ~284,000 generated programs,
  zero divergence). Any divergence is a representation bug: minimize (proptest shrinks), fix
  in-branch failing-test-first, re-run the FULL campaign from scratch.
- [ ] **Step 2:** Where nightly+cargo-fuzz is available, time-boxed libFuzzer runs on the branch:

```bash
cargo +nightly fuzz run differential      -- -runs=200000
cargo +nightly fuzz run worker_serialize  -- -runs=200000
cargo +nightly fuzz run aso_roundtrip     -- -runs=200000
```

  (the airlock and `.aso` reader sit directly on the migrated walkers). Zero crashes/divergences.
- [ ] **Step 3: Commit** — `git commit -m "test(nanb): deep fuzz campaign on value16 — 300k stress + libFuzzer targets, zero divergence" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 3.3: the same-session A/B + RSS + allocation report (Gates 16/18)

**Files:** create `bench/run_nanb_bench.sh`, `bench/nanb_bench.as`, `bench/NANB_RESULTS.md`

- [ ] **Step 1:** `bench/run_nanb_bench.sh` mirrors `run_compact_value_bench.sh` (interleaved
  round-robin, per-cell median, both binaries from one session on one machine): workloads =
  the `compact_value_bench.as` HOT set + cold checks (spec §8.2) + `bench/profiling/`
  (`json_roundtrip`, `object_churn`, `async_inline`, `async_concurrent`) + the LANE Task-0
  functional/call corpus; **columns per workload: base-spec / v16-spec / base-gen / v16-gen /
  base-tw / v16-tw**; ≥5 reps.
- [ ] **Step 2:** Memory: `/usr/bin/time -l` peak RSS per workload per binary (data-heavy set
  highlighted); allocation counts via the bench harness counter / profiler attribution
  (`ascript run --profile cpu` on `json_roundtrip` + `membound_strings`, capturing the
  string-access and alloc attribution deltas).
- [ ] **Step 3:** Write `bench/NANB_RESULTS.md` in the `COMPACT_VALUE_RESULTS.md` format:
  structural fact (`size_of` 24 vs 16, asserted by both binaries' own tests), the full table,
  ALL-HOT / SCALAR / STRING subset geomeans **in both VM modes** + the tree-walker, the RSS/alloc
  tables, cold-path checks, honest-reading notes (scalar deltas = machine drift unless `Str` is
  on the path), and an **explicit line-by-line verdict against every spec §8.1 criterion**.
  Expectations were stated; results are measured; a disappointing number is reported as such.
- [ ] **Step 4:** Gate 12/17 re-run under `value16`: `cargo test --features value16 --test
  vm_bench` — spec/tw ≥2× holds; `dbg_zero_cost_gate` green.
- [ ] **Step 5: Commit** — `git commit -m "bench(nanb): same-session A/B + RSS/alloc report (Gates 16/18) — verdict input" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 3.4: Phase 3 holistic review (the evidence audit)

- [ ] **Step 1:** Holistic subagent re-runs spot checks: re-executes one bench round and confirms
  the report's medians are reproducible in direction; verifies the fuzz campaign logs (N actually
  reached, zero-divergence claim backed by output); verifies the cross-repr diff covered the
  WHOLE corpus (count the programs); confirms no §8.1 criterion was reworded to fit the data.
- [ ] **Step 2:** Findings fixed; only then does Phase 4 open.

---

## Phase 4 — the verdict (both outcomes are first-class; exactly one is executed)

### Task 4.1: judge against spec §8.1 and execute the matching path

- [ ] **Step 1:** The reviewer-of-record (independent subagent, not the implementer) reads
  `bench/NANB_RESULTS.md` and renders KEEP or STOP strictly against §8.1 — geomean ≥1.02×
  spec / ≥1.00× gen, no workload <0.97×, STRING geomean ≥0.99× both modes, RSS ≥5% on the
  data-heavy set, tree-walker ≥0.97×, all correctness green. **The criteria were fixed before
  measurement; they are not negotiable now.** Append the verdict to the spec (the VAL §3.3
  GATE-VERDICT precedent) and to `bench/NANB_RESULTS.md`.

- [ ] **Step 2 — PATH A: SHIP (all criteria met):**
  - Remove the `value16` feature: the 16-byte repr becomes unconditional; delete the
    `Rc<str>`-payload cfg arms; `value_size_is_documented` pins **16** unconditionally (update
    the staged doc comment: 32 → 24 [VAL] → 16 [NANB] → 8 [NaN-box, double-gated future]).
  - Re-run the FULL gates checklist (Task 4.2) on the de-cfg'd tree.
  - Docs/status: `CLAUDE.md` "Values" paragraph (16-byte two-word `Value`, the seam, `ThinStr`,
    the decode-then-operate invariant restated, native handles still untraced); `goal-perf.md`
    NANB row → ✅ with the headline numbers + a note that the JIT §1.1 precondition 2 is now MET;
    `superpowers/roadmap.md` entry; NO user-docs page and NO examples (pure representation — the
    VAL Gate-9 N/A justification, restated in the PR).
  - Merge `feat/value16` → `main` `--no-ff`.
  - **Commit** — `git commit -m "feat(value): ship the 16-byte two-word Value (NANB) — measured win, gates green" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

- [ ] **Step 2 — PATH B: RECORD-REJECT (any criterion missed):**
  - Do NOT merge Phases 2–3. Leave `feat/value16` with a final commit pinning the branch state
    and pointing at the evidence (the VAL `c1571ec` flagged-sub-state precedent).
  - `bench/NANB_RESULTS.md` (committed to `main` via a small docs PR): the STOP verdict in the
    COMPACT_VALUE_RESULTS KEEP-or-STOP format — which criterion failed, by how much, and the
    diagnosis (e.g. the `len`-load on string reads did/didn't amortize).
  - `goal-perf.md`: NANB row → **evidence-rejected** with the numbers; annotate the JIT entry —
    precondition 2 (≤16 B) remains UNMET at 24 B, the JIT stays deferred unless its own
    re-profile overrides; annotate REGION ("depends on NANB" → representation is final at 24 B).
  - Spec + `CLAUDE.md` one-liner (the Values paragraph notes the recorded rejection so it is
    never re-litigated without new evidence); `superpowers/roadmap.md` entry.
  - Phase 1's seam REMAINS on `main` (already merged at 1.9) — the permanent hygiene win and the
    cheap re-run path if hardware or a future ThinStr-SSO variant changes the calculus.
  - **Commit** (docs PR) — `git commit -m "docs(nanb): record the evidence-rejected verdict (the VAL precedent honored)" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"`

### Task 4.2: full gates checklist + whole-effort holistic review (Definition of Done)

> Run in full on PATH A's de-cfg'd tree; on PATH B run the `main`-applicable subset (Phase-1 seam
> + docs) — the branch evidence is already frozen.

- [ ] **Step 1:** `cargo test` (default) — ALL test binaries green.
- [ ] **Step 2:** `cargo test --no-default-features` — green.
- [ ] **Step 3:** `cargo clippy --all-targets` AND `cargo clippy --no-default-features
  --all-targets` — clean.
- [ ] **Step 4:** `cargo test --test vm_differential` both configs — full corpus + goldens,
  four-mode byte-identical; the cross-repr diff (3.1) archived in the PR.
- [ ] **Step 5:** Property suite + saboteur green; the fuzz campaign log archived; Miri output
  for `thin_str` archived; negative space green (`ASO_FORMAT_VERSION` unchanged vs merge-base,
  golden `.aso` byte-compare).
- [ ] **Step 6:** Gate 12/17: spec/tw geomean ≥2× recorded; `dbg_zero_cost_gate` recorded;
  Gate 16/18: `bench/NANB_RESULTS.md` complete (same-session, RSS, alloc counts).
- [ ] **Step 7:** Tooling parity confirmed-working (no surface change → no grammar regen / editor
  pins; `fmt` idempotence suite green; REPL session sanity — values across lines; LSP suite
  green; `examples/**` re-run unchanged — zero new examples, justified per VAL Gate-9 N/A).
- [ ] **Step 8:** Whole-effort holistic-review subagent over the entire branch diff: spec §0–§10
  coverage table; zero TODO/placeholder/silent deferral; every bug found en route has a
  failing-test-first guard; invariants intact (`Value: !Send+!Sync` assert, `SharedNode:
  Send+Sync` assert, native handles untraced, no borrow across await, `MapKey` canonicalization
  byte-identical, serializer wire bytes unchanged); the verdict (either path) recorded in spec +
  goal-perf + bench.
- [ ] **Step 9:** Every checkbox in this plan ticked. Update `goal-perf.md`'s status table in the
  closing merge/PR. REGION is now unblocked (representation final — at 16 or, recorded, at 24).

---

## Self-review (author pass)

- **Spec coverage:** §0 gate → the phase structure itself + Task 4.1's fixed criteria; §2 audit →
  grounded in the Task-2.2 size law; §3.1 Candidate B → Tasks 2.1–2.3 (ThinStr bit-exact,
  AStr/MapKey alignment, no-NaN-games property); §3.2 Candidate A → deliberately NOT implemented
  (the double gate is recorded in the spec; the SMI-boundary battery stays inert-green); §3.3
  recommendation → Phase 2 is (b); §4 seam → Phase 1 (survey numbers drive the task split; the
  seal is compiler-enforced); §5 invariants → Tasks 1.2/2.2/2.3 + the 4.2 checklist; §6 bring-up
  feature + Gate-15 deviation → Task 2.2 (feature) + 4.1 (removal on either path); §7 correctness
  → Tasks 3.1/3.2/2.3 (+ Miri 2.1); §8 performance → Tasks 1.8/3.3/4.1; §9 rejections respected
  (no int escape, no Rc<Box<str>> re-run, no permanent dual repr, SharedNode untouched).
- **No placeholders:** every type/function later tasks consume is named where introduced
  (`ValueKind`, `OwnedKind`, `kind()`, `into_kind()`, `AStr`, `ThinStr`, `StrHeader`,
  `ValueRepr`, `value16`, `run_nanb_bench.sh`, `NANB_RESULTS.md`) and referenced consistently.
- **Risk ordering:** the mechanical seam lands, is zero-cost-proven, and MERGES before any
  representation bit flips; the unsafe core is Miri-gated before it carries the repr; the
  evidence phase is complete before the verdict phase opens; the verdict criteria predate the
  measurement.
- **Honest-outcome symmetry:** PATH B is specified to the same depth as PATH A (what gets
  committed, where, and what downstream specs' status lines change) — rejection is a deliverable,
  not a failure mode.
