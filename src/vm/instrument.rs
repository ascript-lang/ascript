//! The unified VM instrumentation seam (DBG, the debugger/profiler milestone).
//!
//! # Zero-cost when off (the headline constraint, Gate 12)
//!
//! The entire debugger/profiler/coverage tooling hangs off a *single*
//! `Option<Box<Instrumentation>>` field on [`crate::vm::run::Vm`] (beside
//! `specialize`). The `None` default — what every production run carries — is the
//! byte-identical-to-pre-DBG hot path: `run_loop` is NOT modified to load this field
//! per iteration. Breakpoints are reached ONLY through a runtime-patched
//! [`Op::Break`](crate::vm::opcode::Op::Break) byte (a side-table trap), so a run with
//! no breakpoint set never touches this module and the dispatch loop is unchanged.
//!
//! `Box` keeps `Vm` small (one pointer) so the not-attached path stays cache-tight.
//!
//! # The three sub-features (any subset may be armed)
//!
//! - `breakpoints` ([`DebuggerHook`]) — DBG: the breakpoint side table (offset →
//!   saved original byte), step mode, the call depth captured at a step, and the
//!   `Send` command/event channel ends to the DAP server thread.
//! - `profiler` ([`ProfilerHook`]) — DBG §6: a frame-name snapshot publisher read by
//!   a sampler thread (populated by a later task).
//! - `coverage` ([`CoverageTable`]) — DX §6.3: per-line hit counts (a typed
//!   placeholder here; DX owns the implementation).

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};

/// The single instrumentation payload hanging off `Vm.instrument`. Each sub-feature
/// is independently `Option`al, so a run may arm any subset (e.g. `--profile cpu`
/// populates only `profiler`, a debugger only `breakpoints`). When the whole
/// `Vm.instrument` is `None` (the default / production path), none of this is reached
/// and the run loop is byte-identical to pre-DBG.
pub struct Instrumentation {
    /// DBG breakpoints + stepping. `None` until a debugger arms it.
    pub breakpoints: Option<DebuggerHook>,
    /// DBG sampling profiler (a later task). A typed placeholder this unit.
    pub profiler: Option<ProfilerHook>,
    /// DX line-coverage table (DX §6.3). A typed placeholder — DX owns it.
    pub coverage: Option<CoverageTable>,
}

impl Instrumentation {
    /// A fully-inert payload (every sub-feature `None`). Behaviorally identical to a
    /// `Vm.instrument == None` run — used by tests and by an attach that arms a
    /// sub-feature later.
    pub fn empty() -> Self {
        Instrumentation {
            breakpoints: None,
            profiler: None,
            coverage: None,
        }
    }
}

impl Default for Instrumentation {
    fn default() -> Self {
        Self::empty()
    }
}

/// Identifies a single bytecode offset within a specific function prototype. The
/// breakpoint side table is keyed by this so a patch is unambiguous across the proto
/// tree (the same offset exists in many protos). `proto_id` is the proto's `Rc`
/// identity address (`Rc::as_ptr as usize`) — stable for a compiled chunk's lifetime.
pub type ProtoOffset = (usize, usize);

/// How the debugger should resume after a stop: run free, or set transient
/// breakpoints to re-stop after one source-line's worth of progress.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StepMode {
    /// Not stepping — `continue`: resume until the next *persistent* breakpoint.
    #[default]
    Run,
    /// `next` (step over): re-break at the next line in the current frame, or when
    /// the stack returns to the current depth at a new line.
    Over,
    /// `stepIn`: re-break at the next line OR on entering a callee.
    Into,
    /// `stepOut`: re-break when the stack drops below the current depth.
    Out,
}

/// A command sent from the DAP server thread to the parked VM thread (plain data —
/// the worker-airlock discipline; NO `Value`/`Rc`/`Cc` ever crosses). This unit
/// defines the resume commands the trap handler blocks on; the DAP server (a later
/// unit) extends the protocol (set-breakpoints, evaluate, …).
#[derive(Clone, Debug)]
pub enum DebugCommand {
    /// Resume free-running until the next persistent breakpoint.
    Continue,
    /// Step over (`next`).
    Next,
    /// Step into.
    StepIn,
    /// Step out.
    StepOut,
    /// Set the breakpoints for a source file to EXACTLY this line set (DAP setBreakpoints
    /// is declarative/replace-all per source). Lines are 1-based as the editor sends them;
    /// the VM maps each to the first bytecode offset on that line. Plain data — resolution
    /// happens on the VM thread.
    SetBreakpoints { source: String, lines: Vec<u32> },
    /// Clear ALL breakpoints (restore every patched byte).
    ClearBreakpoints,
}

/// One requested breakpoint line and the VM's verdict on it: whether a real instruction
/// could be bound and the bytecode offset it bound to (for the DAP `breakpoint` reply).
/// Plain owned data only — no `Rc`/`Value`/`Cc` (the worker-airlock discipline).
#[derive(Clone, Debug)]
pub struct BreakpointBinding {
    /// The 1-based source line the editor requested.
    pub line: u32,
    /// Whether a breakpoint was actually set (a real instruction was found at/after the
    /// line in a candidate proto). `false` means unbound (no executable line).
    pub verified: bool,
    /// The bytecode offset the breakpoint bound to, when `verified`. `None` if unbound.
    pub offset: Option<u32>,
}

/// A Send-safe snapshot of one call frame at a debugger stop. PLAIN OWNED DATA only —
/// no `Value`/`Rc`/`Cc` crosses the channel (the worker-airlock discipline). Built on
/// the VM thread (where `Value` access is fine) and rendered to `String`/`u32` before
/// sending, so the whole snapshot is `Send` (compile-time-proved by an `_assert_send`
/// test). Never add an `Rc`/`Value`/`Cc` field — that would break the airlock.
#[derive(Clone, Debug)]
pub struct FrameSnapshot {
    /// Human label for the frame: the function name, "<script>" for the bottom/module
    /// frame, or "fn@L<line>" when no name is recorded.
    pub function: String,
    /// 0-based source line of this frame's active instruction (the DAP layer adds +1).
    pub line: u32,
    /// 0-based source column.
    pub column: u32,
    /// Locals in slot order: (name, rendered value). Name is the source name when known
    /// (`FnProto.local_names`) else "slot_N". Value is `format!("{}", v)` (owned String).
    pub locals: Vec<(String, String)>,
}

/// An event sent from the VM thread to the DAP server thread when execution stops or
/// produces output (plain data only). This unit ships the `Stopped` snapshot fields
/// the trap handler builds, INCLUDING the Send-safe per-frame [`FrameSnapshot`] vector
/// (rendered to owned `String`/`u32` on the VM thread before crossing the channel).
#[derive(Clone, Debug)]
pub enum DebugEvent {
    /// The VM parked at a breakpoint / step. Carries the minimal plain-data location
    /// the controller needs plus the full Send-safe frame/variable snapshot.
    Stopped {
        /// The proto identity the break is in (`Rc::as_ptr as usize`).
        proto_id: usize,
        /// The bytecode offset of the patched (trapped) instruction.
        offset: usize,
        /// The call-stack depth at the stop (`fiber.frames.len()`).
        depth: usize,
        /// The Send-safe frame/variable snapshot, innermost frame first. Plain owned
        /// `String`/`u32` only — no `Value`/`Rc`/`Cc` crosses the channel.
        frames: Vec<FrameSnapshot>,
    },
    /// Reply to [`DebugCommand::SetBreakpoints`]: per requested line, whether a
    /// breakpoint was bound and where. Plain owned data only.
    BreakpointsVerified { results: Vec<BreakpointBinding> },
}

/// The DBG debugger hook: the breakpoint side table + stepping state + the `Send`
/// command/event channel ends to the DAP server thread.
///
/// # Patching does not move offsets
///
/// A breakpoint overwrites exactly ONE opcode byte in place (`Op::Break as u8`) and
/// saves the displaced byte. It never inserts or removes bytes, so every other
/// offset — and crucially the inline-cache side maps (`field_ics`/`method_ics`,
/// keyed by offset) and the `spans`/`line_starts` tables — stays valid while
/// breakpoints are set. `clear` restores the exact saved byte.
pub struct DebuggerHook {
    /// Breakpoint side table: `(proto_id, offset)` → the original opcode byte the
    /// `Op::Break` displaced. The trap handler recovers the original op from here and
    /// re-dispatches it after the stop.
    breakpoints: HashMap<ProtoOffset, u8>,
    /// The current step mode (set by a `next`/`stepIn`/`stepOut` resume).
    pub step_mode: StepMode,
    /// The call-stack depth captured when a step began (`fiber.frames.len()`), used to
    /// decide when a `next`/`stepOut` should re-break.
    pub step_depth: usize,
    /// Commands FROM the DAP server thread (the trap handler blocks on this).
    pub commands: Receiver<DebugCommand>,
    /// Events TO the DAP server thread (the trap handler ships `Stopped` here).
    pub events: Sender<DebugEvent>,
}

impl DebuggerHook {
    /// Build a hook over a freshly-created command/event channel pair. Returns the
    /// hook (held by the VM) and the controller ends (held by the DAP server thread):
    /// `(hook, command_sender, event_receiver)`.
    pub fn new() -> (Self, Sender<DebugCommand>, Receiver<DebugEvent>) {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let hook = DebuggerHook {
            breakpoints: HashMap::new(),
            step_mode: StepMode::Run,
            step_depth: 0,
            commands: cmd_rx,
            events: evt_tx,
        };
        (hook, cmd_tx, evt_rx)
    }

    /// Set a breakpoint at `(proto_id, offset)`, saving the original byte and writing
    /// [`Op::Break`](crate::vm::opcode::Op::Break) in its place. `code` is the proto's
    /// `code` byte stream (the patch site). Idempotent: re-setting a live breakpoint
    /// keeps the original byte (does NOT save the already-patched `Op::Break` over it).
    /// Returns the original byte that was displaced.
    ///
    /// Patching is in place — it never moves an offset, so the inline-cache side maps
    /// (`field_ics`/`method_ics`, keyed by offset) and the `spans`/`line_starts`
    /// tables stay valid while the breakpoint is set.
    pub fn set_breakpoint(&mut self, proto_id: usize, offset: usize, code: &mut [u8]) -> u8 {
        let key = (proto_id, offset);
        // If already patched, the saved byte is authoritative — never re-save the
        // Op::Break byte over the real original.
        let original = *self.breakpoints.entry(key).or_insert_with(|| code[offset]);
        code[offset] = crate::vm::opcode::Op::Break as u8;
        original
    }

    /// Set a breakpoint at `(proto_id, offset)` while the chunk is reachable only through
    /// a *shared* `&Chunk` (the runtime case — the parked VM patches a live proto). Saves
    /// the original byte into the side table (idempotent, mirroring [`set_breakpoint`])
    /// and writes [`Op::Break`](crate::vm::opcode::Op::Break) through
    /// [`Chunk::patch_byte`](crate::vm::chunk::Chunk::patch_byte), which is sound because
    /// `Code` is `UnsafeCell`-backed (single-threaded, no live `&` into the buffer at the
    /// debug-stop site). Returns the original byte that was displaced.
    ///
    /// Like [`set_breakpoint`], patching is in place — it never moves an offset, so the
    /// inline-cache side maps and the `spans`/`line_starts` tables stay valid.
    pub fn set_breakpoint_shared(
        &mut self,
        proto_id: usize,
        offset: usize,
        chunk: &crate::vm::chunk::Chunk,
    ) -> u8 {
        let key = (proto_id, offset);
        // If already patched, the saved byte is authoritative — never re-save the
        // Op::Break byte over the real original.
        let original = *self.breakpoints.entry(key).or_insert_with(|| chunk.code[offset]);
        chunk.patch_byte(offset, crate::vm::opcode::Op::Break as u8);
        original
    }

    /// Drain the side table, yielding each `((proto_id, offset), original_byte)` so the
    /// caller can restore every patched byte through its `&Chunk`. Leaves the hook with
    /// no breakpoints (used by `ClearBreakpoints`).
    pub fn drain_breakpoints(&mut self) -> Vec<(ProtoOffset, u8)> {
        self.breakpoints.drain().collect()
    }

    /// The `(proto_id, offset)` keys of the breakpoints currently bound to `source_protos`
    /// — i.e. whose `proto_id` appears in the given set. Used by `SetBreakpoints` to clear
    /// just one source's existing breakpoints before re-binding the requested line set
    /// (DAP setBreakpoints is replace-all *per source*).
    pub fn breakpoints_in(&self, proto_ids: &std::collections::HashSet<usize>) -> Vec<ProtoOffset> {
        self.breakpoints
            .keys()
            .filter(|(pid, _)| proto_ids.contains(pid))
            .copied()
            .collect()
    }

    /// Remove a single side-table entry and return its saved original byte (the caller
    /// restores it through the chunk). No-op (`None`) if absent. Does NOT touch the code
    /// (the caller patches), unlike [`clear_breakpoint`].
    pub fn forget_breakpoint(&mut self, proto_id: usize, offset: usize) -> Option<u8> {
        self.breakpoints.remove(&(proto_id, offset))
    }

    /// Clear a breakpoint at `(proto_id, offset)`, restoring the exact original byte.
    /// No-op (returns `None`) if no breakpoint was set there.
    pub fn clear_breakpoint(
        &mut self,
        proto_id: usize,
        offset: usize,
        code: &mut [u8],
    ) -> Option<u8> {
        let key = (proto_id, offset);
        let original = self.breakpoints.remove(&key)?;
        code[offset] = original;
        Some(original)
    }

    /// The original opcode byte a patched `(proto_id, offset)` displaced, if any. The
    /// trap handler uses this to recover and re-dispatch the real instruction.
    pub fn original_byte(&self, proto_id: usize, offset: usize) -> Option<u8> {
        self.breakpoints.get(&(proto_id, offset)).copied()
    }

    /// Whether any breakpoint is currently set (used to short-circuit teardown).
    pub fn is_empty(&self) -> bool {
        self.breakpoints.is_empty()
    }

    /// The number of live breakpoints (test/diagnostic use).
    pub fn len(&self) -> usize {
        self.breakpoints.len()
    }
}

/// The DBG sampling-profiler hook (a later task). A typed placeholder this unit — the
/// VM will publish a frame-name snapshot here at frame push/pop only, behind the same
/// single `Vm.instrument` gate. Defined now so `Instrumentation`'s shape is stable.
#[derive(Default)]
pub struct ProfilerHook {
    /// Reserved for the `Send` frame-name snapshot seam. Empty placeholder.
    _reserved: (),
}

/// The DX line-coverage table (DX §6.3). A typed placeholder — DX owns the
/// implementation; defined here so the shared `Instrumentation` lands once.
#[derive(Default)]
pub struct CoverageTable {
    /// Reserved for per-line hit counts (DX). Empty placeholder.
    _reserved: (),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_snapshot_is_send() {
        // Compile-time proof the airlock snapshot type stays Send (all String/u32).
        fn _assert_send<T: Send>() {}
        _assert_send::<FrameSnapshot>();
        _assert_send::<DebugEvent>();
        _assert_send::<DebugCommand>();
        _assert_send::<BreakpointBinding>();
    }

    #[test]
    fn empty_instrumentation_is_fully_inert() {
        let inst = Instrumentation::empty();
        assert!(inst.breakpoints.is_none());
        assert!(inst.profiler.is_none());
        assert!(inst.coverage.is_none());
    }

    #[test]
    fn fresh_hook_has_no_breakpoints() {
        let (hook, _cmd, _evt) = DebuggerHook::new();
        assert!(hook.is_empty());
        assert_eq!(hook.len(), 0);
        assert_eq!(hook.original_byte(0, 0), None);
        assert_eq!(hook.step_mode, StepMode::Run);
    }

    #[test]
    fn set_breakpoint_patches_and_saves_original() {
        use crate::vm::opcode::Op;
        let (mut hook, _cmd, _evt) = DebuggerHook::new();
        let mut code = vec![Op::Add as u8, Op::Return as u8];
        let original = hook.set_breakpoint(0xABCD, 0, &mut code);
        assert_eq!(original, Op::Add as u8);
        assert_eq!(code[0], Op::Break as u8, "byte patched to Op::Break");
        assert_eq!(code[1], Op::Return as u8, "neighbor byte untouched");
        assert_eq!(hook.original_byte(0xABCD, 0), Some(Op::Add as u8));
        assert_eq!(hook.len(), 1);
    }

    #[test]
    fn clear_restores_exact_original_byte() {
        use crate::vm::opcode::Op;
        let (mut hook, _cmd, _evt) = DebuggerHook::new();
        let mut code = vec![Op::Mul as u8];
        hook.set_breakpoint(7, 0, &mut code);
        assert_eq!(code[0], Op::Break as u8);
        let restored = hook.clear_breakpoint(7, 0, &mut code);
        assert_eq!(restored, Some(Op::Mul as u8));
        assert_eq!(code[0], Op::Mul as u8, "exact original byte restored");
        assert!(hook.is_empty());
        assert_eq!(hook.original_byte(7, 0), None);
    }

    #[test]
    fn set_breakpoint_is_idempotent_keeps_original() {
        use crate::vm::opcode::Op;
        let (mut hook, _cmd, _evt) = DebuggerHook::new();
        let mut code = vec![Op::Sub as u8];
        hook.set_breakpoint(1, 0, &mut code); // saves Sub, writes Break
        let again = hook.set_breakpoint(1, 0, &mut code); // re-set the live bp
        assert_eq!(again, Op::Sub as u8, "re-set keeps the real original");
        assert_eq!(code[0], Op::Break as u8);
        assert_eq!(hook.len(), 1, "no duplicate entry");
    }

    #[test]
    fn clear_missing_breakpoint_is_noop() {
        use crate::vm::opcode::Op;
        let (mut hook, _cmd, _evt) = DebuggerHook::new();
        let mut code = vec![Op::Nil as u8];
        assert_eq!(hook.clear_breakpoint(0, 0, &mut code), None);
        assert_eq!(code[0], Op::Nil as u8);
    }
}
