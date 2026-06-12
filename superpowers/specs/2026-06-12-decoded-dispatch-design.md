# AScript Pre-Decoded Dispatch + Data-Driven Superinstructions + Speculative Global-Fn Inlining — Design (DECODE)

- **Status:** Draft for review
- **Date:** 2026-06-12
- **Code:** DECODE (the dispatch spec of the PERF campaign — see `goal-perf.md`)
- **Depends on:** **LANE merged** (the sync driver `run_loop_sync` — exported contract:
  `SyncOutcome::{Finished, NeedsAsync}` — is the decoded
  stream's primary — and only v1 — consumer; LANE Task 0's bench corpus + `bench/ab.sh` is the
  measurement instrument). CALL/SHAPE may land before or after — DECODE touches neither the
  call-path allocation story nor object storage.
- **Depended on by:** **JIT** — twice over. (1) The **invalidation machinery** this spec builds
  (`Chunk.patch_epoch`, the embedded-chunk dependency epochs, the drop-don't-edit rule, §4) is
  exactly what compiled native code must consume — the JIT spec
  (`superpowers/specs/2026-06-08-baseline-jit-design.md`) **predates DBG's `Code` byte-patching**
  (DBG merged 2026-06-10) and has no answer for "a breakpoint/coverage patch lands in a compiled
  function"; DECODE builds and battle-tests that answer first. (2) The decoded fixed-width IR is
  the natural input a baseline translator walks (operands widened, jumps pre-resolved) — the JIT
  rehearsal, per `goal-perf.md`'s DECODE entry.
- **Engines:** **VM only.** The tree-walking interpreter is UNTOUCHED — the permanent
  byte-identity oracle. Decoded-on must equal decoded-off must equal the tree-walker, always.
- **Breaking:** **no.** `Chunk.code` stays **BYTE-IDENTICAL** — the disassembler
  (`src/vm/disasm.rs`), the goldens, the verifier (`src/vm/verify.rs`), and the `.aso` format are
  all untouched (`ASO_FORMAT_VERSION` stays at its current value, 27 at drafting —
  `src/vm/aso.rs:167`). The decoded stream is a per-`FnProto`, lazily-built, in-memory **side
  representation** following the `arith_cache` side-table precedent (`src/vm/adapt.rs` module doc:
  "Why a side map, not in-place quickening"; `src/vm/chunk.rs:425-435`). No syntax change, no
  semantics change, no new serialized byte anywhere.

> **Grounding caveat:** every `file:line` below was verified on `main` 2026-06-12, **before LANE
> merges**. LANE adds `run_loop_sync`/`SyncOutcome`/`sync_lane` to `src/vm/run.rs` and will shift
> line numbers. The plan's Task 0 re-greps every citation; the *named* anchors (function names,
> field names) are the stable references.

---

## 0. Read this first — the one-paragraph design

Today every instruction dispatch re-decodes its opcode byte and walks `Op::operand_width`
(`src/vm/opcode.rs:788`) to find its variable-width little-endian operands, and every jump does
signed byte-displacement arithmetic (`run.rs:2172-2191`). DECODE builds, per `FnProto`, **once it
is warm**, a flat `Vec` of fixed-width records — opcode widened, operands pre-read into `u32`
fields, **jump targets pre-resolved to record indices** — and the LANE sync driver executes from
those records instead of re-decoding bytes. On top of that flat IR, two transformations that are
impossible (by design) in the byte stream become trivial: **superinstruction fusion** (a peephole
over decoded records, candidates chosen from committed dynamic-frequency data, never guessed) and
**speculative inlining of small hot global functions** (the callee's decoded body spliced into the
caller's record stream behind the existing `struct_gen` global-version guard plus a closure
identity check, deopting to the untouched generic `Op::Call` record on any miss). A fourth,
evidence-gated unit — **top-of-stack register caching** (Unit D, §7) — keeps the burst's TOS in a
Rust local instead of `fiber.stack`'s last slot, flushed back at every burst exit so the Fiber
keeps externalizing complete state; it is measured against Unit B's residual stack-traffic data
and ships only on a measured win (or is dropped with numbers, like Unit C). `Chunk.code`
never changes; everything decoded is droppable at any moment — and **is** dropped whenever a
debugger/coverage byte-patch touches any chunk it embeds, via a single epoch bump inside
`Chunk::patch_byte` itself. That drop-on-patch contract, proven here by a test battery, is the
contract a future JIT's compiled code consumes verbatim.

## 1. Summary & motivation (the measured evidence)

`bench/PROFILING_RESULTS.md` (Phase-0, 2026-06-06) attributes `object_churn` — the tight-loop,
dispatch-bound workload where the VM's 2.5× advantage lives — as **dispatch/VM 49%** (`run_loop`
18%, `Fiber::frame` 9%, push/pop 6%). `goal-perf.md`'s code-confirmed constant factors include:

- **Variable-width operand decode per instruction** through `Op::operand_width` matches
  (`src/vm/opcode.rs:788`); no pre-decoded representation exists. Each dispatch does:
  `Op::from_u8(code[ip])` (a 110-arm cascading compare, `opcode.rs:639-784`) → `ip += 1 +
  op.operand_width()` (a second full match) → per-arm `read_u16`/`read_u8`/`read_i16` re-reads of
  the operand bytes (`run.rs:1097-1107`). Three passes over the same bytes, every instruction,
  forever.
- Jumps re-read an `i16` displacement and do signed arithmetic per taken branch
  (`run.rs:2172-2224`); a pre-resolved target is one unconditional index assignment.
- Call overhead dominates small-fn-heavy code — the CALL spec's evidence
  (`superpowers/specs/2026-06-12-call-path-diet-design.md`; `goal-perf.md`: ≥3 allocations/call,
  `check_call_args` on every call, `src/vm/run.rs:1757-1827`). CALL removes the allocations;
  DECODE Unit C removes the *dispatch and frame machinery itself* for the smallest, hottest,
  guard-stable callees — the classic complement.

What DECODE deliberately does **not** claim: the async-runtime tax (LANE/EXEC territory),
allocation/hashing (SHAPE/NANB/CALL), fsync (WARM). Decoding is a constant-factor attack on the
49%-dispatch class of workload plus whatever the fusion census proves is hot; expectations are
stated in §9 and **measured, never promised** (campaign Gate 16).

### 1.1 Why now, in this order

LANE made the sync driver the place where suspension-free instructions retire — a plain loop the
compiler can optimize, with a single entry/exit protocol (`SyncOutcome::NeedsAsync` at an
un-advanced `ip`). That driver is the perfect — and only — consumer for a decoded stream: it owns
the hot dispatch, and its escalation protocol already defines the byte-ip handoff discipline the
decoded↔byte mapping needs (§3.4). Building DECODE before the JIT re-profile is the campaign
order: if pre-decoding + fusion + inlining close the dispatch gap, the JIT's gate may never open —
and if they don't, the JIT inherits a proven IR and a proven invalidation story instead of
inventing both under codegen pressure.

## 2. Unit A — the pre-decoded instruction stream

### 2.1 The operand survey (drives the record layout)

Every operand shape in the ISA, from `Op::operand_width` (`src/vm/opcode.rs:788-883`), verified
exhaustive over all 110 opcodes:

| shape | ops (representative) | total inline bytes |
|---|---|---|
| zero-operand | `Add`, `Return`, `GetIndex`, `Await`, `Break`, … (60 ops) | 0 |
| `u8` | `Call`, `MatchRange`, `RangeStepValue`, `RangeResolveStep`, `RangeHasNext` | 1 |
| `u16` | `Const`, `GetLocal`, `GetGlobal`, `Closure`, `GetProp`, … (38 ops) | 2 |
| `i16` (jump) | `Jump`, `JumpIfFalse`, `JumpIfTrue`, `JumpIfNotNil`, `Loop` | 2 |
| `u16` + `u8` | `CallMethod`, `MatchArray`, `DefineGlobal`, `CallNamed` | 3 |
| `u16` + `i16` | `JumpIfArgSupplied` | 4 |

**Facts the layout exploits:** (a) no op carries more than **two** operands; (b) every single
operand is at most **16 bits** wide — so one `u32` field can hold any operand with room to spare,
and a fused record can pack **two** base operands into one `u32` (lo/hi `u16` halves). Jump
displacements (`i16`, measured from the byte after the operand — `chunk.rs:603-646`) are resolved
at decode time to **absolute record indices**, which fit `u32` trivially (a chunk caps at 32 KB
per jump span and `u16::MAX` consts; record counts are bounded by code length).

### 2.2 The record and the `DecodedChunk`

```rust
/// DECODE: one fixed-width pre-decoded instruction. 16 bytes, align 4.
/// `a`/`b` hold the widened operands (or a packed pair / record-index jump
/// target / fused payload — per-op layout documented on `DOp`). `off` is the
/// byte offset of this record's FIRST source instruction in its owning chunk —
/// the ip↔record bridge (§3.4): escalation writes it back as the fiber's byte
/// ip; span lookup runs `chunk.span_at(off)` exactly as byte dispatch does.
pub(crate) struct DecodedInstr {
    pub op: DOp,    // 2 bytes (Base(Op) niches into the u16; fused/synthetic beyond)
    pub a: u32,
    pub b: u32,
    pub off: u32,
}

/// The decoded operation. `Base` is a pass-through of the real ISA; everything
/// else exists ONLY in the decoded stream — never in `Chunk.code`, never
/// serialized, invisible to the verifier and the disassembler.
pub(crate) enum DOp {
    /// A 1:1 decoded base instruction (operands widened into a/b).
    Base(Op),
    /// Unit B fused records — the reviewed FUSION_CANDIDATES set (§5). Example:
    /// GetLocal s1; GetLocal s2; Add  →  LLAdd { a: s1 | s2 << 16, b: add_off }.
    /// Each variant's a/b packing + fault-offset word is documented at the
    /// variant (the panic span must attribute to the faulting COMPONENT).
    LLAdd, LLBinOp, LConstBinOp, GetLocalProp, /* … the census-chosen set … */
    /// Unit C inline records (§6).
    InlineEnter, InlineExit,
}

/// The per-FnProto decoded side representation (the arith_cache precedent:
/// runtime-only, lazily built, never serialized, droppable at any time).
pub(crate) struct DecodedChunk {
    pub records: Vec<DecodedInstr>,
    /// Sorted (byte_off, record_idx) for every record whose `off` is a
    /// CALLER-chunk offset (i.e. every record outside inline segments). Binary-
    /// searched at burst entry to convert the fiber's canonical byte ip to a
    /// record index. Every legal resume point is present (§3.4).
    pub entry_index: Vec<(u32, u32)>,
    /// Validity: the patch epoch of the OWNING chunk at decode time, plus one
    /// (chunk-identity, epoch) entry per FOREIGN chunk whose records were
    /// embedded by inlining (§4.2). Stale ⇒ drop, never edit.
    pub own_epoch: u64,
    pub deps: Vec<(Rc<FnProto>, u64)>,
    /// Whether any inline segment exists (drives the instrument-armed validity
    /// rule, §6.6) + the segment table for span/source attribution (§6.4).
    pub inline_segments: Vec<InlineSegment>,
}
```

Storage: `Chunk` gains two side fields, exactly like the IC/adaptive maps —
`decoded: RefCell<Option<Rc<DecodedChunk>>>` and `decode_warmth: Cell<u16>` (plus
`patch_epoch: Cell<u64>`, §4.1). All three are runtime-only `Default`s: the `.aso`
reader/writer never sees them (`aso.rs` untouched), `verify.rs` untouched, `disasm.rs` untouched
— it reads `Chunk.code`, which never changes.

**Memory accounting (honest, Gate 18):** 16 B/record + 8 B/`entry_index` entry against an average
~1.8 bytes of bytecode per instruction ⇒ roughly **12–15× the bytecode size** for decoded protos.
Bytecode is small in absolute terms (the whole `examples/` corpus compiles to well under a MB),
and lazy decode-on-warmth means cold protos pay **zero**. The decode-stats test entry reports
total decoded bytes per run; `bench/DECODE_RESULTS.md` reports it per workload alongside peak RSS
(`/usr/bin/time -l` via `bench/ab.sh`), and a peak-RSS regression is a bug, not a trade.

### 2.3 When to decode — warmth, not first-run (benchmarked, then pinned)

Decoding is lazy and **threshold-gated**: each frame entry of a proto bumps `decode_warmth`
(saturating); at `DECODE_THRESHOLD` the chunk decodes and installs `Some(Rc<DecodedChunk>)`. The
threshold follows the `adapt.rs` `WARMUP_THRESHOLD` precedent (`src/vm/adapt.rs:44`, = 8). The
brief's open question — decode-on-hot vs decode-on-first-run — is settled **by measurement**
(plan Task 11): both configurations run the full A/B corpus; the shipped default is whichever
wins (expectation: a small threshold ≈ 8 — first-run decoding taxes one-shot/startup code such as
module top-levels for no return; the result and the chosen constant are recorded in
`bench/DECODE_RESULTS.md`). Tests use a forced mode (`DECODE_THRESHOLD = 0` via a test-only
constructor knob) so the differential actually exercises records on short programs — the JIT
spec's anti-false-green rule (JIT spec §5.1) applied here from day one.

A second reason on-hot is the safer default: decoding reads the **current** bytes, so a byte
patched to `Op::Break` decodes as a `Base(Break)` record — an escalation record (§4.3). Decoding
under an active coverage run therefore re-decodes as lines un-patch; the threshold amortizes
that re-decode churn (coverage overhead stays a reported, attached-mode cost — `vm_bench.rs`'s
`cov/off` section already reports it).

### 2.4 The decoded driver — one set of arm bodies, two instruction sources

The decoded stream is consumed by **the LANE sync driver only** (a deliberate narrowing of the
brief's "both drivers", recorded in §10): the async orchestrator executes exactly one *escalation*
op per iteration post-LANE, and an escalation op's body (a spawn, an await park, an import, a
native call) dwarfs its decode cost — putting records under the async arms would buy nothing
measurable while touching the very loop whose lane-off form is the shipped kill-switch path
(LANE §9 rejected exactly that kind of restructuring). The async loop is **untouched** by DECODE.

To avoid a third transcription of the sync arms (LANE already transcribed them once into
`run_loop_sync`'s burst loop; a hand-copied decoded twin would be a drift surface the
differential would have to police forever), DECODE extracts that loop body as **`sync_burst`** —
DECODE's own internal name (LANE's exported contract is `run_loop_sync`/`SyncOutcome`; if LANE's
implementation already factored an inner helper, the plan's Task 0 records its name and the
extraction reuses it) — made **generic over an instruction source**:

```rust
/// DECODE: the per-burst instruction source. Two monomorphized impls — the
/// byte fetcher (today's LANE behavior: decode code[ip], operand_width walk)
/// and the record fetcher (read records[idx]). The shared `sync_burst<S>` arm
/// bodies are THE single source of truth for sync-subset semantics; only
/// fetch/advance/jump mechanics differ per source.
pub(crate) trait InstrSource {
    /// Fetch the next instruction (op + widened operands + byte off of the
    /// opcode byte). Returns None to escalate (out-of-subset op / stale
    /// stream / pending await), leaving the canonical byte ip exact.
    fn fetch(&mut self, fiber: &Fiber) -> Fetch;
    /// Take a jump: byte-displacement arithmetic (byte source) or an
    /// unconditional record-index assignment (record source).
    fn jump(&mut self, fiber: &mut Fiber, target: u32);
    /// Write the canonical byte ip back into the fiber (burst exit / Err).
    fn sync_ip(&mut self, fiber: &mut Fiber);
}
```

`run_loop_sync` picks the source per frame entry: if `self.decode && frame.proto.chunk` has a
**valid** (§4.2) decoded stream, burst on `RecordSource`; else bump `decode_warmth`, possibly
decode, else burst on `ByteSource` (the byte instantiation is LANE's shipped behavior,
monomorphized — proven behavior-preserving by the full differential before any record code
lands; plan Task 4). The fused/inline `DOp` arms exist only in the record instantiation's
extension match. The byte instantiation compiles to the same shape as LANE's hand-written loop
(one `match` over `Op`), and the Gate-12/17 re-runs hold the floor.

**`last_fault_source` hoisting (observation-equivalence argument, supplied as LANE's recorded
follow-up).** Byte dispatch refreshes `last_fault_source` per instruction (`run.rs:1092-1096`);
its **only reader** is the escaping-panic binder in `Vm::run` (`run.rs:1066-1073`, verified by
grep — no other read site). The cell's value is a per-chunk constant (the chunk's module
source), so refreshing it at **frame entry + inline-segment entry/exit** (where the chunk
changes) instead of per record is observationally identical: at any panic, the cell holds the
faulting chunk's source either way. The decoded driver hoists; byte dispatch keeps the
per-instruction refresh (untouched). A cross-module-panic differential test pins it (plan
Task 4).

### 2.5 What gets decoded — subset, escalation, jumps

Decoding covers the **whole chunk** (every instruction becomes a record — escalation ops
included, as `Base(op)` records the driver refuses to execute), because jump-target resolution
needs the complete instruction index anyway. Execution-wise the decoded driver runs exactly
LANE's sync subset (LANE §3 — the `sync_lane_op` allowlist, including the conditional
`Call`-peek and the `Await` ready-future probe): an out-of-subset record returns `NeedsAsync`
with the fiber's byte ip set to `rec.off`, and the async driver re-decodes that byte and runs its
unchanged arm — the identical handoff contract LANE defined, just sourced from a record's `off`
instead of an un-advanced byte ip. DEFER's `DeferPush`/`DeferPushMethod` decode 1:1 as ordinary
in-subset `Base` records (they only push a bound thunk — LANE §3 "DEFER coordination"); **frame
exit with a non-empty `frame.defers` is an escalation edge** (the `Return`/`Propagate` record
returns `NeedsAsync` per LANE's classification — §7.2's flush edge 1).

Jumps: at decode time the resolver maps each jump's byte target (`after-operand + disp`,
`chunk.rs:603-646`) to the index of the record whose `off` equals it. A byte target that lands
mid-instruction is impossible for compiler-emitted code (`emit_jump`/`emit_loop` only target
instruction starts); the decoder **verifies** it anyway and refuses to decode the proto on any
anomaly (fall back to byte dispatch forever — a defensive posture, not a reachable path; the
`.aso` verifier already rejects malformed jump targets at the trust boundary).

## 3. Mapping discipline — ip ↔ record index (the deopt/handoff seams)

**The canonical stored form is the byte ip**, everywhere, always: `CallFrame.ip`
(`src/vm/fiber.rs:23`) keeps today's meaning; suspended generator fibers, paused debugger frames
(`build_frame_snapshots`), the DAP stack trace, error spans (`chunk.span_at(off)`,
`chunk.rs:861-874`), and the profiler all read byte offsets exactly as today. Record indices are
**burst-local transients** that never escape the decoded driver:

1. **Entry (byte → record):** at burst/frame entry, binary-search `entry_index` for the fiber's
   byte ip. Every legal resume point is present: function entry (`off` 0), the instruction after
   any escalation record (escalations end bursts; the following record is never a fused middle —
   §5.2's entry-point rule), the instruction after `Yield`, and every jump target. A lookup miss
   means the stream is stale or the ip is foreign — fall back to byte dispatch for the burst
   (never panic; byte dispatch is always correct).
2. **Exit (record → byte):** on `NeedsAsync`, `Finished`, `Err`, and frame push, the driver
   writes `records[cur].off` (the *not-yet-executed* or faulting instruction's byte offset) into
   the frame's ip — after which the world is exactly as if byte dispatch had run the whole burst.
3. **Spans:** a record's panic span is `owning_chunk.span_at(fault_off)` where `fault_off` is the
   record's fault-attribution offset — `off` for 1:1 records, the faulting **component's** byte
   offset for fused records (packed per §5.3), and a *callee*-chunk offset inside inline segments
   (§6.4). Byte-identical messages + spans are asserted by the panic batteries and the corpus.

Inside inline segments, `off` values are **callee**-chunk offsets and are deliberately absent
from `entry_index` (they are not caller resume points); no suspension can originate inside an
inline segment (§6.3), so no foreign ip can ever be stored in a caller frame.

## 4. The invalidation contract (§ addressed to the future JIT)

### 4.1 The single chokepoint: `Chunk::patch_byte` bumps `Chunk.patch_epoch`

DBG breakpoints and DX coverage mutate live bytecode through the `UnsafeCell`-backed `Code`
newtype (`src/vm/chunk.rs:296-376`; `Code::patch_byte:332`, `Chunk::patch_byte:779`). The
complete patch-site inventory, verified by grep over `src/`:

| site | what | where |
|---|---|---|
| coverage arm | patch every line-start to `Break` | `Vm::arm_coverage`, `run.rs:362-410` (patch at `:407`) |
| coverage trap | un-patch on first hit | `Op::Break` arm, `run.rs:3948-3954` |
| breakpoint trap | un-patch on resume | `Op::Break` arm, `run.rs:4020-4023` |
| setBreakpoints | un-patch removed + patch added lines | `apply_set_breakpoints`, `run.rs:4268-4320` (`forget_breakpoint`/`set_breakpoint_shared`) |
| clearBreakpoints | restore every patched byte | `apply_clear_breakpoints`, `run.rs:4344-4355` |
| break-on-entry | patch entry offset 0 | `src/dap/launch.rs:181` |

Rather than asking each of these six sites (and every future one) to remember an invalidation
call, the epoch lives **inside the chokepoint itself**: `Chunk::patch_byte` increments
`self.patch_epoch` (a `Cell<u64>`, saturating) on **every** call — set *and* restore. No caller
discipline exists to forget. A source-scan tripwire test additionally asserts that no code path
reaches `Code::patch_byte` except through `Chunk::patch_byte` (plan Task 1), so the chokepoint
cannot be bypassed by a future refactor.

### 4.2 Validity — check epochs, drop the artifact, never edit it

A `DecodedChunk` is valid iff `own_epoch == chunk.patch_epoch.get()` **and** every
`deps[(proto, e)]` satisfies `proto.chunk.patch_epoch.get() == e`. The check runs at **frame
entry** (when the driver selects the instruction source — once per call, not per instruction;
cost is one or a few `u64` compares, held to the Gate-12/17 floor). Stale ⇒ `decoded.take()`
(drop the `Rc`; an in-flight borrow elsewhere is impossible — single-threaded, the driver clones
the `Rc` for the burst) and fall back to byte dispatch; the warmth counter re-arms and a later
re-decode reads the **current** bytes.

Why this is sufficient and *timely*: patches only ever execute on the VM thread at points where
no decoded burst is active — `arm_coverage`/`launch` run before the program, and every runtime
patch site lives inside the async driver's `Op::Break` trap arm or the parked command loop, which
are escalation territory (the burst has already exited and synced its byte ip). Single-threaded
execution makes a mid-burst patch structurally impossible; the next frame entry (or burst
re-entry after the escalation) observes the bumped epoch. The patched byte itself is *also*
correct under byte dispatch immediately — the fallback path needs no grace period.

Why deps (the per-proto flag the brief sketched is **not enough**): Unit C copies *callee*
records into a *caller's* stream. A breakpoint set in the callee bumps the **callee's** epoch; a
caller-epoch-only check would keep executing the stale inlined copy and silently miss the
breakpoint. `deps` closes that hole — and it is precisely the shape a JIT needs (compiled code
that inlined `f` must record `f`'s chunk epoch).

### 4.3 Re-decode with live patches — `Break` records make it sound

The brief's "simplest v1" (a patch flag forcing byte dispatch while any patch exists) is
**strengthened**: re-decoding is *always* sound, even with patches outstanding, because the
decoder reads current bytes and a patched byte decodes as a `Base(Op::Break)` record — an
escalation record (zero-operand, `opcode.rs:631,881`). The decoded driver escalates at it; the
async trap arm reads `code[fault_ip]` (still the patched byte), recovers the original from the
side table, parks/marks-coverage, un-patches (epoch bump → the stream that baked the `Break`
record is dropped), and resumes — byte-identical to today's flow (`run.rs:3905-4027`,
LANE §5 #2/#3). Persistent breakpoints in cold protos cost nothing; a breakpoint in a hot proto
costs one re-decode per set/clear cycle — amortized by the warmth threshold.

### 4.4 The contract, stated for the JIT (the deliverable the JIT spec lacks)

A future tier-1 compiled artifact MUST: (1) record, at compile time, `(chunk, patch_epoch)` for
its own chunk **and every chunk whose code it embedded** (inlining); (2) be entered only through
a guard that compares all recorded epochs against the live cells — a mismatch falls back to the
interpreter at a function-entry boundary (the JIT spec's mechanism-(b) entry re-dispatch) and
drops the artifact; (3) never be edited in place — patch-invalidated code is recompiled from
current bytes, where a patched byte compiles to an interpreter-escalation stub exactly as a
`Break` record escalates here. DECODE ships this contract with its test battery (§8.4) so the JIT
inherits it proven, closing the gap in the JIT spec (drafted 2026-06-08, two days before DBG's
`Code` patching merged — its §2.1/§6 cover counters and code caches but no byte-patch
invalidation).

## 5. Unit B — data-driven superinstructions

### 5.1 Empirical candidate selection (never guessed)

A **census mode** counts dynamic `(DOp, DOp)` pair and `(DOp, DOp, DOp)` triple frequencies
retired by the decoded driver, behind a **default-off cargo feature `decode-census`** so the
counting code is *compiled out* of every production build (the JIT spec §2.1 "not-there vs
not-taken" discipline — no Gate-12 exposure at all). An `#[ignore]`d harness
(`tests/decode_census.rs`, run as
`cargo test --release --features decode-census --test decode_census -- --ignored --nocapture`)
runs the full `bench/profiling/*.as` corpus (incl. LANE Task 0's `func_pipeline`/`call_heavy`/
`server_request`) plus the runnable `examples/**` set, aggregates counts, and prints the ranked
table. The run's output is **committed** as `bench/DECODE_PAIR_CENSUS.md` (data + machine/date +
exact command), and the chosen candidates ship as a **reviewed constant**
(`FUSION_CANDIDATES` in `src/vm/decode.rs`) with a doc-comment citing the census line items that
justify each entry. Re-running the census is a one-command affair forever (in-tree,
re-runnable); changing the candidate set is a reviewed diff against refreshed data, never a
runtime heuristic (§10 rejects auto-selection).

Expected shape (to be *confirmed*, not assumed, by the census): `GetLocal+GetLocal+BinOp`,
`GetLocal+Const+BinOp`, `GetLocal+GetProp`, `JumpIfFalse`-fused compares — the classic
stack-machine pairs. v1 ships **at most ~8 fused forms**, each restricted to payloads that fit
the record (§2.1's two-`u16`-per-`u32` packing + one fault-offset word); a census winner that
cannot fit is recorded and deferred, never silently shoehorned.

### 5.2 Fusion is a decode-time peephole with a hard boundary rule

Fusion runs as a peephole over the freshly-decoded 1:1 record vector. A run of consecutive
records may fuse only if **no interior component is an entry point**, where the entry-point set
(computed in the same decode pass) is: record 0, every resolved jump-target record, the record
after any escalation-class record, and the record after `Yield`. This single rule guarantees no
resume/jump can land mid-fusion, so `entry_index` (which lists only record starts) stays
complete (§3.1). Fused records never include terminals (`Return`/`Propagate`/`Unwrap`/`Yield`),
escalation ops, or `Break` records. Jump-target pre-resolution is what makes the rule *checkable*
— the reason fusion lives in the decoded stream and is unbuildable in `Chunk.code`.

### 5.3 Fused semantics = exact composition; spans attribute to the faulting component

Each fused arm executes **the same shared helpers its components execute** — `LLAdd` reads two
locals (`fiber.local`, `fiber.rs:155`) and calls `eval_binop_adaptive` (`run.rs:5270`) with the
**Add component's** byte offset as `fault_ip`, so the adaptive cache keys on the same offset, the
guard/deopt behavior is identical, and an overflow/type panic carries the byte-identical message
*and span* the unfused sequence produces. Components that can panic carry their fault offset in
the record's packed payload (§2.2); components that cannot (a `GetLocal` is infallible —
compiler-verified slots) need none. **Never reimplement arithmetic/member semantics in a fused
arm** — a fused arm that cannot be expressed as straight calls to the existing shared helpers is
not a valid candidate (predates any speed claim; this is LANE's transcription discipline applied
to fusion). Failure modes and their answers: a mid-fusion panic (helper raises at the component's
span — stack effects up to that component already applied, exactly as the unfused sequence would
have); an adaptive deopt inside a fused arm (the helper's own deopt path — the *record* is not
invalidated, the site just runs the generic helper, same as unfused); a breakpoint set on a
fused *middle* (the patch bumps the epoch → whole stream dropped → re-decode sees the `Break`
byte → that run is not fused — the breakpoint binds and fires byte-identically).

## 6. Unit C — speculative inlining of small hot global fns (evidence-gated, droppable)

### 6.1 The candidate site and the predicate (precise)

A call site qualifies at decode time iff the byte sequence is `GET_GLOBAL f; …args…; CALL argc`
where the args region is itself sync-subset straight-line code, and **at decode time** `f`
resolves through `user_globals` (`run.rs:5165`) to a `Value::Closure` whose proto satisfies ALL
of:

- `!is_async && !is_generator && !is_worker` (`chunk.rs:497-501`);
- `chunk.upvalues.is_empty()` and `chunk.cell_slots.is_empty()` (no captures, no cells);
- every param untyped (`p.ty.is_none()`), no rest (`!has_rest`), no defaults (equivalently: the
  body contains no `JumpIfArgSupplied`/`CheckParam`/`CheckLocal` prologue ops) — so
  `check_call_args` (`run.rs:1775`) degenerates to the exact-arity check, which the decoder
  performs **statically**: `argc == proto.arity` (mismatch ⇒ don't inline; the generic path
  raises the byte-identical arity panic at runtime);
- `ret.is_none()` (no return-type contract to run at exit);
- `slot_count ≤ INLINE_MAX_SLOTS` (= 8) and the decoded body is **straight-line sync-subset**
  (no jumps/jump targets, no call-class ops, no `DeferPush`/`DeferPushMethod` — a defer
  registers frame-exit work and an inline segment has no frame to drain — a strict **leaf**),
  at most `INLINE_MAX_RECORDS` (= 16) records, ending in `Return`;
- v1 depth is **1**: an inline segment is never built from a stream that itself contains inline
  segments (no nesting).

A site that fails any clause decodes 1:1 — the generic `Call` record, unchanged. Late binding is
preserved: a global undefined at decode time simply never inlines (and a *later* define bumps
`struct_gen`, so an already-inlined site's guard starts missing — correct, §6.2).

### 6.2 The guard — `struct_gen` is necessary but NOT sufficient (code-verified correction)

The brief's "the existing global-version mechanism (struct_gen) — a DefineGlobal bumps the gen"
is **half** the guard. Verified in code: `update_user_global` (`run.rs:5211-5226`) and
`set_user_global_at` (`run.rs:5187`) deliberately do **NOT** bump `struct_gen` on a plain
reassignment (that is SP8's whole point — a hot reassigned `let` keeps its `IndexBound` cache
hot, `adapt.rs:170-191`). So a *mutable* global rebound to a different closure
(`let f = (x) => x + 1; … ; f = otherFn`) changes the callee without moving `struct_gen`. The
inline guard is therefore **two-part**, recorded in the `InlineEnter` record at decode time:

1. `vm.struct_gen() == recorded_gen` (`run.rs:5146`; bumped only by `define_user_global`,
   `run.rs:5231-5245`) — validates the recorded stable `IndexMap` index;
2. the value at `user_globals[recorded_idx]` is a `Value::Closure` whose
   `Rc::ptr_eq(proto, recorded_proto)` — validates the callee *identity* (covers mutable-`let`
   reassignment, and is harmless insurance for immutable `fn` globals).

Guard **failure** is mechanism-(a)-style fallback, *cheap by construction*: the `GET_GLOBAL`
record before the site still executes normally (it pushes the callee — kept precisely so the
fallback needs no state reconstruction), the arg records still execute, and `InlineEnter`'s miss
arm is **a single branch to the original `Base(Call)` record** (its index is the `InlineEnter`
record's jump payload), which runs the untouched generic dispatch — same helpers, same panics,
same `check_call_args`. No re-decode, no stream invalidation: the site just stops paying off
until/unless re-decoded.

### 6.3 Slot windowing — the real frame-push mechanics, minus the frame

On guard **hit**, `InlineEnter` replicates the plain-call arm's stack discipline
(`run.rs:1757-1820`) without pushing a `CallFrame`:

- `enter_frame_depth(call_span)` — **the SP3 §B counter increments exactly once per LOGICAL
  call** (`run.rs:616-627`); an inlined call at the depth limit panics
  `maximum recursion depth exceeded` byte-identically. On panic *inside* the segment the
  increment is not unwound — matching the real engine, where a panicking frame's unit is
  restored by the `recover`/`call_value` `DepthGuard` boundary in both worlds.
- remove the callee value at `callee_idx` (`stack.remove` — argc ≤ arity ≤ 8 elements shift; the
  real path pops/pushes the same region) so the args occupy
  `stack[callee_idx .. callee_idx+argc]`, then pad `Nil` up to `slot_count` — the callee's slots
  now live at window base `callee_idx`, exactly where a real frame's `slot_base` would be;
- set the burst-local `inline_base = callee_idx` (a scalar — depth 1 means no stack of bases).

Body records execute with their `GetLocal/SetLocal` slots resolved against `inline_base` (the
decoder rewrites them to inline-local record forms). `InlineExit` (the rewrite of the body's
`Return`): pop the return value, `stack.truncate(inline_base)`, `leave_frame_depth()`
(`run.rs:635`), push the value — `return_from_frame`'s effect (`run.rs:5795-5828`) minus the
frame pop and minus the (absent) return contract. **No suspension can originate inside a
segment** (leaf + straight-line + sync-only ⇒ the only exits are `InlineExit` and `Err`), so the
caller's byte ip never needs to encode "inside an inline".

### 6.4 Spans, sources, and panic provenance inside a segment

Segment records keep their **callee**-chunk byte offsets in `off`; the segment table maps record
ranges → the callee `Rc<FnProto>`. A panic inside the segment anchors at
`callee_chunk.span_at(off)` with `last_fault_source` set to the **callee's** module source at
segment entry (restored to the caller's at exit) — byte-identical to a panic raised one frame
down today (`run.rs:1066-1073` binds the faulting module's source). Arity-era panics (the
statically-excluded cases) never arise on the inline path by construction.

### 6.5 What an inlined call does NOT do — and why each is sound

No `CallFrame` push ⇒ no `publish_profile_frames` (`run.rs:663`), no DAP frame, no per-frame
`ret_span`. Each is squared as follows: profiler/debugger — §6.6's validity rule (no inline
segments while *any* instrumentation is armed); `ret_span` — only consumed by the return-type
contract (`run.rs:5804-5811`), excluded by predicate; `argc`/`JumpIfArgSupplied` — excluded;
`def_class`/`GetSuper` — free functions only, and `GetSuper` is not in any leaf body the
predicate admits (it's a method-frame op).

### 6.6 Instrumentation rule: inline segments are valid only while `Vm.instrument` is `None`

Breakpoints and coverage are handled by the patch epoch (§4). The **profiler** is not patch-based
— it observes frame push/pop (`publish_profile_frames`), which inlining elides, so a
deterministic-mode profile would lose callee frames (a golden-visible difference). Rule: the
frame-entry validity check additionally requires `instrument.borrow().is_none()` for any stream
with `inline_segments` non-empty (streams without segments are unaffected — so the
`dbg_zero_cost_gate` armed-idle config keeps full decoded speed minus inlining, and the
armed/none geomean bound stays honest: the only delta is C's contribution, measured and reported
in the gate re-run). Arming happens before the run (`--profile cpu`) or at a park (DAP) — both
outside bursts, so the next frame entry observes it. Standard-practice note: attributing inlined
time to the caller is what every inlining VM's profiler shows; we still choose the conservative
rule for v1 because the deterministic profiler mode is golden-tested.

### 6.7 Droppable by evidence (the shipping condition)

Unit C ships behind its own permanent toggle (`ASCRIPT_NO_DECODE_INLINE`, §8.1), and its
A/B is isolated (inline-on vs inline-off over the call-heavy corpus, same session). **If the
isolated win is < 2% geomean on `call_heavy` + `func_pipeline`, Unit C is dropped**: the code is
removed (not dark-shipped), the census data, the A/B table, and the verdict are recorded in
`bench/DECODE_RESULTS.md` and `goal-perf.md` — an evidence-recorded outcome, not a silent
deferral. (The guard/epoch machinery of §4 stays regardless — it is Unit A's and the JIT's.)

## 7. Unit D — top-of-stack register caching (evidence-gated, droppable; measured AFTER Unit B)

### 7.1 Mechanism

The record-source burst loop (the decoded instantiation of LANE's `run_loop_sync` via DECODE's
`sync_burst<S>`, §2.4) holds the operand stack's top value in a **Rust local** — `tos: Option<Value>` — instead of
`fiber.stack`'s last slot. Every stack touch inside the burst goes through a thin burst-local
accessor layer (`push`/`pop`/`peek(n)`), not through `Fiber`'s methods directly:

- `push(v)`: if `tos` is `Some(old)`, spill `old` to `fiber.stack`; `tos = Some(v)`. The hot
  produce-consume chain (`GetLocal; GetLocal; Add; SetLocal`) thus keeps its intermediate in a
  register/local the optimizer can see, with zero `Vec` traffic.
- `pop()`: `tos.take()` or, when empty, `fiber.stack.pop()` (the underflow-into-deeper-slots
  path — deeper operands stay on the fiber stack exactly as today).
- `peek(n)`: depth-adjusted by whether `tos` is occupied; `peek(0)` reads the local. Arms that
  operate *below* TOS (the builder family — `MapEntry`/`AppendArray`/`AppendObject`/spreads —
  and `Swap`/`Rot3`/`SetIndex`) route through the same accessors, so under-TOS access is
  correct **by the accessor layer, not by per-arm discipline**.
- Locals are unaffected: `GetLocal`/`SetLocal` address `stack[slot_base + slot]`, always *below*
  the operand region (`fiber.rs:155-175`); the cached TOS is always an operand above the frame
  window, so local reads/writes never alias it. (The one place operands *become* locals — the
  call convention windowing args into a callee frame — is a mandatory flush point, below.)

This is the classic stack-caching interpreter optimization (Ertl's one-register TOS cache),
applied ONLY where DECODE already concentrated the hot dispatch: the decoded sync burst.

### 7.2 THE FLUSH INVARIANT (load-bearing — state it like the §4 contract)

The two-lane design works **only because the `Fiber` externalizes COMPLETE execution state at
every lane switch** (LANE §0: "the fiber *is* the state machine"). A cached TOS is a controlled,
burst-local violation of that property — so the contract is:

> **At every edge where control leaves the record burst loop — or where any code outside the
> burst's own accessor layer can observe `fiber.stack` — the TOS cache is empty and
> `fiber.stack` is byte-for-byte what byte dispatch would have left.** A missed flush is a
> wrong-VALUE bug (not a crash) — exactly the class the four-mode differential and the fuzz
> axis exist to catch, which is why each edge gets its own named test (plan Task 10).

The complete flush-edge enumeration, from walking the decoded driver's exits (§2.4/§3):

1. **`NeedsAsync` escalation** — out-of-subset record, pending `Op::Await`, a frame-exit
   record (`Return`/`Propagate`) with a non-empty `frame.defers` (DEFER — the drain runs on
   the async driver), a baked
   `Base(Break)` record (breakpoint/coverage trap), or a stale-stream refusal: flush BEFORE
   `sync_ip` writes the byte ip; the async driver (and the parked debugger's frame/variable
   snapshots, `build_frame_snapshots`) must see the real stack.
2. **`Finished`** — root-frame `Return` and `Yield` (`FiberState::Suspended`): the
   result/yielded value is popped through the accessor, then the cache is flushed (a suspended
   generator fiber is re-entered later by ANY driver; its stack must be complete).
3. **`Err` unwind** — any `Control::Panic`/`Propagate` out of a helper (including a fused
   record's faulting component and an inlined body): flush before propagating — `recover`,
   the SP4 §3 provenance binder, and debugger inspection may observe the fiber afterwards.
4. **Frame push** — the plain-closure `Call` (`push_closure_frame`) and `InlineEnter`: the
   callee value + args must be physically on `fiber.stack` *before* the call mechanics window
   them into the callee's slots (`run.rs:1757-1820` semantics; §6.3's `stack.remove` likewise).
   Flush before executing any Call-class or InlineEnter record.
5. **Frame pop** — `Return`/`Propagate` through the shared `return_from_frame`
   (`run.rs:5795-5828` truncates to `slot_base` and pushes the result) and `InlineExit`: flush
   before invoking the helper; the burst resumes in the caller with an empty cache (v1 keeps
   the helper-pushed result on the real stack — re-priming the cache on resume is a recorded
   possible refinement, not v1).
6. **Instrumentation** — needs no NEW edge: `publish_profile_frames` reads frame *names*, not
   the operand stack, and fires at frame push/pop (edges 4/5 already flushed); breakpoint and
   coverage traps are `Break` records (edge 1); the paused `evaluate` runs only while parked at
   edge 1.

Failure modes and their answers: a **missed flush** — wrong value after a lane switch; caught
by the per-edge battery + the differential/fuzz modes (Gate 15). A **double flush** — a
duplicated stack entry; structurally prevented because flush is `Option::take` (idempotent).
A **helper writing `fiber.stack` directly mid-burst** — only Call-class helpers do
(`CallSpread` flattening, `check_call_args` windowing), and every one sits behind edge 4.

### 7.3 Ordering & the gate input (why Unit D is measured AFTER Unit B)

Unit D builds ONLY on the decoded sync driver (Units A+B merged) and is **measured after Unit
B's fusion A/B**, because fusion already eliminates the cheapest stack traffic — a fused
`LLBinOp` keeps its two operands and intermediate in Rust locals *within* the record, no TOS
needed. Unit D's headroom is therefore the **residual** operand-stack traffic between records
that fusion did not capture. To make that gate input measurable, the Unit B bench deliverable
gains a metric: the decode stats counters record fiber-stack pushes+pops retired by the record
driver (`stack_ops`, the same burst-local-accumulator costing as the other counters), and
`bench/DECODE_RESULTS.md` reports the **residual stack-traffic share** per workload after
fusion. If fusion already drove it to noise, that is Unit D's drop evidence before a line of
TOS code is written.

### 7.4 Kill switch — dedicated `ASCRIPT_NO_DECODE_TOS` (decided, justified)

`Vm.decode_tos: bool` (default `true` once shipped), env `ASCRIPT_NO_DECODE_TOS=1`, mirroring
`ASCRIPT_NO_DECODE_INLINE` — **not** folded under `ASCRIPT_NO_DECODE`: Unit D must be
A/B-isolatable and droppable independently of Units A–C (the no-tos-vs-default isolating run is
the ship/drop instrument, and the differential needs decoded-with-tos vs decoded-without-tos as
distinct modes). Permanent, per campaign Gate 15. When `false`, the record driver's accessors
degenerate to plain `fiber.stack` operations — the Unit A/B shipped path, untouched.

### 7.5 Ship-or-drop gate (the §6.7 discipline, applied verbatim)

Unit D ships **only** on a measured same-session geomean win on the dispatch-bound corpus
(`object_churn` + `call_heavy` + `func_pipeline`) **with zero regression beyond the 0.97× noise
bound on every other workload**. The honest expectation is **single digits** — a one-register
TOS cache on top of an already-decoded, already-fused burst is a constant-factor trim, and the
literature's larger wins predate fusion taking the easy traffic. If the isolated win is < 2%
geomean on that trio (the Unit C bar), Unit D is DROPPED: code removed (the accessor layer
reverts to direct fiber ops), the residual-traffic data, the A/B table, and the verdict recorded
in `bench/DECODE_RESULTS.md` + `goal-perf.md` — an evidence-recorded outcome, never a silent
deferral. (Note for the record: `goal-perf.md`'s parked-list TOS line is superseded by this
unit — the owner edits that file.)

## 8. Correctness — modes, fuzz axis, coverage assertions, kill switches, the battery

### 8.1 Permanent kill switches (campaign Gate 15 — never bring-up scaffolding)

A **dedicated** master switch — not `--no-specialize` — because the two must compose
orthogonally: `specialize` gates ICs/adaptive sites *inside* the shared helpers, `decode` gates
the instruction representation *around* them; the differential must be able to run decoded ×
generic (records calling helpers that take the generic path) to prove the guards independent.
`--no-specialize` keeps meaning exactly what it means today.

- `Vm.decode: bool` — default `true`; `ASCRIPT_NO_DECODE=1` (mirroring `ASCRIPT_NO_SPECIALIZE`,
  `src/lib.rs:2066`, and LANE's `ASCRIPT_NO_SYNC_LANE`; read at `Vm` construction so worker
  isolates inherit). `false` ⇒ no decoding, no records, byte dispatch only — the shipped LANE
  path, instruction for instruction.
- `Vm.decode_inline: bool` — default `true`; `ASCRIPT_NO_DECODE_INLINE=1` ⇒ the decoder never
  builds inline segments (fusion + plain records unaffected).
- `Vm.decode_tos: bool` — default `true` (if Unit D ships); `ASCRIPT_NO_DECODE_TOS=1` ⇒ the
  record driver's accessors operate directly on `fiber.stack` (no TOS local) — §7.4.
- Test entries in `src/lib.rs` beside `vm_run_source_generic` (`lib.rs:2237-2266`):
  `vm_run_source_no_decode`, `vm_run_source_decoded_forced` (threshold 0 — the anti-false-green
  forced mode), `vm_run_source_decoded_no_inline`, `vm_run_source_decoded_no_tos`, and
  `vm_run_source_decode_stats` (a `DecodeStats` struct: `output, exit, decoded_ops, fused_ops,
  inline_hits, inline_misses, decoded_bytes, stack_ops, tos_ops` — `tos_ops` counts records
  retired with the TOS cache active, `stack_ops` the residual fiber-stack pushes+pops, §7.3).

### 8.2 Differential modes (Gates 1 + 15)

`tests/vm_differential.rs` (expression batteries, program batteries, goldens, the
whole-`examples/**` corpus via `all_corpus_examples()`/`EXAMPLE_SKIPS` — never a second list),
in **both feature configs**, asserts byte-identity across:

> tree-walker == specialized-VM(decoded-forced) == specialized-VM(no-decode)
> == generic-VM(decoded-forced) == specialized-VM(decoded-forced, no-inline)
> == specialized-VM(decoded-forced, no-tos)

The decoded-**forced** mode is load-bearing: with the shipped warmth threshold, short corpus
programs would otherwise never decode and the differential would silently degenerate into a
second byte-dispatch run (the JIT spec §5.1 false-green trap). The tree-walker is never relaxed;
a decoded-on/off divergence is a decoder/driver/fusion/guard bug — fix it, never the assertion.

### 8.3 Fuzz axis + coverage assertions (same PR as the driver)

- `fuzz/fuzz_targets/differential.rs` and `tests/property.rs` (the three-way — post-LANE
  four-way — generated-program battery) gain the `vm_run_source_decoded_forced` and
  `vm_run_source_no_decode` projections in the equality assertion and the crash report.
- **Coverage assertions** (each sabotage-tested: hard-disable the path, watch the assertion
  fail, revert): over the corpus in forced mode, (a) `decoded_ops > 0` aggregate and a
  per-program floor on a tight-loop program (≥ 1,000,000 records retired); (b) `fused_ops > 0`
  aggregate (the census winners actually fire on the corpus); (c) `inline_hits > 0` **and**
  `inline_misses > 0` — the miss forced deterministically by (i) defining a NEW top-level global
  after the warm loop (a `struct_gen` bump; note: *redefining* a global is a runtime error —
  `'<name>' is already defined in this scope` — so the brief's "redefine a global" is realized as
  define-new + as (ii) reassigning a mutable `let f = closure` to a different closure, the
  identity-miss path); (d) with `ASCRIPT_NO_DECODE`, `decoded_ops == 0` (the switch kills); with
  `ASCRIPT_NO_DECODE_INLINE`, `inline_hits == 0` while `decoded_ops > 0`; (e) Unit D:
  `tos_ops > 0` on the corpus in the default mode (**TOS-cached bursts actually executed** —
  the anti-false-green rule again), and with `ASCRIPT_NO_DECODE_TOS`, `tos_ops == 0` while
  `decoded_ops > 0`.
- The stat counters follow LANE §6.4's costing: burst-local accumulators flushed once per burst
  exit; the census counting (per-pair maps) is `decode-census`-feature-gated out of production
  entirely.

### 8.4 The invalidation battery (mandatory; the JIT-contract proof)

1. **Breakpoint mid-hot-loop:** run a hot loop to warmth (decoded stream installed and
   executing — assert `decoded_ops` rising), set a line breakpoint in the loop via the DAP/hook
   path, assert the trap **fires** (a stale stream would run past it), evaluate a local at the
   park, clear, resume — final output byte-identical to an uninstrumented run, and the stream
   re-decodes (decoded_ops resumes rising).
2. **Breakpoint in an INLINED callee:** warm a caller that inlined `f`, set a breakpoint on a
   line *inside `f`*, call again — the **deps** epoch check must drop the *caller's* stream and
   the trap must fire in `f`'s real frame (byte dispatch); clear + resume → byte-identical.
3. **Coverage over decoded execution:** `--coverage` run of a hot-loop program — per-line
   covered set and program output identical to the no-decode run; the cov/off bench section
   re-recorded.
4. **Epoch unit tests:** `patch_byte` bumps on set AND restore; `arm_coverage` bumps once per
   trap site; a `DecodedChunk` built at epoch N is invalid at N+1; deps mismatch invalidates.
5. **Chokepoint tripwire:** a source-scan test asserting every `patch_byte(`/`set_breakpoint(`
   caller in `src/` routes through `Chunk::patch_byte` (no raw `Code::patch_byte` outside
   `chunk.rs`).

### 8.5 The flush-edge battery (Unit D — mandatory if Unit D ships)

One named test per §7.2 edge, each asserting byte-identity (tos-on vs tos-off vs tree-walker,
output + panic + span) on a program engineered to cross THAT edge with a live cached TOS:
escalation **mid-expression** (a pending `await` as a binop operand), a **breakpoint patched
mid-hot-loop** while the cache is hot (composes with the §8.4 battery — the trap must observe
the flushed stack), a **panic inside a fused record** (edge 3 through §5.3's fault
attribution), a **plain call at cached-TOS state** (edge 4 — the callee's `check_call_args`
must see the args), `Yield` from a generator burst (edge 2 — the suspended fiber's stack is
re-entered by `resume`), and a frame-pop return into a caller mid-expression (edge 5). Each is
sabotage-verified: skip the edge's flush and watch the test fail, then restore.

### 8.6 Standing invariants (each re-asserted)

`call_depth` exactly once per logical call (incl. inlined — the recursion-limit panic test runs
decoded-forced vs no-decode vs tree-walker); no `RefCell` borrow across an await (the decoded
driver, like the sync driver, contains zero awaits — reviewer greps); redeclaration/const
timing, capacity errors, Tier-2 messages and spans — all via the same shared helpers, enforced
by the corpus + panic batteries; `.aso`/verifier/disasm diffs against `main` empty;
`ASO_FORMAT_VERSION` unchanged; tree-walker diff empty.

## 9. Performance — expectations stated, results measured (Gates 12, 16, 17, 18)

**Methodology:** every headline number is a same-session A/B via `bench/ab.sh` (LANE Task 0)
over the 8-workload corpus — baseline = merge-base build, candidate = branch — plus the
isolating A/Bs: candidate-no-decode vs candidate-decoded (Unit A+B's own contribution),
candidate-no-inline vs candidate (Unit C's, §6.7), and candidate-no-tos vs candidate (Unit D's,
§7.5, run AFTER the Unit B numbers + residual stack-traffic share are recorded — §7.3 is the
gate input). At least one workload profiled with the
shipped profiler (`--profile cpu` — Gate-16 dogfooding). Results in `bench/DECODE_RESULTS.md`;
a post-DECODE re-profile section is appended to `bench/PROFILING_RESULTS.md` — it is the
campaign's mandatory re-rank checkpoint and the **JIT gate input** (`goal-perf.md` execution
order: only if dispatch still dominates after DECODE does the JIT proceed).

**Expectations (honest, not promises):**

- **Should move:** the dispatch-bound class — `object_churn` (49% dispatch; the decode share of
  `run_loop`'s 18% self-time plus jump arithmetic), the `vm_bench` compute corpus, and
  `call_heavy` (Unit C's target — *if* its evidence gate holds; CALL's allocation diet is the
  bigger lever there and DECODE measures its *incremental* win over a post-CALL or pre-CALL
  baseline, whichever is merged — recorded either way).
- **Modest by honesty:** Unit A alone is a constant-factor win on top of an already-cheap loop —
  classic pre-decoded interpreters report single-digit to low-double-digit gains; fusion's
  increment is whatever the census says is actually hot; Unit D's TOS cache is **honest single
  digits at best** on the dispatch-bound trio (fusion takes the easy traffic first — §7.3/§7.5).
  No number is promised; the A/B decides decode-on-hot vs first-run (§2.3) and the threshold,
  by data.
- **Will NOT move (do not claim):** async-dominated workloads (`async_inline`/`async_concurrent`
  — scheduler tax, LANE/EXEC), `workflow_loop` (fsync, WARM), allocation-bound `json_roundtrip`
  slices (SHAPE/NANB/CALL).

**Gate obligations:** (12/17) spec/tw geomean ≥ 2× at merge; `dbg_zero_cost_gate`
(`tests/vm_bench.rs:499`) re-run — DECODE touches the dispatch loop, so the armed-idle bound
(≤ 1.05×) must hold (note §6.6: armed-idle loses only inline segments; the re-run reports that
delta explicitly); no-decode vs pre-DECODE baseline shows no regression (the per-frame validity
check is the only new default-path cost — if it measures, fix the check's home, never the
bench). (18) peak RSS per workload + total decoded bytes reported; a regression is a bug.

## 10. Scope & rejected alternatives

**In scope:** the `DecodedChunk` side representation + warmth-gated decoder; the
`InstrSource`-generic sync-driver consumption; the `Chunk.patch_epoch` chokepoint + deps epochs
+ invalidation battery; the census feature/harness + committed data + the reviewed
`FUSION_CANDIDATES` peephole; the §6 inline transform + two-part guard + its own toggle + the
drop-by-evidence gate; the §7 TOS-cache accessor layer + flush-edge battery + its own toggle +
its drop-by-evidence gate (gated on Unit B's residual stack-traffic data); kill switches,
differential modes, fuzz axis, coverage assertions; Gate-12/16/17/18 artifacts.

**Decided narrowings of the brief (recorded, with reasons):**

- **Sync driver only consumes records** (brief: "both drivers can"). The async loop executes one
  escalation op per iteration; its bodies dwarf decode cost, and the lane-off kill-switch path
  must remain the physically-shipped pre-LANE loop (LANE §9). Zero measurable win, real risk —
  revisit only with profile evidence.
- **Record is 16 bytes, not 8–12** (brief's estimate). The fault-attribution offset (§5.3) and
  the ip↔record bridge (§3) each earn a `u32`; the survey (§2.1) caps operands so `a`/`b`
  suffice for all base + packed fused forms. Memory is accounted, lazy decode bounds it.
- **Dedicated kill switch** (brief offered `--no-specialize`): orthogonal composition
  (decoded × generic) is a differential requirement; overloading `specialize` would conflate two
  guard systems and retire the existing three-way mode's meaning.
- **Per-chunk `patch_epoch` inside `patch_byte`, plus deps** (brief: per-proto patch flag +
  byte-dispatch-while-patched). The chokepoint needs no caller discipline; deps are *required*
  for cross-proto inlining; and `Break`-records make re-decode-under-live-patches sound (§4.3) —
  strictly stronger and simpler than the flag.
- **Inline guard adds closure identity to `struct_gen`** (brief: struct_gen alone): verified
  insufficient — `update_user_global` does not bump `struct_gen` on reassignment
  (`run.rs:5211-5226`); identity closes the mutable-`let` hole.
- **Inline slot windowing over zero-extra-slots** (brief offered either): the window at
  `callee_idx` is the real call's own discipline minus the frame — simpler to prove than an
  expression-only restriction, and `slot_count_inlined` lives in the decoded record, not the
  chunk (the brief's "slot_count is in the chunk" concern dissolves: the fiber stack grows
  dynamically; only the window base matters).
- **Inlining is a decode-time rewrite, not compile-time emission** (brief offered either, decide
  after reading `src/compile/`): compile-time inlining would either mutate `Chunk.code`
  (forbidden — byte-identity, goldens, disasm) or mint new serialized opcodes (`.aso` bump,
  verifier surface, format instability) — both rejected by this spec's breaking-change posture;
  and only runtime knows the global bindings to speculate on.

**Rejected outright:**

- **Register bytecode** — rejected campaign-wide (`goal-perf.md` "Removed/parked"): rewrites
  compiler/VM/verifier/`.aso`/disasm and re-proves the whole differential while LANE+DECODE
  capture most of the win incrementally.
- **Fused ops in `Chunk.code` / `.aso`** — byte-identity, verifier stability, format stability;
  the side representation gets all the benefit with none of the surface.
- **Runtime auto-selection of fusion candidates** — v1 ships a reviewed constant from committed
  census data; a self-tuning fuser is unauditable and untestable against goldens.
- **Inlining methods/closures/builtins** — out of scope v1: the global-fn guard
  (`struct_gen` + identity) exists today; method inlining needs the IC shape guard story
  (CALL/SHAPE territory) and closure inlining needs capture-environment identity — neither has
  shipped guard machinery. Globals only.
- **Editing a decoded stream in place on patch** (surgical record replacement): drop-don't-edit
  is the invariant the JIT must also live by; in-place surgery would be the one place where the
  decoded artifact and `Chunk.code` could drift. Cheapness of re-decode (linear, warmth-gated)
  removes the temptation.
- **TOS caching in the BYTE-dispatch async loop** — rejected for the §2.4 reason records are
  sync-driver-only: the async loop executes one escalation op per iteration, so *every*
  iteration boundary (every await/spawn/method/import/trap) is a mandatory flush point and the
  arm bodies dwarf any cached-read win; it would also restructure the very loop whose lane-off
  form is the shipped kill-switch path (LANE §9). Unit D lives exclusively in the decoded sync
  burst, where flush edges are rare and enumerable (§7.2).
- **A multi-register / always-occupied TOS cache** (Ertl's larger cache-state machines):
  rejected v1 — each additional cached slot multiplies the flush-edge proof surface for a win
  fusion already approximates; the `Option<Value>` single-slot form keeps flush idempotent and
  the state space two-valued. Revisit only with post-D profile evidence.
- **LLVM/Cranelift anything** — that is the JIT spec, gated on the post-DECODE re-profile.

## 11. Grounding (verified file:line on `main`, 2026-06-12, pre-LANE — re-grep before relying)

- `src/vm/opcode.rs` — `Op:29` (110 variants, `Break` last `:631`), `from_u8:639`,
  `operand_width:788-883` (the §2.1 survey source), `has_inline_cache:891`.
- `src/vm/chunk.rs` — `Code` newtype (`UnsafeCell<Vec<u8>>`) `:296-376`, `Code::patch_byte:332`
  (the soundness doc), `Chunk:380` (`code:384`, `spans:397`, side maps `field_ics:421`/
  `method_ics:424`/`arith_caches:431`/`global_caches:435` — the side-table precedent),
  `Chunk::patch_byte:779`, jump emit/patch math `emit_jump:587`/`patch_jump:603`/
  `emit_loop:636`, `span_at:861`, `FnProto:492` (`arity/has_rest/is_async/is_generator/
  is_worker:494-501`, `params:515`, `ret:518`, `slot_count:409`, `cell_slots:407`,
  `upvalues:401`).
- `src/vm/run.rs` — fetch/decode/advance `run_loop:1088-1103` (per-instruction
  `last_fault_source` refresh `:1092-1096`; its ONLY reader `run:1066-1073`), jump arms
  `:2172-2224`, plain-closure call arm `:1757-1827` (`check_call_args:1775`,
  `enter_frame_depth:1807`, `frames.push:1808`, `publish_profile_frames:1823`),
  `enter_frame_depth:616`/`leave_frame_depth:635`, `publish_profile_frames:663`,
  `return_from_frame:5795-5828`, `eval_binop_adaptive:5270` (guard/deopt `:5311-5358`),
  `Op::Break` trap arm `:3905-4027` (coverage un-patch `:3948-3954`, debug un-patch
  `:4020-4023`), `arm_coverage:362-410` (patch `:407`), `apply_set_breakpoints:4268`
  (patches `:4302-4316`), `apply_clear_breakpoints:4344` (`:4353`), `GET_GLOBAL` arm + caches
  `:1337-1396`, `struct_gen:5146`, `get_user_global_full:5165`, `set_user_global_at:5187`,
  `update_user_global (NO struct_gen bump):5211-5226`, `define_user_global (the only bump):
  5231-5245`, `specialize:117`, `instrument:197`.
- `src/vm/adapt.rs` — `WARMUP_THRESHOLD = 8:44`, the side-map rationale (module doc),
  `GlobalCache::IndexBound + struct_gen guard:170-224`.
- `src/vm/fiber.rs` — `CallFrame {ip:23, slot_base, cells, ret_span, def_class, argc}:20-51`,
  `alloc_cells:56`, `Fiber:71`, `local:155`/`set_local:167`.
- `src/vm/instrument.rs` — `Instrumentation:36`, `DebuggerHook` side table + `set_breakpoint_
  shared:268`, `drain_breakpoints:285`, `CoverageTable:501` (`record_trap:526`, `trap:538`).
- `src/dap/launch.rs:181` — break-on-entry patch site. `src/vm/disasm.rs:25` — reads
  `Chunk.code` only. `src/vm/verify.rs:879` — `op_stack_delta` (decoder reuse precedent via
  `bcanalysis`). `src/vm/bcanalysis.rs` — the pure-analysis module-boundary precedent (decode
  walks at `:97-122,151-160`; the natural home for the inline-candidate predicate's static
  checks).
- `src/vm/aso.rs:167` — `ASO_FORMAT_VERSION = 27`, untouched. `src/lib.rs` —
  `ASCRIPT_NO_SPECIALIZE:2066`, `vm_run_source*` entries `:791-804, 2237-2269`.
- `tests/vm_differential.rs` — `assert_vm_matches_treewalker:25`, the whole-corpus gate
  `vm_run_whole_corpus_matches_treewalker:1183`, `EXAMPLE_SKIPS:977`,
  `all_corpus_examples:1141`. `tests/vm_bench.rs` — harness + `dbg_zero_cost_gate:499` +
  the cov/off section `:561`. `tests/property.rs` — the generated-program battery `:117-158`.
  `fuzz/fuzz_targets/differential.rs` — the projection model.
- LANE: `superpowers/specs/2026-06-12-two-lane-engine-design.md` — `run_loop_sync`/
  `SyncOutcome`/escalation-at-un-advanced-ip (§2.2), the sync subset (§3), instrument parity
  (§5), the kill-switch + coverage-assertion discipline (§6), the no-shared-`step()` rejection
  rationale (§9). JIT: `superpowers/specs/2026-06-08-baseline-jit-design.md` — §2.1 counters,
  §3.2 feedback consumption, §4 deopt mechanisms (a)/(b), §5.1 the anti-false-green coverage
  rule; its missing byte-patch invalidation story is §4.4's deliverable. CALL:
  `superpowers/specs/2026-06-12-call-path-diet-design.md` (the call-overhead evidence Unit C
  complements). Campaign: `goal-perf.md` (the DECODE entry + Gates 15–18);
  `bench/PROFILING_RESULTS.md` (the §1 table).
- Precedents: PEP 659 (CPython's specializing adaptive interpreter — the side-map/quickening
  analogy `adapt.rs`); CPython 3.13+ / wasm3-class **pre-decoded / threaded interpreters** (the
  fixed-width-record literature this follows); V8 Sparkplug (compile-from-bytecode-once — the
  decoded IR as translator input); the Self/HotSpot deopt literature via the JIT spec (drop
  compiled artifacts on invalidation, re-derive from the canonical form); Ertl,
  *"Stack Caching for Interpreters"* (PLDI'95) — the §7 one-register TOS cache and the reason
  the multi-state generalization is rejected (§10). Unit D's flush invariant grounds in LANE
  §0/§2 (the Fiber externalizes ALL execution state — `src/vm/fiber.rs:71`; lane switching is
  "choosing which driver polls", which only works if the stack is complete at every switch).
