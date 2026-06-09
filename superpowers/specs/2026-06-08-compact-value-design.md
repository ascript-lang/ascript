# AScript Compact Value Representation & Allocation Discipline — Design (VAL)

- **Status:** Draft for review
- **Date:** 2026-06-08
- **Code:** VAL (the performance foundation of the Serious Language campaign — see `goal.md`)
- **Depends on:** **NUM** (needs `Value::Int(i64)` / `Value::Float(f64)` — `2026-06-08-numeric-model-design.md`).
  NUM is spec-locked, plan-written, and **merged** before VAL implementation begins (`goal.md` execution order).
- **Depended on by:** the deferred **JIT** (a JIT over a fat boxed enum has a low ceiling — `goal.md`);
  benefits **SRV** (smaller values = cheaper per-request churn) but does not block it.
- **Engines:** both (tree-walker oracle == specialized VM == generic VM, byte-identical).
- **Breaking:** **no** — this is a **pure-performance** change. Zero observable-behavior change is the hard
  gate. No surface syntax, no new `type_name`, no new `Value` *kind* (only a different *encoding* of the
  same kinds). Pre-1.0 we break freely (`goal.md`), but VAL deliberately does **not**.

---

## 1. Summary & motivation

`Value` today is a wide tagged union (`src/value.rs:623`) with ~25 variants. The two **widest** variants are
**not** the scalar/string payloads — they are the two-field method bindings `ClassMethod(Rc<Class>, Rc<str>)`
(`src/value.rs:681`) and `GeneratorMethod(Rc<crate::coro::GeneratorHandle>, &'static str)`
(`src/value.rs:675`), each a **24-byte payload** (a 1-word `Rc` pointer + a 2-word fat `Rc<str>`/`&'static
str`). `Rc<str>` is itself two words (`src/value.rs:631`) and `Decimal` is 16 bytes (`Cargo.toml:19`), but
those are 16-byte payloads — the 24-byte two-field variants are what set the enum width. With the
discriminant the enum is therefore **32 bytes** (verified: `std::mem::size_of::<Value>() == 32` — a 24-byte
widest payload rounded with the tag to 32), and every clone of a heap variant is a refcount bump. After NUM
adds `Value::Int(i64)` and renames `Number → Float(f64)`, *the hottest values in any program — loop counters,
array indices, arithmetic temporaries — are scalars that still travel inside a 32-byte enum and still get
copied four words at a time through the VM value stack* (`fiber.push`/`fiber.peek`, `src/vm/run.rs`). That is
the tax this spec attacks.

The goal is to **shrink `Value` from 32 bytes toward a single 8-byte machine word and cut allocation /
refcount churn**, in independently-shippable stages, while holding the campaign's central invariant:
**`tree-walker == specialized-VM == generic-VM` byte-identical** over the whole corpus + goldens, in both
feature configs (`goal.md` Gate 1). VAL changes *how a value is stored*, never *what a program observes*.
Because the win is measured against the **true** 32-byte starting point (not the mistaken 24), it is *larger*
than first framed — a 4× collapse (32→8) at the NaN-box ceiling — and Stage 1 must explicitly box/shrink the
two 24-byte two-field variants (§4), the necessary **32→24** step. (The niche-enum reaches **16** only once
`Str` is ALSO thinned — Task 9 — and **8** only via the NaN-box; see the §3.3 CORRECTION. Boxing the two fat
variants + `Decimal` lands Stage 1 at **24**, not 16.)

Three things make this the right next step and not premature:

- **It is the real performance foundation, ahead of any JIT** (`goal.md` pillar 4; JIT is explicitly gated
  on NUM + VAL). A JIT that boxes every `f64` and threads a 32-byte enum through registers has a low
  ceiling; a JIT over inline scalars + tagged pointers is the LuaJIT/JSC/V8 design point.
- **NUM makes the scalar inlining *free of new semantics*.** `int` is already unboxed conceptually; VAL
  just stops paying heap/width cost to represent it. The hard semantic questions (exact comparison,
  overflow, Map-key folding) were all answered by NUM — VAL inherits them unchanged.
- **The single-threaded `!Send` `Rc`/`Cc` model is *preserved*, not rewritten** (a full `Arc`/`Send`
  rewrite was already rejected by the workers spec, **§12** — "Scope & rejected alternatives"). VAL is a
  within-isolate representation change; the airlock serializer and the worker model are unaffected except
  for a tag-table audit (§6).

### What this spec does NOT do

- It does **not** introduce a new `Value` *kind*, a new `type_name`, or any surface syntax.
- It does **not** make `Value: Send` or change the GC's threading model.
- It does **not** trace native-resource handles or acyclic `Rc` handles — the deterministic-`Drop`
  invariant (`src/gc.rs:23-41`, `goal.md` Gate 4) is sacrosanct and is *reinforced* by the new layout (§3.4).
- It does **not** commit the project to full NaN-boxing up front. NaN-boxing is the *aspiration*; §3.3
  delivers an **honest gcmodule-interaction verdict** and a staged fallback that ships real wins even if the
  full NaN-box is judged not worth its risk.

## 2. The shape of the win (where the bytes and the churn are)

| Source of cost | Today | After VAL |
|---|---|---|
| `Value` width | **32 bytes** (24-byte two-field payload — `ClassMethod`/`GeneratorMethod` — + discriminant/pad) | **8 bytes** (NaN-box, §3.2) or **16** (niche fallback WITH thin-`Str`, §3.3; **24** while `Str` stays the 2-word `Rc<str>`) |
| Widest two-field variants (`ClassMethod(Rc<Class>, Rc<str>)`, `GeneratorMethod(Rc<GeneratorHandle>, &'static str)`) | 24-byte payload each — they set the enum width | **boxed to a 1-word pointer** (Stage 1, §4), removing the floor that pins the enum at 32 |
| `Int`/`Float`/`Bool`/`Nil` on the value stack | enum move of 32 bytes | a single 8-byte word, no heap, no refcount |
| Small string (`"id"`, `"GET"`, single chars from NUM's codepoint methods) | `Rc<str>` alloc + refcount bump per clone | **inlined in the value word** (§5, stage 3), zero alloc |
| Short-lived object that never escapes (`{x, y}` returned/stored once) | `Cc<ObjectCell>` heap alloc + GC-tracked node | **stack/register-allocated** by the compiler (§5, stage 4), zero alloc, never GC-tracked |
| Clone of any heap `Value` | refcount increment (atomic-free but still a write) | unchanged for genuine heap values; eliminated for the inlined/stack-allocated cases above |

The first row — 32→8 — cuts the VM value stack (`Fiber.stack`), every frame slot array, every
`Vec<Value>` array element, and every `IndexMap` object/map slot to a **quarter** of their width, improving
cache density across the entire heap, not just arithmetic. Note the floor: until the two 24-byte two-field
variants (`ClassMethod`/`GeneratorMethod`) are boxed, *no* layout — niche-enum or NaN-box — can get below 32
bytes, because Rust sizes the enum to its widest variant. Boxing them is therefore the **first** Stage-1
deliverable (§4), not an afterthought; it takes the niche-enum floor from 32 to **24** (then Decimal-boxing
holds 24, since the fat `Str` is now the widest payload — **16** arrives only with thin-`Str`, Task 9; see
the §3.3 CORRECTION). So the "first row — 32→8" above is the NaN-box *ceiling*, reached in stages
32→24→16→8, not a single Stage-1 jump.

## 3. The representation

### 3.1 The current layout (grounded)

`Value` (`src/value.rs:623`) is a Rust enum whose **widest** payloads are the two two-field method-binding
variants — `ClassMethod(Rc<Class>, Rc<str>)` (`src/value.rs:681`) and
`GeneratorMethod(Rc<crate::coro::GeneratorHandle>, &'static str)` (`src/value.rs:675`) — each a **24-byte**
payload (a 1-word `Rc` + a 2-word fat second field). After those come the single-fat-field payloads `Rc<str>`
(2 words, `src/value.rs:631`) and `Decimal` (16 bytes, `src/value.rs:630`), then the various single-pointer
handles (`Rc<…>` / `Cc<…>`, each 1 word — `Cc<T>` is `RawCc(NonNull<RawCcBox<T,O>>)`, a single non-null
pointer; verified in gcmodule-0.3.3 `src/cc.rs:79`). Because Rust sizes an enum to its widest variant + a
discriminant that fits the remaining padding, the enum is **32 bytes** (verified empirically:
`std::mem::size_of::<Value>() == 32`). The cycle-collectable containers are `Cc`:
`Array`/`Object`/`Map`/`Set`/`Instance`/`Closure` + upvalue cells (`src/value.rs:639-662`,
`src/gc.rs:14-18`). Everything else — `Str`/`Bytes`/`Native`/`Regex`/`Enum`/`EnumVariant`/`Class`/`Function`/
`Future`/`Generator` and the two method bindings — stays on `Rc` with a no-op `Trace` (`src/gc.rs:24-41`).

### 3.2 The target: NaN-boxing (the aspiration)

A 64-bit IEEE-754 `double` has a vast unused encoding space: every bit pattern with all 11 exponent bits set
and a non-zero mantissa is *some* NaN, but the FPU only ever produces ONE canonical quiet NaN. That leaves
~2^51 spare payloads behind the NaN exponent. NaN-boxing reuses them to encode non-float values *inside the
8-byte word that would hold a double*:

- A word that is **not** a NaN-tagged pattern **is** an `f64` (`Value::Float`) — read directly, zero overhead.
- A word in the NaN space carries a fixed-width **tag** selecting `Nil` / `Bool` / `Int` (as a **small
  integer, "SMI"**) / heap-pointer, plus a payload (a pointer on every mainstream 64-bit OS uses ≤ 48 bits;
  an SMI uses the payload bits directly).

**The exact bit budget (one consistent arithmetic, stated once).** A 64-bit word splits as: bit 63 = sign,
bits 62–52 = the 11 exponent bits, bits 51–0 = the 52-bit mantissa. A value is a NaN-tagged non-float iff
the exponent is all-ones AND the value is not the single canonical quiet NaN the FPU produces; that gives the
**51-bit** mantissa-minus-quiet-bit space the technique reuses. We carve that space as a fixed **3-bit tag**
(8 tag slots: `Nil`, `Bool`, `Int`-SMI, and up to 5 heap-pointer-family tags) in the top 3 bits of the
reused space, leaving a **48-bit payload** — exactly enough for a tagged pointer on every mainstream 64-bit
OS (≤ 48 canonical address bits). The `Int`-SMI shares that **48-bit payload as a two's-complement signed
integer**, so the SMI range is **[−2^47, 2^47 − 1]** (`i48`). An i64 whose value lies **outside** that range
spills to a boxed `Int` (next paragraph). The spill threshold is therefore precise and testable: `|n| ≤
2^47 − 1` inlines; `n == −2^47` inlines; anything beyond spills. (Under the §3.3 niche-enum *fallback* there
is no NaN-box and `Int` is a full inline `i64` — no SMI, no spill; the SMI budget applies only to the Stage-2
NaN-box layout.)

This is the LuaJIT 2 / JavaScriptCore `JSValue` / V8-SMI design (§9). Concretely for AScript:

- **`Float`** — the value bits, verbatim. The common case (any non-NaN double) needs no decode.
- **`Int` as SMI** — NUM's `int` is i64, which is *wider* than the 48-bit NaN-box payload. So VAL inlines
  the **`i48`-fits** common case (range **[−2^47, 2^47 − 1]**, per the bit budget above) as an SMI tagged in
  the NaN space, and **spills the full-width i64 to a small pooled `Rc<i64>` box (a tagged-pointer `Int` heap
  kind)** for the rare magnitudes outside that range. This spill is invisible: `type_name`, arithmetic,
  comparison, printing, and Map-key folding (NUM §3.3) are all decode-then-operate, so an SMI and a boxed-i64
  of equal value are byte-identical. (Honest cost: a tiny branch on the i64 fast path to choose SMI vs.
  boxed, plus a re-check after each arithmetic op that could carry a result across the threshold — measured
  in §7, not assumed free.) The boundary unit test in §7.2 straddles `2^47 − 1` / `2^47` / `−2^47` /
  `−2^47 − 1` and asserts round-trip, arithmetic, comparison, and `MapKey` folding are byte-identical across
  the SMI↔boxed boundary on **both** engines.
- **`Nil` / `Bool`** — singleton tagged payloads, no heap.
- **Heap kinds** (`Str`/`Array`/`Object`/`Map`/`Set`/`Instance`/`Closure`/`Bytes`/`Native`/`Regex`/`Enum`/
  `Class`/`Function`/`Future`/`Generator`/`Decimal`/the various method bindings) — a **tagged pointer**: the
  48-bit pointer payload plus a tag (or a small secondary tag table) identifying which heap kind it is. The
  pointer is the *exact same* `Rc`/`Cc` pointer the enum holds today; tagging only sets unused high bits.

`Decimal` (16 bytes, not a pointer) does **not** fit a 48-bit payload, so under NaN-boxing it becomes a
**heap kind** (`Rc<Decimal>` or a small pooled box) like any other non-scalar — a representation change with
zero behavior change (still `type_name == "decimal"`, still exact). This is recorded as a cost (an extra
indirection for decimal arithmetic) and weighed in §3.3 / §8.

### 3.3 GC interaction — the first-class analysis (and the honest verdict)

The campaign's hardest correctness question for VAL is: **does NaN-boxing fight gcmodule's Bacon–Rajan cycle
collector?** A tagged pointer is no longer a bare `Cc<T>`; can the collector still find it, and can a `Cc`
still be reconstructed from a tagged word for refcount/drop?

**The decisive fact (verified in source):** gcmodule's collector is **registry-based, not a conservative
stack scanner**. The thread-local `ObjectSpace` maintains an **intrusive linked list of `GcHeader`s**
(gcmodule-0.3.3 `src/collect.rs:58-60`, `src/collect.rs:85-114`); `collect_thread_cycles()` walks *that
list* and calls each object's `gc_traverse` → our `Trace::trace` impls (`src/collect.rs:236`, `:310-324`),
which follow `Cc` edges **through the `Trace` impls we hand-wrote** (`src/gc.rs:176-309`). **The collector
never reads `Value` words off the VM value stack and never interprets a `Value`'s bit pattern as a pointer.**
It enumerates objects from its own registry and traces *outward* via `Trace`.

Two consequences:

1. **Reachability/rooting is unaffected by tagging.** A live `Cc` keeps its refcount > internal-edges via
   the *ordinary `Cc` clone/drop* the enum already does (`src/gc.rs:62-67`). NaN-boxing changes how a
   `Value` *stores* the pointer, but as long as a tagged-pointer `Value` **owns a real `Cc<T>`** (its
   `Clone` increments and its `Drop` decrements the refcount), the registry accounting is identical. The
   tagging is a *view*; the owned strong reference is real.

2. **Tracing still reaches every edge — IF `Value::trace` decodes before recursing.** The one mandatory
   change: `impl Trace for Value` (`src/gc.rs:176-206`) must **decode the tagged word back to the typed `Cc`
   and call its `.trace`** for the container kinds, exactly as today's match arms do. Because tracing is
   driven by `Trace` impls (not bit-scanning), a decode-in-`trace` is sufficient and sound. The container
   `Trace` impls themselves (`ObjectCell`/`ArrayCell`/`MapCell`/`SetCell`/`Instance`/`Closure`,
   `src/gc.rs:218-309`) are **untouched** — they receive a real `&self`, never a tagged word.

**Where NaN-boxing genuinely fights gcmodule — the honest part.** The real friction is not *finding*
pointers; it is **owning** them safely behind a tag:

- A tagged `Value` must run the **correct `Cc::clone`/`Cc::drop`** on `Clone`/`Drop`. With a bare enum the
  compiler generates this per-variant for free. With a hand-rolled NaN-box, `Clone`/`Drop` become
  **hand-written, tag-dispatched `unsafe`** that reconstructs the right `Cc<T>` (or `Rc<T>`) from the
  pointer bits and forwards. A single missed decrement is a leak; a single extra is a use-after-free. This
  is the same discipline JSC's `gcmodule`-free, hand-written `JSValue` lives under — *doable, but it moves a
  large, currently compiler-checked surface into `unsafe`*.
- `gcmodule::Cc` exposes **no public `from_raw`/`into_raw`**. Reconstructing a `Cc<T>` from a stored
  `NonNull<RawCcBox<T>>` is not a supported API; doing it via transmute couples VAL to gcmodule's private
  `#[repr(C)]` layout (`src/cc.rs:38-79`), which can change across versions. This is the **single biggest
  risk** in the whole spec.

**Verdict (recorded, not hand-waved):** NaN-boxing is **sound *in principle*** with gcmodule — the collector
is registry-driven, so a decode-in-`Value::trace` plus correct tag-dispatched `Clone`/`Drop` is a complete,
traceable design. It is **not free of risk**: it requires a hand-written `unsafe` ownership layer over
`Cc`/`Rc` whose correctness is no longer compiler-enforced, and it depends on either an upstreamed
`Cc::into_raw`/`from_raw` (preferred — a small PR to gcmodule) or a layout-coupling transmute (rejected as
fragile). **Therefore VAL stages the scalar win *first* and gates the full pointer NaN-box on (a) landing
`Cc::into_raw`/`from_raw` upstream and (b) the differential + GC-soundness suite (`src/gc.rs` V13-T4/T6
gates) passing on the tagged-pointer layout.** If either gate fails, VAL ships the **honest fallback** below
and stops there — a real, measured win without the `unsafe` pointer layer.

#### The honest fallback: a niche-optimized, manually-tagged smaller enum

If the full pointer NaN-box is judged not worth the `unsafe`/upstream-dependency cost, VAL still delivers
**inline scalars with no heap and no new `unsafe` over `Cc`**, by shrinking the enum rather than NaN-boxing
the pointers:

- Keep `Value` a Rust enum (compiler-generated `Clone`/`Drop` over `Cc`/`Rc` — **no hand-written ownership
  `unsafe`**), but **collapse every payload toward ≤ 8 bytes** so the enum shrinks stepwise. **CORRECTION
  (empirically measured during VAL Task 2, scratch-verified at the real variant count):** because the inline
  scalar variants (`Int`/`Float`) take any bit pattern, Rust cannot niche-elide the discriminant — the layout
  is `round_up(widest_payload) + 8-byte tag`. So the floor tracks the **widest payload width**, not a niche:
  with the 2-word fat `Str` it is **24** (16 + 8); thinning `Str` to one word reaches **16** (8 + 8); **8**
  bytes is reachable ONLY via the NaN-box (§3.2 — a hand-tagged machine word, not a Rust enum). (This corrects
  the earlier "fat-`Str` → 16" framing below, which was off by one level.) The stages:
  - **The two 24-byte two-field variants move first.** `ClassMethod(Rc<Class>, Rc<str>)` and
    `GeneratorMethod(Rc<GeneratorHandle>, &'static str)` each become a **single boxed payload** — one
    `Rc<ClassMethodData{class, name}>` / `Rc<GeneratorMethodData{handle, name}>` (a 1-word pointer). This is
    the load-bearing shrink **32 → 24**: while these stay 24-byte, the enum *cannot* drop below 32 (Rust sizes
    to the widest variant), so neither this fallback nor the NaN-box reaches 24/16/8 without it.
  - `Decimal` (16 B) and any other oversized inline payload move **behind a pointer** (`Rc<Decimal>` / a
    pooled box) — the same heap-kind move NaN-boxing forces, but without touching the pointer encoding. (This
    alone does NOT shrink the enum below 24, because the fat `Str` is now the widest payload — it removes the
    *other* 16-byte inline payload so that once `Str` is thinned the enum drops straight to 16.)
  - `Str` stays the 2-word `Rc<str>` only if we accept **24** bytes; to reach **16** we thin `Str` to a
    single-word pointer (a `Rc<StrHeader>`-style thin pointer / small-string — **Task 9**) — staged, optional.
    8 bytes needs the NaN-box, not a thinner enum.
  - The scalar variants (`Nil`/`Bool`/`Int`/`Float`) already fit a word; the win is that the *enum* no
    longer pads to 32 (→ 24 after the two boxings, → 16 after thin-`Str`).
- This uses Rust's **niche optimization** (`NonNull`/`Cc`'s non-null guarantee fills the discriminant niche)
  to keep the tag free where possible — the same mechanism that makes `Option<Cc<T>>` the size of `Cc<T>`.

The fallback yields **24-byte (fat-`Str`) → 16-byte (thin-`Str`, Task 9) values with inline scalars and zero
new ownership `unsafe`** (8 bytes only via the NaN-box, §3.2), at the cost of not collapsing *every* value to
a single word (a 16-byte value is still 2× better
than today's 32 and keeps the whole stage-1/2/3 win). It is the *guaranteed-shippable* floor; the NaN-box is the
*stretch ceiling*. Both are independently differential-tested.

### 3.4 Native handles & acyclic `Rc` handles under the new layout

This is a **strengthening**, not a complication. Today `Native`/`NativeMethod` (OS resources),
`Str`/`Bytes`/`Regex`/`Enum`/`EnumVariant`/`Class`/`Function`/`Future`/`Generator` are `Rc` with **no-op
`Trace`** and **must never be traced** (`src/gc.rs:24-41`, `:296`-style note; `goal.md` Gate 4). Under VAL:

- They become **tagged pointers (NaN-box) or boxed enum payloads (fallback)** holding the *same* `Rc<T>`.
  Their `Clone`/`Drop` run the ordinary `Rc` refcount ops; the resource table (`Interp.resources`,
  `src/gc.rs:819-847`) is untouched.
- **`Value::trace` decodes the tag and, for these kinds, does NOTHING** — exactly the `_ => {}` catch-all
  today (`src/gc.rs:204`). The collector still never reaches a native resource, so deterministic `fd`
  reclamation at `take_resource` / `Interp` drop is preserved bit-for-bit (the `src/gc.rs` V13-T6 gate,
  `:819-1096`, re-runs unchanged on the new layout and is a **required** VAL gate).
- Because the resource table is plain `HashMap<u64, ResourceState>` keyed by id (not a `Value` field), the
  representation of the *handle* is irrelevant to resource lifetime — VAL cannot regress it by construction.

## 4. Staging (each stage independently shippable and independently differential-tested)

VAL lands in four stages. **Each stage is a green merge on its own** — it must pass the four-mode differential
(`goal.md` Gate 1) and both clippy/test configs (Gates 2–3) before the next begins. A stage can be the
*stopping point* if its successor's risk/benefit doesn't justify it (the §3.3 verdict governs stage 2).

**Stage 1 — Inline scalars + SMI fast paths + box the two fat variants (the floor; depends on NUM).**
**First, box the two 24-byte two-field variants** `ClassMethod(Rc<Class>, Rc<str>)` (`src/value.rs:681`) and
`GeneratorMethod(Rc<GeneratorHandle>, &'static str)` (`src/value.rs:675`) into single `Rc`-boxed payloads
(`Rc<ClassMethodData>` / `Rc<GeneratorMethodData>`) — without this the enum stays pinned at 32 bytes (Rust
sizes to the widest variant) and *no* later shrink reaches 16/8. Then shrink the enum so
`Nil`/`Bool`/`Int`/`Float` are inline with no heap and move `Decimal` behind a pointer. Add the **SMI fast
path** in the VM arithmetic site (`src/vm/run.rs:3778` area) and adaptive arithmetic (`src/vm/adapt.rs` — NUM
already adds `ArithKind::Int`; stage 1 makes its operand load/store inline). This stage is the
niche-optimized fallback of §3.3 and ships *regardless* of the NaN-box verdict. Biggest single win (**32→16**,
scalars never touch the heap), zero new ownership `unsafe`. (`ClassMethod`/`GeneratorMethod` are rare, cold
bindings — the extra indirection on their construction/dispatch is negligible and measured in §7.)

**Stage 2 — Tagged pointers for heap kinds (the NaN-box, gated).**
Collapse the heap variants to tagged pointers and the whole `Value` to 8 bytes. **Gated on the §3.3 verdict**
(`Cc::into_raw`/`from_raw` upstreamed + GC-soundness suite green on the tagged layout). Introduces the
hand-written, tag-dispatched `Clone`/`Drop`/`trace` ownership layer (the `unsafe` surface). If the gate
fails, VAL stops at stage 1's 16-byte enum — a documented, owner-noted stopping point (`goal.md` Gate 6: a
deferral is a recorded decision, never a silent drop).

**Stage 3 — Small-string inlining.**
Inline strings up to N bytes (e.g. ≤ 6 under NaN-box, or a small-string-optimized `Str` under the fallback)
directly in the value, eliminating the `Rc<str>` alloc for keys/identifiers/short literals and the
single-codepoint strings NUM's `string.from_codepoints` produces. Display/concat/equality/`MapKey::Str`
folding all decode first, so an inline and a heap `"hi"` are byte-identical (a property test enforces it,
mirroring NUM's Map-key property test).

**Stage 4 — Escape-analysis stack allocation (compiler, `src/compile/`).**
A compiler pass that proves a constructed object/array **does not escape** its creating frame and allocates
it in **frame slots / registers** instead of a `Cc` heap node — so it is never GC-tracked and is reclaimed
by frame pop. This is the classic JVM/Graal escape-analysis + scalar-replacement optimization, scoped
conservatively (any uncertainty ⇒ heap-allocate, the sound default).

**Identity preservation is a SOUNDNESS OBLIGATION on the analysis, not a free property.** AScript object
identity is *observable*: `Array`/`Object`/`Map`/`Set`/`Instance` compare by **pointer identity** via
`crate::gc::cc_ptr_eq` in `Value::PartialEq` (`src/value.rs:707-723`), and that identity is reachable through
`==`/`!=`, the `is` operator, and use as an **identity-keyed `Map`/`Set` member**. A value whose identity can
be observed by any of these MUST be classified as **escaping** and heap-allocated — because two distinct
stack-replaced scalars would lose the shared-pointer identity a heap object has, changing an observable
`==`/`is`/Map-key result. The obligation is therefore stated precisely: *the analysis treats a constructed
container as non-escaping ONLY if it can prove the value is never (a) stored to a heap field, returned,
captured, or sent to a worker, AND (b) the operand of `==`/`!=`/`is`, AND (c) inserted into an
identity-keyed `Map`/`Set`, within its frame.* Any reachability of an identity observation ⇒ escaping. This
is a property of the **analysis**, owed and tested, not an assumed consequence of "it doesn't escape."
Behavior-identical: a value the analysis admits to the stack is, by this obligation, one whose identity is
never observed, so a stack-allocated and a heap-allocated instance are byte-identical for every reachable
operation. Independently differential-tested (the §7.1 fifth axis); the most speculative stage, justified by
profiling before it ships.

## 5. Determinism & the three-way differential (the core guarantee)

VAL is **pure representation**; the guarantee is that it is *invisible*.

- **Byte-identity holds by construction at the `Value` layer both engines share.** Every operation
  (`type_name`, arithmetic via `apply_binop`/`number_fast`/the NUM `int_fast`, comparison, equality,
  `is_truthy`, `Display`, `MapKey::from_value`/`to_value` folding, JSON, hashing) is **decode-then-operate**:
  it pattern-matches the value's *logical* kind, never its physical encoding. So an SMI `Int` and a boxed
  `Int`, an inline `"hi"` and a heap `"hi"`, a stack `{x}` and a heap `{x}` produce identical results and
  identical panic messages. The tree-walker (`src/interp.rs`) and both VM modes consume the same `Value`
  API, so `tree-walker == specialized-VM == generic-VM` is preserved (`goal.md` Gate 1).
- **The `--no-specialize` generic VM stays byte-identical.** The SMI / inline-string / stack-allocation fast
  paths are *encoding* optimizations under the value API, not specialization guards; the generic VM uses the
  same encoded `Value` and the same decode-then-operate ops. The three-way differential
  (`tests/vm_differential.rs`, both feature configs) is the guardrail: if generic and specialized ever
  diverge on the new layout, the encoding decode is wrong — fix the decode, never relax the assertion
  (`goal.md`: "fix the code, never the assertion").
- **Determinism (SP9) is untouched.** No new clock/RNG seam; representation has no observable timing-free
  effect on recorded nondeterminism. The `det.rs` context is unchanged.
- **Inline caches / shapes / adaptive arithmetic stay correct.** Shapes (`src/vm/shape.rs`) key on *keys*,
  not value encoding — unaffected. The field/method ICs (`src/vm/ic.rs`) store `(shape, index)` — unaffected.
  Adaptive arithmetic (`src/vm/adapt.rs`) guards on logical operand *kind* (`ArithKind::Number/Int/...`); the
  guard now reads an encoded word but resolves the same kind. Each fast path remains "guard the kind, then do
  the exact generic computation" (`src/vm/run.rs:3776-3804`) — VAL only makes the guarded load/store cheaper.

## 6. Implementation surface & cross-cutting checklist

Per `CLAUDE.md` "Touching syntax" (the AST/grammar half is **N/A** — no surface change) plus every subsystem
that *handles a `Value`*. **Each item is a required deliverable per stage it applies to.**

**Values & core (`src/value.rs`):** the new `Value` encoding (NaN-box or niche-fallback per §3.3);
hand-written or derived `Clone`/`Drop`/`PartialEq`/`Hash`/`Debug` as the chosen layout requires;
`is_truthy`/`type_name` decode-then-match; constructor/accessor helpers (`Value::int`, `Value::float`,
`Value::object(..)`, `as_int`/`as_object`/…) so the rest of the codebase is textually insulated from the
encoding (mirrors NUM's mechanical-rename discipline). `MapKey` unchanged (already canonical;
`src/value.rs:188`).

**GC (`src/gc.rs`):** `impl Trace for Value` **decodes the tag, then recurses** into the container `Cc`s
exactly as the current arms do (`:176-206`); the container `Trace` impls (`:218-309`) are untouched; the
native-handle catch-all stays a no-op (§3.4). The V13-T4 soundness gate and the **V13-T6 native-Drop gate**
(`:819-1096`) **re-run unchanged on the new layout and are required VAL gates**. If stage 2 lands the
`unsafe` ownership layer, add a `Cc`-roundtrip property test (encode→trace→decode→drop reclaims correctly;
no leak, no double-free under the soak gate).

**VM hot dispatch (`src/vm/run.rs`):** the SMI / inline-scalar fast paths in the arithmetic site
(`:3776-3819`) and anywhere `Value::Number`/`Value::Int` is loaded/stored on the fiber stack; the
inline-string fast path for concat/compare. Every change is "cheaper encode/decode under the same op," and
each is differentially tested.

**Shapes / ICs / adaptive (`src/vm/shape.rs`, `src/vm/ic.rs`, `src/vm/adapt.rs`):** no logic change; confirm
the guards read the decoded kind. Add tests that an SMI/boxed-`Int` mix and an inline/heap-`Str` mix do not
thrash or mis-specialize a cache.

**Compiler (`src/compile/`, stage 4 only):** the escape-analysis pass + scalar-replacement of non-escaping
allocations; frame-slot allocation of stack values; the conservative "any doubt ⇒ heap" default. Gated by a
`--no-escape-analysis` kill switch paralleling `--no-specialize`, so the optimization is independently
differential-tested against the un-optimized path (a fourth differential axis for stage 4).

**`.aso` (`src/vm/aso.rs` + `src/vm/verify.rs`):** **only if the constant-pool serialization layout changes.**
The pool serializes *logical* constants (`Value::Number` bits, etc., `:440`-era format); if stage 1 keeps the
on-disk constant encoding logical (recommended — serialize the decoded value, not the in-memory word), **no
`.aso` change and no `ASO_FORMAT_VERSION` bump is needed**. If any stage alters the *serialized* layout (e.g.
a new inline-string constant form), **bump `ASO_FORMAT_VERSION`** (`src/vm/aso.rs:105`, currently 18 — NUM
will have bumped it) and update `verify.rs`. The default and intent: **`.aso` stays logical; VAL is an
in-memory change.**

**Worker airlock (`src/worker/serialize.rs`):** the wire format is **logical, not memory-layout** (it tags by
*kind* — `TAG_NIL`/`TAG_NUMBER`/`TAG_DECIMAL`/…, the tag constants at `:80-89`; `encode_value` at `:380`,
`decode_value` dispatch at `:525` with e.g. `TAG_NUMBER => Value::Number(...)` at `:534`). VAL must **not**
change the wire tags (that would break NUM's just-added `Int` tag and cross-isolate compatibility). Required
deliverable: an **audit + round-trip test** that every `Value` kind still encodes/decodes identically
post-VAL, including SMI vs. boxed-`Int` (both serialize to the same logical `Int` tag) and inline vs. heap
`Str` (same `Str` tag) — so a value sent to a worker and back is byte-identical regardless of its sender-side
encoding.

**Both engines:** the tree-walker (`src/interp.rs`) consumes the new `Value` API unchanged (it never touched
the physical encoding); the VM consumes it via the fast paths above. The four-mode differential proves equality.

**Tooling (`fmt`/`lsp`/`repl`/`check`):** these consume `Value`/AST through the public API and the formatter
never renders a `Value`'s encoding — **expected no change**; add a regression smoke test in each that a
program round-trips identically (formatter idempotence, REPL cross-line, LSP hover) under the new layout.

**Docs / examples / grammar — ZERO additions, by design (Gate 9 N/A justification).** VAL adds **no surface
syntax, no new `Value` kind, no new `type_name`, and no observable behavior**, so it deliberately adds **zero
`examples/**` programs, zero grammar/tree-sitter changes, and zero `docs/content/**` pages or `NAV` entries**
— there is nothing user-visible to document or exemplify. This is the explicit Gate 9 ("ships an example +
docs") *justification*, not an omission: a reviewer should read the absence of an example/NAV entry as
correct for a pure-representation change, not a gap. The only doc touch-points are *internal*: `CLAUDE.md`
(the "Values" paragraph — note the compact encoding + the decode-then-operate invariant + the native-handle
reinforcement), the main design-spec performance section, `bench/` (the report, §7), and `roadmap.md`.

**Cross-spec coordination (merge order & the variant-adder tax).** This is load-bearing for the campaign
merge sequence and must be read by ADT/IFACE/SRV owners:

- **Stage-2's hand-written `unsafe` tag-dispatched `Clone`/`Drop`/`trace` removes the compiler's
  derive-for-free.** Once VAL Stage 2 lands the NaN-box, **every later variant-adder must hand-edit the
  `unsafe` ownership dispatch** instead of relying on `#[derive(Clone)]` + the generated `Drop`: ADT's
  `EnumVariant` `Rc→Cc` change, IFACE's new `Value::Interface`, and SRV's new `Value::Shared(Arc<…>)` each
  add a tag and therefore a new arm in the hand-written `Clone`/`Drop`/`trace` (and `PartialEq`/`Hash`).
  **Merge-order recommendation:** land the variant-adders (ADT/IFACE/SRV) *before* VAL Stage 2 where
  possible, so they add ordinary enum arms against the still-derived layout; if a variant-adder must follow
  Stage 2, its plan must budget the `unsafe`-arm edit + a `Cc`/`Arc`-roundtrip leak test as explicit work.
  (Stage 1 keeps the derived enum, so variant-adders landing before Stage 2 are unaffected.)
- **SRV's `Send` `Arc` leaf inside a NaN-box is a notable refcount-discipline interaction.** SRV introduces a
  `Value::Shared(Arc<SharedNode>)` whose leaf is `Send`/atomically-refcounted — a *different* refcount
  discipline (`Arc::clone`/`drop` use atomic ops) living behind the same tag space as the `!Send` `Rc`/`Cc`
  kinds. Under the NaN-box that tag's hand-written `Clone`/`Drop` must dispatch to `Arc` (atomic) ops, not
  `Rc`/`Cc` (non-atomic) — a per-tag distinction the `unsafe` layer must get exactly right. This does **not**
  make `Value: Send` (the `Arc` is reachable but the enum still holds `!Send` siblings); it is a
  within-`Value` mixed-refcount note for whoever writes the Stage-2 dispatch.
- **`assert_not_impl_any!(Value: Send)`.** VAL adds a compile-time `assert_not_impl_any!(Value: Send)` (and
  `: Sync`) next to the `Value` definition — via the `static_assertions` crate (a new lightweight dev/normal
  dependency; or an equivalent hand-rolled `const _: fn() = || { fn f<T: ?Sized>() {} /* negative bound */ }`
  guard if a new dep is unwanted) — so that no future edit — VAL's own NaN-box, or SRV's `Arc` leaf, or any
  variant-adder — can *silently* make `Value` `Send` (which would break the
  `#[tokio::main(flavor = "current_thread")]` + `LocalSet` invariant the whole runtime rests on, `CLAUDE.md`
  §"The interpreter"). A deliberate future decision to make `Value` `Send` would have to delete this assert,
  surfacing the choice.

**Unchanged:** the `Interp` async model, structured concurrency, the worker pool/scheduler, the resource
table, all stdlib *behavior*, every observable program output.

## 7. Testing & microbenchmarks (measured, not promised)

### 7.1 Differential (the gate)
- **Four-mode byte-identity** (`tests/vm_differential.rs`, both feature configs): the whole `examples/**`
  corpus + goldens produce identical output on tree-walker, specialized VM, generic VM, and `.aso`-compiled
  — **per stage**. This is the non-negotiable gate (`goal.md` Gate 1); VAL adds no new goldens (no behavior
  change) but re-runs all existing ones on each layout.
- **Stage-4 fifth axis:** `--escape-analysis` vs `--no-escape-analysis` differential, proving stack-allocated
  and heap-allocated objects are observationally identical.
- **GC soundness re-run:** `src/gc.rs` V13-T4 (every cycle class reclaimed) and **V13-T6 (native-Drop
  determinism)** pass unchanged on the new layout; plus the stage-2 `Cc`-roundtrip leak/double-free property
  test under the V13-T5-style soak.

### 7.2 Property & boundary tests (no-bugs pillar)
- **SMI↔boxed spill-boundary unit test (required, both engines).** A targeted unit test straddling the exact
  spill threshold of the §3.2 bit budget — the values `2^47 − 1`, `2^47`, `−2^47`, `−2^47 − 1`, plus a few
  beyond (`2^53`, `i64::MAX`, `i64::MIN`) — asserts that for each: **round-trip** (`decode(encode(n)) == n`),
  **arithmetic** (a `+1`/`-1` that carries a value across the boundary yields the right kind+value),
  **comparison** (`<`/`==` across an SMI and a boxed operand of equal/adjacent value), and **`MapKey` fold**
  (an SMI and a boxed `Int` of equal value are the *same* Map key) are byte-identical on **tree-walker ==
  specialized-VM == generic-VM**. This is the concrete guard that the SMI/boxed boundary has no off-by-one.
- **Encoding round-trip:** `∀ v: decode(encode(v)) == v` (logical equality + `type_name` + `MapKey` fold),
  across SMI/boxed-`Int`, inline/heap-`Str`, every heap kind.
- **No-divergence:** SMI vs boxed-`Int` and inline vs heap-`Str` produce identical results for every op
  (arithmetic, compare, Map-key, JSON, hash, display) — the encoding is invisible.
- **Hooks into FUZZ** (continuous infra): the encoding round-trip and the layout-invariance properties are
  handed to the FUZZ harness as priority targets (`goal.md` FUZZ; the GC is already a named FUZZ target).

### 7.3 Microbenchmarks (`bench/`, reusing `src/stdlib/bench.rs`)
A benchmark harness writing a markdown report (sibling to `bench/PROFILING_RESULTS.md`, mirroring the workers
spec's reporting discipline), reporting **measured, not promised** figures:
- **`size_of::<Value>()`** before/after each stage (the headline structural fact: **32 → 16 → 8**, where the
  first 32→16 step *requires* boxing the two fat variants per Stage 1).
- **Two perf axes per workload — specialized AND `--no-specialize` (generic VM).** Every wall-clock
  microbenchmark below runs in **both** VM modes: the default specialized VM *and* the generic VM
  (`Vm::new_generic` / `--no-specialize`). **Gate 12 requires no regression in *either* mode** — VAL's win is
  an encoding optimization under the value API, not a specialization guard, so the generic VM (which skips
  every IC/adaptive/global fast path) must benefit from the smaller `Value` too and must not regress. A
  generic-mode regression is a VAL bug, not an acceptable trade. The report tabulates both columns side by
  side.
- **Scalar-heavy loops** (integer sum, array index walk, Mandelbrot/Fibonacci over NUM ints) — VM wall-clock
  before/after stage 1, isolating the inline-scalar/SMI win, in both modes.
- **Allocation/refcount churn** — a `Cc`/`Rc` clone counter (or `dhat`/heaptrack) over an
  object-heavy workload before/after stages 3–4, quantifying eliminated allocations.
- **Cache-density proxy** — throughput on a large-`Vec<Value>`/large-`IndexMap` traversal, where 32→8
  should show up as memory-bound speedup, in both modes.
- **Cold-path check:** a `ClassMethod`/`GeneratorMethod` construction+dispatch microbench confirming the
  Stage-1 boxing of the two fat variants adds no *measurable* regression on those (rare) bindings.
- **Honest framing:** the report states the *measured* geomean over the existing perf slice **for each VM
  mode** and explicitly flags any regression (e.g. the SMI-spill branch on huge ints, the extra `Decimal`
  indirection, the boxed-method-binding indirection). **No speedup is claimed in this spec** — the number is
  whatever `bench/` reports. The expectation (documented, not a hard CI gate beyond the Gate-12
  no-regression-either-mode floor): stage 1 alone meaningfully improves scalar throughput and shrinks `Value`
  from 32 to ≤ 16 bytes; stage 2 reaches 8 bytes; the report substantiates which workloads benefit and by how
  much.

## 8. Scope & rejected alternatives

**In scope:** the compact `Value` encoding (NaN-box aspiration with the niche-optimized fallback floor);
inline `Nil`/`Bool`/`Int`(SMI)/`Float`; tagged pointers (or boxed payloads) for heap kinds; small-string
inlining; compiler escape-analysis stack allocation; the GC-`trace` decode; the worker-wire and `.aso`
audits; the four-mode + property + soundness test suite; microbenchmarks. Staged, each independently
shippable and gated.

**Out of scope / deferred (recorded per `goal.md` Gate 6):**
- **Full NaN-box pointer layer if its gate fails (§3.3).** Stage 2 is *gated*; if `Cc::into_raw`/`from_raw`
  isn't available upstream or the GC-soundness suite fails on the tagged layout, VAL stops at the niche
  fallback — an owner-noted stopping point, not a silent drop.
  > **GATE VERDICT (2026-06-09, plan Task 5): STOP — Stage 2 (NaN-box → 8 B) DEFERRED.** gcmodule 0.3.3 (the
  > pinned dependency) exposes **no public `Cc::into_raw`/`from_raw`** (verified against the crate source: only
  > private `Box::into_raw`/`from_raw` over the internal `RawCcBox`). The supported fix (a small upstream PR)
  > is external/multi-week/out-of-campaign-scope, and the private-`RawCcBox` `transmute` is rejected as
  > fragile (§8). So VAL stops at the **niche-optimized enum** and pursues Stage 3 (thin-`Str`, Task 9) to the
  > **16-byte** floor (Stage 1 lands at 24 — see the §3.3 size CORRECTION; 8 B needs the NaN-box). 8 B is a
  > future follow-up IF gcmodule gains the raw API. Tasks 6–8 are skipped.
- **A bump/region allocator or a GC rework.** The cycle-collector rework (if collection pauses ever dominate
  under server load) is an explicitly sanctioned *future* deferral (`goal.md`), not in VAL.
- **The JIT.** Gated on NUM + VAL by design (`goal.md`); VAL is its prerequisite, not its scope.

**Rejected:**
- **Full `Arc`/`Send`/concurrent-GC rewrite to make `Value` `Send`.** Already rejected by the workers spec
  (**§12** "Scope & rejected alternatives", grounded in §13): a measured 5–32% single-threaded tax (Swift BRC
  PACT'18; CPython PEP 703), structurally incompatible with the determinism/replay design, and unnecessary —
  workers give multicore via isolation. VAL is *within-isolate* and preserves `!Send` `Rc`/`Cc` (`CLAUDE.md`
  §"The interpreter"); the `assert_not_impl_any!(Value: Send)` of §6 locks this in.
- **Leaving `Value` fat (32 bytes, box every number).** The status quo this spec exists to fix: it caps VM
  throughput, bloats every stack slot / array element / map slot, and gives a future JIT a low ceiling
  (`goal.md` pillar 4). Rejected as the thing being replaced.
- **A layout-coupling `transmute` to reconstruct `Cc` from raw bits (instead of upstreaming
  `Cc::into_raw`/`from_raw`).** Rejected as fragile — it hard-couples VAL to gcmodule's private
  `#[repr(C)] RawCcBox` layout (`src/cc.rs:38-79`), which can change across patch versions. The supported
  path is a small upstream PR adding `into_raw`/`from_raw`; absent that, the niche fallback (no raw `Cc`
  manipulation) ships instead.
- **Conservative stack scanning for the GC** (so untyped tagged words could be roots). Unnecessary —
  gcmodule is registry-driven (§3.3), so precise tracing via decode-in-`trace` already works; introducing a
  conservative scanner would be strictly more complexity and risk for no benefit.
- **Tagged-union via a `#[repr(C)]` struct + manual discriminant instead of NaN-boxing.** Considered; it is
  essentially the niche fallback without the niche optimization, so it ships *more* bytes for the same
  `unsafe`-free property. The Rust-niche fallback dominates it.

## 9. Grounding (verified sources)

- **NaN-boxing precedent:** LuaJIT 2 value representation (Mike Pall, "NaN tagging" / `lj_obj.h` `TValue`);
  the canonical write-up of the technique (Piotr Duperas / "NaN boxing or how to make the world dynamic").
- **SMI (small-integer) inline tagging:** V8's `Smi` (31/32-bit tagged small integers, pointer-tagging in
  `v8::internal::Object`); the same tagged-pointer-vs-SMI dispatch AScript's `Int`-as-SMI adopts.
- **Tagged `JSValue`:** JavaScriptCore's `JSValue` (the "double / pointer / immediate" encoding;
  `JSCJSValue.h`), the hand-written-ownership precedent for §3.3's `Clone`/`Drop` layer.
- **Escape analysis + scalar replacement (stage 4):** HotSpot / GraalVM partial-escape-analysis and
  scalar-replacement-of-aggregates — the proven "prove-it-doesn't-escape, then stack/register-allocate"
  optimization, scoped conservatively here.
- **gcmodule internals (verified in-tree, gcmodule-0.3.3):** `Cc<T>` is a single `NonNull<RawCcBox<T,O>>`
  (`src/cc.rs:79`); the collector is registry-driven over an intrusive `GcHeader` linked list
  (`src/collect.rs:58-114`, `:236`, `:310-324`) — it traces via `Trace` impls, never by scanning value words.
  This is the fact the §3.3 verdict rests on.
- **`!Send` `Rc`/`Cc` model & the rejected `Send` rewrite:** AScript `CLAUDE.md` §"The interpreter" / §"Values";
  the workers Spec A **§12** ("Scope & rejected alternatives", grounded in §13 — Swift BRC PACT'18; CPython
  PEP 703) — VAL preserves this model. (§10 is the workers spec's *implementation surface*, not the rejection.)
