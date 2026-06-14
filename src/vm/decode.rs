//! DECODE (the decoded-dispatch effort, spec §2): the per-`FnProto` decoded
//! side-representation and the 1:1 decoder.
//!
//! A [`DecodedChunk`] is a flat `Vec` of fixed-width records — opcode widened,
//! operands pre-read into `u32` fields, **jump targets pre-resolved to record
//! indices**. It is a RUNTIME-ONLY side-table (the `arith_caches`/`field_ics`
//! precedent): lazily built on warmth, never serialized to `.aso`, droppable at
//! any time, and invisible to the verifier and the disassembler (which read
//! `Chunk.code`, which never changes). `ASO_FORMAT_VERSION` is unaffected.
//!
//! THIS TASK (DECODE Task 3) builds the decoded stream and its invalidation
//! stamp but does NOT execute from it — nothing consults `Chunk.decoded` during
//! a run yet (Task 4 wires the record-source driver). So behavior is unchanged
//! and the four/five-mode differential is untouched.

// DECODE Task 4 consumes the decoder (the record-source driver in `run_loop.rs`
// fetches from these records). A few forward-declared fields/knobs stay unused
// until Units B/C land (Tasks 8/9) — `deps`/`inline_segments`/`InlineSegment` and
// the `fuse`/`inline` cfg knobs; they carry a targeted `#[allow(dead_code)]` at
// their definition rather than a blanket module allow.

use crate::vm::chunk::Chunk;
use crate::vm::opcode::Op;
use std::fmt;

/// DECODE: one fixed-width pre-decoded instruction (spec §2.2).
///
/// `a`/`b` hold the widened operands. `off` is the byte offset of this record's
/// source instruction in its owning chunk — the ip↔record bridge (spec §3.4):
/// escalation writes it back as the fiber's byte ip; span lookup runs
/// `chunk.span_at(off)` exactly as byte dispatch does.
///
/// Per-op operand layout (1:1 decode, no fusion yet):
/// - zero-operand ops: `a = b = 0`.
/// - `u8` ops (`Call`, `MatchRange`, …): the byte in `a`.
/// - `u16` ops (`Const`, `GetLocal`, …): the widened `u16` in `a`.
/// - `i16` jump ops (`Jump`, `JumpIfFalse`, `JumpIfTrue`, `JumpIfNotNil`,
///   `Loop`): `a` = the pre-resolved TARGET record index (NOT the raw
///   displacement).
/// - `u16`+`u8` ops (`CallMethod`, `MatchArray`, `DefineGlobal`, `CallNamed`):
///   the `u16` in `a`, the `u8` in `b`.
/// - `u16`+`i16` op (`JumpIfArgSupplied`): the `u16` param-index in `a`, the
///   pre-resolved TARGET record index of the i16 jump in `b`.
#[derive(Clone, Copy)]
pub(crate) struct DecodedInstr {
    pub op: DOp,
    pub a: u32,
    pub b: u32,
    pub off: u32,
}

/// The decoded operation. `Base` is a pass-through of the real ISA; the fused
/// (Unit B, Task 8) variant exists ONLY in the decoded stream — never in
/// `Chunk.code`, never serialized, invisible to the verifier and the
/// disassembler. (Unit C inlining, Task 9, lands later.)
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DOp {
    /// A 1:1 decoded base instruction (operands widened into a/b).
    Base(Op),
    /// DECODE §5 (Unit B): a fused superinstruction — N consecutive base ops
    /// executed in ONE dispatch. The [`FusedKind`] names the exact op sequence
    /// (so the record driver runs the right composition of shared helpers); the
    /// per-component operands are packed into the record's `a` field by the
    /// peephole (`decode_fused.rs`). The record's `off` is the FIRST component's
    /// byte offset; the driver reconstructs each later component's byte offset by
    /// adding the (compile-time-constant) component widths, so each component
    /// attributes its span / adaptive-cache key at its OWN byte offset —
    /// byte-identical to executing the components separately.
    Fused(FusedKind),
    /// DECODE §6 (Unit C): the guarded entry of an inlined small global-fn call.
    /// Decoded-stream-ONLY (never a real `Op`, never in `Chunk.code`, never
    /// serialized). `a` = the [`InlineSegment`] index in `inline_segments`
    /// (carrying the recorded `global_idx`, `recorded_struct_gen`, the callee proto
    /// `Rc` for the `Rc::ptr_eq` identity leg, the body record range, the callee
    /// `slot_count`, and `argc`); `b` = the FALLBACK record index — the untouched
    /// `Base(Op::Call)` site reached on a guard MISS (single-branch deopt, §6.2).
    /// On a HIT the driver windows the callee frame in-place (no `CallFrame`),
    /// executes the spliced body records [start, end), and the matching
    /// [`InlineExit`](DOp::InlineExit) restores. The body records keep their CALLEE
    /// byte offsets (span/source provenance, §6.4).
    InlineEnter,
    /// DECODE §6 (Unit C): the rewrite of the inlined body's terminal `Return`.
    /// `a` = the owning [`InlineSegment`] index (symmetry / span anchor). Pops the
    /// return value, truncates the window back to `inline_base`,
    /// `leave_frame_depth`, pushes the value, restores `last_fault_source` — the
    /// `return_from_frame` effect minus the frame pop and the (predicate-excluded)
    /// return contract.
    InlineExit,
}

/// DECODE §5 (Unit B): the REVIEWED fused-superinstruction kinds. Each variant is
/// a fixed op sequence selected from the committed dynamic-adjacency census
/// (`bench/DECODE_PAIR_CENSUS.md`) — never guessed. The variant ITSELF encodes the
/// op sequence (no per-record op storage); the packed operands ride the record's
/// `a` field. Every component executes via the SAME shared helper the single-op
/// `sync_burst` arm calls (`fiber.local`, `vm_read_member`/`ic_get_field`,
/// `eval_binop_adaptive`), so a fused arm is byte-identical to the unfused
/// sequence by construction.
///
/// Operand packing in `DecodedInstr.a` (all base operands are ≤ `u16`, two pack
/// per `u32` — spec §2.1): the FIRST component's u16 operand in the low half, the
/// SECOND component's u16 operand (when it has one) in the high half. A third
/// component (the triple) carries no inline operand of its own (`Add` is
/// zero-operand), so two halves suffice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FusedKind {
    /// `GetLocal s; GetProp name` — census PAIR rank 1 (8.405%). `a = s | name<<16`.
    /// The dominant field-read-after-local shape. GetProp can panic ⇒ its fault
    /// offset (= `off + 3`) is reconstructed by the driver.
    GetLocalGetProp,
    /// `GetLocal s1; GetLocal s2` — census PAIR rank 2 (7.320%). `a = s1 | s2<<16`.
    /// Two adjacent local reads (operand-stack staging). Pure pushes — no fault.
    GetLocalGetLocal,
    /// `GetLocal s; Const k` — census PAIR rank 3 (5.765%). `a = s | k<<16`.
    /// Local-then-const staging into a binop. Pure pushes — no fault.
    GetLocalConst,
    /// `GetProp name; Add` — census PAIR rank 4 (5.691%). `a = name`. Field-read
    /// feeding arithmetic. BOTH components can panic (GetProp read, Add arith).
    GetPropAdd,
    /// `Const k; GetLocal s` — census PAIR rank 5 (5.547%). `a = k | s<<16`.
    /// Const-then-local staging into a binop. Pure pushes — no fault.
    ConstGetLocal,
    /// `GetLocal s; GetProp name; Add` — census TRIPLE rank 1 (5.691%).
    /// `a = s | name<<16`. The field-read-then-use spine. GetProp + Add can panic.
    GetLocalGetPropAdd,
}

impl FusedKind {
    /// The component ops in execution order. Used by the peephole to verify a
    /// match and by the driver-arm fault-offset reconstruction (the widths come
    /// from `op.operand_width()`).
    pub(crate) fn components(self) -> &'static [Op] {
        match self {
            FusedKind::GetLocalGetProp => &[Op::GetLocal, Op::GetProp],
            FusedKind::GetLocalGetLocal => &[Op::GetLocal, Op::GetLocal],
            FusedKind::GetLocalConst => &[Op::GetLocal, Op::Const],
            FusedKind::GetPropAdd => &[Op::GetProp, Op::Add],
            FusedKind::ConstGetLocal => &[Op::Const, Op::GetLocal],
            FusedKind::GetLocalGetPropAdd => &[Op::GetLocal, Op::GetProp, Op::Add],
        }
    }
}

/// DECODE §5: a fusion candidate — a fixed op sequence and the [`FusedKind`] the
/// peephole rewrites it to. The op sequence is matched against the 1:1 records
/// left-to-right (greedy longest-match), subject to the legality rules in
/// [`fuse_records`] (no component but the first may be a jump target; the whole
/// run stays within one basic block).
pub(crate) struct FusedForm {
    /// The base-op sequence (length 2 or 3) this candidate matches.
    pub seq: &'static [Op],
    /// The fused kind the matched run rewrites to.
    pub kind: FusedKind,
}

/// DECODE §5: the REVIEWED fusion set. Each entry cites its census line
/// (`bench/DECODE_PAIR_CENSUS.md`, run of 2026-06-14) and is chosen by dynamic
/// frequency, payload fit (every base operand ≤ `u16`, two packable per `u32`),
/// and shared-helper composability (no candidate needs reimplemented semantics).
/// Changing this set requires a refreshed census commit.
///
/// **Selection (top of the measured ranking, ≤ 8 forms):**
/// - `GetLocal -> GetProp`        — PAIR rank 1, 8.405%.
/// - `GetLocal -> GetLocal`       — PAIR rank 2, 7.320%.
/// - `GetLocal -> Const`          — PAIR rank 3, 5.765%.
/// - `GetProp -> Add`             — PAIR rank 4, 5.691%.
/// - `Const -> GetLocal`          — PAIR rank 5, 5.547%.
/// - `GetLocal -> GetProp -> Add` — TRIPLE rank 1, 5.691% (greedy longest-match
///   subsumes the `GetLocal -> GetProp` pair where an `Add` follows in-block).
///
/// **Recorded-and-REJECTED (high-frequency but not shippable here):**
/// - `Const -> Const` (PAIR rank 8, 3.314%): two const pushes is a legal, no-fault
///   pair, but adds a third "both-operands-from-const" shape with no field/arith
///   helper reuse beyond `Const`'s one-line body; deferred — the local-staging
///   forms above already cover the dispatch-dense staging shapes.
/// - `Pop -> GetLocal` (rank 6) / `Add -> GetLocal` (rank 7): `Pop`/`Add` as the
///   FIRST component would fuse a stack-clearing/arith retire with a following
///   read; legal, but the win is dispatch-only (no stack-traffic removal) and the
///   `GetLocal`-first staging forms already capture the same following reads.
/// - `SetLocal -> GetGlobal` (rank 9): `GetGlobal` carries cache-mutation +
///   builtin-fallback control flow (multiple `continue` exits in its single-op
///   arm) that does NOT compose as a straight helper call — REIMPLEMENTED
///   semantics, rejected per §5.3.
/// - `RangeHasNext -> JumpIfFalse` (rank 18): the for-range loop spine, but
///   `JumpIfFalse` is a BASIC-BLOCK TERMINATOR / control-transfer op — a fused
///   middle/tail jump would need the post-fusion jump-target machinery AND its
///   target is by definition a block boundary; rejected (terminators never fuse).
/// - `SetLocal -> Loop` (rank ~14): same terminator rejection (`Loop` is a jump).
///
/// The peephole NEVER fuses a candidate whose later component is a jump target or
/// a basic-block boundary (see [`fuse_records`]); the rejected control-flow forms
/// above could not pass that gate regardless.
pub(crate) const FUSION_CANDIDATES: &[FusedForm] = &[
    // Greedy longest-match: the TRIPLE must precede the pair it extends so a
    // `GetLocal; GetProp; Add` run fuses to the triple, not the pair + a stray Add.
    FusedForm { seq: &[Op::GetLocal, Op::GetProp, Op::Add], kind: FusedKind::GetLocalGetPropAdd },
    FusedForm { seq: &[Op::GetLocal, Op::GetProp], kind: FusedKind::GetLocalGetProp },
    FusedForm { seq: &[Op::GetLocal, Op::GetLocal], kind: FusedKind::GetLocalGetLocal },
    FusedForm { seq: &[Op::GetLocal, Op::Const], kind: FusedKind::GetLocalConst },
    FusedForm { seq: &[Op::GetProp, Op::Add], kind: FusedKind::GetPropAdd },
    FusedForm { seq: &[Op::Const, Op::GetLocal], kind: FusedKind::ConstGetLocal },
];

/// The per-`FnProto` decoded side representation (the `arith_cache` precedent:
/// runtime-only, lazily built, never serialized, droppable at any time).
pub(crate) struct DecodedChunk {
    /// One record per `Chunk.code` instruction, in code order.
    pub records: Vec<DecodedInstr>,
    /// Sorted `(byte_off, record_idx)` for every record. Binary-searched at
    /// burst entry to convert the fiber's canonical byte ip to a record index.
    /// In a 1:1 decode this is simply every record's `(off, idx)`; once
    /// inlining lands (Task 9) only CALLER-chunk records are entered.
    pub entry_index: Vec<(u32, u32)>,
    /// Validity: the patch epoch of the OWNING chunk at decode time. A later
    /// `Chunk::patch_byte` bumps `Chunk::patch_epoch`; a consult comparing a
    /// stale `own_epoch` rebuilds (Task 4/6). Stale ⇒ drop, never edit.
    pub own_epoch: u64,
    /// Task 9 fills: one `(foreign-chunk identity, epoch)` entry per chunk whose
    /// records were embedded by inlining. Empty until then; consulted by the §4.2
    /// deps-validity check ([`DecodedChunk::is_valid`]).
    pub deps: Vec<(std::rc::Rc<crate::vm::chunk::FnProto>, u64)>,
    /// DECODE §6 (Unit C): the inline-segment table — one row per inlined site,
    /// indexed by an `InlineEnter` record's `a`. Empty unless `cfg.inline` ran and
    /// at least one site qualified. Non-empty ⇒ the §6.6 validity rule forbids
    /// executing this stream while any instrumentation is armed.
    pub inline_segments: Vec<InlineSegment>,
}

/// DECODE §6 (Unit C): one inlined small-global-fn call site. Carries everything
/// the `InlineEnter` driver arm needs that does not fit in the `Copy`
/// [`DecodedInstr`] (the callee proto `Rc` for the identity guard + span/source
/// attribution). One row per inlined site, indexed by the `InlineEnter` record's
/// `a` field.
pub(crate) struct InlineSegment {
    /// Record-index range `[start, end)` of the inlined body records (the callee's
    /// 1:1-decoded straight-line body, EXCLUDING its terminal `Return`, which became
    /// the [`InlineExit`](DOp::InlineExit) record at index `end`). The body records
    /// keep their CALLEE byte offsets in `off` (span/source provenance, §6.4).
    pub start: u32,
    pub end: u32,
    /// §6.2 guard leg 1: the stable `user_globals` index recorded at decode time.
    /// The guard checks `vm.struct_gen() == recorded_gen` first, then that the slot
    /// at this index still holds the recorded callee.
    pub global_idx: u32,
    /// §6.2 guard leg 1: the `struct_gen` snapshot at decode time. A later
    /// `define_user_global` bumps it → the guard misses (the recorded index may have
    /// shifted) → single-branch deopt.
    pub recorded_gen: u64,
    /// §6.2 guard leg 2: the callee proto identity. The guard requires the value at
    /// `user_globals[global_idx]` to be a `Value::Closure` whose proto is
    /// `Rc::ptr_eq` to this — catching a mutable-`let` rebind (which does NOT bump
    /// `struct_gen`). Also the SoT for the body's span/source attribution (§6.4) and
    /// the §4.2 `deps`-epoch validity leg.
    pub callee: std::rc::Rc<crate::vm::chunk::FnProto>,
    /// The callee's `slot_count` — the inline window size padded with `Nil` at entry.
    pub slot_count: u16,
    /// The call's argument count (== the callee arity, statically; the predicate
    /// refuses `argc != arity`). The window base is `stack.len() - argc - 1` at entry.
    pub argc: u8,
}

impl DecodedChunk {
    /// DECODE §4.2: the validity predicate — the SINGLE source of truth the
    /// `select_record_source`/`resync` consults reach (the JIT-staleness contract).
    /// A cached stream is valid iff (a) its `own_epoch` still equals the OWNING
    /// chunk's current `patch_epoch` (a DBG `Chunk::patch_byte` of an `Op::Break`
    /// bumps it — set AND restore both invalidate), AND (b) every `deps` entry's
    /// stored epoch still matches its foreign chunk's current `patch_epoch` (the
    /// cross-proto Unit-C hole: a breakpoint patched into an INLINED callee must
    /// drop the CALLER's stream even though the caller's own bytes are untouched).
    /// `deps` is empty until Task 9, so (b) is trivially true today — but the check
    /// is here, exercised by the §8.4 unit battery, so Unit C inherits a proven
    /// invalidation seam rather than re-deriving one.
    ///
    /// Stale ⇒ the consult DROPS the stream and rebuilds from the (now-patched)
    /// bytes; it NEVER edits a cached stream in place.
    pub(crate) fn is_valid(&self, own_chunk: &Chunk) -> bool {
        if self.own_epoch != own_chunk.patch_epoch.get() {
            return false;
        }
        self.deps
            .iter()
            .all(|(proto, epoch)| proto.chunk.patch_epoch.get() == *epoch)
    }
}

impl fmt::Debug for DecodedChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecodedChunk")
            .field("records", &self.records.len())
            .field("own_epoch", &self.own_epoch)
            .finish()
    }
}

/// DECODE configuration (spec §2.2). In Task 3 only `plain()` (no fusion, no
/// inlining) exists; Task 8/9 add the fuse/inline knobs.
pub(crate) struct DecodeCfg {
    /// Unit B (Task 8): peephole-fuse the census set. ON whenever decode is on
    /// (fusion rides the master `decode` switch — no separate toggle).
    pub fuse: bool,
    /// Unit C (Task 9): inline small callees. Gated by `Vm.decode_inline`
    /// (`ASCRIPT_NO_DECODE_INLINE`). When on, [`decode_chunk`] runs the §6 inline
    /// predicate + transform after fusion (it needs a [`DecodeResolver`] callback to
    /// resolve a `GET_GLOBAL` candidate to its callee proto — `decode.rs` stays
    /// Vm-free / pure-analysis).
    pub inline: bool,
}

impl DecodeCfg {
    /// A 1:1 decode: no fusion, no inlining (Task 3). Now used ONLY by the decode
    /// unit tests that assert the unfused 1:1 record shape (the production path
    /// always fuses via [`fused`](Self::fused)).
    #[cfg(test)]
    pub fn plain() -> Self {
        DecodeCfg { fuse: false, inline: false }
    }

    /// Unit B (Task 8): a fusing decode — the peephole rewrites the census set
    /// into superinstructions. The production decode path (`select_record_source`)
    /// builds with this so fusion is active whenever the `decode` switch is on.
    pub fn fused() -> Self {
        DecodeCfg { fuse: true, inline: false }
    }

    /// Units B+C (Task 9): a fusing + inlining decode — the production path when
    /// `Vm.decode_inline` is on. Fusion runs first (the peephole), then the §6 inline
    /// predicate/transform splices qualifying small global-fn calls.
    pub fn fused_inline() -> Self {
        DecodeCfg { fuse: true, inline: true }
    }
}

/// DECODE §6.1: the Vm-supplied resolution callback. `decode.rs` is pure-analysis
/// and Vm-free; the inline predicate needs to resolve a `GET_GLOBAL name` candidate
/// to its `(global_idx, struct_gen, callee proto)` at decode time. The Vm closure
/// reads `get_user_global_full` + `struct_gen` (mirroring the guard the
/// `InlineEnter` arm re-checks at runtime). Returns `None` when the name does not
/// resolve to a `Value::Closure` user-global (late-binding-correct: an undefined /
/// non-closure global simply never inlines).
pub(crate) struct InlineResolution {
    pub global_idx: u32,
    pub struct_gen: u64,
    pub callee: std::rc::Rc<crate::vm::chunk::FnProto>,
}

/// Decode `chunk` 1:1 into fixed-width records with jump targets pre-resolved to
/// record indices. Returns `None` on any structural anomaly (an unknown opcode
/// byte, a truncated operand, or a jump target that does not land on an
/// instruction boundary) — the caller then falls back to byte dispatch for this
/// proto, permanently. The `.aso` verifier rejects such corruption at the trust
/// boundary; the `None` here is the in-runtime backstop.
///
/// Reads the CURRENT bytes: a byte patched to `Op::Break` decodes as a
/// `Base(Break)` record (an escalation record, spec §4.3). `own_epoch` is read
/// AFTER the walk so it conservatively reflects the bytes that were decoded.
pub(crate) fn decode_chunk(chunk: &Chunk, cfg: &DecodeCfg) -> Option<std::rc::Rc<DecodedChunk>> {
    decode_chunk_resolved(chunk, cfg, None)
}

/// DECODE §6: like [`decode_chunk`] but with the Vm-supplied inline resolver. The
/// resolver maps a `GET_GLOBAL` const-name index to an [`InlineResolution`] (the
/// callee proto + the recorded guard stamp) at decode time. `None` ⇒ no inlining is
/// attempted (the pre-Unit-C path); `cfg.inline` must also be on for the transform
/// to run. The resolver keeps `decode.rs` Vm-free: it is the ONLY way decode reads
/// the live `user_globals`.
pub(crate) fn decode_chunk_resolved(
    chunk: &Chunk,
    cfg: &DecodeCfg,
    resolver: Option<&dyn Fn(u32) -> Option<InlineResolution>>,
) -> Option<std::rc::Rc<DecodedChunk>> {
    let code: &[u8] = &chunk.code;

    // ---- Pass 1: walk the byte stream, one record per instruction. ----------
    // The `bcanalysis` idiom (src/vm/bcanalysis.rs:151-160): `Op::from_u8` +
    // `Op::operand_width` is the SINGLE source of truth for instruction width;
    // we read each instruction's operands by SHAPE (spec §2.1) into `a`/`b`.
    let mut records: Vec<DecodedInstr> = Vec::new();
    // byte_off -> record index (for jump-target resolution in pass 2).
    let mut index_of: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    // Per jump record: (record_idx, target_byte) for pass-2 resolution.
    let mut jump_fixups: Vec<(usize, i64)> = Vec::new();

    let mut ip: usize = 0;
    while ip < code.len() {
        let op = Op::from_u8(code[ip])?;
        let width = op.operand_width();
        // A truncated final instruction (operand runs past the code end) is a
        // structural anomaly.
        if ip + 1 + width > code.len() {
            return None;
        }
        let rec_idx = records.len();
        index_of.insert(ip as u32, rec_idx as u32);

        let (a, b) = decode_operands(chunk, op, ip, width, rec_idx, &mut jump_fixups)?;
        records.push(DecodedInstr { op: DOp::Base(op), a, b, off: ip as u32 });

        ip += 1 + width;
    }

    // ---- Pass 2: resolve every jump's target BYTE to a target RECORD index. --
    // The displacement is measured from the byte AFTER the operand (chunk.rs
    // `patch_jump`/`emit_loop`: `from = site + width_of_disp`); the target byte
    // must land on an instruction boundary (a key in `index_of`) or the stream
    // is corrupt → None (permanent byte-dispatch fallback).
    //
    // DECODE §5.2: collect the JUMP-TARGET record-index set as we resolve. The
    // peephole (`fuse_records`) consults it to keep a jump destination a SEPARATE
    // instruction — a record that is a jump target must remain the FIRST component
    // of any fused run (never swallowed into a fused middle/tail), because the
    // record driver re-derives its cursor from the canonical BYTE ip on every
    // taken jump (`byte_to_record`), which can only land on a record's own `off`.
    let mut jump_targets: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for (rec_idx, target_byte) in jump_fixups {
        if target_byte < 0 || target_byte > u32::MAX as i64 {
            return None;
        }
        let target_rec = *index_of.get(&(target_byte as u32))?;
        jump_targets.insert(target_rec);
        // The jump op's resolved record index lives in `a` for the i16 jumps and
        // in `b` for `JumpIfArgSupplied` (u16 + i16). `decode_operands` stashed
        // the raw displacement in the right field and recorded which via the op.
        let rec = &mut records[rec_idx];
        match rec.op {
            DOp::Base(Op::JumpIfArgSupplied) => rec.b = target_rec,
            // The fixup runs over the 1:1 stream — the peephole (DOp::Fused) and the
            // inline transform (DOp::InlineEnter/Exit) BOTH run AFTER this loop, so
            // only `Base` records appear here; the catch-all keeps the match total.
            DOp::Base(_) | DOp::Fused(_) | DOp::InlineEnter | DOp::InlineExit => {
                rec.a = target_rec
            }
        }
    }

    // ---- Unit B (Task 8): the decode-time PEEPHOLE (only when fusion is on). --
    // Rewrites consecutive base records into fused superinstructions, within basic
    // blocks only, never swallowing a jump target. `entry_index` is rebuilt over
    // the post-fusion record vector. When `cfg.fuse` is off this is a no-op and the
    // stream stays 1:1 (the pre-Task-8 behavior, byte-identical).
    let records = if cfg.fuse {
        fuse_records(records, &jump_targets)
    } else {
        records
    };

    // ---- Unit C (Task 9): the §6 INLINE transform (only when inlining is on AND a
    // resolver is supplied). Splices qualifying small global-fn calls
    // (`GET_GLOBAL f; args; CALL argc`) into the caller stream between an
    // `InlineEnter`/`InlineExit` pair; collects `inline_segments` + `deps`. When off
    // (or no site qualifies) the stream is unchanged and the tables stay empty.
    let (records, inline_segments, deps) = match (cfg.inline, resolver) {
        (true, Some(resolve)) => inline_calls(chunk, records, &jump_targets, resolve),
        _ => (records, Vec::new(), Vec::new()),
    };

    // ---- entry_index: sorted (byte_off, record_idx). Emission is monotonic so the
    // CALLER records are off-ascending; build the pairs directly. TWO offs are NOT
    // entries: (a) a fused component swallowed by the peephole (never a jump target),
    // and (b) an INLINED BODY record (it carries a CALLEE-chunk off — §2.5: "only
    // CALLER-chunk records are entered"; the body is reached only by sequential
    // `idx += 1` from `InlineEnter`, never by a byte→record resync). The driver's
    // resync over caller jump targets / frame entries thus still lands exactly on a
    // surviving CALLER record `off`. We carry an explicit in-segment mask to exclude
    // body offs (their byte values could otherwise collide with caller offs).
    let mut in_segment = vec![false; records.len()];
    for seg in &inline_segments {
        for r in seg.start..=seg.end {
            // [start, end] inclusive: the body records AND the InlineExit at `end`.
            if let Some(slot) = in_segment.get_mut(r as usize) {
                *slot = true;
            }
        }
    }
    let entry_index: Vec<(u32, u32)> = records
        .iter()
        .enumerate()
        .filter(|(i, _)| !in_segment[*i])
        .map(|(i, r)| (r.off, i as u32))
        .collect();

    // own_epoch read AFTER pass 1 (single-threaded: no patch can interleave, but
    // read-late is the conservative order — spec §2.2).
    let own_epoch = chunk.patch_epoch.get();

    Some(std::rc::Rc::new(DecodedChunk {
        records,
        entry_index,
        own_epoch,
        deps,
        inline_segments,
    }))
}

/// Read `op`'s inline operands at `ip` (the opcode byte) by SHAPE into `(a, b)`.
/// For a jump op, the RAW i16 displacement is converted to an absolute target
/// BYTE and pushed onto `jump_fixups` (pass 2 rewrites it to a record index);
/// the operand field that will hold the resolved index is left 0 here.
fn decode_operands(
    chunk: &Chunk,
    op: Op,
    ip: usize,
    width: usize,
    rec_idx: usize,
    jump_fixups: &mut Vec<(usize, i64)>,
) -> Option<(u32, u32)> {
    // The operand bytes start at ip + 1.
    let o = ip + 1;
    Some(match op {
        // i16 jump ops: a = (pass-2) target record index. The displacement is
        // measured from the byte after the operand (o + 2).
        Op::Jump | Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNotNil | Op::Loop => {
            let disp = chunk.read_i16(o) as i64;
            let target_byte = (o + 2) as i64 + disp;
            jump_fixups.push((rec_idx, target_byte));
            (0, 0)
        }
        // u16 + i16: a = u16 param-index, b = (pass-2) target record index. The
        // i16 is the SECOND word; its displacement is measured from the byte
        // after it (o + 2 + 2).
        Op::JumpIfArgSupplied => {
            let param_index = chunk.read_u16(o) as u32;
            let disp = chunk.read_i16(o + 2) as i64;
            let target_byte = (o + 4) as i64 + disp;
            jump_fixups.push((rec_idx, target_byte));
            (param_index, 0)
        }
        // u16 + u8.
        Op::CallMethod | Op::MatchArray | Op::DefineGlobal | Op::CallNamed => {
            (chunk.read_u16(o) as u32, chunk.read_u8(o + 2) as u32)
        }
        // DEFER_PUSH_METHOD: u16 name + u8 flags + u8 argc (width 4). Pack the
        // two trailing bytes into `b` (lo = flags, hi = argc) so no operand is
        // lost; the record driver (Task 4) unpacks them. Distinct from the
        // u16+u8 arm because of the extra trailing byte.
        Op::DeferPushMethod => {
            let name = chunk.read_u16(o) as u32;
            let flags = chunk.read_u8(o + 2) as u32;
            let argc = chunk.read_u8(o + 3) as u32;
            (name, flags | (argc << 8))
        }
        // DEFER_PUSH: u8 flags + u8 argc (width 2). Pack flags|argc<<8 into `a`.
        Op::DeferPush => {
            let flags = chunk.read_u8(o) as u32;
            let argc = chunk.read_u8(o + 1) as u32;
            (flags | (argc << 8), 0)
        }
        // Remaining shapes by width (the survey, spec §2.1).
        _ => match width {
            0 => (0, 0),
            1 => (chunk.read_u8(o) as u32, 0),
            2 => (chunk.read_u16(o) as u32, 0),
            // No base op outside the arms above carries 3/4 operand bytes; an
            // unexpected width is a decode gap → bail to byte dispatch.
            _ => return None,
        },
    })
}

/// DECODE §5.2 (Unit B): the decode-time PEEPHOLE. A single left-to-right pass over
/// the 1:1 `records`, greedy-longest-matching each position against
/// [`FUSION_CANDIDATES`] and rewriting a matched run into ONE [`DOp::Fused`]
/// record. Returns the rewritten record vector (shorter than the input when any
/// fusion fired).
///
/// **Legality (the load-bearing correctness rules — a violation is a real bug):**
/// - **No jump target swallowed.** Only the FIRST component of a fused run may be a
///   jump target; if ANY later component's record index is in `jump_targets`, the
///   match is refused. The driver re-derives its record cursor from the canonical
///   BYTE ip on every taken jump (`byte_to_record`), which can only land on a
///   surviving record's `off` — a swallowed jump-target byte would resync-miss and
///   silently fall back to byte dispatch (correct but dark) or, worse, mis-land.
///   Keeping every jump target a first-component `off` makes the post-fusion
///   `entry_index` contain every legal jump destination.
/// - **One basic block.** A candidate's sequence is matched ONLY against
///   consecutive records; the census already counted pairs/triples within basic
///   blocks, but the peephole re-checks structurally: every component but the last
///   must be a NON-terminator (a terminator op mid-run would mean the run crossed a
///   block boundary). All shipped candidates' non-final components
///   (`GetLocal`/`GetProp`/`Const`) are non-terminators, so this holds by
///   construction; the check is kept defensive.
/// - **Operand fit.** Every component operand is a `u16` (slot / const index),
///   packed two-per-`u32` into the fused record's `a` (low = first, high = second).
///
/// The fused record's `off` is the FIRST component's byte offset (the ip↔record
/// bridge anchor); the driver reconstructs each later component's byte offset by
/// adding the compile-time component widths, so spans / adaptive-cache keys are
/// attributed at each component's OWN byte offset — byte-identical to the unfused
/// sequence.
pub(crate) fn fuse_records(
    records: Vec<DecodedInstr>,
    jump_targets: &std::collections::HashSet<u32>,
) -> Vec<DecodedInstr> {
    let mut out: Vec<DecodedInstr> = Vec::with_capacity(records.len());
    let n = records.len();
    let mut i = 0usize;
    while i < n {
        // Greedy LONGEST-match: FUSION_CANDIDATES is ordered longest-first, so the
        // first candidate whose whole sequence matches here is the longest legal one.
        let mut fused = None;
        for form in FUSION_CANDIDATES {
            let len = form.seq.len();
            if i + len > n {
                continue;
            }
            if try_match(&records, i, form, jump_targets) {
                fused = Some((form, len));
                break;
            }
        }
        match fused {
            Some((form, len)) => {
                out.push(make_fused(&records[i..i + len], form.kind));
                i += len;
            }
            None => {
                out.push(records[i]);
                i += 1;
            }
        }
    }
    out
}

/// `true` iff `form.seq` matches the `len` records starting at `start`, AND the
/// match is LEGAL: no component but the first is a jump target, and no component
/// but the last is a basic-block terminator (a within-block run only). The records
/// must already be `Base` ops (the peephole runs over the 1:1 stream — there is no
/// pre-existing fusion to nest).
fn try_match(
    records: &[DecodedInstr],
    start: usize,
    form: &FusedForm,
    jump_targets: &std::collections::HashSet<u32>,
) -> bool {
    let len = form.seq.len();
    for (k, &want) in form.seq.iter().enumerate() {
        let idx = start + k;
        // The component op must match exactly (1:1 base records only).
        match records[idx].op {
            DOp::Base(op) if op == want => {}
            _ => return false,
        }
        // No component PAST THE FIRST may be a jump target — it must stay a
        // separate, byte-addressable record (a surviving `off`).
        if k > 0 && jump_targets.contains(&(idx as u32)) {
            return false;
        }
        // No component but the last may be a basic-block terminator (a run that
        // crossed a block boundary). All shipped candidates satisfy this; kept
        // defensive so a future candidate cannot silently fuse across a block.
        if k + 1 < len && is_block_terminator_op(want) {
            return false;
        }
    }
    true
}

/// DECODE §5.2: pack a matched run of base records into one fused record. The
/// fused record's `off` is the first component's `off`; operands are packed
/// two-per-`u32` into `a` (low half = first component's u16 operand, high half =
/// second component's u16 operand, or 0 when the component is zero-operand). The
/// triple's third component (`Add`) is zero-operand, so two halves suffice.
fn make_fused(run: &[DecodedInstr], kind: FusedKind) -> DecodedInstr {
    let off = run[0].off;
    // Each base record stashed its (first) u16 operand in `a` during the 1:1
    // decode (slot for GetLocal, const-idx for Const/GetProp). A zero-operand
    // component (`Add`) has `a == 0`.
    let lo = run[0].a & 0xffff;
    let hi = if run.len() >= 2 { run[1].a & 0xffff } else { 0 };
    DecodedInstr { op: DOp::Fused(kind), a: lo | (hi << 16), b: 0, off }
}

/// DECODE §5.2: `true` iff `op` is a basic-block terminator for the peephole's
/// legality check. Mirrors the census's `op_is_block_terminator` set (control
/// flow + control-leaving + call/suspension) so the peephole never fuses across a
/// boundary the census could not have counted across. Available in ALL builds (the
/// census helper is feature-gated; this one rides the always-compiled peephole).
fn is_block_terminator_op(op: Op) -> bool {
    matches!(
        op,
        Op::Jump
            | Op::JumpIfFalse
            | Op::JumpIfTrue
            | Op::JumpIfNotNil
            | Op::Loop
            | Op::JumpIfArgSupplied
            | Op::Return
            | Op::Propagate
            | Op::Yield
            | Op::Unwrap
            | Op::MatchNoArm
            | Op::Call
            | Op::CallSpread
            | Op::CallMethod
            | Op::CallMethodSpread
            | Op::CallNamed
            | Op::CallNamedSpread
            | Op::Await
            | Op::IterNext
            | Op::GetIter
            | Op::Import
            | Op::Break
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// DECODE §6 (Unit C): speculative inlining of small hot global fns.
//
// The transform runs over the post-fusion CALLER records. For each `GET_GLOBAL f;
// args; CALL argc` site whose resolved callee passes the §6.1 predicate, it splices
// the callee's own (fused) body records between an `InlineEnter`/`InlineExit` pair
// and RETAINS the original `Base(Call)` record as the single-branch deopt target.
// ─────────────────────────────────────────────────────────────────────────────

/// §6.1: max callee slots (window size) an inline segment may build.
const INLINE_MAX_SLOTS: u16 = 8;
/// §6.1: max body records (after the callee's own fusion, excluding the terminal
/// `Return`) an inline segment may splice.
const INLINE_MAX_RECORDS: usize = 16;

/// DECODE §6.1: is `op` allowed inside an inlined STRAIGHT-LINE LEAF body? The body
/// is dispatched by [`crate::vm::run::Vm::exec_inline_body`] (a bounded executor
/// reusing the same shared helpers the single-op arms call), so the admissible set
/// is exactly what that executor implements: the arithmetic/stack/local/const
/// sync-subset plus read-only field/global reads — and NOTHING that transfers
/// control (jumps/calls/defer/await/yield/return-mid-body) or needs a frame
/// (`GetSuper`, cells). A body op outside this set ⇒ the site is NOT inlined
/// (decodes 1:1, panics/calls generically). Conservative by construction — adding a
/// body op here requires a matching `exec_inline_body` arm + a differential proof.
fn inline_body_op_ok(op: Op) -> bool {
    matches!(
        op,
        Op::Const
            | Op::Nil
            | Op::True
            | Op::False
            | Op::Pop
            | Op::Dup
            | Op::Swap
            | Op::Rot3
            | Op::GetLocal
            | Op::SetLocal
            | Op::GetGlobal
            | Op::GetProp
            | Op::Add
            | Op::Sub
            | Op::Mul
            | Op::Div
            | Op::Mod
            | Op::Pow
            | Op::Lt
            | Op::Le
            | Op::Gt
            | Op::Ge
            | Op::Eq
            | Op::Ne
            | Op::BitAnd
            | Op::BitOr
            | Op::BitXor
            | Op::Shl
            | Op::Shr
            | Op::WrapAdd
            | Op::WrapSub
            | Op::WrapMul
            | Op::Neg
            | Op::Not
            | Op::BitNot
    )
}

/// DECODE §6.1: the inline PREDICATE over a resolved callee proto + the static
/// `argc`. Returns `Some(body_records)` (the callee's fused, 1:1-decoded body
/// EXCLUDING its terminal `Return`) iff EVERY clause holds; `None` (refuse → decode
/// the site 1:1) otherwise. Table-driven: each `return None` is a labelled refusal
/// clause the reviewer probes.
fn inline_predicate(
    callee: &std::rc::Rc<crate::vm::chunk::FnProto>,
    argc: u8,
) -> Option<Vec<DecodedInstr>> {
    // Clause: never async / generator / worker (a frame-bound suspension model).
    if callee.is_async || callee.is_generator || callee.is_worker {
        return None;
    }
    // Clause: no captures, no cell slots (a capture-free leaf — the window is a flat
    // slice of the operand stack, no upvalue cells to allocate).
    if !callee.chunk.upvalues.is_empty() || !callee.chunk.cell_slots.is_empty() {
        return None;
    }
    // Clause: every param untyped, no rest, no defaults. A typed/defaulted param
    // makes `check_call_args` more than the exact-arity check the decoder performs
    // statically; a rest param collects a tail. (Defaults compile to a
    // `JumpIfArgSupplied` prologue, caught by the straight-line check below too, but
    // the param scan is the direct clause.)
    if callee.has_rest {
        return None;
    }
    for p in &callee.params {
        if p.ty.is_some() || p.rest {
            return None;
        }
    }
    // Clause: no return-type contract (no exit check to run).
    if callee.ret.is_some() {
        return None;
    }
    // Clause: exact-arity match — argc == arity (else the generic path raises the
    // byte-identical arity panic at runtime; never inline a mismatched site).
    if argc as usize != callee.params.len() || argc as u16 != callee.arity as u16 {
        return None;
    }
    // Clause: window size bound.
    if callee.chunk.slot_count > INLINE_MAX_SLOTS {
        return None;
    }
    // Decode the callee body (FUSED, like the production stream) so the spliced
    // records match what the callee would retire on its own. A structural anomaly
    // (None) ⇒ refuse. (Depth-1: the callee is decoded WITHOUT inlining — `fused()`,
    // not `fused_inline()` — so an inline segment is never built from a stream that
    // itself contains inline segments.)
    let body = decode_chunk(&callee.chunk, &DecodeCfg::fused())?;
    // The body must end in exactly one `Return` and be otherwise straight-line leaf:
    // no jumps/jump-targets, no call-class/defer/suspension ops, only the admissible
    // body subset. Walk every record.
    let recs = &body.records;
    if recs.is_empty() {
        return None;
    }
    // The body is STRAIGHT-LINE (no jumps — enforced below), so the FIRST `Return` is
    // the single reachable exit; anything after it is dead epilogue (the compiler
    // appends an implicit `Nil; Return` to every function body). The records BEFORE
    // that first Return are the real body (the inline segment); the first Return
    // becomes the `InlineExit`; the dead tail is dropped.
    let first_return = recs.iter().position(|r| matches!(r.op, DOp::Base(Op::Return)))?;
    let body_recs = &recs[..first_return];
    // Every body record must be an admissible op (Base, or a Fused whose components
    // are all admissible) — in particular NO jump/call/return/suspension (a jump would
    // mean the body is not straight-line, breaking the linear executor + the
    // single-exit assumption).
    for rec in body_recs {
        match rec.op {
            DOp::Base(op) => {
                if !inline_body_op_ok(op) {
                    return None;
                }
            }
            DOp::Fused(kind) => {
                if !kind.components().iter().all(|&c| inline_body_op_ok(c)) {
                    return None;
                }
            }
            // The callee was decoded with `fused()` (inline off) ⇒ no nested
            // InlineEnter/InlineExit can appear. Defensive refuse.
            DOp::InlineEnter | DOp::InlineExit => return None,
        }
    }
    // Body record budget (excluding the terminal Return).
    if body_recs.len() > INLINE_MAX_RECORDS {
        return None;
    }
    // Return the body records BEFORE the first Return (which becomes InlineExit).
    Some(body_recs.to_vec())
}

/// DECODE §6.1/§6.2 (the transform): splice every qualifying `GET_GLOBAL f; args;
/// CALL argc` site. Returns `(records, inline_segments, deps)`.
///
/// Layout per inlined site (record order):
/// ```text
///   … GET_GLOBAL f; <arg records> …      (caller records, kept verbatim)
///   InlineEnter   (off = CALL off; a = seg idx; b = fallback rec idx)
///   <body records>                         (callee offs; run on a guard HIT)
///   InlineExit    (off = CALL off; a = seg idx)
///   Base(Call) argc (off = CALL off)       (the RETAINED single-branch deopt target)
///   … caller records after the call …
/// ```
/// On HIT the driver runs the body + seeks the cursor PAST the retained `Call`; on
/// MISS it seeks to the retained `Call` and dispatches it generically (§6.2). Only
/// the `InlineEnter` of the trio is an `entry_index` boundary; the body offs are
/// callee-chunk and excluded.
///
/// The result: the rewritten records, the inline-segment table, and the §4.2 deps.
type InlineTransform =
    (Vec<DecodedInstr>, Vec<InlineSegment>, Vec<(std::rc::Rc<crate::vm::chunk::FnProto>, u64)>);

fn inline_calls(
    chunk: &Chunk,
    records: Vec<DecodedInstr>,
    jump_targets: &std::collections::HashSet<u32>,
    resolve: &dyn Fn(u32) -> Option<InlineResolution>,
) -> InlineTransform {
    let n = records.len();
    let mut out: Vec<DecodedInstr> = Vec::with_capacity(n);
    let mut segments: Vec<InlineSegment> = Vec::new();
    let mut deps: Vec<(std::rc::Rc<crate::vm::chunk::FnProto>, u64)> = Vec::new();
    let mut i = 0usize;
    while i < n {
        // A candidate HEAD is a `Base(GetGlobal)` whose name resolves to a closure
        // user-global, followed (somewhere ahead, after a straight-line arg region)
        // by a `Base(Call)` with argc == the args between them.
        if let Some(splice) = try_inline_site(&records, i, jump_targets, resolve) {
            let SiteMatch { call_rec_idx, argc, res } = splice;
            // Predicate over the resolved callee.
            if let Some(body) = inline_predicate(&res.callee, argc) {
                let call_off = records[call_rec_idx].off;
                // Emit the caller records from `i` up to and INCLUDING the last arg
                // record (everything before the CALL record) verbatim.
                for r in &records[i..call_rec_idx] {
                    out.push(*r);
                }
                // InlineEnter at the CALL off.
                let seg_idx = segments.len() as u32;
                let enter_idx = out.len() as u32;
                out.push(DecodedInstr {
                    op: DOp::InlineEnter,
                    a: seg_idx,
                    b: 0, // patched below to the retained-Call record index
                    off: call_off,
                });
                // Body records (callee offs), verbatim.
                let body_start = out.len() as u32;
                for r in &body {
                    out.push(*r);
                }
                let body_end = out.len() as u32; // == index of the InlineExit
                out.push(DecodedInstr { op: DOp::InlineExit, a: seg_idx, b: 0, off: call_off });
                // The RETAINED original Call record (the single-branch deopt target).
                let fallback_idx = out.len() as u32;
                out.push(records[call_rec_idx]);
                // Patch InlineEnter.b = the fallback (retained Call) record index.
                out[enter_idx as usize].b = fallback_idx;
                segments.push(InlineSegment {
                    start: body_start,
                    end: body_end, // the InlineExit index; entry-mask covers [start, end]
                    global_idx: res.global_idx,
                    recorded_gen: res.struct_gen,
                    callee: res.callee.clone(),
                    slot_count: res.callee.chunk.slot_count,
                    argc,
                });
                // §4.2 deps: the caller stream now embeds the callee's bytes — a
                // breakpoint patched into the callee bumps its epoch → this caller
                // stream must drop. Record once per distinct callee.
                let ep = res.callee.chunk.patch_epoch.get();
                if !deps.iter().any(|(p, _)| std::rc::Rc::ptr_eq(p, &res.callee)) {
                    deps.push((res.callee.clone(), ep));
                }
                i = call_rec_idx + 1;
                continue;
            }
        }
        out.push(records[i]);
        i += 1;
    }
    let _ = chunk;
    (out, segments, deps)
}

/// A matched inline site: the retained `Call` record index, its static `argc`, and
/// the resolved callee.
struct SiteMatch {
    call_rec_idx: usize,
    argc: u8,
    res: InlineResolution,
}

/// DECODE §6.1: try to match a `GET_GLOBAL f; <argc straight-line arg records>;
/// CALL argc` site STARTING at record `start`. The arg region must be straight-line
/// (no jump targets inside, no terminators) and must push exactly `argc` values
/// (one record per pushed value, counting a fused record by its NET push). For the
/// shapes the predicate admits this is the common `GET_GLOBAL; (GetLocal|Const|…)×argc;
/// CALL argc`. Returns `None` if `start` is not a resolvable-closure `GET_GLOBAL`
/// head, or the following region is not a clean straight-line call.
fn try_inline_site(
    records: &[DecodedInstr],
    start: usize,
    jump_targets: &std::collections::HashSet<u32>,
    resolve: &dyn Fn(u32) -> Option<InlineResolution>,
) -> Option<SiteMatch> {
    // Head must be Base(GetGlobal); its `a` is the const-name index.
    let name_idx = match records.get(start)?.op {
        DOp::Base(Op::GetGlobal) => records[start].a,
        _ => return None,
    };
    // Resolve the name → a closure user-global (else: not inlineable, late binding
    // preserved — an undefined / non-closure global never inlines).
    let res = resolve(name_idx)?;
    // Walk forward over the straight-line arg region until the matching CALL. Track
    // the net operand-stack push so we can match it against the CALL's argc. The arg
    // region must not contain a jump target, a terminator, or a nested call.
    let mut j = start + 1;
    let mut pushed: i64 = 0;
    while j < records.len() {
        // No record past the head may be a jump TARGET (a back-edge into the arg
        // region would make the linear scan unsound).
        if jump_targets.contains(&(j as u32)) {
            return None;
        }
        match records[j].op {
            DOp::Base(Op::Call) => {
                let argc = records[j].a;
                if argc > u8::MAX as u32 {
                    return None;
                }
                // The arg region must have pushed exactly `argc` values atop the
                // callee (the callee value itself was pushed by GET_GLOBAL).
                if pushed != argc as i64 {
                    return None;
                }
                return Some(SiteMatch { call_rec_idx: j, argc: argc as u8, res });
            }
            DOp::Base(op) => {
                // Only a straight-line arg-staging op is allowed in the region. Reuse
                // the same admissible body subset (a superset-safe gate: these all
                // have a well-defined net push and never transfer control). A
                // `SetLocal`/`Pop` is admissible BUT pops — see the `pushed < 0` guard.
                if !inline_body_op_ok(op) {
                    return None;
                }
                pushed += net_push_base(op);
            }
            DOp::Fused(kind) => {
                pushed += net_push_fused(kind);
            }
            DOp::InlineEnter | DOp::InlineExit => return None,
        }
        // CRITICAL SOUNDNESS: the CALLEE value pushed by the head `GET_GLOBAL f` sits
        // at the BOTTOM of the arg region's window; it must remain the value the CALL
        // dispatches. If `pushed` ever goes NEGATIVE, an arg-region op popped BELOW the
        // region baseline — i.e. it consumed the callee (e.g. `GET_GLOBAL f; SetLocal 0;
        // GetLocal 0; CALL` stores f into a local and calls the local, NOT the global f
        // the resolver/guard recorded). REFUSE — the recorded callee identity would not
        // match the value actually called. (This is the bug a net-push-only count
        // missed: the final `pushed == argc` can hold even after the callee was popped.)
        if pushed < 0 {
            return None;
        }
        j += 1;
    }
    None
}

/// Net operand-stack push (pushes − pops) of an admissible straight-line base op.
/// Used to count the arg region against the CALL's argc.
fn net_push_base(op: Op) -> i64 {
    match op {
        Op::Const | Op::Nil | Op::True | Op::False | Op::GetLocal | Op::GetGlobal => 1,
        Op::Pop | Op::SetLocal => -1,
        Op::Dup => 1,
        Op::Swap | Op::Rot3 => 0,
        // unary: pop 1, push 1
        Op::Neg | Op::Not | Op::BitNot => 0,
        Op::GetProp => 0, // pop obj, push field
        // binary: pop 2, push 1
        _ => -1,
    }
}

/// Net operand-stack push of a fused record (the sum of its components' net pushes).
fn net_push_fused(kind: FusedKind) -> i64 {
    kind.components().iter().map(|&c| net_push_base(c)).sum()
}

// ─────────────────────────────────────────────────────────────────────────────
// DECODE §5.1 (Unit B part 1): the PAIR/TRIPLE census (FULLY feature-gated).
//
// The whole counting apparatus below is `#[cfg(feature = "decode-census")]`, so a
// default `cargo build`/`cargo test` compiles NONE of it — the JIT-spec §2.1
// "not there" discipline (zero Gate-12 hot-path exposure). It exists ONLY to feed
// the `tests/decode_census.rs` harness, whose ranked output (committed verbatim to
// `bench/DECODE_PAIR_CENSUS.md`) is the MEASURED data Task 8 fuses into
// superinstructions — the spec mandates fusion pairs be chosen from data, never
// guessed.
// ─────────────────────────────────────────────────────────────────────────────

/// The sentinel `prev` value used as the FIRST key slot of a PAIR record, so a
/// single `HashMap` holds both pairs (`(SENTINEL, prev, op)`) and triples
/// (`(prev2, prev, op)`). `u16::MAX` is unreachable as a real `Op` discriminant
/// (`Op` is `#[repr(u8)]` ⇒ every real discriminant is ≤ 255), so a pair key can
/// never collide with a triple key.
#[cfg(feature = "decode-census")]
pub(crate) const CENSUS_NO_PREV: u16 = u16::MAX;

/// DECODE §5.1: the dynamic pair/triple frequency census. Keyed by `Op`
/// discriminants (`op as u8` widened to `u16`). A PAIR `(prev, op)` is stored at
/// `(CENSUS_NO_PREV, prev, op)`; a TRIPLE `(prev2, prev, op)` at `(prev2, prev,
/// op)`. `total_records` counts every record retired in census mode (the
/// denominator for the "% of total records" column). Burst-local `prev`/`prev2`
/// live on the [`RecordSource`](crate::vm::run) and reset at every basic-block
/// boundary, so NO pair/triple is ever counted across a jump/escalation/entry — a
/// fused superinstruction could not legally cross such a boundary.
/// DECODE §5.1: the census count table — `(slot0, prev, op)` → dynamic count, where
/// slot0 = [`CENSUS_NO_PREV`] marks a PAIR and any other value marks a TRIPLE.
#[cfg(feature = "decode-census")]
pub(crate) type CensusCounts = std::collections::HashMap<(u16, u16, u16), u64>;

#[cfg(feature = "decode-census")]
#[derive(Default)]
pub(crate) struct DecodeCensus {
    /// `(slot0, prev, op)` → dynamic count. Pairs key slot0 = `CENSUS_NO_PREV`.
    pub counts: CensusCounts,
    /// Total records retired in census mode (the % denominator).
    pub total_records: u64,
}

#[cfg(feature = "decode-census")]
impl DecodeCensus {
    /// Record one retired op given the burst-local `(prev2, prev)` predecessors
    /// (each `Some` only when the predecessor is IN THE SAME basic block — the
    /// caller resets them at every boundary). Bumps `total_records`, the
    /// `(prev, op)` pair (when `prev` is in-block), and the `(prev2, prev, op)`
    /// triple (when BOTH predecessors are in-block).
    pub(crate) fn record(&mut self, prev2: Option<u16>, prev: Option<u16>, op: u16) {
        self.total_records += 1;
        if let Some(p) = prev {
            *self.counts.entry((CENSUS_NO_PREV, p, op)).or_insert(0) += 1;
            if let Some(p2) = prev2 {
                *self.counts.entry((p2, p, op)).or_insert(0) += 1;
            }
        }
    }
}

/// Binary-search the byte offset `off` to its record index in `d` (the
/// `entry_index` is sorted ascending by byte offset). `None` if `off` is not a
/// record boundary.
pub(crate) fn byte_to_record(d: &DecodedChunk, off: u32) -> Option<u32> {
    d.entry_index
        .binary_search_by_key(&off, |&(byte_off, _)| byte_off)
        .ok()
        .map(|i| d.entry_index[i].1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::Span;
    use crate::value::Value;

    fn s(a: usize, b: usize) -> Span {
        Span::new(a, b)
    }

    #[test]
    fn decode_one_to_one_covers_every_instruction_and_widens_operands() {
        // Build a chunk exercising every operand SHAPE (spec §2.1): zero-op, u8,
        // u16, i16 jump, u16+u8, u16+i16.
        let mut c = Chunk::new();
        let k = c.add_const(Value::Float(1.0));
        c.emit_u16(Op::Const, k, s(0, 1)); // u16
        c.emit_u8(Op::Call, 2, s(1, 2)); // u8
        c.emit_u16_u8(Op::DefineGlobal, 3, 1, s(2, 3)); // u16+u8
        let site = c.emit_jump(Op::Jump, s(3, 4)); // i16 (forward)
        c.emit(Op::Add, s(4, 5)); // zero-op
        c.patch_jump(site); // target = end
        c.emit(Op::Return, s(5, 6));
        let d = decode_chunk(&c, &DecodeCfg::plain()).expect("decodes");
        // One record per instruction, in order, off = the opcode byte's offset.
        let offs: Vec<u32> = d.records.iter().map(|r| r.off).collect();
        let mut walk = Vec::new(); // independent operand_width walk
        let mut ip = 0;
        while ip < c.code.len() {
            walk.push(ip as u32);
            ip += 1 + Op::from_u8(c.code[ip]).unwrap().operand_width();
        }
        assert_eq!(offs, walk);
        // Operands widened: Const's u16 in `a`; DefineGlobal's u16 in `a`, u8 in `b`.
        assert!(matches!(d.records[0].op, DOp::Base(Op::Const)));
        assert_eq!(d.records[0].a, k as u32);
        assert_eq!((d.records[2].a, d.records[2].b), (3, 1));
    }

    #[test]
    fn decode_resolves_jump_targets_to_record_indices() {
        // while-loop shape: JumpIfFalse forward over the body, Loop back to the test.
        let mut c = Chunk::new();
        c.emit(Op::True, s(0, 1)); // 0: rec 0 (loop head)
        let exit = c.emit_jump(Op::JumpIfFalse, s(1, 2)); // 1: rec 1
        c.emit(Op::Nil, s(2, 3)); // 4: rec 2
        c.emit(Op::Pop, s(3, 4)); // 5: rec 3
        c.emit_loop(Op::Loop, 0, s(4, 5)); // 6: rec 4 → target rec 0
        c.patch_jump(exit); // → target rec 5
        c.emit(Op::Return, s(5, 6)); // 9: rec 5
        let d = decode_chunk(&c, &DecodeCfg::plain()).unwrap();
        assert_eq!(d.records[1].a, 5, "JumpIfFalse pre-resolved to the Return record");
        assert_eq!(d.records[4].a, 0, "Loop pre-resolved to the head record");
        // entry_index is sorted by byte off and binary-searchable.
        for (i, r) in d.records.iter().enumerate() {
            assert_eq!(byte_to_record(&d, r.off), Some(i as u32));
        }
    }

    #[test]
    fn decode_reads_current_bytes_a_patched_break_decodes_as_break_record() {
        let mut c = Chunk::new();
        c.emit(Op::Nil, s(0, 1));
        c.emit(Op::Add, s(1, 2));
        c.emit(Op::Return, s(2, 3));
        c.patch_byte(1, Op::Break as u8); // a live breakpoint
        let d = decode_chunk(&c, &DecodeCfg::plain()).unwrap();
        assert!(
            matches!(d.records[1].op, DOp::Base(Op::Break)),
            "a patched byte bakes a Break (escalation) record — §4.3 soundness"
        );
        assert_eq!(d.own_epoch, c.patch_epoch.get(), "decoded AT the post-patch epoch");
    }

    #[test]
    fn decoder_refuses_anomalous_jump_targets() {
        // A hand-corrupted mid-instruction jump target (unreachable from the
        // compiler; the .aso verifier rejects it at the trust boundary) must yield
        // None — permanent byte-dispatch fallback, never a bad record.
        let mut c = Chunk::new();
        c.emit_u16(Op::Const, 0, s(0, 1));
        let site = c.emit_jump(Op::Jump, s(1, 2));
        c.emit(Op::Return, s(2, 3));
        c.code[site..site + 2].copy_from_slice(&(-4i16).to_le_bytes()); // lands mid-Const
        assert!(decode_chunk(&c, &DecodeCfg::plain()).is_none());
    }

    /// Reviewer checkpoint: the decode walk must reproduce `disasm_at`'s walk on
    /// a REAL compiled program. Compile `fib` (recursion + a branch + arithmetic),
    /// decode it, and assert the record offsets equal the disassembler's
    /// instruction offsets exactly — both engines walk via `Op::operand_width`, so
    /// any width disagreement (a per-op decode gap) surfaces here. Probes the
    /// whole proto tree (the top-level chunk + every nested `FnProto`).
    #[test]
    fn decode_offsets_match_disasm_on_a_real_compiled_program() {
        use crate::vm::disasm::disasm_at;
        let src = "fn fib(n) {\n  if (n < 2) { return n }\n  return fib(n - 1) + fib(n - 2)\n}\nprint(fib(10))\n";
        let chunk = crate::compile::compile_source(src).expect("compiles");

        fn check(chunk: &Chunk) {
            let d = decode_chunk(chunk, &DecodeCfg::plain()).expect("decodes");
            // The disassembler walks the same bytes via operand_width; collect its
            // instruction offsets independently.
            let mut disasm_offs = Vec::new();
            let mut off = 0usize;
            while off < chunk.code.len() {
                disasm_offs.push(off as u32);
                let _ = disasm_at(chunk, &mut off); // advances by the instruction width
            }
            let rec_offs: Vec<u32> = d.records.iter().map(|r| r.off).collect();
            assert_eq!(rec_offs, disasm_offs, "decode walk diverged from disasm walk");
            // Recurse into nested protos (fib's body).
            for proto in &chunk.protos {
                check(&proto.chunk);
            }
        }
        check(&chunk);
    }

    #[test]
    fn decode_side_slots_default_empty() {
        // The Task-1 deferred slot-default test (lands here now that DecodedChunk
        // exists): a fresh chunk has no decoded stream and zero decode warmth.
        let c = Chunk::new();
        assert!(c.decoded.borrow().is_none());
        assert_eq!(c.decode_warmth.get(), 0);
        assert_eq!(c.patch_epoch.get(), 0);
    }

    /// DECODE §8.4 #4 — the epoch/deps validity unit battery (the JIT-contract
    /// proof, the pure-unit half; the behavioral DAP/coverage halves live in
    /// `run.rs`'s test module, the chokepoint scan in `tests/vm_decode.rs`).
    ///
    /// (a) a stream built at epoch N is valid at N, INVALID after one `patch_byte`
    /// (a breakpoint SET, N→N+1), and INVALID again after the restore (N+1→N+2 —
    /// the epoch is monotonic, never compared by value); (b) a `deps` entry whose
    /// stored epoch goes stale invalidates the stream EVEN WHEN `own_epoch` still
    /// matches (the cross-proto Unit-C hole — a breakpoint in an inlined callee must
    /// drop the caller's stream though the caller's own bytes are untouched).
    #[test]
    fn decoded_chunk_validity_unit_tests() {
        // ── (a) own_epoch: built at N, invalid after set AND after restore. ──────
        let mut c = Chunk::new();
        c.emit(Op::Nil, s(0, 1));
        c.emit(Op::Add, s(1, 2));
        c.emit(Op::Return, s(2, 3));
        let d = decode_chunk(&c, &DecodeCfg::plain()).expect("decodes");
        assert_eq!(d.own_epoch, 0, "fresh chunk decoded at epoch 0");
        assert!(d.is_valid(&c), "valid at the epoch it was built");

        // A breakpoint SET (patch_byte of Op::Break) bumps the epoch → stale.
        let original = c.code[1];
        c.patch_byte(1, Op::Break as u8);
        assert_eq!(c.patch_epoch.get(), 1, "patch_byte bumped to N+1");
        assert!(!d.is_valid(&c), "the N-stamped stream is stale after the set");

        // The RESTORE (another patch_byte) bumps AGAIN → still stale (the value
        // returning to the pre-patch byte does NOT make the old stream valid; the
        // epoch is a monotonic generation counter, never compared by content).
        c.patch_byte(1, original);
        assert_eq!(c.patch_epoch.get(), 2, "the restore bumped to N+2");
        assert!(!d.is_valid(&c), "the N-stamped stream is still stale after the restore");

        // A freshly re-decoded stream (reads the restored bytes) is valid again.
        let d2 = decode_chunk(&c, &DecodeCfg::plain()).expect("re-decodes");
        assert_eq!(d2.own_epoch, 2);
        assert!(d2.is_valid(&c), "the rebuilt stream is valid at the current epoch");

        // ── (b) deps: a stale foreign-chunk epoch invalidates even when own matches.
        // Build a tiny foreign proto whose chunk records its epoch at "embed" time,
        // then bump the FOREIGN chunk's epoch (a breakpoint in the inlined callee).
        let mut fc = Chunk::new();
        fc.emit(Op::Return, s(0, 1));
        let foreign = std::rc::Rc::new(crate::vm::chunk::FnProto {
            chunk: fc,
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            is_worker: false,
            owning_class: None,
            params: Vec::new(),
            ret: None,
            local_names: Vec::new(),
            debug_name: None,
        });
        let embed_epoch = foreign.chunk.patch_epoch.get();

        // A caller stream whose own_epoch matches its own chunk, but which embedded
        // the foreign proto at `embed_epoch` (hand-built deps vec — testable now per
        // the plan, before Unit C populates deps for real).
        let caller_with_deps = std::rc::Rc::new(DecodedChunk {
            records: d2.records.clone(),
            entry_index: d2.entry_index.clone(),
            own_epoch: c.patch_epoch.get(), // own matches the (restored) caller chunk
            deps: vec![(foreign.clone(), embed_epoch)],
            inline_segments: Vec::new(),
        });
        assert!(
            caller_with_deps.is_valid(&c),
            "own_epoch + dep epoch both current ⇒ valid"
        );

        // Patch a breakpoint into the INLINED callee's chunk: own_epoch is untouched,
        // but the dep epoch goes stale → the caller's stream must be dropped.
        foreign.chunk.patch_byte(0, Op::Break as u8);
        assert_eq!(
            caller_with_deps.own_epoch,
            c.patch_epoch.get(),
            "the caller's own bytes (and epoch) are untouched"
        );
        assert!(
            !caller_with_deps.is_valid(&c),
            "a stale DEP epoch invalidates the caller stream even when own_epoch matches"
        );
    }

    /// DECODE §5.1: the census `record` helper. A pair is counted only when `prev`
    /// is in-block (`Some`); a triple only when BOTH predecessors are in-block. A
    /// `None` predecessor (the basic-block reset signal the caller passes at a
    /// boundary) suppresses the corresponding pair/triple — so no record straddles
    /// a boundary a fused superinstruction could not legally cross.
    #[cfg(feature = "decode-census")]
    #[test]
    fn census_record_counts_pairs_and_triples_only_within_block() {
        let mut c = DecodeCensus::default();
        // First op of a block: no predecessor → no pair, no triple, but a record.
        c.record(None, None, 10);
        // Second op: prev=10 in-block → a pair (10,11); no triple (prev2 None).
        c.record(None, Some(10), 11);
        // Third op: prev2=10, prev=11 both in-block → a pair AND a triple.
        c.record(Some(10), Some(11), 12);
        assert_eq!(c.total_records, 3, "every record bumps the denominator");
        assert_eq!(c.counts.get(&(CENSUS_NO_PREV, 10, 11)), Some(&1), "pair (10,11)");
        assert_eq!(c.counts.get(&(CENSUS_NO_PREV, 11, 12)), Some(&1), "pair (11,12)");
        assert_eq!(c.counts.get(&(10, 11, 12)), Some(&1), "triple (10,11,12)");
        // The FIRST op recorded no pair (prev was None — a block boundary).
        assert!(
            !c.counts.keys().any(|&(s, p, o)| s == CENSUS_NO_PREV && p == CENSUS_NO_PREV && o == 10),
            "the block's first op emits no pair (the reset suppressed it)"
        );
        // A boundary mid-stream: prev reset to None suppresses the pair across it.
        c.record(None, None, 13); // new block head after a reset
        assert!(
            !c.counts.contains_key(&(CENSUS_NO_PREV, 12, 13)),
            "no pair (12,13) was counted across the reset boundary"
        );
    }

    #[test]
    fn decode_jump_if_arg_supplied_widens_param_and_resolves_target() {
        // u16 + i16: param-index in `a`, resolved target record index in `b`.
        let mut c = Chunk::new();
        let site = c.emit_jump_if_arg_supplied(7, s(0, 1)); // rec 0
        c.emit(Op::Nil, s(1, 2)); // rec 1
        c.patch_jump(site); // forward target = rec 2 (Return)
        c.emit(Op::Return, s(2, 3)); // rec 2
        let d = decode_chunk(&c, &DecodeCfg::plain()).unwrap();
        assert!(matches!(d.records[0].op, DOp::Base(Op::JumpIfArgSupplied)));
        assert_eq!(d.records[0].a, 7, "param-index widened into a");
        assert_eq!(d.records[0].b, 2, "i16 jump (second word) pre-resolved into b");
    }

    // ── DECODE §5 (Unit B / Task 8): the peephole + fusion ──────────────────────

    /// A straight-line `GetLocal s; GetProp name` (no jump target on the GetProp)
    /// fuses to ONE `DOp::Fused(GetLocalGetProp)` record, packing slot|name<<16,
    /// keeping the FIRST component's off, and dropping a record.
    #[test]
    fn peephole_fuses_a_simple_pair() {
        let mut c = Chunk::new();
        let name = c.add_const(Value::Str("field".into()));
        c.emit_u16(Op::GetLocal, 3, s(0, 1)); // rec 0
        c.emit_u16(Op::GetProp, name, s(1, 2)); // rec 1 (swallowed)
        c.emit(Op::Return, s(2, 3)); // rec 2 → record 1 after fusion
        let unfused = decode_chunk(&c, &DecodeCfg::plain()).unwrap();
        assert_eq!(unfused.records.len(), 3, "1:1 keeps all three records");
        let d = decode_chunk(&c, &DecodeCfg::fused()).unwrap();
        assert_eq!(d.records.len(), 2, "the pair fused → one fewer record");
        assert!(matches!(d.records[0].op, DOp::Fused(FusedKind::GetLocalGetProp)));
        assert_eq!(d.records[0].a, 3 | ((name as u32) << 16), "slot|name<<16 packed");
        assert_eq!(d.records[0].off, 0, "fused off = first component's off");
        assert!(matches!(d.records[1].op, DOp::Base(Op::Return)));
        // The swallowed component's byte off is NOT an entry; the survivors are.
        assert_eq!(byte_to_record(&d, 0), Some(0), "head off is an entry");
        assert_eq!(byte_to_record(&d, 1), None, "swallowed GetProp off is no longer an entry");
    }

    /// Greedy LONGEST-match: `GetLocal; GetProp; Add` fuses to the TRIPLE
    /// (`GetLocalGetPropAdd`), not the `GetLocalGetProp` pair plus a stray `Add`.
    #[test]
    fn peephole_prefers_the_triple_over_the_pair() {
        let mut c = Chunk::new();
        let name = c.add_const(Value::Str("v".into()));
        c.emit_u16(Op::GetLocal, 1, s(0, 1));
        c.emit_u16(Op::GetProp, name, s(1, 2));
        c.emit(Op::Add, s(2, 3));
        c.emit(Op::Return, s(3, 4));
        let d = decode_chunk(&c, &DecodeCfg::fused()).unwrap();
        assert_eq!(d.records.len(), 2, "the 3-op run fused to one record");
        assert!(matches!(d.records[0].op, DOp::Fused(FusedKind::GetLocalGetPropAdd)));
        assert_eq!(d.records[0].a, 1 | ((name as u32) << 16));
    }

    /// §5.2 (the load-bearing legality rule): a record that is a JUMP TARGET must
    /// never be a fused MIDDLE. Build a loop whose back-edge targets a `GetProp`
    /// that WOULD otherwise fuse with the preceding `GetLocal`; assert the records
    /// around the target stay 1:1 and `entry_index` still contains the target's off.
    #[test]
    fn peephole_never_fuses_across_an_entry_point() {
        let mut c = Chunk::new();
        let name = c.add_const(Value::Str("f".into()));
        // Layout (byte offsets):
        //   0: GetLocal 0        (3 bytes) — would be the pair HEAD
        //   3: GetProp name      (3 bytes) — the loop back-edge TARGET
        //   6: Pop               (1 byte)
        //   7: Loop -> 3         (3 bytes) — back-edge to the GetProp at byte 3
        //  10: Return            (1 byte)
        c.emit_u16(Op::GetLocal, 0, s(0, 1)); // byte 0
        let target = c.code.len(); // byte 3 — the GetProp
        c.emit_u16(Op::GetProp, name, s(1, 2)); // byte 3
        c.emit(Op::Pop, s(2, 3)); // byte 6
        c.emit_loop(Op::Loop, target, s(3, 4)); // byte 7 → byte 3
        c.emit(Op::Return, s(4, 5)); // byte 10
        let d = decode_chunk(&c, &DecodeCfg::fused()).unwrap();
        // The GetProp is a jump target → it MUST stay a separate Base record (never
        // swallowed into a fused middle). So no `GetLocalGetProp` fusion happened.
        assert!(
            d.records.iter().all(|r| !matches!(r.op, DOp::Fused(_))),
            "no fusion may swallow the jump-target GetProp"
        );
        assert!(
            matches!(d.records[1].op, DOp::Base(Op::GetProp)),
            "the GetProp stays a standalone record"
        );
        // The target byte (3) is still a record boundary the driver can resync to.
        assert_eq!(byte_to_record(&d, 3), Some(1), "the jump target's off is a live entry");
    }

    /// §5.2: a `GetProp` that is itself a HEAD (first component) of a fused pair may
    /// be a jump target — only LATER components are forbidden from being targets.
    /// `GetProp name; Add` with a back-edge onto the GetProp must STILL fuse.
    #[test]
    fn peephole_fuses_when_the_jump_target_is_the_fused_head() {
        let mut c = Chunk::new();
        let name = c.add_const(Value::Str("f".into()));
        //   0: Nil               (1)
        //   1: GetProp name      (3) — back-edge TARGET, but the fused HEAD
        //   4: Add               (1)
        //   5: Loop -> 1         (3)
        //   8: Return            (1)
        c.emit(Op::Nil, s(0, 1)); // byte 0
        let target = c.code.len(); // byte 1
        c.emit_u16(Op::GetProp, name, s(1, 2)); // byte 1
        c.emit(Op::Add, s(2, 3)); // byte 4
        c.emit_loop(Op::Loop, target, s(3, 4)); // byte 5 → byte 1
        c.emit(Op::Return, s(4, 5)); // byte 8
        let d = decode_chunk(&c, &DecodeCfg::fused()).unwrap();
        // The GetProp is the fused HEAD (first component) — a legal jump target.
        assert!(
            matches!(d.records[1].op, DOp::Fused(FusedKind::GetPropAdd)),
            "GetProp;Add fuses even though GetProp is a jump target (it is the head)"
        );
        assert_eq!(byte_to_record(&d, 1), Some(1), "the head's off stays a live entry");
    }
}
