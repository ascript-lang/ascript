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

// DECODE Task 3: the decoder is built but not yet consumed (Task 4 wires the
// record-source driver into `run_loop_sync`). Every item below is exercised by
// this module's own unit tests but is dead from the production graph until then;
// this blanket allow is REMOVED in Task 4 when the driver consumes it.
#![allow(dead_code)]

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
/// (Unit B, Task 8) and inline (Unit C, Task 9) variants land later and exist
/// ONLY in the decoded stream — never in `Chunk.code`, never serialized,
/// invisible to the verifier and the disassembler.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DOp {
    /// A 1:1 decoded base instruction (operands widened into a/b).
    Base(Op),
}

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
    /// records were embedded by inlining. Empty until then.
    pub deps: Vec<(std::rc::Rc<crate::vm::chunk::FnProto>, u64)>,
    /// Task 9 fills: the inline-segment table for span/source attribution. Empty
    /// until then.
    pub inline_segments: Vec<InlineSegment>,
}

/// Task 9 (Unit C): a span of records embedded from a foreign (inlined) chunk.
/// Empty/unused in Task 3 — declared so the `DecodedChunk` shape Task 4/9 were
/// written against compiles.
pub(crate) struct InlineSegment {
    /// Record-index range `[start, end)` of the inlined body.
    pub start: u32,
    pub end: u32,
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
    /// Unit B (Task 8): peephole-fuse the census set. Off in Task 3.
    pub fuse: bool,
    /// Unit C (Task 9): inline small callees. Off in Task 3.
    pub inline: bool,
}

impl DecodeCfg {
    /// A 1:1 decode: no fusion, no inlining (Task 3).
    pub fn plain() -> Self {
        DecodeCfg { fuse: false, inline: false }
    }
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
pub(crate) fn decode_chunk(chunk: &Chunk, _cfg: &DecodeCfg) -> Option<std::rc::Rc<DecodedChunk>> {
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
    for (rec_idx, target_byte) in jump_fixups {
        if target_byte < 0 || target_byte > u32::MAX as i64 {
            return None;
        }
        let target_rec = *index_of.get(&(target_byte as u32))?;
        // The jump op's resolved record index lives in `a` for the i16 jumps and
        // in `b` for `JumpIfArgSupplied` (u16 + i16). `decode_operands` stashed
        // the raw displacement in the right field and recorded which via the op.
        let rec = &mut records[rec_idx];
        match rec.op {
            DOp::Base(Op::JumpIfArgSupplied) => rec.b = target_rec,
            DOp::Base(_) => rec.a = target_rec,
        }
    }

    // ---- entry_index: sorted (byte_off, record_idx) for every record. -------
    // Emission is monotonic so `records` is already off-ascending; build the
    // pairs directly.
    let entry_index: Vec<(u32, u32)> =
        records.iter().enumerate().map(|(i, r)| (r.off, i as u32)).collect();

    // own_epoch read AFTER pass 1 (single-threaded: no patch can interleave, but
    // read-late is the conservative order — spec §2.2).
    let own_epoch = chunk.patch_epoch.get();

    Some(std::rc::Rc::new(DecodedChunk {
        records,
        entry_index,
        own_epoch,
        deps: Vec::new(),
        inline_segments: Vec::new(),
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
}
