# Compact Boxed `Value` — Seam + 16-Byte Two-Word Now, 8-Byte NaN-Box as a Measured Follow-Up — Design (NANB)

- **Status:** Draft for review
- **Date:** 2026-06-12
- **Code:** NANB (PERF campaign, `goal-perf.md` — the Representation wave, second half)
- **Depends on:** **SHAPE merged** (object internals stabilize first — `ObjectCell` storage is
  being rewritten by SHAPE; NANB must not double-churn it: `goal-perf.md` execution order, "NANB
  starts only after SHAPE merges"). **LANE + CALL merged** (the A/B must measure the FINAL call
  path — a `Value` width change measured against a call path that CALL is about to rewrite would
  be a stale number; the SRV MINOR-2 same-session lesson generalized to same-*engine-state*).
- **Depended on by:** **JIT** (this spec satisfies the JIT spec's §1.1 precondition row 2 —
  "scalars unboxed/inlined and `Value` is ≤16 bytes — the property *both* VAL outcomes deliver";
  `superpowers/specs/2026-06-08-baseline-jit-design.md`), **REGION** (`goal-perf.md`: "Depends on
  NANB — value representation must be final first").
- **Engines:** **BOTH.** `Value` (`src/value.rs:1101`) is the one type the tree-walker and the VM
  share — the representation changes under both engines simultaneously. Observable behavior is
  byte-identical, asserted by the existing four-mode differential (the oracle property here is
  *weaker* than SHAPE's: both engines sit on the same representation, so the differential alone
  cannot catch a representation bug that corrupts both identically — §7 adds the cross-binary
  old-repr-vs-new-repr differential to close that hole).
- **Breaking:** **no.** Runtime in-memory representation only. No syntax, no semantics, no new
  `type_name`, no opcode change, and **no `.aso` change**: the constant pool serializes constants
  **by logical value, not by memory layout** — verified at `src/vm/aso.rs:789` (`write_value`
  emits `TAG_NIL/TAG_BOOL/TAG_INT/TAG_NUMBER/TAG_STR/TAG_DECIMAL/TAG_ENUM/TAG_ARRAY` with the
  payload re-encoded as bytes: `w.u64(*i as u64)`, `w.f64(*n)`, `w.str(s)`) and `:871`
  (`read_value` reconstructs fresh `Value`s from those bytes). `ASO_FORMAT_VERSION` is
  **unchanged by THIS spec** (27 at drafting, `src/vm/aso.rs:167`; NANB runs in a branch
  parallel to DEFER, which bumps it to 28), guarded by a merge-base-relative negative-space
  test (§7.6).

---

## 0. Read this first — the measured-win gate IS the design

**This spec is gated like the JIT spec: it describes work that ships ONLY on a measured win.**

The precedent is binding. VAL (`superpowers/specs/2026-06-08-compact-value-design.md`) took
`Value` 32 → 24 bytes (boxing the two fat method-binding variants + `Decimal`), then built and
**measured** the 24 → 16 thin-`Str` step — and **rejected it** (`bench/COMPACT_VALUE_RESULTS.md`,
2026-06-09): the STRING-workload geomean regressed **−2.2% specialized / −1.5% generic**, and the
purpose-built cache-density workload `membound_strings` regressed **+6.8%** — the extra
string-access indirection (`Value → Rc → Box<str> → bytes`) cost more per element than the denser
slot saved. The verdict was STOP, the commit was left flagged on the branch, and `Value` shipped
at 24 bytes. That outcome was **honored, not hidden** — and NANB inherits exactly that posture.

NANB is plausible-but-unproven the same way. Therefore the plan's structure is:

1. **Phase 1 — the API seam** (`ValueKind` view + constructors + the sealed repr). Pure refactor,
   zero behavior change, **merges on its own merits even if every later phase is rejected** — it
   is hygiene that makes the representation swappable and pays the JIT/REGION specs forward.
2. **Phase 2 — the compact representation behind the seam** (the 16-byte two-word layout, §3.2),
   on a branch, behind a bring-up Cargo feature.
3. **Phase 3 — the same-session A/B** on the FULL bench corpus + peak RSS + allocation counts, in
   BOTH feature configs and both VM modes plus the tree-walker, plus the deep fuzz campaign and
   Miri over the new `unsafe` core.
4. **Phase 4 — the verdict.** SHIP only if every criterion in §8.1 is met; otherwise the result is
   recorded in `bench/NANB_RESULTS.md` and `goal-perf.md` as **evidence-rejected** — an honored
   outcome, the VAL precedent. Phase 1 stays merged either way.

No speedup is promised anywhere in this document. Expectations are stated (§8.3); the number is
whatever `bench/NANB_RESULTS.md` reports.

## 1. Summary & motivation

`Value` is 24 bytes today (`src/value.rs:1791`, the `value_size_is_documented` tripwire:
`assert_eq!(std::mem::size_of::<Value>(), 24)`). Every fiber stack slot, frame slot,
`Vec<Value>` array element, and object/map slot moves three words where one or two would do.
`goal-perf.md`'s evidence base puts allocation at 22–38% and dispatch at up to 49% of the
object/json workloads; a narrower `Value` attacks both indirectly (denser stacks and slabs,
cheaper push/pop/clone) and is the **explicit precondition of the JIT** (§1.1 row 2 of the JIT
spec: native code wants operands in registers, which means scalars inline in a ≤16-byte value).

VAL §3.2 sanctioned the 8-byte NaN-box as the endgame and §3.3 recorded the honest gate verdict:
**STOP — deferred**, because gcmodule 0.3 exposes no public `Cc::into_raw`/`from_raw` (verified
again 2026-06-12: `Cargo.toml:46` still pins `gcmodule = "0.3"`; the upstream API has not
appeared), and the hand-written `unsafe` ownership layer is unjustifiable without it. That
verdict still stands. What has changed since VAL:

- **SHAPE stabilized object internals** — `ObjectCell` storage churn is over; a `Value` layout
  change no longer collides with a concurrent storage rewrite.
- **LANE/CALL finalized the call path** — the A/B now measures the representation against the
  engine the JIT would actually sit on.
- **The VAL rejection has a diagnosable cause** (§3.2.2): the rejected thin-`Str` was
  `Rc<Box<str>>` — **two** dependent heap hops to the bytes. A single-allocation thin string
  (header-carried length, one hop) removes the measured culprit. Whether that is enough is
  precisely what the Phase-3 A/B exists to decide.

**Honest framing (the title notwithstanding):** NANB delivers (a) the representation seam and
(b) a **16-byte two-word `Value`** as the Phase-2 candidate, with the **8-byte NaN-box recorded
as a follow-up experiment behind the same gate** (§3.3, §3.4). Both layouts satisfy the JIT's
≤16-byte precondition; the two-word layout gets there with no `int` escape, no NaN
canonicalization burden, no hand-written `Cc` ownership `unsafe`, and no dependency on an
upstream gcmodule API. The A/B decides whether even that ships.

## 2. The current layout — audit (grounded, 2026-06-12)

`Value` (`src/value.rs:1101`) has **29 variants** (28 under `--no-default-features` — `Regex` is
`#[cfg(feature = "data")]`):

| Group | Variants | Payload | Width |
|---|---|---|---|
| Immediates | `Nil`, `Bool(bool)` | inline | 0–1 B |
| Scalars | `Int(i64)`, `Float(f64)` | inline | 8 B |
| **Fat strings** | **`Str(Rc<str>)`, `Builtin(Rc<str>)`** | **fat pointer (ptr + len)** | **16 B ← the width-setters** |
| Boxed scalar | `Decimal(Rc<Decimal>)` (VAL Task 2) | thin pointer | 8 B |
| `Cc` containers | `Array`, `Object`, `Map`, `Set`, `Instance(Cc<RefCell<Instance>>)`, `Closure` | `Cc` = `NonNull` (1 word, VAL §3.1) | 8 B |
| `Rc` handles | `Function`, `Bytes(Rc<RefCell<Vec<u8>>>)`, `Regex`, `Native`, `NativeMethod`, `Enum`, `EnumVariant`, `Class`, `Interface`, `BoundMethod`, `Super`, `Generator`, `GeneratorMethod(Rc<GeneratorMethodData>)`, `ClassMethod(Rc<ClassMethodData>)` (both boxed by VAL Task 1) | thin pointer | 8 B |
| Newtype handle | `Future(crate::task::SharedFuture)` — `SharedFuture(Rc<HandleInner>)`, `src/task.rs:106` | thin pointer | 8 B |
| `Arc` leaf | `Shared(Arc<SharedNode>)` (SRV — the only `Send`-carrying payload) | thin pointer | 8 B |

**The only payloads wider than one word are the two fat `Rc<str>` carriers.** The enum layout law
VAL measured empirically (`src/value.rs:1762-1791` doc comment, scratch-verified at the real
variant count): because `Int`/`Float` occupy every bit pattern of their payload word, Rust cannot
niche-elide the discriminant — the layout is `round_up(widest_payload) + 8-byte tag`. Fat
`Rc<str>` ⇒ 16 + 8 = **24**. Thin every payload to ≤ 8 bytes ⇒ 8 + 8 = **16**. **8 bytes is
unreachable for any Rust enum with a full-`i64` inline variant** — it requires the hand-tagged
machine word (the NaN-box, §3.3).

Refcount disciplines behind the pointers (the thing any hand-written ownership layer must
dispatch on, and the thing the derived enum gets for free): **`Cc` ×6, `Arc` ×1, `Rc` ×~19**,
plus inline scalars.

## 3. The encoding — both candidates, bit-exact

### 3.1 Candidate B — the 16-byte two-word `Value` (RECOMMENDED for Phase 2)

**Layout.** `Value` stays a Rust **enum** (compiler-derived `Clone`/`Drop` over `Rc`/`Cc`/`Arc` —
zero new *ownership* `unsafe`), with every payload ≤ 8 bytes. By the layout law above the
compiler produces the two-word form: **word 0 = discriminant, word 1 = payload** (exact word
order is rustc's choice and deliberately NOT relied on — we assert only
`size_of::<Value>() == 16`). Floats are a raw `f64` in the payload word — **no NaN games at all**
(verified consequence of the two-word form: the tag is explicit, so every f64 bit pattern,
including every user NaN payload, is stored and round-tripped verbatim; a §7.5 property test pins
this). `Int` is a full inline `i64` — **no SMI, no spill, no escape**; NUM's checked-i64
semantics are untouched by construction.

The only change needed to get there: thin the two fat string payloads.

#### 3.1.1 `ThinStr` — a single-allocation, header-length, `!Send` thin string

The ONLY new `unsafe` in Candidate B (~120 lines, Miri-gated §7.4):

```rust
/// One heap allocation: [ StrHeader | utf8 bytes... ]. The handle is a single
/// thin word; len lives beside the refcount in the SAME allocation/cache line.
#[repr(C)]
struct StrHeader {
    strong: Cell<usize>, // non-atomic (Rc discipline); abort-on-overflow like Rc
    len: usize,
}
pub struct ThinStr {
    ptr: NonNull<StrHeader>,
    _not_send: PhantomData<Rc<()>>, // !Send + !Sync, like Rc<str>
}
// Clone = strong += 1. Drop = strong -= 1; dealloc(header_layout + len) at 0.
// Deref<Target = str> = utf8 slice at (ptr + size_of::<StrHeader>(), len).
// Eq/Ord/Hash by content; From<&str>/From<String>; Display/Debug via &str.
```

**Why this is NOT the rejected VAL thin-`Str`.** The rejected encoding was `Rc<Box<str>>`
(`AStr`, `bench/COMPACT_VALUE_RESULTS.md` §1): `Value → RcBox → Box<str> → bytes` — **two**
dependent loads to reach the bytes, and the length a hop away from them. `ThinStr` is **one**
load: `Value → [header | bytes]`, with `len` on the same cache line as the first bytes. Versus
today's fat `Rc<str>` the *remaining* delta is: the fat pointer carries `len` in the value itself
(zero loads) where `ThinStr` pays one load that the byte access was going to pay anyway. The
hypothesis — that header-adjacency makes the delta noise while the 24→16 slot shrink pays on
density — is exactly what the §8 A/B adjudicates, on the SAME string workloads that killed the
VAL attempt (`string_concat`, `string_map`, `string_index`, `membound_strings`). If they regress
again beyond §8.1's tolerance, Candidate B is rejected too, and recorded.

#### 3.1.2 The `AStr` seam and the `MapKey` boundary

Phase 1 introduces `pub type AStr = Rc<str>` and routes every site that extracts/clones a string
*handle out of a `Value`* through it (`ValueKind::Str(&AStr)`, `Value::str(impl Into<AStr>)`).
Phase 2 flips the alias under the bring-up feature: `pub type AStr = ThinStr`. Sites needing text
use `Deref<Target = str>` and never notice.

**`MapKey` — semantics untouched, payload type tracks the alias.** `MapKey` (`src/value.rs:189`)
is and stays a separate type with byte-identical canonicalization (Int folding, NaN unification,
−0.0). But `MapKey::from_value` does `MapKey::Str(s.clone())` (`src/value.rs:236`) — a cheap
refcount bump **only because the payload types match**. If `Value`'s string payload becomes
`ThinStr` while `MapKey::Str` stays `Rc<str>`, every map/set keying of a string pays a full text
copy — a guaranteed regression on `string_map`. So Phase 2 changes `MapKey::Str(AStr)` in the
same commit: content hashing/equality identical (property-tested against the old payload, §7.5),
zero behavioral change, and `SharedKey::from_map_key`/`to_map_key` (`src/value.rs:925-946`)
already copy text across the `Rc`↔`Arc` boundary, so the freeze path is unchanged in shape.

**The boundary-conversion audit (a Phase-2 deliverable).** `Rc<str>` also lives in APIs unrelated
to `Value`'s payload (env binding names, spans, `ClassMethodData.name`). A `Value::Str` →
`Rc<str>` escape under the flipped alias is a text copy; Phase 2 greps every such escape (the
seam makes them textually findable — they are exactly the `AStr::from`/`Rc::from(&*s)`
conversions the compiler forces) and proves each is off the hot path or eliminates it. The audit
table goes in the commit body; the A/B is the backstop.

#### 3.1.3 What Candidate B does NOT need (the risk ledger, all zeros)

- **No `int` escape** — full i64 inline. The forbidden "change int semantics to fit 48/51 bits"
  cannot even arise.
- **No NaN canonicalization** — floats are raw bits behind an explicit tag.
- **No hand-written `Clone`/`Drop`/`trace` over `Cc`** — the enum derives them; gcmodule's
  missing `into_raw`/`from_raw` is irrelevant.
- **No GC change at all** — `impl Trace for Value` keeps matching typed variants and recursing
  (§5.1); `cc_addr`/`cc_ptr_eq` identity untouched.
- **The only `unsafe` is `ThinStr`** — small, local, Miri-clean (the DBG `Code`-newtype
  precedent, `src/vm/chunk.rs:1020`).

### 3.2 Candidate A — the 8-byte NaN-box (designed bit-exact, recorded as a gated follow-up)

Stated precisely so the eventual experiment is a measurement, not a redesign. **The safer
quiet-NaN-boxing scheme is chosen** (all non-floats live in the negative-quiet-NaN space; any
incoming NaN is canonicalized at `Value`-construction boundaries) over the alternative
(canonicalize at every NaN-producing op) because the construction chokepoint is ONE site —
`Value::float(n)` — which both engines and every stdlib/parse path already route through after
Phase 1, whereas the per-op scheme would require finding and guarding every arithmetic, `math.*`,
and parse site forever.

- **Float:** a word `w` is an `f64` (read verbatim) unless
  `(w & 0xFFF8_0000_0000_0000) == 0xFFF8_0000_0000_0000` (sign + all-ones exponent + quiet bit —
  the negative-quiet-NaN space, 2^51 payloads). `Value::float(n)` canonicalizes any incoming NaN
  to positive quiet NaN `0x7FF8_0000_0000_0000`, which is outside the boxed space. AScript never
  exposes NaN payload bits (no `to_bits` surface; `MapKey` already unifies all NaNs to one key,
  `src/value.rs:228-233`; Display prints `NaN`; json rejects/nullifies non-finite), and the
  tree-walker shares the same constructor — so canonicalization is behavior-invisible, with a
  property test owed to prove it.
- **Boxed space, bits 50–48 = a 3-bit family tag**, bits 47–0 = payload:
  - family 0 — immediates: payload 0 = `Nil`, 1 = `false`, 2 = `true`.
  - family 1 — **`Int` as i48 SMI** (two's-complement payload, range `[−2^47, 2^47−1]`); any i64
    outside the range spills to a **boxed `Rc<i64>`** pointer kind (semantics-invisible:
    decode-then-operate everywhere; the VAL §7.2 boundary battery — `2^47±1`, `−2^47±1`, `2^53`,
    `i64::MIN/MAX` — already exists at `src/value.rs:1830` and `tests/vm_differential.rs`
    `smi_boundary_*` and becomes LIVE). **Measurement obligation:** the spill *rate* on the corpus
    + fuzzer (arithmetic produces out-of-range values transiently — `a*b` overflow-checks via
    i128/checked ops and a near-2^47 workload would box per iteration); a high spill rate is
    itself a rejection datum.
  - families 2–7 — **pointer kinds**: payload = the 48-bit pointer (≤48 canonical bits on every
    mainstream 64-bit OS). All our heap allocations are ≥8-aligned, so the low 3 pointer bits are
    free: **kind = (family − 2) × 8 + (w & 7)**, pointer = `w & 0x0000_FFFF_FFFF_FFF8`. 6×8 = 48
    kind slots ≥ the 27 pointer kinds + the boxed-i64 spill. This is the flat-tag answer to a
    fact JSC never faces: AScript's heap kinds are bare `Rc<T>`/`Cc<T>`/`Arc<T>` of ~26 distinct
    `T`s with **no common header** to read a kind from, so the kind must live in the word.
- **The hard part is unchanged from VAL §3.3:** the word must OWN the pointer — hand-written,
  tag-dispatched `Clone`/`Drop`/`trace` across **three refcount disciplines** (`Rc`, `Cc`, `Arc`
  — SRV's atomic leaf in the same tag space as the non-atomic kinds), reconstructing `Cc<T>` from
  raw bits. That requires `Cc::into_raw`/`from_raw`, which **gcmodule 0.3 still does not expose**
  (re-verified; the VAL gate verdict of 2026-06-09 stands; the layout-coupling transmute remains
  rejected as fragile).

**Candidate A is therefore double-gated:** (g1) the gcmodule raw API lands upstream, AND (g2) the
same §8.1 measured-win gate, run as its own A/B against the then-current baseline (which, if
Candidate B ships, is the 16-byte layout). It is a recorded future experiment, not Phase 2.

### 3.3 The (a)-vs-(b) evaluation — and the recommendation

| Axis | (a) 8-byte NaN-box | (b) 16-byte two-word |
|---|---|---|
| `Value` width | 8 B | 16 B (24 → 16, −33%) |
| JIT ≤16 B precondition | ✅ | ✅ (the JIT spec accepts both, §1.1 row 2) |
| `int` semantics | i48 SMI + boxed spill (rate unmeasured; transient arithmetic boxes) | full inline i64 — **no escape** |
| Float handling | NaN canonicalization at construction + a decode test per float read | raw f64 — **no NaN games** |
| Ownership safety | hand-written tag-dispatched `Clone`/`Drop`/`trace` over Rc/Cc/Arc (~26 kinds × 3 disciplines of `unsafe`) | compiler-derived — **zero ownership `unsafe`** |
| gcmodule dependency | **blocked** on upstream `Cc::into_raw`/`from_raw` (still absent, 0.3) | none |
| String representation | thin tagged pointer — needs the same single-allocation header string as (b) | `ThinStr` (the one new `unsafe`, ~120 lines) |
| Decode cost per op | tag test on every read incl. every float | discriminant load (what the enum does today) |
| Risk class | the VAL §3.3 "single biggest risk" verbatim | the VAL Stage-1 risk class (shipped clean) |

**Recommendation: run Phase 2 as (b).** It is the lower-risk, dependency-free, semantics-inert
layout; it satisfies the JIT precondition outright; and it isolates the ONE genuinely contested
question — does a properly-built thin string plus 33% denser value slots win or lose? — as a
clean measurable. If (b) ships and later profiling shows width still material, (a) is the
recorded follow-up behind its double gate. If (b) is rejected, (a) is almost certainly rejected
too (it contains (b)'s string trade *plus* the SMI spill *plus* the unsafe layer), and the
rejection report says so. Honest credit to the purist case: under (a) scalars and floats move in
ONE word and the fiber stack halves again — the ceiling is real, which is why (a) stays designed
and gated rather than deleted.

## 4. The API seam (Phase 1 — the real work, and the part that merges unconditionally)

### 4.1 The migration surface, measured (ripgrep, 2026-06-12)

| Probe | Count |
|---|---|
| `Value::` tokens | **5,437** across **103 files** |
| Pattern-position match arms (`^\s*Value::… =>`) | **893** |
| Tuple-pattern arms (`(Value::…, Value::…)` — `apply_binop`, adaptive guards) | ~**155** |
| `if let Value::…` | **106** |
| `matches!(… Value:: …)` | **155** |
| `let Value::` (let-else) | **9** |
| Scalar construction tokens (`Value::{Int,Float,Bool,Str,Nil}(…)`) | **3,405** |
| Heaviest files | `interp.rs` 675 · `vm/run.rs` 435 · `stdlib/schema.rs` 408 · `value.rs` 380 · `stdlib/ffi.rs` 163 · `worker/serialize.rs` 138 |

A raw representation swap rewrites all of it at once — unreviewable and unbisectable. The seam
makes the swap a one-file edit.

### 4.2 The design: `ValueKind<'_>` view + owned deconstruction + constructors + a sealed repr

Two options were surveyed:

- **Rejected:** keep `pub enum Value` as the public view and add an internal compact storage type
  converted at boundaries. Every boundary crossing is a real conversion (refcount traffic both
  ways), the "boundary" is the entire codebase, and nothing stops new code from matching the
  public enum — the seam never seals.
- **Chosen:** migrate match sites to a **borrowed view** `fn kind(&self) -> ValueKind<'_>` (plus
  an owned `fn into_kind(self) -> OwnedKind` for the consuming matches the VM uses to avoid
  clones), full constructor coverage (`Value::int/float/str/object/...` — extending the VAL
  Task-0 helpers already at `src/value.rs:1283-1326`), and then **seal the representation**:
  `pub struct Value(ValueRepr)` with `ValueRepr` private to `src/value.rs`. The compiler then
  *proves* there are no stragglers (the SHAPE "make `map` private" trick, one level up). After
  the seal, the repr is free to change behind `kind()`/`into_kind()`/the constructors, and ONLY
  `value.rs` changes in Phase 2.

```rust
/// Borrowed view of a Value's logical kind. Variant set mirrors today's enum
/// 1:1 so a match-site migration is textual: `match v { Value::X(p) => … }`
/// becomes `match v.kind() { ValueKind::X(p) => … }` with bodies unchanged
/// modulo one `&`/`*`. Scalars are by-value (Copy); handles are borrowed.
pub enum ValueKind<'a> {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Decimal(&'a Rc<Decimal>),
    Str(&'a AStr),            // AStr = Rc<str> in Phase 1; ThinStr in Phase 2(b)
    Builtin(&'a AStr),
    Function(&'a Rc<Function>),
    Closure(&'a Cc<crate::vm::value_ext::Closure>),
    Array(&'a Cc<ArrayCell>),
    Object(&'a Cc<ObjectCell>),
    Map(&'a Cc<MapCell>),
    Set(&'a Cc<SetCell>),
    Bytes(&'a Rc<RefCell<Vec<u8>>>),
    #[cfg(feature = "data")]
    Regex(&'a Rc<RegexHandle>),
    Native(&'a Rc<NativeObject>),
    NativeMethod(&'a Rc<NativeMethod>),
    Enum(&'a Rc<EnumDef>),
    EnumVariant(&'a Rc<EnumVariant>),
    Class(&'a Rc<Class>),
    Interface(&'a Rc<InterfaceDef>),
    Instance(&'a Cc<RefCell<Instance>>),
    BoundMethod(&'a Rc<BoundMethod>),
    Super(&'a Rc<SuperRef>),
    Future(&'a crate::task::SharedFuture),
    Generator(&'a Rc<crate::coro::GeneratorHandle>),
    GeneratorMethod(&'a Rc<GeneratorMethodData>),
    ClassMethod(&'a Rc<ClassMethodData>),
    Shared(&'a Arc<SharedNode>),
}
```

`OwnedKind` is the by-value mirror (payloads moved, not borrowed) for consuming matches.
Everything is `#[inline(always)]`; over today's enum, `kind()` is a re-projection of the same
discriminant and LLVM compiles `match v.kind()` to the identical jump table — **a claim the
Phase-1 zero-cost gate measures rather than assumes** (§8.2: Phase-1-only vs `main`, same
session, geomean within noise, plus the standing `dbg_zero_cost_gate` since dispatch-arm text is
touched).

Inside `src/value.rs` the repr-private impls (`PartialEq` `:1393`, `Debug` `:1487`, `Display`
`:1560`, `is_truthy` `:1210`, `MapKey::from_value` `:204`, `frozen_kind` `:263`, the accessors)
keep matching `ValueRepr` directly — they ARE the encoding. `impl Trace for Value`
(`src/gc.rs:193`) moves into `value.rs` beside the repr (gcmodule trait, our crate — placement is
free) so `gc.rs` needs no privileged repr access; the container `Trace` impls
(`ObjectCell`/`ArrayCell`/`MapCell`/`SetCell`/`Instance`/`Closure`, `src/gc.rs:252-367`) stay in
`gc.rs` untouched — they receive `&self`, never a `Value` word.

### 4.3 Representative migrations (the textual rules the plan executes)

The adaptive-arithmetic guard (`src/vm/run.rs:5318`) and the generic arm
(`src/interp.rs:6924`):

```rust
// before                                            // after
match (kind, &a, &b) {                               match (kind, a.kind(), b.kind()) {
    (ArithKind::Int, Value::Int(x), Value::Int(y))       (ArithKind::Int, ValueKind::Int(x), ValueKind::Int(y))
        => int_binop(op, *x, *y, span),                      => int_binop(op, x, y, span),
…
(Value::Int(a), Value::Int(b)) => int_binop(…)       (ValueKind::Int(a), ValueKind::Int(b)) => int_binop(…)
```

`if let Value::Str(s) = &v {…}` → `if let ValueKind::Str(s) = v.kind() {…}`;
`matches!(v, Value::Int(_) | Value::Float(_))` → `matches!(v.kind(), ValueKind::Int(_) | ValueKind::Float(_))`
(or the existing `v.is_number()`); `Option`-wrapped patterns (`Some(Value::Str(s))`) restructure
to match the inner value's `kind()`; consuming matches use `into_kind()`. Construction:
`Value::Int(n)` → `Value::int(n)`, `Value::Str(Rc::from(s))` → `Value::str(s)`, etc. The
serializers (`src/worker/serialize.rs` `encode_value`, json/msgpack/cbor walkers, `aso.rs`
`write_value`) migrate identically — after Phase 1 they are representation-agnostic by
construction.

## 5. Invariants preserved (GC / identity / Send / serializers)

1. **GC ownership and tracing.** Under Candidate B the enum still holds typed `Cc<T>`s:
   `Clone`/`Drop` are compiler-derived refcount ops, exactly as today; `Value::trace` keeps its
   typed arms (containers recurse, `EnumVariant` payload-traces per ADT, `Shared`/natives no-op
   per the deterministic-`Drop` invariant, `src/gc.rs:23-47`). `cc_addr`/`cc_ptr_eq`
   (`src/gc.rs:152-161`) read through the same `Cc` deref — Display cycle guards, `==` identity,
   identity-keyed maps, and `shared.rs`'s two freeze tables (`src/stdlib/shared.rs:54-77`, keyed
   by `cc_addr`) are bit-for-bit unaffected. (Under the deferred Candidate A this whole block
   becomes the hand-written layer — the reason A is gated.)
2. **`Value` stays `!Send`/`!Sync`.** The compile-time
   `static_assertions::assert_not_impl_any!(Value: Send, Sync)` (`src/value.rs:1203`) and the
   gc.rs test sibling (`src/gc.rs:1230`) are kept verbatim; `ThinStr` carries
   `PhantomData<Rc<()>>` so it can never silently flip the assert. `SharedNode`'s positive
   `Send + Sync` assert (`src/value.rs:1078`) is untouched — `Value::Shared(Arc<SharedNode>)` is
   one word and fits every candidate layout unchanged.
3. **`SharedNode` is out of scope.** A separate type with its own `Arc` graph (`src/value.rs:865`)
   — NANB never edits it.
4. **`MapKey` semantics untouched** (§3.1.2 — payload type tracks `AStr` under Phase 2(b), with a
   cross-payload property test; canonicalization byte-identical).
5. **Serializers are logical, and Phase 1 makes them structurally so.** The `.aso` constant pool
   (`src/vm/aso.rs:789/871`) and the worker airlock (`src/worker/serialize.rs:83-107`,
   `TAG_NIL…TAG_SHARED`) tag by KIND and re-encode payload bytes — no memory layout on any wire.
   §7.5 adds round-trip assertions that a value serializes to identical bytes under old and new
   repr. `src/det.rs` is deliberately `Value`-free (verified: 3 mentions, all doc comments saying
   so) — no seam work needed.
6. **Native handles stay GC-opaque**, the resource table is keyed by id, not by `Value` — the
   representation of a handle cannot affect fd lifetime (VAL §3.4, re-inherited).
7. **Size tripwire.** `value_size_is_documented` (`src/value.rs:1791`) keeps the VAL staged-assert
   style: Phase 1 asserts 24 unchanged; Phase 2 asserts 16 under the bring-up feature and 24
   without; Phase 4 SHIP pins 16 unconditionally (REJECT restores the 24 pin). The `#[ignore]`d
   `value_size_print` stays as the manual probe.

## 6. Bring-up configuration & Gate-15 reconciliation

A type's memory layout cannot be a runtime flag — `--no-specialize`-style switches do not apply.
The bring-up axis (Gate 15's own text scopes it: "NaN-box **(during bring-up)**") is realized as:

- A Cargo feature **`value16`** (default-OFF on the branch) selecting the Phase-2 repr behind the
  seam. Both configurations build and run the FULL suite + differential in CI on the branch
  (×2 feature configs ⇒ a 4-way matrix during bring-up).
- A **cross-binary differential**: the entire `examples/**` corpus + goldens run on the
  baseline-repr binary and the `value16` binary, outputs diffed **byte-for-byte** (this is the
  oracle the within-binary four-mode differential structurally cannot provide, since both engines
  share `Value` — §0 "Engines").
- On the Phase-4 **SHIP** verdict the feature is removed and the new repr becomes unconditional
  in the same merge; on **REJECT** the feature and its commits stay flagged on the branch (the
  VAL `c1571ec` precedent) and only Phase 1 survives on `main`. A permanent dual-repr feature is
  explicitly rejected (an untested zombie config is a correctness liability, not a kill switch) —
  this is the recorded, justified deviation from Gate 15's "permanent" clause; the permanent
  guards are the seam, the size tripwire, and the property suite.

## 7. Correctness (the oracle battery)

1. **Four-mode differential**, both feature configs, every phase: `tests/vm_differential.rs`
   (tree-walker == specialized == generic == `.aso`) over corpus + goldens. Fix the engine, never
   the assertion.
2. **Cross-binary old-vs-new repr differential** (§6) — required green before Phase 4.
3. **Deep fuzz campaign on the branch before any verdict** (the FUZZ ~284,000-case precedent —
   merge `9b202eb`: "~284,000 generated programs across all four modes found ZERO further
   divergence"): `FUZZ_STRESS_N=300000 cargo test --test property
   stress_differential_many_seeds -- --ignored --nocapture` under `value16`
   (`tests/property.rs:186-195`), plus time-boxed cargo-fuzz runs of
   `fuzz/fuzz_targets/{differential,worker_serialize,aso_roundtrip}.rs` where nightly is
   available. Representation bugs present as wrong values/crashes — this is the net that catches
   them.
4. **Miri on the `unsafe` core** (the DBG `Code`-newtype precedent, `src/vm/chunk.rs:1020`):
   `cargo +nightly miri test thin_str` over the `ThinStr` unit/property tests (alloc/dealloc,
   clone/drop balance incl. zero-len and huge-len, deref aliasing, hash/eq). Miri-clean is the
   stated bar for new `unsafe`.
5. **Property tests** (`tests/property.rs`, the FUZZ precedent): encode/decode round-trip over
   EVERY kind — float bit patterns including signaling/quiet NaN payloads and −0.0 stored
   verbatim (Candidate B) ; `i64::MIN`/`MAX` and the §3.2 SMI boundary battery (inert under B,
   live under a future A — already present at `src/value.rs:1830`); every heap kind's identity
   stable through clone (`cc_addr` before == after); `ThinStr` ↔ `Rc<str>` content
   hash/eq/ord/Display agreement; `MapKey` fold equality across payload types; worker-wire and
   `.aso` byte-stability across reprs.
6. **Negative space**: `ASO_FORMAT_VERSION` asserted **unchanged vs the branch's merge-base**
   (compare against the value recorded at branch time, never a literal — NANB runs PARALLEL to
   DEFER, which legitimately bumps 27 → 28; the assertion is "unchanged by THIS spec") + a
   golden `.aso` byte-compare against
   the merge-base (the SHAPE Task-5.3 / `srv_negative_space` precedent); no opcode count change; no
   grammar/parser change at all (no regen, no editor pins).

## 8. Performance (the gate, the protocol, the honest expectations)

### 8.1 The SHIP criteria (all required; any miss ⇒ evidence-rejected)

Measured same-session, interleaved round-robin, per-cell median of ≥5 reps, one machine
(`bench/run_compact_value_bench.sh` protocol, extended):

1. **Geomean time:** ALL-HOT geomean ≥ **1.02×** (≥2% faster) on the specialized VM AND ≥
   **1.00×** (no regression) on the generic VM — the win must clear the measured noise band, not
   ride it (the VAL report's "SCALAR +2.2% is noise" lesson).
2. **No pathological regression:** no single HOT workload below **0.97×** in either VM mode; the
   STRING subset geomean ≥ **0.99×** in both modes (the exact VAL failure mode, named).
3. **Memory:** peak RSS (`/usr/bin/time -l`) improves ≥ **5%** median on the data-heavy set
   (`json_roundtrip`, `membound_strings`, the large-`Vec<Value>` walk); allocation counts
   reported per Gate 18 and not regressed.
4. **Tree-walker:** geomean within **0.97×** (it shares `Value`; a tree-walker regression is a
   bug, and the Gate-17 spec/tw ≥2× floor is re-verified either way).
5. **All §7 correctness green** (differential ×2 configs ×2 reprs, fuzz campaign, Miri,
   properties, negative space) and clippy/tests green in both feature configs.

Otherwise: **REJECT and record** — `bench/NANB_RESULTS.md` gets the KEEP-or-STOP verdict in the
COMPACT_VALUE_RESULTS format, `goal-perf.md`'s NANB row flips to evidence-rejected with the
numbers, the JIT spec's precondition-2 status is annotated (≤16 B then NOT met at 24 B — the JIT
defers further unless its own re-profile says otherwise), and Phase 1 remains merged.

### 8.2 The A/B protocol

- **Workloads:** the full `bench/compact_value_bench.as` set (int_sum, fib_iter, array_walk,
  object_churn, float_sum, string_concat, string_map, string_index, membound_strings +
  decimal_cold/method_cold as cold checks) + `bench/profiling/` (json_roundtrip, object_churn,
  async pair) + the LANE Task-0 functional-idiom/call-heavy corpus — the post-LANE/CALL/SHAPE
  engine is the baseline, which is why those merges are hard dependencies.
- **Modes per workload:** specialized VM, generic VM (`--no-specialize`), tree-walker — both
  binaries (baseline `main`-repr build and `value16` build) in ONE session, interleaved.
- **Instruments:** wall-clock medians; the shipped profiler (`ascript run --profile cpu`) for
  attribution deltas (string-access, alloc); `/usr/bin/time -l` peak RSS; allocation counts via
  the bench harness counter (Gate 18's sanctioned instruments).
- **Phase-1-only A/B** (a separate, earlier measurement): seam-only vs `main` must be ≈1.00×
  geomean (the "enum-view inlining compiles away" claim, verified not assumed) and
  `dbg_zero_cost_gate` re-run green (dispatch-arm text is touched).
- Results in **`bench/NANB_RESULTS.md`**: machine/date/commits, the full table, both-mode
  columns, the subset geomeans (ALL-HOT / SCALAR / STRING), RSS/alloc tables, and the explicit
  verdict against each §8.1 criterion.

### 8.3 Expectations (stated, not promised)

24 → 16 cuts every `Value` slot by a third: fiber stacks, frame slot vectors, `Vec<Value>`,
slab/IndexMap slots — the cache-density bet, again, this time without the double-hop string that
poisoned it. Plausible wins: array/object scans, call-argument traffic, json tree walks. Plausible
losses: the `len`-load on string reads, `AStr` boundary conversions that escape audit. The VAL
data showed the scalar loops are insensitive (they never touch `Str`) — honest reporting will
label any scalar delta as machine drift, exactly as `bench/COMPACT_VALUE_RESULTS.md` did.

## 9. Scope & rejected alternatives

**In scope:** the Phase-1 seam (ValueKind/OwnedKind/constructors/sealed repr, the ~5,400-site
mechanical migration, zero-cost-proven); `ThinStr`; the 16-byte two-word repr behind `value16`;
the `MapKey::Str` payload alignment; the cross-binary differential + fuzz campaign + Miri +
property suite; the same-session A/B with RSS/alloc; the §8.1-gated ship-or-record verdict; the
bit-exact Candidate-A design as a recorded follow-up.

**Out of scope / deferred (recorded, owner-noted):**
- **Candidate A (8-byte NaN-box)** — double-gated on the gcmodule `Cc::into_raw`/`from_raw`
  upstream API (still absent in 0.3) AND its own §8.1-style measured win (§3.2).
- **Small-string inlining** (SSO inside `ThinStr` or NaN-box payloads) — a recorded follow-up
  experiment behind the same gate, per the `goal-perf.md` parked list ("demoted to
  opportunistic"); not bundled into Phase 2 so the A/B measures ONE change.
- **`SharedNode` representation** — untouched, a different ownership domain (SRV).
- **`MapKey` semantics** — untouched (only the §3.1.2 payload-type alignment, property-pinned).

**Rejected:**
- **Changing `int` semantics to fit 48/51 bits** — FORBIDDEN. NUM's full-i64 `int` is the
  language; any int-escape design must be semantics-invisible (Candidate A's boxed spill is; a
  truncating int is never considered).
- **Public-enum + internal-storage conversion at boundaries** (§4.2) — conversion cost
  everywhere, seam never seals.
- **`Rc<Box<str>>` thin-`Str`** — already built, measured, and rejected by VAL
  (`bench/COMPACT_VALUE_RESULTS.md`); NANB does not re-run a known-lost experiment, it fixes the
  diagnosed cause (§3.1.1).
- **A permanent dual-repr Cargo feature as the "kill switch"** — an untested zombie configuration
  is a liability, not a safety net; bring-up-only, then one repr (§6).
- **Pointer compression** — not applicable on 64-bit-only targets v1; nothing to compress into.
- **The layout-coupling `transmute` into gcmodule's private `RawCcBox`** — rejected by VAL §8,
  stays rejected.

## 10. Grounding (verified file:line, 2026-06-12)

- `src/value.rs:1101` — `pub enum Value`, 29 variants; `:1122` `Decimal(Rc<Decimal>)` (VAL
  Task 2); `:1123/:1125` the two fat `Rc<str>` payloads (`Str`/`Builtin`) — the width-setters;
  `:1176/:1185` the VAL Task-1 boxed `GeneratorMethod`/`ClassMethod`; `:1191`
  `Shared(Arc<SharedNode>)`; `:1203` `assert_not_impl_any!(Value: Send, Sync)`; `:1283-1326` the
  VAL Task-0 accessor seam this spec extends; `:1393` `PartialEq` (identity via `cc_ptr_eq`);
  `:1791` `assert_eq!(size_of::<Value>(), 24)` + the layout-law doc comment (`:1762-1790`);
  `:1830` the SMI-boundary battery (inert today, live under Candidate A); `:189/:204/:236`
  `MapKey` + `from_value`'s `Str(s.clone())`; `:865/:1078` `SharedNode` + its `Send+Sync` assert;
  `:925-946` `SharedKey` boundary copies.
- `src/gc.rs:193` `impl Trace for Value` (typed decode-then-recurse arms); `:152/:159`
  `cc_addr`/`cc_ptr_eq` (deref-address identity); `:23-47` the traced-vs-acyclic invariant;
  `:1230` the Send assert sibling.
- `src/vm/value_ext.rs:22` `Closure` (Cc payload, one word).
- `src/task.rs:106` `SharedFuture(Rc<HandleInner>)` — one word.
- `src/vm/aso.rs:167` `ASO_FORMAT_VERSION = 27`; `:789/:871` `write_value`/`read_value` — the
  constant pool serializes by logical value (the no-bump proof).
- `src/worker/serialize.rs:83-107` logical wire tags; `encode_value` walks variants via match —
  rides the seam.
- `src/det.rs` — `Value`-free by design (3 doc-comment mentions only).
- `src/stdlib/shared.rs:54-77/:121` — the freeze identity tables keyed by `cc_addr`.
- `src/vm/run.rs:5270/:5318` `eval_binop_adaptive` + the `(ArithKind::Int, Value::Int, Value::Int)`
  guard; `src/interp.rs:6924` the generic int arm; `:7763` `type_name` — the representative
  migration sites.
- `src/vm/chunk.rs:1020` — the Miri-clean precedent (`patch_byte_through_shared`).
- `tests/property.rs:186-195` — `stress_differential_many_seeds` + `FUZZ_STRESS_N`;
  `fuzz/fuzz_targets/{differential,worker_serialize,aso_roundtrip,parser,archive_roundtrip}.rs`.
- `bench/COMPACT_VALUE_RESULTS.md` — the thin-`Str` rejection evidence (the binding precedent);
  `bench/run_compact_value_bench.sh` + `bench/compact_value_bench.as` — the A/B protocol reused.
- `superpowers/specs/2026-06-08-compact-value-design.md` §3.2/§3.3 — the sanctioned NaN-box
  endgame + the gcmodule gate verdict (STOP, 2026-06-09); `Cargo.toml:46` `gcmodule = "0.3"`
  (raw API still absent).
- `superpowers/specs/2026-06-08-baseline-jit-design.md` §1.1 row 2 — the ≤16-byte precondition
  NANB satisfies.
- Survey counts: §4.1 (ripgrep, this date).
- External precedent: LuaJIT 2 NaN tagging / JSC `JSValue` (the §3.2 scheme; JSC reads kinds from
  a common cell header — AScript cannot, hence the family+alignment tag design); V8 SMI (the i48
  spill model).
