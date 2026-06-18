//! The VM's async run loop (`Vm::run`).
//!
//! V2 implements the **synchronous core**: constants, literal pushes, stack
//! `Pop`/`Dup`, locals/globals, calls, templates, the full binary/unary operators
//! (string concat / decimal / range / cross-type equality / numeric) and `Return`.
//! Every other opcode is a documented `not yet implemented` Tier-2 panic that
//! later VM slices fill in. Panics carry the faulting instruction's [`Span`] so
//! ariadne points at the source exactly like the tree-walker.
//!
//! The binary/unary arms call the SAME `apply_binop`/`apply_unop` free functions
//! the tree-walker uses (`src/interp.rs`), so the two engines cannot drift on
//! arithmetic semantics or panic messages — there is one implementation.

use crate::ast::{BinOp, UnOp};
use crate::error::AsError;
use crate::interp::{error_message, Control, Interp};
use crate::span::Span;
use crate::value::Value;
use crate::value::{OwnedKind, ValueKind};
use crate::vm::fiber::Fiber;
// NANB Task 1.3: rebuild an owned `Value` from a consumed `OwnedKind` (the inverse
// of `Value::into_kind`). Each handle moves straight back through its total
// constructor — zero clones, no refcount change. Used by the consuming dispatch
// matches whose fallback arm must hand the whole value to the shared `Interp`
// dispatch (which takes `Value` by value).
#[inline]
#[allow(dead_code)]
fn rebuild_value(k: OwnedKind) -> Value {
    match k {
        OwnedKind::Nil => Value::nil(),
        OwnedKind::Bool(b) => Value::bool_(b),
        OwnedKind::Int(i) => Value::int(i),
        OwnedKind::Float(f) => Value::float(f),
        OwnedKind::Decimal(d) => Value::decimal_rc(d),
        OwnedKind::Str(s) => Value::str(s),
        OwnedKind::Builtin(s) => Value::builtin(s),
        OwnedKind::Function(f) => Value::function(f),
        OwnedKind::Closure(c) => Value::closure(c),
        OwnedKind::Array(a) => Value::array_cell(a),
        OwnedKind::Object(o) => Value::object_cell(o),
        OwnedKind::Map(m) => Value::map_cell(m),
        OwnedKind::Set(s) => Value::set_cell(s),
        OwnedKind::Bytes(b) => Value::bytes_rc(b),
        #[cfg(feature = "data")]
        OwnedKind::Regex(r) => Value::regex(r),
        OwnedKind::Native(n) => Value::native(n),
        OwnedKind::NativeMethod(m) => Value::native_method(m),
        OwnedKind::Enum(e) => Value::enum_(e),
        OwnedKind::EnumVariant(v) => Value::enum_variant(v),
        OwnedKind::Class(c) => Value::class(c),
        OwnedKind::Interface(i) => Value::interface(i),
        OwnedKind::Instance(i) => Value::instance(i),
        OwnedKind::BoundMethod(b) => Value::bound_method(b),
        OwnedKind::Super(s) => Value::super_(s),
        OwnedKind::Future(f) => Value::future(f),
        OwnedKind::Generator(g) => Value::generator(g),
        OwnedKind::GeneratorMethod(g) => Value::generator_method(g),
        OwnedKind::ClassMethod(c) => Value::class_method(c),
        OwnedKind::Shared(s) => Value::shared(s),
    }
}
use crate::vm::opcode::Op;
use crate::vm::value_ext::{Closure, RunOutcome};
use gcmodule::Cc;
use rustc_hash::{FxBuildHasher, FxHashMap};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// A module's export collector (V12-T4): an insertion-ordered name→value map behind
/// shared interior mutability, so `Op::DefineExport` records into it and an importer
/// reads it back (the namespace form clones it into a `Value::object_cell`).
type ModuleExports = Rc<RefCell<indexmap::IndexMap<String, Value>>>;

/// A module-scope user-global slot: its current value plus its REASSIGNABILITY,
/// mirroring the tree-walker's per-binding `Environment` mutability flag. A `let`
/// (or `param`, never a global) is mutable; `const`/`fn`/`class`/`enum`/`import` are
/// immutable. `Op::SetGlobal` consults `mutable` at RUNTIME so an immutable global
/// reassigned from a LATER, separately-compiled chunk (REPL line-to-line, or a main
/// module reassigning an import) errors `cannot assign to immutable binding` exactly
/// like the tree-walker — the compile-time `Op::ImmutableError` only sees same-chunk
/// assignments. The value is a plain owned `Value` (the `Vm` is the GC root, so it
/// stays reachable — NO `Cc` cell, preserving the deterministic native-Drop gate).
struct GlobalSlot {
    value: Value,
    mutable: bool,
}

/// DBG: the outcome of one `debug_stop` park (the parked command loop). A resume command
/// (`Continue`/`Next`/`StepIn`/`StepOut`) records the step mode on the hook and yields
/// [`StopOutcome::Resume`] (the trap arm un-patches + resumes). An `Evaluate` command is
/// NOT a resume — `debug_stop` returns [`StopOutcome::Evaluate`] (with the instrument box
/// already restored, no borrow held) so the async trap arm can run the re-entrant
/// evaluator on the tree-walker and then re-park. Plain owned data only.
enum StopOutcome {
    /// Resume execution (the step mode was already recorded on the hook).
    Resume,
    /// Evaluate `expr` in paused frame `frame_id`, ship the result, and re-park.
    Evaluate { expr: String, frame_id: usize },
}

/// LANE §2.2: outcome of one synchronous dispatch burst in `run_loop_sync`.
// `Finished` is not yet constructed in Task 4 (Return/Yield not in the Task-4
// subset); it becomes live in Task 5.
#[allow(dead_code)]
pub(crate) enum SyncOutcome {
    /// The fiber finished normally: root frame returned or a generator hit `Op::Yield`.
    Finished(RunOutcome),
    /// The burst stopped at an op outside the sync subset. The fiber's `ip` still
    /// points AT the escalating opcode byte — the async driver re-decodes it.
    NeedsAsync,
}

/// DECODE §2.4/§3.1: how a `sync_burst<S>` returns. The byte source always
/// produces `Sync`; the record source produces `FellBack` when it can no longer
/// fetch from records (a frame transition into an un-decoded/stale callee), at
/// which point `run_loop_sync` continues the same burst on the byte source — the
/// canonical ip is exact, so the byte burst resumes exactly where records stopped.
pub(crate) enum BurstExit {
    /// The burst reached a normal sync-driver exit edge (finished or escalation).
    Sync(SyncOutcome),
    /// The record source gave up mid-burst; fall back to byte dispatch (record
    /// source only — `ByteSource::fetch` never returns `None`).
    FellBack,
}

/// DECODE §2.4: one fetched instruction — the op plus the byte offsets the
/// verbatim arm bodies read operands from (`operand_at`) and anchor spans/panics
/// at (`fault_ip`). For both sources `operand_at == fault_ip + 1`.
///
/// DECODE §5 (Unit B): `fused` is `Some((kind, packed_a))` when the record source
/// fetched a FUSED superinstruction — `kind` names the op sequence and `packed_a`
/// is the record's `a` field (the two component u16 operands, low/high). `op` is
/// then the FIRST component's op (for the subset/canonical-ip prologue), and
/// `sync_burst` dispatches the whole fused run in one dedicated block (advancing
/// the canonical ip past ALL components). The byte source never fuses
/// (`fused == None` always).
struct Fetched {
    op: Op,
    fault_ip: usize,
    operand_at: usize,
    fused: Option<(crate::vm::decode::FusedKind, u32)>,
}

/// DECODE §2.4: the per-burst instruction source. Two monomorphized impls — the
/// [`ByteSource`] (LANE's shipped behavior: decode `code[ip]`, walk by
/// `operand_width`) and the [`RecordSource`] (read `records[idx]`). The shared
/// `sync_burst<S>` arm bodies are THE single source of truth for sync-subset
/// semantics; only fetch/advance mechanics differ per source.
///
/// CANONICAL IP INVARIANT (§3): `fiber.frame().ip` is the byte ip at all times.
/// Both sources read/write it identically; the record source additionally tracks
/// a burst-local record cursor that it resyncs from the canonical ip whenever the
/// ip moved non-sequentially (a taken jump or an escalation restore) or the active
/// frame changed (a push/pop) — so the arm bodies need ZERO source-aware edits.
trait InstrSource {
    /// Fetch the next instruction WITHOUT advancing the cursor (so the subset
    /// check can escalate with the ip un-advanced). Refreshes `last_fault_source`
    /// as appropriate. `None` ⇒ the record source can no longer fetch (frame
    /// transition into an un-decoded/stale callee) — the burst falls back to byte
    /// dispatch (`ByteSource` never returns `None`).
    fn fetch(&mut self, vm: &Vm, fiber: &mut Fiber) -> Option<Fetched>;

    /// Advance the cursor past `op` (whose operands begin at `operand_at`): write
    /// the canonical byte ip forward and, for the record source, step `idx`.
    fn advance(&mut self, fiber: &mut Fiber, op: Op, operand_at: usize);

    /// DECODE §8.3 coverage: a record fully retired. The record source bumps
    /// `decoded_ops`/`stack_ops`; the byte source is a no-op.
    fn note_retired(&mut self, vm: &Vm, op: Op);
}

/// DECODE §2.4: the byte instruction source — LANE's shipped behavior, made a
/// monomorphization of the generic burst. Carries no state: the canonical
/// `fiber.frame().ip` IS its cursor.
struct ByteSource;

impl InstrSource for ByteSource {
    #[inline]
    fn fetch(&mut self, vm: &Vm, fiber: &mut Fiber) -> Option<Fetched> {
        // Mirror run_loop exactly: capture fault_ip first, refresh last_fault_source
        // per instruction, decode the opcode byte.
        let fault_ip = fiber.frame().ip;
        if let Some(src) = fiber.frame().closure.proto.chunk.source.borrow().as_ref() {
            *vm.last_fault_source.borrow_mut() = Some(src.clone());
        }
        let byte = fiber.frame().closure.proto.chunk.code[fault_ip];
        let op = Op::from_u8(byte)
            .unwrap_or_else(|| panic!("invalid opcode byte {byte:#x} at ip {fault_ip}"));
        Some(Fetched { op, fault_ip, operand_at: fault_ip + 1, fused: None })
    }

    #[inline]
    fn advance(&mut self, fiber: &mut Fiber, op: Op, operand_at: usize) {
        fiber.frame_mut().ip = operand_at + op.operand_width();
    }

    #[inline]
    fn note_retired(&mut self, _vm: &Vm, _op: Op) {}
}

/// DECODE §2.4: the record instruction source — fetches from a valid
/// [`DecodedChunk`](crate::vm::decode::DecodedChunk), skipping the per-instruction
/// `Op::from_u8` decode + `operand_width` walk. The arm bodies still read operands
/// from `chunk.read_*(operand_at)` (1:1 decode keeps the bytes the source of truth
/// for operand VALUES — fusion/inline that consult the widened `a`/`b` land in
/// Tasks 8/9); the record source's win here is the cursor: a hot straight-line run
/// advances `idx += 1` instead of decoding a byte.
struct RecordSource {
    /// The decoded chunk of the frame `idx` indexes (resynced on a frame change).
    d: std::rc::Rc<crate::vm::decode::DecodedChunk>,
    /// Record cursor into `d.records` — the NEXT record to fetch.
    idx: u32,
    /// The `fiber.frames.len()` this `(d, idx)` is valid for. A change means a
    /// frame push/pop happened and the cursor must re-derive from the new frame.
    frames_len: usize,
    /// Identity of the chunk `d` decodes (`Rc::as_ptr`), to detect a frame change
    /// even when `frames.len()` coincidentally matches (a pop-then-push could).
    chunk_ptr: *const crate::vm::chunk::Chunk,
    /// **DECODE §5.1 census (feature-gated): the burst-local predecessor op.** The
    /// op discriminant of the IMMEDIATELY PRECEDING record retired IN THE SAME basic
    /// block, or `None` at a block boundary. Reset to `None` whenever `fetch` detects
    /// a discontinuity (a taken jump or a frame push/pop → `resync`) and at burst
    /// entry (a fresh `RecordSource` per burst → escalations/fallbacks reset too), so
    /// a pair/triple is NEVER counted across a boundary a fused superinstruction could
    /// not legally cross. FULLY `#[cfg(feature = "decode-census")]`.
    #[cfg(feature = "decode-census")]
    prev: Option<u16>,
    /// **DECODE §5.1 census (feature-gated): the second burst-local predecessor.**
    /// The op two records back, in-block. `Some` only when BOTH it and `prev` are
    /// in the same basic block — reset to `None` alongside `prev` at every boundary.
    #[cfg(feature = "decode-census")]
    prev2: Option<u16>,
}

impl RecordSource {
    /// DECODE §3.1 (entry: byte → record): position a record source at the entry
    /// frame's current byte ip. `None` if the ip is not a record boundary (a stale
    /// stream or a foreign ip) — the caller then runs on bytes.
    fn at_entry(
        vm: &Vm,
        fiber: &Fiber,
        d: std::rc::Rc<crate::vm::decode::DecodedChunk>,
    ) -> Option<Self> {
        let chunk = &fiber.frame().closure.proto.chunk;
        let idx = crate::vm::decode::byte_to_record(&d, fiber.frame().ip as u32)?;
        // SP4 §3 (hoisted per §2.4): refresh last_fault_source at frame entry —
        // the cell's value is the per-chunk constant (the chunk's module source),
        // so refreshing where the chunk changes is observationally identical to
        // the byte path's per-instruction refresh.
        if let Some(src) = chunk.source.borrow().as_ref() {
            *vm.last_fault_source.borrow_mut() = Some(src.clone());
        }
        Some(RecordSource {
            d,
            idx,
            frames_len: fiber.frames.len(),
            chunk_ptr: chunk as *const crate::vm::chunk::Chunk,
            // DECODE §5.1: a fresh source begins a basic block (the entry point is a
            // boundary) — no in-block predecessor yet.
            #[cfg(feature = "decode-census")]
            prev: None,
            #[cfg(feature = "decode-census")]
            prev2: None,
        })
    }

    /// DECODE §3.1: re-derive `(d, idx)` from the CURRENT frame's canonical byte
    /// ip. Called when the cursor detects a discontinuity (a frame change, or the
    /// ip moved off the sequential record). `false` ⇒ cannot fetch from records
    /// (the new frame has no valid decoded stream, or the ip is not a record
    /// boundary) — the burst falls back to byte dispatch.
    fn resync(&mut self, vm: &Vm, fiber: &Fiber) -> bool {
        // DECODE §5.1 census: a resync is the load-bearing BASIC-BLOCK BOUNDARY
        // signal — `fetch` calls it ONLY on a discontinuity (a taken jump, or a
        // frame push/pop). Reset the in-block predecessors HERE (before either
        // branch) so the next retired op opens a fresh basic block and no pair/triple
        // is counted across the boundary. Whether the resync ultimately succeeds or
        // falls back, the predecessors must NOT carry over the jump/frame edge.
        #[cfg(feature = "decode-census")]
        {
            self.prev = None;
            self.prev2 = None;
        }
        let chunk = &fiber.frame().closure.proto.chunk;
        let cur_ptr = chunk as *const crate::vm::chunk::Chunk;
        if cur_ptr != self.chunk_ptr {
            // Frame changed to a different chunk: consult ITS decoded stream.
            let slot = chunk.decoded.borrow();
            let d = match slot.as_ref() {
                // §4.2 validity (own_epoch + deps) — the single SoT in `is_valid`.
                Some(d) if d.is_valid(chunk) => d.clone(),
                _ => return false, // un-decoded / stale callee → byte fallback
            };
            drop(slot);
            let idx = match crate::vm::decode::byte_to_record(&d, fiber.frame().ip as u32) {
                Some(i) => i,
                None => return false,
            };
            self.d = d;
            self.chunk_ptr = cur_ptr;
            self.idx = idx;
            self.frames_len = fiber.frames.len();
            // Hoisted last_fault_source refresh at the chunk boundary (§2.4).
            if let Some(src) = chunk.source.borrow().as_ref() {
                *vm.last_fault_source.borrow_mut() = Some(src.clone());
            }
            true
        } else {
            // Same chunk, but the ip moved off the sequential record (a taken jump
            // within the frame). Re-derive idx; the stream identity is unchanged.
            match crate::vm::decode::byte_to_record(&self.d, fiber.frame().ip as u32) {
                Some(i) => {
                    self.idx = i;
                    self.frames_len = fiber.frames.len();
                    true
                }
                None => false,
            }
        }
    }
}

impl InstrSource for RecordSource {
    #[inline]
    fn fetch(&mut self, vm: &Vm, fiber: &mut Fiber) -> Option<Fetched> {
        let frame_ip = fiber.frame().ip as u32;
        // Fast path: same frame, same chunk, and `idx` still points at the record
        // whose off equals the canonical ip (sequential fall-through). Otherwise a
        // discontinuity (jump / frame push-pop) happened → resync.
        let need_resync = fiber.frames.len() != self.frames_len
            || (self.idx as usize) >= self.d.records.len()
            || self.d.records[self.idx as usize].off != frame_ip;
        if need_resync && !self.resync(vm, fiber) {
            return None;
        }
        let rec = self.d.records[self.idx as usize];
        let (op, fused) = match rec.op {
            crate::vm::decode::DOp::Base(op) => (op, None),
            // DECODE §5 (Unit B): a fused record. `op` is the FIRST component (the
            // subset check + the canonical-ip prologue see the head op); the whole
            // run is dispatched + the ip advanced past ALL components by the fused
            // block in `sync_burst`.
            crate::vm::decode::DOp::Fused(kind) => {
                (crate::vm::decode::FusedKind::components(kind)[0], Some((kind, rec.a)))
            }
        };
        Some(Fetched {
            op,
            fault_ip: rec.off as usize,
            operand_at: rec.off as usize + 1,
            fused,
        })
    }

    #[inline]
    fn advance(&mut self, fiber: &mut Fiber, op: Op, operand_at: usize) {
        // Canonical ip forward, exactly like the byte source — for a fused record
        // `sync_burst` overrides this with the full multi-component width before
        // dispatching (so the head op's width here is harmless: it is recomputed).
        fiber.frame_mut().ip = operand_at + op.operand_width();
        // Step the record cursor to the next record (sequential). A non-sequential
        // continuation (jump / frame change) is caught + resynced at the next fetch.
        self.idx += 1;
    }

    #[inline]
    fn note_retired(&mut self, vm: &Vm, op: Op) {
        #[cfg(any(test, feature = "fuzzgen", fuzzing))]
        vm.bump_decode_stat(|s| {
            s.decoded_ops = s.decoded_ops.saturating_add(1);
            // §7.3 gate input: fiber-stack pushes+pops this record retired. A
            // well-defined per-op magnitude (the operand-stack traffic), so it
            // EXISTS from the first record; Unit B/D measure its reduction.
            s.stack_ops = s.stack_ops.saturating_add(op_stack_traffic(op));
        });
        #[cfg(not(any(test, feature = "fuzzgen", fuzzing)))]
        {
            let _ = (vm, op);
        }
        // DECODE §5.1 census (feature-gated): record this retired op against the
        // in-block predecessors, then advance the burst-local window. The reset of
        // `prev`/`prev2` at jump/frame boundaries happens in `fetch`→`resync`; HERE we
        // additionally treat a CONTROL-FLOW op as a block TERMINATOR (a conditional
        // jump ends a basic block even on the not-taken fall-through, where `fetch`
        // would NOT resync) — so the op AFTER a terminator opens a fresh block and no
        // pair/triple straddles a boundary a fused superinstruction could not cross.
        #[cfg(feature = "decode-census")]
        {
            vm.census_record(self.prev2, self.prev, op as u8 as u16);
            if op_is_block_terminator(op) {
                // This op ends the block — the next record begins a new block.
                self.prev = None;
                self.prev2 = None;
            } else {
                // Shift the window: prev2 ← prev, prev ← this op.
                self.prev2 = self.prev;
                self.prev = Some(op as u8 as u16);
            }
        }
    }
}

/// DECODE §7.3: the operand-stack traffic (pushes + pops) of one `op`, the
/// `stack_ops` gate input. A static per-op magnitude — the exact count for the
/// common ops, a conservative lower bound for the rare variadic builders (whose
/// width is a runtime operand). Used ONLY for the coverage counter (never on the
/// production hot path).
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
fn op_stack_traffic(op: Op) -> u64 {
    match op {
        // pure pushes (no pop)
        Op::Const | Op::Nil | Op::True | Op::False | Op::GetLocal | Op::GetGlobal
        | Op::Closure => 1,
        // pop-only
        Op::Pop | Op::SetLocal | Op::SetGlobal => 1,
        // dup: 1 read + 1 push
        Op::Dup => 2,
        // binary: 2 pops + 1 push
        Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Mod | Op::Pow | Op::Lt | Op::Le
        | Op::Gt | Op::Ge | Op::Eq | Op::Ne | Op::InstanceOf | Op::InstanceOfType
        | Op::BitAnd | Op::BitOr | Op::BitXor | Op::Shl | Op::Shr | Op::WrapAdd
        | Op::WrapSub | Op::WrapMul | Op::Range | Op::GetIndex => 3,
        // unary: 1 pop + 1 push
        Op::Neg | Op::Not | Op::BitNot => 2,
        // conditional jumps pop their test
        Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNotNil => 1,
        // everything else: a conservative 1 (it touched the stack at least once,
        // or — for pure control flow like Op::Jump/Loop — count it as a unit so
        // the gate input is monotone with record count).
        _ => 1,
    }
}

/// DECODE §5.1 census (feature-gated): `true` iff `op` ENDS a basic block — the
/// op AFTER it begins a fresh block, so a fused superinstruction must not span the
/// boundary. The set is the union of:
/// - the control-flow ops (every jump/branch/loop + the arg-supplied prologue
///   jump): a conditional branch ends a block on BOTH edges, including the
///   not-taken fall-through where `fetch` would NOT resync, so the reset MUST be
///   driven here;
/// - the control-LEAVING ops (`Return`/`Propagate`/`Yield`/`Unwrap`/`MatchNoArm`):
///   they transfer control out of the current straight-line region;
/// - the CALL family + suspension points (`Await`/`IterNext`/`Break`): they hand
///   control to a callee frame / the reactor / the debugger; the next in-lane op
///   begins a new region.
///
/// Conservative by construction: over-marking an op as a terminator only SUPPRESSES
/// a pair/triple (never invents one across a real boundary), which is the safe
/// direction — Task 8 must only ever fuse pairs proven legal.
#[cfg(feature = "decode-census")]
fn op_is_block_terminator(op: Op) -> bool {
    matches!(
        op,
        // control flow
        Op::Jump
            | Op::JumpIfFalse
            | Op::JumpIfTrue
            | Op::JumpIfNotNil
            | Op::Loop
            | Op::JumpIfArgSupplied
            // control-leaving
            | Op::Return
            | Op::Propagate
            | Op::Yield
            | Op::Unwrap
            | Op::MatchNoArm
            // calls + suspension points (frame/reactor/debugger transfer)
            | Op::Call
            | Op::CallElided
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

/// **CALL §8.3 — per-VM fast-path coverage counters.**
///
/// Bumped (via [`Vm::bump_stat`]) when a call-path fast path actually fires.
/// All four counters start at 0 and stay 0 until the corresponding fast path
/// is wired in Phase 2/3; Phase 1 ships the struct and accessor so the test
/// scaffolding can compile.  Asserted `>0` over the functional corpus in
/// `tests/call_fast.rs` as an anti-false-green gate.
#[derive(Clone, Copy, Default, Debug)]
pub struct CallFastStats {
    /// A1+A2: calls that used in-place operand-stack argument binding (no Vec).
    pub inplace_binds: u64,
    /// A3: re-entrant `call_value` / method-dispatch calls that reused a pooled fiber.
    pub pooled_fiber_reuses: u64,
    /// B: callback elements dispatched through the `CallbackTrampoline` sync lane.
    pub trampoline_calls: u64,
    /// B: trampoline elements that escalated to the async driver (callback suspended).
    pub trampoline_escalations: u64,
}

/// **SHAPE §3.5 — per-VM storage-mode coverage counters (anti-false-green Gate 15).**
///
/// Compiled only under `#[cfg(any(test, feature = "fuzzgen", fuzzing))]` so
/// production builds carry zero overhead.  `cargo test` enables the `fuzzgen`
/// feature (via the self-dev-dependency), so the counters are live in every test
/// run.  Asserted `> 0` over the functional corpus in `tests/vm_differential.rs`
/// as Gate 15.
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
#[derive(Clone, Copy, Default, Debug)]
pub struct ShapeStats {
    /// Every fresh `ObjectStorage::Slab` construction — `Op::NewObject` warm-hit
    /// path, `new_object_generic` slab arm, and instance construction.
    pub obj_slab_constructed: u64,
    /// Every fresh `ObjectStorage::Dict` construction on the VM — the
    /// `new_object_generic` cap-refused arm, the `Op::ObjectRest` rest-collector
    /// (both lanes), the namespace-import build, and each slab→dict demotion (a
    /// demotion materializes a fresh dict). Covers every `ObjectCell::new(map)` site.
    pub obj_dict_constructed: u64,
    /// Every `demote_to_dict()` call (both object and instance insert paths).
    pub obj_demotions: u64,
}

/// **DECODE — per-VM stat counters (anti-false-green, DECODE §8.3).**
///
/// Compiled only under `#[cfg(any(test, feature = "fuzzgen", fuzzing))]` so
/// the production build carries zero overhead. Wired counters:
/// - RecordSource (Unit A): `decoded_ops`, `decoded_bytes`, `stack_ops`
/// - Unit B fusion:          `fused_ops`
/// - `inline_hits`/`inline_misses` (Unit C) and `tos_ops` (Unit D) stay 0 —
///   those units were EVIDENCE-DROPPED, so the counters are permanently inert.
///
/// The public wrapper is [`DecodeStats`] in `lib.rs`, which bundles the counters
/// together with the program output for the test entry points.
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
#[derive(Clone, Copy, Default, Debug)]
pub struct DecodeStatsInner {
    /// Total records retired by the `RecordSource` driver across all bursts.
    pub decoded_ops: u64,
    /// Fused superinstruction records retired (Unit B).
    pub fused_ops: u64,
    /// INERT (Unit C evidence-dropped) — would have counted inline guard hits.
    pub inline_hits: u64,
    /// INERT (Unit C evidence-dropped) — would have counted inline guard misses.
    pub inline_misses: u64,
    /// Total bytes occupied by decoded record streams at the end of the run
    /// (memory accounting gate input).
    pub decoded_bytes: u64,
    /// Fiber-stack push + pop operations retired by the record driver — the
    /// §7.3 stack-traffic gate input (Unit B sees the post-fusion reduction).
    pub stack_ops: u64,
    /// INERT (Unit D evidence-dropped) — would have counted TOS-cached records.
    pub tos_ops: u64,
}

/// The bytecode virtual machine.
///
/// Holds the shared [`Interp`] (the runtime state the VM and tree-walker share)
/// and a self-`Weak` mirroring [`Interp`]'s pattern, so a `&self` method can
/// recover an owned `Rc<Vm>` to hand to a spawned task in V7.
pub struct Vm {
    interp: Rc<Interp>,
    self_weak: RefCell<Weak<Vm>>,
    /// Per-class compiled-method table (V9). `value.rs`'s `Class`/`Method` is
    /// frozen and holds a TREE-WALKER body the VM cannot run, so the VM compiles
    /// each method to a `Value::closure` and stores it HERE instead — keyed by the
    /// class's `Rc` IDENTITY (`Rc::as_ptr` address) → method name → compiled
    /// closure. A class's `Value::class.methods` map is left empty; method dispatch
    /// goes through this table (`compiled_method`). The key is stable because the
    /// `Rc<Class>` is created once at compile time and shared by every instance.
    // SHAPE §6.1: Fx — bounded inflow (class-identity pointers / source identifiers),
    // never attacker-scaled, iteration order never observed (see audit) — this table is
    // accessed only via get/insert/contains_key, never iterated into output.
    class_methods: RefCell<FxHashMap<usize, FxHashMap<String, Cc<Closure>>>>,
    /// Per-class STATIC method table (SP1 §3): class `Rc` identity → static name →
    /// compiled closure. A SEPARATE namespace from `class_methods`; a static is
    /// called as `C.name(args)` with NO receiver (a plain `Value::closure` call),
    /// resolved up the superclass chain by `find_compiled_static_method`.
    // SHAPE §6.1: Fx — bounded inflow (class-identity pointers / source identifiers),
    // never attacker-scaled, iteration order never observed (see audit) — get/insert only.
    class_static_methods: RefCell<FxHashMap<usize, FxHashMap<String, Cc<Closure>>>>,
    /// Per-class field-default thunk table (V9): class `Rc` identity → field name →
    /// a zero-arg closure that produces the field's default value. Run once per
    /// constructed instance (so a mutable default yields a fresh value each time,
    /// matching the tree-walker's per-construct default eval).
    // SHAPE §6.1: Fx — bounded inflow (class-identity pointers / source identifiers),
    // never attacker-scaled, iteration order never observed (see audit). The inner map is
    // consulted only via `contains_key`/`get`; the order it is walked in comes from
    // `Class.fields.keys()` (declared schema order), NOT from this table's hash order.
    class_defaults: RefCell<FxHashMap<usize, FxHashMap<String, Cc<Closure>>>>,
    /// Per-VM hidden-class registry (V11-T2). Assigns a `shape_id` to every
    /// object/instance key-LAYOUT via a transition tree; V11-T3 inline caches key
    /// on these ids. Only VM code paths touch it (the tree-walker leaves shapes 0).
    shapes: RefCell<crate::vm::shape::ShapeRegistry>,
    /// A shared `def_env` for every VM-created class (task #157). The compiler
    /// leaves `Class.def_env` as an inert `global_env()` placeholder because the VM
    /// has no tree-walker Environment; but the SHARED `Interp::validate_into`
    /// (powering `ClassName.from` / typed-parse) resolves a NESTED-class field-type
    /// name and a default-expr name via `def_class.def_env.get(name)`. So `Op::Class`
    /// (a) rebuilds the class with `def_env` set to this env, and (b) registers the
    /// new class into it. The env is a single CHILD of `global_env()` shared by all
    /// classes — mirroring the tree-walker, where every top-level class's `def_env`
    /// is the SAME module `env` (so siblings/forward refs resolve, late-bound). The
    /// init is deferred to first use (built lazily) so a VM that never declares a
    /// class allocates nothing.
    class_env: RefCell<Option<crate::env::Environment>>,
    /// **The `--no-specialize` KILL SWITCH (V11-T5).** When `true` (the default),
    /// every specialization fast path is active: the polymorphic field/method
    /// inline caches (`GET_PROP`/`SET_PROP`/`CALL_METHOD`) and the PEP-659 adaptive
    /// arithmetic + `GET_GLOBAL` caches are consulted and recorded in front of the
    /// generic path. When `false`, ALL of those fast paths are skipped — every
    /// property read/write, method dispatch, arithmetic op, and global resolve goes
    /// straight through the generic lookup with NO IC/adaptive consult or record.
    ///
    /// The two modes MUST produce byte-identical results (both correct); the only
    /// difference is speed. The three-way differential in `tests/vm_differential.rs`
    /// asserts `generic-VM == specialized-VM == tree-walker` over the whole corpus,
    /// so any IC/adaptive guard bug makes generic and specialized diverge instantly.
    specialize: bool,
    /// The CURRENT module's export collector (V12-T4). `Op::DefineExport` records
    /// each `export`ed top-level binding here. While running an imported file module
    /// (`Vm::run_file_module`), this points at THAT module's fresh exports map; while
    /// running the entry program it points at a throwaway map (a main program's
    /// exports are unused, mirroring the tree-walker). Swapped on a stack-discipline
    /// basis around a nested module run so transitive imports collect into the right
    /// module. Insertion-ordered so a namespace import reflects declaration order.
    module_exports: RefCell<ModuleExports>,
    /// Cache of already-loaded FILE modules (V12-T4), keyed by canonical path →
    /// the module's exports map. Mirrors the tree-walker's `Interp::modules` cache:
    /// a module's top-level runs at most once; repeated `import`s reuse the cached
    /// exports. Inserted BEFORE the module body runs so a circular import resolves to
    /// the (then partially-populated) in-progress entry instead of re-running.
    file_modules: RefCell<HashMap<std::path::PathBuf, ModuleExports>>,
    /// The directory of the module currently executing (V12-T4), used to resolve a
    /// relative file import (`from "./mod"`). Mirrors `Interp::module_dir`. Swapped
    /// around a nested module run and restored after.
    module_dir: RefCell<std::path::PathBuf>,
    /// **SELF-CONTAINED-BUNDLES Phase 1.** An optional in-memory module archive. `None`
    /// is the default and the production disk path: `load_file_module` resolves every
    /// relative import on disk exactly as before (byte-identical). When `Some` (a `run`
    /// of a `.aso` archive / native bundle, installed via [`set_module_archive`]), each
    /// relative import is FIRST looked up in the archive by its machine-independent
    /// LOGICAL KEY (`join_logical(module_logical_dir, source)`) — a hit runs the embedded
    /// verified chunk through the SAME `from_bytes_verified` trust boundary as the disk
    /// path, with NO source tree on disk; a miss falls through to the unchanged disk path.
    module_archive: RefCell<Option<Rc<crate::vm::archive::ModuleArchive>>>,
    /// **SELF-CONTAINED-BUNDLES Phase 1.** The CURRENT module's LOGICAL directory — its
    /// archive-relative, `/`-separated directory (the entry's is `""`). Parallel to
    /// [`module_dir`](Self::module_dir) and swapped IN LOCKSTEP with it around a nested
    /// module run. It is the resolution base for an archive lookup: an import `S` from a
    /// module whose logical dir is `D` keys at `join_logical(D, S)`. Inert overhead when
    /// no archive is installed (a single string swap), so the default path stays
    /// byte-identical. The two must compute the SAME key `compile_archive` stored —
    /// guaranteed by sharing `crate::vm::archive::join_logical`.
    module_logical_dir: RefCell<String>,
    /// MODULE-SCOPE USER-GLOBALS: every DIRECT-child top-level binding of the entry
    /// program (`let`/`const`/`fn`/`class`/`enum`/`import`) is a late-bound global
    /// stored here by name, NOT a SourceFile-frame slot-local. `Op::DefineGlobal`
    /// inserts, `Op::SetGlobal` updates, and `Op::GetGlobal` consults this table
    /// BEFORE the bare builtins — so a function/thunk body that references a top-level
    /// binding declared LATER resolves at run time, matching the tree-walker's single
    /// shared module `Environment`. Plain owned `Value`s (the `Vm` is the GC root, so
    /// they stay reachable) in insertion (declaration) order. This table is ALSO the
    /// REPL's cross-line persistence: one `Vm` kept alive across lines carries its
    /// globals forward. (A file module's exports use the separate `module_exports`
    /// path; only the entry chunk defines into this table.)
    // SHAPE §6.1: Fx — bounded inflow (source identifiers), never attacker-scaled. This
    // table IS iterated (def-env rebuild at ~1107/~6657) and index-cached
    // (`GlobalCache::IndexBound`), but `IndexMap` iteration is INSERTION-ordered and its
    // indices are STABLE regardless of hasher — so Fx changes neither the observable order
    // nor cache validity. The two iteration sites only build a flat (order-independent)
    // binding env; no hash-order leaks to output.
    user_globals: RefCell<indexmap::IndexMap<Rc<str>, GlobalSlot, FxBuildHasher>>,
    /// Monotonic version counter, bumped on every global (re)definition or
    /// assignment. The V11-T4 GET_GLOBAL inline cache (`adapt::GlobalCache`) guards
    /// its cached value with this version: a cache entry recorded at version V is
    /// valid only while the version is still V, so any global write invalidates it.
    /// Top-level defines run once at load, then the version is stable, so the caches
    /// stay hot for the steady-state hot loops.
    global_version: std::cell::Cell<u64>,
    /// STRUCTURAL generation (SP8). Bumped ONLY when a NEW global is DEFINED/inserted
    /// (`define_user_global`), NEVER on a plain reassignment (`update_user_global`).
    /// The SP8 index-stable `GET_GLOBAL`/`SET_GLOBAL` cache (`GlobalCache::IndexBound`)
    /// guards its cached `IndexMap` index with this generation: only a define can
    /// change which index a name maps to (or introduce a shadow), so a hot reassigned
    /// top-level `let` loop never bumps it — the index cache stays hot every iteration
    /// (no thrash). Distinct from `global_version`, which keeps serving the builtin
    /// `Cached` path (and DOES bump on define).
    struct_gen: std::cell::Cell<u64>,
    /// The MODULE source of the frame most recently about to execute (SP4 §3).
    /// Updated each instruction; read by [`run`] to bind a span's own module
    /// source onto an escaping panic, so a cross-module panic renders its caret in
    /// the module the span belongs to. `None` until the first sourced frame runs.
    last_fault_source: RefCell<Option<Rc<crate::error::SourceInfo>>>,
    /// **DBG — the unified instrumentation seam (debugger/profiler/coverage).**
    /// `None` is the default and the **production hot path**: with no debugger,
    /// profiler, or coverage attached, this is `None` and `run_loop` is
    /// byte-identical to pre-DBG — it is NEVER loaded per dispatch iteration. The
    /// only hot-loop coupling is the [`Op::Break`] match arm, reached SOLELY when a
    /// breakpoint patched a byte (a side-table trap). `Box` keeps `Vm` small so the
    /// not-attached path stays cache-tight; `RefCell` lets a `&self` method arm/read
    /// the hook (the VM is driven by `&self`). Mirrors the `None`-gated
    /// zero-cost-when-off pattern of `specialize` and the SP9 determinism cell. See
    /// [`crate::vm::instrument`].
    instrument: RefCell<Option<Box<crate::vm::instrument::Instrumentation>>>,
    /// **LANE — the sync-lane kill switch (LANE §6.1).** When `true` (the default),
    /// the two-lane driver may run synchronous instruction bursts without suspending
    /// to the async executor. When `false` (`ASCRIPT_NO_SYNC_LANE=1`), the sync lane
    /// is entirely suppressed and every instruction goes through the async driver —
    /// observable behavior is byte-identical; only throughput differs. Orthogonal to
    /// `specialize` (IC/adaptive guards are inside shared helpers). Worker isolates
    /// inherit this flag from the env at construction time.
    sync_lane: bool,
    /// **LANE — sync-lane ops counter (LANE §6.4).** Counts the total number of
    /// bytecode instructions retired inside the sync lane across all bursts.
    /// Incremented once per burst (flushed from a burst-local `u64` accumulator).
    /// Read via `lane_sync_ops()`. Zero until Task 4 wires up the burst driver;
    /// used by `vm_run_source_lane_stats` to assert coverage.
    lane_sync_ops: std::cell::Cell<u64>,
    /// **LANE — sync-lane burst counter (LANE §6.4).** Counts the number of times
    /// the sync driver was entered (one burst = one uninterrupted run of the sync
    /// lane until a non-sync op is reached). Zero until Task 4.
    lane_bursts: std::cell::Cell<u64>,
    /// **The CALL kill switch (CALL §8.1).** Gates EVERY call-path fast path this
    /// spec adds: the empty-cells return (A1), in-place arg binding (A2), fiber
    /// pooling (A3), and trampoline arming (B). Permanent, mirroring `specialize`;
    /// `Vm::new_generic` sets BOTH false so the generic mode stays the complete
    /// everything-off floor.
    ///
    /// NOTE: A1's empty-cells early-return is behavior-invisible and gated only by
    /// the differential, not this flag; the flag gates new CONTROL FLOW (A2/A3/B).
    call_fast: bool,
    /// **CALL §8.3 coverage counters (anti-false-green).** Plain `Cell` bumps whose
    /// cost is bounded by the Gate-17 zero-cost bench. Asserted >0 over the
    /// functional corpus so the differential proves the fast paths actually ran.
    call_fast_stats: std::cell::Cell<CallFastStats>,
    /// **SHAPE §3.5 coverage counters (anti-false-green Gate 15).** Compiled only
    /// under test/fuzzgen/fuzzing so production carries zero overhead.
    #[cfg(any(test, feature = "fuzzgen", fuzzing))]
    shape_stats: std::cell::Cell<ShapeStats>,
    // ──────────────────────────────────────────────────────────────────────────
    // DECODE — kill switches + stat counters (DECODE Task 2, inert until Task 4)
    // ──────────────────────────────────────────────────────────────────────────
    /// **DECODE master kill switch.** When `true` (the default), the VM is
    /// PERMITTED to lazily decode `FnProto` chunks into fixed-width record
    /// streams and execute from them. When `false` (`ASCRIPT_NO_DECODE=1`),
    /// the VM always executes from `Chunk.code` bytes — byte-identical to the
    /// on path; only throughput may differ. Worker isolates inherit this flag
    /// from the environment at construction time. The `RecordSource` driver
    /// (Units A+B — decoded stream + fusion) honors it on the real hot path.
    decode: bool,
    /// **DECODE Unit-C kill switch — INERT (Unit C evidence-dropped).** The
    /// `ASCRIPT_NO_DECODE_INLINE` env read still populates this flag, but Unit C
    /// (speculative global-fn inlining) was reverted by evidence, so nothing
    /// consults it: it is a permanent no-op kept only so the env var keeps
    /// parsing. Removing it cascades through the 8-arg `with_all_flags`/census
    /// constructors for no behavioral gain.
    decode_inline: bool,
    /// **DECODE Unit-D kill switch — INERT (Unit D evidence-dropped).** As with
    /// `decode_inline`: the `ASCRIPT_NO_DECODE_TOS` env read still populates this
    /// flag, but Unit D (top-of-stack register caching) was reverted by evidence,
    /// so nothing consults it — a permanent no-op kept only for env-var parity.
    decode_tos: bool,
    /// **DECODE warmth threshold (Task-11 A/B knob).** A proto must be entered
    /// at least this many times before its chunk is decoded. Production default
    /// = `DECODE_THRESHOLD` (placeholder 8, pinned by Task-11 A/B data).
    /// Test entry points set this to 0 so decoding triggers immediately.
    decode_threshold: u16,
    /// **DECODE stat counters (anti-false-green, DECODE §8.3).** `decoded_ops`/
    /// `decoded_bytes`/`stack_ops` are wired by the RecordSource driver and
    /// `fused_ops` by Unit B; the `inline_*`/`tos_ops` counters stay 0 (Units
    /// C/D evidence-dropped — inert). Compiled only under test/fuzzgen/fuzzing.
    /// See [`DecodeStatsInner`] for field semantics.
    #[cfg(any(test, feature = "fuzzgen", fuzzing))]
    decode_stats: std::cell::Cell<crate::vm::run::DecodeStatsInner>,
    /// **DECODE §5.1 (Unit B part 1): the pair/triple census table.** FULLY
    /// `#[cfg(feature = "decode-census")]` — the field DOES NOT EXIST in a default
    /// build (the JIT-spec §2.1 "not there" discipline; zero Gate-12 exposure). It
    /// is also flag-gated: `None` (the default) ⇒ the census is INERT even when the
    /// feature is on (the record driver's `note_retired` only records when this is
    /// `Some`). Armed ONLY by `tests/decode_census.rs` via [`Vm::arm_census`], which
    /// runs the corpus in forced-decode mode; the ranked table the harness prints is
    /// the MEASURED data Task 8 fuses. A `RefCell` because the table accumulates; the
    /// borrow is a short synchronous bump in `note_retired`, never held across
    /// `.await`.
    #[cfg(feature = "decode-census")]
    census: RefCell<Option<crate::vm::decode::DecodeCensus>>,
    /// **CALL §4 A3 — fiber pool for re-entrant calls.**
    ///
    /// Holds recycled [`Fiber`]s that `take_pooled_fiber` / `return_pooled_fiber`
    /// manage. Every native→VM re-entry (`call_value`, `invoke_compiled_method`,
    /// `invoke_compiled_static`) previously built a brand-new `Fiber` (two `Vec`s)
    /// and dropped it; this pool amortises those allocations over repeated calls.
    ///
    /// **Protocol:**
    /// * Take = exclusive ownership transfer (POP from the pool, call `Fiber::reset`
    ///   on the popped fiber, and hand it to `run`). A nested re-entry that arrives
    ///   while a fiber is mid-flight finds a different slot or allocates — safe by
    ///   construction because removal happens before `run`.
    /// * Return ONLY on `RunOutcome::Done`. On `Err` (panic/propagate) the fiber is
    ///   DROPPED, never pooled — mid-flight state, fresh fibers have no old state.
    /// * This field is a `RefCell` but every borrow is a short synchronous pop/push
    ///   — NEVER held across an `.await`.
    ///
    /// Capacity is capped at [`FIBER_POOL_MAX`]. The pool only ever holds fibers
    /// with `frames.is_empty()` (cleared before parking; take calls `reset` before
    /// handing out).
    ///
    /// **Not used when `call_fast = false`** (the kill switch disables this path
    /// exactly like A2's in-place binding, so the no-call-fast differential mode
    /// stays the complete everything-off floor).
    fiber_pool: RefCell<Vec<crate::vm::fiber::Fiber>>,
    /// DBG: the flattened proto tree of the program under inspection, for resolving a
    /// source (file, line) to a `(proto_id, offset)` to patch. Populated ONLY when a
    /// debugger arms breakpoints (empty otherwise → zero cost, NEVER read on the hot
    /// path; the dispatch loop never touches it). Each entry is an `Rc<FnProto>` clone
    /// (the entry body + every nested fn, recursively) so a `(file, line)` can target
    /// any proto, not just the current frame.
    debug_protos: RefCell<Vec<Rc<crate::vm::chunk::FnProto>>>,
    /// **ELIDE §4.2/§5.2 — contract-elision compile mode for IMPORTED modules.**
    /// When `true`, `compile_module_file` runs the `ElisionSet` collector on each
    /// imported module's source and compiles it via `compile_source_with_elision`
    /// (proven contract checks dropped from the bytecode). When `false` (the
    /// default, and the `--no-elide` / `ASCRIPT_NO_ELIDE=1` kill switch), imports
    /// compile byte-identically to pre-ELIDE. The ENTRY module's elision is applied
    /// by the runner before the VM exists; this flag governs only the import loader.
    /// A `Cell` set after construction (like `module_dir`), never read on the hot
    /// path — only at module-compile time. **NOT inherited by worker isolates** (a
    /// worker `Interp`/`Vm` is built fresh with `elide=false`; worker slices keep
    /// full checks, §4.6).
    elide: std::cell::Cell<bool>,
}

/// DBG: the final path component of a `/`- or `\`-separated path (the file name),
/// used by the v1 multi-module breakpoint source-matching heuristic. A path with no
/// separator returns itself.
fn file_name_of(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

impl Vm {
    /// Build a VM over `interp` and install its self-`Weak` (mirroring
    /// [`Interp::install_self`]).
    pub fn new(interp: Rc<Interp>) -> Rc<Self> {
        Self::with_specialize(interp, true)
    }

    /// Build a NON-specializing ("generic") VM — the `--no-specialize` kill switch
    /// (V11-T5). All inline-cache and adaptive fast paths are disabled; every
    /// dispatch takes the generic path. Used by `vm_run_source_generic` and the
    /// three-way differential to prove the fast paths never change a result.
    ///
    /// CALL §8.1: `new_generic` sets BOTH `specialize` AND `call_fast` to `false`,
    /// keeping the generic mode as the complete everything-off floor.
    pub fn new_generic(interp: Rc<Interp>) -> Rc<Self> {
        Self::with_specialize(interp, false)
    }

    /// Shared constructor: build a VM with `specialize` set explicitly and install
    /// its self-`Weak` (mirroring [`Interp::install_self`]).
    ///
    /// CALL §8.1: delegates to [`with_flags`](Self::with_flags) with
    /// `call_fast = specialize` — so `specialize = false` (generic) ⇒ both off.
    pub fn with_specialize(interp: Rc<Interp>, specialize: bool) -> Rc<Self> {
        // LANE §6.1: default sync_lane from the environment, exactly like
        // ASCRIPT_NO_SPECIALIZE does for `specialize`. The env default is what
        // lets worker isolates inherit the kill switch without explicit plumbing.
        let sync_lane = std::env::var("ASCRIPT_NO_SYNC_LANE").as_deref() != Ok("1");
        // CALL §8.1: call_fast defaults from the environment like specialize/sync_lane.
        let call_fast = specialize
            && std::env::var("ASCRIPT_NO_CALL_FAST").as_deref() != Ok("1");
        // DECODE Task 2: decode flags default from the environment (worker-isolate
        // inheritance — the same pattern as ASCRIPT_NO_SYNC_LANE / ASCRIPT_NO_CALL_FAST).
        let decode = std::env::var("ASCRIPT_NO_DECODE").as_deref() != Ok("1");
        let decode_inline = std::env::var("ASCRIPT_NO_DECODE_INLINE").as_deref() != Ok("1");
        let decode_tos = std::env::var("ASCRIPT_NO_DECODE_TOS").as_deref() != Ok("1");
        // DECODE §2.3: the warmth threshold A/B knob (documented in `docs/content/cli.md`).
        // Defaults to `DECODE_THRESHOLD`; a parseable `ASCRIPT_DECODE_THRESHOLD` overrides
        // it (0 = decode immediately). Threshold only affects WHEN a proto decodes, never
        // observable behavior — the four/seven-mode byte-identity is unaffected.
        let decode_threshold = std::env::var("ASCRIPT_DECODE_THRESHOLD")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(Self::DECODE_THRESHOLD);
        Self::with_all_flags(interp, specialize, sync_lane, call_fast, decode, decode_inline, decode_tos, decode_threshold)
    }

    /// Build a VM with `specialize` and `sync_lane` set explicitly (LANE §6.1).
    ///
    /// Unlike [`with_specialize`](Self::with_specialize), this constructor does NOT
    /// consult the environment — the caller controls both kill switches directly.
    /// Used by the test entry points (`vm_run_source_no_sync_lane`,
    /// `vm_run_source_lane_stats`) so tests can toggle `sync_lane` without setting
    /// environment variables.
    ///
    /// CALL §8.1: `call_fast` mirrors `specialize` here (the same as the
    /// `with_specialize` default). Use [`with_flags`](Self::with_flags) to set
    /// `call_fast` independently (e.g. for `vm_run_source_no_call_fast`).
    pub fn with_lanes(interp: Rc<Interp>, specialize: bool, sync_lane: bool) -> Rc<Self> {
        Self::with_flags(interp, specialize, sync_lane, specialize)
    }

    /// Build a VM with all three legacy kill switches set explicitly (CALL §8.1).
    ///
    /// The lowest-level constructor (CALL era): does NOT consult environment
    /// variables. Used by test entry points that need independent control of each
    /// switch (`vm_run_source_no_call_fast` sets `specialize=true, sync_lane=true,
    /// call_fast=false` to isolate CALL divergences from IC/lane divergences).
    ///
    /// DECODE Task 2: delegates to [`with_all_flags`](Self::with_all_flags) with
    /// `decode = true` (on), `decode_inline = true`, `decode_tos = true`, and
    /// `decode_threshold = DECODE_THRESHOLD` so this constructor keeps its
    /// existing callers working without change.
    pub fn with_flags(
        interp: Rc<Interp>,
        specialize: bool,
        sync_lane: bool,
        call_fast: bool,
    ) -> Rc<Self> {
        Self::with_all_flags(
            interp,
            specialize,
            sync_lane,
            call_fast,
            true,  // decode = on
            true,  // decode_inline = on
            true,  // decode_tos = on
            Self::DECODE_THRESHOLD,
        )
    }

    /// **The lowest-level constructor (DECODE Task 2).** Accepts all kill
    /// switches (specialize, sync_lane, call_fast, decode, decode_inline,
    /// decode_tos) and the warmth threshold explicitly. Does NOT consult
    /// environment variables — all test entry points that need DECODE-specific
    /// control use this constructor so tests never set env vars (parallel-test
    /// hygiene, the LANE/CALL pattern).
    #[allow(clippy::too_many_arguments)] // DECODE adds 4 flags; a builder struct is overkill for an internal-only constructor
    pub fn with_all_flags(
        interp: Rc<Interp>,
        specialize: bool,
        sync_lane: bool,
        call_fast: bool,
        decode: bool,
        decode_inline: bool,
        decode_tos: bool,
        decode_threshold: u16,
    ) -> Rc<Self> {
        let vm = Rc::new(Vm {
            interp,
            self_weak: RefCell::new(Weak::new()),
            class_methods: RefCell::new(FxHashMap::default()),
            class_static_methods: RefCell::new(FxHashMap::default()),
            class_defaults: RefCell::new(FxHashMap::default()),
            shapes: RefCell::new(crate::vm::shape::ShapeRegistry::new()),
            class_env: RefCell::new(None),
            specialize,
            module_exports: RefCell::new(Rc::new(RefCell::new(indexmap::IndexMap::new()))),
            file_modules: RefCell::new(HashMap::new()),
            module_dir: RefCell::new(std::env::current_dir().unwrap_or_else(|_| ".".into())),
            module_archive: RefCell::new(None),
            module_logical_dir: RefCell::new(String::new()),
            user_globals: RefCell::new(indexmap::IndexMap::with_hasher(FxBuildHasher)),
            global_version: std::cell::Cell::new(0),
            struct_gen: std::cell::Cell::new(0),
            last_fault_source: RefCell::new(None),
            instrument: RefCell::new(None),
            debug_protos: RefCell::new(Vec::new()),
            elide: std::cell::Cell::new(false),
            sync_lane,
            lane_sync_ops: std::cell::Cell::new(0),
            lane_bursts: std::cell::Cell::new(0),
            call_fast,
            call_fast_stats: std::cell::Cell::new(CallFastStats::default()),
            #[cfg(any(test, feature = "fuzzgen", fuzzing))]
            shape_stats: std::cell::Cell::new(ShapeStats::default()),
            fiber_pool: RefCell::new(Vec::new()),
            decode,
            decode_inline,
            decode_tos,
            decode_threshold,
            #[cfg(any(test, feature = "fuzzgen", fuzzing))]
            decode_stats: std::cell::Cell::new(DecodeStatsInner::default()),
            #[cfg(feature = "decode-census")]
            census: RefCell::new(None),
        });
        *vm.self_weak.borrow_mut() = Rc::downgrade(&vm);
        // Register the VM on the shared interpreter so a native higher-order
        // stdlib function (e.g. `array.map`, `recover`) can re-enter the VM to
        // run a `Value::closure` callback (the `native → VM` half of the bridge;
        // see `Interp::call_value`'s `Closure` arm and `Vm::call_value`).
        vm.interp.set_vm(Rc::downgrade(&vm));
        vm
    }

    /// Whether the sync lane is enabled (LANE §6.1). `true` = sync bursts allowed;
    /// `false` = kill switch active, every instruction takes the async driver.
    pub fn sync_lane(&self) -> bool {
        self.sync_lane
    }

    /// Total bytecode instructions retired inside the sync lane (LANE §6.4).
    /// Always 0 until Task 4 wires up the burst driver.
    pub fn lane_sync_ops(&self) -> u64 {
        self.lane_sync_ops.get()
    }

    /// Number of sync-lane bursts entered (LANE §6.4).
    /// Always 0 until Task 4 wires up the burst driver.
    pub fn lane_bursts(&self) -> u64 {
        self.lane_bursts.get()
    }

    /// Whether the CALL fast paths are enabled (CALL §8.1). `true` = call-path
    /// fast paths active (A2 in-place binding, A3 fiber pooling, B trampoline);
    /// `false` = kill switch active, all CALL fast paths skip to the slow generic
    /// path. Behavior is byte-identical regardless; only throughput differs.
    pub fn call_fast(&self) -> bool {
        self.call_fast
    }

    /// CALL §8.3 coverage counters (anti-false-green). All counters are 0 until
    /// the corresponding fast paths are wired in Phase 2/3. `#[doc(hidden)]` test
    /// API — not part of the public surface.
    #[doc(hidden)]
    pub fn call_fast_stats(&self) -> CallFastStats {
        self.call_fast_stats.get()
    }

    /// CALL §8.3: bump one or more coverage counters atomically (a Cell, so this
    /// is a synchronous `get → mutate → set` — never called across an `.await`).
    /// Not yet called (Phase 1 is INERT); wired in Phase 2/3 when each fast path lands.
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn bump_stat(&self, f: impl FnOnce(&mut CallFastStats)) {
        let mut s = self.call_fast_stats.get();
        f(&mut s);
        self.call_fast_stats.set(s);
    }

    /// CALL §5 B: record one trampoline callback element dispatched on the sync lane.
    /// Called from `trampoline.rs` (same crate, so `pub(crate)` suffices).
    #[inline]
    pub(crate) fn bump_trampoline_call(&self) {
        self.bump_stat(|s| s.trampoline_calls += 1);
    }

    /// CALL §5 B: record one trampoline escalation to the async driver.
    #[inline]
    pub(crate) fn bump_trampoline_escalation(&self) {
        self.bump_stat(|s| s.trampoline_escalations += 1);
    }

    // ──────────────────────────────────────────────────────────────────────────
    // SHAPE §3.5 — storage-mode coverage counters
    // ──────────────────────────────────────────────────────────────────────────

    /// SHAPE §3.5: return the three storage-mode counters as `(slab, dict, demote)`.
    /// `#[doc(hidden)]` — test API only; not a stable public surface.
    #[cfg(any(test, feature = "fuzzgen", fuzzing))]
    #[doc(hidden)]
    pub fn obj_mode_stats(&self) -> (u64, u64, u64) {
        let s = self.shape_stats.get();
        (s.obj_slab_constructed, s.obj_dict_constructed, s.obj_demotions)
    }

    /// SHAPE §3.5: bump one or more shape-stats counters (Cell, never held across
    /// `.await`).  Compiled only under test/fuzzgen/fuzzing — the call sites are
    /// individually gated with `#[cfg(any(test, feature = "fuzzgen", fuzzing))]`.
    #[cfg(any(test, feature = "fuzzgen", fuzzing))]
    #[inline]
    pub(crate) fn bump_shape_stat(&self, f: impl FnOnce(&mut ShapeStats)) {
        let mut s = self.shape_stats.get();
        f(&mut s);
        self.shape_stats.set(s);
    }

    // ──────────────────────────────────────────────────────────────────────────
    // DECODE — kill switches + stat counters (DECODE Task 2)
    // ──────────────────────────────────────────────────────────────────────────

    /// **DECODE §2.3 — warmth threshold.** A proto must be entered at least this
    /// many times before its chunk is decoded into a fixed-width record stream.
    /// Placeholder value 8 — pinned by Task-11 threshold A/B data.
    /// Test entry points override it to 0 so decoding triggers immediately.
    /// INERT until Task 4 consults it; declared here so Task 4's `decode_threshold`
    /// field initializer and the `ASCRIPT_DECODE_THRESHOLD` env knob reference a
    /// single constant.
    pub(crate) const DECODE_THRESHOLD: u16 = 8;

    /// Whether the DECODE fast path is enabled. `true` = lazy decode + record
    /// execution permitted; `false` = always byte-dispatch (INERT until Task 4).
    pub fn decode(&self) -> bool {
        self.decode
    }

    /// Whether DECODE Unit-C speculative inlining is enabled. INERT until Task 9.
    pub fn decode_inline(&self) -> bool {
        self.decode_inline
    }

    /// Whether DECODE Unit-D TOS caching is enabled. INERT until Task 10.
    pub fn decode_tos(&self) -> bool {
        self.decode_tos
    }

    /// The current DECODE warmth threshold. INERT until Task 4.
    pub fn decode_threshold(&self) -> u16 {
        self.decode_threshold
    }

    /// **DECODE §8.3 coverage counters.** Returns the raw `DecodeStatsInner`.
    /// All fields are 0 until the corresponding task wires them up.
    /// `#[doc(hidden)]` — test API only; not a stable surface.
    #[cfg(any(test, feature = "fuzzgen", fuzzing))]
    #[doc(hidden)]
    pub fn decode_stats_inner(&self) -> DecodeStatsInner {
        self.decode_stats.get()
    }

    /// **DECODE §8.3: bump one or more DECODE stat counters** (Cell, never held
    /// across `.await`). Compiled only under test/fuzzgen/fuzzing. Not yet called
    /// (INERT); wired in Task 4+ when each path lands.
    #[cfg(any(test, feature = "fuzzgen", fuzzing))]
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn bump_decode_stat(&self, f: impl FnOnce(&mut DecodeStatsInner)) {
        let mut s = self.decode_stats.get();
        f(&mut s);
        self.decode_stats.set(s);
    }

    // ──────────────────────────────────────────────────────────────────────────
    // WARM B §3.1 — PGO warm-state harvest
    // ──────────────────────────────────────────────────────────────────────────

    /// **WARM B §3.1 — harvest the VM's warmed inline caches into a [`PgoSection`].**
    ///
    /// Called after the training run completes (success or caught panic) to snapshot
    /// the adaptive state that the VM accumulated during execution.
    ///
    /// The caller supplies `modules`: each entry is
    /// - the module's logical archive key,
    /// - the sha256 of the module's raw `.aso` chunk bytes (pre-computed by the caller
    ///   from the archive bytes — the same value the seeder will hash-validate), and
    /// - a reference to the **live `FnProto`** that the training run executed.  The ICs
    ///   are stored on `FnProto.chunk.{field_ics,arith_caches,global_caches}` — they are
    ///   populated by the VM during the run and MUST be read from those same instances,
    ///   NOT from a freshly-decoded copy (fresh copies have empty side tables).
    ///
    /// For each proto (recursively) the function keeps:
    /// - `ArithCache::Specialized { kind }` → `(offset, kind as u8)`
    /// - `InlineCache::Mono/Poly` → shape → `keys_of_pgo` → deduped key-list index
    /// - `GlobalCache::Cached` → offset only (the seeder re-resolves the builtin)
    ///
    /// `GlobalCache::IndexBound` is deliberately excluded (define-order dependent).
    /// `InlineCache::Mega` is excluded (too many shapes; no useful seed).
    ///
    /// Key lists are deduplicated across the whole section: two Mono sites with the same
    /// field layout share one key-list entry.
    ///
    /// An unresolvable shape id (the shape was promoted/evicted since the run) is silently
    /// skipped — the seeder is written to handle missing seeds gracefully.
    ///
    /// All borrows are plain synchronous (`RefCell::borrow`) — this method is NOT `async`
    /// and MUST NOT be called while any other borrow is held on the VM's side tables.
    pub fn harvest_pgo(
        &self,
        modules: &[(String, [u8; 32], &crate::vm::chunk::FnProto)],
    ) -> crate::vm::pgo::PgoSection {
        use crate::vm::adapt::{ArithCache, GlobalCache};
        use crate::vm::ic::InlineCache;
        use crate::vm::pgo::{PgoModule, PgoProto, PgoSection};
        use std::collections::HashMap;

        // Deduped key-list table: a key-list (ordered Vec<String>) → its index in
        // `key_lists`.  We build this incrementally as we encounter IC shapes.
        let mut key_lists: Vec<Vec<String>> = Vec::new();
        let mut key_list_index: HashMap<Vec<String>, u32> = HashMap::new();

        /// Intern `keys` into the dedup table, returning its index.
        fn intern_keys(
            keys: Vec<String>,
            key_lists: &mut Vec<Vec<String>>,
            key_list_index: &mut HashMap<Vec<String>, u32>,
        ) -> u32 {
            if let Some(&idx) = key_list_index.get(&keys) {
                return idx;
            }
            let idx = key_lists.len() as u32;
            key_lists.push(keys.clone());
            key_list_index.insert(keys, idx);
            idx
        }

        // Walk a FnProto recursively with a path tracking the index sequence from the
        // chunk root.  Populates `protos_out` with one `PgoProto` per reachable proto
        // that has at least one IC entry (empty protos are elided from the section).
        fn walk_proto(
            proto: &crate::vm::chunk::FnProto,
            path: &mut Vec<u32>,
            shapes: &crate::vm::shape::ShapeRegistry,
            key_lists: &mut Vec<Vec<String>>,
            key_list_index: &mut HashMap<Vec<String>, u32>,
            protos_out: &mut Vec<PgoProto>,
        ) {
            // ── arith seeds ─────────────────────────────────────────────────────
            let arith_entries: Vec<(u32, u8)> = {
                let ics = proto.chunk.arith_caches.borrow();
                ics.iter()
                    .filter_map(|(&off, cache)| {
                        if let ArithCache::Specialized { kind } = cache {
                            Some((off as u32, *kind as u8))
                        } else {
                            None
                        }
                    })
                    .collect()
            };

            // ── field IC seeds ───────────────────────────────────────────────────
            // For each warm IC site collect the set of shape key-lists seen there.
            let field_entries: Vec<(u32, Vec<u32>)> = {
                let ics = proto.chunk.field_ics.borrow();
                ics.iter()
                    .filter_map(|(&off, ic)| {
                        // Collect all shape_ids from this IC site.
                        let shape_ids: Vec<u32> = match ic {
                            InlineCache::Mono { shape, .. } => vec![*shape],
                            InlineCache::Poly { entries, len } => {
                                entries[..*len as usize]
                                    .iter()
                                    .map(|&(s, _)| s)
                                    .collect()
                            }
                            InlineCache::Cold | InlineCache::Mega => return None,
                        };

                        // Resolve shapes → key lists → dedup indices.
                        let mut list_indices: Vec<u32> = Vec::with_capacity(shape_ids.len());
                        for shape_id in shape_ids {
                            if let Some(keys) = shapes.keys_of_pgo(shape_id) {
                                let idx = intern_keys(keys, key_lists, key_list_index);
                                if !list_indices.contains(&idx) {
                                    list_indices.push(idx);
                                }
                            }
                            // unknown shape id → skip silently
                        }
                        if list_indices.is_empty() {
                            return None;
                        }
                        Some((off as u32, list_indices))
                    })
                    .collect()
            };

            // ── global cache seeds ────────────────────────────────────────────────
            let global_entries: Vec<u32> = {
                let ics = proto.chunk.global_caches.borrow();
                ics.iter()
                    .filter_map(|(&off, cache)| {
                        if matches!(cache, GlobalCache::Cached { .. }) {
                            Some(off as u32)
                        } else {
                            None
                        }
                    })
                    .collect()
            };

            // Only emit a proto record when at least one IC was warmed.
            if !arith_entries.is_empty() || !field_entries.is_empty() || !global_entries.is_empty() {
                protos_out.push(PgoProto {
                    path: path.clone(),
                    arith: arith_entries,
                    fields: field_entries,
                    globals: global_entries,
                });
            }

            // Recurse into nested protos (child proto index = position in
            // `chunk.protos`).
            for (i, child) in proto.chunk.protos.iter().enumerate() {
                path.push(i as u32);
                walk_proto(child, path, shapes, key_lists, key_list_index, protos_out);
                path.pop();
            }
        }

        let shapes_ref = self.shapes.borrow();
        let mut pgo_modules: Vec<PgoModule> = Vec::with_capacity(modules.len());

        for (module_key, chunk_sha256, root_proto) in modules {
            let mut protos: Vec<PgoProto> = Vec::new();
            let mut path: Vec<u32> = Vec::new();
            walk_proto(
                root_proto,
                &mut path,
                &shapes_ref,
                &mut key_lists,
                &mut key_list_index,
                &mut protos,
            );

            pgo_modules.push(PgoModule {
                module_key: module_key.clone(),
                chunk_sha256: *chunk_sha256,
                protos,
            });
        }

        PgoSection {
            key_lists,
            modules: pgo_modules,
        }
    }

    /// **WARM B §3.3/§3.5 — seed one module's warmed side tables from a PGO profile.**
    ///
    /// Pre-installs the arith/field-IC/global cache entries a runtime warm-up would have
    /// produced, so a warm-started run skips the warm-up window. Returns the number of
    /// entries actually installed (the COVERAGE metric — `> 0` proves the seed was live).
    ///
    /// **THE SOUNDNESS KEYSTONE (§3.5):** every install lands BEHIND AN EXISTING GUARD; no
    /// path trusts a profile index. A corrupt/stale/LYING profile therefore degrades to a
    /// cache MISS (→ the generic path), never wrong behavior:
    ///
    /// 1. **Digest gate** — `module.chunk_sha256` must equal the sha256 of `chunk_sha256`
    ///    (the live module's stored bytes, supplied by the caller); a mismatch ⇒ return 0
    ///    (the profile was recorded against different bytecode — its offsets/paths are
    ///    meaningless). `chunk` is the entry chunk; `proto_at` resolves nested protos.
    /// 2. **Shape remap** — each profile key-list is interned through THIS `Vm`'s
    ///    `ShapeRegistry` → a fresh per-`Vm` id (ids are per-Vm and never serialized). A
    ///    seeded `Mono{shape}` only hits if a runtime receiver has the IDENTICAL key
    ///    layout, in which case the derived index is correct by the shape invariant.
    /// 3. **Per-proto resolution** — an out-of-range proto path ⇒ skip that proto.
    /// 4. **Arith** — install `ArithCache::Specialized{kind}` (kind byte range-checked;
    ///    the run loop re-guards operand kinds and deopts on a miss).
    /// 5. **Field** — the index is **DERIVED, never trusted**: read the property NAME from
    ///    the chunk's own const operand at the site, find its position in the interned key
    ///    list, and install `Mono`/`Poly` with THAT index. A name absent from the key list
    ///    ⇒ skip the entry (the one hole a trusted index would open — closed here).
    /// 6. **Global** — read the site's name operand; install `GlobalCache::Cached` ONLY if
    ///    the name resolves in the LIVE builtin table (else skip). The version guard stays.
    ///
    /// ALL borrows are plain synchronous (`RefCell`); the shape-registry borrow is SCOPED
    /// per key-list (never held across an install). Not `async` — no await anywhere.
    pub fn seed_chunk(
        &self,
        chunk: &crate::vm::chunk::Chunk,
        module: &crate::vm::pgo::PgoModule,
        chunk_sha256: &[u8; 32],
        key_lists: &[Vec<String>],
    ) -> usize {
        use crate::vm::adapt::{ArithCache, GlobalCache};
        use crate::vm::ic::InlineCache;
        use crate::vm::opcode::Op;

        // ── Step 1: digest gate ─────────────────────────────────────────────────
        // A stale profile (recorded against different bytecode) is rejected wholesale —
        // its offsets and proto paths address a chunk that no longer exists.
        if &module.chunk_sha256 != chunk_sha256 {
            return 0;
        }

        // Pre-intern every referenced key-list to a fresh per-Vm shape id. An interning
        // failure (a layout exceeding the shape caps — SLAB_MAX_KEYS / SHAPE_FANOUT_MAX)
        // yields `None`, and any field entry referencing it is skipped (a miss, never a lie).
        // The borrow is SCOPED to this block — released before any install.
        let interned: Vec<Option<u32>> = {
            let mut shapes = self.shapes.borrow_mut();
            key_lists
                .iter()
                .map(|kl| shapes.shape_for(kl.iter().map(String::as_str)))
                .collect()
        };

        let mut installed = 0usize;

        for pproto in &module.protos {
            // ── Step 3: resolve the proto at this index path (out-of-range ⇒ skip) ──
            let Some(target) = proto_at(chunk, &pproto.path) else {
                continue;
            };
            let code = &*target.code;
            let code_len = code.len();

            // ── Step 4: arith seeds ─────────────────────────────────────────────
            for &(off, kind_tag) in &pproto.arith {
                let off = off as usize;
                // The arith cache is keyed by the op byte offset; an offset past the
                // code is a corrupt profile → skip (never index out of range).
                if off >= code_len {
                    continue;
                }
                let Some(kind) = arith_kind_from_tag(kind_tag) else {
                    continue; // unknown kind byte ⇒ skip (range-checked)
                };
                // Behind a guard: the run loop's fast path re-confirms operand kinds and
                // deopts on a miss, so a wrong seed can only be a deopt, never a wrong result.
                target.set_arith_cache(off, ArithCache::Specialized { kind });
                installed += 1;
            }

            // ── Step 5: field-IC seeds — the DERIVED index ──────────────────────
            for (off, list_idxs) in &pproto.fields {
                let off = *off as usize;
                // Need the opcode byte + a u16 operand: off, off+1, off+2 must be in range.
                if off + 2 >= code_len {
                    continue;
                }
                // Defensive: the site MUST be a field op (GET_PROP/SET_PROP). A profile
                // that points `off` at some other op is corrupt → skip.
                let opb = Op::from_u8(code[off]);
                if !matches!(opb, Some(Op::GetProp) | Some(Op::SetProp)) {
                    continue;
                }
                // Read the property NAME from the chunk's own (verified) const operand.
                let Some(name) = const_str_operand(target, off + 1) else {
                    continue;
                };

                // Build the IC by deriving the index for `name` in each referenced key list.
                // NEVER trust the profile's claimed index — there is none in the wire format.
                let mut ic = InlineCache::Cold;
                for &li in list_idxs {
                    let Some(shape) = interned.get(li as usize).copied().flatten() else {
                        continue; // out-of-range list idx OR un-internable layout ⇒ skip
                    };
                    let Some(kl) = key_lists.get(li as usize) else {
                        continue;
                    };
                    // DERIVE: the index is the name's position in the ACTUAL key list. A
                    // lying layout (name absent / mis-ordered) ⇒ no position ⇒ skip → the
                    // shape guard at runtime would miss anyway (shape-id only equals a
                    // receiver whose layout IS this list, where the position is correct).
                    let Some(pos) = kl.iter().position(|k| k == &name) else {
                        continue; // name absent ⇒ skip this layout (the §3.3 keystone)
                    };
                    ic.record(shape, pos as u32);
                }
                // Only install if at least one layout survived derivation.
                if !matches!(ic, InlineCache::Cold) {
                    target.set_field_ic(off, ic);
                    installed += 1;
                }
            }

            // ── Step 6: global seeds — live builtin resolution only ─────────────
            for &off in &pproto.globals {
                let off = off as usize;
                if off + 2 >= code_len {
                    continue;
                }
                if !matches!(Op::from_u8(code[off]), Some(Op::GetGlobal)) {
                    continue;
                }
                let Some(name) = const_str_operand(target, off + 1) else {
                    continue;
                };
                // Install ONLY if the name resolves in the LIVE builtin table. A user-named
                // or unresolvable site is skipped; a name that later becomes a shadowing
                // user-global bumps `global_version`, invalidating this seed (the guard).
                if crate::interp::BUILTIN_NAMES.contains(&name.as_str()) {
                    let v = Value::builtin(Rc::from(name.as_str()));
                    target.set_global_cache(off, GlobalCache::set(v, self.global_version()));
                    installed += 1;
                }
            }
        }

        installed
    }

    /// **WARM B §3.3 — seed the entry chunk from a decoded PGO section** (the archive
    /// load entry point). Gated on `vm.specialize` (the generic VM is the semantic floor —
    /// it consults no caches, so a seed would be dead weight AND must be skipped to keep
    /// generic == specialized) and on the caller's `seed` flag (the `ASCRIPT_NO_PGO` kill
    /// switch / the test seam). Finds the entry module record by logical key, validates its
    /// digest against `entry_sha256`, and seeds. Returns the installed count (0 when gated
    /// off, the section is absent, the module is not recorded, or the digest mismatches).
    pub fn seed_entry_from_section(
        &self,
        chunk: &crate::vm::chunk::Chunk,
        section: &crate::vm::pgo::PgoSection,
        entry_key: &str,
        entry_sha256: &[u8; 32],
        seed: bool,
    ) -> usize {
        if !seed || !self.specialize {
            return 0;
        }
        let Some(module) = section.modules.iter().find(|m| m.module_key == entry_key) else {
            return 0;
        };
        self.seed_chunk(chunk, module, entry_sha256, &section.key_lists)
    }

    // ──────────────────────────────────────────────────────────────────────────
    // DECODE §5.1 (Unit B part 1) — the pair/triple census (feature-gated)
    // ──────────────────────────────────────────────────────────────────────────

    /// **DECODE §5.1: arm the pair/triple census.** After this call the record
    /// driver's `note_retired` records `(prev, op)` pairs and `(prev2, prev, op)`
    /// triples WITHIN BASIC BLOCKS (the burst-local `prev`/`prev2` reset at every
    /// jump/escalation/entry, so no record straddles a boundary a fused
    /// superinstruction could not legally cross). FULLY `#[cfg(feature =
    /// "decode-census")]` — never present in a default build. Used ONLY by
    /// `tests/decode_census.rs`.
    #[cfg(feature = "decode-census")]
    pub fn arm_census(&self) {
        *self.census.borrow_mut() = Some(crate::vm::decode::DecodeCensus::default());
    }

    /// **DECODE §5.1: drain the census table.** Returns `(counts, total_records)`
    /// where a pair key is `(CENSUS_NO_PREV, prev, op)` and a triple key is
    /// `(prev2, prev, op)`. `None` if the census was never armed. The harness
    /// merges per-program drains into a global aggregate before ranking.
    #[cfg(feature = "decode-census")]
    pub fn take_census(&self) -> Option<(crate::vm::decode::CensusCounts, u64)> {
        self.census
            .borrow_mut()
            .take()
            .map(|c| (c.counts, c.total_records))
    }

    /// **DECODE §5.1: record one retired op into the census** (if armed). Given the
    /// burst-local `(prev2, prev)` predecessors — each `Some` ONLY when the
    /// predecessor is in the SAME basic block (the [`RecordSource`] resets them to
    /// `None` at every boundary). A no-op (a single `Option` borrow + early return)
    /// when the census is not armed; the whole method is gated out of default builds.
    #[cfg(feature = "decode-census")]
    #[inline]
    pub(crate) fn census_record(&self, prev2: Option<u16>, prev: Option<u16>, op: u16) {
        if let Some(c) = self.census.borrow_mut().as_mut() {
            c.record(prev2, prev, op);
        }
    }

    // ──────────────────────────────────────────────────────────────────────────
    // CALL §4 A3 — fiber pool
    // ──────────────────────────────────────────────────────────────────────────

    /// Maximum number of idle [`Fiber`]s kept in the pool.
    const FIBER_POOL_MAX: usize = 8;

    /// **CALL §4 A3 — take a fiber from the pool (or allocate a fresh one).**
    ///
    /// POPs one fiber from `fiber_pool`, resets it for `top`, and returns it.
    /// Because the pop REMOVES the fiber, a nested re-entrant call that arrives
    /// while this fiber is mid-flight will take a DIFFERENT entry from the pool
    /// (or allocate a fresh one if the pool is empty) — safe by construction.
    ///
    /// When `call_fast = false` the pool is bypassed entirely: always allocate a
    /// fresh `Fiber::new(top)` (the kill switch keeps no-call-fast identical).
    ///
    /// # Await discipline
    /// The `fiber_pool` `RefCell` borrow is held only for the synchronous pop —
    /// it is NEVER held across an `.await`.
    #[inline]
    fn take_pooled_fiber(&self, top: gcmodule::Cc<crate::vm::value_ext::Closure>) -> crate::vm::fiber::Fiber {
        if self.call_fast {
            if let Some(mut fiber) = self.fiber_pool.borrow_mut().pop() {
                fiber.reset(top);
                self.bump_stat(|s| s.pooled_fiber_reuses += 1);
                return fiber;
            }
        }
        crate::vm::fiber::Fiber::new(top)
    }

    /// **CALL §4 A3 — return a fiber to the pool after clean completion.**
    ///
    /// Called ONLY after `RunOutcome::Done` — the fiber ran to completion and its
    /// frame stack was popped to empty by the normal `return_from_frame` path.
    /// Clears the stack, then pushes the fiber back if the pool is below
    /// [`FIBER_POOL_MAX`] and `call_fast` is on.
    ///
    /// On `Err` (panic / propagate) the caller DROPS the fiber — this method is
    /// NOT called — so stale mid-flight state can never enter the pool.
    ///
    /// # Await discipline
    /// The `fiber_pool` `RefCell` borrow is held only for the synchronous push —
    /// NEVER across an `.await`.
    #[inline]
    fn return_pooled_fiber(&self, mut fiber: crate::vm::fiber::Fiber) {
        if !self.call_fast {
            return;
        }
        let mut pool = self.fiber_pool.borrow_mut();
        if pool.len() < Self::FIBER_POOL_MAX {
            // Defensive: the fiber should already have an empty frame stack after
            // `RunOutcome::Done` (the root `return_from_frame` pops it). Clear both
            // vecs before parking to release any refs and keep the invariant:
            // pooled fibers hold no live frame or stack data.
            fiber.frames.clear();
            fiber.stack.clear();
            pool.push(fiber);
        }
        // If the pool is full, `fiber` simply drops here.
    }

    /// **DBG.** Build a specializing VM with an instrumentation payload installed
    /// (a debugger/profiler/coverage attach). Mirrors [`Vm::with_specialize`] — it
    /// builds the default VM and then arms `instrument`, so the only difference from
    /// a plain [`Vm::new`] is the `Some(inst)` seam. A run with an all-`None`
    /// [`Instrumentation`](crate::vm::instrument::Instrumentation) is behaviorally
    /// identical to `Vm::new` (the trap arm is unreachable until a byte is patched).
    pub fn with_instrument(
        interp: Rc<Interp>,
        inst: crate::vm::instrument::Instrumentation,
    ) -> Rc<Self> {
        let vm = Self::with_specialize(interp, true);
        *vm.instrument.borrow_mut() = Some(Box::new(inst));
        vm
    }

    /// **DBG.** Whether any instrumentation is currently attached. `false` is the
    /// production path. NOT consulted by `run_loop` (which reaches instrumentation
    /// only via the patched-byte `Op::Break` trap) — exposed for the driver / tests.
    pub fn is_instrumented(&self) -> bool {
        self.instrument.borrow().is_some()
    }

    /// **DBG Task 5b.** Install a [`DebuggerHook`](crate::vm::instrument::DebuggerHook)
    /// as the sole armed sub-feature, replacing any existing instrumentation payload.
    /// The DAP launcher uses this after patching break-on-entry on the hook. Behaves
    /// exactly like setting `instrument = Some(Instrumentation{ breakpoints: Some(hook),
    /// .. })` — kept as a method so the field stays private.
    pub fn install_debugger_hook(&self, hook: crate::vm::instrument::DebuggerHook) {
        *self.instrument.borrow_mut() = Some(Box::new(crate::vm::instrument::Instrumentation {
            breakpoints: Some(hook),
            profiler: None,
            coverage: None,
        }));
    }

    /// **DBG Task 5b.** Reclaim the armed
    /// [`DebuggerHook`](crate::vm::instrument::DebuggerHook) (if any), leaving the VM
    /// with no instrumentation. The DAP launcher calls this after `run` returns to ship
    /// the terminal `Output`/`Terminated` events and then drop the hook (closing the
    /// event channel). `None` if no debugger was armed.
    pub fn take_debugger_hook(&self) -> Option<crate::vm::instrument::DebuggerHook> {
        self.instrument
            .borrow_mut()
            .take()
            .and_then(|mut i| i.breakpoints.take())
    }

    /// **DBG Task 7.** Reclaim the armed
    /// [`ProfilerHook`](crate::vm::instrument::ProfilerHook) (if any). The profiled-run
    /// driver calls this after `run` returns to stop the sampler (joining its thread in
    /// wallclock mode) and aggregate its samples. `None` if no profiler was armed.
    pub fn take_profiler(&self) -> Option<crate::vm::instrument::ProfilerHook> {
        self.instrument
            .borrow_mut()
            .as_mut()
            .and_then(|i| i.profiler.take())
    }

    /// **DX D2 Task 6.** Reclaim the armed
    /// [`CoverageTable`](crate::vm::instrument::CoverageTable) (if any) after a run, so the
    /// caller can report it / merge it across isolates. `None` if no coverage was armed.
    pub fn take_coverage(&self) -> Option<crate::vm::instrument::CoverageTable> {
        self.instrument
            .borrow_mut()
            .as_mut()
            .and_then(|i| i.coverage.take())
    }

    /// **DBG.** Register the program's proto tree for source-line breakpoint resolution.
    /// Walks `entry` and recursively each `chunk.protos`, storing every `Rc<FnProto>`
    /// flat in `debug_protos`. A debugger/launcher calls this once before `run` under
    /// instrumentation (so a parked VM can resolve a `(file, line)` to ANY proto, not
    /// just the current frame). Idempotent-ish: replaces the table each call.
    ///
    /// Zero-cost when off: `debug_protos` is read ONLY inside `debug_stop` (reached only
    /// via a patched `Op::Break`), never on the dispatch hot path.
    pub fn register_debug_protos(&self, entry: &Rc<crate::vm::chunk::FnProto>) {
        fn walk(proto: &Rc<crate::vm::chunk::FnProto>, out: &mut Vec<Rc<crate::vm::chunk::FnProto>>) {
            out.push(proto.clone());
            for nested in &proto.chunk.protos {
                walk(nested, out);
            }
        }
        let mut table = self.debug_protos.borrow_mut();
        table.clear();
        walk(entry, &mut table);
    }

    /// **DX D2 Task 6.** Arm LINE COVERAGE over the program's proto tree. Walks `entry`
    /// and recursively each `chunk.protos` (mirroring [`register_debug_protos`]); for each
    /// proto it builds the `line → first-bytecode-offset` table ([`Chunk::build_line_starts`])
    /// and PATCHES each line's first offset to [`Op::Break`](crate::vm::opcode::Op::Break),
    /// recording in the armed [`CoverageTable`](crate::vm::instrument::CoverageTable) the
    /// displaced byte + line + the proto's source path. The cold `Op::Break` trap arm then
    /// recovers each line on first execution (covered), un-patches, and re-dispatches — so
    /// the hot loop is byte-identical to today (Gate 12) and the program output is unchanged.
    ///
    /// No-op unless a `CoverageTable` is armed in `instrument`. Skips cleanly (never panics)
    /// on a proto with no bound source, an offset past the end, or an empty line table.
    pub fn arm_coverage(&self, entry: &Rc<crate::vm::chunk::FnProto>) {
        // Collect every proto (parent-before-child) into a flat list, like the debugger.
        fn walk(
            proto: &Rc<crate::vm::chunk::FnProto>,
            out: &mut Vec<Rc<crate::vm::chunk::FnProto>>,
        ) {
            out.push(proto.clone());
            for nested in &proto.chunk.protos {
                walk(nested, out);
            }
        }
        let mut protos: Vec<Rc<crate::vm::chunk::FnProto>> = Vec::new();
        walk(entry, &mut protos);

        let mut inst = self.instrument.borrow_mut();
        let Some(table) = inst.as_mut().and_then(|i| i.coverage.as_mut()) else {
            return; // no coverage armed — nothing to do.
        };
        for proto in &protos {
            let chunk = &proto.chunk;
            // The proto's source path (skip a proto with no bound source — it cannot be
            // attributed to a file, so it is simply not instrumented).
            let path = match chunk.source.borrow().as_ref() {
                Some(src) => src.path.clone(),
                None => continue,
            };
            let proto_id = Rc::as_ptr(proto) as *const () as usize;
            table.record_path(proto_id, path);
            let code_len = chunk.code.len();
            for (line, offset) in chunk.build_line_starts() {
                let off = offset as usize;
                if off >= code_len {
                    continue; // malformed/out-of-range offset — skip cleanly.
                }
                // Already patched at this offset (a line table that maps two lines to one
                // offset cannot happen — first-wins — but guard anyway): keep the first.
                if table.trap(proto_id, off).is_some() {
                    continue;
                }
                let original = chunk.code[off];
                // Don't double-patch an existing Op::Break (a debugger breakpoint, etc.).
                if original == crate::vm::opcode::Op::Break as u8 {
                    continue;
                }
                table.record_trap(proto_id, off, original, line);
                chunk.patch_byte(off, crate::vm::opcode::Op::Break as u8);
            }
        }
    }

    /// **DBG.** Resolve a source `(source, line_1based)` to a `(proto_id, offset)` to
    /// patch with a breakpoint, by consulting the registered `debug_protos` tree.
    /// Returns `None` when no proto has any instruction on or after the line (unbound).
    ///
    /// # v1 matching rules (single-module happy path + nested fns)
    ///
    /// - **Source match.** A proto's `chunk.source` carries the module path. If exactly
    ///   one distinct source path is present across all protos, accept it for ANY
    ///   requested `source` (the single-module case — the editor's path and the compiler's
    ///   recorded path need not be byte-equal). Otherwise compare on the file NAME (the
    ///   final path component), so `/abs/foo.as` matches a requested `foo.as`. A proto with
    ///   no bound source is skipped.
    /// - **Proto selection.** Convert to 0-based (`line0`). The right proto is the MOST
    ///   SPECIFIC one whose own `build_line_starts()` has an instruction EXACTLY on
    ///   `line0` (a real instruction starts on that line). Among several such, prefer the
    ///   DEEPEST (last-registered ⇒ most-nested) proto — a nested fn body wins over the
    ///   enclosing body. If NO proto has an exact-line instruction, fall back to the
    ///   FIRST candidate's `first_offset_for_line` (next-line binding — DAP allows a
    ///   breakpoint to bind to a later line).
    fn resolve_line_breakpoint(
        &self,
        source: &str,
        line_1based: u32,
    ) -> Option<(usize, usize)> {
        let line0 = line_1based.saturating_sub(1);
        let protos = self.debug_protos.borrow();

        // Collect the distinct bound source paths to decide the matching mode.
        let mut paths: std::collections::HashSet<String> = std::collections::HashSet::new();
        for p in protos.iter() {
            if let Some(src) = p.chunk.source.borrow().as_ref() {
                paths.insert(src.path.clone());
            }
        }
        let single_source = paths.len() == 1;
        let want_file = file_name_of(source);

        // A proto is a candidate when its bound source matches the requested `source`
        // per the v1 rules above.
        let is_candidate = |p: &Rc<crate::vm::chunk::FnProto>| -> bool {
            match p.chunk.source.borrow().as_ref() {
                None => false,
                Some(src) => {
                    if single_source {
                        true
                    } else {
                        file_name_of(&src.path) == want_file
                    }
                }
            }
        };

        // First pass: a proto whose own line table has an EXACT instruction on `line0`.
        // `debug_protos` is registered parent-before-child (pre-order walk), so a LATER
        // index is a more-nested proto — iterate in reverse to prefer the deepest match.
        for p in protos.iter().rev() {
            if !is_candidate(p) {
                continue;
            }
            let starts = p.chunk.build_line_starts();
            if let Some((_, off)) = starts.iter().find(|(l, _)| *l == line0) {
                let proto_id = Rc::as_ptr(p) as *const () as usize;
                return Some((proto_id, *off as usize));
            }
        }

        // Fallback: next-line binding on the FIRST candidate that has an instruction
        // at/after `line0` (DAP permits binding to a later line). Iterate in registration
        // (pre-)order so the outermost/entry-style proto is preferred for the fallback.
        for p in protos.iter() {
            if !is_candidate(p) {
                continue;
            }
            if let Some(off) = p.chunk.first_offset_for_line(line0) {
                let proto_id = Rc::as_ptr(p) as *const () as usize;
                return Some((proto_id, off as usize));
            }
        }
        None
    }

    /// **DBG.** Find the chunk of a registered proto by its `Rc::as_ptr` identity, so a
    /// parked VM can patch/restore a breakpoint byte through the shared `&Chunk`. Returns
    /// `None` if the id is not in `debug_protos` (a stale/foreign id).
    fn debug_proto_for(&self, proto_id: usize) -> Option<Rc<crate::vm::chunk::FnProto>> {
        self.debug_protos
            .borrow()
            .iter()
            .find(|p| Rc::as_ptr(p) as *const () as usize == proto_id)
            .cloned()
    }

    /// Set the directory used to resolve relative FILE imports from the ENTRY
    /// program (V12-T4). The entry program is not loaded via `load_file_module`, so
    /// its `module_dir` must be seeded here before `run` (e.g. to the `.aso`/`.as`
    /// file's parent directory) so `import ... from "./mod"` resolves correctly.
    pub fn set_module_dir(&self, dir: std::path::PathBuf) {
        *self.module_dir.borrow_mut() = dir;
    }

    /// **ELIDE §4.2/§5.2** — enable contract elision for IMPORTED modules. When on,
    /// `compile_module_file` runs the `ElisionSet` collector + compiles each import
    /// via `compile_source_with_elision`. The default is `false` (the kill-switch
    /// state). The runner sets this to the §5.1 decision value AFTER constructing the
    /// VM (the entry module's elision is applied separately by the runner before the
    /// VM exists). Worker isolates are built fresh and never call this, so worker
    /// slices keep full checks (§4.6).
    pub fn set_elide(&self, on: bool) {
        self.elide.set(on);
    }

    /// Whether the import-loader compiles with contract elision (ELIDE §4.2).
    pub fn elide(&self) -> bool {
        self.elide.get()
    }

    /// **SELF-CONTAINED-BUNDLES Phase 1.** Install an in-memory [`ModuleArchive`] as the
    /// source for relative file imports, and seed the entry's logical dir to the archive
    /// root (`""`). After this, `load_file_module` consults the archive by logical key
    /// BEFORE touching disk, so the program runs with NO source tree present. With no
    /// archive installed (the default) the loader is byte-identical to the disk-only path.
    ///
    /// The caller seeds the entry program's run with the entry CHUNK (the archive's
    /// `modules[entry]` bytes) separately — this only governs how that entry's *imports*
    /// resolve.
    ///
    /// [`ModuleArchive`]: crate::vm::archive::ModuleArchive
    pub fn set_module_archive(&self, archive: Rc<crate::vm::archive::ModuleArchive>) {
        *self.module_archive.borrow_mut() = Some(archive);
        *self.module_logical_dir.borrow_mut() = String::new();
    }

    /// Recover an owned `Rc<Vm>` from `&self`. Used by the async-fn eager-spawn in
    /// the `Op::Call` arm (V7) to hand an owned VM into the `'static` spawned task.
    pub fn rc(&self) -> Rc<Vm> {
        self.self_weak
            .borrow()
            .upgrade()
            .expect("Vm self-ref not installed")
    }

    /// The shared interpreter state.
    pub fn interp(&self) -> &Rc<Interp> {
        &self.interp
    }

    /// Workers Spec A: dispatch a `worker fn` closure to a pooled isolate, returning
    /// the `Value::future`. Builds the shippable code slice — preferring the source
    /// recompile path (via `Interp::worker_source`) when source is available (the normal
    /// run-from-source path, shared with the tree-walker), or falling back to building
    /// the slice directly from the stored pre-compiled top-level chunk (the `.aso`
    /// run path, via `Interp::worker_aso_bytes`) when no source is recorded. The entry name
    /// is the closure's compiled chunk name (a top-level `worker fn`).
    fn dispatch_worker_closure(
        &self,
        callee: &crate::vm::value_ext::Closure,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let entry_name = callee.proto.chunk.name.as_deref().ok_or_else(|| {
            Control::Panic(crate::error::AsError::at(
                "worker fn has no name (internal invariant)".to_string(),
                span,
            ))
        })?;
        // Inline-nesting: a worker fn called from inside an isolate runs locally (no
        // pool round-trip, no slice build) — the entry is already a global on the VM.
        if crate::worker::pool::in_isolate() {
            return crate::worker::dispatch_worker_inline(&self.interp, entry_name, args, span);
        }
        // Route to the static-method or free-function slice builder depending on
        // whether the proto carries an owning class name. A `static worker fn`
        // compiled on a class has `proto.owning_class = Some(class_name)` (set by
        // the compiler when emitting static method protos); a free `worker fn` has
        // `owning_class = None` and goes through the ordinary top-level path.
        let class_name: Option<&str> = callee.proto.owning_class.as_deref();

        // Prefer the source recompile path (produces an identical slice for any run
        // mode that has source). Fall back to the pre-compiled chunk derived from the raw
        // `.aso` bytes stored by `run_aso_file` when no source is recorded (the .aso path).
        let slice = if self.interp.worker_source().is_some() {
            if let Some(cls) = class_name {
                crate::worker::build_code_slice_for_static_method_from_source(
                    &self.interp, cls, entry_name,
                )?
            } else {
                crate::worker::build_code_slice_from_source(&self.interp, entry_name, None)?
            }
        } else if let Some(raw) = self.interp.worker_aso_bytes() {
            let top = crate::vm::chunk::Chunk::from_bytes(&raw).map_err(|e| {
                Control::Panic(crate::error::AsError::at(
                    format!("cannot re-parse .aso for worker dispatch: {e:?}"),
                    span,
                ))
            })?;
            if let Some(cls) = class_name {
                crate::worker::build_code_slice_for_static_method(&top, cls, entry_name)?
            } else {
                crate::worker::build_code_slice(&top, entry_name, None)?
            }
        } else {
            return Err(Control::Panic(crate::error::AsError::at(
                format!(
                    "cannot dispatch worker '{entry_name}': the program source is unavailable \
                     (worker fns require running via `ascript run`)"
                ),
                span,
            )));
        };
        crate::worker::dispatch_worker(&self.interp, slice, args, span)
    }

    /// SP3 §B: increment the SHARED logical call-depth on establishing a new VM
    /// call frame, returning the clean Tier-2 panic if it would exceed
    /// [`crate::interp::MAX_CALL_DEPTH`]. Called at the in-loop `fiber.frames.push`
    /// sites (the frame-Vec call path) — one increment per logical call, matching
    /// the tree-walker's one-per-`run_body`. The matching decrement is in
    /// [`Vm::return_from_frame`] on the non-root pop, so the count tracks the live
    /// frame depth. The counter is `Interp.call_depth` (a `Cell`), never held
    /// across an `.await`.
    fn enter_frame_depth(&self, span: crate::span::Span) -> Result<(), Control> {
        let depth = self.interp.call_depth_cell();
        let next = depth.get() + 1;
        if next > crate::interp::MAX_CALL_DEPTH {
            return Err(Control::Panic(crate::error::AsError::at(
                "maximum recursion depth exceeded",
                span,
            )));
        }
        depth.set(next);
        Ok(())
    }

    /// SP3 §B: decrement the shared logical call-depth when a non-root frame is
    /// popped (the matching dec for [`Vm::enter_frame_depth`]). The ROOT/initial
    /// frame of a fiber is NOT decremented here — its depth unit is owned by the
    /// program root (counter returns to 0 at program end) or by the re-entrant
    /// `self.run`'s RAII [`crate::interp::DepthGuard`] (`invoke_compiled_method` /
    /// `call_value`), so it unwinds exactly once.
    fn leave_frame_depth(&self) {
        let depth = self.interp.call_depth_cell();
        depth.set(depth.get() - 1);
    }

    /// **DBG Task 7 — the CPU-profiler publish seam (zero-cost when off).**
    ///
    /// Publishes the CURRENT frame-name stack (root → leaf) into the armed
    /// profiler hook's `Send` snapshot, so a sampler thread (wallclock mode) or the
    /// inline recorder (deterministic mode) can capture it. Called right AFTER every
    /// frame push and right AFTER every frame pop, so the published stack always
    /// reflects the NEW depth.
    ///
    /// # Zero-cost when no profiler is armed (Gate 12)
    ///
    /// The fast path is a SINGLE `None`-check: if `instrument` is `None`, or its
    /// `profiler` sub-feature is `None`, this returns IMMEDIATELY without touching
    /// `fiber.frames` or allocating anything. The per-instruction dispatch loop is
    /// UNCHANGED — this is only called at the per-CALL push/pop sites (already cold
    /// relative to dispatch), mirroring the placement of `enter_frame_depth` /
    /// `leave_frame_depth`. When no profiler is armed the cost is exactly that one
    /// borrow + two pointer comparisons.
    ///
    /// # The airlock
    ///
    /// Only owned `String`s cross to the sampler thread — each frame maps to its
    /// `proto.debug_name` clone (or `"<anon>"` when unnamed), with the bottom (root)
    /// frame rendered as `"<script>"`. No `Value`/`Rc`/`Cc` ever crosses.
    fn publish_profile_frames(&self, fiber: &Fiber) {
        // FAST PATH (Gate 12): a single None-check, BEFORE building anything.
        {
            let inst = self.instrument.borrow();
            if inst.as_ref().and_then(|i| i.profiler.as_ref()).is_none() {
                return;
            }
        }
        // Armed: build the owned-String stack (root → leaf). Bottom frame = "<script>".
        let mut stack: Vec<String> = Vec::with_capacity(fiber.frames.len());
        for (i, frame) in fiber.frames.iter().enumerate() {
            let name = match &frame.closure.proto.debug_name {
                Some(n) => n.to_string(),
                None if i == 0 => "<script>".to_string(),
                None => "<anon>".to_string(),
            };
            stack.push(name);
        }
        let mut inst = self.instrument.borrow_mut();
        if let Some(hook) = inst.as_mut().and_then(|i| i.profiler.as_mut()) {
            hook.publish(stack);
            // Deterministic mode: each push/pop also records a sample inline, so the
            // sample set is a pure function of call structure (golden-stable).
            if hook.mode == crate::vm::instrument::ProfileMode::Deterministic {
                hook.record_inline_sample();
            }
        }
    }

    /// **LANE Task 3** — shared plain synchronous closure call body.
    ///
    /// Used by both the async `Op::Call` arm (`run_loop`) and (from Task 4 on) the
    /// sync `run_loop_sync` driver, ensuring byte-identical behavior across both lanes.
    ///
    /// Responsibilities (verbatim from the `Op::Call Value::closure` plain arm):
    /// 1. Pops `argc` args from `fiber.stack` (top = last arg) then pops the callee slot.
    /// 2. Runs the SHARED `check_call_args` (arity + per-param contracts + rest
    ///    collection) — a mismatch returns a `Control::Panic` anchored at `call_span`.
    /// 3. Allocates cell slots, places bound params into their slots (cell or plain).
    /// 4. Pushes a new `CallFrame` — ONE `enter_frame_depth` increment (SP3 §B;
    ///    the matching decrement is in `return_from_frame`).
    /// 5. Publishes the updated frame stack to an armed profiler (DBG Task 7; no-op
    ///    zero-cost None-check when no profiler is attached).
    ///
    /// **No await** — this is a synchronous method. The run loop continues in the new
    /// frame immediately after return.
    fn push_closure_frame(
        &self,
        fiber: &mut Fiber,
        callee: Cc<Closure>,
        argc: usize,
        callee_idx: usize,
        call_span: Span,
        // ELIDE §4.4: when true, skip per-param type-contract checks. Only the
        // Op::CallElided dispatch paths pass true; all other callers pass false.
        elide_contracts: bool,
    ) -> Result<(), Control> {
        // `what` mirrors the tree-walker's `func.name.as_deref().unwrap_or("function")`
        // so the wording of arity/contract panics matches byte-for-byte.
        let what = callee.proto.chunk.name.as_deref().unwrap_or("function");
        // Pop the `argc` args into an owned vec (top of stack is the LAST arg), then
        // drop the callee value beneath them.
        let mut args = vec![Value::nil(); argc];
        for slot in args.iter_mut().rev() {
            *slot = fiber.pop();
        }
        fiber.pop(); // the callee value at callee_idx
                     // Arity + per-param contracts + rest collection, shared verbatim with
                     // the tree-walker via `check_call_args`. On a mismatch this returns a
                     // `Control::Panic` carrying the identical message anchored at `call_span`.
        let bound = crate::interp::check_call_args(
            &callee.proto.params,
            args,
            call_span,
            what,
            Some(&self.interp),
            Some(&self.class_env()),
            elide_contracts,
        )?;
        // The args/rest array are gone from the stack; the new frame's window starts
        // where the callee value was.
        let slot_base = callee_idx;
        let slot_count = callee.proto.chunk.slot_count as usize;
        // Allocate cells, then place each bound param into its slot (cell slot → cell;
        // plain slot → stack). Reserve the remaining locals as Nil so the window is full.
        let cells = super::fiber::alloc_cells(slot_count, &callee.proto.chunk.cell_slots);
        fiber.stack.resize(slot_base + slot_count, Value::nil());
        let supplied = bound.supplied;
        for (slot, v) in bound.values.into_iter().enumerate() {
            // CALL §2 A1: cells may be empty (no cell slots); use .get so the
            // empty-vec path is safe.
            if let Some(cell) = cells.get(slot).and_then(|c| c.as_ref()) {
                *cell.borrow_mut() = v;
            } else {
                fiber.stack[slot_base + slot] = v;
            }
        }
        // SP3 §B: one logical-call increment per frame push (matches the tree-walker's
        // one-per-`run_body`); the matching decrement is in `return_from_frame`. Over the
        // limit → the clean Tier-2 panic anchored at the call.
        self.enter_frame_depth(call_span)?;
        fiber.frames.push(super::fiber::CallFrame {
            closure: callee,
            ip: 0,
            slot_base,
            cells,
            ret_span: call_span,
            // A plain in-VM function/closure call is never a method frame; only
            // `invoke_compiled_method` sets a `def_class` (so `super` is unavailable
            // here, which is correct — `super` only appears in method bodies).
            def_class: None,
            argc: supplied,
            defers: Vec::new(),
        });
        // DBG Task 7: publish the new (deeper) frame stack to an armed profiler.
        // Zero-cost None-check when off.
        self.publish_profile_frames(fiber);
        // Continue the run loop in the new frame (the run loop reads `fiber.frame()`
        // at the top of each iteration). RETURN pops this frame and restores the caller.
        Ok(())
    }

    /// Force a full cycle collection (V13-T3). Thin pass-through to
    /// [`crate::gc::collect`] so tests (V13-T4's soundness gate) can deterministically
    /// trigger trial-deletion at a known point and assert a cycle was reclaimed. The
    /// collector is thread-local; the VM runs single-threaded so this collects this
    /// VM's whole `Cc` graph. Returns the number of objects reclaimed.
    pub fn collect(&self) -> usize {
        crate::gc::collect()
    }

    /// The shared `def_env` for VM-created classes (task #157), built lazily as a
    /// single child of `global_env()` and reused for every class. See the
    /// `class_env` field doc for why this mirrors the tree-walker's module env.
    pub(crate) fn class_env(&self) -> crate::env::Environment {
        let mut slot = self.class_env.borrow_mut();
        if slot.is_none() {
            // First build: seed with the module-scope user-globals already defined, so
            // the SHARED `validate_into` (`ClassName.from` / typed-parse) resolves a
            // field default that references a top-level `let`/`const`/`fn`/`class`
            // (e.g. `n: number = LATER` where `const LATER` is a module global) — the
            // VM's construct path reads those via `GET_GLOBAL`, but `.from` evaluates
            // the default through this `def_env`. Kept in sync by `define_user_global`.
            let env = crate::interp::global_env().child();
            for (name, gslot) in self.user_globals.borrow().iter() {
                let _ = env.define(name, gslot.value.clone(), true);
            }
            *slot = Some(env);
        }
        slot.as_ref().unwrap().clone()
    }

    /// Resolve, load, and run a FILE module on the VM, returning its exports map
    /// (V12-T4). Mirrors the tree-walker's `Interp::load_module` + the `.aso`/`.as`
    /// precedence rule:
    ///
    /// - `source` is resolved relative to the CURRENT module's directory
    ///   (`self.module_dir`). The extension defaults to `.as` if absent.
    /// - Both `mod.aso` (compiled) and `mod.as` (source) are considered. The `.aso`
    ///   is PREFERRED when there is no source present OR the `.aso` is at least as new
    ///   as the source (`aso_mtime >= src_mtime`) — Python's rule. Otherwise (source
    ///   newer, or `.aso` absent) the source is compiled fresh. A present-but-stale or
    ///   version-mismatched / unverifiable `.aso` falls back to recompiling the source
    ///   when source is present, else surfaces a clear error.
    /// - The module top-level runs on a fresh fiber with `module_exports` and
    ///   `module_dir` swapped to this module; `Op::DefineExport` collects its exports.
    /// - The result is cached by canonical path; a repeated import reuses it (and a
    ///   circular import resolves to the in-progress entry, populated so far).
    ///
    /// `fault_ip`/`fiber` anchor any error at the importing `Op::Import` site.
    #[async_recursion::async_recursion(?Send)]
    async fn load_file_module(
        &self,
        source: &str,
        fault_ip: usize,
        fiber: &Fiber,
    ) -> Result<ModuleExports, Control> {
        use std::path::PathBuf;

        // SELF-CONTAINED-BUNDLES Phase 1: if an in-memory archive is installed, look the
        // import up FIRST by its machine-independent logical key, computed against the
        // importer's CURRENT logical dir with the SAME `join_logical` the builder used.
        // A hit runs the embedded verified chunk with NO disk access. A miss (no archive,
        // or key absent) falls straight through to the unchanged disk path below.
        //
        // Only RELATIVE imports are archive-resolved here: `load_file_module` is reached
        // for relative file imports (std is linked; package/unknown specifiers are routed
        // earlier), so the importer-relative `join_logical` matches the builder's relative
        // branch. (Package archive keys — a future `pkg/` namespace — are out of Phase 1
        // scope; no resolver ships one yet.)
        // Clone the `Rc<ModuleArchive>` OUT of the cell so no borrow is held across the
        // `.await` below (the `await_holding_refcell_ref` invariant).
        let archive = self.module_archive.borrow().clone();
        if let Some(archive) = archive {
            let logical_dir = self.module_logical_dir.borrow().clone();
            let logical_key = crate::vm::archive::join_logical(&logical_dir, source);
            if let Some(bytes) = archive.get(&logical_key) {
                return self
                    .load_archived_module(bytes, &logical_key, fault_ip, fiber)
                    .await;
            }
        }

        // Resolve the requested module path relative to the importer's dir; default
        // the extension to `.as` (so `./mod` finds `mod.as`/`mod.aso`).
        let requested = self.module_dir.borrow().join(source);
        let stem_path: PathBuf = if requested.extension().is_some() {
            // An explicit `.aso`/`.as` extension — honor it literally.
            requested.clone()
        } else {
            requested.with_extension("as")
        };
        let as_path = stem_path.with_extension("as");
        let aso_path = stem_path.with_extension("aso");

        // Canonical cache key: prefer the source path's canonical form, else the
        // `.aso`'s, else the requested path (so a missing-file error is reported
        // against a stable key and the cache dedups regardless of which file exists).
        let canon = as_path
            .canonicalize()
            .or_else(|_| aso_path.canonicalize())
            .unwrap_or_else(|_| stem_path.clone());

        if let Some(entry) = self.file_modules.borrow().get(&canon) {
            return Ok(entry.clone()); // cached (or in-progress: circular import)
        }

        // Decide whether to load the `.aso` or compile the `.as`, by mtime.
        let src_meta = std::fs::metadata(&as_path).ok();
        let aso_meta = std::fs::metadata(&aso_path).ok();
        let src_mtime = src_meta.as_ref().and_then(|m| m.modified().ok());
        let aso_mtime = aso_meta.as_ref().and_then(|m| m.modified().ok());

        // Prefer `.aso` when present AND (no source, OR aso is at least as new).
        let prefer_aso = aso_meta.is_some()
            && match (aso_mtime, src_mtime) {
                (_, None) => true,            // no source: must use .aso
                (Some(a), Some(s)) => a >= s, // .aso fresh enough
                (None, Some(_)) => false,     // can't read .aso mtime: recompile
            };

        let chunk: crate::vm::chunk::Chunk = if prefer_aso {
            match std::fs::read(&aso_path) {
                Ok(bytes) => match crate::vm::chunk::Chunk::from_bytes_verified(&bytes) {
                    Ok(c) => c,
                    Err(e) => {
                        // Stale/invalid `.aso`: recompile from source if present,
                        // else surface a clear error.
                        if src_meta.is_some() {
                            self.compile_module_file(&as_path, fault_ip, fiber)?
                        } else {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "cannot load compiled module {}: {} (and no source to recompile)",
                                    aso_path.display(),
                                    e
                                ),
                            ));
                        }
                    }
                },
                Err(e) => {
                    return Err(self.panic_at(
                        fiber,
                        fault_ip,
                        format!("cannot read compiled module {}: {}", aso_path.display(), e),
                    ))
                }
            }
        } else if src_meta.is_some() {
            self.compile_module_file(&as_path, fault_ip, fiber)?
        } else {
            return Err(self.panic_at(
                fiber,
                fault_ip,
                format!(
                    "cannot find module '{source}' (looked for {} and {})",
                    as_path.display(),
                    aso_path.display()
                ),
            ));
        };

        // The module's own logical dir governs how ITS transitive imports resolve. On
        // the disk path there is no archive consulted, so the logical dir is inert — but
        // keep it consistent (the canonical-path parent has no archive-relative meaning,
        // so a disk-loaded module resolves its children purely by `module_dir`). The
        // logical-dir swap below is a no-op string move on the pure-disk path.
        let module_dir = canon
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        self.run_module_body(chunk, canon, module_dir, None, fault_ip, fiber)
            .await
    }

    /// SELF-CONTAINED-BUNDLES Phase 1: load + run a module whose VERIFIED chunk bytes
    /// come from the in-memory archive (NO disk file). The trust boundary is identical to
    /// the disk `.aso` path — the bytes go through `from_bytes_verified`, so a corrupt
    /// embedded chunk surfaces the SAME clean error as a corrupt `.aso`.
    ///
    /// Cache + circular-import identity: there is no canonical disk path, so the stable
    /// identity is the module's LOGICAL KEY (unique per archive module). Keying
    /// `file_modules` by a `PathBuf` built from the logical key gives correct once-only
    /// side effects (a re-import returns the cached exports) and cycle termination (a
    /// cycle resolves to the in-progress entry inserted before the body runs) WITHOUT a
    /// real file. The logical key is also swapped in as the module's logical dir base so
    /// ITS nested imports resolve against `logical_parent(logical_key)`.
    #[async_recursion::async_recursion(?Send)]
    async fn load_archived_module(
        &self,
        bytes: &[u8],
        logical_key: &str,
        fault_ip: usize,
        fiber: &Fiber,
    ) -> Result<ModuleExports, Control> {
        use std::path::PathBuf;

        // The cache/circular identity for an archived module: a virtual path derived from
        // its unique logical key (prefixed so it can never collide with a real canonical
        // disk path the disk branch would produce).
        let cache_key = PathBuf::from(format!("<archive>/{logical_key}"));
        if let Some(entry) = self.file_modules.borrow().get(&cache_key) {
            return Ok(entry.clone()); // cached (or in-progress: circular import)
        }

        // SAME trust boundary as the disk `.aso` path: re-verify the embedded chunk. A
        // corrupt embedded chunk → the same clean Tier-2 error a corrupt `.aso` gives.
        let chunk = crate::vm::chunk::Chunk::from_bytes_verified(bytes).map_err(|e| {
            self.panic_at(
                fiber,
                fault_ip,
                format!("cannot load embedded module '{logical_key}': {e}"),
            )
        })?;

        // The embedded module's transitive imports resolve against ITS logical dir.
        let child_logical_dir = crate::vm::archive::logical_parent(logical_key);
        // `module_dir` has no on-disk meaning for an archived module; keep the importer's
        // current `module_dir` for the body run (it is only consulted on the disk
        // fall-through path, which an archive hit never reaches for THIS module's own
        // imports — those resolve via the archive against `child_logical_dir`).
        let module_dir = self.module_dir.borrow().clone();
        self.run_module_body(
            chunk,
            cache_key,
            module_dir,
            Some(child_logical_dir),
            fault_ip,
            fiber,
        )
        .await
    }

    /// The shared body-run tail for BOTH module load paths (disk and archive). Inserts a
    /// fresh in-progress exports map under `cache_key` BEFORE running the body (so a
    /// circular import resolves to it), swaps in this module's `module_exports`,
    /// `module_dir`, AND `module_logical_dir` for the duration of its top-level run, then
    /// restores all three regardless of outcome.
    ///
    /// `target_logical_dir` is `Some(dir)` for an archived module (so its nested archive
    /// imports resolve against its own logical dir) and `None` for a disk module (the
    /// logical dir is inert on the pure-disk path — kept at the importer's current value).
    ///
    /// ASSUMPTION (load-bearing for the archive-installed + archive-MISS → disk fallthrough):
    /// leaving `module_logical_dir` at the importer's value for a disk module is sound ONLY
    /// because a PRODUCED archive is COMPLETE — `compile_archive` walks the whole graph and
    /// errors on any unresolvable import, so every embedded module's relative imports are
    /// archive HITS; the disk fallthrough fires only for sibling-on-disk modules that are
    /// absent from the archive entirely, and THEIR transitive imports likewise miss the
    /// archive and resolve purely by `module_dir` on disk. Phase 2/3 (PARTIAL archives /
    /// `pkg/` keys) MUST revisit: if a disk module's transitive import could legitimately
    /// resolve to an *archived* module, this stale logical dir would compute a wrong key —
    /// at that point the loader must derive the disk module's own logical dir here too.
    #[allow(clippy::too_many_arguments)]
    async fn run_module_body(
        &self,
        chunk: crate::vm::chunk::Chunk,
        cache_key: std::path::PathBuf,
        module_dir: std::path::PathBuf,
        target_logical_dir: Option<String>,
        fault_ip: usize,
        fiber: &Fiber,
    ) -> Result<ModuleExports, Control> {
        // Build a fresh exports map and cache it BEFORE running the body so a
        // circular import resolves to this (in-progress) entry rather than re-running.
        let exports: ModuleExports = Rc::new(RefCell::new(indexmap::IndexMap::new()));
        self.file_modules
            .borrow_mut()
            .insert(cache_key.clone(), exports.clone());

        // Swap in this module's exports + dir (+ logical dir) for the top-level run.
        let prev_exports = self.module_exports.replace(exports.clone());
        let prev_dir = self.module_dir.replace(module_dir);
        // Swap the logical dir in LOCKSTEP with module_dir. For a disk module there is no
        // archive-relative dir, so leave the current value in place (inert) by replacing
        // it with itself; for an archived module swap to its own logical dir.
        let prev_logical_dir = target_logical_dir.map(|d| self.module_logical_dir.replace(d));

        // Run the module's top-level on its own fiber. Build a zero-arg top-level
        // closure exactly like `vm_run_source_with`.
        let proto = Rc::new(crate::vm::chunk::FnProto {
            chunk,
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
            name_span: None,
        });
        let closure = Closure::new(proto);
        let mut module_fiber = Fiber::new(closure);
        let run_result = self.run(&mut module_fiber).await;

        // Restore the importer's exports/dir/logical-dir regardless of outcome.
        self.module_exports.replace(prev_exports);
        self.module_dir.replace(prev_dir);
        if let Some(prev) = prev_logical_dir {
            self.module_logical_dir.replace(prev);
        }

        match run_result {
            Ok(RunOutcome::Done(_)) => Ok(exports),
            Ok(RunOutcome::Yielded(_)) => Err(self.panic_at(
                fiber,
                fault_ip,
                "module top-level unexpectedly yielded".to_string(),
            )),
            Err(c) => {
                // On failure, drop the half-built cache entry so a retry can re-run.
                self.file_modules.borrow_mut().remove(&cache_key);
                Err(c)
            }
        }
    }

    /// Compile a module's `.as` source file to a [`Chunk`], mapping a read or
    /// compile error to a Tier-2 panic anchored at the importing site.
    fn compile_module_file(
        &self,
        as_path: &std::path::Path,
        fault_ip: usize,
        fiber: &Fiber,
    ) -> Result<crate::vm::chunk::Chunk, Control> {
        // RT §2.3(a): the runtime-only build has NO compiler. A bundle whose import
        // misses the embedded archive and finds a sibling `.as` on disk would reach
        // here — refuse loudly (a recoverable Tier-2 panic) BEFORE touching disk. The
        // `.aso` disk fallback in `load_file_module` stays (the verifier IS in the
        // runtime). The non-rt path below is byte-identical to pre-RT.
        #[cfg(ascript_rt)]
        {
            Err(self.panic_at(
                fiber,
                fault_ip,
                format!(
                    "cannot compile module '{}': this runtime has no compiler — the module is not embedded in the bundle (rebuild with the ascript toolchain)",
                    as_path.display()
                ),
            ))
        }
        #[cfg(not(ascript_rt))]
        {
        let src = std::fs::read_to_string(as_path).map_err(|e| {
            self.panic_at(
                fiber,
                fault_ip,
                format!("cannot read module {}: {}", as_path.display(), e),
            )
        })?;
        // ELIDE §4.2: when import-elision is on, run the per-module ElisionSet
        // collector and compile this module with the proven contract checks dropped.
        // Per-module scoping is by construction — the set is computed from THIS
        // module's own source, so cross-module span collisions are impossible (§4.3).
        // Off (the default / kill-switch) → byte-identical to pre-ELIDE.
        let compiled = if self.elide.get() {
            let set = crate::check::infer::elision_proofs(&src);
            crate::compile::compile_source_with_elision(&src, Some(&set))
        } else {
            crate::compile::compile_source(&src)
        };
        let chunk = compiled.map_err(|e| {
            self.panic_at(
                fiber,
                fault_ip,
                format!(
                    "compile error in module {}: {}",
                    as_path.display(),
                    e.message
                ),
            )
        })?;
        // Bind THIS module's source onto its whole proto tree (SP4 §3) so a panic
        // raised in any of its functions — even when invoked from a different
        // module — renders its caret in this module's own file.
        let src_info = Rc::new(crate::error::SourceInfo {
            path: as_path.display().to_string(),
            text: src,
        });
        chunk.set_module_source(&src_info);
        Ok(chunk)
        }
    }

    /// Drive `fiber` until it returns (or panics). V1 runs the synchronous
    /// arithmetic subset only.
    ///
    /// The faulting `ip` is captured *before* advancing past the opcode and its
    /// operands so diagnostics point at the instruction that faulted. The current
    /// chunk is re-borrowed per access (`&fiber.frame().closure.proto.chunk`) and
    /// never held across a suspension point, keeping
    /// `clippy::await_holding_refcell_ref` clean once V7 introduces awaits.
    pub async fn run(&self, fiber: &mut Fiber) -> Result<RunOutcome, Control> {
        let result = self.run_loop(fiber).await;

        // DEFER §3.3: on a panic escaping the run loop, drain the defer lists of
        // ALL live frames (innermost first — top of `fiber.frames` stack first).
        // This mirrors the tree-walker's `run_body` which drains even on Panic.
        // We use mem::take + run_defers per frame so no borrow is held across .await.
        if let Err(Control::Panic(_)) = &result {
            // Check whether any live frame has defers to run.
            let any_defers = fiber.frames.iter().any(|f| !f.defers.is_empty());
            if any_defers {
                // Seed the outcome with the escaping panic so merge_defer_panic
                // applies §3.6 r3 (suppressed note) if any deferred call also panics.
                let panic_err = match result {
                    Err(e) => e,
                    Ok(_) => unreachable!(),
                };
                let mut outcome: Result<Value, Control> = Err(panic_err);
                // Drain from innermost (last) to outermost (first) frame.
                for frame in fiber.frames.iter_mut().rev() {
                    let defers = std::mem::take(&mut frame.defers);
                    if !defers.is_empty() {
                        self.vm_run_defers(defers, &mut outcome).await;
                    }
                }
                return outcome.map(|_| RunOutcome::Done(Value::nil()));
            }
        }

        // SP4 §3: bind the FAULTING frame's module source onto an escaping panic
        // that has a span but no span-source yet. The fault propagates
        // synchronously up this `run` (no `.await` between the raise and here), so
        // `last_fault_source` still holds the chunk source of the frame that
        // faulted — the module the span belongs to. Innermost-wins (a nested
        // `run` already bound it). `None` (e.g. an `.aso` with no source) leaves
        // the error untouched, so the driver's entry-source fallback applies.
        if let Err(Control::Panic(e)) = &result {
            if e.span.is_some() && e.span_source.is_none() {
                if let Some(src) = self.last_fault_source.borrow().clone() {
                    return Err(Control::Panic(
                        e.clone().with_span_source(src),
                    ));
                }
            }
        }
        result
    }

    // ── LANE §2.2 — synchronous dispatch driver ──────────────────────────────
    //
    // `run_loop_sync` / `sync_burst` / `sync_lane_op` are PLAIN (non-async)
    // functions. The compiler enforces this: a plain `fn` cannot contain `.await`.
    //
    // Correctness invariant: every arm in `sync_burst` is a byte-for-byte
    // transcription of the corresponding arm in `run_loop`, delegating to the SAME
    // shared helpers (`eval_binop_adaptive`, `apply_unop`, `materialize_range*`,
    // `panic_at`, …). Where `run_loop` does `self.eval_binop_adaptive(…)` the
    // sync arm does exactly the same call — both produce identical results because
    // they share the helper's implementation.
    //
    // ── DECODE §2.4 — `sync_burst` is generic over the instruction SOURCE ─────
    //
    // The burst loop body (every arm) is THE single source of truth for sync-subset
    // semantics. To execute hot code from a pre-decoded record stream WITHOUT a second
    // transcription of the arms (a drift surface the differential would have to police
    // forever), the prologue mechanics — fetch the op + operand offset, advance the
    // cursor — are extracted behind the [`InstrSource`] trait. Two monomorphizations:
    //
    //   * [`ByteSource`] — today's LANE behavior: decode `code[ip]`, walk by
    //     `operand_width`. Proven behavior-preserving by the full differential before
    //     any record code lands (DECODE Task 4a).
    //   * [`RecordSource`] — read `records[idx]` from a valid [`DecodedChunk`]; skip
    //     the per-instruction `Op::from_u8` decode + width walk (DECODE Task 4b).
    //
    // CANONICAL IP INVARIANT (DECODE §3): `fiber.frame().ip` is ALWAYS the byte ip,
    // updated by EVERY arm exactly as byte dispatch does (jumps write it, escalations
    // restore it). The record cursor `idx` is a burst-local TRANSIENT that the record
    // source resyncs from the canonical ip on any discontinuity (a taken jump, an
    // escalation restore, a frame push/pop) — so the verbatim arm bodies need ZERO
    // edits: they keep reading operands via `chunk.read_*(operand_at)` and writing
    // `fiber.frame_mut().ip`, and the record source merely observes the ip it left.

    /// LANE §2.2: run the sync-lane burst driver.
    ///
    /// Executes the suspension-free opcode subset in a tight loop until either the
    /// fiber finishes or an escalation op is reached. Counters are flushed once per
    /// call (not per instruction) so per-instruction counter traffic is avoided.
    ///
    /// DECODE §2.4: picks the instruction source for the *entry* frame. When
    /// `self.decode` and the frame's chunk has a **valid** decoded stream (§4.2 —
    /// `own_epoch == patch_epoch`), the burst runs on [`RecordSource`]; otherwise it
    /// bumps `decode_warmth`, possibly lazily decodes at the threshold, and runs on
    /// [`ByteSource`]. If a record burst can no longer fetch from records mid-burst
    /// (a frame transition into an un-decoded/stale callee), it falls back to a byte
    /// burst that seamlessly continues from the canonical ip.
    pub(crate) fn run_loop_sync(&self, fiber: &mut Fiber) -> Result<SyncOutcome, Control> {
        let mut retired: u64 = 0;
        let r = self.run_loop_sync_inner(fiber, &mut retired);
        if retired > 0 {
            self.lane_sync_ops.set(self.lane_sync_ops.get() + retired);
            self.lane_bursts.set(self.lane_bursts.get() + 1);
        }
        r
    }

    /// Source selection + the byte-fallback continuation (DECODE §2.4). Flushing
    /// lives in [`run_loop_sync`] so a mid-burst `Err` still records progress.
    fn run_loop_sync_inner(
        &self,
        fiber: &mut Fiber,
        retired: &mut u64,
    ) -> Result<SyncOutcome, Control> {
        // DECODE §2.4 (Task 4b): when decode is enabled and the entry frame has a
        // valid decoded stream (building it lazily at the warmth threshold), run the
        // burst on `RecordSource`. A `None` from `select_record_source` means "run on
        // bytes" (cold / disabled / structural anomaly). A `FellBack` mid-burst means
        // the record source could no longer fetch (a frame transition into an
        // un-decoded callee) — the canonical ip is exact, so the byte burst below
        // continues seamlessly from where records stopped.
        if self.decode {
            if let Some(mut src) = self.select_record_source(fiber) {
                match self.sync_burst(fiber, retired, &mut src)? {
                    BurstExit::Sync(outcome) => return Ok(outcome),
                    BurstExit::FellBack => {}
                }
            }
        }
        // Byte dispatch — LANE's shipped behavior (DECODE Task 4a monomorphization).
        let mut src = ByteSource;
        match self.sync_burst(fiber, retired, &mut src)? {
            BurstExit::Sync(outcome) => Ok(outcome),
            // The byte source never falls back (it is always able to fetch).
            BurstExit::FellBack => unreachable!("ByteSource never returns FellBack"),
        }
    }

    /// DECODE §2.4/§4.2: select a [`RecordSource`] for the entry frame, or `None` to
    /// run on bytes. Bumps `decode_warmth` and lazily decodes at the threshold.
    ///
    /// Validity (§4.2): a cached decoded stream is consulted ONLY when its `own_epoch`
    /// equals the chunk's current `patch_epoch` (a DBG `patch_byte` bumps the epoch —
    /// a stale stream is dropped, never executed). Deps + the §6.6 instrument rule are
    /// trivially satisfied until Task 9; the epoch consult is the live guard here.
    fn select_record_source(&self, fiber: &Fiber) -> Option<RecordSource> {
        let chunk = &fiber.frame().closure.proto.chunk;
        // Consult an already-built stream first (the warm path).
        {
            let slot = chunk.decoded.borrow();
            if let Some(d) = slot.as_ref() {
                // §4.2 validity (own_epoch + deps) — the single SoT in `is_valid`.
                if d.is_valid(chunk) {
                    return RecordSource::at_entry(self, fiber, d.clone());
                }
            }
        }
        // No (or stale) stream. Drop a stale one, then warm + maybe decode.
        {
            let mut slot = chunk.decoded.borrow_mut();
            if slot.as_ref().is_some_and(|d| !d.is_valid(chunk)) {
                *slot = None;
            }
        }
        let warmth = chunk.decode_warmth.get().saturating_add(1);
        chunk.decode_warmth.set(warmth);
        if warmth < self.decode_threshold {
            return None;
        }
        // At/over the threshold: decode now (or permanently-byte on a `None` —
        // a structural anomaly the decoder refused; never retry it).
        // DECODE §5 (Unit B, Task 8): fusion rides the master `decode` switch —
        // build the stream with the peephole active so hot protos retire fused
        // superinstructions. The fused arms are byte-identical to the unfused
        // sequence (the same shared helpers); emptying `FUSION_CANDIDATES` reverts
        // to a 1:1 stream (the §5 sabotage / Unit-D delta measurement).
        let built =
            crate::vm::decode::decode_chunk(chunk, &crate::vm::decode::DecodeCfg::fused());
        match built {
            Some(d) => {
                #[cfg(any(test, feature = "fuzzgen", fuzzing))]
                self.bump_decode_stat(|s| {
                    // Memory accounting (§7.3 gate input): record bytes occupied.
                    s.decoded_bytes = s
                        .decoded_bytes
                        .saturating_add((d.records.len() * std::mem::size_of::<
                            crate::vm::decode::DecodedInstr,
                        >()) as u64);
                });
                *chunk.decoded.borrow_mut() = Some(d.clone());
                RecordSource::at_entry(self, fiber, d)
            }
            None => {
                // Permanently byte: park the warmth at the threshold so we do not
                // re-attempt decode every burst (a sticky "do not decode" marker
                // would be cleaner; pinning warmth is sufficient and side-effect-free).
                chunk.decode_warmth.set(self.decode_threshold);
                None
            }
        }
    }

    /// Inner dispatch loop for the sync lane, generic over the instruction
    /// [`InstrSource`]. Mutates `retired` on each completed op.
    ///
    /// DECODE §2.4: the arm bodies below are byte-for-byte the LANE transcription —
    /// only the per-instruction PROLOGUE (fetch op + operand offset, subset check,
    /// advance the cursor) routes through `source`. Operand reads stay
    /// `chunk.read_*(operand_at)`; jumps and escalation ip-restores write
    /// `fiber.frame_mut().ip` exactly as before (the record source resyncs `idx`
    /// from the canonical ip on the next fetch).
    fn sync_burst<S: InstrSource>(
        &self,
        fiber: &mut Fiber,
        retired: &mut u64,
        source: &mut S,
    ) -> Result<BurstExit, Control> {
        loop {
            // DECODE §2.4: fetch the next instruction (op + byte off + operand off)
            // from the source WITHOUT advancing — the subset check below must be
            // able to escalate with the ip un-advanced. A `None` is the record
            // source signalling it can no longer fetch (frame transition into an
            // un-decoded callee); the burst falls back to byte dispatch.
            let Fetched { op, fault_ip, operand_at, fused } = match source.fetch(self, fiber) {
                Some(f) => f,
                None => return Ok(BurstExit::FellBack),
            };

            // LANE: check subset membership BEFORE advancing ip. If this op is
            // not in the sync subset, return NeedsAsync with ip still at fault_ip
            // so the async driver re-decodes and executes the same byte. (For a
            // fused record `op` is the head component; all shipped candidates'
            // components are in-subset, so this never escalates a fused record —
            // but the check is honored for the head op regardless.)
            if !sync_lane_op(op) {
                return Ok(BurstExit::Sync(SyncOutcome::NeedsAsync));
            }

            // Advance the cursor past the opcode byte and its inline operands —
            // identical canonical-ip arithmetic to run_loop (ip = operand_at +
            // width); the record source also steps its `idx` to the next record.
            source.advance(fiber, op, operand_at);

            // ── DECODE §5 (Unit B): FUSED SUPERINSTRUCTION dispatch ─────────────
            // A fused record runs N components in ONE dispatch. `advance` already
            // stepped the record cursor (idx += 1) but set the canonical ip past
            // only the HEAD op's width; the fused executor finishes the work and
            // sets the ip past ALL components. Each component runs the SAME shared
            // helper its single-op arm calls, at its OWN reconstructed byte offset
            // (so spans / adaptive-cache keys are byte-identical to the unfused
            // sequence). A fused record never escalates (its components are the
            // straight-line subset), so it always falls through to `note_retired`.
            if let Some((kind, packed)) = fused {
                self.exec_fused(fiber, fault_ip, kind, packed)?;
                *retired += 1;
                source.note_retired(self, op);
                #[cfg(any(test, feature = "fuzzgen", fuzzing))]
                self.bump_decode_stat(|s| s.fused_ops = s.fused_ops.saturating_add(1));
                continue;
            }

            match op {
                // ── consts / stack ────────────────────────────────────────────
                Op::Const => {
                    let idx =
                        fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.frame().closure.proto.chunk.consts[idx].clone();
                    fiber.push(v);
                }
                Op::Nil => fiber.push(Value::nil()),
                Op::True => fiber.push(Value::bool_(true)),
                Op::False => fiber.push(Value::bool_(false)),
                Op::Pop => {
                    fiber.pop();
                }
                Op::Dup => {
                    let top = fiber.peek(0).clone();
                    fiber.push(top);
                }
                Op::Swap => {
                    let b = fiber.pop();
                    let a = fiber.pop();
                    fiber.push(b);
                    fiber.push(a);
                }
                Op::Rot3 => {
                    let c = fiber.pop();
                    let b = fiber.pop();
                    let a = fiber.pop();
                    fiber.push(b);
                    fiber.push(c);
                    fiber.push(a);
                }

                // ── binop family (shared eval_binop_adaptive / apply_binop) ──
                Op::Add
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
                | Op::InstanceOf
                | Op::BitAnd
                | Op::BitOr
                | Op::BitXor
                | Op::Shl
                | Op::Shr
                | Op::WrapAdd
                | Op::WrapSub
                | Op::WrapMul
                | Op::Range => {
                    let b = fiber.pop();
                    let a = fiber.pop();
                    let binop = binop_of(op);
                    let v = self.eval_binop_adaptive(fiber, fault_ip, binop, a, b)?;
                    fiber.push(v);
                }

                Op::InstanceOfType => {
                    let idx =
                        fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "INSTANCE_OF_TYPE name is not a string constant: {other:?}"
                                ),
                            ))
                        }
                    };
                    let subject = fiber.pop();
                    let yes = match crate::interp::instanceof_reserved_type(&subject, &name) {
                        Some(b) => b,
                        None => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("INSTANCE_OF_TYPE unknown reserved type name '{name}'"),
                            ))
                        }
                    };
                    fiber.push(Value::bool_(yes));
                }

                // ── unary ops ────────────────────────────────────────────────
                Op::Neg | Op::Not | Op::BitNot => {
                    let a = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let v = crate::interp::apply_unop(unop_of(op), a, span)?;
                    fiber.push(v);
                }

                // ── range ops ────────────────────────────────────────────────
                Op::RangeInclusive => {
                    let b = fiber.pop();
                    let a = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let v = crate::interp::materialize_range(&a, &b, true, span)?;
                    fiber.push(v);
                }

                Op::RangeStepValue => {
                    let flags = fiber.frame().closure.proto.chunk.read_u8(operand_at);
                    let inclusive = (flags & 0b01) != 0;
                    let present = (flags & 0b10) != 0;
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let step = fiber.pop();
                    let hi = fiber.pop();
                    let lo = fiber.pop();
                    let step_arg = if present { Some(&step) } else { None };
                    let v = crate::interp::materialize_range_stepped(
                        &lo, &hi, inclusive, step_arg, span,
                    )?;
                    fiber.push(v);
                }

                Op::RangeResolveStep => {
                    let present =
                        fiber.frame().closure.proto.chunk.read_u8(operand_at) == 1;
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let step = fiber.pop();
                    let hi_v = fiber.peek(0);
                    let hi = match hi_v.as_f64() {
                        Some(n) => n,
                        None => unreachable!(
                            "RANGE_RESOLVE_STEP hi must be a number (CHECK_NUMBERS)"
                        ),
                    };
                    let hi_int = hi_v.is_int_value();
                    let lo_v = fiber.peek(1);
                    let lo = match lo_v.as_f64() {
                        Some(n) => n,
                        None => unreachable!(
                            "RANGE_RESOLVE_STEP lo must be a number (CHECK_NUMBERS)"
                        ),
                    };
                    let lo_int = lo_v.is_int_value();
                    let (step_v, step_int) = if present {
                        match step.as_f64() {
                            Some(s) => (Some(s), step.is_int_value()),
                            None => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    "for-range step must be a number".to_string(),
                                ))
                            }
                        }
                    } else {
                        (None, true)
                    };
                    let resolved = crate::interp::resolve_step(lo, hi, step_v, span)?;
                    let yields_int = lo_int && hi_int && step_int;
                    fiber.push(crate::interp::range_counter_value(resolved, yields_int));
                }

                Op::RangeHasNext => {
                    let inclusive =
                        fiber.frame().closure.proto.chunk.read_u8(operand_at) == 1;
                    let step = fiber.pop();
                    let hi = fiber.pop();
                    let i = fiber.pop();
                    let ok = match (i.as_f64(), hi.as_f64(), step.as_f64()) {
                        (Some(i), Some(hi), Some(step)) => {
                            crate::interp::range_has_next(i, hi, step, inclusive)
                        }
                        _ => unreachable!("RANGE_HAS_NEXT operands must be numbers"),
                    };
                    fiber.push(Value::bool_(ok));
                }

                Op::CheckNumbers => {
                    let end_ok = fiber.peek(0).is_number();
                    let start_ok = fiber.peek(1).is_number();
                    if !(end_ok && start_ok) {
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            "for-range bounds must be numbers".to_string(),
                        ));
                    }
                }

                // ── locals / globals ─────────────────────────────────────────
                Op::GetLocal => {
                    let slot =
                        fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.local(slot).clone();
                    fiber.push(v);
                }
                Op::SetLocal => {
                    let slot =
                        fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.pop();
                    fiber.set_local(slot, v);
                }

                Op::GetGlobal => {
                    // Mirror run_loop's GET_GLOBAL exactly — same cache logic.
                    let idx =
                        fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "GET_GLOBAL operand is not a string constant: {other:?}"
                                ),
                            ))
                        }
                    };
                    let version = self.global_version();
                    let cache = fiber.frame().closure.proto.chunk.global_cache(fault_ip);
                    if self.specialize {
                        if let Some(idx) = cache.get_index(self.struct_gen()) {
                            fiber.push(self.user_global_value_at(idx));
                            *retired += 1;
                            source.note_retired(self, op);
                            continue;
                        }
                    }
                    if let Some(v) = cache.get(version).filter(|_| self.specialize) {
                        fiber.push(v);
                    } else if let Some((idx, v)) = self.get_user_global_full(&name) {
                        if self.specialize {
                            fiber.frame().closure.proto.chunk.set_global_cache(
                                fault_ip,
                                crate::vm::adapt::GlobalCache::index_bound(
                                    idx,
                                    self.struct_gen(),
                                ),
                            );
                        }
                        fiber.push(v);
                    } else if crate::interp::BUILTIN_NAMES.contains(&name.as_ref()) {
                        let v = Value::builtin(name);
                        if self.specialize {
                            fiber.frame().closure.proto.chunk.set_global_cache(
                                fault_ip,
                                crate::vm::adapt::GlobalCache::set(v.clone(), version),
                            );
                        }
                        fiber.push(v);
                    } else {
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            format!("undefined variable '{name}'"),
                        ));
                    }
                }

                Op::DefineGlobal => {
                    let idx =
                        fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let mutable =
                        fiber.frame().closure.proto.chunk.read_u8(operand_at + 2) != 0;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "DEFINE_GLOBAL operand is not a string constant: {other:?}"
                                ),
                            ))
                        }
                    };
                    let v = fiber.pop();
                    if self.user_globals.borrow().contains_key(name.as_ref()) {
                        return Err(Control::Panic(AsError::new(format!(
                            "'{name}' is already defined in this scope"
                        ))));
                    }
                    self.define_user_global(name, v, mutable);
                }

                Op::SetGlobal => {
                    let idx =
                        fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "SET_GLOBAL operand is not a string constant: {other:?}"
                                ),
                            ))
                        }
                    };
                    let v = fiber.peek(0).clone();
                    let cache = fiber.frame().closure.proto.chunk.global_cache(fault_ip);
                    if self.specialize {
                        if let Some(idx) = cache.get_index(self.struct_gen()) {
                            match self.set_user_global_at(idx, v.clone()) {
                                Some(true) => {
                                    *retired += 1;
                                    source.note_retired(self, op);
                                    continue;
                                }
                                Some(false) => {
                                    return Err(self.panic_at(
                                        fiber,
                                        fault_ip,
                                        format!(
                                            "cannot assign to immutable binding '{name}'"
                                        ),
                                    ));
                                }
                                None => {}
                            }
                        }
                    }
                    match self.user_global_mutable(name.as_ref()) {
                        Some(true) => {
                            self.update_user_global(&name, v);
                            if self.specialize {
                                if let Some((idx, _)) =
                                    self.get_user_global_full(name.as_ref())
                                {
                                    fiber.frame().closure.proto.chunk.set_global_cache(
                                        fault_ip,
                                        crate::vm::adapt::GlobalCache::index_bound(
                                            idx,
                                            self.struct_gen(),
                                        ),
                                    );
                                }
                            }
                        }
                        Some(false) => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("cannot assign to immutable binding '{name}'"),
                            ));
                        }
                        None => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("cannot assign to undefined variable '{name}'"),
                            ));
                        }
                    }
                }

                Op::ImmutableError => {
                    let idx =
                        fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "IMMUTABLE_ERROR operand is not a string constant: {other:?}"
                                ),
                            ))
                        }
                    };
                    return Err(self.panic_at(
                        fiber,
                        fault_ip,
                        format!("cannot assign to immutable binding '{name}'"),
                    ));
                }

                // ── jumps ─────────────────────────────────────────────────────
                Op::Jump => {
                    let disp =
                        fiber.frame().closure.proto.chunk.read_i16(operand_at);
                    let base = fiber.frame().ip as isize;
                    fiber.frame_mut().ip = (base + disp as isize) as usize;
                }
                Op::Loop => {
                    let disp =
                        fiber.frame().closure.proto.chunk.read_i16(operand_at);
                    let base = fiber.frame().ip as isize;
                    fiber.frame_mut().ip = (base + disp as isize) as usize;
                }
                Op::JumpIfFalse => {
                    let v = fiber.pop();
                    if !v.is_truthy() {
                        let disp =
                            fiber.frame().closure.proto.chunk.read_i16(operand_at);
                        let base = fiber.frame().ip as isize;
                        fiber.frame_mut().ip = (base + disp as isize) as usize;
                    }
                }
                Op::JumpIfTrue => {
                    let v = fiber.pop();
                    if v.is_truthy() {
                        let disp =
                            fiber.frame().closure.proto.chunk.read_i16(operand_at);
                        let base = fiber.frame().ip as isize;
                        fiber.frame_mut().ip = (base + disp as isize) as usize;
                    }
                }
                Op::JumpIfNotNil => {
                    let v = fiber.pop();
                    if v != Value::nil() {
                        let disp =
                            fiber.frame().closure.proto.chunk.read_i16(operand_at);
                        let base = fiber.frame().ip as isize;
                        fiber.frame_mut().ip = (base + disp as isize) as usize;
                    }
                }

                // ── Template ──────────────────────────────────────────────────
                Op::Template => {
                    let n =
                        fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let mut parts = vec![Value::nil(); n];
                    for slot in parts.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    let mut out = String::new();
                    for v in &parts {
                        out.push_str(&v.to_string());
                    }
                    fiber.push(Value::str(out));
                }

                // ── Return / Propagate / Unwrap / Yield ──────────────────────
                Op::Return => {
                    // DEFER GUARD: if defers are pending, we cannot drain them in the
                    // sync lane (drain requires .await). Restore ip to fault_ip and
                    // escalate to the async driver which will drain + return.
                    if !fiber.frame().defers.is_empty() {
                        fiber.frame_mut().ip = fault_ip;
                        return Ok(BurstExit::Sync(SyncOutcome::NeedsAsync));
                    }
                    let result = fiber.pop();
                    if let Some(outcome) = self.return_from_frame(fiber, result)? {
                        return Ok(BurstExit::Sync(SyncOutcome::Finished(outcome)));
                    }
                    // Non-root frame: return_from_frame pushed the result and popped
                    // the frame; continue the loop in the caller's frame.
                    *retired += 1;
                    source.note_retired(self, op);
                    continue;
                }

                Op::Propagate => {
                    // DEFER GUARD: pending defers require async drain on propagation.
                    let v = fiber.pop();
                    let (value, err) = match v.kind() {
                        ValueKind::Array(a) if a.borrow().len() == 2 => {
                            let b = a.borrow();
                            (b[0].clone(), b[1].clone())
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "the ? operator requires a Result pair [value, err]".to_string(),
                            ))
                        }
                    };
                    if err == Value::nil() {
                        fiber.push(value);
                    } else {
                        // Error path: need to drain defers (possibly async) and then
                        // return. Restore ip so the async driver re-executes the op.
                        // Re-push the pair so the stack is untouched from the caller's
                        // perspective; actually the async path will re-pop it.
                        // The cleanest approach: since we already popped `v`, just
                        // restore ip and re-push v, then let async driver handle it.
                        fiber.push(v);
                        fiber.frame_mut().ip = fault_ip;
                        return Ok(BurstExit::Sync(SyncOutcome::NeedsAsync));
                    }
                }

                Op::Unwrap => {
                    // No defer guard needed: Unwrap never drains defers (it either
                    // pushes the value or raises a recoverable Panic — no unwind).
                    let v = fiber.pop();
                    let (value, err) = match v.kind() {
                        ValueKind::Array(a) if a.borrow().len() == 2 => {
                            let b = a.borrow();
                            (b[0].clone(), b[1].clone())
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "the ! operator requires a Result pair [value, err]".to_string(),
                            ))
                        }
                    };
                    if err == Value::nil() {
                        fiber.push(value);
                    } else {
                        return Err(self.panic_at(fiber, fault_ip, error_message(&err)));
                    }
                }

                Op::Yield => {
                    // Pop the yielded value, suspend the fiber, and return Yielded.
                    // ip is already advanced past this op so the next resume continues
                    // after the yield. The frame stack is left intact in the Fiber.
                    let v = fiber.pop();
                    fiber.state = crate::vm::FiberState::Suspended;
                    return Ok(BurstExit::Sync(SyncOutcome::Finished(crate::vm::value_ext::RunOutcome::Yielded(v))));
                }

                // ── DeferPush / DeferPushMethod ───────────────────────────────
                // These only CAPTURE callee + args onto the frame's defers list —
                // no call, no await. The drain happens at Return/Propagate time
                // (which escalates to async when defers are non-empty).
                Op::DeferPush => {
                    let flags = fiber.frame().closure.proto.chunk.read_u8(operand_at);
                    let argc = fiber.frame().closure.proto.chunk.read_u8(operand_at + 1) as usize;
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let awaited = (flags & 1) != 0;
                    let spread = (flags & 2) != 0;
                    let args: Vec<Value> = if spread {
                        let arr = fiber.pop();
                        match arr.kind() {
                            ValueKind::Array(a) => a.borrow().clone(),
                            _ => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "defer spread requires an array, got {}",
                                        crate::interp::type_name(&arr)
                                    ),
                                ))
                            }
                        }
                    } else {
                        let mut v = vec![Value::nil(); argc];
                        for slot in v.iter_mut().rev() {
                            *slot = fiber.pop();
                        }
                        v
                    };
                    let callee = fiber.pop();
                    fiber.frame_mut().defers.push(crate::interp::DeferEntry {
                        kind: crate::interp::DeferKind::Call { callee },
                        args,
                        awaited,
                        span,
                    });
                    #[cfg(any(test, feature = "fuzzgen", fuzzing))]
                    crate::vm::defer_metrics::defer_metrics::ENTRIES_PUSHED
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }

                Op::DeferPushMethod => {
                    let name_idx =
                        fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let flags = fiber.frame().closure.proto.chunk.read_u8(operand_at + 2);
                    let argc = fiber.frame().closure.proto.chunk.read_u8(operand_at + 3) as usize;
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let awaited = (flags & 1) != 0;
                    let spread = (flags & 2) != 0;
                    let name_const = &fiber.frame().closure.proto.chunk.consts[name_idx];
                    let name = match name_const.kind() {
                        ValueKind::Str(s) => s.clone(),
                        _ => {
                            let ty = crate::interp::type_name(name_const);
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("defer method name must be a string, got {ty}"),
                            ));
                        }
                    };
                    let args: Vec<Value> = if spread {
                        let arr = fiber.pop();
                        match arr.kind() {
                            ValueKind::Array(a) => a.borrow().clone(),
                            _ => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "defer spread requires an array, got {}",
                                        crate::interp::type_name(&arr)
                                    ),
                                ))
                            }
                        }
                    } else {
                        let mut v = vec![Value::nil(); argc];
                        for slot in v.iter_mut().rev() {
                            *slot = fiber.pop();
                        }
                        v
                    };
                    let recv = fiber.pop();
                    // Mirror run_loop: resolve non-hook receivers to BoundMethod now;
                    // hook receivers keep DeferKind::Method so call-site hooks fire at
                    // drain time (which happens async via run_loop's Op::Return path).
                    let kind = if self.interp.member_call_is_hook(&recv, &name) {
                        crate::interp::DeferKind::Method { recv, name }
                    } else {
                        let callee = self.vm_read_member(&recv, &name, span)?;
                        crate::interp::DeferKind::Call { callee }
                    };
                    fiber.frame_mut().defers.push(crate::interp::DeferEntry {
                        kind,
                        args,
                        awaited,
                        span,
                    });
                    #[cfg(any(test, feature = "fuzzgen", fuzzing))]
                    crate::vm::defer_metrics::defer_metrics::ENTRIES_PUSHED
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }

                // ── GetIndex / SetIndex / GetProp / GetPropOpt / SetProp ───────
                Op::GetIndex => {
                    let idx = fiber.pop();
                    let obj = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let v = crate::interp::index_get(&obj, &idx, span, span)?;
                    fiber.push(v);
                }

                Op::SetIndex => {
                    // SHAPE Task 3.1: for Object receivers, bypass the shared
                    // `index_set` (which calls borrow_mut() and panics on slab mode)
                    // and use vm_object_insert directly. Array/error paths are
                    // unchanged. The frozen check and error messages are preserved.
                    let val = fiber.pop();
                    let idx = fiber.pop();
                    let obj = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let v = match obj.kind() {
                        ValueKind::Object(cell) => {
                            // Frozen guard (mirrors index_set's frozen_kind check).
                            if let Some(kind) = crate::value::frozen_kind(&obj) {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("cannot mutate a frozen {kind}"),
                                ));
                            }
                            match idx.kind() {
                                ValueKind::Str(key) => {
                                    let key = key.clone();
                                    self.vm_object_insert(cell, &key, val.clone());
                                    val
                                }
                                _ => {
                                    return Err(self.panic_at(
                                        fiber,
                                        fault_ip,
                                        "object index must be a string".to_string(),
                                    ))
                                }
                            }
                        }
                        _ => crate::interp::index_set(&obj, &idx, val, span, span)
                            .map_err(Control::Panic)?,
                    };
                    fiber.push(v);
                }

                Op::GetProp | Op::GetPropOpt => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("GET_PROP operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let obj = fiber.pop();
                    if op == Op::GetPropOpt && obj == Value::nil() {
                        fiber.push(Value::nil());
                    } else {
                        let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                        let proto = fiber.frame().closure.proto.clone();
                        let cached = if self.specialize {
                            self.ic_get_field(&proto.chunk, fault_ip, &obj, &name)
                        } else {
                            None
                        };
                        let v = match cached {
                            Some(v) => v,
                            None => self.vm_read_member(&obj, &name, span)?,
                        };
                        fiber.push(v);
                    }
                }

                Op::SetProp => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("SET_PROP operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let value = fiber.pop();
                    let obj = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let proto = fiber.frame().closure.proto.clone();
                    let v = self.vm_set_prop(&proto.chunk, fault_ip, &obj, &name, value, span)?;
                    fiber.push(v);
                }

                Op::GetSuper => {
                    // `super.<name>` (V9-T2): resolve `name` starting at the CURRENT
                    // method's DEFINING class's superclass, bound to `self` (slot 0).
                    // Mirrors the tree-walker: `super` is a `Value::super_` whose
                    // `start` is `defining_class.superclass`, and `read_member` on it
                    // finds the method up that chain and produces a BoundMethod on
                    // `self` (which the subsequent CALL invokes). The `defining_class`
                    // we stamp onto the BoundMethod is the ANCESTOR that actually
                    // declared the method, so a NESTED `super` resolves from the right
                    // link too.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("GET_SUPER name is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    // The defining class of the running method (set by
                    // `invoke_compiled_method`). Absent only if `super` somehow
                    // appears outside a method frame — a compiler invariant violation.
                    let def_class = match &fiber.frame().def_class {
                        Some(c) => c.clone(),
                        None => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "'super' used outside of a method".to_string(),
                            ))
                        }
                    };
                    // self = slot 0, read cell-aware (it is a cell slot whenever a
                    // nested closure captured it). CALL §2 A1: cells may be empty
                    // when slot 0 is not captured; use .first so the empty-vec fast
                    // path is safe.
                    let receiver = match fiber.frame().cells.first().and_then(|c| c.as_ref()) {
                        Some(cell) => cell.borrow().clone(),
                        None => fiber.local(0).clone(),
                    };
                    // Resolve up from the DEFINING class's superclass (NOT the
                    // instance's class), matching `SuperRef { start: superclass }`.
                    let start = def_class.superclass.clone();
                    let bound = match start
                        .as_ref()
                        .and_then(|s| self.find_compiled_method(s, &name))
                    {
                        Some((_closure, found_class)) => {
                            Value::bound_method(Rc::new(crate::value::BoundMethod {
                                receiver,
                                method: Rc::new(crate::value::Method {
                                    params: Vec::new(),
                                    ret: None,
                                    body: Vec::new(),
                                    is_async: false,
                                    is_generator: false,
                                    is_worker: false,
                                }),
                                defining_class: found_class,
                                name: name.to_string(),
                            }))
                        }
                        None => {
                            // Mirror the tree-walker's `Value::super_` member-read
                            // error wording (with/without a superclass).
                            let msg = if start.is_some() {
                                format!("no superclass method '{name}'")
                            } else {
                                format!("no superclass method '{name}' (no superclass)")
                            };
                            return Err(Control::Panic(AsError::at(msg, span)));
                        }
                    };
                    fiber.push(bound);
                }

                // ── IterSnapshot ──────────────────────────────────────────────
                Op::IterSnapshot => {
                    let iterable = fiber.pop();
                    let items: Vec<Value> = match iterable.kind() {
                        ValueKind::Array(arr) => arr.borrow().clone(),
                        ValueKind::Str(s) => s
                            .chars()
                            .map(|c| Value::str(c.to_string()))
                            .collect(),
                        ValueKind::Shared(node) => match crate::interp::shared_iter_values(node) {
                            Some(items) => items,
                            None => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "value of type {} is not iterable",
                                        crate::interp::type_name(&iterable)
                                    ),
                                ))
                            }
                        },
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "value of type {} is not iterable",
                                    crate::interp::type_name(&iterable)
                                ),
                            ))
                        }
                    };
                    fiber.push(Value::array(items));
                }

                Op::ArrayLen => {
                    // Pop a (compiler-produced) snapshot array and push its element
                    // count as a `Number`. The operand is never user input — the
                    // compiler emits this only over an `IterSnapshot` result — so a
                    // non-array is a compiler bug surfaced as a Tier-2 panic.
                    let v = fiber.pop();
                    match v.kind() {
                        ValueKind::Array(arr) => {
                            let len = arr.borrow().len();
                            fiber.push(Value::float(len as f64));
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("ARRAY_LEN operand is not an array: {other:?}"),
                            ))
                        }
                    }
                }

                // ── Builders ─────────────────────────────────────────────────
                Op::NewArray => {
                    let n = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let mut values = vec![Value::nil(); n];
                    for slot in values.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    fiber.push(Value::array(values));
                }

                Op::NewObject => {
                    // SHAPE Task 3.1/3.2: shared body for both lanes (see
                    // `exec_new_object`). `fault_ip` is the op offset (cache key +
                    // panic span); the operand at `operand_at` is the pair count.
                    let n = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    self.exec_new_object(fiber, fault_ip, n)?;
                }

                Op::NewMap => {
                    fiber.push(Value::map(indexmap::IndexMap::new()));
                }

                Op::MapEntry => {
                    let val = fiber.pop();
                    let key_val = fiber.pop();
                    let key = match crate::value::MapKey::from_value(&key_val) {
                        Some(k) => k,
                        None => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "cannot use {} as a map key",
                                    crate::interp::type_name(&key_val)
                                ),
                            ))
                        }
                    };
                    match fiber.peek(0).kind() {
                        ValueKind::Map(m) => {
                            m.borrow_mut().insert(key, val);
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "MAP_ENTRY target is not a map: {}",
                                    crate::interp::type_name(fiber.peek(0))
                                ),
                            ))
                        }
                    }
                }

                Op::Spread | Op::SpreadArgs => {
                    let operand = fiber.pop();
                    match operand.kind() {
                        ValueKind::Array(src) => {
                            let items: Vec<Value> = src.borrow().iter().cloned().collect();
                            match fiber.peek(0).kind() {
                                ValueKind::Array(arr) => arr.borrow_mut().extend(items),
                                _ => {
                                    return Err(self.panic_at(
                                        fiber,
                                        fault_ip,
                                        format!(
                                            "SPREAD target is not an array: {}",
                                            crate::interp::type_name(fiber.peek(0))
                                        ),
                                    ))
                                }
                            }
                        }
                        _ => {
                            let msg = if matches!(op, Op::SpreadArgs) {
                                format!(
                                    "can only spread an array as call arguments, got {}",
                                    crate::interp::type_name(&operand)
                                )
                            } else {
                                format!(
                                    "can only spread an array into an array, got {}",
                                    crate::interp::type_name(&operand)
                                )
                            };
                            return Err(self.panic_at(fiber, fault_ip, msg));
                        }
                    }
                }

                Op::AppendArray => {
                    let item = fiber.pop();
                    match fiber.peek(0).kind() {
                        ValueKind::Array(arr) => arr.borrow_mut().push(item),
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "APPEND_ARRAY target is not an array: {}",
                                    crate::interp::type_name(fiber.peek(0))
                                ),
                            ))
                        }
                    }
                }

                Op::AppendObject => {
                    // SHAPE Task 3.1: use vm_object_insert so slab-mode builder
                    // objects grow via precise registry transitions instead of
                    // dict borrow_mut + resync. The key must be a string const.
                    let val = fiber.pop();
                    let key = match fiber.pop().into_kind() {
                        OwnedKind::Str(s) => s,
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("APPEND_OBJECT key is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    match fiber.peek(0).kind() {
                        ValueKind::Object(obj) => {
                            let obj = obj.clone();
                            self.vm_object_insert(&obj, &key, val);
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "APPEND_OBJECT target is not an object: {}",
                                    crate::interp::type_name(fiber.peek(0))
                                ),
                            ))
                        }
                    }
                }

                Op::SpreadObject => {
                    // SHAPE Task 3.1: snapshot source entries via the accessor
                    // (works across slab and dict modes), then insert into the
                    // builder object via vm_object_insert (precise transitions).
                    let operand = fiber.pop();
                    match operand.kind() {
                        ValueKind::Object(src) => {
                            // Snapshot FIRST (avoids borrow conflict on self-spread).
                            let entries = src.entries();
                            match fiber.peek(0).kind() {
                                ValueKind::Object(obj) => {
                                    let obj = obj.clone();
                                    for (k, v) in entries {
                                        self.vm_object_insert(&obj, &k, v);
                                    }
                                }
                                _ => {
                                    return Err(self.panic_at(
                                        fiber,
                                        fault_ip,
                                        format!(
                                            "SPREAD_OBJECT target is not an object: {}",
                                            crate::interp::type_name(fiber.peek(0))
                                        ),
                                    ))
                                }
                            }
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "can only spread an object into an object, got {}",
                                    crate::interp::type_name(&operand)
                                ),
                            ))
                        }
                    }
                }

                Op::AppendNamedArg => {
                    // ADT §3.2 (spread+named lockstep builder). Stack
                    // `[..., argsArray, namesArray, value]`: pop `value`, push it onto
                    // `argsArray` (peek 1) and push the field name `consts[idx]` (a
                    // `Str`) onto `namesArray` (peek 0).
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => Value::str(s.clone()),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("APPEND_NAMED_ARG name operand is not a string: {other:?}"),
                            ))
                        }
                    };
                    let value = fiber.pop();
                    match (fiber.peek(1).kind(), fiber.peek(0).kind()) {
                        (ValueKind::Array(args), ValueKind::Array(names)) => {
                            args.borrow_mut().push(value);
                            names.borrow_mut().push(name);
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "APPEND_NAMED_ARG builder targets are not both arrays".to_string(),
                            ))
                        }
                    }
                }

                Op::AppendPosArg => {
                    // ADT §3.2 (spread+named lockstep builder). Stack
                    // `[..., argsArray, namesArray, value]`: pop `value`, push it onto
                    // `argsArray` and push `Nil` onto `namesArray` (a positional value).
                    let value = fiber.pop();
                    match (fiber.peek(1).kind(), fiber.peek(0).kind()) {
                        (ValueKind::Array(args), ValueKind::Array(names)) => {
                            args.borrow_mut().push(value);
                            names.borrow_mut().push(Value::nil());
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "APPEND_POS_ARG builder targets are not both arrays".to_string(),
                            ))
                        }
                    }
                }

                Op::AppendSpreadArg => {
                    // ADT §3.2 (spread+named lockstep builder). Stack
                    // `[..., argsArray, namesArray, operand]`: pop `operand` (MUST be an
                    // Array), extend `argsArray` with its elements and push `Nil` ONCE
                    // PER element onto `namesArray`.
                    let operand = fiber.pop();
                    let items: Vec<Value> = match operand.kind() {
                        ValueKind::Array(src) => src.borrow().iter().cloned().collect(),
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "can only spread an array as call arguments, got {}",
                                    crate::interp::type_name(&operand)
                                ),
                            ))
                        }
                    };
                    let n = items.len();
                    match (fiber.peek(1).kind(), fiber.peek(0).kind()) {
                        (ValueKind::Array(args), ValueKind::Array(names)) => {
                            args.borrow_mut().extend(items);
                            names.borrow_mut().extend(std::iter::repeat_n(Value::nil(), n));
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "APPEND_SPREAD_ARG builder targets are not both arrays".to_string(),
                            ))
                        }
                    }
                }

                // ── Destructure / match family ────────────────────────────────
                Op::CheckArrayDestructure => {
                    if !matches!(fiber.peek(0).kind(), ValueKind::Array(_)) {
                        let t = crate::interp::type_name(fiber.peek(0));
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            format!("cannot destructure a non-array value of type {t}"),
                        ));
                    }
                }

                Op::CheckObjectDestructure => {
                    if !matches!(fiber.peek(0).kind(), ValueKind::Object(_) | ValueKind::Instance(_)) {
                        let t = crate::interp::type_name(fiber.peek(0));
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            format!("cannot destructure a non-object value of type {t}"),
                        ));
                    }
                }

                Op::ArrayElem => {
                    let index = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let src = fiber.pop();
                    match src.kind() {
                        ValueKind::Array(arr) => {
                            let v = arr.borrow().get(index).cloned().unwrap_or(Value::nil());
                            fiber.push(v);
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("ARRAY_ELEM operand is not an array: {other:?}"),
                            ))
                        }
                    }
                }

                Op::ObjectKey => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let key = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("OBJECT_KEY operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let src = fiber.pop();
                    let v = match src.kind() {
                        ValueKind::Object(o) => {
                            o.get(key.as_ref()).unwrap_or(Value::nil())
                        }
                        ValueKind::Instance(i) => i
                            .borrow()
                            .get(key.as_ref())
                            .unwrap_or(Value::nil()),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("OBJECT_KEY operand is not an object: {other:?}"),
                            ))
                        }
                    };
                    fiber.push(v);
                }

                Op::ArrayRest => {
                    let start = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let src = fiber.pop();
                    match src.kind() {
                        ValueKind::Array(arr) => {
                            let tail: Vec<Value> =
                                arr.borrow().iter().skip(start).cloned().collect();
                            fiber.push(Value::array(tail));
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("ARRAY_REST operand is not an array: {other:?}"),
                            ))
                        }
                    }
                }

                Op::ObjectRest => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let bound: std::collections::HashSet<Rc<str>> =
                        match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                            ValueKind::Array(keys) => keys
                                .borrow()
                                .iter()
                                .filter_map(|v| match v.kind() {
                                    ValueKind::Str(s) => Some(s.clone()),
                                    _ => None,
                                })
                                .collect(),
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "OBJECT_REST operand is not a key array: {other:?}"
                                    ),
                                ))
                            }
                        };
                    let src = fiber.pop();
                    let mut remaining: indexmap::IndexMap<String, Value> =
                        indexmap::IndexMap::new();
                    match src.kind() {
                        ValueKind::Object(o) => {
                            for (k, v) in o.entries() {
                                if !bound.contains(k.as_ref()) {
                                    remaining.insert(k.to_string(), v);
                                }
                            }
                        }
                        ValueKind::Instance(i) => {
                            for (k, v) in i.borrow().entries() {
                                if !bound.contains(k.as_ref()) {
                                    remaining.insert(k.to_string(), v);
                                }
                            }
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("OBJECT_REST operand is not an object: {other:?}"),
                            ))
                        }
                    }
                    fiber.push(Value::object(remaining));
                    // SHAPE §3.5: count this fresh-dict OBJECT_REST build.
                    #[cfg(any(test, feature = "fuzzgen", fuzzing))]
                    self.bump_shape_stat(|s| s.obj_dict_constructed += 1);
                }

                Op::MatchArray => {
                    let len = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let exact = fiber.frame().closure.proto.chunk.read_u8(operand_at + 2) == 1;
                    let subject = fiber.pop();
                    let ok = match subject.kind() {
                        ValueKind::Array(a) => {
                            let n = a.borrow().len();
                            if exact { n == len } else { n >= len }
                        }
                        _ => false,
                    };
                    fiber.push(Value::bool_(ok));
                }

                Op::MatchObject => {
                    let subject = fiber.pop();
                    let ok = matches!(subject.kind(), ValueKind::Object(_) | ValueKind::Instance(_));
                    fiber.push(Value::bool_(ok));
                }

                Op::MatchHasKey => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let key = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "MATCH_HAS_KEY operand is not a string constant: {other:?}"
                                ),
                            ))
                        }
                    };
                    let subject = fiber.pop();
                    let ok = match subject.kind() {
                        ValueKind::Object(o) => o.contains_key(key.as_ref()),
                        ValueKind::Instance(i) => i.borrow().contains_key(key.as_ref()),
                        _ => false,
                    };
                    fiber.push(Value::bool_(ok));
                }

                Op::MatchVariant => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let (want_variant, want_enum) =
                        match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                            ValueKind::Array(a) => {
                                let b = a.borrow();
                                let v = match b.first().map(Value::kind) {
                                    Some(ValueKind::Str(s)) => s.clone(),
                                    _ => {
                                        return Err(self.panic_at(
                                            fiber,
                                            fault_ip,
                                            "MATCH_VARIANT operand[0] is not a string"
                                                .to_string(),
                                        ))
                                    }
                                };
                                let e = match b.get(1).map(Value::kind) {
                                    Some(ValueKind::Str(s)) => Some(s.clone()),
                                    _ => None,
                                };
                                (v, e)
                            }
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "MATCH_VARIANT operand is not an array: {other:?}"
                                    ),
                                ))
                            }
                        };
                    let subject = fiber.pop();
                    let ok = match subject.kind() {
                        ValueKind::EnumVariant(ev) if ev.payload.is_some() => {
                            ev.name.as_str() == want_variant.as_ref()
                                && want_enum
                                    .as_ref()
                                    .map(|e| ev.enum_name.as_str() == e.as_ref())
                                    .unwrap_or(true)
                        }
                        _ => false,
                    };
                    fiber.push(Value::bool_(ok));
                }

                Op::MatchVariantArity => {
                    let n = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let subject = fiber.pop();
                    let len = variant_payload_len(&subject);
                    fiber.push(Value::bool_(len == Some(n)));
                }

                Op::MatchVariantHasField => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let key = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "MATCH_VARIANT_HAS_FIELD operand is not a string: {other:?}"
                                ),
                            ))
                        }
                    };
                    let subject = fiber.pop();
                    let ok = match subject.kind() {
                        ValueKind::EnumVariant(ev) => match &ev.payload {
                            Some(crate::value::Payload::Named(o)) => {
                                o.contains_key(key.as_ref())
                            }
                            _ => false,
                        },
                        _ => false,
                    };
                    fiber.push(Value::bool_(ok));
                }

                Op::VariantElem => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let subject = fiber.pop();
                    let v = match subject.kind() {
                        ValueKind::EnumVariant(ev) => match &ev.payload {
                            Some(crate::value::Payload::Positional(a)) => {
                                a.borrow().get(idx).cloned().unwrap_or(Value::nil())
                            }
                            Some(crate::value::Payload::Named(o)) => o
                                .borrow()
                                .get_index(idx)
                                .map(|(_, v)| v.clone())
                                .unwrap_or(Value::nil()),
                            None => Value::nil(),
                        },
                        _ => Value::nil(),
                    };
                    fiber.push(v);
                }

                Op::VariantField => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let key = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("VARIANT_FIELD operand is not a string: {other:?}"),
                            ))
                        }
                    };
                    let subject = fiber.pop();
                    let v = match subject.kind() {
                        ValueKind::EnumVariant(ev) => match &ev.payload {
                            Some(crate::value::Payload::Named(o)) => {
                                o.get(key.as_ref()).unwrap_or(Value::nil())
                            }
                            _ => Value::nil(),
                        },
                        _ => Value::nil(),
                    };
                    fiber.push(v);
                }

                Op::MatchRange => {
                    let flags = fiber.frame().closure.proto.chunk.read_u8(operand_at);
                    let inclusive = (flags & 0b01) != 0;
                    let present = (flags & 0b10) != 0;
                    let step = fiber.pop();
                    let hi = fiber.pop();
                    let lo = fiber.pop();
                    let subject = fiber.pop();
                    let ok = match (subject.as_f64(), lo.as_f64(), hi.as_f64()) {
                        (Some(n), Some(lo), Some(hi)) => {
                            let step_v = if present {
                                match step.as_f64() {
                                    Some(s) => Some(s),
                                    None => {
                                        return Err(self.panic_at(
                                            fiber,
                                            fault_ip,
                                            "range step must be a number".to_string(),
                                        ))
                                    }
                                }
                            } else {
                                None
                            };
                            if step_v.is_some() {
                                let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                                crate::interp::resolve_step(lo, hi, step_v, span)?;
                            }
                            crate::interp::range_pattern_contains(n, lo, hi, step_v, inclusive)
                        }
                        _ => false,
                    };
                    fiber.push(Value::bool_(ok));
                }

                Op::MatchNoArm => {
                    return Err(self.panic_at(
                        fiber,
                        fault_ip,
                        "no matching arm in match expression".to_string(),
                    ));
                }

                // ── Cells / upvalues / param-prologue ────────────────────────
                Op::GetLocalCell => {
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.get_local_cell(slot);
                    fiber.push(v);
                }
                Op::SetLocalCell => {
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.pop();
                    fiber.set_local_cell(slot, v);
                }
                Op::FreshCell => {
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    fiber.fresh_cell(slot);
                }

                Op::GetUpvalue => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.frame().closure.upvalues[idx].borrow().clone();
                    fiber.push(v);
                }
                Op::SetUpvalue => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.pop();
                    *fiber.frame().closure.upvalues[idx].borrow_mut() = v;
                }

                Op::CheckParam => {
                    let param = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let span = fiber.frame().ret_span;
                    let ty = fiber.frame().closure.proto.params[param].ty.clone();
                    if let Some(ty) = ty {
                        let v = fiber.peek(0).clone();
                        if !crate::interp::check_type(&v, &ty) {
                            // §6.3 paranoid: the call-site span is in `calls` set.
                            if let Some(e) = self.interp.maybe_paranoid_escalate(&ty, &v, span) {
                                return Err(e);
                            }
                            return Err(crate::interp::contract_panic(&ty, &v, span));
                        }
                    }
                }

                Op::CheckLocal => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let ty = match fiber.frame().closure.proto.chunk.type_consts.get(idx) {
                        Some(ty) => ty.clone(),
                        None => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("CHECK_LOCAL type-const index {idx} out of range"),
                            ))
                        }
                    };
                    let v = fiber.peek(0).clone();
                    if !crate::interp::check_type(&v, &ty) {
                        // §6.3 paranoid: the let initializer span is in `lets` set.
                        if let Some(e) = self.interp.maybe_paranoid_escalate(&ty, &v, span) {
                            return Err(e);
                        }
                        return Err(crate::interp::contract_panic(&ty, &v, span));
                    }
                }

                Op::JumpIfArgSupplied => {
                    let chunk = &fiber.frame().closure.proto.chunk;
                    let param = chunk.read_u16(operand_at) as usize;
                    let disp = chunk.read_i16(operand_at + 2);
                    if fiber.frame().argc > param {
                        let base = fiber.frame().ip as isize;
                        fiber.frame_mut().ip = (base + disp as isize) as usize;
                    }
                }

                // ── Closure ──────────────────────────────────────────────────
                Op::Closure => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let proto = fiber.frame().closure.proto.chunk.protos[idx].clone();
                    let mut upvalues = Vec::with_capacity(proto.chunk.upvalues.len());
                    for desc in &proto.chunk.upvalues {
                        let cell = match *desc {
                            crate::syntax::resolve::types::UpvalueDescriptor::ParentLocal {
                                slot,
                                by_value: false,
                            } => fiber
                                .frame()
                                .cells
                                .get(slot as usize)
                                .and_then(|c| c.as_ref())
                                .unwrap_or_else(|| {
                                    panic!(
                                        "CLOSURE captures parent local slot {slot} that is not a cell (compiler/resolver bug)"
                                    )
                                })
                                .clone(),
                            crate::syntax::resolve::types::UpvalueDescriptor::ParentLocal {
                                slot,
                                by_value: true,
                            } => {
                                let v = fiber.local(slot as usize).clone();
                                gcmodule::Cc::new(std::cell::RefCell::new(v))
                            }
                            crate::syntax::resolve::types::UpvalueDescriptor::ParentUpvalue(up) => {
                                fiber.frame().closure.upvalues[up as usize].clone()
                            }
                        };
                        upvalues.push(cell);
                    }
                    let closure = crate::vm::value_ext::Closure::with_upvalues(proto, upvalues);
                    fiber.push(Value::closure(closure));
                }

                // ── Call / CallElided / CallSpread (plain sync closure only) ─────
                Op::Call | Op::CallElided | Op::CallSpread => {
                    // For CallSpread: peek at the callee before popping the args array.
                    // For Call / CallElided: peek at the callee using the static argc operand.
                    // If the callee is a plain (non-async, non-worker, non-generator)
                    // closure: push a new CallFrame (sync, no await) and continue.
                    // Any other callee kind: restore ip to fault_ip and escalate to async.
                    //
                    // ELIDE §4.4: CallElided is semantically identical to Call here; the
                    // elide flag threads into the binder to skip per-param type-contract
                    // checks at statically-proven sites (Task 2.2).
                    //
                    // INVARIANT: we must NOT pop anything from the stack before
                    // confirming the callee is sync-eligible, so that escalation
                    // leaves the stack completely untouched.
                    // ELIDE §4.4: CallElided dispatch sets elide=true so the shared binder
                    // skips per-param type-contract checks at this site.
                    let elide = matches!(op, Op::CallElided);
                    if matches!(op, Op::CallSpread) {
                        // Stack is `[..., callee, argsArray]`. Peek callee at index -2.
                        let callee_ref = &fiber.stack[fiber.stack.len() - 2];
                        let is_sync_closure = match callee_ref.kind() {
                            ValueKind::Closure(c) => {
                                !c.proto.is_async && !c.proto.is_worker && !c.proto.is_generator
                            }
                            _ => false,
                        };
                        if !is_sync_closure {
                            fiber.frame_mut().ip = fault_ip;
                            return Ok(BurstExit::Sync(SyncOutcome::NeedsAsync));
                        }
                        // Pop the args array and flatten it, then dispatch.
                        let args_arr = fiber.pop();
                        let args_ty = crate::interp::type_name(&args_arr);
                        let args = match args_arr.into_kind() {
                            OwnedKind::Array(a) => a,
                            _ => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("CALL_SPREAD args are not an array: {args_ty}"),
                                ))
                            }
                        };
                        let items: Vec<Value> = args.borrow().iter().cloned().collect();
                        let argc = items.len();
                        for v in items {
                            fiber.push(v);
                        }
                        let callee_idx = fiber.stack.len() - argc - 1;
                        let callee = match fiber.stack[callee_idx].clone().into_kind() {
                            OwnedKind::Closure(c) => c,
                            _ => unreachable!("already checked above"),
                        };
                        let call_span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                        // CallSpread always goes through push_closure_frame (args were
                        // already flattened onto the stack, so in-place applies, but
                        // CallSpread is rare enough and rest-eligible that we keep the
                        // simpler path for clarity; the A2 fast path targets the common
                        // Op::Call shape where args are already individually on the stack).
                        self.push_closure_frame(fiber, callee, argc, callee_idx, call_span, false)?;
                    } else {
                        // Op::Call: argc is the static operand.
                        let argc =
                            fiber.frame().closure.proto.chunk.read_u8(operand_at) as usize;
                        let callee_idx = fiber.stack.len() - argc - 1;
                        let callee_ref = &fiber.stack[callee_idx];
                        let is_sync_closure = match callee_ref.kind() {
                            ValueKind::Closure(c) => {
                                !c.proto.is_async && !c.proto.is_worker && !c.proto.is_generator
                            }
                            _ => false,
                        };
                        if !is_sync_closure {
                            // Restore ip (already advanced) before escalating.
                            fiber.frame_mut().ip = fault_ip;
                            return Ok(BurstExit::Sync(SyncOutcome::NeedsAsync));
                        }
                        let callee = match fiber.stack[callee_idx].clone().into_kind() {
                            OwnedKind::Closure(c) => c,
                            _ => unreachable!("already checked above"),
                        };
                        let call_span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                        // CALL §3 (A2): in-place arg binding fast path in the sync lane.
                        // Same qualification and mechanism as the async arm above.
                        if self.call_fast && !callee.proto.has_rest {
                            let supplied = crate::interp::check_call_args_in_place(
                                &callee.proto.params,
                                &fiber.stack[callee_idx + 1..callee_idx + 1 + argc],
                                call_span,
                                callee.proto.chunk.name.as_deref().unwrap_or("function"),
                                Some(&self.interp),
                                Some(&self.class_env()),
                                elide,
                            )?;
                            fiber.stack.remove(callee_idx);
                            let slot_base = callee_idx;
                            let slot_count = callee.proto.chunk.slot_count as usize;
                            let cells = super::fiber::alloc_cells(
                                slot_count,
                                &callee.proto.chunk.cell_slots,
                            );
                            fiber.stack.resize(slot_base + slot_count, Value::nil());
                            if !cells.is_empty() {
                                for slot in 0..supplied {
                                    if let Some(cell) =
                                        cells.get(slot).and_then(|c| c.as_ref())
                                    {
                                        *cell.borrow_mut() = std::mem::replace(
                                            &mut fiber.stack[slot_base + slot],
                                            Value::nil(),
                                        );
                                    }
                                }
                            }
                            self.bump_stat(|s| s.inplace_binds += 1);
                            self.enter_frame_depth(call_span)?;
                            fiber.frames.push(super::fiber::CallFrame {
                                closure: callee,
                                ip: 0,
                                slot_base,
                                cells,
                                ret_span: call_span,
                                def_class: None,
                                argc: supplied,
                                defers: Vec::new(),
                            });
                            self.publish_profile_frames(fiber);
                        } else {
                            self.push_closure_frame(fiber, callee, argc, callee_idx, call_span, elide)?;
                        }
                    }
                    // Continue the loop in the new (or returned-to) frame.
                    *retired += 1;
                    source.note_retired(self, op);
                    continue;
                }

                // ── LANE §4: inline ready-future completion ──────────────────
                //
                // Peek-first to decide whether to stay in-lane or escalate:
                //
                //   1. Non-Future TOS → identity (await 5 == 5): pop + push back,
                //      retire. Mirrors run_loop's `other => fiber.push(other)`.
                //   2. Future with try_get() == Some(Ok(v)) → pop future, push v,
                //      retire (the inline take).
                //   3. Future with try_get() == Some(Err(c)) → pop future, return
                //      Err(c): the stored Control surfaces with the IDENTICAL value
                //      the async arm's `f.get().await?` would re-raise.
                //   4. Future with try_get() == None (still pending) → un-advance ip
                //      and return NeedsAsync: the async driver re-decodes Op::Await,
                //      parks on f.get().await, and wakes when the future resolves.
                //
                // CRITICAL: no borrow is held while pushing to the fiber. We read
                // `try_get` (which borrows the cell, then drops the borrow inside
                // the call), clone the result out, then do the pop+push with NO
                // borrow held. `try_get` is a plain synchronous call — ZERO awaits.
                Op::Await => {
                    // Peek TOS read-only before deciding whether to pop.
                    let probe = match fiber.peek(0).kind() {
                        ValueKind::Future(f) => {
                            // try_get clones the result out; the slot borrow is
                            // dropped before try_get returns — no borrow held.
                            f.try_get()
                        }
                        _ => {
                            // Case 1: non-Future → identity. Pop and push back.
                            // ip is already advanced.
                            let v = fiber.pop();
                            fiber.push(v);
                            // retire += 1 falls through at the end of the match.
                            *retired += 1;
                            source.note_retired(self, op);
                            continue;
                        }
                    };
                    match probe {
                        None => {
                            // Case 4: future still pending — escalate.
                            // ip was already advanced above; restore it so the
                            // async driver re-decodes the same Op::Await byte.
                            fiber.frame_mut().ip = fault_ip;
                            return Ok(BurstExit::Sync(SyncOutcome::NeedsAsync));
                        }
                        Some(r) => {
                            // Cases 2 and 3: future already resolved.
                            // Pop the Future handle (one Rc decrement, no side
                            // effect on cancel-on-drop — the future is resolved).
                            fiber.pop();
                            // Re-raise stored Control (Error branch → Err(c))
                            // or push the value (Ok branch). `?` propagates the
                            // same Control the async arm's `f.get().await?` would.
                            let v = r?;
                            fiber.push(v);
                            // retire += 1 falls through at the end of the match.
                        }
                    }
                }

                _ => unreachable!(
                    "sync_lane_op admitted an unimplemented op {op:?}"
                ),
            }
            *retired += 1;
            // DECODE §8.3: a record fully retired (anti-false-green coverage). The
            // record source bumps `decoded_ops`/`stack_ops`; the byte source is a
            // no-op. Bounded by the Gate-17 zero-cost bench; compiled only under
            // test/fuzzgen/fuzzing.
            source.note_retired(self, op);
        }
    }

    /// The instruction-dispatch loop. Wrapped by [`run`] which binds the faulting
    /// module's source onto an escaping panic (SP4 §3 cross-module provenance).
    ///
    /// DBG (debug-info, §5.1) ZERO-COST INVARIANT: the debug-info tables
    /// `Chunk::build_line_starts`/`line_col_at`/`first_offset_for_line` and
    /// `FnProto.local_names` are PURE compile-time metadata — they are NEVER consulted
    /// anywhere in this loop (an attached debugger reads them out-of-band). A grep for
    /// `line_starts`/`local_names` in this file finds only the empty-`Vec::new()`
    /// construction of runtime-built protos, never a read. Keep it that way: a debug
    /// table reached from the hot loop is a Gate-12 regression.
    async fn run_loop(&self, fiber: &mut Fiber) -> Result<RunOutcome, Control> {
        loop {
            // LANE §2.3: burst through the suspension-free subset on the sync driver.
            // `sync_lane == false` (the kill switch) skips straight to the
            // pre-LANE async dispatch below — the permanent diagnostic mode.
            if self.sync_lane {
                match self.run_loop_sync(fiber)? {
                    SyncOutcome::Finished(outcome) => return Ok(outcome),
                    SyncOutcome::NeedsAsync => {} // fall through: async-execute ONE op
                }
            }

            // Capture the faulting ip (the opcode byte's offset) before advancing.
            let fault_ip = fiber.frame().ip;
            // SP4 §3: remember the source of the frame about to execute, so a panic
            // it raises can be bound to its own module's text on the way out.
            if let Some(src) = fiber.frame().closure.proto.chunk.source.borrow().as_ref() {
                *self.last_fault_source.borrow_mut() = Some(src.clone());
            }
            let byte = fiber.frame().closure.proto.chunk.code[fault_ip];
            let op = Op::from_u8(byte)
                .unwrap_or_else(|| panic!("invalid opcode byte {byte:#x} at ip {fault_ip}"));

            // Advance ip past the opcode byte and its inline operands.
            let operand_at = fault_ip + 1;
            fiber.frame_mut().ip = operand_at + op.operand_width();

            match op {
                Op::Const => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.frame().closure.proto.chunk.consts[idx].clone();
                    fiber.push(v);
                }
                Op::Nil => fiber.push(Value::nil()),
                Op::True => fiber.push(Value::bool_(true)),
                Op::False => fiber.push(Value::bool_(false)),
                Op::Pop => {
                    fiber.pop();
                }
                Op::Dup => {
                    let top = fiber.peek(0).clone();
                    fiber.push(top);
                }
                Op::Swap => {
                    // `a b -- b a`. Both operands are compiler-produced, so the
                    // stack always has the two values (a non-empty stack is a
                    // compiler invariant, not user-reachable).
                    let b = fiber.pop();
                    let a = fiber.pop();
                    fiber.push(b);
                    fiber.push(a);
                }
                Op::Rot3 => {
                    // `a b c -- b c a` (the value 3rd from the top rotates to the
                    // top). Compiler-produced three-value group; never user-reachable
                    // with fewer than three on the stack.
                    let c = fiber.pop();
                    let b = fiber.pop();
                    let a = fiber.pop();
                    fiber.push(b);
                    fiber.push(c);
                    fiber.push(a);
                }

                Op::Add
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
                | Op::InstanceOf
                // Bitwise/shift/wrapping (NUM §3.2) — int-only ops dispatched
                // through the SAME shared `apply_binop` as everything else.
                | Op::BitAnd
                | Op::BitOr
                | Op::BitXor
                | Op::Shl
                | Op::Shr
                | Op::WrapAdd
                | Op::WrapSub
                | Op::WrapMul
                | Op::Range => {
                    // The two operands were pushed lhs-then-rhs, so pop rhs first.
                    // The op's span anchors any Tier-2 panic so the VM's
                    // diagnostics are byte-identical to the tree-walker.
                    let b = fiber.pop();
                    let a = fiber.pop();
                    let binop = binop_of(op);
                    // ONE shared dispatch with the tree-walker (`apply_binop`):
                    // string concat / decimal / range / cross-type equality /
                    // numeric, plus every exact panic message. And/Or/Coalesce are
                    // never lowered to these ops (they short-circuit via jumps), so
                    // `binop_of` never maps to one of them.
                    //
                    // V11-T4 PEP-659 adaptive specialization: a fast path IN FRONT
                    // of `apply_binop` for the common monomorphic operand kinds,
                    // guarded so it can never diverge from the generic result.
                    let v = self.eval_binop_adaptive(fiber, fault_ip, binop, a, b)?;
                    fiber.push(v);
                }

                Op::InstanceOfType => {
                    // `x instanceof int|float|number|string|bool` (NUM §6). The RHS is
                    // a reserved scalar type NAME (a string const), pre-resolved at
                    // compile time — the operand is NOT a value on the stack. Pop the
                    // subject and run the SAME subtype check the tree-walker uses, so
                    // the two engines are byte-identical.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("INSTANCE_OF_TYPE name is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let subject = fiber.pop();
                    let yes = match crate::interp::instanceof_reserved_type(&subject, &name) {
                        Some(b) => b,
                        None => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("INSTANCE_OF_TYPE unknown reserved type name '{name}'"),
                            ))
                        }
                    };
                    fiber.push(Value::bool_(yes));
                }

                Op::RangeInclusive => {
                    // Inclusive value-range `a..=b` — eager `array<number>`,
                    // ascending/step-1, byte-identical to the tree-walker's
                    // value-position `..=` materialization (shared materializer so
                    // the bounds-panic message matches `Op::Range`).
                    let b = fiber.pop();
                    let a = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let v = crate::interp::materialize_range(&a, &b, true, span)?;
                    fiber.push(v);
                }

                Op::RangeStepValue => {
                    // `lo hi step -- array<number>`. flags bit0 = inclusive,
                    // bit1 = step PRESENT. Delegates to the SHARED stepped
                    // materializer so direction, validation, and panic messages are
                    // byte-identical to the tree-walker's value-position `..`/`..=`.
                    let flags = fiber.frame().closure.proto.chunk.read_u8(operand_at);
                    let inclusive = (flags & 0b01) != 0;
                    let present = (flags & 0b10) != 0;
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let step = fiber.pop();
                    let hi = fiber.pop();
                    let lo = fiber.pop();
                    // The SHARED stepped materializer preserves the step's KIND (an Int
                    // bounds + Int step range yields Int elements; a Float step or
                    // Float bound yields Float), so pass the step `Value` through when
                    // present and `None` for the omitted-default placeholder. Direction,
                    // validation, and panic messages are byte-identical to the
                    // tree-walker's value-position `..`/`..=`.
                    let step_arg = if present { Some(&step) } else { None };
                    let v = crate::interp::materialize_range_stepped(
                        &lo, &hi, inclusive, step_arg, span,
                    )?;
                    fiber.push(v);
                }

                Op::RangeResolveStep => {
                    // For-range SETUP: `lo hi step -- lo hi resolved_step`. Peek
                    // lo/hi (already CHECK_NUMBERS-verified), take step, run the
                    // SHARED `resolve_step` (panics on zero/non-finite/mismatch at
                    // this op's span = the START bound's), push the resolved step.
                    let present = fiber.frame().closure.proto.chunk.read_u8(operand_at) == 1;
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let step = fiber.pop();
                    // Peek lo/hi without disturbing them (they stay on the stack). NUM
                    // §4: accept Int OR Float bounds; track Int-ness so the resolved
                    // step (and thus the `Op::Add`-driven counter) stays Int when the
                    // bounds + step are all Int — byte-identical to the tree-walker's
                    // `range_counter_value`.
                    let hi_v = fiber.peek(0);
                    let hi = match hi_v.as_f64() {
                        Some(n) => n,
                        None => unreachable!("RANGE_RESOLVE_STEP hi must be a number (CHECK_NUMBERS)"),
                    };
                    let hi_int = hi_v.is_int_value();
                    let lo_v = fiber.peek(1);
                    let lo = match lo_v.as_f64() {
                        Some(n) => n,
                        None => unreachable!("RANGE_RESOLVE_STEP lo must be a number (CHECK_NUMBERS)"),
                    };
                    let lo_int = lo_v.is_int_value();
                    let (step_v, step_int) = if present {
                        match step.as_f64() {
                            Some(s) => (Some(s), step.is_int_value()),
                            None => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    "for-range step must be a number".to_string(),
                                ))
                            }
                        }
                    } else {
                        // Omitted step is the integral `±1`.
                        (None, true)
                    };
                    let resolved = crate::interp::resolve_step(lo, hi, step_v, span)?;
                    let yields_int = lo_int && hi_int && step_int;
                    fiber.push(crate::interp::range_counter_value(resolved, yields_int));
                }

                Op::RangeHasNext => {
                    // For-range CONDITION: `i hi step -- ok:bool`. Direction-aware
                    // continue predicate via the SHARED `range_has_next` (positive
                    // step: i < hi / i <= hi; negative: i > hi / i >= hi). Never
                    // panics (validation done in RANGE_RESOLVE_STEP).
                    let inclusive = fiber.frame().closure.proto.chunk.read_u8(operand_at) == 1;
                    let step = fiber.pop();
                    let hi = fiber.pop();
                    let i = fiber.pop();
                    let ok = match (i.as_f64(), hi.as_f64(), step.as_f64()) {
                        (Some(i), Some(hi), Some(step)) => {
                            crate::interp::range_has_next(i, hi, step, inclusive)
                        }
                        _ => unreachable!("RANGE_HAS_NEXT operands must be numbers"),
                    };
                    fiber.push(Value::bool_(ok));
                }

                Op::Neg | Op::Not | Op::BitNot => {
                    let a = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let v = crate::interp::apply_unop(unop_of(op), a, span)?;
                    fiber.push(v);
                }

                Op::GetLocal => {
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.local(slot).clone();
                    fiber.push(v);
                }
                Op::SetLocal => {
                    // Clean stack discipline: SET_LOCAL POPS the value and stores
                    // it. Assignment-as-expression `DUP`s beforehand so a copy
                    // remains as the expression's result (see `compile_assign`).
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.pop();
                    fiber.set_local(slot, v);
                }

                Op::GetGlobal => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("GET_GLOBAL operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    // Resolution ORDER (matching the tree-walker's module
                    // `Environment`, a child of the builtins): consult the
                    // module-scope USER-GLOBALS first, THEN the bare builtins, ELSE
                    // the tree-walker's exact runtime message (`undefined variable
                    // '<n>'`, see `Interp::eval_expr`'s `ExprKind::Ident` arm). A user
                    // global thus SHADOWS a builtin of the same name (e.g.
                    // `import { test }` shadowing the builtin `test`), exactly as in
                    // the tree-walker.
                    // V11-T4 GET_GLOBAL_CACHED: cache the resolved value at this op's
                    // offset, guarded by the global-table VERSION. A version hit
                    // returns the cached value (skipping the lookup); a version miss
                    // (any global write bumps the version) re-resolves. Correctness:
                    // the cached value is exactly what the resolve below produces, and
                    // the version guard invalidates it on every global mutation.
                    // KILL SWITCH (V11-T5): with specialization OFF, NEVER consult or
                    // record the global cache — always re-resolve generically. The
                    // resolved value is identical either way, so generic and
                    // specialized stay byte-identical.
                    // SP8 INDEX-STABLE user-global cache: when specializing, consult
                    // the site cache for an `IndexBound { idx, struct_gen }` entry. A
                    // `struct_gen` hit reads the user-global by its STABLE IndexMap
                    // index (no string hash) — this is the regression recovery for a
                    // hot reassigned top-level `let` (a SET never bumps `struct_gen`).
                    let version = self.global_version();
                    let cache = fiber.frame().closure.proto.chunk.global_cache(fault_ip);
                    if self.specialize {
                        if let Some(idx) = cache.get_index(self.struct_gen()) {
                            fiber.push(self.user_global_value_at(idx));
                            continue;
                        }
                    }
                    if let Some(v) = cache.get(version).filter(|_| self.specialize) {
                        fiber.push(v);
                    } else if let Some((idx, v)) = self.get_user_global_full(&name) {
                        // A user global resolves by name (the cold/miss path); when
                        // specializing, RECORD its stable IndexMap index so subsequent
                        // executions of this site hit the index fast path above. We
                        // cache the INDEX (not the value), so a value reassignment is
                        // immediately visible (the next read re-reads the slot) — no
                        // thrash, no stale value.
                        if self.specialize {
                            fiber.frame().closure.proto.chunk.set_global_cache(
                                fault_ip,
                                crate::vm::adapt::GlobalCache::index_bound(idx, self.struct_gen()),
                            );
                        }
                        fiber.push(v);
                    } else if crate::interp::BUILTIN_NAMES.contains(&name.as_ref()) {
                        let v = Value::builtin(name);
                        if self.specialize {
                            fiber.frame().closure.proto.chunk.set_global_cache(
                                fault_ip,
                                crate::vm::adapt::GlobalCache::set(v.clone(), version),
                            );
                        }
                        fiber.push(v);
                    } else {
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            format!("undefined variable '{name}'"),
                        ));
                    }
                }

                Op::DefineGlobal => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    // The u8 mutability flag follows the u16 name const (1 = `let`,
                    // 0 = immutable `const`/`fn`/`class`/`enum`/`import`).
                    let mutable = fiber.frame().closure.proto.chunk.read_u8(operand_at + 2) != 0;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "DEFINE_GLOBAL operand is not a string constant: {other:?}"
                                ),
                            ))
                        }
                    };
                    let v = fiber.pop();
                    // A REDECLARATION (the name is already a module global — e.g.
                    // `let x; let x`, `fn f; fn f`, `fn f; let f`) is the tree-walker's
                    // runtime same-scope `Environment::define` rejection, fired when the
                    // SECOND define executes. It uses `AsError::new` (NO span — span
                    // `None`), so we match byte-for-byte (message + absent span). Because
                    // this fires on EXECUTION, a redeclaration in dead/unreached code (an
                    // un-entered block, an uncalled function — those are slot-locals, not
                    // globals, anyway) never triggers, exactly like the tree-walker.
                    if self.user_globals.borrow().contains_key(name.as_ref()) {
                        return Err(Control::Panic(AsError::new(format!(
                            "'{name}' is already defined in this scope"
                        ))));
                    }
                    self.define_user_global(name, v, mutable);
                }

                Op::SetGlobal => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("SET_GLOBAL operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    // Top-level reassignment `x = …` of an EXISTING module global. A
                    // SET_LOCAL-style assignment leaves the value on the stack (an
                    // assignment is an expression yielding the assigned value), so we
                    // PEEK (clone TOS) rather than pop.
                    //
                    // RUNTIME mutability check (the single source of truth for GLOBAL
                    // assignment targets — the compiler always lowers a global-target
                    // assignment to SET_GLOBAL, never the compile-time IMMUTABLE_ERROR):
                    //   - IMMUTABLE global (`const`/`fn`/`class`/`enum`/`import`) → the
                    //     tree-walker's `cannot assign to immutable binding '<n>'`,
                    //     anchored at the TARGET span (this op's span). This fires even
                    //     when the immutable decl was in an EARLIER, separately-compiled
                    //     chunk (REPL line-to-line; a main module reassigning an import),
                    //     which the compile-time IMMUTABLE_ERROR cannot see. It is
                    //     RUNTIME-timed: only an EXECUTED store errors (a dead
                    //     `if false { k = 2 }` never runs this op), matching the
                    //     tree-walker's `Environment::assign`.
                    //   - Absent name → `cannot assign to undefined variable '<n>'`.
                    //   - Mutable global (`let`) → update in place. We do NOT bump the
                    //     global version OR `struct_gen`: a SET is not a define, so it
                    //     cannot move any index or change a cached name's target — no
                    //     cache can go stale. Keeps a hot reassignment loop cheap (no
                    //     per-iteration cache invalidation), matching the generic VM.
                    //
                    // SP8 INDEX-STABLE set cache: when specializing, consult the site
                    // cache (a distinct bytecode offset from any GET_GLOBAL, so the
                    // offset-keyed `global_caches` disambiguates GET vs SET sites). A
                    // `struct_gen` hit writes by the stable index (one `get_index_mut`,
                    // no string hash). On a miss, fall through to the name-keyed path
                    // AND record the index for next time.
                    let v = fiber.peek(0).clone();
                    let cache = fiber.frame().closure.proto.chunk.global_cache(fault_ip);
                    if self.specialize {
                        if let Some(idx) = cache.get_index(self.struct_gen()) {
                            match self.set_user_global_at(idx, v.clone()) {
                                Some(true) => continue,
                                Some(false) => {
                                    return Err(self.panic_at(
                                        fiber,
                                        fault_ip,
                                        format!("cannot assign to immutable binding '{name}'"),
                                    ));
                                }
                                // Out-of-range cached index (impossible while the
                                // struct_gen matches, since user-globals are never
                                // removed) — defensively fall through to re-resolve.
                                None => {}
                            }
                        }
                    }
                    match self.user_global_mutable(name.as_ref()) {
                        Some(true) => {
                            self.update_user_global(&name, v);
                            if self.specialize {
                                if let Some((idx, _)) = self.get_user_global_full(name.as_ref()) {
                                    fiber.frame().closure.proto.chunk.set_global_cache(
                                        fault_ip,
                                        crate::vm::adapt::GlobalCache::index_bound(
                                            idx,
                                            self.struct_gen(),
                                        ),
                                    );
                                }
                            }
                        }
                        Some(false) => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("cannot assign to immutable binding '{name}'"),
                            ));
                        }
                        None => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("cannot assign to undefined variable '{name}'"),
                            ));
                        }
                    }
                }

                Op::ImmutableError => {
                    // Unconditionally raise the tree-walker's immutable-binding panic.
                    // Emitted at the store position of an assignment whose target is an
                    // immutable binding (const/fn/class/enum/import/loop-var/const-pattern
                    // bind), AFTER the RHS has been evaluated — so the timing (RHS
                    // side-effects first; dead/unreached assignments never trigger),
                    // message, and span all match the tree-walker's `Environment::assign`
                    // immutable error byte-for-byte.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "IMMUTABLE_ERROR operand is not a string constant: {other:?}"
                                ),
                            ))
                        }
                    };
                    return Err(self.panic_at(
                        fiber,
                        fault_ip,
                        format!("cannot assign to immutable binding '{name}'"),
                    ));
                }

                Op::Call | Op::CallElided | Op::CallSpread => {
                    // `Op::Call`/`Op::CallElided` carry a STATIC `u8` argc; `Op::CallSpread`
                    // carries none — its arguments arrived as a single runtime
                    // `Value::array_cell` (built by the array/spread builder ops) sitting
                    // on top of the callee `[..., callee, argsArray]`. For `CallSpread` we
                    // POP the args array and re-push its elements as individual stack slots,
                    // so the stack becomes `[..., callee, arg0, .., arg{n-1}]` — the
                    // EXACT shape `Op::Call` expects — and dispatch is shared below
                    // (arity/contracts then apply to the flattened list, byte-
                    // identical to the tree-walker's `eval_call_args` → call).
                    //
                    // ELIDE §4.4: CallElided sets elide=true so the shared binder skips
                    // per-param type-contract checks at statically-proven sites.
                    let elide = matches!(op, Op::CallElided);
                    let argc = if matches!(op, Op::CallSpread) {
                        let args_arr = fiber.pop();
                        let args_ty = crate::interp::type_name(&args_arr);
                        let args = match args_arr.into_kind() {
                            OwnedKind::Array(a) => a,
                            _ => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("CALL_SPREAD args are not an array: {args_ty}"),
                                ))
                            }
                        };
                        let items: Vec<Value> = args.borrow().iter().cloned().collect();
                        let n = items.len();
                        for v in items {
                            fiber.push(v);
                        }
                        n
                    } else {
                        fiber.frame().closure.proto.chunk.read_u8(operand_at) as usize
                    };
                    // The callee sits just below its `argc` arguments on the stack:
                    // `[..., callee, arg0, .., arg{argc-1}]`. Its stack index is the
                    // base where, for a Closure callee, the args become the callee
                    // frame's first local slots (the CALL convention).
                    let callee_idx = fiber.stack.len() - argc - 1;
                    match fiber.stack[callee_idx].clone().into_kind() {
                        // A generator closure (`fn*` / `async fn*`) is NOT run and
                        // NOT spawned: calling it builds a NOT-STARTED Fiber for the
                        // closure (args bound into its slots, ip 0) and wraps it in a
                        // VM-backed `GeneratorHandle`, pushing a `Value::generator`
                        // immediately. The body runs only when the consumer calls
                        // `gen.next()` (→ `GeneratorHandle::resume`), exactly like the
                        // tree-walker's `is_generator` branch of `call_function`.
                        // Both sync and async generators take this path (the async-
                        // generator yield+await fusion is V8-T5; for now we build the
                        // generator the same way). Arg binding reuses the SAME
                        // `check_call_args` the tree-walker / plain-call path uses, so
                        // arity/contract panics are byte-identical and surface eagerly
                        // at the call (the tree-walker also binds args eagerly when
                        // building the generator). AWAIT DISCIPLINE: no await here;
                        // the fiber is built synchronously and handed to the handle.
                        // A `worker fn*` (Spec B Task 6) is a STREAMING generator: its
                        // body runs in a DEDICATED isolate, consumed via a cross-thread
                        // demand-driven driver. Must precede the plain-generator arm (a
                        // `worker fn*` has BOTH flags) and the `worker fn` arm. Same
                        // `Interp::spawn_worker_stream` as the tree-walker → byte-
                        // identical. AWAIT DISCIPLINE: pop the args synchronously, then
                        // `.await` the spawn with no fiber borrow held.
                        OwnedKind::Closure(callee)
                            if callee.proto.is_worker && callee.proto.is_generator =>
                        {
                            let call_span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            let entry_name = callee
                                .proto
                                .chunk
                                .name
                                .clone()
                                .ok_or_else(|| {
                                    Control::Panic(crate::error::AsError::at(
                                        "worker fn* must be a named top-level function"
                                            .to_string(),
                                        call_span,
                                    ))
                                })?;
                            let mut args = vec![Value::nil(); argc];
                            for slot in args.iter_mut().rev() {
                                *slot = fiber.pop();
                            }
                            fiber.pop(); // the callee value at callee_idx
                            let gen = self
                                .interp
                                .spawn_worker_stream(&entry_name, args, call_span)
                                .await?;
                            fiber.push(gen);
                        }
                        OwnedKind::Closure(callee) if callee.proto.is_generator => {
                            let call_span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            let what = callee.proto.chunk.name.as_deref().unwrap_or("function");
                            // Pop the args, then drop the callee value beneath them.
                            let mut args = vec![Value::nil(); argc];
                            for slot in args.iter_mut().rev() {
                                *slot = fiber.pop();
                            }
                            fiber.pop(); // the callee value at callee_idx
                                         // Bind args (arity + per-param contracts + rest) — shared
                                         // with every other call path. A mismatch is a Tier-2
                                         // panic at the call site, eager (like the tree-walker).
                            let bound = crate::interp::check_call_args(
                                &callee.proto.params,
                                args,
                                call_span,
                                what,
                                Some(&self.interp),
                                Some(&self.class_env()),
                                false,
                            )?;
                            // Build a NOT-STARTED one-frame Fiber for the closure and
                            // place the bound params into its slots (cell slot → cell,
                            // plain slot → stack). `Fiber::new` reserved the locals
                            // and the cell vector. We do NOT run it.
                            let mut gfiber = Fiber::new(callee);
                            gfiber.frame_mut().ret_span = call_span;
                            gfiber.frame_mut().argc = bound.supplied;
                            let cells = gfiber.frame().cells.clone();
                            for (slot, v) in bound.values.into_iter().enumerate() {
                                // CALL §2 A1: use .get so empty-vec is safe.
                                if let Some(cell) = cells.get(slot).and_then(|c| c.as_ref()) {
                                    *cell.borrow_mut() = v;
                                } else {
                                    gfiber.stack[slot] = v;
                                }
                            }
                            let handle = crate::coro::GeneratorHandle::new_vm(
                                gfiber,
                                Rc::downgrade(&self.rc()),
                            );
                            fiber.push(Value::generator(Rc::new(handle)));
                        }
                        // An `async fn` closure is NOT run inline: it is scheduled
                        // eagerly (M17 model 2a), exactly like the tree-walker's
                        // `is_async` branch of `call_function`. We build a body future
                        // that re-enters the VM via `Vm::call_value` (which sets up a
                        // fresh one-frame fiber, binds args via `check_call_args`, and
                        // runs to Done), `spawn_local` it onto the current-thread
                        // LocalSet, and hand back a `Value::future` IMMEDIATELY; the
                        // caller `await`s it later. Because `call_value` runs the arity
                        // /contract check INSIDE the spawned task, an async arity or
                        // contract violation surfaces LAZILY — it resolves into the
                        // SharedFuture and re-emerges at the `await` site — byte-
                        // identical to the tree-walker. AWAIT DISCIPLINE: the closure
                        // and its args move into the `'static` spawned task; `vm` is an
                        // owned `Rc<Vm>`; no `fiber` RefCell borrow is held across the
                        // spawn/await below.
                        // A `worker fn` closure dispatches to a pooled isolate
                        // (Workers Spec A): pop the args, build the code slice from the
                        // entry program source, ship + return a `Value::future`. Must
                        // precede the `is_async` branch (a worker fn is not async).
                        OwnedKind::Closure(callee) if callee.proto.is_worker => {
                            let call_span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            let mut args = vec![Value::nil(); argc];
                            for slot in args.iter_mut().rev() {
                                *slot = fiber.pop();
                            }
                            fiber.pop(); // the callee value at callee_idx
                            let fut = self.dispatch_worker_closure(&callee, args, call_span)?;
                            fiber.push(fut);
                        }
                        OwnedKind::Closure(callee) if callee.proto.is_async => {
                            let call_span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            // Pop the `argc` args into an owned vec (top of stack is
                            // the LAST arg), then drop the callee value beneath them.
                            let mut args = vec![Value::nil(); argc];
                            for slot in args.iter_mut().rev() {
                                *slot = fiber.pop();
                            }
                            fiber.pop(); // the callee value at callee_idx
                                         // Reuse the shared M17 dance (mirrors `call_function`'s
                                         // async branch and `BoundMethod`'s): an owned `Rc<Vm>`
                                         // (Vm self-weak, installed at `Vm::new`) drives the body;
                                         // the task resolves the CELL (never a `SharedFuture` clone)
                                         // so cancel-on-drop works; the inflight guard provides
                                         // backpressure (reused from the shared interp).
                            let vm = self.rc();
                            let fut = crate::task::SharedFuture::new();
                            let cell = fut.cell();
                            let guard = self.interp.inflight_guard();
                            let handle = tokio::task::spawn_local(async move {
                                let _g = guard;
                                let r =
                                    vm.call_value(Value::closure(callee), args, call_span).await;
                                cell.resolve(r);
                            });
                            fut.set_abort(handle.abort_handle());
                            self.interp.maybe_yield_for_inflight().await;
                            fiber.push(Value::future(fut));
                        }
                        OwnedKind::Closure(callee) => {
                            // The call-site span anchors arity/contract/return
                            // panics exactly where the tree-walker's do.
                            let call_span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            // CALL §3 (A2): in-place arg binding fast path.
                            //
                            // Qualifies when:
                            //   1. `call_fast` kill switch is on.
                            //   2. `!has_rest` — rest collection genuinely allocates a
                            //      new tail array; those calls keep the Vec path.
                            //
                            // The args already sit contiguously at
                            // `stack[callee_idx+1 .. callee_idx+1+argc]`, one slot above
                            // the new frame's window. `check_call_args_in_place` runs the
                            // SAME shared cores (arity + per-param contracts) that
                            // `check_call_args` uses — wording/order byte-identical by
                            // construction (CALL §3.4). Defaults are NOT evaluated here;
                            // the callee prologue (`Op::JumpIfArgSupplied`) reads
                            // `frame.argc` (the supplied count) to decide per defaulted
                            // param, and `resize` fills remaining slots with the same
                            // `Nil` the `BoundArgs` placeholders carried — identical
                            // layout, zero extra allocation.
                            if self.call_fast && !callee.proto.has_rest {
                                let supplied = crate::interp::check_call_args_in_place(
                                    &callee.proto.params,
                                    &fiber.stack[callee_idx + 1..callee_idx + 1 + argc],
                                    call_span,
                                    callee.proto.chunk.name.as_deref().unwrap_or("function"),
                                    Some(&self.interp),
                                    Some(&self.class_env()),
                                    elide,
                                )?;
                                // Drop the callee value; the `argc` args shift down one
                                // slot to start AT slot_base (an argc-element memmove,
                                // zero allocation).
                                fiber.stack.remove(callee_idx);
                                let slot_base = callee_idx;
                                let slot_count = callee.proto.chunk.slot_count as usize;
                                let cells = super::fiber::alloc_cells(
                                    slot_count,
                                    &callee.proto.chunk.cell_slots,
                                );
                                // Extend to the full frame window; slots beyond `argc`
                                // default to `Nil` (the same value the BoundArgs
                                // placeholders carried for omitted defaulted params).
                                fiber.stack.resize(slot_base + slot_count, Value::nil());
                                // Rare: a param whose resolver-assigned slot is a cell
                                // slot (a callback that captures AND mutates a param).
                                // Move it from the window into its cell.
                                if !cells.is_empty() {
                                    for slot in 0..supplied {
                                        if let Some(cell) =
                                            cells.get(slot).and_then(|c| c.as_ref())
                                        {
                                            *cell.borrow_mut() = std::mem::replace(
                                                &mut fiber.stack[slot_base + slot],
                                                Value::nil(),
                                            );
                                        }
                                    }
                                }
                                self.bump_stat(|s| s.inplace_binds += 1);
                                // SP3 §B: one logical-call increment per frame push.
                                self.enter_frame_depth(call_span)?;
                                fiber.frames.push(super::fiber::CallFrame {
                                    closure: callee,
                                    ip: 0,
                                    slot_base,
                                    cells,
                                    ret_span: call_span,
                                    def_class: None,
                                    argc: supplied,
                                    defers: Vec::new(),
                                });
                                // DBG Task 7: publish the new frame stack to a profiler.
                                self.publish_profile_frames(fiber);
                            } else {
                                // Fallback: pop-into-Vec + check_call_args (rest params,
                                // or call_fast kill switch off). Behavior-identical to the
                                // pre-A2 path.
                                // LANE Task 3: shared plain-call body (also used by
                                // run_loop_sync). Pops args + callee, checks arity +
                                // contracts, allocates cells, pushes the CallFrame (one
                                // enter_frame_depth — SP3 §B), publishes profiler frames.
                                self.push_closure_frame(
                                    fiber, callee, argc, callee_idx, call_span, elide,
                                )?;
                            }
                            // Continue the loop in the new frame.
                        }
                        _ => {
                            // Native callee (Builtin/Function/Class/BoundMethod/...):
                            // delegate to the VM-aware `call_value`, which routes a
                            // VM class constructor / VM bound method to COMPILED code
                            // (V9) and everything else to the shared `Interp`
                            // dispatch. Pop the args and the callee into owned locals
                            // BEFORE the await so no borrow of `fiber` is held across
                            // the suspension point (`await_holding_refcell_ref` stays
                            // clean).
                            let mut args = vec![Value::nil(); argc];
                            for slot in args.iter_mut().rev() {
                                *slot = fiber.pop();
                            }
                            let callee_v = fiber.pop(); // the Value at callee_idx
                            let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            let result = self.call_value(callee_v, args, span).await?;
                            fiber.push(result);
                        }
                    }
                }

                Op::CallNamed => {
                    // ADT §3.2: a call carrying NAMED arguments. Two operands: a u16
                    // names-array const index then a u8 argc. The stack is
                    // `[..., callee, v0, .., v{argc-1}]` (values in source order). The
                    // names array (`consts[idx]`, length argc) pairs each value with a
                    // `Str` field name or `Nil` (positional). The only valid callee is
                    // an enum-variant constructor → `construct_variant_args`, byte-
                    // identical to the tree-walker's `call_value_named`. AWAIT
                    // DISCIPLINE: pop values + callee into owned locals first; read the
                    // names const into an owned `Vec` before the await; hold no `fiber`
                    // borrow across the suspension point.
                    let names_idx =
                        fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let argc = fiber.frame().closure.proto.chunk.read_u8(operand_at + 2) as usize;
                    let names: Vec<Option<Rc<str>>> = match fiber
                        .frame()
                        .closure
                        .proto
                        .chunk
                        .consts[names_idx]
                        .kind()
                    {
                        ValueKind::Array(a) => a
                            .borrow()
                            .iter()
                            .map(|v| match v.kind() {
                                ValueKind::Str(s) => Some(s.clone()),
                                _ => None,
                            })
                            .collect(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("CALL_NAMED names operand is not an array: {other:?}"),
                            ))
                        }
                    };
                    let mut args = vec![Value::nil(); argc];
                    for slot in args.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    let callee = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let result = match callee.kind() {
                        ValueKind::EnumVariant(ev) => {
                            self.interp
                                .construct_variant_args(ev, args, &names, span)
                                .await?
                        }
                        _ => {
                            return Err(Control::Panic(crate::error::AsError::at(
                                format!(
                                    "named arguments are only valid for enum-variant \
                                     construction, not for {}",
                                    crate::interp::type_name(&callee)
                                ),
                                span,
                            )))
                        }
                    };
                    fiber.push(result);
                }

                Op::AppendNamedArg => {
                    // ADT §3.2 (spread+named lockstep builder). Stack
                    // `[..., argsArray, namesArray, value]`: pop `value`, push it onto
                    // `argsArray` (peek 1) and push the field name `consts[idx]` (a
                    // `Str`) onto `namesArray` (peek 0).
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => Value::str(s.clone()),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("APPEND_NAMED_ARG name operand is not a string: {other:?}"),
                            ))
                        }
                    };
                    let value = fiber.pop();
                    match (fiber.peek(1).kind(), fiber.peek(0).kind()) {
                        (ValueKind::Array(args), ValueKind::Array(names)) => {
                            args.borrow_mut().push(value);
                            names.borrow_mut().push(name);
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "APPEND_NAMED_ARG builder targets are not both arrays".to_string(),
                            ))
                        }
                    }
                }

                Op::AppendPosArg => {
                    // ADT §3.2 (spread+named lockstep builder). Stack
                    // `[..., argsArray, namesArray, value]`: pop `value`, push it onto
                    // `argsArray` and push `Nil` onto `namesArray` (a positional value).
                    let value = fiber.pop();
                    match (fiber.peek(1).kind(), fiber.peek(0).kind()) {
                        (ValueKind::Array(args), ValueKind::Array(names)) => {
                            args.borrow_mut().push(value);
                            names.borrow_mut().push(Value::nil());
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "APPEND_POS_ARG builder targets are not both arrays".to_string(),
                            ))
                        }
                    }
                }

                Op::AppendSpreadArg => {
                    // ADT §3.2 (spread+named lockstep builder). Stack
                    // `[..., argsArray, namesArray, operand]`: pop `operand` (MUST be an
                    // Array — else the SAME `can only spread an array as call arguments`
                    // panic the positional path produces), extend `argsArray` with its
                    // elements and push `Nil` ONCE PER element onto `namesArray`.
                    let operand = fiber.pop();
                    let items: Vec<Value> = match operand.kind() {
                        ValueKind::Array(src) => src.borrow().iter().cloned().collect(),
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "can only spread an array as call arguments, got {}",
                                    crate::interp::type_name(&operand)
                                ),
                            ))
                        }
                    };
                    let n = items.len();
                    match (fiber.peek(1).kind(), fiber.peek(0).kind()) {
                        (ValueKind::Array(args), ValueKind::Array(names)) => {
                            args.borrow_mut().extend(items);
                            names.borrow_mut().extend(std::iter::repeat_n(Value::nil(), n));
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "APPEND_SPREAD_ARG builder targets are not both arrays".to_string(),
                            ))
                        }
                    }
                }

                Op::CallNamedSpread => {
                    // ADT §3.2: the dynamic-arity named call (spread+named). Stack
                    // `[..., callee, argsArray, namesArray]`. Pop both arrays + the
                    // callee, then dispatch EXACTLY like CALL_NAMED →
                    // `construct_variant_args` (byte-identical to the tree-walker's
                    // `call_value_named`). AWAIT DISCIPLINE: pull args + names into owned
                    // Vecs before the await; hold no `fiber` borrow across it.
                    let names: Vec<Option<Rc<str>>> = match fiber.pop().kind() {
                        ValueKind::Array(a) => a
                            .borrow()
                            .iter()
                            .map(|v| match v.kind() {
                                ValueKind::Str(s) => Some(s.clone()),
                                _ => None,
                            })
                            .collect(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("CALL_NAMED_SPREAD names is not an array: {other:?}"),
                            ))
                        }
                    };
                    let args: Vec<Value> = match fiber.pop().kind() {
                        ValueKind::Array(a) => a.borrow().iter().cloned().collect(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("CALL_NAMED_SPREAD args is not an array: {other:?}"),
                            ))
                        }
                    };
                    let callee = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let result = match callee.kind() {
                        ValueKind::EnumVariant(ev) => {
                            self.interp
                                .construct_variant_args(ev, args, &names, span)
                                .await?
                        }
                        _ => {
                            return Err(Control::Panic(crate::error::AsError::at(
                                format!(
                                    "named arguments are only valid for enum-variant \
                                     construction, not for {}",
                                    crate::interp::type_name(&callee)
                                ),
                                span,
                            )))
                        }
                    };
                    fiber.push(result);
                }

                Op::CallMethod => {
                    // A method call `recv.<name>(args)`. Mirrors the tree-walker's
                    // `eval_chain` Call arm for a `Member` callee: the schema
                    // fluent-method hook, else `read_member(recv, name)` →
                    // `call_value`. The receiver sits below its args on the stack.
                    //
                    // ORDERING NOTE: the tree-walker reads the member BEFORE
                    // evaluating the call args (so a member-read error preempts arg
                    // side effects). Here the compiler already evaluated the args
                    // (they are on the stack), so a member-read error does NOT
                    // preempt arg side effects. This sub-case (a side-effecting arg
                    // AND an erroring member read) is the documented deviation
                    // deferred to the full V9 method-call slice; the generator
                    // consumer API (`gen.next(v)`/`gen.close()`) and the rest of the
                    // gated corpus do not hit it. Everything else is byte-identical.
                    let name = match fiber.frame().closure.proto.chunk.consts
                        [fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize]
                    .kind()
                    {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("CALL_METHOD name is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let argc = fiber.frame().closure.proto.chunk.read_u8(operand_at + 2) as usize;
                    // Pop the args (top is the LAST arg), then the receiver beneath.
                    let mut args = vec![Value::nil(); argc];
                    for slot in args.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    let recv = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    // The entire dispatch (schema hook → IC compiled-method fast path
                    // → `read_member`→`call_value` fallback) is shared with
                    // `Op::CallMethodSpread` — see `dispatch_method`. It either pushes
                    // the result or pushes a frame (the IC in-frame fast path) and lets
                    // the run loop continue.
                    self.dispatch_method(fiber, recv, &name, args, fault_ip, span)
                        .await?;
                }

                Op::CallMethodSpread => {
                    // A method call `recv.<name>(...args)` whose argument list contains
                    // a spread (dynamic arity). Mirrors `Op::CallMethod` EXACTLY for
                    // dispatch — the only difference is how the arg list is obtained:
                    // the args arrived as a single runtime `Value::array_cell` (built by the
                    // array/spread builder ops), sitting on top of the receiver
                    // `[..., recv, argsArray]`. Pop the args array and flatten it into
                    // a positional `Vec`, then pop the receiver — yielding the SAME
                    // `(recv, args)` shape `Op::CallMethod` produces — and dispatch via
                    // the shared `dispatch_method`. Arity/contracts apply to the
                    // FLATTENED list, byte-identical to the tree-walker's
                    // `eval_call_args` (spread flatten) → method dispatch.
                    //
                    // ORDERING NOTE: identical to `Op::CallMethod` — the compiler
                    // already evaluated the receiver and the (spread-flattened) args
                    // onto the stack, so a member-read error does NOT preempt arg side
                    // effects. This is the SAME documented deviation as `Op::CallMethod`.
                    let name = match fiber.frame().closure.proto.chunk.consts
                        [fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize]
                    .kind()
                    {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "CALL_METHOD_SPREAD name is not a string constant: {other:?}"
                                ),
                            ))
                        }
                    };
                    // Pop the runtime args array (built by NEW_ARRAY + spread ops) and
                    // re-materialize its elements as a positional `Vec`. (The builder
                    // always produces a `Value::array_cell`; a non-array OPERAND was already
                    // rejected by `SPREAD_ARGS` with the byte-identical message.)
                    let args_arr = fiber.pop();
                    let args = match args_arr.kind() {
                        ValueKind::Array(a) => a.borrow().iter().cloned().collect::<Vec<_>>(),
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "CALL_METHOD_SPREAD args are not an array: {}",
                                    crate::interp::type_name(&args_arr)
                                ),
                            ))
                        }
                    };
                    let recv = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    self.dispatch_method(fiber, recv, &name, args, fault_ip, span)
                        .await?;
                }

                Op::Template => {
                    // Pop `n` parts (pushed left-to-right) and concatenate their
                    // string coercions in source order. The coercion is exactly
                    // the tree-walker's `Value::to_string()` (the `Display` impl
                    // shared with `print`), so a template interpolating any value
                    // renders byte-identically to `ExprKind::Template`.
                    let n = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let mut parts = vec![Value::nil(); n];
                    for slot in parts.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    let mut out = String::new();
                    for v in &parts {
                        out.push_str(&v.to_string());
                    }
                    fiber.push(Value::str(out));
                }

                Op::Jump => {
                    // Unconditional relative jump. The displacement is measured
                    // from the byte AFTER the operand to the target (see
                    // `Chunk::patch_jump`/`emit_loop`). At this point we have
                    // already advanced `ip` past the opcode and its 2-byte
                    // operand, so `fiber.frame().ip == operand_at + 2` is exactly
                    // that base; add the signed displacement to land on target.
                    let disp = fiber.frame().closure.proto.chunk.read_i16(operand_at);
                    let base = fiber.frame().ip as isize;
                    fiber.frame_mut().ip = (base + disp as isize) as usize;
                }
                Op::Loop => {
                    // Unconditional backward (relative) jump used for loop
                    // back-edges. Identical mechanics to `Op::Jump` — the
                    // displacement (negative for a real backward jump) is measured
                    // from the byte AFTER the operand to the target (see
                    // `Chunk::emit_loop`).
                    let disp = fiber.frame().closure.proto.chunk.read_i16(operand_at);
                    let base = fiber.frame().ip as isize;
                    fiber.frame_mut().ip = (base + disp as isize) as usize;
                }
                Op::JumpIfFalse => {
                    // Pop the tested value; jump iff it is falsy. Short-circuit
                    // lowering `DUP`s the operand beforehand so the un-tested copy
                    // survives as the expression's result when we jump.
                    let v = fiber.pop();
                    if !v.is_truthy() {
                        let disp = fiber.frame().closure.proto.chunk.read_i16(operand_at);
                        let base = fiber.frame().ip as isize;
                        fiber.frame_mut().ip = (base + disp as isize) as usize;
                    }
                }
                Op::JumpIfTrue => {
                    // Pop the tested value; jump iff it is truthy.
                    let v = fiber.pop();
                    if v.is_truthy() {
                        let disp = fiber.frame().closure.proto.chunk.read_i16(operand_at);
                        let base = fiber.frame().ip as isize;
                        fiber.frame_mut().ip = (base + disp as isize) as usize;
                    }
                }
                Op::JumpIfNotNil => {
                    // Pop the tested value; jump iff it is NOT `nil`. Mirrors the
                    // tree-walker's `??` test (`l == Value::nil()` selects the RHS;
                    // anything else keeps the left), so the jump fires on "keep
                    // the non-nil left operand".
                    let v = fiber.pop();
                    if v != Value::nil() {
                        let disp = fiber.frame().closure.proto.chunk.read_i16(operand_at);
                        let base = fiber.frame().ip as isize;
                        fiber.frame_mut().ip = (base + disp as isize) as usize;
                    }
                }
                Op::JumpIfArgSupplied => {
                    // Default-parameter prologue guard. If the caller SUPPLIED this
                    // positional param (frame `argc` > param-index), jump forward
                    // past its default-eval code; otherwise fall through and run
                    // the default. Touches no operand stack. The i16 jump offset is
                    // the SECOND operand (after the u16 param-index), and `ip` is
                    // already past the whole instruction.
                    let chunk = &fiber.frame().closure.proto.chunk;
                    let param = chunk.read_u16(operand_at) as usize;
                    let disp = chunk.read_i16(operand_at + 2);
                    if fiber.frame().argc > param {
                        let base = fiber.frame().ip as isize;
                        fiber.frame_mut().ip = (base + disp as isize) as usize;
                    }
                }
                Op::CheckParam => {
                    // Contract-check the just-evaluated default value (TOS, left in
                    // place) against the param's declared type, byte-identical to
                    // the tree-walker's default contract (same message; span = the
                    // frame's call site `ret_span`). Untyped params emit no
                    // CHECK_PARAM, so a type is always present here.
                    let param = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let span = fiber.frame().ret_span;
                    let ty = fiber.frame().closure.proto.params[param].ty.clone();
                    if let Some(ty) = ty {
                        let v = fiber.peek(0).clone();
                        if !crate::interp::check_type(&v, &ty) {
                            // §6.3 paranoid: call-site span in `calls` set.
                            if let Some(e) = self.interp.maybe_paranoid_escalate(&ty, &v, span) {
                                return Err(e);
                            }
                            return Err(crate::interp::contract_panic(&ty, &v, span));
                        }
                    }
                }

                Op::CheckLocal => {
                    // Contract-check the just-evaluated initializer (TOS, left in
                    // place) of an annotated `let`/`const` against its declared type,
                    // byte-identical to the tree-walker's `Stmt::Let` check (same
                    // message; span = the initializer EXPRESSION's span, which is this
                    // op's own span). The operand indexes the chunk's `type_consts`
                    // side-pool. The compiler emits CHECK_LOCAL only for an annotated
                    // binding, so a type is always present at this index.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let ty = match fiber.frame().closure.proto.chunk.type_consts.get(idx) {
                        Some(ty) => ty.clone(),
                        None => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("CHECK_LOCAL type-const index {idx} out of range"),
                            ))
                        }
                    };
                    let v = fiber.peek(0).clone();
                    if !crate::interp::check_type(&v, &ty) {
                        // §6.3 paranoid: let initializer span in `lets` set.
                        if let Some(e) = self.interp.maybe_paranoid_escalate(&ty, &v, span) {
                            return Err(e);
                        }
                        return Err(crate::interp::contract_panic(&ty, &v, span));
                    }
                }

                Op::NewArray => {
                    // Pop `n` elements (pushed in source order, so the last
                    // pushed is on top) into a Vec preserving source order, then
                    // push `Value::array_cell`. Matches the tree-walker's
                    // `ExprKind::Array` construction (`Rc<RefCell<Vec>>`).
                    let n = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let mut values = vec![Value::nil(); n];
                    for slot in values.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    fiber.push(Value::array(values));
                }

                Op::NewObject => {
                    // SHAPE Task 3.1/3.2: shared body for both lanes (see
                    // `exec_new_object`) — byte-identical to the sync lane and to
                    // the tree-walker's `ExprKind::Object`. `fault_ip` is the op
                    // offset; the operand is the pair count.
                    let n = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    self.exec_new_object(fiber, fault_ip, n)?;
                }

                Op::NewMap => {
                    // Push a fresh, empty `Value::map_cell`. The `#{…}` builder runs one
                    // `MAP_ENTRY` per entry after this (or nothing for `#{}`).
                    fiber.push(Value::map(indexmap::IndexMap::new()));
                }

                Op::MapEntry => {
                    // `[map, key, val] -- [map]` — convert `key` to a `MapKey` and
                    // insert later-wins into the builder `map`. Byte-identical to the
                    // tree-walker's `ExprKind::Map`: an unhashable key is the SAME
                    // Tier-2 panic `cannot use {type} as a map key`, anchored at this
                    // op's span (the key's trivia-trimmed code span).
                    let val = fiber.pop();
                    let key_val = fiber.pop();
                    let key = match crate::value::MapKey::from_value(&key_val) {
                        Some(k) => k,
                        None => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "cannot use {} as a map key",
                                    crate::interp::type_name(&key_val)
                                ),
                            ))
                        }
                    };
                    match fiber.peek(0).kind() {
                        ValueKind::Map(m) => {
                            m.borrow_mut().insert(key, val);
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "MAP_ENTRY target is not a map: {}",
                                    crate::interp::type_name(fiber.peek(0))
                                ),
                            ))
                        }
                    }
                }

                Op::Spread | Op::SpreadArgs => {
                    // `[arr, operand] -- [arr]` — flatten the spread `operand` (an
                    // Array) into the under-construction array `arr` below it.
                    // Mirrors the tree-walker's `ExprKind::Array` / `eval_call_args`
                    // spread arm: a non-array is the SAME Tier-2 panic, anchored at
                    // this op's span (the operand's trivia-trimmed code span). The
                    // ONLY difference between SPREAD and SPREAD_ARGS is the message
                    // ("into an array" vs "as call arguments").
                    let operand = fiber.pop();
                    match operand.kind() {
                        ValueKind::Array(src) => {
                            // Clone elements out FIRST so a self-spread (`[...a]`
                            // where `arr` aliased `a`) cannot observe a borrow
                            // conflict, then extend the builder array.
                            let items: Vec<Value> = src.borrow().iter().cloned().collect();
                            match fiber.peek(0).kind() {
                                ValueKind::Array(arr) => arr.borrow_mut().extend(items),
                                _ => {
                                    return Err(self.panic_at(
                                        fiber,
                                        fault_ip,
                                        format!(
                                            "SPREAD target is not an array: {}",
                                            crate::interp::type_name(fiber.peek(0))
                                        ),
                                    ))
                                }
                            }
                        }
                        _ => {
                            let msg = if matches!(op, Op::SpreadArgs) {
                                format!(
                                    "can only spread an array as call arguments, got {}",
                                    crate::interp::type_name(&operand)
                                )
                            } else {
                                format!(
                                    "can only spread an array into an array, got {}",
                                    crate::interp::type_name(&operand)
                                )
                            };
                            return Err(self.panic_at(fiber, fault_ip, msg));
                        }
                    }
                }

                Op::AppendArray => {
                    // `[arr, item] -- [arr]` — push one `item` onto the builder
                    // array `arr` below it.
                    let item = fiber.pop();
                    match fiber.peek(0).kind() {
                        ValueKind::Array(arr) => arr.borrow_mut().push(item),
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "APPEND_ARRAY target is not an array: {}",
                                    crate::interp::type_name(fiber.peek(0))
                                ),
                            ))
                        }
                    }
                }

                Op::AppendObject => {
                    // SHAPE Task 3.1: `[obj, key, val] -- [obj]` — insert via
                    // vm_object_insert (precise registry transition, works in both
                    // slab and dict mode). Later-wins + first-position, byte-identical
                    // to the tree-walker's `ExprKind::Object`.
                    let val = fiber.pop();
                    let key = match fiber.pop().into_kind() {
                        OwnedKind::Str(s) => s,
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("APPEND_OBJECT key is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    match fiber.peek(0).kind() {
                        ValueKind::Object(obj) => {
                            let obj = obj.clone();
                            self.vm_object_insert(&obj, &key, val);
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "APPEND_OBJECT target is not an object: {}",
                                    crate::interp::type_name(fiber.peek(0))
                                ),
                            ))
                        }
                    }
                }

                Op::SpreadObject => {
                    // SHAPE Task 3.1: `[obj, operand] -- [obj]` — snapshot source
                    // entries via the accessor (works across slab/dict), then insert
                    // each via vm_object_insert. The non-object spread is the SAME
                    // Tier-2 panic, anchored at this op's span; entries insert
                    // later-wins/first-pos — byte-identical to the tree-walker.
                    let operand = fiber.pop();
                    match operand.kind() {
                        ValueKind::Object(src) => {
                            // Snapshot FIRST (avoids borrow conflict on self-spread).
                            let entries = src.entries();
                            match fiber.peek(0).kind() {
                                ValueKind::Object(obj) => {
                                    let obj = obj.clone();
                                    for (k, v) in entries {
                                        self.vm_object_insert(&obj, &k, v);
                                    }
                                }
                                _ => {
                                    return Err(self.panic_at(
                                        fiber,
                                        fault_ip,
                                        format!(
                                            "SPREAD_OBJECT target is not an object: {}",
                                            crate::interp::type_name(fiber.peek(0))
                                        ),
                                    ))
                                }
                            }
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "can only spread an object into an object, got {}",
                                    crate::interp::type_name(&operand)
                                ),
                            ))
                        }
                    }
                }

                Op::GetIndex => {
                    // `obj idx -- obj[idx]`. The two operands were pushed
                    // obj-then-idx, so pop idx first. The shared `index_get`
                    // dispatch (with the tree-walker) anchors every panic at the
                    // op's span; the VM has a single instruction span, so it is
                    // passed for both the receiver-span and index-span parameters.
                    let idx = fiber.pop();
                    let obj = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let v = crate::interp::index_get(&obj, &idx, span, span)?;
                    fiber.push(v);
                }

                Op::SetIndex => {
                    // SHAPE Task 3.1: for Object receivers, use vm_object_insert
                    // directly (avoids borrow_mut panic on slab mode). Array and
                    // error paths stay on the shared index_set (dict-only objects
                    // from the tree-walker). Frozen check and error messages are
                    // preserved byte-for-byte.
                    let val = fiber.pop();
                    let idx = fiber.pop();
                    let obj = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let v = match obj.kind() {
                        ValueKind::Object(cell) => {
                            // Frozen guard (mirrors index_set's frozen_kind check).
                            if let Some(kind) = crate::value::frozen_kind(&obj) {
                                return Err(Control::Panic(AsError::at(
                                    format!("cannot mutate a frozen {kind}"),
                                    span,
                                )));
                            }
                            match idx.kind() {
                                ValueKind::Str(key) => {
                                    let key = key.clone();
                                    self.vm_object_insert(cell, &key, val.clone());
                                    val
                                }
                                _ => {
                                    return Err(Control::Panic(AsError::at(
                                        "object index must be a string",
                                        span,
                                    )));
                                }
                            }
                        }
                        _ => crate::interp::index_set(&obj, &idx, val, span, span)
                            .map_err(Control::Panic)?,
                    };
                    fiber.push(v);
                }

                Op::GetProp | Op::GetPropOpt => {
                    // `obj -- obj.<name>` (the optional form short-circuits to
                    // `nil` when the receiver is `nil`). `read_member` is the SAME
                    // member-access dispatch the tree-walker runs (fields, methods
                    // → BoundMethod, enum variants, native handles, nil-receiver
                    // errors), so the two engines cannot drift.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("GET_PROP operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let obj = fiber.pop();
                    if op == Op::GetPropOpt && obj == Value::nil() {
                        // `?.` short-circuit guard: a nil receiver never consults
                        // the IC (and never resolves a field), matching the generic
                        // path's nil short-circuit exactly.
                        fiber.push(Value::nil());
                    } else {
                        let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                        // Try the field inline cache (fast path for FIELD reads on a
                        // shaped Object/Instance). On a hit it returns the cached
                        // field's value, which is byte-identical to `vm_read_member`
                        // (whose Object/Instance field arm clones the same stored
                        // value). On a miss it returns `None` and we fall to the SAME
                        // generic member read — which also handles methods (→
                        // BoundMethod), non-shaped receivers, enum/native/nil, etc.
                        // Resolve `proto` out of the fiber so the chunk borrow does
                        // not collide with the later `fiber.push`.
                        let proto = fiber.frame().closure.proto.clone();
                        // KILL SWITCH (V11-T5): only consult/record the field IC
                        // when specialization is ON. Generic mode always takes the
                        // shared `vm_read_member` path (same value, byte-identical).
                        let cached = if self.specialize {
                            self.ic_get_field(&proto.chunk, fault_ip, &obj, &name)
                        } else {
                            None
                        };
                        let v = match cached {
                            Some(v) => v,
                            None => self.vm_read_member(&obj, &name, span)?,
                        };
                        fiber.push(v);
                    }
                }

                Op::CheckNumbers => {
                    // Peek-only bounds guard for for-range: the top two stack
                    // values (start below, end on top) must both be numbers.
                    // Leaves them in place so the surrounding lowering can store
                    // them into slots. The op's span is the START bound's span, so
                    // the panic is byte-identical to the tree-walker's
                    // `Stmt::ForRange` ("for-range bounds must be numbers" at
                    // `start.span`).
                    let end_ok = fiber.peek(0).is_number();
                    let start_ok = fiber.peek(1).is_number();
                    if !(end_ok && start_ok) {
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            "for-range bounds must be numbers".to_string(),
                        ));
                    }
                }

                Op::IterSnapshot => {
                    // Materialize the SYNC for-of snapshot from the iterable on
                    // TOS. Byte-identical to the tree-walker's `Stmt::ForOf` (sync,
                    // `for_await == false`) `items` build: an `Array` snapshots a
                    // CLONE of its current elements (so the iteration is fixed even
                    // if the body mutates the source array), a `Str` snapshots its
                    // chars each as a 1-char string, and ANYTHING ELSE — including
                    // object/map/set, which are NOT iterable in sync for-of —
                    // raises the Tier-2 panic at this op's span (the iterable
                    // expression's trivia-trimmed code span), exactly like
                    // `AsError::at(format!("value of type {} is not iterable", ...))`.
                    let iterable = fiber.pop();
                    let items: Vec<Value> = match iterable.kind() {
                        ValueKind::Array(arr) => arr.borrow().clone(),
                        ValueKind::Str(s) => s
                            .chars()
                            .map(|c| Value::str(c.to_string()))
                            .collect(),
                        // SRV §3.5: a frozen `Shared` array/string/set iterates
                        // zero-copy, byte-identical to the tree-walker's `ForOf`.
                        ValueKind::Shared(node) => match crate::interp::shared_iter_values(node) {
                            Some(items) => items,
                            None => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "value of type {} is not iterable",
                                        crate::interp::type_name(&iterable)
                                    ),
                                ))
                            }
                        },
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "value of type {} is not iterable",
                                    crate::interp::type_name(&iterable)
                                ),
                            ))
                        }
                    };
                    fiber.push(Value::array(items));
                }

                Op::ArrayLen => {
                    // Pop a (compiler-produced) snapshot array and push its element
                    // count as a `Number`. The operand is never user input — the
                    // compiler emits this only over an `IterSnapshot` result — so a
                    // non-array is a compiler bug surfaced as a Tier-2 panic.
                    let v = fiber.pop();
                    match v.kind() {
                        ValueKind::Array(arr) => {
                            let len = arr.borrow().len();
                            fiber.push(Value::float(len as f64));
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("ARRAY_LEN operand is not an array: {other:?}"),
                            ))
                        }
                    }
                }

                Op::Closure => {
                    // Build a closure over a nested proto, capturing its upvalues per
                    // the proto's capture plan (`proto.chunk.upvalues`, indexed by
                    // upvalue number):
                    //   - ParentLocal { slot, by_value: false }: BY REFERENCE — clone
                    //     the CURRENT frame's cell `Cc` for that slot, so the closure
                    //     sees later mutation. The resolver guarantees a `mutated`
                    //     captured local is a cell slot, so `cells[slot]` is `Some`; a
                    //     `None` is a compiler/resolver bug (clear panic).
                    //   - ParentLocal { slot, by_value: true } (SP8 #136): BY VALUE —
                    //     the source binding is never reassigned, so its slot is a PLAIN
                    //     stack local (no cell in the declaring frame). Copy the slot's
                    //     value into a FRESH private cell owned solely by this closure.
                    //     Per-iteration loop freshness is automatic: each iteration's
                    //     Op::Closure copies that iteration's slot value. Byte-identical
                    //     to a shared cell (the value can never change after capture).
                    //   - ParentUpvalue(idx): clone the CURRENT closure's upvalue cell
                    //     (a transitive capture; keeps the source's representation).
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let proto = fiber.frame().closure.proto.chunk.protos[idx].clone();
                    let mut upvalues = Vec::with_capacity(proto.chunk.upvalues.len());
                    for desc in &proto.chunk.upvalues {
                        let cell = match *desc {
                            crate::syntax::resolve::types::UpvalueDescriptor::ParentLocal {
                                slot,
                                by_value: false,
                            } => fiber
                                .frame()
                                .cells
                                .get(slot as usize)
                                .and_then(|c| c.as_ref())
                                .unwrap_or_else(|| {
                                    panic!(
                                        "CLOSURE captures parent local slot {slot} that is not a cell (compiler/resolver bug)"
                                    )
                                })
                                .clone(),
                            crate::syntax::resolve::types::UpvalueDescriptor::ParentLocal {
                                slot,
                                by_value: true,
                            } => {
                                let v = fiber.local(slot as usize).clone();
                                gcmodule::Cc::new(std::cell::RefCell::new(v))
                            }
                            crate::syntax::resolve::types::UpvalueDescriptor::ParentUpvalue(up) => {
                                fiber.frame().closure.upvalues[up as usize].clone()
                            }
                        };
                        upvalues.push(cell);
                    }
                    let closure = crate::vm::value_ext::Closure::with_upvalues(proto, upvalues);
                    fiber.push(Value::closure(closure));
                }

                Op::GetLocalCell => {
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.get_local_cell(slot);
                    fiber.push(v);
                }
                Op::SetLocalCell => {
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.pop();
                    fiber.set_local_cell(slot, v);
                }
                Op::FreshCell => {
                    // Install a brand-new heap cell into this slot, dropping the
                    // frame's ref to the previous cell (any closure that captured
                    // it keeps its own `Rc`, so it retains that iteration's value).
                    // Emitted at the top of each loop iteration for per-iteration
                    // capture freshness.
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    fiber.fresh_cell(slot);
                }

                Op::Import => {
                    // Read the descriptor (cloned out of the chunk so no chunk borrow
                    // is held). For a `std/*` source resolve via the SAME
                    // `load_std_module` the tree-walker uses (V12-T1); for a FILE
                    // source (`./mod`, `../mod`, …) resolve+compile/load+run the file
                    // module on the VM and bind its `export`ed values (V12-T4). The op
                    // leaves nothing on the stack — byte-identical to the tree-walker's
                    // `Stmt::Import` arm.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let desc = fiber.frame().closure.proto.chunk.imports[idx].clone();
                    let source = desc.source().to_string();

                    // Resolve the export accessor once: a closure that, given an export
                    // name, returns its value (or None if absent), plus an ordered list
                    // of export names for the namespace form. For std we wrap the
                    // `ModuleEntry`; for a file module we use its IndexMap directly.
                    //
                    // SP6 §6: the SHARED `classify_specifier` drives the split (the
                    // SAME helper the tree-walker's `Stmt::Import` uses, so the two
                    // engines route identically). `Std` → the static registry;
                    // `Relative`/`Package` → the SAME `load_file_module` (a
                    // package's resolved `target` is an absolute path, so
                    // `module_dir.join(target)` yields `target` unchanged and
                    // package-internal `./` imports still resolve within the store
                    // root); `UnknownPackage` → a Tier-2 error, message identical
                    // to the tree-walker. The owned `target` string is taken out of
                    // the resolver borrow before this `.await`.
                    let exports: ModuleExports =
                        match self.interp.classify_specifier(&source) {
                            crate::interp::SpecifierKind::Std => {
                                let entry = self.interp.import_std(&source)?;
                                // Materialize the std module's exports into an ordered
                                // map so both import forms share one code path. The std
                                // export set is unordered (a HashSet); order is
                                // irrelevant for the named form and matches the
                                // tree-walker's unordered namespace object.
                                let mut m = indexmap::IndexMap::new();
                                for name in entry.exports.borrow().iter() {
                                    m.insert(
                                        name.clone(),
                                        entry.env.get(name).unwrap_or(Value::nil()),
                                    );
                                }
                                Rc::new(RefCell::new(m))
                            }
                            crate::interp::SpecifierKind::Relative(_) => {
                                self.load_file_module(&source, fault_ip, fiber).await?
                            }
                            crate::interp::SpecifierKind::Package { target, .. } => {
                                let target = target.to_string_lossy().into_owned();
                                self.load_file_module(&target, fault_ip, fiber).await?
                            }
                            crate::interp::SpecifierKind::UnknownPackage(key) => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "unknown package '{key}' — add it with 'ascript add'"
                                    ),
                                ));
                            }
                        };

                    match desc {
                        crate::vm::chunk::ImportDesc::Named { source, names } => {
                            for (name, slot, is_cell, is_global) in names {
                                let v = {
                                    let ex = exports.borrow();
                                    match ex.get(&name) {
                                        Some(v) => v.clone(),
                                        None => {
                                            drop(ex);
                                            return Err(self.panic_at(
                                                fiber,
                                                fault_ip,
                                                format!("module '{source}' has no export '{name}'"),
                                            ));
                                        }
                                    }
                                };
                                if is_global {
                                    // An imported name is an IMMUTABLE module global
                                    // (tree-walker `define(..., false)`).
                                    self.define_user_global(Rc::from(name.as_str()), v, false);
                                } else if is_cell {
                                    fiber.set_local_cell(slot as usize, v);
                                } else {
                                    fiber.set_local(slot as usize, v);
                                }
                            }
                        }
                        crate::vm::chunk::ImportDesc::Namespace {
                            alias,
                            slot,
                            is_cell,
                            is_global,
                            ..
                        } => {
                            let map = exports.borrow().clone();
                            let ns = Value::object(map);
                            // SHAPE §3.5: count this fresh-dict namespace-import build.
                            #[cfg(any(test, feature = "fuzzgen", fuzzing))]
                            self.bump_shape_stat(|s| s.obj_dict_constructed += 1);
                            if is_global {
                                // A namespace alias is an IMMUTABLE module global.
                                self.define_user_global(Rc::from(alias.as_str()), ns, false);
                            } else if is_cell {
                                fiber.set_local_cell(slot as usize, ns);
                            } else {
                                fiber.set_local(slot as usize, ns);
                            }
                        }
                    }
                }

                Op::DefineExport => {
                    // `value -- `. Pop the exported binding's value and record it
                    // under its name (`consts[idx]`, a Str) in the CURRENT module's
                    // export map. Mirrors the tree-walker's `Stmt::Export`. When the
                    // top-level chunk is the entry program the recorded map is a
                    // throwaway (its exports are unused), exactly as the tree-walker
                    // discards the main program's `current_exports`.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name_const = &fiber.frame().closure.proto.chunk.consts[idx];
                    let name = match name_const.kind() {
                        ValueKind::Str(s) => s.to_string(),
                        _ => {
                            let ty = crate::interp::type_name(name_const);
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("DEFINE_EXPORT name const is not a string: {ty}"),
                            ));
                        }
                    };
                    let v = fiber.pop();
                    self.module_exports.borrow().borrow_mut().insert(name, v);
                }

                Op::CheckArrayDestructure => {
                    // Peek the RHS on TOS and validate it is an Array, exactly like
                    // the tree-walker's `Stmt::LetDestructure` type check (which runs
                    // ONCE before binding any name). Leaves the source in place so the
                    // surrounding lowering can stash it in a temp slot.
                    if !matches!(fiber.peek(0).kind(), ValueKind::Array(_)) {
                        let t = crate::interp::type_name(fiber.peek(0));
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            format!("cannot destructure a non-array value of type {t}"),
                        ));
                    }
                }

                Op::CheckObjectDestructure => {
                    // Peek the RHS on TOS and validate it is an Object or Instance,
                    // exactly like the tree-walker's `Stmt::LetDestructureObject` type
                    // check. Leaves the source in place.
                    if !matches!(fiber.peek(0).kind(), ValueKind::Object(_) | ValueKind::Instance(_)) {
                        let t = crate::interp::type_name(fiber.peek(0));
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            format!("cannot destructure a non-object value of type {t}"),
                        ));
                    }
                }

                Op::ArrayElem => {
                    // `src -- src[index]`. Pop the (already-validated) array and push
                    // the element at `index`, or `nil` for an out-of-bounds position
                    // (positions past the length bind nil — `items.get(i).cloned()
                    // .unwrap_or(Value::nil())`).
                    let index = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let src = fiber.pop();
                    match src.kind() {
                        ValueKind::Array(arr) => {
                            let v = arr.borrow().get(index).cloned().unwrap_or(Value::nil());
                            fiber.push(v);
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("ARRAY_ELEM operand is not an array: {other:?}"),
                            ))
                        }
                    }
                }

                Op::ObjectKey => {
                    // `src -- src[key]` where `key = consts[idx]`. Pop the
                    // (already-validated) Object/Instance and push the value under
                    // `key`, or `nil` if absent. Mirrors the tree-walker's destructure
                    // `get` closure EXACTLY: an Instance reads only its `fields` (it
                    // does NOT fall back to methods like `read_member` would).
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let key = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("OBJECT_KEY operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let src = fiber.pop();
                    let v = match src.kind() {
                        ValueKind::Object(o) => {
                            o.get(key.as_ref()).unwrap_or(Value::nil())
                        }
                        ValueKind::Instance(i) => i
                            .borrow()
                            .get(key.as_ref())
                            .unwrap_or(Value::nil()),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("OBJECT_KEY operand is not an object: {other:?}"),
                            ))
                        }
                    };
                    fiber.push(v);
                }

                Op::ArrayRest => {
                    // `src -- src[start..]`. Pop the (already-validated) array and push
                    // a NEW array of its elements from `start` to the end — the `...rest`
                    // collector (`items.iter().skip(names.len())`).
                    let start = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let src = fiber.pop();
                    match src.kind() {
                        ValueKind::Array(arr) => {
                            let tail: Vec<Value> =
                                arr.borrow().iter().skip(start).cloned().collect();
                            fiber.push(Value::array(tail));
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("ARRAY_REST operand is not an array: {other:?}"),
                            ))
                        }
                    }
                }

                Op::ObjectRest => {
                    // `src -- leftover` where `consts[idx]` is an Array of the bound
                    // key strings. Pop the (already-validated) Object/Instance and push
                    // a NEW object of its entries whose key is NOT bound, in source
                    // order — the object-rest collector (excludes already-bound SOURCE
                    // keys).
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let bound: std::collections::HashSet<Rc<str>> =
                        match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                            ValueKind::Array(keys) => keys
                                .borrow()
                                .iter()
                                .filter_map(|v| match v.kind() {
                                    ValueKind::Str(s) => Some(s.clone()),
                                    _ => None,
                                })
                                .collect(),
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("OBJECT_REST operand is not a key array: {other:?}"),
                                ))
                            }
                        };
                    let src = fiber.pop();
                    let mut remaining: indexmap::IndexMap<String, Value> =
                        indexmap::IndexMap::new();
                    match src.kind() {
                        ValueKind::Object(o) => {
                            for (k, v) in o.entries() {
                                if !bound.contains(k.as_ref()) {
                                    remaining.insert(k.to_string(), v);
                                }
                            }
                        }
                        ValueKind::Instance(i) => {
                            for (k, v) in i.borrow().entries() {
                                if !bound.contains(k.as_ref()) {
                                    remaining.insert(k.to_string(), v);
                                }
                            }
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("OBJECT_REST operand is not an object: {other:?}"),
                            ))
                        }
                    }
                    fiber.push(Value::object(remaining));
                    // SHAPE §3.5: count this fresh-dict OBJECT_REST build.
                    #[cfg(any(test, feature = "fuzzgen", fuzzing))]
                    self.bump_shape_stat(|s| s.obj_dict_constructed += 1);
                }

                Op::MatchArray => {
                    // `subject -- ok:bool`. Pop the subject; push whether it is an
                    // Array whose length is exactly `len` (exact == 1) or at least
                    // `len` (exact == 0, the `...rest` case). A non-array → false.
                    // Mirrors the tree-walker's `Pattern::Array` length/type guard.
                    let len = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let exact = fiber.frame().closure.proto.chunk.read_u8(operand_at + 2) == 1;
                    let subject = fiber.pop();
                    let ok = match subject.kind() {
                        ValueKind::Array(a) => {
                            let n = a.borrow().len();
                            if exact {
                                n == len
                            } else {
                                n >= len
                            }
                        }
                        _ => false,
                    };
                    fiber.push(Value::bool_(ok));
                }

                Op::MatchObject => {
                    // `subject -- ok:bool`. Pop the subject; push whether it is an
                    // Object or Instance. Mirrors the head guard of the tree-walker's
                    // `Pattern::Object` (any other value is a structural mismatch).
                    let subject = fiber.pop();
                    let ok = matches!(subject.kind(), ValueKind::Object(_) | ValueKind::Instance(_));
                    fiber.push(Value::bool_(ok));
                }

                Op::MatchHasKey => {
                    // `subject -- ok:bool`. Pop the subject (an Object/Instance per
                    // `MatchObject`) and push whether it has the field `consts[idx]`.
                    // Mirrors the per-entry `fields.get(key)` presence check. Popping
                    // (not peeking) avoids orphaning the subject on a missing-key
                    // fail-jump; the matched path reloads the subject temp.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let key = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "MATCH_HAS_KEY operand is not a string constant: {other:?}"
                                ),
                            ))
                        }
                    };
                    let subject = fiber.pop();
                    let ok = match subject.kind() {
                        ValueKind::Object(o) => o.contains_key(key.as_ref()),
                        ValueKind::Instance(i) => i.borrow().contains_key(key.as_ref()),
                        _ => false,
                    };
                    fiber.push(Value::bool_(ok));
                }

                Op::MatchVariant => {
                    // ADT: `subject -- ok:bool`. Pop the subject; push whether it is a
                    // CONSTRUCTED `EnumVariant` (payload present, not a constructor)
                    // matching the variant name (and enum name, if the const carries
                    // one). The const is `[variantName:Str, enumNameOrNil]`. Mirrors the
                    // head tag-test of the tree-walker's `match_variant_pattern`.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let (want_variant, want_enum) =
                        match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                            ValueKind::Array(a) => {
                                let b = a.borrow();
                                let v = match b.first().map(Value::kind) {
                                    Some(ValueKind::Str(s)) => s.clone(),
                                    _ => {
                                        return Err(self.panic_at(
                                            fiber,
                                            fault_ip,
                                            "MATCH_VARIANT operand[0] is not a string".to_string(),
                                        ))
                                    }
                                };
                                let e = match b.get(1).map(Value::kind) {
                                    Some(ValueKind::Str(s)) => Some(s.clone()),
                                    _ => None,
                                };
                                (v, e)
                            }
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("MATCH_VARIANT operand is not an array: {other:?}"),
                                ))
                            }
                        };
                    let subject = fiber.pop();
                    let ok = match subject.kind() {
                        ValueKind::EnumVariant(ev) if ev.payload.is_some() => {
                            ev.name.as_str() == want_variant.as_ref()
                                && want_enum
                                    .as_ref()
                                    .map(|e| ev.enum_name.as_str() == e.as_ref())
                                    .unwrap_or(true)
                        }
                        _ => false,
                    };
                    fiber.push(Value::bool_(ok));
                }

                Op::MatchVariantArity => {
                    // ADT: `subject -- ok:bool`. Pop the subject; push whether its
                    // payload has exactly `n` values. Mirrors the positional length
                    // guard `items.len() != pats.len()`.
                    let n = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let subject = fiber.pop();
                    let len = variant_payload_len(&subject);
                    fiber.push(Value::bool_(len == Some(n)));
                }

                Op::MatchVariantHasField => {
                    // ADT: `subject -- ok:bool`. Pop the subject; push whether it has a
                    // NAMED payload field `consts[idx]`. Mirrors the named-destructure
                    // presence check.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let key = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("MATCH_VARIANT_HAS_FIELD operand is not a string: {other:?}"),
                            ))
                        }
                    };
                    let subject = fiber.pop();
                    let ok = match subject.kind() {
                        ValueKind::EnumVariant(ev) => match &ev.payload {
                            Some(crate::value::Payload::Named(o)) => {
                                o.contains_key(key.as_ref())
                            }
                            _ => false,
                        },
                        _ => false,
                    };
                    fiber.push(Value::bool_(ok));
                }

                Op::VariantElem => {
                    // ADT: `subject -- value`. Pop a constructed variant; push its
                    // `idx`-th payload value IN DECLARATION ORDER (positional element,
                    // or named field value in insertion order). Mirrors the tree-walker
                    // positional destructure.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let subject = fiber.pop();
                    let v = match subject.kind() {
                        ValueKind::EnumVariant(ev) => match &ev.payload {
                            Some(crate::value::Payload::Positional(a)) => {
                                a.borrow().get(idx).cloned().unwrap_or(Value::nil())
                            }
                            Some(crate::value::Payload::Named(o)) => {
                                o.get_index(idx).map(|(_, v)| v).unwrap_or(Value::nil())
                            }
                            None => Value::nil(),
                        },
                        _ => Value::nil(),
                    };
                    fiber.push(v);
                }

                Op::VariantField => {
                    // ADT: `subject -- value`. Pop a constructed variant; push its
                    // NAMED payload field `consts[idx]` (presence already checked).
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let key = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("VARIANT_FIELD operand is not a string: {other:?}"),
                            ))
                        }
                    };
                    let subject = fiber.pop();
                    let v = match subject.kind() {
                        ValueKind::EnumVariant(ev) => match &ev.payload {
                            Some(crate::value::Payload::Named(o)) => {
                                o.get(key.as_ref()).unwrap_or(Value::nil())
                            }
                            _ => Value::nil(),
                        },
                        _ => Value::nil(),
                    };
                    fiber.push(v);
                }

                Op::MatchRange => {
                    // `subject lo hi step -- ok:bool` (step on top). flags bit0 =
                    // inclusive, bit1 = step PRESENT. Pop all four; push whether the
                    // subject is a Number that matches the range. With step OMITTED
                    // (placeholder `nil`) this is the plain in-bounds test; with step
                    // PRESENT it is strided membership (spec §3.7) anchored at `lo`,
                    // via the SHARED `resolve_step` (validates → PANICS on
                    // zero/non-finite/mismatch, byte-identical to iteration) +
                    // `range_pattern_contains`. A non-number subject OR bound → false
                    // (a non-panic mismatch), mirroring the tree-walker exactly.
                    let flags = fiber.frame().closure.proto.chunk.read_u8(operand_at);
                    let inclusive = (flags & 0b01) != 0;
                    let present = (flags & 0b10) != 0;
                    let step = fiber.pop();
                    let hi = fiber.pop();
                    let lo = fiber.pop();
                    let subject = fiber.pop();
                    // NUM §4: a number subject/bound is Int OR Float (exact-on-f64
                    // membership); a non-number subject/bound is a non-panic mismatch.
                    let ok = match (subject.as_f64(), lo.as_f64(), hi.as_f64()) {
                        (Some(n), Some(lo), Some(hi)) => {
                            let step_v = if present {
                                match step.as_f64() {
                                    Some(s) => Some(s),
                                    None => {
                                        return Err(self.panic_at(
                                            fiber,
                                            fault_ip,
                                            "range step must be a number".to_string(),
                                        ))
                                    }
                                }
                            } else {
                                None
                            };
                            // Validate an EXPLICIT step (PANICS on a bad step, at
                            // this op's span = the START bound's), then test
                            // membership. A plain pattern (step omitted) keeps its
                            // no-stride behavior via the raw `Option`.
                            if step_v.is_some() {
                                let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                                crate::interp::resolve_step(lo, hi, step_v, span)?;
                            }
                            crate::interp::range_pattern_contains(n, lo, hi, step_v, inclusive)
                        }
                        _ => false,
                    };
                    fiber.push(Value::bool_(ok));
                }

                Op::MatchNoArm => {
                    // No arm matched: raise the Tier-2 panic at this op's span (the
                    // `MatchExpr`'s code span), byte-identical to the tree-walker's
                    // `AsError::at("no matching arm in match expression", expr.span)`.
                    return Err(self.panic_at(
                        fiber,
                        fault_ip,
                        "no matching arm in match expression".to_string(),
                    ));
                }

                Op::GetUpvalue => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.frame().closure.upvalues[idx].borrow().clone();
                    fiber.push(v);
                }
                Op::SetUpvalue => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.pop();
                    *fiber.frame().closure.upvalues[idx].borrow_mut() = v;
                }

                // DEFER §5.2: capture callee + args at statement time; push a
                // DeferEntry onto the current frame's defer list. Execution is
                // deferred to frame exit (Op::Return / Op::Propagate drain).
                // Operand layout: u8 flags (bit0=awaited, bit1=spread) + u8 argc.
                Op::DeferPush => {
                    let flags = fiber.frame().closure.proto.chunk.read_u8(operand_at);
                    let argc = fiber.frame().closure.proto.chunk.read_u8(operand_at + 1) as usize;
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let awaited = (flags & 1) != 0;
                    let spread = (flags & 2) != 0;
                    // Pop args (stack is callee, arg0, arg1, … argN-1 top; pop in reverse).
                    let args: Vec<Value> = if spread {
                        // Spread args: single array/spread was compiled; read it.
                        let arr = fiber.pop();
                        match arr.kind() {
                            ValueKind::Array(a) => a.borrow().clone(),
                            _ => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "defer spread requires an array, got {}",
                                        crate::interp::type_name(&arr)
                                    ),
                                ))
                            }
                        }
                    } else {
                        let mut v = vec![Value::nil(); argc];
                        for slot in v.iter_mut().rev() {
                            *slot = fiber.pop();
                        }
                        v
                    };
                    let callee = fiber.pop();
                    fiber.frame_mut().defers.push(crate::interp::DeferEntry {
                        kind: crate::interp::DeferKind::Call { callee },
                        args,
                        awaited,
                        span,
                    });
                    #[cfg(any(test, feature = "fuzzgen", fuzzing))]
                    crate::vm::defer_metrics::defer_metrics::ENTRIES_PUSHED
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }

                // DEFER §5.2: method-call variant. Captures receiver + method name +
                // args; execution deferred to frame exit.
                // Operand layout: u16 name_idx + u8 flags + u8 argc.
                Op::DeferPushMethod => {
                    let name_idx =
                        fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let flags = fiber.frame().closure.proto.chunk.read_u8(operand_at + 2);
                    let argc = fiber.frame().closure.proto.chunk.read_u8(operand_at + 3) as usize;
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let awaited = (flags & 1) != 0;
                    let spread = (flags & 2) != 0;
                    let name_const = &fiber.frame().closure.proto.chunk.consts[name_idx];
                    let name = match name_const.kind() {
                        ValueKind::Str(s) => s.clone(),
                        _ => {
                            let ty = crate::interp::type_name(name_const);
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("defer method name must be a string, got {ty}"),
                            ));
                        }
                    };
                    let args: Vec<Value> = if spread {
                        let arr = fiber.pop();
                        match arr.kind() {
                            ValueKind::Array(a) => a.borrow().clone(),
                            _ => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "defer spread requires an array, got {}",
                                        crate::interp::type_name(&arr)
                                    ),
                                ))
                            }
                        }
                    } else {
                        let mut v = vec![Value::nil(); argc];
                        for slot in v.iter_mut().rev() {
                            *slot = fiber.pop();
                        }
                        v
                    };
                    let recv = fiber.pop();
                    // For VM-compiled class instances, methods are stored in the VM's
                    // `class_methods` side table — NOT in `Class.methods` (which is
                    // empty for VM classes). The tree-walker's `read_member` uses
                    // `find_method` on `Class.methods` and therefore cannot dispatch to
                    // VM-compiled methods at drain time. Resolve non-hook receivers to
                    // a `BoundMethod` NOW (at capture time, matching spec §3.1 semantics
                    // since the receiver is captured at statement time anyway) so that
                    // `exec_defer_entry` uses the `DeferKind::Call` arm → `call_value`
                    // which correctly dispatches VM `BoundMethod`s.
                    //
                    // Hook receivers (schema, workflow-ctx, shared, actor, worker-class
                    // spawn) MUST be kept as `DeferKind::Method` so that
                    // `call_method_recv`'s hook dispatch fires at drain time (spec §3.1
                    // — pre-binding would silently skip the hooks).
                    let kind = if self.interp.member_call_is_hook(&recv, &name) {
                        crate::interp::DeferKind::Method { recv, name }
                    } else {
                        // Resolve the method to a BoundMethod/Closure/etc. now.
                        // If the lookup fails (e.g. the method does not exist), surface
                        // the error immediately at defer-statement time (consistent with
                        // how the tree-walker would surface it on the read).
                        let callee = match self.vm_read_member(&recv, &name, span) {
                            Ok(v) => v,
                            Err(e) => return Err(e),
                        };
                        crate::interp::DeferKind::Call { callee }
                    };
                    fiber.frame_mut().defers.push(crate::interp::DeferEntry {
                        kind,
                        args,
                        awaited,
                        span,
                    });
                    #[cfg(any(test, feature = "fuzzgen", fuzzing))]
                    crate::vm::defer_metrics::defer_metrics::ENTRIES_PUSHED
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }

                Op::Return => {
                    // Pop the result and unwind one frame, returning that value to
                    // the caller (or ending the program if this was the root frame).
                    // DEFER §3.3: drain the current frame's defers BEFORE applying
                    // the return-type contract — mirrors run_body drain order. Take
                    // the list with mem::take to satisfy the no-borrow-across-await
                    // invariant, then run_defers, then fall through to return_from_frame.
                    let result = fiber.pop();
                    let defers = std::mem::take(&mut fiber.frame_mut().defers);
                    if !defers.is_empty() {
                        let mut outcome: Result<Value, Control> = Ok(result);
                        self.vm_run_defers(defers, &mut outcome).await;
                        let drained = match outcome {
                            Ok(v) => v,
                            Err(e) => return Err(e),
                        };
                        if let Some(out) = self.return_from_frame(fiber, drained)? {
                            return Ok(out);
                        }
                    } else if let Some(outcome) = self.return_from_frame(fiber, result)? {
                        return Ok(outcome);
                    }
                }

                Op::Propagate => {
                    // The `?` operator. Mirrors the tree-walker's `ExprKind::Try`
                    // exactly: the operand must be a 2-element `[value, err]` Result
                    // pair (else a Tier-2 panic with the identical message, anchored
                    // at this op's span = the `TryExpr`'s code span). If `err == nil`
                    // the `value` is left on the stack (the `?` expression's result);
                    // otherwise it does a FUNCTION-LEVEL early return of `[nil, err]`
                    // — the SAME unwind-one-frame logic as `Op::Return` — so the
                    // enclosing function returns the propagated pair (and at the top
                    // level the program ends with that pair, treated as `Ok` by the
                    // driver, just like `Control::Propagate` in `run_file`).
                    // DEFER §3.3: on an early-return propagation, drain defers first
                    // (with the Propagate stash so §3.6 r2 fires if a defer panics).
                    let v = fiber.pop();
                    let (value, err) = match v.kind() {
                        ValueKind::Array(a) if a.borrow().len() == 2 => {
                            let b = a.borrow();
                            (b[0].clone(), b[1].clone())
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "the ? operator requires a Result pair [value, err]".to_string(),
                            ))
                        }
                    };
                    if err == Value::nil() {
                        fiber.push(value);
                    } else {
                        let pair = crate::interp::make_pair(Value::nil(), err);
                        let defers = std::mem::take(&mut fiber.frame_mut().defers);
                        if !defers.is_empty() {
                            // Stash as Propagate so §3.6 r2 fires if a defer panics.
                            let mut outcome: Result<Value, Control> =
                                Err(Control::Propagate(pair.clone()));
                            self.vm_run_defers(defers, &mut outcome).await;
                            let result_pair = match outcome {
                                Ok(v) => v,
                                Err(Control::Propagate(p)) => p,
                                Err(e) => return Err(e),
                            };
                            if let Some(out) = self.return_from_frame(fiber, result_pair)? {
                                return Ok(out);
                            }
                        } else if let Some(outcome) = self.return_from_frame(fiber, pair)? {
                            return Ok(outcome);
                        }
                    }
                }

                Op::Unwrap => {
                    // The `!` force-unwrap operator. Mirrors the tree-walker's
                    // `ExprKind::Unwrap` exactly: the operand must be a 2-element
                    // `[value, err]` Result pair (else a Tier-2 panic with the
                    // identical message, anchored at this op's span = the
                    // `UnwrapExpr`'s code span). If `err == nil` the `value` is
                    // left on the stack (the `!` expression's result); otherwise
                    // it raises a RECOVERABLE `Control::Panic` carrying the
                    // original error's message (`error_message`), so `recover`
                    // round-trips it into `[nil, err]` IDENTICALLY to the
                    // tree-walker's `AsError::at(error_message(&err), span)`.
                    let v = fiber.pop();
                    let (value, err) = match v.kind() {
                        ValueKind::Array(a) if a.borrow().len() == 2 => {
                            let b = a.borrow();
                            (b[0].clone(), b[1].clone())
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "the ! operator requires a Result pair [value, err]".to_string(),
                            ))
                        }
                    };
                    if err == Value::nil() {
                        fiber.push(value);
                    } else {
                        return Err(self.panic_at(fiber, fault_ip, error_message(&err)));
                    }
                }

                Op::Await => {
                    // `await expr`. Mirrors the tree-walker's `ExprKind::Await`
                    // EXACTLY: if the operand is a `Value::future`, drive it to
                    // completion (`f.get().await`) — a panic/propagation raised in
                    // the spawned task re-surfaces HERE (cross-task propagation),
                    // byte-identical to the tree-walker; otherwise `await` on a
                    // non-future is identity (`await 5 == 5`). Pop the operand into
                    // an owned local BEFORE the await so no `fiber` RefCell borrow is
                    // held across the suspension point (`await_holding_refcell_ref`
                    // stays clean).
                    let v = fiber.pop();
                    match v.into_kind() {
                        OwnedKind::Future(f) => {
                            let r = f.get().await?;
                            fiber.push(r);
                        }
                        other => fiber.push(rebuild_value(other)),
                    }
                }

                Op::Yield => {
                    // `yield expr`. The Fiber model makes this trivial: the yielded
                    // value is on TOS; pop it and return `RunOutcome::Yielded(v)`
                    // WITHOUT unwinding any frames — the frame stack stays live in
                    // the Fiber and `ip` is already past this op, so the next
                    // `resume` continues exactly here. The consumer's `next(v)`
                    // (driven via `GeneratorHandle::resume_vm`) pushes its `v` back
                    // onto the Fiber's stack, where the bytecode after `Op::Yield`
                    // expects the yield expression's value — that is the value-
                    // injection mechanism. `yield` with no operand pushed a `Nil`
                    // (the compiler emits NIL), so the popped value is `nil`.
                    let v = fiber.pop();
                    fiber.state = crate::vm::FiberState::Suspended;
                    return Ok(RunOutcome::Yielded(v));
                }

                Op::GetIter => {
                    // `for await` async-iterable validation: TOS must be a
                    // `Value::generator` (driven by `resume`) or a native stream
                    // handle (WebSocket `recv` / SSE `next`). ANYTHING ELSE is the
                    // Tier-2 panic `value of type {t} is not async-iterable`,
                    // byte-identical to the tree-walker's `exec_for_await` (the
                    // `other =>` and the Native-with-no-stream-method arms both
                    // produce this message). We PEEK (leave the value in place): the
                    // compiler immediately stores it into a scratch slot to drive
                    // lazily across iterations.
                    let ok = match fiber.peek(0).kind() {
                        ValueKind::Generator(_) => true,
                        ValueKind::Native(n) => crate::interp::native_stream_method(n.kind).is_some(),
                        _ => false,
                    };
                    if !ok {
                        let t = crate::interp::type_name(fiber.peek(0));
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            format!("value of type {t} is not async-iterable"),
                        ));
                    }
                }

                Op::IterNext => {
                    // Drive one lazy `for await` step over the async-iterable on TOS.
                    // Pop it into an owned local BEFORE any `.await` so no `fiber`
                    // RefCell borrow is held across the suspension point
                    // (`await_holding_refcell_ref` stays clean), then push back the
                    // produced `value` and a `done` boolean. Byte-identical to
                    // `exec_for_await` (`src/interp.rs`).
                    // The op's span (the iterable expression's code span), captured
                    // before any borrow/await so a native-stream call has a site.
                    let op_span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let iterable = fiber.pop();
                    match iterable.into_kind() {
                        OwnedKind::Generator(g) => {
                            // `resume(nil)` drives the backing Fiber to its next
                            // `Op::Yield` (awaiting any inner futures along the way —
                            // this is how an async generator's await+yield fuse).
                            // `Some(v)` -> a value; `None` -> done.
                            match g.resume(Value::nil()).await? {
                                Some(v) => {
                                    fiber.push(v);
                                    fiber.push(Value::bool_(false));
                                }
                                None => {
                                    fiber.push(Value::nil());
                                    fiber.push(Value::bool_(true));
                                }
                            }
                        }
                        OwnedKind::Native(n) => {
                            // A native stream: call its `recv`/`next` method for a
                            // `[value, err]` pair (a non-nil `err` is a Tier-2 panic,
                            // a nil `value` ends the stream), mirroring
                            // `exec_for_await`'s `Value::native` arm exactly.
                            // `GetIter` already validated the handle, so a missing
                            // stream method here is a wiring bug — surface it as a
                            // defensive Tier-2 panic rather than an `unwrap`.
                            let method = match crate::interp::native_stream_method(n.kind) {
                                Some(m) => m,
                                None => {
                                    return Err(self.panic_at(
                                        fiber,
                                        fault_ip,
                                        format!(
                                            "value of type {} is not async-iterable",
                                            crate::interp::type_name(&Value::native(n))
                                        ),
                                    ))
                                }
                            };
                            let bound = Value::native_method(Rc::new(crate::value::NativeMethod {
                                receiver: n,
                                method: method.to_string(),
                            }));
                            // Box this edge: `call_value` may re-enter `run`, so
                            // the recursive future needs a finite size.
                            let pair =
                                Box::pin(self.call_value(bound, Vec::new(), op_span)).await?;
                            let (value, err) = match pair.kind() {
                                ValueKind::Array(a) if a.borrow().len() == 2 => {
                                    let b = a.borrow();
                                    (b[0].clone(), b[1].clone())
                                }
                                // Defensive: a non-pair return ends iteration.
                                _ => {
                                    fiber.push(Value::nil());
                                    fiber.push(Value::bool_(true));
                                    continue;
                                }
                            };
                            if err != Value::nil() {
                                let msg = crate::interp::error_message(&err);
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("for await stream error: {msg}"),
                                ));
                            }
                            if value == Value::nil() {
                                fiber.push(Value::nil());
                                fiber.push(Value::bool_(true));
                            } else {
                                fiber.push(value);
                                fiber.push(Value::bool_(false));
                            }
                        }
                        other => {
                            // GetIter validated the iterable, so this is unreachable
                            // in practice; surface defensively rather than panic the
                            // host.
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "value of type {} is not async-iterable",
                                    crate::interp::type_name(&rebuild_value(other))
                                ),
                            ));
                        }
                    }
                }

                Op::IterClose => {
                    // Close the async-iterable on TOS on a `break`/early-`return` out
                    // of a `for await` over a generator — `g.close()` drops the
                    // backing Fiber so it is reclaimed promptly, byte-identical to
                    // the tree-walker. A native stream is reclaimed at scope end, so
                    // closing it is a no-op here.
                    let iterable = fiber.pop();
                    if let OwnedKind::Generator(g) = iterable.into_kind() {
                        g.close();
                    }
                }

                Op::SetProp => {
                    // `obj value -- value` — store `obj.<name> = value`, applying a
                    // declared field-type contract on an Instance field. The SAME
                    // `set_member` the tree-walker's `assign_to` Member arm uses, so
                    // the field contract panic (message + span) is byte-identical.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("SET_PROP operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let value = fiber.pop();
                    let obj = fiber.pop();
                    // The op's span is the VALUE's span (see the compiler), matching
                    // the tree-walker's `value_span` for the contract panic; reuse it
                    // for the "cannot set property" error too (single VM span).
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    // Resolve `proto` out of the fiber so the chunk IC borrow does not
                    // collide with the later `fiber.push`.
                    let proto = fiber.frame().closure.proto.clone();
                    let v = self.vm_set_prop(&proto.chunk, fault_ip, &obj, &name, value, span)?;
                    fiber.push(v);
                }

                Op::Class => {
                    // Build a class value (V9). The compiler emitted, just below this
                    // op, one closure per defaulted field (declaration order) then
                    // one closure per method (declaration order); the class proto
                    // carries the prebuilt `Rc<Class>` and the parallel name lists.
                    // Register the default thunks and method closures in the VM side
                    // tables keyed by the class's `Rc` identity, then push the class.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let cp = fiber.frame().closure.proto.chunk.class_protos[idx].clone();
                    let n_methods = cp.method_names.len();
                    let n_statics = cp.static_method_names.len();
                    let n_defaults = cp.default_fields.len();
                    // Pop in reverse push order: static closures (top), then instance
                    // method closures, then default thunks (SP1 §3 stack layout
                    // `[super?, ..thunks.., ..methods.., ..statics..]`).
                    let mut statics = vec![Value::nil(); n_statics];
                    for slot in statics.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    let mut methods = vec![Value::nil(); n_methods];
                    for slot in methods.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    let mut defaults = vec![Value::nil(); n_defaults];
                    for slot in defaults.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    // For an `extends` clause, the superclass class-value was pushed
                    // FIRST (it is the bottom of the group), so it pops LAST. Build a
                    // FRESH `Rc<Class>` with `superclass` set (the prebuilt template
                    // had `superclass: None`); the method/default tables are then
                    // registered under the NEW class's identity key. Mirrors the
                    // tree-walker's `Stmt::Class`, which sets `superclass` to the
                    // resolved parent `Value::class`.
                    // The shared `def_env` for VM classes (task #157): the SHARED
                    // `validate_into` (`.from`/typed-parse) resolves nested-class
                    // field-type names and default-expr names through it, so EVERY
                    // class gets it (not just the `extends` case). This means we
                    // always build a FRESH `Rc<Class>` (the compiler's template had
                    // the inert `global_env()` placeholder + no superclass).
                    let def_env = self.class_env();
                    let superclass = if cp.has_super {
                        let sup = fiber.pop();
                        match sup.into_kind() {
                            OwnedKind::Class(c) => Some(c),
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("'{}' is not a class", rebuild_value(other)),
                                ))
                            }
                        }
                    } else {
                        None
                    };
                    let class: Rc<crate::value::Class> = Rc::new(crate::value::Class {
                        name: cp.class.name.clone(),
                        superclass,
                        fields: cp.class.fields.clone(),
                        methods: cp.class.methods.clone(),
                        // VM static methods live in a separate per-class proto
                        // table (keyed by Rc::as_ptr); this runtime `Class` value
                        // carries an empty namespace (populated for the tree-walker
                        // path only). The VM resolves `C.name` statics via that
                        // table (SP1 §3, C5).
                        static_methods: indexmap::IndexMap::new(),
                        def_env: def_env.clone(),
                        is_worker: cp.class.is_worker,
                    });
                    // Register the class into the shared env so a sibling/forward
                    // nested-class field type (or a default-expr name) resolves at
                    // `.from` time — late-bound exactly like the tree-walker's module
                    // env. A redefinition (same name re-run) overwrites the binding.
                    if def_env
                        .define(&class.name, Value::class(class.clone()), false)
                        .is_err()
                    {
                        let _ = def_env.assign(&class.name, Value::class(class.clone()));
                    }
                    let key = Rc::as_ptr(&class) as usize;
                    let mut method_map: FxHashMap<String, Cc<Closure>> = FxHashMap::default();
                    for (name, mv) in cp.method_names.iter().zip(methods) {
                        match mv.into_kind() {
                            OwnedKind::Closure(c) => {
                                method_map.insert(name.clone(), c);
                            }
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "class method '{name}' is not a closure: {:?}",
                                        rebuild_value(other)
                                    ),
                                ))
                            }
                        }
                    }
                    let mut default_map: FxHashMap<String, Cc<Closure>> = FxHashMap::default();
                    for (i, (name, dv)) in cp.default_fields.iter().zip(defaults).enumerate() {
                        match dv.into_kind() {
                            OwnedKind::Closure(c) => {
                                // Mirror the enclosing-scope names this default
                                // captures into `def_env` (read from the thunk's
                                // captured upvalue cells), so the SHARED
                                // `validate_into` (`.from`/typed-parse) resolves the
                                // same binding the construct-time thunk closes over.
                                // The construct path still runs the thunk unchanged.
                                // Mirror as MUTABLE so a default that ASSIGNS to a
                                // captured name (`x: number = (g = 5)`) evaluates on
                                // the `.from` path exactly as the tree-walker does
                                // against its real `def_env` chain (where the captured
                                // `let` keeps its declared mutability). For the common
                                // read-only default the mutability flag is irrelevant.
                                if let Some(caps) = cp.default_captures.get(i) {
                                    for (cap_name, up_idx) in caps {
                                        if let Some(cell) = c.upvalues.get(*up_idx as usize) {
                                            let val = cell.borrow().clone();
                                            if def_env.define(cap_name, val.clone(), true).is_err()
                                            {
                                                let _ = def_env.assign(cap_name, val);
                                            }
                                        }
                                    }
                                }
                                default_map.insert(name.clone(), c);
                            }
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "field default '{name}' thunk is not a closure: {:?}",
                                        rebuild_value(other)
                                    ),
                                ))
                            }
                        }
                    }
                    let mut static_map: FxHashMap<String, Cc<Closure>> = FxHashMap::default();
                    for (name, sv) in cp.static_method_names.iter().zip(statics) {
                        match sv.into_kind() {
                            OwnedKind::Closure(c) => {
                                static_map.insert(name.clone(), c);
                            }
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "static method '{name}' is not a closure: {:?}",
                                        rebuild_value(other)
                                    ),
                                ))
                            }
                        }
                    }
                    self.class_methods.borrow_mut().insert(key, method_map);
                    self.class_static_methods.borrow_mut().insert(key, static_map);
                    self.class_defaults.borrow_mut().insert(key, default_map);
                    // Invalidate any verdict cached against a now-reusable class pointer
                    // (the Interp cache is shared by both engines). See its field doc.
                    self.interp.bump_iface_cache_gen();
                    fiber.push(Value::class(class));
                }

                Op::DefineInterface => {
                    // IFACE: build the `InterfaceDef` from the proto and self-bind it.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let ip = fiber.frame().closure.proto.chunk.interface_protos[idx].clone();
                    // `def_env` is the VM's shared class/module env (mirroring the
                    // tree-walker, where every top-level decl's def_env is the module
                    // env), so lazy `flatten` resolves sibling/forward-ref `extends`.
                    let def_env = self.class_env();
                    let mut own_methods: indexmap::IndexMap<String, crate::value::MethodReq> =
                        indexmap::IndexMap::new();
                    for (mname, arity, has_rest) in &ip.methods {
                        own_methods.insert(
                            mname.clone(),
                            crate::value::MethodReq {
                                arity: *arity,
                                has_rest: *has_rest,
                            },
                        );
                    }
                    let iface = Value::interface(Rc::new(crate::value::InterfaceDef {
                        name: ip.name.clone(),
                        own_methods,
                        extends: ip.extends.clone(),
                        def_env: def_env.clone(),
                        flat: std::cell::RefCell::new(None),
                    }));
                    // Register into the shared class/module env so a SIBLING interface's
                    // `extends` resolves it (late-bound). define-or-assign like classes.
                    if def_env.define(&ip.name, iface.clone(), false).is_err() {
                        let _ = def_env.assign(&ip.name, iface.clone());
                    }
                    // Push the descriptor; the compiler emitted the matching bind op
                    // (DEFINE_GLOBAL for a top-level interface, SET_LOCAL for a nested
                    // one) immediately after — exactly like Op::Class.
                    // Invalidate any verdict cached against a now-reusable interface pointer.
                    self.interp.bump_iface_cache_gen();
                    fiber.push(iface);
                }

                Op::GetSuper => {
                    // `super.<name>` (V9-T2): resolve `name` starting at the CURRENT
                    // method's DEFINING class's superclass, bound to `self` (slot 0).
                    // Mirrors the tree-walker: `super` is a `Value::super_` whose
                    // `start` is `defining_class.superclass`, and `read_member` on it
                    // finds the method up that chain and produces a BoundMethod on
                    // `self` (which the subsequent CALL invokes). The `defining_class`
                    // we stamp onto the BoundMethod is the ANCESTOR that actually
                    // declared the method, so a NESTED `super` resolves from the right
                    // link too.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match fiber.frame().closure.proto.chunk.consts[idx].kind() {
                        ValueKind::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("GET_SUPER name is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    // The defining class of the running method (set by
                    // `invoke_compiled_method`). Absent only if `super` somehow
                    // appears outside a method frame — a compiler invariant violation.
                    let def_class = match &fiber.frame().def_class {
                        Some(c) => c.clone(),
                        None => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "'super' used outside of a method".to_string(),
                            ))
                        }
                    };
                    // self = slot 0, read cell-aware (it is a cell slot whenever a
                    // nested closure captured it). CALL §2 A1: cells may be empty
                    // when slot 0 is not captured; use .first so the empty-vec fast
                    // path is safe.
                    let receiver = match fiber.frame().cells.first().and_then(|c| c.as_ref()) {
                        Some(cell) => cell.borrow().clone(),
                        None => fiber.local(0).clone(),
                    };
                    // Resolve up from the DEFINING class's superclass (NOT the
                    // instance's class), matching `SuperRef { start: superclass }`.
                    let start = def_class.superclass.clone();
                    let bound = match start
                        .as_ref()
                        .and_then(|s| self.find_compiled_method(s, &name))
                    {
                        Some((_closure, found_class)) => {
                            Value::bound_method(Rc::new(crate::value::BoundMethod {
                                receiver,
                                method: Rc::new(crate::value::Method {
                                    params: Vec::new(),
                                    ret: None,
                                    body: Vec::new(),
                                    is_async: false,
                                    is_generator: false,
                                    is_worker: false,
                                }),
                                defining_class: found_class,
                                name: name.to_string(),
                            }))
                        }
                        None => {
                            // Mirror the tree-walker's `Value::super_` member-read
                            // error wording (with/without a superclass).
                            let msg = if start.is_some() {
                                format!("no superclass method '{name}'")
                            } else {
                                format!("no superclass method '{name}' (no superclass)")
                            };
                            return Err(Control::Panic(AsError::at(msg, span)));
                        }
                    };
                    fiber.push(bound);
                }

                Op::Break => {
                    // DBG: the software-breakpoint trap. Reached ONLY because the
                    // debugger PATCHED this opcode byte (the side table holds the
                    // displaced original op). This is the ONLY new hot-loop arm; the
                    // `None`/no-breakpoint path never produces an `Op::Break` byte, so
                    // every production run is byte-identical to pre-DBG (Gate 12).
                    //
                    // Re-execution technique (the simplest one — no dispatch
                    // restructuring): recover the original byte, park on the command
                    // channel, then UN-PATCH the byte (write the original back) and
                    // reset `ip` to the break offset. The next iteration of THIS very
                    // loop re-reads `code[ip]` — now the restored original opcode —
                    // and executes it normally. The displaced instruction therefore
                    // runs exactly once and the program result is identical to an
                    // un-instrumented run.
                    //
                    // v1 trade-off (DOCUMENTED): a hit breakpoint UN-PATCHES itself,
                    // so it traps at most once per set. Persistent re-arming (re-patch
                    // after stepping off) is a Task-6 follow-up. Tests that need a
                    // breakpoint to fire on every loop iteration re-patch from the
                    // controller after each stop.
                    let proto_id =
                        Rc::as_ptr(&fiber.frame().closure.proto) as *const () as usize;
                    let depth = fiber.frames.len();

                    // DX D2 Task 6 — COVERAGE TRAP (checked FIRST). If this patched byte is
                    // a coverage trap, mark the line covered, restore the original op, point
                    // `ip` back at it, and `continue` so the next loop iteration executes the
                    // real op. No debugger stop, no pause — each line traps at most once then
                    // runs free (zero steady-state cost; program output unchanged). Only when
                    // it is NOT a coverage trap do we fall through to the debugger logic
                    // below, byte-identical to pre-coverage. (Coverage's side table is
                    // consulted ONLY here, inside the COLD trap arm — never on the hot path.)
                    let cov_hit = {
                        let mut inst = self.instrument.borrow_mut();
                        match inst.as_mut().and_then(|i| i.coverage.as_mut()) {
                            Some(table) => table.trap(proto_id, fault_ip).map(|(orig, line)| {
                                table.mark_covered(proto_id, line);
                                orig
                            }),
                            None => None,
                        }
                    };
                    if let Some(original_byte) = cov_hit {
                        let chunk = &fiber.frame().closure.proto.chunk;
                        chunk.patch_byte(fault_ip, original_byte);
                        fiber.frame_mut().ip = fault_ip;
                        // Re-dispatch the recovered op next iteration. No debugger stop.
                        continue;
                    }

                    // Recover the original opcode byte from the hook's side table
                    // (scoped borrow, dropped immediately — Gate 4).
                    let original = {
                        let inst = self.instrument.borrow();
                        inst.as_ref()
                            .and_then(|i| i.breakpoints.as_ref())
                            .and_then(|h| h.original_byte(proto_id, fault_ip))
                    };
                    let Some(original_byte) = original else {
                        // A stray Break with no side-table entry (no debugger, or a
                        // cleared breakpoint left a Break byte): nothing to recover.
                        // `ip` already advanced past the (0-width) Break, so just
                        // continue — treat it as a no-op safepoint.
                        continue;
                    };

                    // Park: ship a `Stopped` event (carrying a fresh frame snapshot) and
                    // block on the command channel. No Value/Cc/RefCell borrow is held
                    // across the blocking recv (Gate 4) — `debug_stop` scopes every borrow.
                    //
                    // The command loop runs while parked. An `Evaluate` command is NOT a
                    // resume: `debug_stop` returns it (instrument box already restored, no
                    // borrow held), we evaluate the expression in the paused frame on the
                    // tree-walker, ship the `EvaluateResult`, then LOOP back to park again
                    // and wait for the next command. A resume command breaks the loop.
                    loop {
                        // Build the Send-safe frame/variable snapshot while `&fiber` is
                        // live (Value access stays on the VM thread) and BEFORE the blocking
                        // recv — owned Strings only, no borrow held across the wait (Gate 4).
                        // Innermost frame first; the innermost frame's active instruction is
                        // the trapped `fault_ip`. Re-built per Evaluate iteration so an
                        // inspecting expression that mutated a local is reflected on the next
                        // park (cheap; only reached while interactively paused — Gate 12 hot
                        // loop untouched).
                        let snap = self.build_frame_snapshots(fiber, fault_ip);
                        match self.debug_stop(proto_id, fault_ip, depth, snap) {
                            StopOutcome::Resume => break,
                            StopOutcome::Evaluate { expr, frame_id } => {
                                let (ok, display) =
                                    self.eval_in_paused_frame(fiber, frame_id, &expr).await;
                                // Ship the result back (scoped instrument borrow, dropped
                                // before the next loop iteration — Gate 4). A disconnected
                                // controller is harmless (the next park's recv catches it).
                                let inst = self.instrument.borrow();
                                if let Some(hook) =
                                    inst.as_ref().and_then(|i| i.breakpoints.as_ref())
                                {
                                    let _ = hook.events.send(
                                        crate::vm::instrument::DebugEvent::EvaluateResult {
                                            ok,
                                            display,
                                        },
                                    );
                                }
                            }
                        }
                    }

                    // Un-patch this breakpoint and re-point `ip` to the break offset so
                    // the next loop iteration reads + executes the recovered original
                    // op normally. `code` lives behind a `RefCell` on the chunk; write
                    // the restored byte through it (no offset moves). If the byte was
                    // already restored (e.g. the controller cleared it during the
                    // stop), the write is harmless.
                    {
                        let chunk = &fiber.frame().closure.proto.chunk;
                        chunk.patch_byte(fault_ip, original_byte);
                    }
                    fiber.frame_mut().ip = fault_ip;
                    // Fall through to the top of the loop: it re-reads code[fault_ip]
                    // (now the original op) and dispatches it. No restructuring.
                }

                other => {
                    return Err(self.panic_at(
                        fiber,
                        fault_ip,
                        format!("opcode {other:?} not yet implemented"),
                    ))
                }
            }
        }
    }

    /// DBG: park the fiber at a breakpoint. Ships a `Stopped` event to the DAP server
    /// thread and blocks on the command channel until a resume command (recording the
    /// resume's step mode on the hook) OR an `Evaluate` command (returned to the caller,
    /// which evaluates it and re-parks). Breakpoint-management commands (SetBreakpoints /
    /// ClearBreakpoints) are applied IN PLACE and the loop waits for the next command.
    ///
    /// Gate 4: builds the plain-data snapshot and ships it, then drops every borrow
    /// BEFORE blocking on the channel `recv` — no `RefCell`/`Cc` borrow is held across
    /// the wait. The `Instrumentation` box is TAKEN out of the cell across the blocking
    /// recv (a re-entrant native call the resumed program triggers could otherwise
    /// re-borrow the cell), then put back BEFORE returning (so the caller's `Evaluate`
    /// handling re-borrows a fully-restored cell — no borrow held across the return).
    fn debug_stop(
        &self,
        proto_id: usize,
        offset: usize,
        depth: usize,
        frames: Vec<crate::vm::instrument::FrameSnapshot>,
    ) -> StopOutcome {
        use crate::vm::instrument::{DebugCommand, DebugEvent, StepMode};
        // Ship the Stopped event (scoped borrow, dropped immediately). `frames` is
        // already plain owned Send-safe data (built at the trap before this call).
        {
            let inst = self.instrument.borrow();
            let Some(hook) = inst.as_ref().and_then(|i| i.breakpoints.as_ref()) else {
                return StopOutcome::Resume;
            };
            // If the controller end has hung up, there is nothing to wait for —
            // resume immediately (a detached debugger must not deadlock the program).
            if hook
                .events
                .send(DebugEvent::Stopped {
                    proto_id,
                    offset,
                    depth,
                    frames,
                })
                .is_err()
            {
                return StopOutcome::Resume;
            }
        }
        // Take the payload OUT across the blocking recv (Gate 4 — hold no cell borrow
        // while blocked). Nothing from the script heap (Value/Cc/fiber) is touched.
        let mut taken = self.instrument.borrow_mut().take();

        // The command loop: breakpoint-management commands (SetBreakpoints /
        // ClearBreakpoints) are applied IN PLACE and the loop waits for the NEXT command
        // (they are not a resume); a resume command (Continue/Next/StepIn/StepOut) records
        // the step mode and RETURNS `Resume`; an `Evaluate` command RETURNS `Evaluate{..}`
        // for the caller to handle and re-park. Each iteration blocks on `recv` holding NO
        // RefCell/Cc borrow (Gate 4): we resolve/patch synchronously between recvs.
        let outcome = loop {
            // Block on the channel with no borrow held. `recv` returns `Err` if the
            // controller hung up → resume free-running.
            let cmd = match taken
                .as_mut()
                .and_then(|i| i.breakpoints.as_mut())
                .map(|hook| hook.commands.recv())
            {
                Some(Ok(cmd)) => cmd,
                // No hook, or the controller disconnected — resume free-running.
                _ => break StopOutcome::Resume,
            };

            match cmd {
                DebugCommand::Continue => {
                    self.set_step(taken.as_mut(), StepMode::Run, depth);
                    break StopOutcome::Resume;
                }
                DebugCommand::Next => {
                    self.set_step(taken.as_mut(), StepMode::Over, depth);
                    break StopOutcome::Resume;
                }
                DebugCommand::StepIn => {
                    self.set_step(taken.as_mut(), StepMode::Into, depth);
                    break StopOutcome::Resume;
                }
                DebugCommand::StepOut => {
                    self.set_step(taken.as_mut(), StepMode::Out, depth);
                    break StopOutcome::Resume;
                }
                DebugCommand::Evaluate { expr, frame_id } => {
                    // Not a resume: hand the request back to the caller (which re-enters
                    // the async evaluator). Restore the box FIRST so no borrow is held
                    // across the return and the evaluator sees a fully-armed debugger.
                    break StopOutcome::Evaluate { expr, frame_id };
                }
                DebugCommand::SetBreakpoints { source, lines } => {
                    let results = self.apply_set_breakpoints(taken.as_mut(), &source, &lines);
                    // Ship the verdict; ignore a disconnected controller (it will be
                    // caught on the next recv). No borrow held across the send.
                    if let Some(hook) = taken.as_ref().and_then(|i| i.breakpoints.as_ref()) {
                        let _ = hook
                            .events
                            .send(DebugEvent::BreakpointsVerified { results });
                    }
                    // Not a resume — wait for the next command.
                }
                DebugCommand::ClearBreakpoints => {
                    self.apply_clear_breakpoints(taken.as_mut());
                    // Not a resume — wait for the next command.
                }
            }
        };

        // Restore the payload (the program continues with the debugger still armed).
        *self.instrument.borrow_mut() = taken;
        outcome
    }

    /// DBG: record the resume step mode + depth on the hook (in the already-TAKEN
    /// instrumentation box — no re-borrow of `self.instrument`). A no-op if the hook is
    /// gone (controller disconnected mid-stop).
    fn set_step(
        &self,
        inst: Option<&mut Box<crate::vm::instrument::Instrumentation>>,
        mode: crate::vm::instrument::StepMode,
        depth: usize,
    ) {
        if let Some(hook) = inst.and_then(|i| i.breakpoints.as_mut()) {
            hook.step_mode = mode;
            hook.step_depth = depth;
        }
    }

    /// DBG: evaluate `expr_text` in the PAUSED frame `frame_id` (the DAP `evaluate`
    /// request — Watch / Debug Console / hover). The clean approach: REUSE the tree-walker
    /// as the evaluator — the parked `Vm` already holds a full [`Interp`] (`self.interp()`)
    /// with an async re-entrant `eval_expr` and an `Environment`, which `debug_stop`'s
    /// blocking-`recv` command loop (a SYNC fn) cannot do. We bridge the paused frame's
    /// live values into an `Environment` and run the parsed expression on the tree-walker.
    ///
    /// Returns plain owned `(ok, display)`: on success the value rendered via `Value`'s
    /// `Display`; on a parse error / thrown panic / `?`-propagation, `ok=false` and the
    /// error text. The caller ships this as a [`DebugEvent::EvaluateResult`] — NO
    /// `Value`/`Rc`/`Cc` crosses the channel (the worker-airlock discipline).
    ///
    /// Side-effects in the expression DO run (like the V8 debug console) — the debugger
    /// only ever evaluates an expression the user explicitly requested, so the
    /// non-evaluated run's observation contract is untouched (Gate 1: `evaluate` adds no
    /// behavior to a normal run; it is reached ONLY from the interactive command loop).
    /// Note the asymmetry: a HEAP mutation (a field/element write through a shared
    /// `Rc`/`Cc` value) PERSISTS into the resumed program exactly as a normal write would,
    /// but a rebind of a paused-frame LOCAL (`x = 99`) mutates only the throwaway eval
    /// `Environment` and is LOST — the VM frame's stack slot is never written back. This
    /// is what keeps the resumed frame uncorruptible by an evaluate.
    ///
    /// Gate 4: no `RefCell`/`Cc` borrow is held across the `.await` — the environment is
    /// built (cloning live values out) BEFORE `eval_expr`, and the `fiber` reads are
    /// synchronous.
    ///
    /// This is the SHARED evaluator that conditional breakpoints / logpoints will reuse at
    /// breakpoint-check time (a documented follow-up — DBG Task 8 scope is read/expression
    /// evaluation for Watch/Repl/Hover).
    async fn eval_in_paused_frame(
        &self,
        fiber: &Fiber,
        frame_id: usize,
        expr_text: &str,
    ) -> (bool, String) {
        // RT §2.3(c/d): paused-frame expression evaluation re-parses on the legacy
        // front-end (lexer/parser/ast), which the runtime-only build compiles OUT. The
        // DAP feature is never in a tier, so this is unreachable on a stub; the refusal
        // keeps the core VM debug seam compiling. Non-rt below is byte-identical.
        #[cfg(ascript_rt)]
        {
            let _ = (fiber, frame_id, expr_text);
            (false, "<evaluation unavailable: this runtime has no parser>".to_string())
        }
        #[cfg(not(ascript_rt))]
        {
        // (1) Map the DAP frame id (innermost-first, as `build_frame_snapshots` /
        // `stackTrace` order it) back to the bottom-first `fiber.frames` index.
        let n = fiber.frames.len();
        if frame_id >= n {
            return (false, format!("<no such frame: {frame_id}>"));
        }
        let frame = &fiber.frames[n - 1 - frame_id];
        let proto = &frame.closure.proto;

        // (2) Build the evaluation environment: a fresh builtins child, then the module
        // user-globals (so a top-level binding / the function itself resolves), then the
        // frame's live locals LAST so they shadow globals. Every value is cloned OUT here
        // (no borrow held across the later `.await`).
        let env = crate::interp::global_env().child();
        for (name, gslot) in self.user_globals.borrow().iter() {
            let _ = env.define(name, gslot.value.clone(), gslot.mutable);
        }
        for (slot, name) in &proto.local_names {
            let slot_idx = *slot as usize;
            // Read the live value: a captured slot through its cell, else the plain stack
            // slot at `slot_base + slot` (bounds-checked).
            let val = match frame.cells.get(slot_idx).and_then(|c| c.as_ref()) {
                Some(cell) => cell.borrow().clone(),
                None => match fiber.stack.get(frame.slot_base + slot_idx) {
                    Some(v) => v.clone(),
                    None => continue,
                },
            };
            // Define-or-reassign (a later same-named slot wins; locals shadow globals).
            if env.define(name, val.clone(), true).is_err() {
                let _ = env.assign(name, val);
            }
        }

        // (3) Parse the expression with the legacy front-end (the tree-walker's parser).
        // There is no single-expression entry, so parse it as a one-statement program and
        // extract the `Stmt::Expr`. A parse error → a clean `(false, …)` (never a panic).
        let expr = match crate::lexer::lex(expr_text)
            .map_err(|e| e.message)
            .and_then(|toks| crate::parser::parse(&toks).map_err(|e| e.message))
        {
            Ok(stmts) => match stmts.into_iter().next() {
                Some(crate::ast::Stmt::Expr(e)) => e,
                _ => return (false, "<parse error: expected an expression>".to_string()),
            },
            Err(msg) => return (false, format!("<parse error: {msg}>")),
        };

        // (4) Evaluate on the tree-walker. Render the value via `Display`; a thrown panic
        // / propagation becomes a `(false, message)` result.
        match self.interp().eval_expr(&expr, &env).await {
            Ok(v) => (true, format!("{v}")),
            Err(Control::Panic(e)) => (false, e.message),
            Err(_) => (false, "<propagated>".to_string()),
        }
        }
    }

    /// DBG: apply a `SetBreakpoints { source, lines }` against the live proto tree while
    /// parked. DAP setBreakpoints is declarative/replace-all PER SOURCE: first clear this
    /// source's existing breakpoints (restoring their bytes), then for each requested line
    /// resolve to a `(proto_id, offset)` and patch `Op::Break` through the shared `&Chunk`.
    /// Returns one [`BreakpointBinding`] per requested line (verdict + bound offset).
    ///
    /// Gate 4: fully synchronous — no `.await`, no `recv`. It briefly borrows
    /// `debug_protos` to resolve/look up chunks, dropping the borrow before each
    /// `patch_byte` is irrelevant (no await between), and operates on the hook in the
    /// already-TAKEN instrumentation box (`inst`), never re-borrowing `self.instrument`.
    fn apply_set_breakpoints(
        &self,
        inst: Option<&mut Box<crate::vm::instrument::Instrumentation>>,
        source: &str,
        lines: &[u32],
    ) -> Vec<crate::vm::instrument::BreakpointBinding> {
        use crate::vm::instrument::BreakpointBinding;
        let Some(hook) = inst.and_then(|i| i.breakpoints.as_mut()) else {
            return Vec::new();
        };

        // (1) Clear this source's existing breakpoints (replace-all per source). Determine
        // which registered protos belong to `source`, then restore + forget any live
        // breakpoint on them.
        let source_proto_ids: std::collections::HashSet<usize> = {
            let protos = self.debug_protos.borrow();
            let mut paths: std::collections::HashSet<String> = std::collections::HashSet::new();
            for p in protos.iter() {
                if let Some(src) = p.chunk.source.borrow().as_ref() {
                    paths.insert(src.path.clone());
                }
            }
            let single_source = paths.len() == 1;
            let want_file = file_name_of(source);
            protos
                .iter()
                .filter(|p| match p.chunk.source.borrow().as_ref() {
                    None => false,
                    Some(src) => single_source || file_name_of(&src.path) == want_file,
                })
                .map(|p| Rc::as_ptr(p) as *const () as usize)
                .collect()
        };
        for (pid, off) in hook.breakpoints_in(&source_proto_ids) {
            if let Some(original) = hook.forget_breakpoint(pid, off) {
                if let Some(proto) = self.debug_proto_for(pid) {
                    proto.chunk.patch_byte(off, original);
                }
            }
        }

        // (2) Resolve + set each requested line. Resolution borrows `debug_protos`
        // internally and drops the borrow before we patch (all synchronous, no await).
        let mut results = Vec::with_capacity(lines.len());
        for &line in lines {
            match self.resolve_line_breakpoint(source, line) {
                Some((proto_id, off)) => {
                    if let Some(proto) = self.debug_proto_for(proto_id) {
                        hook.set_breakpoint_shared(proto_id, off, &proto.chunk);
                        results.push(BreakpointBinding {
                            line,
                            verified: true,
                            offset: Some(off as u32),
                        });
                    } else {
                        // Resolved to a proto not in the tree (should not happen) — unbound.
                        results.push(BreakpointBinding {
                            line,
                            verified: false,
                            offset: None,
                        });
                    }
                }
                None => results.push(BreakpointBinding {
                    line,
                    verified: false,
                    offset: None,
                }),
            }
        }
        results
    }

    /// DBG: apply a `ClearBreakpoints` while parked — restore EVERY patched byte (drain
    /// the side table; for each `(proto_id, offset) → original`, find the chunk in
    /// `debug_protos` and write the original byte back). Synchronous, no await/recv.
    fn apply_clear_breakpoints(
        &self,
        inst: Option<&mut Box<crate::vm::instrument::Instrumentation>>,
    ) {
        let Some(hook) = inst.and_then(|i| i.breakpoints.as_mut()) else {
            return;
        };
        for ((pid, off), original) in hook.drain_breakpoints() {
            if let Some(proto) = self.debug_proto_for(pid) {
                proto.chunk.patch_byte(off, original);
            }
        }
    }

    /// DBG: build the Send-safe per-frame snapshot at a debugger stop. Walks the fiber's
    /// call stack INNERMOST-first (`frames.iter().rev()`), rendering each frame's
    /// location and locals to PLAIN OWNED `String`/`u32` — no `Value`/`Rc`/`Cc` escapes
    /// (the worker-airlock discipline). Called ONLY from the `Op::Break` trap (reached
    /// solely via a patched breakpoint byte), so the hot dispatch loop is untouched
    /// (Gate 12). Fully synchronous — no `.await`; every Value access stays on the VM
    /// thread and no `RefCell`/`Cc` borrow outlives its `format!`.
    fn build_frame_snapshots(
        &self,
        fiber: &Fiber,
        fault_ip: usize,
    ) -> Vec<crate::vm::instrument::FrameSnapshot> {
        use crate::vm::instrument::FrameSnapshot;
        let n = fiber.frames.len();
        let mut out = Vec::with_capacity(n);
        // Innermost frame first; `idx` is the original (bottom = 0) frame index so we
        // can detect the bottom/script frame and pick the right active instruction.
        for (rev_i, frame) in fiber.frames.iter().rev().enumerate() {
            let idx = n - 1 - rev_i;
            let proto = &frame.closure.proto;
            // Active instruction offset: the innermost (first visited) frame uses the
            // trapped `fault_ip`; caller frames use their saved return address
            // `frame.ip` — close enough to the call site for v1 (the displacement is at
            // most one instruction past the call).
            let offset = if rev_i == 0 { fault_ip } else { frame.ip };
            let (line, column) = proto.chunk.line_col_at(offset).unwrap_or((0, 0));

            // Frame label: declared name; else "<script>" for the bottom/module frame;
            // else "fn@L<line>" (1-based line) for an anonymous proto.
            let function = match &proto.debug_name {
                Some(name) => name.to_string(),
                None if idx == 0 => "<script>".to_string(),
                None => format!("fn@L{}", line + 1),
            };

            // Locals: render each named slot to an owned String. Read a cell slot's live
            // value through its `Cc<RefCell<Value>>`; else the plain stack slot at
            // `slot_base + slot`. Defensive: skip an out-of-range stack index. The
            // `format!` produces the owned String; no borrow outlives it (airlock).
            let mut locals = Vec::with_capacity(proto.local_names.len());
            for (slot, name) in &proto.local_names {
                let slot_idx = *slot as usize;
                let rendered = match frame.cells.get(slot_idx).and_then(|c| c.as_ref()) {
                    Some(cell) => format!("{}", cell.borrow()),
                    None => match fiber.stack.get(frame.slot_base + slot_idx) {
                        Some(v) => format!("{v}"),
                        None => continue,
                    },
                };
                locals.push((name.to_string(), rendered));
            }

            out.push(FrameSnapshot {
                function,
                line,
                column,
                locals,
            });
        }
        out
    }

    /// Call ANY value, the single primitive both engines re-enter through.
    ///
    /// This is the bridge in BOTH directions:
    /// - A `Value::closure` (`native → VM`): a native higher-order stdlib function
    ///   (`array.map`, a sort comparator, `recover`, …) invokes a user callback
    ///   the VM produced. We build a fresh one-frame [`Fiber`] whose sole frame is
    ///   the closure called with `args`, then drive it to completion. Each closure
    ///   invocation gets its OWN Fiber, so the reentrant nesting (VM run → native
    ///   HOF → `call_value` → `Vm::call_value` → `run(new fiber)`) is naturally
    ///   recursive and self-contained.
    /// - Anything else (`VM → native`): delegate to the shared
    ///   [`Interp::call_value`] — identical to the `Op::Call` non-Closure arm.
    ///
    /// Arity / per-param contracts / rest collection use the SAME
    /// [`check_call_args`](crate::interp::check_call_args) the tree-walker and the
    /// `Op::Call` arm use, so a closure called from native code binds its args and
    /// surfaces arity/contract panics byte-identically. The return-type contract is
    /// enforced by `Op::Return` against the frame's `ret_span` (the call span),
    /// exactly as for an in-VM call.
    /// Workers Spec B §Task 5 (actor isolate side): call the method `name` on a VM
    /// instance `receiver` with `args`, resolving the method through the VM's
    /// per-class method side table (`vm_read_member` → `BoundMethod`) and driving any
    /// returned `Value::future` (an `async` method) to its value. Used by the actor
    /// mailbox loop, which runs on the isolate's own `Vm` — `Interp::read_member`
    /// cannot be used because a VM-built class keeps its methods in the side table,
    /// not in `Class.methods`.
    pub async fn call_method_named(
        &self,
        receiver: Value,
        name: &str,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let bound = self.vm_read_member(&receiver, name, span)?;
        let r = self.call_value(bound, args, span).await?;
        match r.into_kind() {
            OwnedKind::Future(f) => f.get().await,
            other => Ok(rebuild_value(other)),
        }
    }

    /// VM-aware defer entry execution (DEFER §3.1). Routes through `Vm::call_value`
    /// instead of `Interp::call_value` so VM-compiled class methods (stored in
    /// `class_methods`, NOT in `Class.methods`) dispatch correctly. Mirrors
    /// `Interp::exec_defer_entry`'s semantics exactly: discards the return value,
    /// converts `Propagate` to `Ok(())`, surfaces `Panic`/`Exit` to the caller.
    ///
    /// For `DeferKind::Method { recv, name }` (hook receivers only — schema,
    /// workflow-ctx, shared, actor, worker-class spawn): re-enters
    /// `Interp::call_method_recv` so the hook dispatch fires.
    /// For `DeferKind::Call { callee }`: dispatches via `Vm::call_value`.
    async fn vm_exec_defer_entry(
        &self,
        entry: crate::interp::DeferEntry,
    ) -> Result<(), crate::interp::Control> {
        use crate::interp::Control;
        // DEFER §3.4 VM fix: a bare `defer async_fn()` must produce the §3.4 loud
        // Tier-2 error on the VM exactly as on the tree-walker. On the tree-walker,
        // `call_value` for an async fn returns `Value::future`, and the check below
        // (`else if let Value::future(_)`) fires. On the VM, `Vm::call_value` runs the
        // async body INLINE (a fresh fiber, no `spawn_local`) and returns the body
        // result directly — so the result is `Value::nil()`, not `Value::future`, and the
        // check never fires. We detect the mismatch HERE, before calling, by inspecting
        // whether the callee is a VM async closure: if it is and `!entry.awaited`, raise
        // the §3.4 panic immediately (byte-identical to the tree-walker). The `awaited`
        // path intentionally keeps the inline execution (running the body synchronously
        // inside the drain is correct for `defer await` — the side effects must happen
        // before the drain completes, and a `spawn_local` inside the panic-unwind path
        // would race with the caller frame's teardown).
        if !entry.awaited {
            let is_vm_async_closure = match &entry.kind {
                crate::interp::DeferKind::Call { callee } => match callee.kind() {
                    ValueKind::Closure(c) => c.proto.is_async && !c.proto.is_generator,
                    _ => false,
                },
                crate::interp::DeferKind::Method { .. } => false,
            };
            if is_vm_async_closure {
                return Err(Control::Panic(crate::AsError::at(
                    "deferred call returned a future that would be cancelled on drop \
                     — use 'defer await f()' or do async cleanup before exit",
                    entry.span,
                )));
            }
        }
        let result = match entry.kind {
            crate::interp::DeferKind::Call { callee } => {
                // Route through the VM's call_value so VM-compiled BoundMethods
                // and Closures are dispatched correctly (the tree-walker's
                // call_value would use the stub BoundMethod's empty params).
                self.call_value(callee, entry.args, entry.span).await
            }
            crate::interp::DeferKind::Method { recv, name } => {
                // Hook receivers (schema, workflow-ctx, shared, actor, spawn) must
                // go through call_method_recv so the hooks fire — same as the
                // interp path. This arm is only reached when member_call_is_hook
                // returned true at capture time (see Op::DeferPushMethod).
                self.interp
                    .call_method_recv(recv, &name, entry.args, entry.span)
                    .await
            }
        };
        let result_v = match result {
            Ok(v) => v,
            // A Tier-1 `?`-propagation from a deferred call: discard per §3.2.
            Err(Control::Propagate(_)) => return Ok(()),
            // Panic or Exit propagates to vm_run_defers for handling.
            Err(e) => return Err(e),
        };
        if entry.awaited {
            if let ValueKind::Future(f) = result_v.kind() {
                f.get().await.map(|_| ())?;
            }
        } else if let ValueKind::Future(_) = result_v.kind() {
            // Non-VM-compiled callables (native fns, interp builtins) that return a
            // future: the existing post-call check catches them (byte-identical to the
            // tree-walker's path).
            return Err(Control::Panic(crate::AsError::at(
                "deferred call returned a future that would be cancelled on drop \
                 — use 'defer await f()' or do async cleanup before exit",
                entry.span,
            )));
        }
        Ok(())
    }

    /// VM-aware defer drain (DEFER §3.6). Mirrors `Interp::run_defers` but routes
    /// each entry through `Vm::vm_exec_defer_entry` so VM-compiled callables are
    /// dispatched correctly. All drain sites in the VM run loop must use this.
    pub(super) async fn vm_run_defers(
        &self,
        entries: Vec<crate::interp::DeferEntry>,
        outcome: &mut Result<Value, crate::interp::Control>,
    ) {
        use crate::interp::{merge_defer_panic, Control};
        #[cfg(any(test, feature = "fuzzgen", fuzzing))]
        if !entries.is_empty() {
            crate::vm::defer_metrics::defer_metrics::CHOKEPOINT_DRAINS
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        // LIFO: entries were pushed in declaration order; drain in reverse.
        for entry in entries.into_iter().rev() {
            #[cfg(any(test, feature = "fuzzgen", fuzzing))]
            crate::vm::defer_metrics::defer_metrics::ENTRIES_DRAINED
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match self.vm_exec_defer_entry(entry).await {
                Ok(()) => {}
                Err(Control::Exit(code)) => {
                    *outcome = Err(Control::Exit(code));
                    return;
                }
                Err(Control::Panic(e)) => merge_defer_panic(outcome, e),
                Err(Control::Propagate(_)) => {}
            }
        }
    }

    #[async_recursion::async_recursion(?Send)]
    pub async fn call_value(
        &self,
        callee: Value,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        match callee.into_kind() {
            OwnedKind::Closure(closure) => {
                // Workers Spec A: a `worker fn` passed as a higher-order value (e.g.
                // `array.map(seeds, workerFn)`) must be dispatched to a pooled isolate
                // when running on the CALLER thread — otherwise the worker body runs
                // inline and NO parallelism occurs. Mirror the `Op::Call` arm's
                // `is_worker` branch so that the native → VM re-entry path produces a
                // `Value::future` (dispatched) rather than a synchronously-computed
                // result.
                //
                // INSIDE an isolate, this path is NOT taken: the `Op::Call` handler
                // dispatches `dispatch_worker_inline` for the inline-nesting case, and
                // `dispatch_worker_inline`'s `spawn_local` task calls `vm.call_value`
                // on the entry — at that point we are still inside the isolate thread
                // and the entry must run as a plain closure (the code-slice already
                // defined it; running it inline IS the intended behavior). Skip the
                // re-dispatch to avoid infinite recursion:
                //   dispatch_worker_closure → in_isolate → dispatch_worker_inline
                //   → vm.call_value → is_worker → dispatch_worker_closure → ...
                // Only re-dispatch when there is something to build a slice FROM;
                // if neither worker_source nor worker_aso_bytes is set, we are in a
                // test harness or a fresh isolate that loaded the slice directly —
                // fall through to the plain inline path (which is correct there).
                if closure.proto.is_worker
                    && !closure.proto.is_generator
                    && !crate::worker::pool::in_isolate()
                    && (self.interp.worker_source().is_some()
                        || self.interp.worker_aso_bytes().is_some())
                {
                    return self.dispatch_worker_closure(&closure, args, span);
                }
                // A GENERATOR closure (`fn*` / `async fn*` / `worker fn*`) is NOT run to
                // completion here — it builds a NOT-STARTED VM fiber wrapped in a
                // `GeneratorHandle`, returning a `Value::generator` (the consumer drives
                // it via `resume`). This mirrors the `Op::Call` generator arm and is the
                // path taken when a `worker fn*` runs ON ITS DEDICATED ISOLATE: the
                // isolate's `build_producer` calls `call_value(entry, ..)` and expects a
                // LOCAL generator back (the cross-thread streaming is the CALLER-side
                // driver, not the isolate's). Without this, the body would run inline and
                // hit "a closure cannot yield".
                if closure.proto.is_generator {
                    let what = closure.proto.chunk.name.as_deref().unwrap_or("function");
                    let bound =
                        crate::interp::check_call_args(&closure.proto.params, args, span, what, Some(&self.interp), Some(&self.class_env()), false)?;
                    let mut gfiber = Fiber::new(closure);
                    gfiber.frame_mut().ret_span = span;
                    gfiber.frame_mut().argc = bound.supplied;
                    let cells = gfiber.frame().cells.clone();
                    for (slot, v) in bound.values.into_iter().enumerate() {
                        // CALL §2 A1: use .get so empty-vec is safe.
                        if let Some(cell) = cells.get(slot).and_then(|c| c.as_ref()) {
                            *cell.borrow_mut() = v;
                        } else {
                            gfiber.stack[slot] = v;
                        }
                    }
                    let handle =
                        crate::coro::GeneratorHandle::new_vm(gfiber, Rc::downgrade(&self.rc()));
                    return Ok(Value::generator(Rc::new(handle)));
                }
                // `what` mirrors the tree-walker's
                // `func.name.as_deref().unwrap_or("function")` so an arity/contract
                // panic message matches.
                let what = closure.proto.chunk.name.as_deref().unwrap_or("function");
                // Arity + per-param contracts + rest collection, shared verbatim
                // with the tree-walker and the `Op::Call` arm.
                let bound =
                    crate::interp::check_call_args(&closure.proto.params, args, span, what, Some(&self.interp), Some(&self.class_env()), false)?;
                // CALL §4 A3: take a pooled fiber (or allocate a fresh one when
                // the pool is empty or call_fast=false). `take_pooled_fiber` pops
                // the fiber from the pool so nested re-entrant calls grab a DIFFERENT
                // entry — safe by construction.
                let mut fiber = self.take_pooled_fiber(closure);
                fiber.frame_mut().ret_span = span;
                fiber.frame_mut().argc = bound.supplied;
                // Snapshot the cell `Rc`s for the param slots so we don't hold a
                // frame borrow while also writing `fiber.stack` (plain slots).
                let cells = fiber.frame().cells.clone();
                for (slot, v) in bound.values.into_iter().enumerate() {
                    // CALL §2 A1: use .get so empty-vec is safe.
                    if let Some(cell) = cells.get(slot).and_then(|c| c.as_ref()) {
                        *cell.borrow_mut() = v;
                    } else {
                        fiber.stack[slot] = v;
                    }
                }
                // SP3 §B: this re-enters `Vm::run` on a FRESH native stack frame —
                // the fiber's initial frame is one logical call. Guard it (RAII, the
                // counter unwinds on drop); the initial frame's RETURN hits the root
                // path in `return_from_frame` which does NOT decrement, so the guard
                // owns exactly that unit. A `Cell`, never held as a RefCell borrow
                // across the `.await`.
                let _depth = self.interp.enter_call_depth_scoped(span)?;
                // Drive the fiber to completion. A top-level closure body cannot
                // `yield` (yield is only valid inside a generator, which is driven
                // differently), so `Done(v)` is the only outcome; a `yield` here
                // would be a compiler bug.
                // SP9 §1: this is a native re-entry funnel for higher-order stdlib
                // callbacks (`array.map`/`reduce`/comparators) — a deep `map`-of-`map`
                // nests Rust frames here. Grow the native stack per poll so the
                // re-entry reaches the logical cap cleanly instead of SIGABRTing.
                // CALL §4 A3: return the fiber to the pool ONLY on Done; on Err
                // the fiber is dropped (never pooled — mid-flight state).
                match crate::vm::stack::grow_future(self.run(&mut fiber)).await {
                    Ok(RunOutcome::Done(v)) => {
                        self.return_pooled_fiber(fiber);
                        Ok(v)
                    }
                    Ok(RunOutcome::Yielded(_)) => {
                        unreachable!("a closure called via Vm::call_value cannot yield")
                    }
                    Err(e) => Err(e),
                }
            }
            // A class constructor (V9): build an instance VM-side (defaults via
            // thunks + compiled `init`) so the init method runs as COMPILED code.
            OwnedKind::Class(class) if self.is_vm_class(&class) => {
                self.vm_construct(class, args, span).await
            }
            // A bound method (V9) on a VM-registered class: run the COMPILED method
            // closure with `self` bound to the receiver (slot 0).
            OwnedKind::BoundMethod(bm) if self.bound_method_is_vm(&bm).is_some() => {
                let closure = self.bound_method_is_vm(&bm).expect("checked above");
                // The BoundMethod's `defining_class` is the class that actually
                // declared the method (set by `vm_read_member` / `Op::GetSuper` via
                // the chain walk), so a `super.<name>` inside it resolves correctly.
                self.invoke_compiled_method(
                    closure,
                    bm.receiver.clone(),
                    args,
                    span,
                    Some(bm.defining_class.clone()),
                )
                .await
            }
            // Native callee: delegate to the shared dispatch (same as the
            // `Op::Call` non-Closure arm). Rebuild the owned `Value` from the
            // consumed kind (zero-clone: each handle moves straight back through
            // its total constructor).
            other => {
                let callee = rebuild_value(other);
                self.interp.call_value(callee, args, span).await
            }
        }
    }

    /// Whether `class` is a VM-registered class (it has a compiled-method table).
    /// A class minted by the tree-walker (e.g. via a native module) is NOT here, so
    /// it falls through to the shared `Interp` dispatch.
    fn is_vm_class(&self, class: &Rc<crate::value::Class>) -> bool {
        let key = Rc::as_ptr(class) as usize;
        self.class_methods.borrow().contains_key(&key)
    }

    /// IFACE: whether `class` (walking its superclass chain) exposes an INSTANCE method
    /// `name` whose call-shape satisfies `req` — consulting the VM's compiled-method
    /// side table (VM classes keep an EMPTY `Class.methods`; their methods live keyed by
    /// `Rc::as_ptr` here). Returns `Some(true/false)` when `class` is a VM class,
    /// `None` when it is not VM-registered (so `conforms` falls back to the shared
    /// tree-walker `find_method`). Mirrors `arity_compatible` over the compiled proto's
    /// params.
    pub(crate) fn class_method_satisfies(
        &self,
        class: &Rc<crate::value::Class>,
        name: &str,
        req: &crate::value::MethodReq,
    ) -> Option<bool> {
        if !self.is_vm_class(class) {
            return None;
        }
        match self.find_compiled_method(class, name) {
            Some((closure, _)) => {
                let params = &closure.proto.params;
                Some(crate::interp::arity_compatible_params(params, req))
            }
            None => Some(false),
        }
    }

    /// The compiled method closure for `(class identity, name)` looked up ON the
    /// given class ONLY (no chain walk), if registered.
    fn compiled_method_own(
        &self,
        class: &Rc<crate::value::Class>,
        name: &str,
    ) -> Option<Cc<Closure>> {
        let key = Rc::as_ptr(class) as usize;
        self.class_methods
            .borrow()
            .get(&key)
            .and_then(|m| m.get(name))
            .cloned()
    }

    /// Walk the superclass chain from `class` upward, returning the first compiled
    /// method named `name` plus the ANCESTOR class that DEFINED it. The VM method
    /// side-table is keyed by `Rc::as_ptr(class)`, so walking the chain means
    /// probing each ancestor's table in turn. Mirrors the tree-walker's
    /// `value::find_method` (own class first, then up `superclass`), so an
    /// inherited method runs the ancestor's COMPILED closure and a `super` lookup
    /// gets the correct defining class.
    fn find_compiled_method(
        &self,
        class: &Rc<crate::value::Class>,
        name: &str,
    ) -> Option<(Cc<Closure>, Rc<crate::value::Class>)> {
        let mut cur = Some(class.clone());
        while let Some(c) = cur {
            if let Some(closure) = self.compiled_method_own(&c, name) {
                return Some((closure, c));
            }
            cur = c.superclass.clone();
        }
        None
    }

    /// The compiled STATIC closure for `(class identity, name)` looked up ON the
    /// given class ONLY (no chain walk), if registered (SP1 §3).
    fn compiled_static_own(
        &self,
        class: &Rc<crate::value::Class>,
        name: &str,
    ) -> Option<Cc<Closure>> {
        let key = Rc::as_ptr(class) as usize;
        self.class_static_methods
            .borrow()
            .get(&key)
            .and_then(|m| m.get(name))
            .cloned()
    }

    /// Walk the superclass chain for a compiled STATIC method `name` (SP1 §3),
    /// mirroring `find_compiled_method` over the static side-table. A subclass
    /// resolves an unknown static up its superclass chain.
    fn find_compiled_static_method(
        &self,
        class: &Rc<crate::value::Class>,
        name: &str,
    ) -> Option<Cc<Closure>> {
        let mut cur = Some(class.clone());
        while let Some(c) = cur {
            if let Some(closure) = self.compiled_static_own(&c, name) {
                return Some(closure);
            }
            cur = c.superclass.clone();
        }
        None
    }

    /// If `bm` is a bound method on a VM-registered class, return its compiled
    /// method closure (resolved up the chain); else `None` (so a tree-walker
    /// BoundMethod delegates).
    fn bound_method_is_vm(&self, bm: &crate::value::BoundMethod) -> Option<Cc<Closure>> {
        if let ValueKind::Instance(inst) = bm.receiver.kind() {
            let class = inst.borrow().class.clone();
            // Resolve from the method's DEFINING class (set by `vm_read_member` /
            // `Op::GetSuper`) so an inherited or super-dispatched method runs the
            // right ancestor's closure; fall back to the instance's class chain for
            // a BoundMethod minted elsewhere.
            return self
                .find_compiled_method(&bm.defining_class, &bm.name)
                .or_else(|| self.find_compiled_method(&class, &bm.name))
                .map(|(closure, _)| closure);
        }
        None
    }

    /// VM member read (V9). For an `Instance` of a VM-registered class, a method
    /// name resolves to a `Value::bound_method` carrying the receiver + class +
    /// method name (the compiled closure is looked up at CALL time via
    /// `bound_method_is_vm`); a field name reads the stored field; anything else
    /// (and any non-VM receiver) delegates to the shared `Interp::read_member` so
    /// the two engines share field/enum/native member-access semantics. The dummy
    /// `Method` carried by the `BoundMethod` is never executed by the VM — its body
    /// is empty — it exists only to satisfy the frozen `value.rs` `BoundMethod`
    /// shape; method dispatch always runs the COMPILED closure.
    /// FIELD inline-cache fast path for `GET_PROP` (V11-T3). Returns `Some(value)`
    /// when `name` resolves to a FIELD of a shaped `Object`/`Instance` — either
    /// from a cache HIT (`recv.shape == cached.shape` → read `get_index(idx)`) or
    /// after a fresh generic field resolution that is then RECORDED into the cache.
    /// Returns `None` when the field fast path does not apply — in which case the
    /// caller MUST take the generic `vm_read_member` path (which resolves methods,
    /// enums, natives, nil, non-shaped receivers, …). The returned value is always
    /// byte-identical to what `vm_read_member` would return for the same input.
    ///
    /// GUARDS (force `None`, i.e. generic path):
    /// - a receiver that is not an `Object`/`Instance` (modules, strings, enums,
    ///   classes, generators, nil → handled by `read_member`);
    /// - a shape of `0` (unset — a tree-walker-built value the IC cannot key on);
    /// - a SCHEMA-VALUE object (`is_schema_value`): never cached, so a schema
    ///   object's member access always flows through the generic path;
    /// - a name that is NOT a field (`get_index_of` → `None`): on an Instance this
    ///   is a METHOD (→ BoundMethod via generic) or a missing field; either way the
    ///   IC neither caches nor answers, so it can never return a wrong value for a
    ///   method-named access.
    fn ic_get_field(
        &self,
        chunk: &crate::vm::chunk::Chunk,
        op_off: usize,
        obj: &Value,
        name: &str,
    ) -> Option<Value> {
        match obj.kind() {
            ValueKind::Object(cell) => {
                let shape = cell.shape.get();
                if shape == 0 || crate::stdlib::schema::is_schema_value(obj) {
                    return None;
                }
                // Cache hit: read the field directly by its stable index.
                let ic = chunk.field_ic(op_off);
                if let Some(idx) = ic.lookup(shape) {
                    // The index is keyed by shape (V11-T2: shape ⇒ key layout), so
                    // it is always in range for an object of that shape.
                    if let Some(v) = cell.value_at(idx as usize) {
                        return Some(v);
                    }
                    // Defensive: a stale/out-of-range index never feeds a wrong
                    // value — fall through to re-resolve generically below.
                }
                // Miss: resolve the field index generically and RECORD it.
                match cell.get_index_of(name) {
                    Some(idx) => {
                        let v = cell.value_at(idx);
                        let mut ic = chunk.field_ic(op_off);
                        ic.record(shape, idx as u32);
                        chunk.set_field_ic(op_off, ic);
                        v
                    }
                    // Not a field on this object → generic path (returns nil).
                    None => None,
                }
            }
            ValueKind::Instance(inst) => {
                let b = inst.borrow();
                let shape = b.shape_id.get();
                if shape == 0 {
                    return None;
                }
                // Cache hit: read the field directly by its stable index.
                let ic = chunk.field_ic(op_off);
                if let Some(idx) = ic.lookup(shape) {
                    if let Some(v) = b.value_at(idx as usize) {
                        return Some(v);
                    }
                    // Defensive fall-through (see Object arm).
                }
                // Miss: resolve generically and record IF it is a FIELD. A
                // method-named access yields `None` here → generic path →
                // BoundMethod (never cached, never mis-answered).
                match b.get_index_of(name) {
                    Some(idx) => {
                        let v = b.value_at(idx);
                        drop(b);
                        let mut ic = chunk.field_ic(op_off);
                        ic.record(shape, idx as u32);
                        chunk.set_field_ic(op_off, ic);
                        v
                    }
                    None => None,
                }
            }
            // Every other receiver kind: generic path.
            _ => None,
        }
    }

    /// METHOD inline-cache fast path for `CALL_METHOD` (V11-T3). Returns
    /// `Some((closure, defining_class))` when `recv` is a VM `Instance` whose
    /// `name` resolves up the class chain to a COMPILED method AND is NOT shadowed
    /// by an instance field — exactly the case the generic
    /// `vm_read_member → BoundMethod → bound_method_is_vm` path would dispatch to
    /// the same compiled closure. Returns `None` (→ generic path) for every other
    /// receiver, a schema value, a name that is an instance FIELD (a field shadows
    /// a method), or a name with no compiled method.
    ///
    /// On a hit it serves the cached `(closure, defining_class)`; on a miss it
    /// resolves via `find_compiled_method` and RECORDS the result keyed by the
    /// receiver's CLASS IDENTITY (`Rc` pointer) — never the field shape, because two
    /// distinct classes may share a field layout but resolve methods differently.
    /// The SHARED method-dispatch body for `Op::CallMethod` and
    /// `Op::CallMethodSpread`. Both ops produce the SAME `(recv, args)` (CallMethod
    /// from a static argc; CallMethodSpread from a flattened runtime args array),
    /// then call this with the method `name`, the op's bytecode offset `fault_ip`
    /// (which keys the per-site method IC), and the trivia-trimmed call `span`.
    ///
    /// Dispatch mirrors the tree-walker's `eval_chain` Member-callee Call arm:
    ///   1. Schema fluent-method hook (`is_schema_value` + `is_schema_method`) →
    ///      `call_schema(name, [recv, ...args])`.
    ///   2. METHOD inline-cache fast path (V11-T3/T6): a VM instance whose `name`
    ///      resolves up the chain to a COMPILED method (not shadowed by a field).
    ///      For a plain method, push a frame onto THIS fiber and continue the run
    ///      loop in place (no fresh Fiber, no recursive `run`); for async/generator
    ///      methods, `invoke_compiled_method`.
    ///   3. Generic fallback: `vm_read_member(recv, name)` → `call_value`.
    ///
    /// On every path EXCEPT the plain-method in-frame fast path it pushes the result
    /// onto the stack; the fast path pushes a `CallFrame` and the run loop continues
    /// (RETURN pops it and pushes the result onto the caller's stack). The behavior
    /// is byte-identical between the two callers — the only difference upstream is
    /// how the arg list was obtained.
    async fn dispatch_method(
        &self,
        fiber: &mut Fiber,
        recv: Value,
        name: &str,
        args: Vec<Value>,
        fault_ip: usize,
        span: Span,
    ) -> Result<(), Control> {
        // Resolve the calling frame's `proto` so the chunk's method IC (keyed by this
        // op's bytecode offset) can be consulted without holding a fiber borrow
        // across the dispatch.
        let proto = fiber.frame().closure.proto.clone();
        // (1) Schema fluent-method hook (same predicate the tree-walker uses).
        if crate::stdlib::schema::is_schema_value(&recv)
            && crate::stdlib::schema::is_schema_method(name)
        {
            let mut sargs = Vec::with_capacity(args.len() + 1);
            sargs.push(recv);
            sargs.extend(args);
            let v = self.interp.call_schema(name, &sargs, span).await?;
            fiber.push(v);
            return Ok(());
        }
        // (1a) SP9 §2: workflow `ctx.<method>()` hook (same predicate + shape as the
        // schema hook, same routing to the shared `Interp`).
        #[cfg(feature = "workflow")]
        if crate::stdlib::workflow::is_ctx_value(&recv)
            && crate::stdlib::workflow::is_ctx_method(name)
        {
            let mut wargs = Vec::with_capacity(args.len() + 1);
            wargs.push(recv);
            wargs.extend(args);
            let v = self.interp.call_workflow_ctx(name, &wargs, span).await?;
            fiber.push(v);
            return Ok(());
        }
        // (1a') Workers Spec B §Task 5: `WorkerClass.spawn(args)` → spawn an actor
        // isolate, return `future<handle>`. Mirrors the tree-walker `eval_chain` hook
        // exactly (same `Interp::spawn_actor`), so the VM matches byte-for-byte. A
        // bare `WorkerClass(args)` construction is UNCHANGED (handled by `Op::Call`).
        if let ValueKind::Class(class) = recv.kind() {
            if class.is_worker && name == "spawn" {
                let v = self.interp.spawn_actor(class, args, span).await?;
                fiber.push(v);
                return Ok(());
            }
        }
        // (1a'') Actor-handle async method dispatch: a member-CALL on a
        // `Value::native(WorkerActor)` sends an `ActorMsg::Call` (or `close()`) and
        // returns `future<T>`. Same `Interp::actor_handle_call` as the tree-walker.
        if let ValueKind::Native(n) = recv.kind() {
            if n.kind == crate::value::NativeKind::WorkerActor {
                let v = self.interp.actor_handle_call(n, name, args, span).await?;
                fiber.push(v);
                return Ok(());
            }
        }
        // (1a''') SRV §3.5/§3.8: a member-CALL on a frozen `Value::shared` routes to
        // the read-only `call_shared` dispatcher (read-only methods; mutating-method
        // names + frozen-instance user-methods → the Tier-2 panics). Mirrors the
        // tree-walker `eval_chain` hook byte-for-byte (same `crate::interp::call_shared`).
        if let ValueKind::Shared(node) = recv.kind() {
            let v = crate::interp::call_shared(node, name, &args, span)?;
            fiber.push(v);
            return Ok(());
        }
        // (1b) STATIC method call `C.name(args)` (SP1 §3): the receiver is a VM
        // class whose `name` resolves (up the chain) to a compiled STATIC closure.
        // Dispatch with NO receiver, with full generator/async/sync handling
        // matching the `Op::Call` closure arm (so a `static fn*` returns a
        // `Value::generator` and a `static async fn` a `Value::future`, byte-
        // identical to the tree-walker's `call_static_method`). A non-static name
        // (the built-in `from`, or an error) falls through to the shared dispatch.
        if let ValueKind::Class(class) = recv.kind() {
            if self.is_vm_class(class) {
                if let Some(closure) = self.find_compiled_static_method(class, name) {
                    let v = self.invoke_compiled_static(closure, args, span).await?;
                    fiber.push(v);
                    return Ok(());
                }
            }
        }
        // (2) METHOD inline-cache fast path (V11-T3): the receiver is a VM instance
        // whose `name` resolves (up the chain) to a COMPILED method and is NOT
        // shadowed by an instance field. Byte-identical to the generic
        // `vm_read_member → BoundMethod → call_value → invoke_compiled_method` path.
        if let Some((closure, def_class)) = self
            .specialize
            .then(|| self.ic_resolve_method(&proto.chunk, fault_ip, &recv, name))
            .flatten()
        {
            // V11-T6 TUNING: for a plain (non-async, non-generator) method, push a
            // frame onto THIS fiber and continue the run loop in place — exactly like
            // the `Op::Call` VM-closure arm. Same arity/contract check, slot binding
            // (self→0, args→1..), `def_class` for `super`, and return-contract check.
            if !closure.proto.is_async && !closure.proto.is_generator {
                let what = closure.proto.chunk.name.as_deref().unwrap_or("method");
                let bound =
                    crate::interp::check_call_args(&closure.proto.params, args, span, what, Some(&self.interp), Some(&self.class_env()), false)?;
                let slot_base = fiber.stack.len();
                let slot_count = closure.proto.chunk.slot_count as usize;
                let cells = super::fiber::alloc_cells(slot_count, &closure.proto.chunk.cell_slots);
                fiber.stack.resize(slot_base + slot_count, Value::nil());
                // self -> slot 0 (cell-aware). CALL §2 A1: use .first so empty-vec
                // is safe.
                if let Some(cell) = cells.first().and_then(|c| c.as_ref()) {
                    *cell.borrow_mut() = recv;
                } else {
                    fiber.stack[slot_base] = recv;
                }
                // bound args -> slots 1..n+1 (cell-aware).
                let supplied = bound.supplied;
                for (i, v) in bound.values.into_iter().enumerate() {
                    let slot = i + 1;
                    // CALL §2 A1: use .get so empty-vec is safe.
                    if let Some(cell) = cells.get(slot).and_then(|c| c.as_ref()) {
                        *cell.borrow_mut() = v;
                    } else {
                        fiber.stack[slot_base + slot] = v;
                    }
                }
                // SP3 §B: one logical-call increment per method-call frame push
                // (matches the tree-walker `run_body`); decremented in
                // `return_from_frame`.
                self.enter_frame_depth(span)?;
                fiber.frames.push(super::fiber::CallFrame {
                    closure,
                    ip: 0,
                    slot_base,
                    cells,
                    ret_span: span,
                    def_class: Some(def_class),
                    argc: supplied,
                    defers: Vec::new(),
                });
                // DBG Task 7: publish the new (deeper) frame stack to an armed
                // profiler. Zero-cost None-check when off.
                self.publish_profile_frames(fiber);
                // Continue the loop in the new frame; RETURN pops it and pushes the
                // result onto the caller's stack.
            } else {
                let v = self
                    .invoke_compiled_method(closure, recv, args, span, Some(def_class))
                    .await?;
                fiber.push(v);
            }
            return Ok(());
        }
        // (3) Fallback: read the member, then call it. `vm_read_member` yields a VM
        // `BoundMethod` for an Instance method on a VM class (dispatched to COMPILED
        // code by `call_value`), else the SAME dispatch the tree-walker runs (a
        // BoundMethod / GeneratorMethod / NativeMethod / Builtin / … bound to
        // `recv`). This also covers an instance FIELD holding a callable (a field
        // shadows a method — the IC fast path declines those).
        let callee_v = self.vm_read_member(&recv, name, span)?;
        let v = self.call_value(callee_v, args, span).await?;
        fiber.push(v);
        Ok(())
    }

    fn ic_resolve_method(
        &self,
        chunk: &crate::vm::chunk::Chunk,
        op_off: usize,
        recv: &Value,
        name: &str,
    ) -> Option<(Cc<Closure>, Rc<crate::value::Class>)> {
        let ValueKind::Instance(inst) = recv.kind() else {
            return None;
        };
        // A field SHADOWS a method — never fast-path a field-named access (it is
        // not a method dispatch; the generic path reads the field instead).
        let (class, has_field) = {
            let b = inst.borrow();
            (b.class.clone(), b.contains_key(name))
        };
        if has_field {
            return None;
        }
        let class_id = Rc::as_ptr(&class) as usize;
        // Cache hit: serve the resolved compiled method for this exact class.
        let ic = chunk.method_ic(op_off);
        if let Some(hit) = ic.lookup(class_id) {
            return Some(hit);
        }
        // Miss: resolve the compiled method up the chain and record it.
        let resolved = self.find_compiled_method(&class, name)?;
        let mut ic = chunk.method_ic(op_off);
        ic.record(class_id, resolved.0.clone(), resolved.1.clone());
        chunk.set_method_ic(op_off, ic);
        Some(resolved)
    }

    fn vm_read_member(&self, obj: &Value, name: &str, span: Span) -> Result<Value, Control> {
        if let ValueKind::Instance(inst) = obj.kind() {
            let (class, has_field) = {
                let b = inst.borrow();
                (b.class.clone(), b.contains_key(name))
            };
            if !has_field {
                // Walk the chain so an INHERITED method binds with the ANCESTOR
                // class as `defining_class` (so a `super` inside it resolves from
                // the right link), mirroring `value::find_method`.
                if let Some((_closure, def_class)) = self.find_compiled_method(&class, name) {
                    let bm = crate::value::BoundMethod {
                        receiver: obj.clone(),
                        method: Rc::new(crate::value::Method {
                            params: Vec::new(),
                            ret: None,
                            body: Vec::new(),
                            is_async: false,
                            is_generator: false,
                            is_worker: false,
                        }),
                        defining_class: def_class,
                        name: name.to_string(),
                    };
                    return Ok(Value::bound_method(Rc::new(bm)));
                }
            }
        }
        // `C.name` static-method read (SP1 §3): a VM-compiled static resolves up
        // the superclass chain to its closure, returned as a plain `Value::closure`
        // (called with NO receiver). Falls through to the shared dispatch for the
        // built-in `from` and the "no static member" error (C3 generalization).
        if let ValueKind::Class(class) = obj.kind() {
            if self.is_vm_class(class) {
                if let Some(closure) = self.find_compiled_static_method(class, name) {
                    return Ok(Value::closure(closure));
                }
            }
        }
        // Field / non-VM receiver: shared dispatch (also yields the correct
        // nil-field / nil-receiver behavior, byte-identical to the tree-walker).
        self.interp
            .read_member(obj, name, span)
            .map_err(Control::from)
    }

    /// The current global-table version (V11-T4). Bumped on every user-global
    /// (re)definition (`Op::DefineGlobal`) or assignment (`Op::SetGlobal`). The
    /// [`GET_GLOBAL`](Op::GetGlobal) inline cache guards its cached value with this
    /// version, so a global write invalidates every cached entry. Top-level defines
    /// run once at load, then the version is stable and the caches stay hot.
    fn global_version(&self) -> u64 {
        self.global_version.get()
    }

    /// Bump the global-table version (invalidates every `GET_GLOBAL` cache entry
    /// recorded at the previous version). Saturating to avoid wraparound issues over
    /// an extremely long-lived `Vm` (a wrap is harmless for correctness — a stale
    /// cache hit would still be re-validated by the value's identity — but saturation
    /// keeps the invariant "any write changes the version" exact).
    fn bump_global_version(&self) {
        self.global_version
            .set(self.global_version.get().saturating_add(1));
    }

    /// The current STRUCTURAL generation (SP8). Bumped ONLY on a user-global DEFINE
    /// (insertion), never on a reassignment. The SP8 `IndexBound` global cache guards
    /// its stable `IndexMap` index with this generation.
    fn struct_gen(&self) -> u64 {
        self.struct_gen.get()
    }

    /// Read a module-scope user-global's (cloned) `Value` by name, or `None` if it
    /// is not (yet) defined. Public so the worker subsystem can fetch a freshly-run
    /// code-slice's ENTRY function out of a fresh isolate's globals and call it
    /// (`src/worker/dispatch.rs`); also the natural read hook for the REPL/embedders.
    pub fn user_global(&self, name: &str) -> Option<Value> {
        self.user_globals
            .borrow()
            .get(name)
            .map(|s| s.value.clone())
    }

    /// Resolve a module-scope user-global by name, returning BOTH its stable
    /// `IndexMap` index and its (cloned) `Value`, or `None` if not yet defined (SP8).
    /// The index is stable for the `Vm`'s life (user-globals are only ever inserted),
    /// so the `GET_GLOBAL` site can cache it as `GlobalCache::IndexBound`.
    fn get_user_global_full(&self, name: &str) -> Option<(usize, Value)> {
        self.user_globals
            .borrow()
            .get_full(name)
            .map(|(idx, _k, s)| (idx, s.value.clone()))
    }

    /// Read a user-global's (cloned) `Value` by its stable index (SP8 fast path).
    /// The caller has a live `IndexBound` cache entry, so the index is in range.
    fn user_global_value_at(&self, idx: usize) -> Value {
        self.user_globals
            .borrow()
            .get_index(idx)
            .map(|(_k, s)| s.value.clone())
            .expect("IndexBound cache holds an in-range user-global index")
    }

    /// Update a user-global's value IN PLACE by its stable index, returning its
    /// `mutable` flag for the SET mutability check (`Some(true)` → updated;
    /// `Some(false)` → immutable, caller errors; `None` → index out of range, caller
    /// re-resolves). Keeps the class `def_env` in sync (the same invariant as
    /// `update_user_global`). Does NOT bump any generation (a SET is not a define).
    fn set_user_global_at(&self, idx: usize, value: Value) -> Option<bool> {
        let (mutable, name) = {
            let map = self.user_globals.borrow();
            let (name, slot) = map.get_index(idx)?;
            (slot.mutable, name.clone())
        };
        if mutable {
            if let Some((_k, slot)) = self.user_globals.borrow_mut().get_index_mut(idx) {
                slot.value = value.clone();
            }
            if let Some(env) = self.class_env.borrow().as_ref() {
                let _ = env.assign(&name, value);
            }
        }
        Some(mutable)
    }

    /// Whether a module-scope user-global named `name` exists and is REASSIGNABLE.
    /// `None` if not yet defined; `Some(false)` if it is an immutable binding
    /// (`const`/`fn`/`class`/`enum`/`import`). Consulted by `Op::SetGlobal`.
    fn user_global_mutable(&self, name: &str) -> Option<bool> {
        self.user_globals.borrow().get(name).map(|s| s.mutable)
    }

    /// Update an EXISTING module-scope user-global's value (preserving its mutability
    /// flag) WITHOUT bumping the global version OR `struct_gen` (a value update cannot
    /// invalidate any cache — the SP8 user-global cache stores the STABLE INDEX, which
    /// an in-place value update does not move, and builtin caches key on the NAME's
    /// resolution target, which a value update does not change). This is exactly why a
    /// hot reassigned top-level `let` loop keeps the index cache hot every iteration.
    /// Keeps the class `def_env` in sync for `.from`/typed-parse default resolution.
    /// Caller has confirmed the key exists AND is mutable.
    fn update_user_global(&self, name: &str, value: Value) {
        if let Some(slot) = self.user_globals.borrow_mut().get_mut(name) {
            slot.value = value.clone();
        }
        if let Some(env) = self.class_env.borrow().as_ref() {
            let _ = env.assign(name, value);
        }
    }

    /// Define (create/overwrite) a module-scope user-global with its REASSIGNABILITY
    /// (`mutable` = a `let`; `false` = `const`/`fn`/`class`/`enum`/`import`) and bump
    /// the version.
    fn define_user_global(&self, name: Rc<str>, value: Value, mutable: bool) {
        self.user_globals.borrow_mut().insert(
            name.clone(),
            GlobalSlot {
                value: value.clone(),
                mutable,
            },
        );
        self.bump_global_version();
        // SP8: a DEFINE (insertion) is the ONLY event that can change which stable
        // index a name maps to (a new entry) or introduce a shadow, so it invalidates
        // every `IndexBound` cache. A plain reassignment (`update_user_global`) does
        // NOT bump this — that is the whole point (a hot reassigned-`let` loop keeps
        // the index cache hot). Saturating for an extremely long-lived `Vm`.
        self.struct_gen.set(self.struct_gen.get().saturating_add(1));
        // Keep the lazily-built class `def_env` (used by the SHARED `validate_into`
        // for `.from`/typed-parse field-default resolution) in sync, so a default
        // that references this top-level binding resolves on the `.from` path too.
        if let Some(env) = self.class_env.borrow().as_ref() {
            if env.define(&name, value.clone(), true).is_err() {
                let _ = env.assign(&name, value);
            }
        }
    }

    /// PEP-659 adaptive arithmetic (V11-T4): a guarded fast path in FRONT of the
    /// shared [`crate::interp::apply_binop`], specializing a hot arithmetic site to
    /// the monomorphic operand kind it keeps seeing.
    ///
    /// CORRECTNESS: the fast path runs ONLY after its guard confirms the exact
    /// operand kinds it specialized for, and then performs the SAME computation
    /// `apply_binop` would for those kinds (the `f64`/`Decimal`/concat arms are
    /// copied from `apply_binop`'s own arms). Every other case — a guard miss, a
    /// non-specializable op, or an as-yet-unspecialized site — falls through to
    /// `apply_binop`, which produces the canonical result and panic messages. A
    /// guard miss additionally DEOPTs the site (revert to a fresh warmup). So
    /// specialization can never change a result or a diagnostic; it only skips the
    /// generic dispatch when the kinds match. The whole-corpus differential and
    /// goldens stay byte-identical.
    /// DECODE §5 (Unit B): execute a FUSED superinstruction — N components in one
    /// dispatch. `head_off` is the fused record's `off` (= the FIRST component's
    /// byte offset); `packed` is the record's `a` (low u16 = first component's
    /// operand, high u16 = second's). Each component runs the SAME shared helper
    /// its single-op `sync_burst` arm calls, AT ITS OWN reconstructed byte offset
    /// (so a span / a panic / an adaptive-cache key is attributed identically to
    /// the unfused sequence). The caller has already advanced the record cursor
    /// (`idx += 1`); THIS sets the canonical byte ip past ALL components.
    ///
    /// Byte-identity is structural: each block below is a transcription of the
    /// single-op arm (`GetLocal`/`Const`/`GetProp`/`Add`) it stands in for —
    /// same operand source (the packed u16 == the byte the arm would read), same
    /// helper call, same fault offset, same stack effect.
    #[inline]
    fn exec_fused(
        &self,
        fiber: &mut Fiber,
        head_off: usize,
        kind: crate::vm::decode::FusedKind,
        packed: u32,
    ) -> Result<(), Control> {
        use crate::vm::decode::FusedKind;
        let lo = (packed & 0xffff) as usize; // first component's u16 operand
        let hi = ((packed >> 16) & 0xffff) as usize; // second component's u16 operand
        // Component byte offsets (the ip↔record bridge): comp0 at head_off, each
        // later component after the previous op's (1 + operand_width) bytes. The
        // component widths are compile-time constants per kind.
        let comps = kind.components();
        // off of component 1 (the second op) = head + 1 + width(comp0).
        let comp1_off = head_off + 1 + comps[0].operand_width();
        // off of component 2 (the third op, triples only).
        let comp2_off = comp1_off + 1 + comps.get(1).map_or(0, |o| o.operand_width());
        // Each arm returns the LAST component's byte offset (the canonical-ip anchor).
        let last_off = match kind {
            // ── GetLocal s; GetProp name ───────────────────────────────────────
            FusedKind::GetLocalGetProp => {
                // GetLocal s: read the local (no push — fed straight to GetProp). lo
                // = slot. GetProp name: the obj is the value GetLocal produced, fed
                // directly (the single-op pair pushes then pops it). hi = name const
                // idx; fault offset = the GetProp component's byte off.
                let lv = fiber.local(lo).clone();
                let v = self.fused_get_prop(fiber, comp1_off, hi, lv)?;
                fiber.push(v);
                comp1_off
            }
            // ── GetLocal s1; GetLocal s2 ───────────────────────────────────────
            FusedKind::GetLocalGetLocal => {
                let v1 = fiber.local(lo).clone();
                let v2 = fiber.local(hi).clone();
                fiber.push(v1);
                fiber.push(v2);
                comp1_off
            }
            // ── GetLocal s; Const k ────────────────────────────────────────────
            FusedKind::GetLocalConst => {
                let v1 = fiber.local(lo).clone();
                let v2 = fiber.frame().closure.proto.chunk.consts[hi].clone();
                fiber.push(v1);
                fiber.push(v2);
                comp1_off
            }
            // ── GetProp name; Add ──────────────────────────────────────────────
            FusedKind::GetPropAdd => {
                // GetProp name: pops the obj (top of stack), produces the field. lo =
                // name idx; fault = head_off (the GetProp component).
                let obj = fiber.pop();
                let field = self.fused_get_prop(fiber, head_off, lo, obj)?;
                // Add: pops the operand below it, combines with the field, pushes the
                // sum. The Add component's byte off = comp1_off (so its adaptive-cache
                // key is unchanged). a = the lower operand, b = field (stack order).
                let a = fiber.pop();
                let v = self.eval_binop_adaptive(fiber, comp1_off, BinOp::Add, a, field)?;
                fiber.push(v);
                comp1_off
            }
            // ── Const k; GetLocal s ────────────────────────────────────────────
            FusedKind::ConstGetLocal => {
                let v1 = fiber.frame().closure.proto.chunk.consts[lo].clone();
                let v2 = fiber.local(hi).clone();
                fiber.push(v1);
                fiber.push(v2);
                comp1_off
            }
            // ── GetLocal s; GetProp name; Add (triple) ─────────────────────────
            FusedKind::GetLocalGetPropAdd => {
                // GetLocal s; GetProp name → the field value (fed `lv` directly).
                let lv = fiber.local(lo).clone();
                let field = self.fused_get_prop(fiber, comp1_off, hi, lv)?;
                // Add: pops the operand below + the field. fault = comp2_off.
                let a = fiber.pop();
                let v = self.eval_binop_adaptive(fiber, comp2_off, BinOp::Add, a, field)?;
                fiber.push(v);
                comp2_off
            }
        };
        // Canonical ip past the LAST component (its off + 1 + its operand width).
        let last = comps[comps.len() - 1];
        fiber.frame_mut().ip = last_off + 1 + last.operand_width();
        Ok(())
    }

    /// DECODE §5 (Unit B): the `GetProp`-component body shared by every fused arm
    /// that reads a field — a transcription of the single-op `Op::GetProp` arm
    /// (NON-opt form; the peephole only fuses `Op::GetProp`, never `GetPropOpt`).
    /// `name_idx` is the field-name const index; `prop_off` is the GetProp
    /// component's byte offset (span anchor + IC key); `obj` is the receiver
    /// (already popped / read by the caller). Calls the SAME `ic_get_field` /
    /// `vm_read_member` the single-op arm calls → byte-identical.
    #[inline]
    fn fused_get_prop(
        &self,
        fiber: &Fiber,
        prop_off: usize,
        name_idx: usize,
        obj: Value,
    ) -> Result<Value, Control> {
        let name = match fiber.frame().closure.proto.chunk.consts[name_idx].kind() {
            ValueKind::Str(s) => s.clone(),
            other => {
                return Err(self.panic_at(
                    fiber,
                    prop_off,
                    format!("GET_PROP operand is not a string constant: {other:?}"),
                ))
            }
        };
        let span = fiber.frame().closure.proto.chunk.span_at(prop_off);
        let proto = fiber.frame().closure.proto.clone();
        let cached = if self.specialize {
            self.ic_get_field(&proto.chunk, prop_off, &obj, &name)
        } else {
            None
        };
        match cached {
            Some(v) => Ok(v),
            None => self.vm_read_member(&obj, &name, span),
        }
    }

    fn eval_binop_adaptive(
        &self,
        fiber: &Fiber,
        fault_ip: usize,
        op: BinOp,
        a: Value,
        b: Value,
    ) -> Result<Value, Control> {
        use crate::vm::adapt::{ArithCache, ArithKind};

        let chunk = &fiber.frame().closure.proto.chunk;
        // IFACE §5.2: `instanceof` routes through the shared `&self`
        // `Interp::eval_instanceof` (class → nominal `is_instance_of`; interface →
        // structural `conforms`) — the SAME path the tree-walker takes. It needs the
        // engine state (verdict cache) the free `apply_binop` cannot reach, so it is
        // intercepted here ahead of BOTH the kill-switch and the arithmetic fast path
        // (a pure memo, active in specialized AND generic modes → byte-identical).
        if let BinOp::InstanceOf = op {
            let span = chunk.span_at(fault_ip);
            return self.interp.eval_instanceof(a, b, span);
        }
        // KILL SWITCH (V11-T5): with specialization OFF, never observe/specialize/
        // deopt — go straight through the shared generic `apply_binop`. The result
        // and every panic message are identical to the specialized fast path (which
        // only ever runs `apply_binop`'s own arms behind a guard), so generic and
        // specialized stay byte-identical; the only difference is speed.
        if !self.specialize {
            let span = chunk.span_at(fault_ip);
            return crate::interp::apply_binop(op, a, b, span);
        }
        // Only `+ - * /`-style arithmetic participates; comparisons, equality and
        // range have no monomorphic fast path here (they go straight to generic).
        let arith_op = matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::Pow
        );
        if !arith_op {
            let span = chunk.span_at(fault_ip);
            return crate::interp::apply_binop(op, a, b, span);
        }

        let cache = chunk.arith_cache(fault_ip);

        // Already specialized: GUARD the operands; on a hit take the inline fast
        // path, on a miss DEOPT and fall to generic.
        if let Some(kind) = cache.specialized() {
            match (kind, a.kind(), b.kind()) {
                (ArithKind::Int, ValueKind::Int(x), ValueKind::Int(y)) => {
                    // SAME i64 arithmetic + checked-overflow/div-by-zero panics as
                    // apply_binop's int arm — delegated to the shared `int_binop`
                    // so the two paths cannot drift (NUM §7). The span is needed
                    // for the panic message.
                    //
                    // VAL Task 3 (inline-scalar SMI fast path, spec §7.2): `*x`/`*y`
                    // read the operand's INLINE SCALAR WORD with no heap touch and no
                    // refcount op — under the Stage-1 niche layout `Value::int(i64)`
                    // carries the full i64 inline, so this IS the SMI/inline-scalar
                    // load. (Under a future Stage-2 NaN-box the matched `i64` would be
                    // the decoded i48 SMI, or the boxed-`i64` spill — the §7.2
                    // boundary differential pins both encodings byte-identical to this
                    // generic-delegating computation.)
                    let span = chunk.span_at(fault_ip);
                    return crate::interp::int_binop(op, x, y, span);
                }
                (ArithKind::Number, ValueKind::Float(x), ValueKind::Float(y)) => {
                    // SAME f64 arithmetic as apply_binop's final numeric arm.
                    return Ok(number_fast(op, x, y));
                }
                (ArithKind::Decimal, ValueKind::Decimal(x), ValueKind::Decimal(y))
                    if ArithCache::decimal_specializable(op) =>
                {
                    // SAME rust_decimal op as apply_binop's decimal arm. Both
                    // operands are real Decimals (always finite), Add/Sub/Mul only
                    // — no coercion, no div-by-zero. `decimal_fast` returns `None`
                    // on 96-bit-mantissa overflow; we then DEOPT to the shared
                    // `apply_binop`, which raises the canonical Tier-2
                    // `decimal <op> overflowed` panic (byte-identical message — a
                    // bare operator would `panic!`/abort instead).
                    if let Some(v) = decimal_fast(op, **x, **y) {
                        return Ok(v);
                    }
                    let span = chunk.span_at(fault_ip);
                    return crate::interp::apply_binop(op, a, b, span);
                }
                (ArithKind::ConcatStr, ValueKind::Str(x), ValueKind::Str(y))
                    if matches!(op, BinOp::Add) =>
                {
                    // SAME concat as apply_binop's string arm.
                    return Ok(Value::str(format!("{}{}", x, y)));
                }
                _ => {
                    // Guard miss: deopt and run the generic path.
                    chunk.set_arith_cache(fault_ip, cache.deopt());
                    let span = chunk.span_at(fault_ip);
                    return crate::interp::apply_binop(op, a, b, span);
                }
            }
        }

        // Not specialized yet: OBSERVE this execution's operand kinds (warmup),
        // then run the generic path (the result is identical regardless of warmup).
        let observed = match (a.kind(), b.kind()) {
            (ValueKind::Int(_), ValueKind::Int(_)) => Some(ArithKind::Int),
            (ValueKind::Float(_), ValueKind::Float(_)) => Some(ArithKind::Number),
            (ValueKind::Decimal(_), ValueKind::Decimal(_))
                if ArithCache::decimal_specializable(op) =>
            {
                Some(ArithKind::Decimal)
            }
            (ValueKind::Str(_), ValueKind::Str(_)) if matches!(op, BinOp::Add) => {
                Some(ArithKind::ConcatStr)
            }
            _ => None,
        };
        chunk.set_arith_cache(fault_ip, cache.observe(observed));
        let span = chunk.span_at(fault_ip);
        crate::interp::apply_binop(op, a, b, span)
    }

    /// Store `name = value` on an Object cell, preserving exact IndexMap semantics:
    /// - **Existing key:** overwrite in place (shape unchanged; position kept).
    /// - **New key on slab:** one registry transition (`add_key`) + `slab_append`
    ///   (shape transitions to the child). A cap refusal demotes to dict first.
    /// - **New key on dict / already-demoted:** plain dict insert (shape stays 0).
    ///
    /// This replaces `set_member` + `resync_object_shape` for the Object case:
    /// the shape is always exactly right AFTER this call — no re-walk needed.
    /// The frozen check is the CALLER's responsibility (`vm_set_prop` / `SetIndex`
    /// arm checks `check_not_frozen` before reaching here). SHAPE Task 3.1.
    fn vm_object_insert(&self, cell: &Cc<crate::value::ObjectCell>, name: &str, value: Value) {
        // Fast path: key already exists — overwrite in place, shape unchanged.
        if let Some(i) = cell.get_index_of(name) {
            cell.set_value_at(i, value);
            return;
        }
        // New key. Try a registry transition from the current shape if the cell
        // is in slab mode (shape 0 is valid for a freshly-built empty literal).
        let shape = cell.shape.get();
        if cell.is_slab() {
            let mut reg = self.shapes.borrow_mut();
            if let Some(child) = reg.add_key(shape, name) {
                let child_keys = reg.keys_of(child);
                drop(reg);
                if cell.slab_append(child, child_keys, value.clone()) {
                    return; // successful slab grow
                }
                // slab_append returning false means the cell is in dict mode
                // (can't happen in practice; defensive fall-through to dict insert).
            } else {
                drop(reg);
                // Cap exceeded: demote to dict then insert.
                cell.demote_to_dict();
                // SHAPE §3.5: a demotion IS a fresh dict construction — bump both.
                #[cfg(any(test, feature = "fuzzgen", fuzzing))]
                self.bump_shape_stat(|s| {
                    s.obj_demotions += 1;
                    s.obj_dict_constructed += 1;
                });
            }
        }
        // Dict mode (or just demoted): plain insert, shape stays 0.
        cell.insert(name, value);
    }

    /// Store `inst.<name> = value` into a VM (slab) instance with the PRECISE
    /// registry transition (SHAPE Task 3.4) — the instance flavor of
    /// `vm_object_insert`. Does NO contract check (the caller runs it via the shared
    /// `check_instance_field_contract` before calling this). Behavior:
    ///
    /// - **Existing key:** overwrite in place via `set_value_at`; shape unchanged.
    /// - **New key, slab mode:** mint/follow the registry edge (`add_key`) and grow
    ///   the slab (`slab_append`), transitioning `shape_id` to the child shape.
    /// - **New key, cap exceeded:** demote to dict (shape → 0), then dict-insert.
    /// - **New key, dict mode:** plain dict insert (shape stays 0).
    ///
    /// Replaces the old `resync_instance_shape` full re-derive — each write
    /// transitions precisely (analogous to the object path), so the GET/SET field IC
    /// stays sound with no re-derive.
    fn vm_instance_insert(
        &self,
        inst: &Cc<RefCell<crate::value::Instance>>,
        name: &str,
        value: Value,
    ) {
        // Fast path: key already exists — overwrite in place, shape unchanged.
        let existing = inst.borrow().get_index_of(name);
        if let Some(i) = existing {
            inst.borrow_mut().set_value_at(i, value);
            return;
        }
        // New key. Try a registry transition from the current shape if slab mode.
        let (is_slab, shape) = {
            let b = inst.borrow();
            (b.is_slab(), b.shape_id.get())
        };
        if is_slab {
            let mut reg = self.shapes.borrow_mut();
            if let Some(child) = reg.add_key(shape, name) {
                let child_keys = reg.keys_of(child);
                drop(reg);
                if inst.borrow_mut().slab_append(child, child_keys, value.clone()) {
                    return; // successful slab grow
                }
                // slab_append false ⇒ not slab (can't happen here); fall through.
            } else {
                drop(reg);
                // Cap exceeded: demote to dict (shape → 0) then dict-insert.
                inst.borrow_mut().demote_to_dict();
                // SHAPE §3.5: a demotion IS a fresh dict construction — bump both.
                #[cfg(any(test, feature = "fuzzgen", fuzzing))]
                self.bump_shape_stat(|s| {
                    s.obj_demotions += 1;
                    s.obj_dict_constructed += 1;
                });
            }
        }
        // Dict mode (or just demoted): plain insert, shape stays 0.
        inst.borrow_mut().insert(name, value);
    }

    /// `SET_PROP` with the field inline cache (V11-T3). Stores `obj.<name> = value`
    /// and returns `value`, BYTE-IDENTICALLY to the generic `set_member` path:
    ///
    /// - **Object, existing field, cache hit (shape unchanged):** write the value
    ///   in place at the cached index via `get_index_mut`. This is identical to
    ///   `IndexMap::insert` of an EXISTING key (same slot, same position), so the
    ///   shape does not change and no resync is needed. Objects carry no field-type
    ///   contracts, so there is nothing to check.
    /// - **Object, miss:** fall to `set_member` (which may ADD a key), then resync
    ///   the object's shape and RECORD the (now-existing) field's index for next
    ///   time. Adding a key transitions the shape, so a prior cache entry for the
    ///   old shape correctly misses.
    /// - **Instance (always):** go through `set_member` so the declared FIELD-TYPE
    ///   CONTRACT is applied exactly as the tree-walker (same panic message/span) —
    ///   the IC never bypasses the contract. Then resync the instance shape (a set
    ///   may have added an undeclared field) and record the field's index.
    /// - **Any other receiver:** `set_member` raises the same Tier-2 "cannot set
    ///   property" panic.
    fn vm_set_prop(
        &self,
        chunk: &crate::vm::chunk::Chunk,
        op_off: usize,
        obj: &Value,
        name: &str,
        value: Value,
        span: Span,
    ) -> Result<Value, Control> {
        // `object.freeze` guard (SP2 §4): BEFORE any write — incl. the IC fast
        // path below, which bypasses `set_member`. Byte-identical to the
        // tree-walker's `set_member` frozen check.
        crate::interp::check_not_frozen(obj, span)?;
        match obj.kind() {
            ValueKind::Object(cell) => {
                let shape = cell.shape.get();
                // Fast path (specialize ON only): a shaped, non-schema object whose
                // key already exists at the cached index — write in place (no
                // layout/shape change). KILL SWITCH (V11-T5): skipped when OFF.
                if self.specialize && shape != 0 && !crate::stdlib::schema::is_schema_value(obj) {
                    let ic = chunk.field_ic(op_off);
                    if let Some(idx) = ic.lookup(shape) {
                        if cell.set_value_at(idx as usize, value.clone()) {
                            return Ok(value);
                        }
                        // Defensive: stale index → fall through to generic set.
                    }
                }
                // SHAPE Task 3.1: generic store via vm_object_insert (precise
                // registry transition; no resync needed — the shape is already
                // up-to-date after the insert). Replaces set_member + resync.
                self.vm_object_insert(cell, name, value.clone());
                let new_shape = cell.shape.get();
                if self.specialize && new_shape != 0 && !crate::stdlib::schema::is_schema_value(obj)
                {
                    if let Some(idx) = cell.get_index_of(name) {
                        let mut ic = chunk.field_ic(op_off);
                        ic.record(new_shape, idx as u32);
                        chunk.set_field_ic(op_off, ic);
                    }
                }
                Ok(value)
            }
            ValueKind::Instance(inst) => {
                // SHAPE Task 3.4: run the declared field-type CONTRACT via the SHARED
                // chokepoint (`check_instance_field_contract`, the same code the
                // tree-walker's `set_member` reaches) — byte-identical panic
                // message/span. The frozen guard already ran at the top of this fn.
                let class = inst.borrow().class.clone();
                self.interp
                    .check_instance_field_contract(&class, name, &value, span)?;
                // Then do ONE precise registry transition / demotion (existing key →
                // overwrite in place, shape unchanged; new key → slab grow or
                // demote). Replaces set_member + the old full re-derive resync.
                self.vm_instance_insert(inst, name, value.clone());
                // KILL SWITCH (V11-T5): only record the field IC when specialize ON.
                let recorded = self.specialize.then(|| {
                    let b = inst.borrow();
                    let new_shape = b.shape_id.get();
                    (new_shape != 0)
                        .then(|| b.get_index_of(name).map(|idx| (new_shape, idx)))
                        .flatten()
                });
                if let Some(Some((new_shape, idx))) = recorded {
                    let mut ic = chunk.field_ic(op_off);
                    ic.record(new_shape, idx as u32);
                    chunk.set_field_ic(op_off, ic);
                }
                Ok(value)
            }
            // Non-settable receiver: shared Tier-2 panic (byte-identical).
            _ => self.interp.set_member(obj, name, value, span, span),
        }
    }

    /// Construct an instance of a VM-registered class (V9). Mirrors the
    /// tree-walker's `construct`: create the instance, apply field DEFAULTS (each
    /// via its compiled thunk closure, so a mutable default is fresh per instance),
    /// checking each default against its field-type contract, then run the compiled
    /// `init` method (if present) with the args; a class with no `init` rejects any
    /// args, byte-identically.
    #[async_recursion::async_recursion(?Send)]
    async fn vm_construct(
        &self,
        class: Rc<crate::value::Class>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        // SHAPE Task 3.4: build the instance's fields as an EMPTY slab at
        // EMPTY_SHAPE (shape 0). Each default / init-assigned / auto-init field is
        // inserted through the precise registry transition (`vm_instance_insert`),
        // so the FINAL shape reflects the ACTUAL field layout in insertion order —
        // identical to the old "IndexMap then resync" end state, but transitioned
        // precisely (no trailing full re-derive). Mirrors the object path.
        let instance =
            Cc::new(RefCell::new(crate::value::Instance::new_empty_slab(class.clone())));
        let inst_val = Value::instance(instance.clone());
        // SHAPE §3.5: count every fresh instance slab construction.
        #[cfg(any(test, feature = "fuzzgen", fuzzing))]
        self.bump_shape_stat(|s| s.obj_slab_constructed += 1);

        // Apply field defaults BASE-CLASS FIRST so a subclass default overrides a
        // base one with the same name (mirrors the tree-walker's `construct`, which
        // iterates `merged_field_schema` — base-first). For each class in the chain
        // (deepest ancestor first), run its defaulted fields' compiled thunks (each
        // thunk is registered under THAT class's identity key) to get a fresh value,
        // check the contract, then store it. The contract panic span is the
        // construct call site (`span`), matching `construct`.
        let mut chain: Vec<Rc<crate::value::Class>> = Vec::new();
        {
            let mut cur = Some(class.clone());
            while let Some(c) = cur {
                cur = c.superclass.clone();
                chain.push(c);
            }
        }
        for c in chain.iter().rev() {
            let key = Rc::as_ptr(c) as usize;
            // Defaulted field names for THIS class, in declared (schema) order.
            let default_names: Vec<String> = self
                .class_defaults
                .borrow()
                .get(&key)
                .map(|m| {
                    c.fields
                        .keys()
                        .filter(|k| m.contains_key(*k))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            for fname in default_names {
                let thunk = self
                    .class_defaults
                    .borrow()
                    .get(&key)
                    .and_then(|m| m.get(&fname))
                    .cloned();
                let Some(thunk) = thunk else { continue };
                let dv = self
                    .call_value(Value::closure(thunk), Vec::new(), span)
                    .await?;
                if let Some(schema) = c.fields.get(&fname) {
                    if !crate::interp::check_type(&dv, &schema.ty) {
                        return Err(crate::interp::contract_panic(&schema.ty, &dv, span));
                    }
                }
                self.vm_instance_insert(&instance, &fname, dv);
            }
        }

        // Run the compiled `init`, if any — resolved up the chain (a subclass may
        // inherit the base init). `def_class` is the class that DEFINED init, so a
        // `super.init(...)` inside it resolves from the correct link.
        if let Some((init, def_class)) = self.find_compiled_method(&class, "init") {
            self.invoke_compiled_method(init, inst_val.clone(), args, span, Some(def_class))
                .await?;
        } else {
            // SP2 §5 records: no explicit `init` → auto-derive a positional
            // constructor over the declared fields (merged base-first order).
            // Defaults were already applied above; the positional args OVERRIDE
            // the supplied leading fields, each contract-checked via the SHARED
            // `auto_init_bindings` helper — byte-identical arity/contract messages
            // to the tree-walker's `construct`. A zero-field class with no args is
            // unchanged (empty params → only `C()` valid).
            let fields = crate::value::merged_field_schema(&class);
            let bindings = crate::interp::auto_init_bindings(&fields, &class.name, args, span)?;
            for (fname, v) in bindings {
                self.vm_instance_insert(&instance, &fname, v);
            }
        }
        // SHAPE Task 3.4: NO trailing resync — each `vm_instance_insert` above (and
        // any `self.f = …` inside `init`, routed through `vm_set_prop` →
        // `vm_instance_insert`) transitions the shape PRECISELY, so `shape_id`
        // already reflects the real insertion-order layout. (V11-T3 IC soundness.)
        Ok(inst_val)
    }

    /// Invoke a COMPILED method closure with `self`=`receiver` bound to slot 0 and
    /// the arguments bound to slots `1..n+1`. The method proto's `arity`/`params`
    /// EXCLUDE `self` (the resolver declares `self` as the method frame's slot 0,
    /// the compiler builds the params from the user params), so arity + per-param
    /// contracts use the SAME `check_call_args` every other call path uses — the
    /// arg contract panic is byte-identical. Drives a fresh one-frame Fiber to
    /// completion (a non-generator/non-async method body cannot `yield`). Async
    /// methods are out of scope for V9-T1 (deferred — a sync `init`/method is the
    /// T1 surface).
    #[async_recursion::async_recursion(?Send)]
    async fn invoke_compiled_method(
        &self,
        closure: Cc<Closure>,
        receiver: Value,
        args: Vec<Value>,
        span: Span,
        def_class: Option<Rc<crate::value::Class>>,
    ) -> Result<Value, Control> {
        let what = closure.proto.chunk.name.as_deref().unwrap_or("method");
        // Bind the user args (arity + per-param contracts + rest) against the
        // method's declared params (which EXCLUDE self) — shared with every call
        // path. The bound values land in slots 1.. (self is slot 0).
        let bound = crate::interp::check_call_args(&closure.proto.params, args, span, what, Some(&self.interp), Some(&self.class_env()), false)?;
        // A generator method (`fn*` / `async fn*`) is NOT run inline: it binds `self`
        // and args into a NOT-STARTED fiber and wraps it in a VM-backed
        // `GeneratorHandle`, returning a `Value::generator` immediately — exactly like
        // the standalone-generator CALL path (`Op::Call`) and the tree-walker's
        // `invoke_method` generator branch. The body runs only when the consumer
        // drives it via `gen.next()` / `for await`; `self` (slot 0) is visible to a
        // `yield self.x`. Both sync and async generator methods take this path.
        if closure.proto.is_generator {
            let mut gfiber = Fiber::new(closure);
            gfiber.frame_mut().ret_span = span;
            gfiber.frame_mut().def_class = def_class;
            gfiber.frame_mut().argc = bound.supplied;
            let cells = gfiber.frame().cells.clone();
            // self -> slot 0 (cell-aware). CALL §2 A1: use .first so empty-vec is safe.
            if let Some(cell) = cells.first().and_then(|c| c.as_ref()) {
                *cell.borrow_mut() = receiver;
            } else {
                gfiber.stack[0] = receiver;
            }
            // bound args -> slots 1..n+1 (cell-aware).
            for (i, v) in bound.values.into_iter().enumerate() {
                let slot = i + 1;
                // CALL §2 A1: use .get so empty-vec is safe.
                if let Some(cell) = cells.get(slot).and_then(|c| c.as_ref()) {
                    *cell.borrow_mut() = v;
                } else {
                    gfiber.stack[slot] = v;
                }
            }
            let handle =
                crate::coro::GeneratorHandle::new_vm(gfiber, Rc::downgrade(&self.rc()));
            return Ok(Value::generator(Rc::new(handle)));
        }
        // CALL §4 A3: take a pooled fiber (or allocate fresh when pool is empty /
        // call_fast=false). Removal from pool means a nested re-entrant method
        // call takes a different entry — safe by construction.
        let mut fiber = self.take_pooled_fiber(closure);
        fiber.frame_mut().ret_span = span;
        // Record the DEFINING class so a `super.<name>` in this method body
        // (Op::GetSuper) resolves up from `def_class.superclass`, exactly like the
        // tree-walker's `invoke_method` super binding.
        fiber.frame_mut().def_class = def_class;
        fiber.frame_mut().argc = bound.supplied;
        let cells = fiber.frame().cells.clone();
        // self -> slot 0 (cell-aware, in case a nested closure captured self).
        // CALL §2 A1: use .first so empty-vec is safe.
        if let Some(cell) = cells.first().and_then(|c| c.as_ref()) {
            *cell.borrow_mut() = receiver;
        } else {
            fiber.stack[0] = receiver;
        }
        // bound args -> slots 1..n+1.
        for (i, v) in bound.values.into_iter().enumerate() {
            let slot = i + 1;
            // CALL §2 A1: use .get so empty-vec is safe.
            if let Some(cell) = cells.get(slot).and_then(|c| c.as_ref()) {
                *cell.borrow_mut() = v;
            } else {
                fiber.stack[slot] = v;
            }
        }
        // SP3 §B: native re-entry into `Vm::run` — guard the fiber's initial
        // (method) frame as one logical call (RAII, the root pop does not
        // decrement, so the guard owns this unit). Matches one tree-walker
        // `run_body`.
        let _depth = self.interp.enter_call_depth_scoped(span)?;
        // SP9 §1: native re-entry funnel for non-IC method dispatch — grow the
        // native stack per poll (see `call_value`).
        // CALL §4 A3: return fiber to pool ONLY on Done; drop on Err.
        match crate::vm::stack::grow_future(self.run(&mut fiber)).await {
            Ok(RunOutcome::Done(v)) => {
                self.return_pooled_fiber(fiber);
                Ok(v)
            }
            Ok(RunOutcome::Yielded(_)) => {
                unreachable!("a non-generator method cannot yield")
            }
            Err(e) => Err(e),
        }
    }

    /// Dispatch a compiled STATIC method (SP1 §3): a class-level call with NO
    /// receiver. Args bind to slots `0..n` (no `self` slot). A `static fn*` returns
    /// a `Value::generator`; a `static async fn` is scheduled eagerly and returns a
    /// `Value::future`; a plain static runs to completion. Mirrors the `Op::Call`
    /// closure arms and the tree-walker's `call_static_method` so the engines agree.
    #[async_recursion::async_recursion(?Send)]
    async fn invoke_compiled_static(
        &self,
        closure: Cc<Closure>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        // `static async fn`: schedule eagerly (M17), return a `Value::future`. The
        // body re-enters via `Vm::call_value` inside the spawned task with the RAW
        // args (so the arity/contract check runs INSIDE the task and surfaces
        // lazily at `await`, byte-identical to the tree-walker and the `Op::Call`
        // async-closure arm). Handled before arg-binding so the bind happens once.
        if closure.proto.is_async {
            let vm = self.rc();
            let fut = crate::task::SharedFuture::new();
            let cell = fut.cell();
            let guard = self.interp.inflight_guard();
            let handle = tokio::task::spawn_local(async move {
                let _g = guard;
                let r = vm.call_value(Value::closure(closure), args, span).await;
                cell.resolve(r);
            });
            fut.set_abort(handle.abort_handle());
            self.interp.maybe_yield_for_inflight().await;
            return Ok(Value::future(fut));
        }
        let what = closure.proto.chunk.name.as_deref().unwrap_or("function");
        let bound = crate::interp::check_call_args(&closure.proto.params, args, span, what, Some(&self.interp), Some(&self.class_env()), false)?;
        // `static fn*` / `static async fn*`: build a NOT-STARTED fiber, bind args
        // into slots 0.., wrap in a VM `GeneratorHandle`. No receiver/self slot.
        if closure.proto.is_generator {
            let mut gfiber = Fiber::new(closure);
            gfiber.frame_mut().ret_span = span;
            gfiber.frame_mut().argc = bound.supplied;
            let cells = gfiber.frame().cells.clone();
            for (slot, v) in bound.values.into_iter().enumerate() {
                // CALL §2 A1: use .get so empty-vec is safe.
                if let Some(cell) = cells.get(slot).and_then(|c| c.as_ref()) {
                    *cell.borrow_mut() = v;
                } else {
                    gfiber.stack[slot] = v;
                }
            }
            let handle = crate::coro::GeneratorHandle::new_vm(gfiber, Rc::downgrade(&self.rc()));
            return Ok(Value::generator(Rc::new(handle)));
        }
        // Plain sync static: run a fiber to completion (args bound into slots 0..,
        // no receiver) — mirrors `invoke_compiled_method`'s sync tail without the
        // `self` slot.
        // CALL §4 A3: take a pooled fiber (or fresh if pool empty / call_fast=false).
        let mut fiber = self.take_pooled_fiber(closure);
        fiber.frame_mut().ret_span = span;
        fiber.frame_mut().argc = bound.supplied;
        let cells = fiber.frame().cells.clone();
        for (slot, v) in bound.values.into_iter().enumerate() {
            // CALL §2 A1: use .get so empty-vec is safe.
            if let Some(cell) = cells.get(slot).and_then(|c| c.as_ref()) {
                *cell.borrow_mut() = v;
            } else {
                fiber.stack[slot] = v;
            }
        }
        // SP9 §1: native re-entry funnel for static-method dispatch — grow the
        // native stack per poll (see `call_value`).
        // CALL §4 A3: return fiber to pool ONLY on Done; drop on Err.
        match crate::vm::stack::grow_future(self.run(&mut fiber)).await {
            Ok(RunOutcome::Done(v)) => {
                self.return_pooled_fiber(fiber);
                Ok(v)
            }
            Ok(RunOutcome::Yielded(_)) => unreachable!("a non-generator static cannot yield"),
            Err(e) => Err(e),
        }
    }

    /// Build a Tier-2 [`Control::Panic`] whose [`AsError`] is anchored at the span
    /// of the instruction at `ip`, so ariadne points at the source exactly like
    /// the tree-walker.
    fn panic_at(&self, fiber: &Fiber, ip: usize, msg: String) -> Control {
        let chunk = &fiber.frame().closure.proto.chunk;
        let span = chunk.span_at(ip);
        // Bind the span to its OWN module's source (SP4 §3) so a panic raised in
        // one module renders its caret in that module's file even when the error
        // propagates up to a caller in a different module. `None` (no module
        // source bound — e.g. an `.aso` with no source) falls back to the entry
        // source at the top of the run, preserving single-module behavior.
        match chunk.source.borrow().as_ref() {
            Some(src) => Control::Panic(AsError::at_in(msg, span, src.clone())),
            None => Control::Panic(AsError::at(msg, span)),
        }
    }

    /// SHAPE Task 3.1/3.2 — shared `Op::NewObject` body for BOTH the sync and
    /// async lanes (the single source of truth so the two drivers cannot diverge;
    /// the four/five-mode differential enforces it). `fault_ip` is the op's
    /// bytecode offset (both the `lit_shapes` cache key and the panic span); `n`
    /// is the pair count. Pops `n` (key, value) pairs (stack top-down is
    /// vN,kN,…,v1,k1) and pushes the constructed `Value::object_cell`.
    ///
    /// On the SPECIALIZED path a warm `lit_shapes` entry skips the registry probe
    /// and IndexMap fold; a cold pass runs the generic build and records the
    /// result. On the GENERIC path (`!specialize`) the cache is NEVER read or
    /// written — the generic slab/dict build runs unconditionally. Both produce
    /// byte-identical objects.
    fn exec_new_object(&self, fiber: &mut Fiber, fault_ip: usize, n: usize) -> Result<(), Control> {
        if self.specialize {
            // Consult the per-site cache.
            let cached = fiber
                .frame()
                .closure
                .proto
                .chunk
                .lit_shape(fault_ip);
            match cached {
                Some(crate::vm::chunk::LitShapeCache::Warm {
                    shape,
                    keys,
                    slot_of_pair,
                }) => {
                    // Warm hit: pop `n` (value, key) pairs straight into slots,
                    // discarding the key constants (already validated when this
                    // site was first recorded). Build a pre-sized values vector.
                    let slot_count = keys.len();
                    let mut values: Vec<Value> = vec![Value::nil(); slot_count];
                    match &slot_of_pair {
                        // Identity case: pair `i` → slot `i`, no dups, popped
                        // order is reverse source order so write directly by
                        // index. slot_count == n here.
                        None => {
                            for slot in (0..n).rev() {
                                let value = fiber.pop();
                                let _key = fiber.pop(); // discard the key const
                                values[slot] = value;
                            }
                        }
                        // Remap / dup-fold case. We pop in REVERSE source order
                        // (last source pair first), so to honor later-source-wins
                        // we must NOT let an earlier (later-popped) pair overwrite
                        // a slot a later (earlier-popped) pair already filled.
                        // Track filled slots and write only the first arrival in
                        // pop order (= the latest source position for that slot).
                        Some(sop) => {
                            let mut filled = vec![false; slot_count];
                            for src_idx in (0..n).rev() {
                                let value = fiber.pop();
                                let _key = fiber.pop(); // discard the key const
                                let slot = sop[src_idx] as usize;
                                if !filled[slot] {
                                    values[slot] = value;
                                    filled[slot] = true;
                                }
                            }
                        }
                    }
                    let cell = crate::value::ObjectCell::new_slab(keys, values, shape);
                    // SHAPE §3.5: count this warm-hit slab build.
                    #[cfg(any(test, feature = "fuzzgen", fuzzing))]
                    self.bump_shape_stat(|s| s.obj_slab_constructed += 1);
                    fiber.push(Value::object_cell(cell));
                    return Ok(());
                }
                Some(crate::vm::chunk::LitShapeCache::Negative) => {
                    // Cap-refused site: skip the registry probe, build a dict.
                    return self.new_object_generic(fiber, fault_ip, n, false);
                }
                None => {
                    // Cold: build generically AND record the per-site cache.
                    return self.new_object_generic(fiber, fault_ip, n, true);
                }
            }
        }
        // Generic / kill-switch path: never touch the cache.
        self.new_object_generic(fiber, fault_ip, n, false)
    }

    /// The generic (registry-probe + IndexMap-fold) `NewObject` build, shared by
    /// both lanes and reused on every cold/Negative/`--no-specialize` path. When
    /// `record` is true (the specialized COLD pass) it writes the resulting
    /// `lit_shapes` entry for the site at `fault_ip`. Byte-identical to the
    /// tree-walker's `ExprKind::Object` (same order + duplicate-key semantics).
    fn new_object_generic(
        &self,
        fiber: &mut Fiber,
        fault_ip: usize,
        n: usize,
        record: bool,
    ) -> Result<(), Control> {
        // Pop `n` (key, value) pairs in source order (stack: vN,kN,…,v1,k1).
        let mut pairs: Vec<(Rc<str>, Value)> = vec![(Rc::from(""), Value::nil()); n];
        for slot in pairs.iter_mut().rev() {
            let value = fiber.pop();
            let key = match fiber.pop().into_kind() {
                OwnedKind::Str(s) => s,
                other => {
                    return Err(self.panic_at(
                        fiber,
                        fault_ip,
                        format!("NEW_OBJECT key is not a string constant: {other:?}"),
                    ))
                }
            };
            *slot = (key, value);
        }
        // Fold duplicates into an ordered (key, value) sequence: first occurrence
        // wins position, last occurrence wins value — exactly IndexMap::insert.
        // `slot_of_src[i]` is the final slab slot for the i-th SOURCE pair.
        let mut order: Vec<Rc<str>> = Vec::with_capacity(n);
        let mut vals: indexmap::IndexMap<String, Value> = indexmap::IndexMap::with_capacity(n);
        let mut slot_of_src: Vec<u16> = Vec::with_capacity(n);
        let mut had_dup = false;
        for (k, v) in &pairs {
            let ks = k.to_string();
            let slot = match vals.get_full(&ks) {
                Some((idx, _, _)) => {
                    had_dup = true;
                    idx
                }
                None => {
                    let idx = order.len();
                    order.push(k.clone());
                    idx
                }
            };
            slot_of_src.push(slot as u16);
            vals.insert(ks, v.clone());
        }
        // Try slab mode: intern the ordered key sequence through the registry. A
        // cap refusal falls back to dict mode (shape 0).
        let cell = {
            let mut reg = self.shapes.borrow_mut();
            match reg.shape_for(order.iter().map(|k| k.as_ref())) {
                Some(shape) => {
                    let keys = reg.keys_of(shape);
                    drop(reg);
                    let values: Vec<Value> = order
                        .iter()
                        .map(|k| vals.shift_remove(k.as_ref()).unwrap())
                        .collect();
                    if record {
                        // Identity fast case: no dups AND popped order already
                        // equals the slab order (slot_of_src is 0,1,2,…).
                        let identity = !had_dup
                            && slot_of_src
                                .iter()
                                .enumerate()
                                .all(|(i, &s)| s as usize == i);
                        let slot_of_pair: Option<Rc<[u16]>> = if identity {
                            None
                        } else {
                            Some(Rc::from(slot_of_src.as_slice()))
                        };
                        fiber.frame().closure.proto.chunk.set_lit_shape(
                            fault_ip,
                            crate::vm::chunk::LitShapeCache::Warm {
                                shape,
                                keys: keys.clone(),
                                slot_of_pair,
                            },
                        );
                    }
                    // SHAPE §3.5: count this generic-path slab build.
                    #[cfg(any(test, feature = "fuzzgen", fuzzing))]
                    self.bump_shape_stat(|s| s.obj_slab_constructed += 1);
                    crate::value::ObjectCell::new_slab(keys, values, shape)
                }
                None => {
                    drop(reg);
                    // Cap exceeded: fall back to dict mode (shape 0).
                    let mut map = indexmap::IndexMap::with_capacity(order.len());
                    for k in &order {
                        map.insert(k.to_string(), vals.shift_remove(k.as_ref()).unwrap());
                    }
                    if record {
                        fiber
                            .frame()
                            .closure
                            .proto
                            .chunk
                            .set_lit_shape(fault_ip, crate::vm::chunk::LitShapeCache::Negative);
                    }
                    // SHAPE §3.5: count this fresh-dict build (cap refused).
                    #[cfg(any(test, feature = "fuzzgen", fuzzing))]
                    self.bump_shape_stat(|s| s.obj_dict_constructed += 1);
                    crate::value::ObjectCell::new(map)
                }
            }
        };
        fiber.push(Value::object_cell(cell));
        Ok(())
    }

    /// Unwind ONE call frame, returning `value` from it.
    ///
    /// Shared by `Op::Return` (a normal `return v`) and `Op::Propagate` (a `?`
    /// early-return of a `[nil, err]` pair) — the two have the same mechanics:
    /// pop the current frame; if it declared a `: T` return contract, check the
    /// returned value against it (panicking exactly as the tree-walker's
    /// `run_body` does — anchored at the CALL-site span `frame.ret_span`, with the
    /// identical message — and note the tree-walker applies this same contract to a
    /// `Control::Propagate`-derived value too); truncate the stack back to the
    /// frame's `slot_base` (discarding the callee's locals/operands). Dropping the
    /// frame releases ITS cell `Rc`s — closures that captured them keep their own
    /// strong refs, so by-reference captures stay alive. Recursion is heap-bounded:
    /// each CALL pushed a heap frame and this pops one, so the Rust stack stays flat.
    ///
    /// Returns `Ok(Some(outcome))` when the ROOT frame was popped — the program is
    /// done and `outcome` is its result (the driver treats a top-level propagated
    /// pair as `Ok`, exactly like `run_file`'s `Control::Propagate => Ok`). Returns
    /// `Ok(None)` when a caller frame remains — `value` was pushed onto its stack
    /// and execution continues there.
    fn return_from_frame(
        &self,
        fiber: &mut Fiber,
        value: Value,
    ) -> Result<Option<RunOutcome>, Control> {
        let frame = fiber
            .frames
            .pop()
            .expect("return/propagate with no active frame (VM bug)");
        if let Some(ret_ty) = &frame.closure.proto.ret {
            if !crate::interp::check_type(&value, ret_ty) {
                // §6.3 paranoid: the fn name span is in `fn_rets` set.
                if let Some(ns) = frame.closure.proto.name_span {
                    if let Some(e) = self.interp.maybe_paranoid_escalate(ret_ty, &value, ns) {
                        return Err(e);
                    }
                }
                return Err(crate::interp::contract_panic(
                    ret_ty,
                    &value,
                    frame.ret_span,
                ));
            }
        }
        fiber.stack.truncate(frame.slot_base);
        if fiber.frames.is_empty() {
            // ROOT/initial frame of this fiber — its logical-depth unit is owned by
            // the program root (counter returns to 0 at program end) or by the
            // re-entrant `self.run`'s RAII guard, so do NOT decrement here.
            return Ok(Some(RunOutcome::Done(value)));
        }
        // SP3 §B: a non-root frame was popped — match the `enter_frame_depth`
        // increment from when it was pushed.
        self.leave_frame_depth();
        // DBG Task 7: publish the now-shallower frame stack to an armed profiler so a
        // sample taken after the return attributes time to the caller. Zero-cost
        // None-check when off.
        self.publish_profile_frames(fiber);
        fiber.push(value);
        Ok(None)
    }
}

/// ADT: the number of payload values a constructed variant carries (positional →
/// element count; named → field count), or `None` if `v` is not a constructed
/// variant. Used by `MATCH_VARIANT_ARITY` (the positional length guard).
fn variant_payload_len(v: &Value) -> Option<usize> {
    match v.kind() {
        ValueKind::EnumVariant(ev) => match &ev.payload {
            Some(crate::value::Payload::Positional(a)) => Some(a.borrow().len()),
            Some(crate::value::Payload::Named(o)) => Some(o.len()),
            None => None,
        },
        _ => None,
    }
}

/// Map a binary-operator opcode to the shared [`BinOp`] the tree-walker uses, so
/// both engines run the SAME `apply_binop` dispatch. Short-circuit operators
/// (`&&`/`||`/`??`) are never lowered to a single binary opcode — the compiler
/// emits jumps for them (V2-T6) — so they have no opcode and never reach here.
fn binop_of(op: Op) -> BinOp {
    match op {
        Op::Add => BinOp::Add,
        Op::Sub => BinOp::Sub,
        Op::Mul => BinOp::Mul,
        Op::Div => BinOp::Div,
        Op::Mod => BinOp::Mod,
        Op::Pow => BinOp::Pow,
        Op::Lt => BinOp::Lt,
        Op::Le => BinOp::Le,
        Op::Gt => BinOp::Gt,
        Op::Ge => BinOp::Ge,
        Op::Eq => BinOp::Eq,
        Op::Ne => BinOp::Ne,
        Op::Range => BinOp::Range,
        Op::InstanceOf => BinOp::InstanceOf,
        Op::BitAnd => BinOp::BitAnd,
        Op::BitOr => BinOp::BitOr,
        Op::BitXor => BinOp::BitXor,
        Op::Shl => BinOp::Shl,
        Op::Shr => BinOp::Shr,
        Op::WrapAdd => BinOp::WrapAdd,
        Op::WrapSub => BinOp::WrapSub,
        Op::WrapMul => BinOp::WrapMul,
        _ => unreachable!("binop_of called with non-binary opcode {op:?}"),
    }
}

// ── WARM B §3.3 — PGO seeder helpers (all pure, hostile-input-safe) ──────────────

/// Map a profile arith-kind tag byte to an [`ArithKind`], range-checked. An unknown
/// byte (from a stale/corrupt profile) yields `None` ⇒ the seeder skips the entry.
/// The byte→variant mapping mirrors `ArithKind as u8` (the harvest writes `kind as u8`).
fn arith_kind_from_tag(tag: u8) -> Option<crate::vm::adapt::ArithKind> {
    use crate::vm::adapt::ArithKind;
    match tag {
        0 => Some(ArithKind::Int),
        1 => Some(ArithKind::Number),
        2 => Some(ArithKind::Decimal),
        3 => Some(ArithKind::ConcatStr),
        _ => None,
    }
}

/// Resolve a nested proto by its index path through `chunk.protos` (empty = the chunk
/// itself), returning its `Chunk`. An out-of-range step ⇒ `None` (skip that proto):
/// a corrupt/stale profile path is never trusted to index the proto tree.
fn proto_at<'a>(
    chunk: &'a crate::vm::chunk::Chunk,
    path: &[u32],
) -> Option<&'a crate::vm::chunk::Chunk> {
    let mut cur = chunk;
    for &step in path {
        let child = cur.protos.get(step as usize)?;
        cur = &child.chunk;
    }
    Some(cur)
}

/// Read the property-name string constant the op at byte offset `operand_at` references
/// (a u16 const-pool index). Returns `None` on any anomaly (operand out of range, const
/// index out of range, or a non-string const) — the bytes are verified, but the PROFILE
/// offset is not, so this is a bounds-checked read that fails to a skip, never a panic.
fn const_str_operand(chunk: &crate::vm::chunk::Chunk, operand_at: usize) -> Option<String> {
    let code = &*chunk.code;
    let lo = *code.get(operand_at)?;
    let hi = *code.get(operand_at + 1)?;
    let idx = u16::from_le_bytes([lo, hi]) as usize;
    match chunk.consts.get(idx)?.kind() {
        crate::value::ValueKind::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

/// Inline numeric arithmetic for the `ADD_NUMBER`-family fast path (V11-T4).
/// BYTE-IDENTICAL to [`crate::interp::apply_binop`]'s final two-`Number` arm — the
/// same `f64` ops, so the specialized result equals the generic one bit-for-bit
/// (incl. `NaN`/`Infinity`/`-0.0`). Only the arithmetic ops reach here (the
/// adaptive guard restricts to Add/Sub/Mul/Div/Mod/Pow over two `Number`s).
#[inline]
fn number_fast(op: BinOp, a: f64, b: f64) -> Value {
    match op {
        BinOp::Add => Value::float(a + b),
        BinOp::Sub => Value::float(a - b),
        BinOp::Mul => Value::float(a * b),
        BinOp::Div => Value::float(a / b),
        BinOp::Mod => Value::float(a % b),
        BinOp::Pow => Value::float(a.powf(b)),
        _ => unreachable!("number_fast called with non-arithmetic op {op:?}"),
    }
}

/// Inline decimal arithmetic for the `ADD_DECIMAL`-family fast path (V11-T4).
/// BYTE-IDENTICAL to [`crate::interp::apply_binop`]'s decimal Add/Sub/Mul arms:
/// uses the CHECKED rust_decimal ops and returns `None` on 96-bit-mantissa
/// overflow so the caller deopts to the shared `apply_binop` (which raises the
/// canonical Tier-2 `decimal <op> overflowed` panic) — a bare operator would
/// `panic!` and abort the thread. Restricted by the adaptive guard to Add/Sub/Mul
/// over two real `Decimal`s (always finite), so there is no coercion, no
/// non-finite check and no div-by-zero to defer.
#[inline]
fn decimal_fast(op: BinOp, a: rust_decimal::Decimal, b: rust_decimal::Decimal) -> Option<Value> {
    let d = match op {
        BinOp::Add => a.checked_add(b),
        BinOp::Sub => a.checked_sub(b),
        BinOp::Mul => a.checked_mul(b),
        _ => unreachable!("decimal_fast called with non-specializable op {op:?}"),
    };
    d.map(Value::decimal)
}

/// Map a unary-operator opcode to the shared [`UnOp`].
fn unop_of(op: Op) -> UnOp {
    match op {
        Op::Neg => UnOp::Neg,
        Op::Not => UnOp::Not,
        Op::BitNot => UnOp::BitNot,
        _ => unreachable!("unop_of called with non-unary opcode {op:?}"),
    }
}

/// LANE §3: returns `true` iff this opcode is in the Task-5 sync subset.
/// An op NOT in this set causes the sync driver to return `NeedsAsync` (ip un-advanced).
///
/// Opcodes in the subset are: consts/stack, binop family (via eval_binop_adaptive),
/// unary, range ops, locals/globals, jumps, Template, Return/Propagate/Unwrap/Yield
/// (with defer guard), DeferPush/DeferPushMethod (capture only — no call), member/index
/// access, IterSnapshot, builders (NewArray/NewObject/NewMap/MapEntry/Spread/SpreadArgs/
/// AppendArray/AppendObject/SpreadObject/AppendNamedArg/AppendPosArg/AppendSpreadArg),
/// destructure/match family, cells/upvalues/param-prologue, Closure, and Call/CallSpread
/// (plain sync closure only — escalates for async/worker/generator/native).
///
/// NOT included (always async): Import, CallNamed, CallNamedSpread, Class,
/// DefineInterface, Break (debug trap), GetIter, worker ops.
/// Await IS included (conditional — escalates only if the future is still pending).
pub(crate) fn sync_lane_op(op: Op) -> bool {
    matches!(
        op,
        // consts / stack
        Op::Const
            | Op::Nil
            | Op::True
            | Op::False
            | Op::Pop
            | Op::Dup
            | Op::Swap
            | Op::Rot3
            | Op::Template
            // binop family
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
            | Op::InstanceOf
            | Op::InstanceOfType
            | Op::BitAnd
            | Op::BitOr
            | Op::BitXor
            | Op::Shl
            | Op::Shr
            | Op::WrapAdd
            | Op::WrapSub
            | Op::WrapMul
            | Op::Range
            // unary
            | Op::Neg
            | Op::Not
            | Op::BitNot
            // range ops
            | Op::RangeInclusive
            | Op::RangeStepValue
            | Op::RangeResolveStep
            | Op::RangeHasNext
            | Op::CheckNumbers
            // locals / globals
            | Op::GetLocal
            | Op::SetLocal
            | Op::GetGlobal
            | Op::DefineGlobal
            | Op::SetGlobal
            | Op::ImmutableError
            // jumps
            | Op::Jump
            | Op::Loop
            | Op::JumpIfFalse
            | Op::JumpIfTrue
            | Op::JumpIfNotNil
            // return / propagate / unwrap / yield (defer guard applied in sync_burst)
            | Op::Return
            | Op::Propagate
            | Op::Unwrap
            | Op::Yield
            // defer capture (no call, no await — pure stack mutation)
            | Op::DeferPush
            | Op::DeferPushMethod
            // member / index access
            | Op::GetIndex
            | Op::SetIndex
            | Op::GetProp
            | Op::GetPropOpt
            | Op::SetProp
            | Op::GetSuper
            // iter / for-of snapshot
            | Op::IterSnapshot
            | Op::ArrayLen
            // builders
            | Op::NewArray
            | Op::NewObject
            | Op::NewMap
            | Op::MapEntry
            | Op::Spread
            | Op::SpreadArgs
            | Op::AppendArray
            | Op::AppendObject
            | Op::SpreadObject
            | Op::AppendNamedArg
            | Op::AppendPosArg
            | Op::AppendSpreadArg
            // destructure / match family
            | Op::CheckArrayDestructure
            | Op::CheckObjectDestructure
            | Op::ArrayElem
            | Op::ObjectKey
            | Op::ArrayRest
            | Op::ObjectRest
            | Op::MatchArray
            | Op::MatchObject
            | Op::MatchHasKey
            | Op::MatchVariant
            | Op::MatchVariantArity
            | Op::MatchVariantHasField
            | Op::VariantElem
            | Op::VariantField
            | Op::MatchRange
            | Op::MatchNoArm
            // cells / upvalues / param-prologue
            | Op::GetLocalCell
            | Op::SetLocalCell
            | Op::FreshCell
            | Op::GetUpvalue
            | Op::SetUpvalue
            | Op::CheckParam
            | Op::CheckLocal
            | Op::JumpIfArgSupplied
            // closure construction
            | Op::Closure
            // call (plain sync closure only; escalates for async/worker/generator/native)
            | Op::Call
            // ELIDE §4.2: CallElided is dispatched IDENTICALLY to Call in the sync
            // burst (only the contract-skip flag differs), so it MUST share Call's
            // sync-lane admission — otherwise every proven call site escalates to the
            // async driver and loses LANE's fast path (a measured ~19% regression on
            // call-heavy code, untyped INCLUDED since an all-untyped-param call is a
            // free-pass elide site). The §4.4 binder fast path handles it inline.
            | Op::CallElided
            | Op::CallSpread
            // await (conditional — escalates only if the operand is a pending future;
            // a non-future operand or a resolved future completes inline per LANE §4)
            | Op::Await
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::Span;
    use crate::vm::chunk::{Chunk, FnProto};
    use crate::vm::value_ext::Closure;
    use tokio::task::LocalSet;

    /// Wrap a chunk in a closure + fiber and run it to completion on a
    /// current-thread runtime inside a `LocalSet` (the runtime is `!Send`).
    fn run_chunk(chunk: Chunk) -> Result<RunOutcome, Control> {
        let proto = Rc::new(FnProto {
            chunk,
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
            name_span: None,
        });
        let closure = Closure::new(proto);
        let mut fiber = Fiber::new(closure);

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let local = LocalSet::new();
        local.block_on(&rt, async move {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            let vm = Vm::new(interp);
            vm.run(&mut fiber).await
        })
    }

    fn expect_number(chunk: Chunk) -> f64 {
        match run_chunk(chunk).expect("run ok") {
            RunOutcome::Done(v) => match v.kind() {
                ValueKind::Float(n) => n,
                _ => panic!("expected Done(Number), got {v:?}"),
            },
            other => panic!("expected Done(Number), got {other:?}"),
        }
    }

    // `RunOutcome` has no Debug; small helper for assert messages.
    impl std::fmt::Debug for RunOutcome {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                RunOutcome::Done(v) => write!(f, "Done({v:?})"),
                RunOutcome::Yielded(v) => write!(f, "Yielded({v:?})"),
            }
        }
    }

    fn s() -> Span {
        Span::new(0, 1)
    }

    #[test]
    fn arithmetic_one_plus_two_times_four() {
        // (1 + 2) * 4 == 12
        let mut c = Chunk::new();
        let k1 = c.add_const(Value::float(1.0));
        let k2 = c.add_const(Value::float(2.0));
        let k4 = c.add_const(Value::float(4.0));
        c.emit_u16(Op::Const, k1, s());
        c.emit_u16(Op::Const, k2, s());
        c.emit(Op::Add, s());
        c.emit_u16(Op::Const, k4, s());
        c.emit(Op::Mul, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 12.0);
    }

    #[test]
    fn negate() {
        let mut c = Chunk::new();
        let k = c.add_const(Value::float(5.0));
        c.emit_u16(Op::Const, k, s());
        c.emit(Op::Neg, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), -5.0);
    }

    #[test]
    fn modulo() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::float(7.0));
        let b = c.add_const(Value::float(3.0));
        c.emit_u16(Op::Const, a, s());
        c.emit_u16(Op::Const, b, s());
        c.emit(Op::Mod, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 1.0);
    }

    #[test]
    fn power() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::float(2.0));
        let b = c.add_const(Value::float(10.0));
        c.emit_u16(Op::Const, a, s());
        c.emit_u16(Op::Const, b, s());
        c.emit(Op::Pow, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 1024.0);
    }

    #[test]
    fn less_than_true() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::float(1.0));
        let b = c.add_const(Value::float(2.0));
        c.emit_u16(Op::Const, a, s());
        c.emit_u16(Op::Const, b, s());
        c.emit(Op::Lt, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("run ok") {
            RunOutcome::Done(v) if matches!(v.kind(), ValueKind::Bool(true)) => {}
            other => panic!("expected Done(Bool), got {other:?}"),
        }
    }

    #[test]
    fn not_on_truthy() {
        let mut c = Chunk::new();
        c.emit(Op::True, s());
        c.emit(Op::Not, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("run ok") {
            RunOutcome::Done(v) if matches!(v.kind(), ValueKind::Bool(false)) => {}
            other => panic!("expected Done(Bool), got {other:?}"),
        }
    }

    #[test]
    fn eq_numbers() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::float(3.0));
        let b = c.add_const(Value::float(3.0));
        c.emit_u16(Op::Const, a, s());
        c.emit_u16(Op::Const, b, s());
        c.emit(Op::Eq, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("run ok") {
            RunOutcome::Done(v) if matches!(v.kind(), ValueKind::Bool(true)) => {}
            other => panic!("expected Done(Bool), got {other:?}"),
        }
    }

    #[test]
    fn neg_non_number_panics_with_span() {
        // Push a Str const, then NEG -> "cannot negate" panic with a real span.
        let mut c = Chunk::new();
        let k = c.add_const(Value::str("nope"));
        c.emit_u16(Op::Const, k, s());
        // give NEG a distinct, non-empty span so we can assert it is carried.
        let neg_span = Span::new(5, 9);
        c.emit(Op::Neg, neg_span);
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                assert!(
                    e.message.contains("cannot negate"),
                    "message was: {}",
                    e.message
                );
                let span = e.span.expect("panic carries a span");
                assert_eq!(span, neg_span, "panic carries the faulting op's span");
                assert!(span.end > span.start, "span is non-empty");
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn add_non_numbers_panics() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::str("a"));
        let b = c.add_const(Value::float(1.0));
        c.emit_u16(Op::Const, a, s());
        c.emit_u16(Op::Const, b, s());
        c.emit(Op::Add, s());
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => assert!(
                e.message.contains("operator requires two numbers"),
                "message was: {}",
                e.message
            ),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    /// A `Value::decimal_rc` from a decimal string literal (test helper). The VM
    /// compiler cannot yet *produce* a decimal (that needs `import`/member-access
    /// for `std/decimal`), so the decimal arithmetic path is exercised by pushing
    /// decimal consts directly. The semantics themselves are the SAME shared
    /// `apply_binop` the tree-walker runs, so these tests pin the VM's dispatch to
    /// it.
    fn dec(s: &str) -> Value {
        use std::str::FromStr;
        Value::decimal(rust_decimal::Decimal::from_str(s).expect("valid decimal literal"))
    }

    /// Push two decimal consts and apply `op`, returning the run outcome.
    fn run_decimal_binop(a: &str, op: Op, b: &str) -> Result<RunOutcome, Control> {
        let mut c = Chunk::new();
        let ka = c.add_const(dec(a));
        let kb = c.add_const(dec(b));
        c.emit_u16(Op::Const, ka, s());
        c.emit_u16(Op::Const, kb, s());
        c.emit(op, s());
        c.emit(Op::Return, s());
        run_chunk(c)
    }

    #[test]
    fn decimal_arithmetic_through_shared_dispatch() {
        // Add / Sub / Mul / Div over two decimals → Decimal, formatted exactly.
        // Expected renderings preserve rust_decimal's scale exactly (the same
        // `Value::Display` the tree-walker uses), so e.g. `3 / 2` is `1.50`.
        for (a, op, b, want) in [
            ("1.5", Op::Add, "2.5", "4.0"),
            ("2.5", Op::Sub, "0.5", "2.0"),
            ("1.5", Op::Mul, "2", "3.0"),
            ("3", Op::Div, "2", "1.50"),
        ] {
            match run_decimal_binop(a, op, b).expect("decimal arith ok") {
                RunOutcome::Done(v) => {
                    assert_eq!(v.to_string(), want, "{a} {op:?} {b} rendered wrong")
                }
                other => panic!("expected Done, got {other:?}"),
            }
        }
    }

    #[test]
    fn decimal_division_by_zero_panics() {
        match run_decimal_binop("1", Op::Div, "0") {
            Err(Control::Panic(e)) => {
                assert_eq!(e.message, "decimal division by zero", "msg: {}", e.message)
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn decimal_remainder_by_zero_panics() {
        match run_decimal_binop("1", Op::Mod, "0") {
            Err(Control::Panic(e)) => {
                assert_eq!(e.message, "decimal remainder by zero", "msg: {}", e.message)
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn decimal_pow_is_unsupported() {
        match run_decimal_binop("2", Op::Pow, "3") {
            Err(Control::Panic(e)) => assert_eq!(
                e.message,
                "exponentiation (**) is not supported for decimal; use math.pow or convert to number",
                "msg: {}",
                e.message
            ),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn decimal_ordering_through_shared_dispatch() {
        match run_decimal_binop("1.5", Op::Lt, "2.5").expect("ok") {
            RunOutcome::Done(v) if matches!(v.kind(), ValueKind::Bool(true)) => {}
            other => panic!("expected Done(Bool), got {other:?}"),
        }
        match run_decimal_binop("3", Op::Ge, "3").expect("ok") {
            RunOutcome::Done(v) if matches!(v.kind(), ValueKind::Bool(true)) => {}
            other => panic!("expected Done(Bool), got {other:?}"),
        }
    }

    #[test]
    fn decimal_vs_number_cross_equality() {
        // decimal("1") == 1 → true (cross-type Decimal↔Number equality), exactly
        // as the tree-walker's `decimal_cross_eq`.
        let mut c = Chunk::new();
        let kd = c.add_const(dec("1"));
        let kn = c.add_const(Value::float(1.0));
        c.emit_u16(Op::Const, kd, s());
        c.emit_u16(Op::Const, kn, s());
        c.emit(Op::Eq, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(v) if matches!(v.kind(), ValueKind::Bool(true)) => {}
            other => panic!("expected Done(Bool), got {other:?}"),
        }
    }

    #[test]
    fn range_op_builds_half_open_array() {
        // 0 .. 5 → [0, 1, 2, 3, 4].
        let mut c = Chunk::new();
        let k0 = c.add_const(Value::float(0.0));
        let k5 = c.add_const(Value::float(5.0));
        c.emit_u16(Op::Const, k0, s());
        c.emit_u16(Op::Const, k5, s());
        c.emit(Op::Range, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(v) => match v.kind() {
                ValueKind::Array(a) => {
                    let got: Vec<f64> = a
                        .borrow()
                        .iter()
                        .map(|v| match v.kind() {
                            ValueKind::Float(n) => n,
                            other => panic!("non-number in range array: {other:?}"),
                        })
                        .collect();
                    assert_eq!(got, vec![0.0, 1.0, 2.0, 3.0, 4.0]);
                }
                other => panic!("expected Done(Array), got {other:?}"),
            },
            other => panic!("expected Done(Array), got {other:?}"),
        }
    }

    #[test]
    fn range_op_non_number_bounds_panics() {
        let mut c = Chunk::new();
        let ks = c.add_const(Value::str("x"));
        let k5 = c.add_const(Value::float(5.0));
        c.emit_u16(Op::Const, ks, s());
        c.emit_u16(Op::Const, k5, s());
        c.emit(Op::Range, s());
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                assert_eq!(
                    e.message, "range bounds must be numbers",
                    "msg: {}",
                    e.message
                )
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn string_concat_through_add() {
        let mut c = Chunk::new();
        let ka = c.add_const(Value::str("foo"));
        let kb = c.add_const(Value::str("bar"));
        c.emit_u16(Op::Const, ka, s());
        c.emit_u16(Op::Const, kb, s());
        c.emit(Op::Add, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(v) if matches!(v.kind(), ValueKind::Str(s) if &**s == "foobar") => {}
            other => panic!("expected Done(Str), got {other:?}"),
        }
    }

    /// Run a chunk and return the shared interp's captured output alongside the
    /// outcome — for exercising the `print` builtin via `CALL`.
    fn run_chunk_with_output(chunk: Chunk) -> (Result<RunOutcome, Control>, String) {
        let proto = Rc::new(FnProto {
            chunk,
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
            name_span: None,
        });
        let closure = Closure::new(proto);
        let mut fiber = Fiber::new(closure);

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let local = LocalSet::new();
        local.block_on(&rt, async move {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            let vm = Vm::new(interp.clone());
            let outcome = vm.run(&mut fiber).await;
            (outcome, interp.output())
        })
    }

    #[test]
    fn call_print_writes_to_shared_sink() {
        // GET_GLOBAL print; CONST 42; CALL 1; RETURN (CALL leaves print's nil
        // result, which RETURN pops).
        let mut c = Chunk::new();
        let name = c.add_const(Value::str("print"));
        c.emit_u16(Op::GetGlobal, name, s());
        let k = c.add_const(Value::float(42.0));
        c.emit_u16(Op::Const, k, s());
        c.emit_u8(Op::Call, 1, s());
        c.emit(Op::Return, s());
        let (outcome, out) = run_chunk_with_output(c);
        assert!(matches!(outcome, Ok(RunOutcome::Done(_))), "ran ok");
        assert_eq!(out, "42.0\n", "print wrote to the shared capture sink");
    }

    // ---- DBG Task 1: the Vm.instrument seam (zero-cost when off) -----------

    /// Run a chunk on a VM built with the given (already-armed) instrumentation,
    /// capturing program output — the Task-1 analogue of `run_chunk_with_output`.
    fn run_chunk_with_instrument(
        chunk: Chunk,
        inst: crate::vm::instrument::Instrumentation,
    ) -> (Result<RunOutcome, Control>, String) {
        let proto = Rc::new(FnProto {
            chunk,
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
            name_span: None,
        });
        let closure = Closure::new(proto);
        let mut fiber = Fiber::new(closure);

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let local = LocalSet::new();
        local.block_on(&rt, async move {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            let vm = Vm::with_instrument(interp.clone(), inst);
            let outcome = vm.run(&mut fiber).await;
            (outcome, interp.output())
        })
    }

    #[test]
    fn default_vm_has_no_instrument() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let local = LocalSet::new();
        local.block_on(&rt, async {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            let vm = Vm::new(interp);
            assert!(
                vm.instrument.borrow().is_none(),
                "a default Vm has no instrumentation attached"
            );
            assert!(!vm.is_instrumented());
        });
    }

    #[test]
    fn with_instrument_round_trips() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let local = LocalSet::new();
        local.block_on(&rt, async {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            let inst = crate::vm::instrument::Instrumentation::empty();
            let vm = Vm::with_instrument(interp, inst);
            assert!(vm.is_instrumented(), "with_instrument arms the seam");
            assert!(vm.instrument.borrow().is_some());
        });
    }

    /// A chunk that exercises a spread of opcode kinds (const push, print call,
    /// arithmetic, return) so the byte-identity check covers more than a trivial op.
    fn output_demo_chunk() -> Chunk {
        let mut c = Chunk::new();
        let name = c.add_const(Value::str("print"));
        c.emit_u16(Op::GetGlobal, name, s());
        let a = c.add_const(Value::float(2.0));
        c.emit_u16(Op::Const, a, s());
        let b = c.add_const(Value::float(3.0));
        c.emit_u16(Op::Const, b, s());
        c.emit(Op::Add, s()); // 2.0 + 3.0
        c.emit_u8(Op::Call, 1, s()); // print(5.0)
        c.emit(Op::Return, s());
        c
    }

    #[test]
    fn noop_instrumentation_is_byte_identical_to_plain_run() {
        // The whole Gate-12 promise at the unit level: an all-None Instrumentation
        // produces byte-identical program output to a plain Vm::new run.
        let (plain_outcome, plain_out) = run_chunk_with_output(output_demo_chunk());
        let (inst_outcome, inst_out) = run_chunk_with_instrument(
            output_demo_chunk(),
            crate::vm::instrument::Instrumentation::empty(),
        );
        assert!(matches!(plain_outcome, Ok(RunOutcome::Done(_))));
        assert!(matches!(inst_outcome, Ok(RunOutcome::Done(_))));
        assert_eq!(
            plain_out, inst_out,
            "an all-None Instrumentation must not perturb program output"
        );
        assert_eq!(inst_out, "5.0\n");
    }

    // ---- DBG Task 2: the Op::Break trap / re-dispatch ---------------------

    /// Run a watchdog-guarded closure. macOS has no `timeout(1)`, and a buggy
    /// `Op::Break` arm BLOCKS forever on the command channel. The VM runtime is `!Send`
    /// (`Rc`/`RefCell`), so the body CANNOT run on a spawned worker thread — it runs on
    /// the calling (test) thread. Instead a separate WATCHDOG timer thread is armed: if
    /// the body does not signal completion within `secs`, the watchdog ABORTS the
    /// process with a loud message, so a regression-induced hang is a visible failure
    /// (a non-zero exit + diagnostic) rather than a silent stall of the host / CI.
    /// On normal completion the watchdog is disarmed (signalled) and joins immediately.
    fn with_watchdog<T>(secs: u64, f: impl FnOnce() -> T) -> T {
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let watchdog = std::thread::spawn(move || {
            if done_rx
                .recv_timeout(std::time::Duration::from_secs(secs))
                .is_err()
            {
                eprintln!(
                    "DBG breakpoint test exceeded {secs}s watchdog — likely an Op::Break \
                     arm hang (blocked on the command channel with no driver). Aborting."
                );
                std::process::abort();
            }
        });
        let r = f();
        let _ = done_tx.send(()); // disarm
        watchdog.join().expect("watchdog thread");
        r
    }

    /// Find the byte offset of the first instruction whose opcode is `target`, by
    /// walking the chunk with the disassembler's offset stepper (so an operand byte
    /// that happens to equal an opcode value is never mistaken for an instruction).
    fn find_op_offset(chunk: &Chunk, target: Op) -> Option<usize> {
        let mut off = 0;
        while off < chunk.code.len() {
            let at = off;
            let op = Op::from_u8(chunk.code[at])?;
            if op == target {
                return Some(at);
            }
            off = at + 1 + op.operand_width();
        }
        None
    }

    /// Run `chunk` with a breakpoint patched at `bp_offset` (a real opcode-byte
    /// offset). A controller thread DRIVES the command channel — it auto-`Continue`s
    /// on every `Stopped` event (the `--inspect`-with-breakpoints-but-auto-continue
    /// observation contract), so the trap arm never blocks indefinitely. Returns the
    /// program output plus the number of times the breakpoint trapped.
    ///
    /// The whole thing runs under a watchdog: a bug that leaves the trap blocked
    /// fails the test instead of hanging the host.
    fn run_with_breakpoint_auto_continue(
        chunk: Chunk,
        bp_offset: usize,
    ) -> (bool, String, usize) {
        use crate::vm::instrument::{DebugCommand, DebuggerHook, Instrumentation};

        with_watchdog(10, move || {
            // Build the proto, capture its identity, then patch ONE byte via the hook
            // while we still uniquely own the Rc (refcount 1, before `Closure::new`).
            let mut proto = Rc::new(FnProto {
                chunk,
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
            name_span: None,
            });
            let proto_id = Rc::as_ptr(&proto) as *const () as usize;

            let (mut hook, cmd_tx, evt_rx) = DebuggerHook::new();
            {
                let p = Rc::get_mut(&mut proto).expect("uniquely own the proto");
                let original = hook.set_breakpoint(proto_id, bp_offset, &mut p.chunk.code);
                assert_ne!(
                    original,
                    crate::vm::opcode::Op::Break as u8,
                    "patched a real opcode byte, not an already-Break byte"
                );
            }

            // The controller thread: auto-continue on each Stopped, counting hits.
            // It ends when the event Sender is dropped (the VM/hook is dropped).
            let controller = std::thread::spawn(move || {
                let mut hits = 0usize;
                while evt_rx.recv().is_ok() {
                    hits += 1;
                    if cmd_tx.send(DebugCommand::Continue).is_err() {
                        break;
                    }
                }
                hits
            });

            let closure = Closure::new(proto);
            let mut fiber = Fiber::new(closure);
            let inst = Instrumentation {
                breakpoints: Some(hook),
                profiler: None,
                coverage: None,
            };

            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("build current-thread runtime");
            let local = LocalSet::new();
            let (outcome, out) = local.block_on(&rt, async move {
                let interp = Rc::new(Interp::new());
                interp.install_self();
                let vm = Vm::with_instrument(interp.clone(), inst);
                let outcome = vm.run(&mut fiber).await;
                (outcome, interp.output())
            });
            let ok = matches!(outcome, Ok(RunOutcome::Done(_)));
            // Dropping the VM dropped the hook's event Sender, so the controller loop
            // ends; join it to read the hit count.
            let hits = controller.join().expect("controller thread");
            (ok, out, hits)
        })
    }

    #[test]
    fn breakpoint_traps_and_program_output_unchanged_after_continue() {
        // Breakpoint on the ADD op: it traps once, the recovered ADD executes, and
        // the program output is byte-identical to the un-instrumented run.
        let plain = run_chunk_with_output(output_demo_chunk()).1;
        let add_off = find_op_offset(&output_demo_chunk(), Op::Add).expect("chunk has an ADD");
        let (ok, out, hits) = run_with_breakpoint_auto_continue(output_demo_chunk(), add_off);
        assert!(ok, "program ran to completion");
        assert_eq!(hits, 1, "the ADD breakpoint trapped exactly once");
        assert_eq!(out, plain, "recovered op ran; output == un-instrumented run");
        assert_eq!(out, "5.0\n");
    }

    #[test]
    fn breakpoint_on_multi_operand_op_redispatches_correctly() {
        // Patch the GET_GLOBAL (a u16-operand op at offset 0): the recovered op must
        // read its 2 operand bytes from fault_ip+1 and advance ip by 2, then the
        // program completes identically.
        let plain = run_chunk_with_output(output_demo_chunk()).1;
        let gg_off = find_op_offset(&output_demo_chunk(), Op::GetGlobal).expect("has GET_GLOBAL");
        assert_eq!(gg_off, 0, "GET_GLOBAL is the first op");
        let (ok, out, hits) = run_with_breakpoint_auto_continue(output_demo_chunk(), gg_off);
        assert!(ok, "program ran to completion");
        assert_eq!(hits, 1);
        assert_eq!(out, plain, "multi-operand recovered op ran; output unchanged");
        assert_eq!(out, "5.0\n");
    }

    #[test]
    fn breakpoint_on_zero_operand_op_redispatches_correctly() {
        // A pure 0-operand op (RETURN) as the breakpoint target.
        fn demo() -> Chunk {
            let mut c = Chunk::new();
            let name = c.add_const(Value::str("print"));
            c.emit_u16(Op::GetGlobal, name, s());
            let k = c.add_const(Value::float(7.0));
            c.emit_u16(Op::Const, k, s());
            c.emit_u8(Op::Call, 1, s());
            c.emit(Op::Return, s());
            c
        }
        let plain = run_chunk_with_output(demo()).1;
        let ret_off = find_op_offset(&demo(), Op::Return).expect("has RETURN");
        let (ok, out, hits) = run_with_breakpoint_auto_continue(demo(), ret_off);
        assert!(ok, "program ran to completion");
        assert_eq!(hits, 1, "RETURN breakpoint trapped once");
        assert_eq!(out, plain);
        assert_eq!(out, "7.0\n");
    }

    #[test]
    fn breakpoint_in_dead_code_never_traps() {
        // A breakpoint on an instruction never reached (after RETURN) must never trap;
        // the program output is unchanged.
        fn demo() -> (Chunk, usize) {
            let mut c = Chunk::new();
            let name = c.add_const(Value::str("print"));
            c.emit_u16(Op::GetGlobal, name, s());
            let k = c.add_const(Value::float(9.0));
            c.emit_u16(Op::Const, k, s());
            c.emit_u8(Op::Call, 1, s());
            c.emit(Op::Return, s());
            // Dead code after the RETURN: a NIL that never executes.
            let dead_off = c.code.len();
            c.emit(Op::Nil, s());
            c.emit(Op::Return, s());
            (c, dead_off)
        }
        let (c, dead_off) = demo();
        let plain = run_chunk_with_output(demo().0).1;
        let (ok, out, hits) = run_with_breakpoint_auto_continue(c, dead_off);
        assert!(ok, "program ran to completion");
        assert_eq!(hits, 0, "a breakpoint in dead/uncalled code never traps");
        assert_eq!(out, plain);
        assert_eq!(out, "9.0\n");
    }

    #[test]
    fn breakpoint_in_loop_traps_once_then_unpatches_v1() {
        // A breakpoint INSIDE a real loop body. Under the v1 un-patch-on-hit model
        // (DOCUMENTED on Op::Break), the breakpoint traps EXACTLY ONCE: the first hit
        // restores the original byte so subsequent iterations run un-patched. Crucially
        // the loop output is byte-identical to an un-instrumented run (the displaced op
        // ran exactly once per visit; iterations after the first run normally).
        fn demo() -> (Chunk, usize) {
            let mut c = Chunk::new();
            c.slot_count = 1; // one local: the counter
            let pname = c.add_const(Value::str("print"));
            let one = c.add_const(Value::float(1.0));

            // Seed slot 0 = 2.0: CONST 2.0; SET_LOCAL 0 (SET_LOCAL POPS the value).
            let two = c.add_const(Value::float(2.0));
            c.emit_u16(Op::Const, two, s());
            c.emit_u16(Op::SetLocal, 0, s());

            let loop_top = c.code.len();
            c.emit_u16(Op::GetLocal, 0, s());
            let exit_jump = c.emit_jump(Op::JumpIfFalse, s()); // 0.0 is falsy → exit
            c.emit_u16(Op::GetGlobal, pname, s());
            let bp = c.code.len(); // BREAKPOINT: GET_LOCAL 0 (the print arg)
            c.emit_u16(Op::GetLocal, 0, s());
            c.emit_u8(Op::Call, 1, s());
            c.emit(Op::Pop, s());
            // counter -= 1
            c.emit_u16(Op::GetLocal, 0, s());
            c.emit_u16(Op::Const, one, s());
            c.emit(Op::Sub, s());
            c.emit_u16(Op::SetLocal, 0, s()); // pops the result into slot 0
            c.emit_loop(Op::Loop, loop_top, s());
            c.patch_jump(exit_jump);
            c.emit(Op::Return, s());
            (c, bp)
        }
        let (c, bp) = demo();
        let plain = run_chunk_with_output(demo().0).1;
        let (ok, out, hits) = run_with_breakpoint_auto_continue(c, bp);
        assert!(ok, "program ran to completion");
        assert_eq!(hits, 1, "v1: a hit breakpoint un-patches itself (traps once)");
        // Counter 2 → prints 2.0 then 1.0; the loop body ran identically under the bp.
        assert_eq!(out, plain, "loop output identical to the un-instrumented run");
        assert_eq!(out, "2.0\n1.0\n");
    }

    #[test]
    fn detached_controller_does_not_deadlock_the_program() {
        // If the controller hangs up (drops the command sender) without sending a
        // resume, the trap must NOT deadlock — `debug_stop` resumes free-running on a
        // disconnected channel. Drive: a controller that drops immediately on the first
        // Stopped (no Continue sent).
        use crate::vm::instrument::{DebuggerHook, Instrumentation};
        let (ok, out) = with_watchdog(10, || {
            let chunk = output_demo_chunk();
            let add_off = find_op_offset(&chunk, Op::Add).expect("has ADD");
            let mut proto = Rc::new(FnProto {
                chunk,
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
            name_span: None,
            });
            let proto_id = Rc::as_ptr(&proto) as *const () as usize;
            let (mut hook, cmd_tx, evt_rx) = DebuggerHook::new();
            {
                let p = Rc::get_mut(&mut proto).unwrap();
                hook.set_breakpoint(proto_id, add_off, &mut p.chunk.code);
            }
            // Controller: receive one Stopped, then DROP both ends (no resume).
            let controller = std::thread::spawn(move || {
                let _ = evt_rx.recv();
                drop(cmd_tx);
                drop(evt_rx);
            });
            let closure = Closure::new(proto);
            let mut fiber = Fiber::new(closure);
            let inst = Instrumentation {
                breakpoints: Some(hook),
                profiler: None,
                coverage: None,
            };
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let local = LocalSet::new();
            let (outcome, out) = local.block_on(&rt, async move {
                let interp = Rc::new(Interp::new());
                interp.install_self();
                let vm = Vm::with_instrument(interp.clone(), inst);
                let outcome = vm.run(&mut fiber).await;
                (outcome, interp.output())
            });
            controller.join().unwrap();
            (matches!(outcome, Ok(RunOutcome::Done(_))), out)
        });
        assert!(ok, "a detached controller must not deadlock the program");
        assert_eq!(out, "5.0\n", "output still correct after the auto-resume");
    }

    // ---- DBG Task 4: the Send-safe frame/variable snapshot ----------------

    #[test]
    fn stopped_event_carries_frame_variable_snapshot() {
        // Compile a real program with a named function holding locals, set a breakpoint
        // INSIDE that function, run, and assert the `Stopped` event ships the Send-safe
        // frame/variable snapshot: innermost frame is `add` with locals `a`/`b` bound,
        // and the bottom frame is `<script>`. Watchdog-guarded so a regression hangs the
        // test (fails loudly) rather than the host.
        use crate::vm::instrument::{
            DebugCommand, DebugEvent, DebuggerHook, FrameSnapshot, Instrumentation,
        };

        let src = "fn add(a, b) {\n  let s = a + b\n  return s\n}\nprint(add(2, 3))\n";
        let frames: Vec<FrameSnapshot> = with_watchdog(10, move || {
            let mut top_chunk = crate::compile::compile_source(src).expect("compiles");

            // The `add` function is the top chunk's single nested proto. Bind the module
            // source onto its chunk so `line_col_at` yields real line numbers, and pick
            // the FIRST executable instruction of its body as the breakpoint site (params
            // `a`/`b` are bound at frame entry, before any body op runs).
            let src_info = Rc::new(crate::error::SourceInfo {
                path: "<test>".into(),
                text: src.into(),
            });
            {
                let add = Rc::get_mut(&mut top_chunk.protos[0])
                    .expect("uniquely own the nested proto");
                add.chunk.set_module_source(&src_info);
                assert_eq!(
                    add.debug_name.as_deref(),
                    Some("add"),
                    "the nested fn proto carries its declared name"
                );
            }
            // First executable op of the body (offset 0 of the add chunk).
            let bp_off = 0usize;
            let proto_id = Rc::as_ptr(&top_chunk.protos[0]) as *const () as usize;

            let (mut hook, cmd_tx, evt_rx) = DebuggerHook::new();
            {
                let add = Rc::get_mut(&mut top_chunk.protos[0]).expect("own nested proto");
                hook.set_breakpoint(proto_id, bp_off, &mut add.chunk.code);
            }

            // Controller: capture the FIRST Stopped event's frames, then auto-continue
            // every stop so the program runs to completion (no deadlock).
            let (snap_tx, snap_rx) = std::sync::mpsc::channel::<Vec<FrameSnapshot>>();
            let controller = std::thread::spawn(move || {
                let mut first = true;
                while let Ok(evt) = evt_rx.recv() {
                    if first {
                        if let DebugEvent::Stopped { frames, .. } = evt {
                            let _ = snap_tx.send(frames);
                            first = false;
                        }
                    }
                    if cmd_tx.send(DebugCommand::Continue).is_err() {
                        break;
                    }
                }
            });

            // Run the TOP-LEVEL chunk: it defines `add` as a module global and calls it,
            // so the breakpoint inside `add` traps when the call runs.
            let top_proto = Rc::new(FnProto {
                chunk: top_chunk,
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
            name_span: None,
            });
            let closure = Closure::new(top_proto);
            let mut fiber = Fiber::new(closure);
            let inst = Instrumentation {
                breakpoints: Some(hook),
                profiler: None,
                coverage: None,
            };
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let local = LocalSet::new();
            let outcome = local.block_on(&rt, async move {
                let interp = Rc::new(Interp::new());
                interp.install_self();
                let vm = Vm::with_instrument(interp.clone(), inst);
                vm.run(&mut fiber).await
            });
            assert!(
                matches!(outcome, Ok(RunOutcome::Done(_))),
                "program ran to completion"
            );
            controller.join().expect("controller thread");
            snap_rx.recv().expect("a Stopped snapshot was captured")
        });

        // Two frames: the innermost is `add`, the bottom is the script.
        assert!(!frames.is_empty(), "snapshot has at least one frame");
        assert_eq!(frames.len(), 2, "add called from the script: two frames");

        let inner = &frames[0];
        assert_eq!(inner.function, "add", "innermost frame is the `add` function");
        let names: Vec<&str> = inner.locals.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"a") && names.contains(&"b"),
            "params a, b are present in the innermost locals: {names:?}"
        );
        // Params are bound to the call args 2 and 3 at frame entry.
        let a = inner.locals.iter().find(|(n, _)| n == "a").map(|(_, v)| v.as_str());
        let b = inner.locals.iter().find(|(n, _)| n == "b").map(|(_, v)| v.as_str());
        assert_eq!(a, Some("2"), "a is bound to the first call arg");
        assert_eq!(b, Some("3"), "b is bound to the second call arg");

        let bottom = frames.last().expect("a bottom frame");
        assert_eq!(bottom.function, "<script>", "the bottom frame is the script");
    }

    // ---- DBG Task 5a: parked breakpoint-management protocol ---------------

    /// DBG Task-5a test scaffold: compile `src`, bind its module source onto the whole
    /// chunk tree, build the entry `FnProto`, and register the proto tree on a freshly
    /// built instrumented `Vm` (so `resolve_line_breakpoint` can see every proto).
    /// Returns the entry proto, the bound `SourceInfo`, and the `Vm`. The caller installs
    /// a `DebuggerHook` and drives the channel from a controller thread.
    fn compile_and_register(
        src: &str,
        vm: &Rc<Vm>,
    ) -> (Rc<FnProto>, Rc<crate::error::SourceInfo>) {
        let top_chunk = crate::compile::compile_source(src).expect("compiles");
        let src_info = Rc::new(crate::error::SourceInfo {
            path: "prog.as".into(),
            text: src.into(),
        });
        // Bind the source onto the entry chunk AND recursively every nested proto, so
        // `build_line_starts`/`first_offset_for_line` work for the whole tree.
        top_chunk.set_module_source(&src_info);
        let entry = Rc::new(FnProto {
            chunk: top_chunk,
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
            name_span: None,
        });
        vm.register_debug_protos(&entry);
        (entry, src_info)
    }

    // ---- DX D2 Task 6: line coverage on the Op::Break trap ----------------

    /// Compile `src`, bind its source, arm coverage over the proto tree, run to
    /// completion capturing output. Returns the reclaimed `CoverageTable` + the program
    /// output. Coverage runs entirely on the VM via the patched `Op::Break` trap.
    fn run_with_coverage(src: &str) -> (crate::vm::instrument::CoverageTable, String) {
        use crate::vm::instrument::{CoverageTable, Instrumentation};
        let top_chunk = crate::compile::compile_source(src).expect("compiles");
        let src_info = Rc::new(crate::error::SourceInfo {
            path: "prog.as".into(),
            text: src.into(),
        });
        top_chunk.set_module_source(&src_info);
        let entry = Rc::new(FnProto {
            chunk: top_chunk,
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
            name_span: None,
        });
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let local = LocalSet::new();
        local.block_on(&rt, async move {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            let vm = Vm::with_instrument(
                interp.clone(),
                Instrumentation {
                    breakpoints: None,
                    profiler: None,
                    coverage: Some(CoverageTable::new()),
                },
            );
            vm.arm_coverage(&entry);
            let closure = Closure::new(entry);
            let mut fiber = Fiber::new(closure);
            let outcome = vm.run(&mut fiber).await;
            assert!(
                matches!(outcome, Ok(RunOutcome::Done(_))),
                "program ran to completion: {outcome:?}"
            );
            let table = vm.take_coverage().expect("coverage was armed");
            (table, interp.output())
        })
    }

    /// Run `src` on a plain (non-instrumented) VM and return its captured output, for
    /// the observation-only equality assertion.
    fn run_plain_output(src: &str) -> String {
        let top_chunk = crate::compile::compile_source(src).expect("compiles");
        let entry = Rc::new(FnProto {
            chunk: top_chunk,
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
            name_span: None,
        });
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let local = LocalSet::new();
        local.block_on(&rt, async move {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            let vm = Vm::new(interp.clone());
            let closure = Closure::new(entry);
            let mut fiber = Fiber::new(closure);
            let outcome = vm.run(&mut fiber).await;
            assert!(matches!(outcome, Ok(RunOutcome::Done(_))));
            interp.output()
        })
    }

    #[test]
    fn coverage_records_executed_lines_and_misses_dead_branch() {
        // Line 1: `let x = 1` (executed)
        // Line 2: `if (x > 5) {` (executed, condition false)
        // Line 3: `  print("big")` (NEVER executed — dead branch)
        // Line 4: `}` (no executable instruction)
        // Line 5: `print("done")` (executed)
        let src = "let x = 1\nif (x > 5) {\n  print(\"big\")\n}\nprint(\"done\")\n";
        let (table, out) = run_with_coverage(src);
        assert_eq!(out, "done\n", "the false branch never prints 'big'");
        let files = table.by_file();
        assert_eq!(files.len(), 1, "one source file");
        let f = &files[0];
        // The never-taken branch (line 3, 1-based) is uncovered; it must appear in the
        // instrumented universe (so we can report it as a MISS) but not be covered.
        assert!(
            f.uncovered_lines().contains(&3),
            "line 3 (dead branch) is uncovered: {:?}",
            f.uncovered_lines()
        );
        // The executed lines are covered (not in the uncovered set).
        assert!(!f.uncovered_lines().contains(&1), "line 1 executed");
        assert!(!f.uncovered_lines().contains(&5), "line 5 executed");
        assert!(f.covered() >= 1 && f.total() > f.covered());
    }

    #[test]
    fn coverage_observation_only_stdout_identical() {
        // A representative program with a loop, a fn call, and a conditional. Its stdout
        // under coverage must be byte-identical to a plain run (the trap re-dispatches the
        // SAME op — observation-only, Gate 1).
        let src = "fn sq(n) {\n  return n * n\n}\nlet total = 0\n\
                   for (i in 1..4) {\n  total = total + sq(i)\n}\nprint(total)\n";
        let (_table, cov_out) = run_with_coverage(src);
        let plain_out = run_plain_output(src);
        assert_eq!(cov_out, plain_out, "coverage stdout == plain stdout");
        assert_eq!(cov_out, "14\n", "1+4+9 = 14");
    }

    #[test]
    fn coverage_covers_a_called_function_body() {
        // The body of `f` (line 2) is reached only via the call on line 4 — coverage must
        // record it (the trap fires inside f's proto, a different proto than the entry).
        let src = "fn f() {\n  return 42\n}\nlet r = f()\nprint(r)\n";
        let (table, out) = run_with_coverage(src);
        assert_eq!(out, "42\n");
        let files = table.by_file();
        assert_eq!(files.len(), 1);
        let f = &files[0];
        // Line 2 (f's body) is covered.
        assert!(!f.uncovered_lines().contains(&2), "f's body line 2 covered");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // DECODE §8.4 — the invalidation battery (the JIT-contract proof).
    //
    // A `DecodedChunk` caches `own_epoch = chunk.patch_epoch` at build time. A DBG
    // breakpoint / coverage trap is installed by `Chunk::patch_byte`-ing an
    // `Op::Break` into `Chunk.code`, which BUMPS `patch_epoch` (the Task-1
    // chokepoint). A previously-built decoded stream is therefore STALE
    // (`own_epoch != patch_epoch`); the Task-4 consult MUST drop it and rebuild
    // from the now-patched bytes (which decode the `Op::Break` as an escalation
    // record) so the trap actually fires. Without the invalidation, the cached
    // records would execute the OLD (un-patched) instruction and the breakpoint
    // would be silently missed — a debugger correctness bug AND the JIT-staleness
    // hazard. These tests prove the consult is sound; the structural chokepoint
    // guard lives in `tests/vm_decode.rs`, the epoch unit tests in `decode.rs`.
    // ─────────────────────────────────────────────────────────────────────────

    /// Build a FORCED-decode VM (`decode_threshold = 0` → the entry chunk decodes
    /// on its first burst) and install `inst` as its instrumentation. The forced
    /// threshold is the in-crate analogue of `vm_run_source_decoded_forced`; the
    /// instrument is layered on AFTER construction (the production constructors
    /// keep their default threshold). Returns the VM.
    fn forced_decode_vm_with_instrument(
        interp: Rc<Interp>,
        inst: crate::vm::instrument::Instrumentation,
    ) -> Rc<Vm> {
        // specialize/sync_lane/call_fast all on; decode + inline + tos on; threshold 0.
        let vm = Vm::with_all_flags(interp, true, true, true, true, true, true, 0);
        *vm.instrument.borrow_mut() = Some(Box::new(inst));
        vm
    }

    #[test]
    fn breakpoint_set_mid_hot_loop_invalidates_the_decoded_stream_and_fires() {
        // §8.4 #1 (THE mandatory test). Warm a loop until its proto decodes
        // (forced threshold 0 → the first run installs the decoded stream and
        // `decoded_ops` rises), THEN patch a breakpoint on a line ALREADY in the
        // decoded stream (epoch bumps → the cached stream is stale), run again on
        // the SAME persisted proto + VM — the trap MUST fire (a stale stream would
        // sail straight past it). Then clear (another epoch bump) + re-run → no
        // phantom trap, output byte-identical to an uninstrumented run, and the
        // stream re-decodes (decoded_ops keeps rising). One program text, run
        // three times on one VM (the REPL-style persistence the consult relies on).
        use crate::vm::instrument::{
            DebugCommand, DebuggerHook, Instrumentation,
        };

        // A loop whose body has a clear, breakpoint-able line (`s = s + i`, line 2).
        let src = "let s = 0\nfor (i in 0..50) { s = s + i }\nprint(s)\n";

        let (ok, hits_run2, hits_run3, decoded_after_warm, decoded_after_run3, out_seq) =
            with_watchdog(15, move || {
                let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
                let local = LocalSet::new();
                local.block_on(&rt, async move {
                    let interp = Rc::new(Interp::new());
                    interp.install_self();

                    // ── WARMUP: forced-decode VM, NO instrument, run once. ───────
                    let warm_vm =
                        Vm::with_all_flags(interp.clone(), true, true, true, true, true, true, 0);
                    let (entry, _src) = compile_and_register(src, &warm_vm);
                    {
                        let mut fiber = Fiber::new(Closure::new(entry.clone()));
                        let outcome = warm_vm.run(&mut fiber).await;
                        assert!(matches!(outcome, Ok(RunOutcome::Done(_))), "warmup ran");
                    }
                    // The cumulative capture buffer is sliced per-run (Interp has no
                    // reset_output); each run appends its own line.
                    let warm_out = interp.output();
                    // The entry chunk decoded (the stream is installed) and records retired.
                    let decoded_after_warm = entry.chunk.decoded.borrow().is_some()
                        && warm_vm.decode_stats_inner().decoded_ops > 0;
                    let warm_epoch = entry.chunk.patch_epoch.get();

                    // ── PATCH: set a breakpoint on the loop body (line 2), which is
                    //    already present in the decoded stream. This bumps the chunk's
                    //    patch_epoch → the cached decoded stream is now STALE.
                    let bp = warm_vm
                        .resolve_line_breakpoint("prog.as", 2)
                        .expect("loop body line 2 resolves to a breakpoint");
                    // The breakpoint must land in the ENTRY chunk (the warmed proto), so
                    // its already-built decoded stream is the one under test.
                    let entry_id = Rc::as_ptr(&entry) as *const () as usize;
                    assert_eq!(bp.0, entry_id, "the loop-body bp binds the warmed entry proto");

                    let (mut hook, cmd_tx, evt_rx) = DebuggerHook::new();
                    hook.set_breakpoint_shared(bp.0, bp.1, &entry.chunk);
                    assert_ne!(
                        warm_epoch,
                        entry.chunk.patch_epoch.get(),
                        "patch_byte bumped the epoch — the cached decoded stream is now stale"
                    );

                    // Auto-continue controller: count Stopped events (the breakpoint hits).
                    let controller = std::thread::spawn(move || {
                        let mut hits = 0usize;
                        while evt_rx.recv().is_ok() {
                            hits += 1;
                            if cmd_tx.send(DebugCommand::Continue).is_err() {
                                break;
                            }
                        }
                        hits
                    });

                    // ── RUN 2: same proto + a debugger hook, on a fresh forced-decode
                    //    VM that SHARES the same already-decoded chunk (the proto carries
                    //    `chunk.decoded`). The stale stream must be DROPPED + rebuilt from
                    //    the patched bytes so the Break record escalates and the trap fires.
                    let run_vm = forced_decode_vm_with_instrument(
                        interp.clone(),
                        Instrumentation {
                            breakpoints: Some(hook),
                            profiler: None,
                            coverage: None,
                        },
                    );
                    run_vm.register_debug_protos(&entry);
                    {
                        let mut fiber = Fiber::new(Closure::new(entry.clone()));
                        let outcome = run_vm.run(&mut fiber).await;
                        assert!(matches!(outcome, Ok(RunOutcome::Done(_))), "run 2 completed");
                    }
                    // Slice this run's contribution off the cumulative buffer.
                    let run2_out = interp.output()[warm_out.len()..].to_string();
                    // Reclaim the hook, drain its breakpoints for the clear step, then
                    // DROP it (dropping the event Sender) so the controller loop ends
                    // before we join it (else `evt_rx.recv()` blocks forever).
                    let mut hook = run_vm.take_debugger_hook().expect("debugger hook armed");
                    // The v1 un-patch-on-hit model already restored the loop byte after the
                    // single trap; drain whatever remains (idempotent for the clear step).
                    let remaining = hook.drain_breakpoints();
                    drop(hook);
                    let hits_run2 = controller.join().expect("controller thread");

                    // ── CLEAR: ensure the original byte is restored (un-patch → another
                    //    epoch bump → stale again) and run a third time with NO live
                    //    breakpoint. No phantom trap; the stream re-decodes (decoded_ops
                    //    keeps rising).
                    for ((pid, off), original) in remaining {
                        assert_eq!(pid, bp.0);
                        entry.chunk.patch_byte(off, original);
                    }
                    // A FRESH forced-decode VM (no instrument) for run 3 — a top-level
                    // `let s` would redeclare on a second run of the SAME Vm's
                    // user_globals, so each run uses its own Vm; the shared proto carries
                    // `chunk.decoded` across all of them (that is the unit under test).
                    let run3_vm =
                        Vm::with_all_flags(interp.clone(), true, true, true, true, true, true, 0);
                    {
                        let mut fiber = Fiber::new(Closure::new(entry.clone()));
                        let outcome = run3_vm.run(&mut fiber).await;
                        assert!(matches!(outcome, Ok(RunOutcome::Done(_))), "run 3 completed: {outcome:?}");
                    }
                    let cumulative = interp.output();
                    let run3_out = cumulative[warm_out.len() + run2_out.len()..].to_string();
                    let decoded_after_run3 = entry.chunk.decoded.borrow().is_some()
                        && run3_vm.decode_stats_inner().decoded_ops > 0;
                    // run 3 must NOT trap (the hook has no live breakpoints; the byte is
                    // restored). hits_run3 measured via a quick byte-comparison: the byte
                    // at the bp offset is the original, not Op::Break.
                    let hits_run3 = if entry.chunk.code[bp.1] == crate::vm::opcode::Op::Break as u8 {
                        1
                    } else {
                        0
                    };

                    let outcome_ok = true;
                    (
                        outcome_ok,
                        hits_run2,
                        hits_run3,
                        decoded_after_warm,
                        decoded_after_run3,
                        (warm_out, run2_out, run3_out),
                    )
                })
            });

        assert!(ok, "all three runs completed");
        let (warm_out, run2_out, run3_out) = out_seq;
        assert!(
            decoded_after_warm,
            "the warmup decoded the entry chunk and retired records (decoded_ops > 0)"
        );
        assert_eq!(
            hits_run2, 1,
            "the breakpoint set AFTER warmup fired — the stale decoded stream was \
             invalidated + rebuilt, not executed (a stale stream would miss the trap)"
        );
        assert_eq!(hits_run3, 0, "after clear, the byte is restored — no phantom Break");
        assert!(
            decoded_after_run3,
            "after the un-patch the stream re-decodes (decoded_ops rises again)"
        );
        // All three runs produce identical, uninstrumented output (0+1+..+49 = 1225).
        assert_eq!(warm_out, "1225\n", "warmup output");
        assert_eq!(run2_out, "1225\n", "the trapped run's output is byte-identical");
        assert_eq!(run3_out, "1225\n", "the cleared run's output is byte-identical");
    }

    #[test]
    fn coverage_over_decoded_execution_is_byte_identical_and_complete() {
        // §8.4 #3: --coverage of a hot-loop program under FORCED decode. arm_coverage
        // patches every line start to Op::Break (each patch bumps patch_epoch) BEFORE
        // the run, so the very first decode reads the PATCHED bytes (Break records that
        // escalate + trap on byte dispatch); after a line traps once it un-patches
        // (another epoch bump) and the now-hot loop re-decodes + runs from records.
        // Assert: program output == a no-decode coverage run, AND the covered line set
        // == the no-decode covered set. (Decode must not change WHICH lines coverage
        // sees nor WHAT the program prints.)
        use crate::vm::instrument::{CoverageTable, Instrumentation};

        let src = "fn sq(n) {\n  return n * n\n}\nlet total = 0\n\
                   for (i in 1..6) {\n  total = total + sq(i)\n}\nprint(total)\n";

        // A by-file covered view: `(path, [(line_1based, covered)])`.
        type CoveredSet = Vec<(String, Vec<(u32, bool)>)>;

        /// Run `src` under coverage with `decode_threshold` (0 = forced decode,
        /// `DECODE_THRESHOLD` = the production warmth) and return
        /// (covered set, output, decoded_ops).
        fn cov_run(src: &str, decode_threshold: u16) -> (CoveredSet, String, u64) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let local = LocalSet::new();
            local.block_on(&rt, async move {
                let interp = Rc::new(Interp::new());
                interp.install_self();
                let mut inst = Instrumentation::empty();
                inst.coverage = Some(CoverageTable::new());
                let vm = Vm::with_all_flags(
                    interp.clone(),
                    true,
                    true,
                    true,
                    true,
                    true,
                    true,
                    decode_threshold,
                );
                *vm.instrument.borrow_mut() = Some(Box::new(inst));
                let (entry, _src) = compile_and_register(src, &vm);
                vm.arm_coverage(&entry);
                let mut fiber = Fiber::new(Closure::new(entry));
                let outcome = vm.run(&mut fiber).await;
                assert!(matches!(outcome, Ok(RunOutcome::Done(_))), "ran: {outcome:?}");
                let table = vm.take_coverage().expect("coverage armed");
                let covered: CoveredSet = table
                    .by_file()
                    .into_iter()
                    .map(|f| (f.path, f.lines))
                    .collect();
                (covered, interp.output(), vm.decode_stats_inner().decoded_ops)
            })
        }

        let (cov_decoded, out_decoded, decoded_ops) = cov_run(src, 0); // forced decode
        let (cov_bytes, out_bytes, _) = cov_run(src, Vm::DECODE_THRESHOLD); // never decodes (short prog)

        // The forced run genuinely decoded + executed records (else this is a vacuous
        // equality of two byte-dispatch runs — anti-false-green, spec §8.3a).
        assert!(
            decoded_ops > 0,
            "the forced-decode coverage run must retire records (it ran {decoded_ops})"
        );
        assert_eq!(
            out_decoded, out_bytes,
            "coverage stdout is byte-identical decode-on vs decode-off"
        );
        assert_eq!(out_decoded, "55\n", "1+4+9+16+25 = 55");
        assert_eq!(
            cov_decoded, cov_bytes,
            "the covered/instrumented line set is identical decode-on vs decode-off"
        );
    }

    #[test]
    fn resolve_line_breakpoint_maps_line_into_a_nested_fn() {
        // A program with a named function. The `let x = 1` line lives INSIDE `f`, so its
        // resolved (proto_id, offset) must point into `f`'s proto, not the entry proto.
        let src = "fn f() {\n  let x = 1\n  return x\n}\nprint(f())\n";
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let local = LocalSet::new();
        local.block_on(&rt, async {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            let vm = Vm::with_instrument(
                interp,
                crate::vm::instrument::Instrumentation::empty(),
            );
            let (entry, _src) = compile_and_register(src, &vm);

            // Line 2 (1-based) is `let x = 1`, inside `f`. The nested fn proto is the
            // entry's single nested proto.
            let f_proto = &entry.chunk.protos[0];
            let f_id = Rc::as_ptr(f_proto) as *const () as usize;

            let resolved = vm
                .resolve_line_breakpoint("prog.as", 2)
                .expect("line 2 resolves to a breakpoint");
            assert_eq!(
                resolved.0, f_id,
                "the `let x` line binds INSIDE f (not the entry proto)"
            );
            // The resolved offset must be a real instruction offset inside f's chunk.
            let f_line_starts = f_proto.chunk.build_line_starts();
            assert!(
                f_line_starts.iter().any(|(_, off)| *off as usize == resolved.1),
                "resolved offset {} is a real line-start offset in f: {:?}",
                resolved.1,
                f_line_starts
            );
        });
    }

    #[test]
    fn parked_set_breakpoints_binds_and_re_stops_at_the_new_breakpoint() {
        // Break-on-entry (offset 0 of the entry chunk). When parked, the controller sends
        // SetBreakpoints for a line INSIDE `f`, expects a BreakpointsVerified{verified}
        // reply with the resolved offset, then Continue — and the program must STOP AGAIN
        // at exactly that resolved offset before completing with correct output.
        use crate::vm::instrument::{
            BreakpointBinding, DebugCommand, DebugEvent, DebuggerHook, Instrumentation,
        };

        let src = "fn f() {\n  let x = 1\n  return x\n}\nprint(f())\n";
        let (ok, out, second_offset, verified) = with_watchdog(10, move || {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let local = LocalSet::new();
            local.block_on(&rt, async move {
                let interp = Rc::new(Interp::new());
                interp.install_self();
                let vm = Vm::with_instrument(
                    interp.clone(),
                    Instrumentation::empty(),
                );
                let (entry, _src) = compile_and_register(src, &vm);

                // The breakpoint line inside f (line 2, 1-based) — pre-resolve the expected
                // offset so the controller can assert the second stop lands there.
                let expected = vm
                    .resolve_line_breakpoint("prog.as", 2)
                    .expect("line 2 resolves");

                // Patch a break-on-entry (offset 0 of the ENTRY chunk) via the hook, while
                // the controller drives set/continue. Install the hook into the VM's
                // instrumentation.
                let (mut hook, cmd_tx, evt_rx) = DebuggerHook::new();
                let entry_id = Rc::as_ptr(&entry) as *const () as usize;
                hook.set_breakpoint_shared(entry_id, 0, &entry.chunk);
                *vm.instrument.borrow_mut() =
                    Some(Box::new(Instrumentation {
                        breakpoints: Some(hook),
                        profiler: None,
                        coverage: None,
                    }));

                // Controller thread: on the FIRST Stopped, send SetBreakpoints{line 2},
                // read the BreakpointsVerified reply, then Continue. Capture the SECOND
                // Stopped's offset (the newly-set breakpoint), then Continue to finish.
                let (out_tx, out_rx) =
                    std::sync::mpsc::channel::<(usize, Vec<BreakpointBinding>)>();
                let controller = std::thread::spawn(move || {
                    let mut stops = 0usize;
                    let mut second_off: Option<usize> = None;
                    let mut verified: Vec<BreakpointBinding> = Vec::new();
                    while let Ok(evt) = evt_rx.recv() {
                        match evt {
                            DebugEvent::Stopped { offset, .. } => {
                                stops += 1;
                                if stops == 1 {
                                    // Set a breakpoint inside f, then continue.
                                    if cmd_tx
                                        .send(DebugCommand::SetBreakpoints {
                                            source: "prog.as".into(),
                                            lines: vec![2],
                                        })
                                        .is_err()
                                    {
                                        break;
                                    }
                                    // The verified reply arrives as the next event.
                                } else {
                                    // Second stop: record the offset, then continue.
                                    second_off = Some(offset);
                                }
                                if stops >= 2 {
                                    let _ = out_tx.send((
                                        second_off.unwrap_or(usize::MAX),
                                        verified.clone(),
                                    ));
                                }
                                let _ = cmd_tx.send(DebugCommand::Continue);
                            }
                            DebugEvent::BreakpointsVerified { results } => {
                                verified = results;
                            }
                            _ => {}
                        }
                    }
                });

                let closure = Closure::new(entry);
                let mut fiber = Fiber::new(closure);
                let outcome = vm.run(&mut fiber).await;
                let ok = matches!(outcome, Ok(RunOutcome::Done(_)));
                let out = interp.output();
                // Drop the VM's hook (drops the event Sender) so the controller ends.
                *vm.instrument.borrow_mut() = None;
                controller.join().expect("controller thread");
                let (second_off, verified) =
                    out_rx.recv().expect("the second stop was reported");
                (ok, out, second_off, (verified, expected))
            })
        });

        let (verified, expected) = verified;
        assert!(ok, "program ran to completion");
        assert_eq!(out, "1\n", "program output is correct and unchanged");
        // The set-breakpoints reply verified the requested line and reported its offset.
        assert_eq!(verified.len(), 1, "one line requested → one binding");
        assert!(verified[0].verified, "the line inside f was bound");
        assert_eq!(verified[0].line, 2);
        assert_eq!(
            verified[0].offset,
            Some(expected.1 as u32),
            "binding reports the resolved offset"
        );
        // The program stopped AGAIN at exactly the newly-set breakpoint offset.
        assert_eq!(
            second_offset, expected.1,
            "the second stop landed at the newly-set breakpoint inside f"
        );
    }

    #[test]
    fn parked_clear_breakpoints_restores_the_original_byte() {
        // Set a breakpoint inside f via SetBreakpoints while parked at entry, then send
        // ClearBreakpoints and assert the patched byte is restored to the original opcode
        // and the program runs free to completion (no second stop).
        use crate::vm::instrument::{
            DebugCommand, DebugEvent, DebuggerHook, Instrumentation,
        };

        let src = "fn f() {\n  let x = 1\n  return x\n}\nprint(f())\n";
        let (ok, out, stops, byte_after_clear, expected_original) =
            with_watchdog(10, move || {
                let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
                let local = LocalSet::new();
                local.block_on(&rt, async move {
                    let interp = Rc::new(Interp::new());
                    interp.install_self();
                    let vm =
                        Vm::with_instrument(interp.clone(), Instrumentation::empty());
                    let (entry, _src) = compile_and_register(src, &vm);

                    // The offset we will set + then clear inside f, and the original byte
                    // there (read BEFORE any patching).
                    let expected = vm
                        .resolve_line_breakpoint("prog.as", 2)
                        .expect("line 2 resolves");
                    let f_proto = vm
                        .debug_proto_for(expected.0)
                        .expect("resolved proto is registered");
                    let original_byte = f_proto.chunk.code[expected.1];

                    let (mut hook, cmd_tx, evt_rx) = DebuggerHook::new();
                    let entry_id = Rc::as_ptr(&entry) as *const () as usize;
                    hook.set_breakpoint_shared(entry_id, 0, &entry.chunk);
                    *vm.instrument.borrow_mut() = Some(Box::new(Instrumentation {
                        breakpoints: Some(hook),
                        profiler: None,
                        coverage: None,
                    }));

                    // Controller: on the first stop, SetBreakpoints{line 2} → read reply →
                    // ClearBreakpoints → Continue. The clear restores the byte BEFORE the
                    // call to f reaches it, so the program must NOT stop a second time.
                    let controller = std::thread::spawn(move || {
                        let mut stops = 0usize;
                        while let Ok(evt) = evt_rx.recv() {
                            match evt {
                                DebugEvent::Stopped { .. } => {
                                    stops += 1;
                                    if stops == 1 {
                                        let _ = cmd_tx.send(DebugCommand::SetBreakpoints {
                                            source: "prog.as".into(),
                                            lines: vec![2],
                                        });
                                        // wait for verified, handled below
                                    } else {
                                        let _ = cmd_tx.send(DebugCommand::Continue);
                                    }
                                }
                                DebugEvent::BreakpointsVerified { .. } => {
                                    // Now clear all breakpoints, then resume.
                                    let _ = cmd_tx.send(DebugCommand::ClearBreakpoints);
                                    let _ = cmd_tx.send(DebugCommand::Continue);
                                }
                                _ => {}
                            }
                        }
                        stops
                    });

                    let closure = Closure::new(entry);
                    let mut fiber = Fiber::new(closure);
                    let outcome = vm.run(&mut fiber).await;
                    let ok = matches!(outcome, Ok(RunOutcome::Done(_)));
                    let out = interp.output();
                    // After the run, the byte at the cleared offset must be the original.
                    let byte_after = f_proto.chunk.code[expected.1];
                    *vm.instrument.borrow_mut() = None;
                    let stops = controller.join().expect("controller thread");
                    (ok, out, stops, byte_after, original_byte)
                })
            });

        assert!(ok, "program ran free to completion");
        assert_eq!(out, "1\n", "program output unchanged");
        assert_eq!(
            stops, 1,
            "only the entry break fired; the cleared breakpoint never trapped"
        );
        assert_eq!(
            byte_after_clear, expected_original,
            "ClearBreakpoints restored the exact original opcode byte"
        );
        assert_ne!(
            byte_after_clear,
            Op::Break as u8,
            "the byte is not left patched after clear"
        );
    }

    #[test]
    fn debug_command_and_event_types_are_send() {
        // Mirror the instrument.rs airlock proof at the run.rs layer too.
        fn _assert_send<T: Send>() {}
        _assert_send::<crate::vm::instrument::DebugCommand>();
        _assert_send::<crate::vm::instrument::DebugEvent>();
        _assert_send::<crate::vm::instrument::BreakpointBinding>();
    }

    #[test]
    fn get_global_undefined_panics() {
        let mut c = Chunk::new();
        let name = c.add_const(Value::str("not_a_builtin"));
        let gg_span = Span::new(3, 16);
        c.emit_u16(Op::GetGlobal, name, gg_span);
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                // The message matches the tree-walker's runtime undefined-name
                // error exactly (`undefined variable '<name>'`), so the two
                // engines stay byte-identical even on this defence-in-depth path.
                assert!(
                    e.message.contains("undefined variable"),
                    "message was: {}",
                    e.message
                );
                assert_eq!(e.span, Some(gg_span));
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn unimplemented_op_panics() {
        // An opcode with no exec arm must surface a span-carrying "not yet
        // implemented" Tier-2 panic. `MAKE_GENERATOR` is never emitted by the
        // compiler (a `fn*` CALL builds the generator directly in the CALL arm,
        // mirroring the tree-walker), so it remains unimplemented — a good probe
        // for the catch-all guard. (JUMP/JUMP_IF_* land in V2-T6, AWAIT in V7,
        // YIELD in V8.)
        let mut c = Chunk::new();
        let op_span = Span::new(2, 4);
        c.emit(Op::Nil, s());
        c.emit(Op::MakeGenerator, op_span);
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                assert!(
                    e.message.contains("not yet implemented"),
                    "message was: {}",
                    e.message
                );
                assert_eq!(e.span, Some(op_span));
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    // ---- await exec arm (V7) ---------------------------------------------

    #[test]
    fn await_non_future_is_identity() {
        // `await 5` is identity on a non-future, exactly like the tree-walker's
        // `ExprKind::Await` (`other => Ok(other)`).
        let mut c = Chunk::new();
        let k = c.add_const(Value::float(5.0));
        c.emit_u16(Op::Const, k, s());
        c.emit(Op::Await, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 5.0);
    }

    // ---- jump exec arms (V2-T6) -------------------------------------------

    #[test]
    fn jump_skips_intervening_code() {
        // NIL is pushed, then an unconditional JUMP hops over a CONST 999, so the
        // result is `nil` (proving the jump landed past the skipped push).
        let mut c = Chunk::new();
        c.emit(Op::Nil, s());
        let site = c.emit_jump(Op::Jump, s());
        let k = c.add_const(Value::float(999.0));
        c.emit_u16(Op::Const, k, s()); // skipped
        c.patch_jump(site); // land here, leaving only NIL
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(v) if matches!(v.kind(), ValueKind::Nil) => {}
            other => panic!("expected Done(Nil), got {other:?}"),
        }
    }

    #[test]
    fn jump_if_false_pops_and_branches_on_falsy() {
        // FALSE on the stack -> JUMP_IF_FALSE pops it and jumps; the CONST 1 in
        // between is skipped, so RETURN sees the trailing CONST 2.
        let mut c = Chunk::new();
        c.emit(Op::False, s());
        let site = c.emit_jump(Op::JumpIfFalse, s());
        let k1 = c.add_const(Value::float(1.0));
        c.emit_u16(Op::Const, k1, s()); // skipped (would otherwise be the result)
        c.patch_jump(site);
        let k2 = c.add_const(Value::float(2.0));
        c.emit_u16(Op::Const, k2, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 2.0);
    }

    #[test]
    fn jump_if_true_pops_and_falls_through_on_falsy() {
        // FALSE -> JUMP_IF_TRUE pops, does NOT jump, falls through to CONST 7.
        let mut c = Chunk::new();
        c.emit(Op::False, s());
        let site = c.emit_jump(Op::JumpIfTrue, s());
        let k7 = c.add_const(Value::float(7.0));
        c.emit_u16(Op::Const, k7, s()); // executed (no jump)
        c.emit(Op::Return, s());
        c.patch_jump(site); // target is past RETURN; never reached
        assert_eq!(expect_number(c), 7.0);
    }

    #[test]
    fn jump_if_not_nil_pops_and_branches_on_non_nil() {
        // CONST 5 (non-nil) -> JUMP_IF_NOT_NIL pops & jumps over CONST 1; RETURN
        // sees the trailing CONST 2.
        let mut c = Chunk::new();
        let k5 = c.add_const(Value::float(5.0));
        c.emit_u16(Op::Const, k5, s());
        let site = c.emit_jump(Op::JumpIfNotNil, s());
        let k1 = c.add_const(Value::float(1.0));
        c.emit_u16(Op::Const, k1, s()); // skipped
        c.patch_jump(site);
        let k2 = c.add_const(Value::float(2.0));
        c.emit_u16(Op::Const, k2, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 2.0);
    }

    // ---- collections: literals + index/member read (V2-T4b) ---------------

    #[test]
    fn new_array_preserves_source_order() {
        // CONST 1; CONST 2; CONST 3; NEW_ARRAY 3 → [1, 2, 3].
        let mut c = Chunk::new();
        for n in [1.0, 2.0, 3.0] {
            let k = c.add_const(Value::float(n));
            c.emit_u16(Op::Const, k, s());
        }
        c.emit_u16(Op::NewArray, 3, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(v) => match v.kind() {
                ValueKind::Array(a) => {
                    let got: Vec<f64> = a
                        .borrow()
                        .iter()
                        .map(|v| match v.kind() {
                            ValueKind::Float(n) => n,
                            other => panic!("non-number: {other:?}"),
                        })
                        .collect();
                    assert_eq!(got, vec![1.0, 2.0, 3.0]);
                }
                other => panic!("expected Done(Array), got {other:?}"),
            },
            other => panic!("expected Done(Array), got {other:?}"),
        }
    }

    #[test]
    fn new_object_builds_indexmap_in_order() {
        // CONST "a"; CONST 1; CONST "b"; CONST 2; NEW_OBJECT 2 → {a:1, b:2}.
        let mut c = Chunk::new();
        for (k, v) in [("a", 1.0), ("b", 2.0)] {
            let ki = c.add_const(Value::str(k));
            c.emit_u16(Op::Const, ki, s());
            let vi = c.add_const(Value::float(v));
            c.emit_u16(Op::Const, vi, s());
        }
        c.emit_u16(Op::NewObject, 2, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(v) => match v.kind() {
                ValueKind::Object(o) => {
                    let keys: Vec<String> = o.keys_snapshot();
                    assert_eq!(keys, vec!["a", "b"], "keys in insertion order");
                    assert_eq!(o.get("a"), Some(Value::float(1.0)));
                    assert_eq!(o.get("b"), Some(Value::float(2.0)));
                }
                other => panic!("expected Done(Object), got {other:?}"),
            },
            other => panic!("expected Done(Object), got {other:?}"),
        }
    }

    /// Like `run_chunk`, but returns the surviving `Rc<FnProto>` alongside the
    /// outcome so a test can inspect the chunk's runtime side tables (the
    /// IC-style `lit_shapes` cache) AFTER the run. The fiber is built fresh from
    /// a clone of the proto so the original `Rc<FnProto>` (and thus the chunk)
    /// outlives the VM. `specialize` toggles the kill switch.
    fn run_chunk_retain(
        chunk: Chunk,
        specialize: bool,
    ) -> (Result<RunOutcome, Control>, Rc<FnProto>) {
        let proto = Rc::new(FnProto {
            chunk,
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
            name_span: None,
        });
        let closure = Closure::new(proto.clone());
        let mut fiber = Fiber::new(closure);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let local = LocalSet::new();
        let outcome = local.block_on(&rt, async move {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            let vm = if specialize {
                Vm::new(interp)
            } else {
                Vm::new_generic(interp)
            };
            vm.run(&mut fiber).await
        });
        (outcome, proto)
    }

    #[test]
    fn new_object_warms_per_site_lit_shape_cache() {
        // SHAPE Task 3.2: after running a `NewObject 2` site the per-site
        // `lit_shapes` entry is Warm with the right shape id and keys.
        let mut c = Chunk::new();
        for (k, v) in [("a", 1.0), ("b", 2.0)] {
            let ki = c.add_const(Value::str(k));
            c.emit_u16(Op::Const, ki, s());
            let vi = c.add_const(Value::float(v));
            c.emit_u16(Op::Const, vi, s());
        }
        let site_off = c.code.len(); // the NewObject op offset
        c.emit_u16(Op::NewObject, 2, s());
        c.emit(Op::Return, s());

        let (outcome, proto) = run_chunk_retain(c, true);
        let obj1 = match outcome.expect("run ok") {
            RunOutcome::Done(v) => match v.into_kind() {
                OwnedKind::Object(o) => o,
                _ => panic!("expected Done(Object)"),
            },
            other => panic!("expected Done(Object), got {other:?}"),
        };
        let entry = proto
            .chunk
            .lit_shape(site_off)
            .expect("lit_shape recorded after a NewObject run");
        match &entry {
            crate::vm::chunk::LitShapeCache::Warm { shape, keys, .. } => {
                assert_eq!(
                    keys.iter().map(|k| k.as_ref()).collect::<Vec<_>>(),
                    vec!["a", "b"],
                    "cached keys in insertion order"
                );
                assert_eq!(*shape, obj1.shape.get(), "cached shape matches the object");
            }
            other => panic!("expected Warm lit_shape, got {other:?}"),
        }

        // Two objects from the SAME shape share their keys Rc and shape id.
        let mut c2 = Chunk::new();
        for _ in 0..2 {
            for (k, v) in [("a", 1.0), ("b", 2.0)] {
                let ki = c2.add_const(Value::str(k));
                c2.emit_u16(Op::Const, ki, s());
                let vi = c2.add_const(Value::float(v));
                c2.emit_u16(Op::Const, vi, s());
            }
            c2.emit_u16(Op::NewObject, 2, s());
        }
        c2.emit_u16(Op::NewArray, 2, s());
        c2.emit(Op::Return, s());
        let (out2, _p2) = run_chunk_retain(c2, true);
        match out2.expect("run ok") {
            RunOutcome::Done(v) => match v.kind() {
                ValueKind::Array(a) => {
                    let a = a.borrow();
                    let (o1, o2) = match (a[0].kind(), a[1].kind()) {
                        (ValueKind::Object(o1), ValueKind::Object(o2)) => {
                            (o1.clone(), o2.clone())
                        }
                        other => panic!("expected two objects, got {other:?}"),
                    };
                    assert_eq!(o1.shape.get(), o2.shape.get(), "same shape id");
                    assert!(
                        Rc::ptr_eq(&o1.slab_keys().unwrap(), &o2.slab_keys().unwrap()),
                        "two objects from the same shape share their keys Rc"
                    );
                }
                other => panic!("expected Done(Array), got {other:?}"),
            },
            other => panic!("expected Done(Array), got {other:?}"),
        }
    }

    #[test]
    fn new_object_duplicate_key_site_records_slot_of_pair() {
        // SHAPE Task 3.2: a duplicate-key site `{a: 1, a: 2}` (constants
        // ["a","a"]) folds to {a: 2} (later source position wins) and records a
        // `slot_of_pair = Some([0, 0])` (both source pairs map to slot 0).
        let mut c = Chunk::new();
        for v in [1.0, 2.0] {
            let ki = c.add_const(Value::str("a"));
            c.emit_u16(Op::Const, ki, s());
            let vi = c.add_const(Value::float(v));
            c.emit_u16(Op::Const, vi, s());
        }
        let site_off = c.code.len();
        c.emit_u16(Op::NewObject, 2, s());
        c.emit(Op::Return, s());
        let (outcome, proto) = run_chunk_retain(c, true);
        match outcome.expect("run ok") {
            RunOutcome::Done(v) => match v.kind() {
                ValueKind::Object(o) => {
                    assert_eq!(o.keys_snapshot(), vec!["a"], "duplicate key folded");
                    assert_eq!(
                        o.get("a"),
                        Some(Value::float(2.0)),
                        "later source position wins"
                    );
                }
                other => panic!("expected Done(Object), got {other:?}"),
            },
            other => panic!("expected Done(Object), got {other:?}"),
        }
        match proto.chunk.lit_shape(site_off).expect("warm") {
            crate::vm::chunk::LitShapeCache::Warm { slot_of_pair, .. } => {
                let sop = slot_of_pair.expect("duplicate site has an explicit slot_of_pair");
                assert_eq!(
                    sop.iter().copied().collect::<Vec<u16>>(),
                    vec![0u16, 0u16],
                    "both source pairs map to slot 0"
                );
            }
            other => panic!("expected Warm, got {other:?}"),
        }
    }

    #[test]
    fn get_index_array() {
        // [10, 20, 30]; CONST 1; GET_INDEX → 20.
        let mut c = Chunk::new();
        for n in [10.0, 20.0, 30.0] {
            let k = c.add_const(Value::float(n));
            c.emit_u16(Op::Const, k, s());
        }
        c.emit_u16(Op::NewArray, 3, s());
        let i = c.add_const(Value::float(1.0));
        c.emit_u16(Op::Const, i, s());
        c.emit(Op::GetIndex, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 20.0);
    }

    #[test]
    fn get_index_out_of_bounds_panics() {
        let mut c = Chunk::new();
        let k = c.add_const(Value::float(10.0));
        c.emit_u16(Op::Const, k, s());
        c.emit_u16(Op::NewArray, 1, s());
        let i = c.add_const(Value::float(5.0));
        c.emit_u16(Op::Const, i, s());
        c.emit(Op::GetIndex, s());
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                assert!(e.message.contains("out of bounds"), "msg: {}", e.message)
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn variant_elem_oob_operand_is_nil_not_panic() {
        // Task 0.7 robustness: a `VARIANT_ELEM` whose operand is past the end of the
        // variant's positional payload reads as `Value::nil()` (via `.get(idx)`), NOT a
        // host panic. We build a real 2-field positional variant as a const and index
        // element 0xFFFF — verify(`variant_elem_max_operand_verifies`) already accepts
        // the operand; here we prove the run loop is independently OOB-safe so the
        // verifier need not (and soundly cannot) cap the bare index below `u16::MAX`.
        use crate::value::{ArrayCell, EnumVariant, Payload};
        let variant = Value::enum_variant(Rc::new(EnumVariant {
            enum_name: "E".to_string(),
            name: "Pair".to_string(),
            value: Value::nil(),
            payload: Some(Payload::Positional(ArrayCell::new(vec![
                Value::float(1.0),
                Value::float(2.0),
            ]))),
            ctor: false,
            def: None,
        }));
        let mut c = Chunk::new();
        let k = c.add_const(variant);
        c.emit_u16(Op::Const, k, s()); // push the variant (depth 1)
        c.emit_u16(Op::VariantElem, 0xFFFF, s()); // OOB index → Nil (net 0)
        c.emit(Op::Return, s());
        match run_chunk(c).expect("must not panic on an out-of-range VARIANT_ELEM index") {
            RunOutcome::Done(v) if matches!(v.kind(), ValueKind::Nil) => {}
            other => panic!("expected Done(Nil) for an OOB VARIANT_ELEM, got {other:?}"),
        }
    }

    #[test]
    fn match_variant_arity_oob_operand_is_false_not_panic() {
        // Companion: `MATCH_VARIANT_ARITY(0xFFFF)` on a 2-field variant is a false
        // match (`len == Some(n)`), never an index panic.
        use crate::value::{ArrayCell, EnumVariant, Payload};
        let variant = Value::enum_variant(Rc::new(EnumVariant {
            enum_name: "E".to_string(),
            name: "Pair".to_string(),
            value: Value::nil(),
            payload: Some(Payload::Positional(ArrayCell::new(vec![
                Value::float(1.0),
                Value::float(2.0),
            ]))),
            ctor: false,
            def: None,
        }));
        let mut c = Chunk::new();
        let k = c.add_const(variant);
        c.emit_u16(Op::Const, k, s());
        c.emit_u16(Op::MatchVariantArity, 0xFFFF, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("must not panic on an out-of-range MATCH_VARIANT_ARITY count") {
            RunOutcome::Done(v) if matches!(v.kind(), ValueKind::Bool(false)) => {}
            other => panic!("expected Done(Bool(false)) for an OOB arity, got {other:?}"),
        }
    }

    #[test]
    fn get_index_object_missing_key_is_nil() {
        // {a:1}["b"] → nil (missing object key is nil, not a panic).
        let mut c = Chunk::new();
        let ka = c.add_const(Value::str("a"));
        c.emit_u16(Op::Const, ka, s());
        let v1 = c.add_const(Value::float(1.0));
        c.emit_u16(Op::Const, v1, s());
        c.emit_u16(Op::NewObject, 1, s());
        let kb = c.add_const(Value::str("b"));
        c.emit_u16(Op::Const, kb, s());
        c.emit(Op::GetIndex, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(v) if matches!(v.kind(), ValueKind::Nil) => {}
            other => panic!("expected Done(Nil), got {other:?}"),
        }
    }

    #[test]
    fn get_prop_object_field() {
        // {a:1}.a → 1 via GET_PROP "a".
        let mut c = Chunk::new();
        let ka = c.add_const(Value::str("a"));
        c.emit_u16(Op::Const, ka, s());
        let v1 = c.add_const(Value::float(1.0));
        c.emit_u16(Op::Const, v1, s());
        c.emit_u16(Op::NewObject, 1, s());
        let name = c.add_const(Value::str("a"));
        c.emit_u16(Op::GetProp, name, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 1.0);
    }

    #[test]
    fn get_prop_opt_nil_receiver_is_nil() {
        // nil?.a → nil (short-circuit, no read_member call).
        let mut c = Chunk::new();
        c.emit(Op::Nil, s());
        let name = c.add_const(Value::str("a"));
        c.emit_u16(Op::GetPropOpt, name, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(v) if matches!(v.kind(), ValueKind::Nil) => {}
            other => panic!("expected Done(Nil), got {other:?}"),
        }
    }

    #[test]
    fn get_prop_nil_receiver_panics() {
        // nil.a → "cannot read property 'a' of nil" (NOT short-circuited).
        let mut c = Chunk::new();
        c.emit(Op::Nil, s());
        let name = c.add_const(Value::str("a"));
        c.emit_u16(Op::GetProp, name, s());
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => assert!(
                e.message.contains("cannot read property 'a' of nil"),
                "msg: {}",
                e.message
            ),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    // ---- Vm::call_value bridge (native → VM closures), V4-T5 ---------------

    /// Compile a program whose trailing expression evaluates to a closure, run it
    /// on the VM, and return that `Value::closure`. This is how a native
    /// higher-order function would *receive* a user callback (e.g. the `f` arg of
    /// `array.map`). The closure is self-contained (proto + captured upvalue
    /// cells), so a fresh VM can later drive it via `Vm::call_value`.
    fn compile_closure(src: &str) -> Value {
        let chunk = crate::compile::compile_source(src).expect("compile ok");
        match run_chunk(chunk).expect("run ok") {
            RunOutcome::Done(v) if matches!(v.kind(), ValueKind::Closure(_)) => v,
            other => panic!("expected the program to yield a closure, got {other:?}"),
        }
    }

    /// Run `body(vm)` on a current-thread runtime inside a `LocalSet` with a fresh
    /// `Vm` over a fresh `Interp`, mirroring the production entry points. Returns
    /// whatever the async body returns.
    fn with_vm<F, Fut, T>(body: F) -> T
    where
        F: FnOnce(Rc<Vm>) -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let local = LocalSet::new();
        local.block_on(&rt, async move {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            let vm = Vm::new(interp);
            body(vm).await
        })
    }

    #[test]
    fn call_value_runs_a_vm_closure_with_native_supplied_args() {
        // The exact `array.map` shape: a native caller hands the closure ONE arg
        // per element. `(x) => x * 2` called with 21 → 42.
        let f = compile_closure("(x) => x * 2");
        let got = with_vm(|vm| async move {
            vm.call_value(f, vec![Value::float(21.0)], s())
                .await
                .expect("call ok")
        });
        assert!(matches!(got.kind(), ValueKind::Float(n) if n == 42.0), "got {got:?}");
    }

    #[test]
    fn call_value_invokes_a_closure_repeatedly_each_on_its_own_fiber() {
        // A native HOF calls the SAME closure once per element; each invocation is
        // an independent Fiber, so there is no cross-call state leakage.
        let f = compile_closure("(x) => x + 1");
        let got = with_vm(|vm| async move {
            let mut out = Vec::new();
            for n in [10.0, 20.0, 30.0] {
                let v = vm
                    .call_value(f.clone(), vec![Value::float(n)], s())
                    .await
                    .expect("call ok");
                out.push(v);
            }
            out
        });
        let nums: Vec<f64> = got
            .iter()
            .map(|v| match v.kind() {
                ValueKind::Float(n) => n,
                other => panic!("non-number: {other:?}"),
            })
            .collect();
        assert_eq!(nums, vec![11.0, 21.0, 31.0]);
    }

    #[test]
    fn call_value_closure_observes_its_captured_upvalue() {
        // A closure capturing an outer FUNCTION-LOCAL `k` and applied to a
        // native-supplied arg — exactly `array.map([..], (x) => x + k)` inside a fn.
        // The captured cell travels WITH the closure value (it is a genuine upvalue,
        // not a module global), so a fresh VM driving it still sees k = 10. (A
        // top-level `let k` would instead be a module global read via GET_GLOBAL.)
        let f = compile_closure("fn make() {\n let k = 10\n return (x) => x + k\n}\nmake()");
        let got = with_vm(|vm| async move {
            vm.call_value(f, vec![Value::float(5.0)], s())
                .await
                .expect("call ok")
        });
        assert!(matches!(got.kind(), ValueKind::Float(n) if n == 15.0), "got {got:?}");
    }

    // ---- V7-T4: structured-concurrency over VM-produced futures -----------
    //
    // The std/task ops (`gather`/`race`/`timeout`/`spawn`) are native fns on the
    // shared `Interp` that await/select over `Value::future`s. The VM produces
    // ordinary `Value::future`s (the SAME `SharedFuture` the tree-walker uses;
    // see the `Op::Call` async-fn arm). These tests de-risk the V12 end-to-end
    // structured-concurrency differential (`concurrency.as` /
    // `structured_concurrency.as`, which need `import` — not compiled until V12)
    // by exercising a task op DIRECTLY over a VM-produced future, with no
    // `import`. They prove the bridge is sound today: `task.gather` over two VM
    // async-fn futures awaits both and preserves order.

    /// Spawn a VM async-fn call exactly the way the `Op::Call` async arm does:
    /// `spawn_local` a task that drives `Vm::call_value(closure, args)` and
    /// resolves a `SharedFuture` cell, returning the `Value::future` handle
    /// immediately. This is the canonical "VM-produced future".
    fn spawn_vm_future(vm: &Rc<Vm>, closure: Value, args: Vec<Value>) -> Value {
        let vm2 = vm.rc();
        let fut = crate::task::SharedFuture::new();
        let cell = fut.cell();
        let handle = tokio::task::spawn_local(async move {
            let r = vm2.call_value(closure, args, s()).await;
            cell.resolve(r);
        });
        fut.set_abort(handle.abort_handle());
        Value::future(fut)
    }

    /// Compile + run a whole `.as` program `src` on a fresh Vm (mirroring the
    /// `vm_run_source` entry point) and return the shared `Interp`'s in-flight
    /// high-water mark — used to prove un-awaited async tasks are reaped (bounded),
    /// not leaked (the M17 memory-leak guard, on the VM).
    fn run_program_max_inflight(src: &str) -> u64 {
        let chunk = crate::compile::compile_source(src).expect("compile ok");
        let proto = Rc::new(FnProto {
            chunk,
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
            name_span: None,
        });
        let closure = Closure::new(proto);
        let mut fiber = Fiber::new(closure);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let local = LocalSet::new();
        let interp = Rc::new(Interp::new());
        interp.install_self();
        let vm = Vm::new(interp.clone());
        local.block_on(&rt, async {
            local.run_until(vm.run(&mut fiber)).await.expect("run ok");
        });
        interp.max_inflight()
    }

    #[test]
    fn unawaited_async_loop_keeps_inflight_bounded_on_the_vm() {
        // M17 leak guard, on the VM: a tight loop spawning async calls WITHOUT
        // awaiting them must stay bounded. Each un-awaited future is dropped → its
        // task is cancelled; the cooperative yield above `INFLIGHT_YIELD_CAP`
        // (256) reaps finished/cancelled tasks so the in-flight high-water mark
        // stays well below the iteration count. Without reaping a 5000-iteration
        // loop would peak near 5000. Mirrors the interp's
        // `unawaited_async_loop_keeps_inflight_bounded`.
        let src = "async fn work(n) { return n }\n\
                   let i = 0\n\
                   while (i < 5000) {\n  work(i)\n  i = i + 1\n}\n\
                   print(\"done\")\n";
        let peak = run_program_max_inflight(src);
        assert!(
            peak < 1000,
            "in-flight high-water mark should stay bounded (got {peak})"
        );
    }

    #[test]
    fn task_gather_awaits_vm_produced_futures_in_order() {
        // `(n) => n + 1` invoked as two independent VM futures, gathered. The
        // native `task.gather` op awaits each `Value::future` and returns the
        // values in input order — proving the VM's futures interoperate with the
        // structured-concurrency machinery (Part C de-risk; full e2e is V12).
        let f = compile_closure("(n) => n + 1");
        let out = with_vm(|vm| async move {
            let a = spawn_vm_future(&vm, f.clone(), vec![Value::float(10.0)]);
            let b = spawn_vm_future(&vm, f, vec![Value::float(20.0)]);
            let arr = Value::array(vec![a, b]);
            vm.interp()
                .call_task("gather", &[arr], s())
                .await
                .expect("gather ok")
        });
        match out.kind() {
            ValueKind::Array(a) => {
                let got: Vec<f64> = a
                    .borrow()
                    .iter()
                    .map(|v| match v.kind() {
                        ValueKind::Float(n) => n,
                        other => panic!("non-number in gather result: {other:?}"),
                    })
                    .collect();
                assert_eq!(
                    got,
                    vec![11.0, 21.0],
                    "gather preserves order over VM futures"
                );
            }
            other => panic!("gather should return an array, got {other:?}"),
        }
    }

    #[test]
    fn task_race_resolves_a_vm_produced_future() {
        // A single VM-produced future raced resolves to its value — `task.race`
        // selects over `Value::future`s and the VM's future drives to completion.
        let f = compile_closure("(n) => n * 2");
        let out = with_vm(|vm| async move {
            let a = spawn_vm_future(&vm, f, vec![Value::float(21.0)]);
            let arr = Value::array(vec![a]);
            vm.interp()
                .call_task("race", &[arr], s())
                .await
                .expect("race ok")
        });
        assert!(matches!(out.kind(), ValueKind::Float(n) if n == 42.0), "got {out:?}");
    }

    #[test]
    fn call_value_propagates_a_closure_panic() {
        // A native HOF whose callback panics must see the SAME `Control::Panic`
        // surface out of `call_value` (so e.g. `array.map` aborts identically).
        // `(x) => x[9]` indexes a 1-element array out of bounds at runtime.
        let f = compile_closure("(x) => x[9]");
        let err = with_vm(|vm| async move {
            let arr = Value::array(vec![Value::float(0.0)]);
            vm.call_value(f, vec![arr], s())
                .await
                .expect_err("expected a panic")
        });
        match err {
            Control::Panic(e) => assert!(e.message.contains("out of bounds"), "msg: {}", e.message),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn call_value_arity_mismatch_panics_like_the_tree_walker() {
        // Calling a 1-param closure with 0 args from native code surfaces the
        // shared `check_call_args` arity panic (same wording as the tree-walker).
        let f = compile_closure("(x) => x");
        let err = with_vm(|vm| async move {
            vm.call_value(f, Vec::new(), s())
                .await
                .expect_err("expected an arity panic")
        });
        match err {
            Control::Panic(e) => assert!(
                e.message.contains("expected 1 argument(s), got 0"),
                "msg: {}",
                e.message
            ),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn call_value_delegates_native_callees_to_the_interp() {
        // A non-closure callee (here the `print` builtin) routes to the shared
        // `Interp::call_value`, exactly like the `Op::Call` non-Closure arm.
        let out = with_vm(|vm| async move {
            let r = vm
                .call_value(
                    Value::builtin("print"),
                    vec![Value::float(7.0)],
                    s(),
                )
                .await
                .expect("call ok");
            // print returns nil and writes to the shared sink.
            assert!(matches!(r.kind(), ValueKind::Nil), "print returns nil");
            vm.interp().output()
        });
        assert_eq!(out, "7.0\n", "print wrote through the delegated path");
    }

    #[test]
    fn jump_if_not_nil_falls_through_on_nil() {
        // NIL -> JUMP_IF_NOT_NIL pops, does NOT jump, falls through to CONST 9.
        let mut c = Chunk::new();
        c.emit(Op::Nil, s());
        let site = c.emit_jump(Op::JumpIfNotNil, s());
        let k9 = c.add_const(Value::float(9.0));
        c.emit_u16(Op::Const, k9, s()); // executed (no jump)
        c.emit(Op::Return, s());
        c.patch_jump(site); // never reached
        assert_eq!(expect_number(c), 9.0);
    }

    // ---- PROPAGATE (? operator) at the bytecode level (V6-T1) -------------

    /// A success pair `[7, nil]` through PROPAGATE leaves `7` on the stack
    /// (the `?` expression's result), so the surrounding RETURN yields `7`.
    #[test]
    fn propagate_success_yields_value() {
        let mut c = Chunk::new();
        let pair = c.add_const(crate::interp::make_pair(Value::float(7.0), Value::nil()));
        c.emit_u16(Op::Const, pair, s());
        c.emit(Op::Propagate, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 7.0);
    }

    /// A failure pair `[nil, "boom"]` through PROPAGATE early-returns the
    /// `[nil, err]` pair from the (root) frame — the trailing CONST 999 / RETURN
    /// never run, so the program result is the propagated pair.
    #[test]
    fn propagate_failure_early_returns_pair_from_frame() {
        let mut c = Chunk::new();
        let pair = c.add_const(crate::interp::make_pair(
            Value::nil(),
            Value::str("boom"),
        ));
        c.emit_u16(Op::Const, pair, s());
        c.emit(Op::Propagate, s());
        // Never reached: PROPAGATE early-returned from the root frame.
        let k999 = c.add_const(Value::float(999.0));
        c.emit_u16(Op::Const, k999, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(v) => match v.kind() {
                ValueKind::Array(a) => {
                    let b = a.borrow();
                    assert_eq!(b.len(), 2);
                    assert_eq!(b[0], Value::nil());
                    assert_eq!(b[1], Value::str("boom"));
                }
                other => panic!("expected Done([nil, \"boom\"]), got {other:?}"),
            },
            other => panic!("expected Done([nil, \"boom\"]), got {other:?}"),
        }
    }

    /// Compile + run `src` on the VM and return the top-level program's value.
    /// (Mirrors the production `vm_eval_source` path; used by the shape tests to
    /// inspect the `shape_id` the VM assigned to the returned object/instance.)
    fn eval_src(src: &str) -> Value {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        rt.block_on(async { crate::vm_eval_source(src).await.expect("vm eval ok") })
    }

    fn obj_shape(v: &Value) -> u32 {
        match v.kind() {
            ValueKind::Object(o) => o.shape.get(),
            other => panic!("expected an Object, got {other:?}"),
        }
    }

    // V11-T2: the VM assigns each object literal a hidden-class shape; two literals
    // with the SAME ordered keys converge on the SAME id, a different key set
    // differs, and key ORDER matters. We bundle them in one array so a single VM
    // (hence one ShapeRegistry) assigns all the ids.
    #[test]
    fn vm_object_literals_share_shape_by_layout() {
        // [{a,b}, {a,b}, {a,c}, {b,a}]
        let v = eval_src("[{a: 1, b: 2}, {a: 9, b: 8}, {a: 1, c: 2}, {b: 1, a: 2}]");
        let arr = match v.kind() {
            ValueKind::Array(a) => a.borrow().clone(),
            other => panic!("expected array, got {other:?}"),
        };
        let s_ab1 = obj_shape(&arr[0]);
        let s_ab2 = obj_shape(&arr[1]);
        let s_ac = obj_shape(&arr[2]);
        let s_ba = obj_shape(&arr[3]);
        assert_eq!(s_ab1, s_ab2, "same ordered keys → same shape");
        assert_ne!(s_ab1, s_ac, "different key set → different shape");
        assert_ne!(s_ab1, s_ba, "different key ORDER → different shape");
        assert_ne!(s_ab1, 0, "a non-empty object is not the empty shape");
    }

    #[test]
    fn vm_empty_object_literal_is_shape_zero() {
        // `{}` at statement position parses as a block, so bind it first.
        let v = eval_src("let o = {}\no");
        assert_eq!(obj_shape(&v), 0);
    }

    // Adding a NEW key via `o.newkey = v` transitions the shape; REASSIGNING an
    // existing key keeps it (V11-T3's inline-cache validity relies on this). One
    // VM (one registry) builds all three objects so the ids are comparable.
    #[test]
    fn vm_adding_key_transitions_shape_reassign_keeps_it() {
        // Build {a}, then a mutated copy where `a` is reassigned, then one where a
        // NEW key `b` is added — return all three to compare their live shapes.
        let v = eval_src(
            "let base = {a: 1}\n\
             let reassigned = {a: 1}\n\
             reassigned.a = 5\n\
             let added = {a: 1}\n\
             added.b = 9;\n\
             [base, reassigned, added]",
        );
        let arr = match v.kind() {
            ValueKind::Array(a) => a.borrow().clone(),
            other => panic!("expected array, got {other:?}"),
        };
        let s_base = obj_shape(&arr[0]);
        let s_reassigned = obj_shape(&arr[1]);
        let s_added = obj_shape(&arr[2]);
        assert_eq!(
            s_base, s_reassigned,
            "reassigning an existing key keeps the shape"
        );
        assert_ne!(s_base, s_added, "adding a new key transitions the shape");
        assert_ne!(s_added, 0);
    }

    // A class gives its instances a stable BASE shape (declared-field layout).
    #[test]
    fn vm_instance_has_class_base_shape() {
        let v = eval_src(
            "class P { x: number = 0\n y: number = 0\n }\n\
             [P(), P()]",
        );
        let arr = match v.kind() {
            ValueKind::Array(a) => a.borrow().clone(),
            other => panic!("expected array, got {other:?}"),
        };
        let s0 = match arr[0].kind() {
            ValueKind::Instance(i) => i.borrow().shape_id.get(),
            other => panic!("expected instance, got {other:?}"),
        };
        let s1 = match arr[1].kind() {
            ValueKind::Instance(i) => i.borrow().shape_id.get(),
            other => panic!("expected instance, got {other:?}"),
        };
        assert_eq!(s0, s1, "two instances of one class share the base shape");
        assert_ne!(s0, 0, "a class with declared fields has a non-empty shape");
    }

    // ---- V11-T4 adaptive specialization -----------------------------------

    use crate::vm::adapt::{ArithCache, ArithKind, GlobalCache, WARMUP_THRESHOLD};
    use rust_decimal::Decimal;

    /// Build a `(Vm, Fiber)` over a single-op chunk whose op at offset 0 is `op`
    /// (with a real span), so a test can repeatedly call `eval_binop_adaptive` at
    /// `fault_ip = 0` and read back `chunk.arith_cache(0)` to watch specialization.
    fn adaptive_harness(op: Op) -> (Rc<Vm>, Fiber) {
        let mut c = Chunk::new();
        c.emit(op, Span::new(0, 3));
        c.emit(Op::Return, s());
        let proto = Rc::new(FnProto {
            chunk: c,
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
            name_span: None,
        });
        let closure = Closure::new(proto);
        let fiber = Fiber::new(closure);
        let interp = Rc::new(Interp::new());
        interp.install_self();
        let vm = Vm::new(interp);
        (vm, fiber)
    }

    /// Like [`adaptive_harness`] but builds a NON-specializing VM (the
    /// `--no-specialize` kill switch). Used to prove the fast paths never run.
    fn generic_adaptive_harness(op: Op) -> (Rc<Vm>, Fiber) {
        let mut c = Chunk::new();
        c.emit(op, Span::new(0, 3));
        c.emit(Op::Return, s());
        let proto = Rc::new(FnProto {
            chunk: c,
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
            name_span: None,
        });
        let closure = Closure::new(proto);
        let fiber = Fiber::new(closure);
        let interp = Rc::new(Interp::new());
        interp.install_self();
        let vm = Vm::new_generic(interp);
        (vm, fiber)
    }

    // ---- V11-T5 KILL SWITCH (--no-specialize) -----------------------------

    #[test]
    fn kill_switch_never_specializes_arithmetic_and_stays_correct() {
        // With specialization OFF, driving FAR past the warmup threshold must leave
        // the arith cache COLD (never warmed, never specialized) — yet every result
        // is byte-identical to the specializing path's result.
        let (vm, fiber) = generic_adaptive_harness(Op::Add);
        for i in 0..(WARMUP_THRESHOLD + 50) {
            let v = vm
                .eval_binop_adaptive(
                    &fiber,
                    0,
                    BinOp::Add,
                    Value::float(i as f64),
                    Value::float(1.0),
                )
                .expect("ok");
            assert_eq!(v, Value::float(i as f64 + 1.0));
        }
        // The cache MUST still be at its default cold state — the generic path
        // never observes (no warmup candidate, count 0) and never specializes.
        assert_eq!(
            fiber.frame().closure.proto.chunk.arith_cache(0),
            ArithCache::default(),
            "kill switch must leave the arith cache cold (no warmup/specialize)"
        );
    }

    #[test]
    fn kill_switch_default_constructor_specializes() {
        // The DEFAULT `Vm::new` specializes; only `new_generic` disables it. This
        // pins the default so a future refactor cannot silently flip the switch.
        let (vm, fiber) = adaptive_harness(Op::Add);
        for _ in 0..WARMUP_THRESHOLD {
            vm.eval_binop_adaptive(
                &fiber,
                0,
                BinOp::Add,
                Value::float(1.0),
                Value::float(1.0),
            )
            .expect("ok");
        }
        assert!(
            fiber
                .frame()
                .closure
                .proto
                .chunk
                .arith_cache(0)
                .specialized()
                .is_some(),
            "default Vm::new must specialize a hot monomorphic site"
        );
    }

    #[test]
    fn add_warms_up_then_specializes_to_number() {
        let (vm, fiber) = adaptive_harness(Op::Add);
        // Drive N number adds at offset 0; each returns the correct sum and the
        // last one flips the side-map cache to Specialized(Number).
        for i in 0..WARMUP_THRESHOLD {
            let v = vm
                .eval_binop_adaptive(
                    &fiber,
                    0,
                    BinOp::Add,
                    Value::float(i as f64),
                    Value::float(1.0),
                )
                .expect("ok");
            assert_eq!(v, Value::float(i as f64 + 1.0));
        }
        let cache = fiber.frame().closure.proto.chunk.arith_cache(0);
        assert_eq!(
            cache,
            ArithCache::Specialized {
                kind: ArithKind::Number
            }
        );
        // A subsequent number add still takes the (now specialized) fast path with
        // the byte-identical result.
        let v = vm
            .eval_binop_adaptive(
                &fiber,
                0,
                BinOp::Add,
                Value::float(40.0),
                Value::float(2.0),
            )
            .expect("ok");
        assert_eq!(v, Value::float(42.0));
    }

    #[test]
    fn specialized_number_add_deopts_on_string_operand_and_stays_correct() {
        let (vm, fiber) = adaptive_harness(Op::Add);
        for _ in 0..WARMUP_THRESHOLD {
            vm.eval_binop_adaptive(
                &fiber,
                0,
                BinOp::Add,
                Value::float(1.0),
                Value::float(1.0),
            )
            .expect("ok");
        }
        assert!(fiber
            .frame()
            .closure
            .proto
            .chunk
            .arith_cache(0)
            .specialized()
            .is_some());
        // Now feed two strings: the Number guard misses → deopt → generic concat.
        let v = vm
            .eval_binop_adaptive(
                &fiber,
                0,
                BinOp::Add,
                Value::str("a"),
                Value::str("b"),
            )
            .expect("ok");
        assert_eq!(v, Value::str("ab"), "generic path gave the concat");
        // The site deoptimized back to a fresh warmup (the deopt branch reverts and
        // runs generic without re-observing in the same step); a subsequent
        // execution starts observing anew.
        let cache = fiber.frame().closure.proto.chunk.arith_cache(0);
        assert_eq!(cache, ArithCache::default());
        assert!(cache.specialized().is_none());
    }

    #[test]
    fn add_specializes_to_concat_str() {
        let (vm, fiber) = adaptive_harness(Op::Add);
        for _ in 0..WARMUP_THRESHOLD {
            let v = vm
                .eval_binop_adaptive(
                    &fiber,
                    0,
                    BinOp::Add,
                    Value::str("x"),
                    Value::str("y"),
                )
                .expect("ok");
            assert_eq!(v, Value::str("xy"));
        }
        let cache = fiber.frame().closure.proto.chunk.arith_cache(0);
        assert_eq!(
            cache,
            ArithCache::Specialized {
                kind: ArithKind::ConcatStr
            }
        );
        // Specialized concat still byte-identical (incl. a key containing braces).
        let v = vm
            .eval_binop_adaptive(
                &fiber,
                0,
                BinOp::Add,
                Value::str("1"),
                Value::str("2"),
            )
            .expect("ok");
        assert_eq!(v, Value::str("12"));
    }

    #[test]
    fn add_specializes_to_decimal() {
        let (vm, fiber) = adaptive_harness(Op::Add);
        let a = Decimal::new(15, 1); // 1.5
        let b = Decimal::new(25, 1); // 2.5
        for _ in 0..WARMUP_THRESHOLD {
            let v = vm
                .eval_binop_adaptive(&fiber, 0, BinOp::Add, Value::decimal(a), Value::decimal(b))
                .expect("ok");
            assert_eq!(v, Value::decimal(a + b));
        }
        let cache = fiber.frame().closure.proto.chunk.arith_cache(0);
        assert_eq!(
            cache,
            ArithCache::Specialized {
                kind: ArithKind::Decimal
            }
        );
        // Specialized decimal add equals the generic apply_binop result bit-exact.
        let v = vm
            .eval_binop_adaptive(&fiber, 0, BinOp::Add, Value::decimal(a), Value::decimal(b))
            .expect("ok");
        let generic =
            crate::interp::apply_binop(BinOp::Add, Value::decimal(a), Value::decimal(b), s())
                .expect("ok");
        assert_eq!(v, generic);
    }

    #[test]
    fn polymorphic_add_never_specializes_and_stays_correct() {
        let (vm, fiber) = adaptive_harness(Op::Add);
        for i in 0..(WARMUP_THRESHOLD as usize * 4) {
            let (a, b, want) = if i % 2 == 0 {
                (Value::float(2.0), Value::float(3.0), Value::float(5.0))
            } else {
                (
                    Value::str("a"),
                    Value::str("b"),
                    Value::str("ab"),
                )
            };
            let v = vm
                .eval_binop_adaptive(&fiber, 0, BinOp::Add, a, b)
                .expect("ok");
            assert_eq!(v, want);
            // Alternating kinds reset the warmup, so the site never specializes.
            assert!(
                fiber
                    .frame()
                    .closure
                    .proto
                    .chunk
                    .arith_cache(0)
                    .specialized()
                    .is_none(),
                "polymorphic site stays generic at i={i}"
            );
        }
    }

    #[test]
    fn specialized_number_add_panics_identically_on_non_number_after_deopt() {
        // After specializing to Number, a number+nil add must produce the SAME
        // Tier-2 panic the generic apply_binop gives (it deopts, then runs generic).
        let (vm, fiber) = adaptive_harness(Op::Add);
        for _ in 0..WARMUP_THRESHOLD {
            vm.eval_binop_adaptive(
                &fiber,
                0,
                BinOp::Add,
                Value::float(1.0),
                Value::float(1.0),
            )
            .expect("ok");
        }
        let got = vm.eval_binop_adaptive(&fiber, 0, BinOp::Add, Value::float(1.0), Value::nil());
        let generic =
            crate::interp::apply_binop(BinOp::Add, Value::float(1.0), Value::nil(), Span::new(0, 3));
        match (got, generic) {
            (Err(Control::Panic(a)), Err(Control::Panic(b))) => {
                assert_eq!(a.message, b.message);
                assert_eq!(a.span, b.span, "deopt path carries the op's span");
            }
            other => panic!("expected matching panics, got {other:?}"),
        }
    }

    #[test]
    fn get_global_cached_returns_same_builtin() {
        // Manually populate + read the global cache for a GET_GLOBAL site.
        let mut c = Chunk::new();
        let name = c.add_const(Value::str("print"));
        c.emit_u16(Op::GetGlobal, name, s());
        c.emit(Op::Return, s());
        let version = 0u64;
        assert!(c.global_cache(0).get(version).is_none(), "cold initially");
        c.set_global_cache(0, GlobalCache::set(Value::builtin("print"), version));
        match c.global_cache(0).get(version).map(Value::into_kind) {
            Some(OwnedKind::Builtin(n)) => assert_eq!(&*n, "print"),
            other => panic!("expected cached print builtin, got {other:?}"),
        }
        // A version bump invalidates it (defence-in-depth; never happens today).
        assert!(c.global_cache(0).get(version + 1).is_none());
    }

    #[test]
    fn hot_global_loop_resolves_print_consistently() {
        // End-to-end: a loop that references `print` many times prints each line —
        // the GET_GLOBAL_CACHED path must resolve the same builtin every iteration.
        let (out, _code) = {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            let local = LocalSet::new();
            local.block_on(&rt, async {
                crate::vm_run_source("for (i in range(0, 5)) { print(i) }")
                    .await
                    .unwrap()
            })
        };
        assert_eq!(out, "0\n1\n2\n3\n4\n");
    }

    // ── LANE §2.2: escalation leaves ip un-advanced ──────────────────────────

    /// LANE §2.2 reviewer-mandated gate: when `sync_burst` encounters an op that
    /// is NOT in the sync subset, it must return `Ok(SyncOutcome::NeedsAsync)`
    /// **without advancing the fiber's `ip`**. The async driver in `run_loop`
    /// re-decodes the same byte; if ip were advanced the op would be silently
    /// skipped — a correctness hole.
    ///
    /// Construction: a chunk whose FIRST instruction is `Op::Import` (not in the
    /// sync subset, has a u16 operand). Before calling `run_loop_sync` the fiber
    /// is at `ip == 0`. After the call it must still be at `ip == 0`.
    ///
    /// NOTE: `Op::Await` was previously used here but is now IN the sync subset
    /// (LANE Task 6 §4); it completes inline for non-Future TOS and pending-future
    /// TOS with a restored ip. `Op::Import` is the canonical always-async op.
    #[test]
    fn escalation_leaves_ip_un_advanced() {
        let mut c = Chunk::new();
        // Op::Import is NOT in sync_lane_op — it will trigger NeedsAsync immediately.
        // We need a valid u16 import index operand (0); the burst checks membership
        // before decoding operands, so the import table entry doesn't matter for
        // this test (NeedsAsync fires before the operand is read).
        c.emit_u16(Op::Import, 0, s());

        let proto = Rc::new(FnProto {
            chunk: c,
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
            name_span: None,
        });
        let closure = Closure::new(proto);
        let mut fiber = Fiber::new(closure);

        // ip starts at 0.
        assert_eq!(fiber.frame().ip, 0, "ip must start at 0");

        let interp = Rc::new(Interp::new());
        interp.install_self();
        // Use with_lanes so the sync_lane kill switch is explicitly ON.
        let vm = Vm::with_lanes(interp, true, true);

        // run_loop_sync must return NeedsAsync and leave ip == 0.
        let outcome = vm
            .run_loop_sync(&mut fiber)
            .expect("run_loop_sync must not panic on an escalating op");

        assert!(
            matches!(outcome, SyncOutcome::NeedsAsync),
            "expected NeedsAsync for a non-subset op, got Finished"
        );
        assert_eq!(
            fiber.frame().ip,
            0,
            "ip must be un-advanced after escalation: async driver must re-decode the same byte"
        );
    }

    /// LANE §4.1 case 4: `Op::Await` on a PENDING `Value::future` must escape
    /// to the async driver with `ip` restored to the `Op::Await` byte. The async
    /// driver re-decodes and parks on `f.get().await`. If ip were left advanced the
    /// Await instruction would be silently skipped — a correctness hole.
    ///
    /// Construction: push a pending `Value::future` onto the fiber's stack, then
    /// place a single `Op::Await` instruction (ip == 0). After `run_loop_sync`
    /// the ip must still be 0 and the future must still be on TOS.
    #[test]
    fn await_pending_future_escalates_with_ip_restored() {
        use crate::task::SharedFuture;

        let mut c = Chunk::new();
        c.emit(Op::Await, s()); // ip 0, no operand bytes

        let proto = Rc::new(FnProto {
            chunk: c,
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
            name_span: None,
        });
        let closure = Closure::new(proto);
        let mut fiber = Fiber::new(closure);

        // Push a PENDING future onto TOS.
        let pending = SharedFuture::new(); // not resolved — try_get() returns None
        fiber.push(Value::future(pending));

        assert_eq!(fiber.frame().ip, 0, "ip must start at 0");

        let interp = Rc::new(Interp::new());
        interp.install_self();
        let vm = Vm::with_lanes(interp, true, true);

        let outcome = vm
            .run_loop_sync(&mut fiber)
            .expect("run_loop_sync must not panic for a pending future await");

        assert!(
            matches!(outcome, SyncOutcome::NeedsAsync),
            "a pending future must cause NeedsAsync, not Finished"
        );
        assert_eq!(
            fiber.frame().ip,
            0,
            "ip must be restored to Op::Await after pending-future escalation"
        );
        // The future must still be on TOS (the burst peeked only — did not pop).
        assert!(
            matches!(fiber.peek(0).kind(), ValueKind::Future(_)),
            "pending future must remain on TOS after escalation"
        );
    }

    /// `expr?` where `expr` is not a 2-element array is a Tier-2 panic carrying
    /// the exact message and the PROPAGATE op's span (the `TryExpr`'s code span).
    #[test]
    fn propagate_non_pair_panics_with_span() {
        let mut c = Chunk::new();
        let k = c.add_const(Value::float(5.0));
        c.emit_u16(Op::Const, k, s());
        let prop_span = Span::new(8, 10);
        c.emit(Op::Propagate, prop_span);
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                assert_eq!(
                    e.message, "the ? operator requires a Result pair [value, err]",
                    "msg: {}",
                    e.message
                );
                assert_eq!(e.span, Some(prop_span), "panic carries the op's span");
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }
}
