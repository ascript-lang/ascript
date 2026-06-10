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
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    /// Evaluate `expr` in the PAUSED frame `frame_id` (the DAP `evaluate` request — Watch
    /// panel / Debug Console / hover). `frame_id` is the index into the innermost-first
    /// frame snapshot order (the same id the DAP `stackTrace` reply assigns). The VM bridges
    /// the live frame locals + module globals into an `Environment` and runs the parsed
    /// expression on the tree-walker (`Interp::eval_expr`), then ships an
    /// [`DebugEvent::EvaluateResult`] back — plain owned data only (the rendered value is a
    /// `String`; NO `Value`/`Rc`/`Cc` crosses the channel).
    ///
    /// Side-effects in the expression DO run (like the V8/Chrome debug console). This is the
    /// SHARED evaluator that conditional breakpoints / logpoints will reuse at
    /// breakpoint-check time (a documented follow-up — they build on `eval_in_paused_frame`).
    Evaluate { expr: String, frame_id: usize },
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
    /// Program output (the debuggee's captured `print` stream), shipped to the DAP
    /// server so it can be re-emitted as an `output` event on the protocol stdout.
    /// Plain owned `String` — keeps program output OFF the protocol stdout (which the
    /// DAP framing owns), so the editor sees it as DAP output while the channel stays
    /// the single transport. `stderr` selects the DAP `output` category (`true` →
    /// `"stderr"`, e.g. an uncaught panic; `false` → `"stdout"`). Sent by the debuggee
    /// thread (DBG Task 5b).
    Output { text: String, stderr: bool },
    /// The debuggee program finished. Carries the process exit code (0 = normal,
    /// non-zero = an `exit(n)` or an uncaught Tier-2 panic). Sent by the debuggee
    /// thread after `vm.run` returns, just before it drops the hook (DBG Task 5b).
    Terminated { exit_code: i32 },
    /// Reply to [`DebugCommand::Evaluate`]: the rendered result of evaluating an
    /// expression in a paused frame. `ok` is `false` for a parse error, a thrown
    /// (Tier-2) panic, or a `?`-propagation; `display` then carries the error text (or
    /// `<parse error: …>`). On success `display` is the value rendered via `Value`'s
    /// `Display`. PLAIN OWNED DATA only — the value is rendered to a `String` on the VM
    /// thread; NO `Value`/`Rc`/`Cc` crosses the channel (the worker-airlock discipline).
    EvaluateResult { ok: bool, display: String },
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

/// The CROSS-THREAD frame-name snapshot the sampler thread reads. The VM thread
/// publishes the current frame-name stack (root → leaf) into this on every frame
/// push/pop; the sampler thread locks it, clones it, and records it as one sample.
///
/// PLAIN OWNED DATA ONLY — every element is an owned `String` (a function name, or
/// `"<script>"`/`"<anon>"`), so the whole thing is `Send` (the worker-airlock
/// discipline: NO `Value`/`Rc`/`Cc` crosses to the sampler thread). This is the only
/// state shared between the VM thread and the sampler thread.
pub type FrameStack = Arc<Mutex<Vec<String>>>;

/// The mode the profiler samples in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProfileMode {
    /// A wall-clock sampler thread snapshots the published frame stack every
    /// `interval`. The natural, real-world mode.
    Wallclock,
    /// No timer thread — a sample is recorded INLINE at each frame push (sampling is
    /// driven by call structure, not wall-clock). Makes goldens byte-stable.
    Deterministic,
}

/// The DBG sampling-profiler hook. The VM publishes the current frame-name stack here
/// at frame push/pop ONLY (never per instruction), behind the same single
/// `Vm.instrument` gate as the debugger — so a run with no profiler armed is
/// byte-identical to pre-DBG (the per-instruction dispatch loop is untouched; the only
/// cost is a single `None`-check at the per-CALL push/pop sites).
///
/// # Two sampling modes
///
/// - [`ProfileMode::Wallclock`]: the hook owns a sampler thread (started by
///   [`ProfilerHook::start`]) that wakes every `interval`, locks [`frames`], clones the
///   current stack, and pushes it as one sample. On [`ProfilerHook::finish`] the stop
///   flag is set, the thread joined, and its accumulated samples returned.
/// - [`ProfileMode::Deterministic`]: NO thread — a sample is recorded inline at each
///   publish (each frame push = one sample), so the sample set is a pure function of
///   the program's call structure (golden-stable).
///
/// [`frames`]: ProfilerHook::frames
pub struct ProfilerHook {
    /// The current frame-name stack snapshot (root → leaf), shared with the sampler
    /// thread. The VM writes it on every publish; the sampler reads it. Owned
    /// `String`s only (`Send`).
    pub frames: FrameStack,
    /// The sampling mode.
    pub mode: ProfileMode,
    /// The wall-clock sampling interval (ignored in deterministic mode).
    pub interval: Duration,
    /// Stop flag for the sampler thread (set by `finish`).
    stop: Arc<AtomicBool>,
    /// The sampler thread handle (`Wallclock` mode only; `None` until `start`, and in
    /// deterministic mode). The thread owns its sample buffer and returns it on join.
    sampler: Option<std::thread::JoinHandle<Vec<Vec<String>>>>,
    /// Samples collected INLINE in deterministic mode (one per publish). Unused in
    /// wallclock mode (the thread owns the buffer there).
    inline_samples: Vec<Vec<String>>,
}

impl ProfilerHook {
    /// Build a hook in the given mode. In `Wallclock` mode the sampler thread is NOT
    /// started yet — call [`start`](ProfilerHook::start) once the run begins. In
    /// `Deterministic` mode there is no thread (samples accrue inline).
    pub fn new(mode: ProfileMode, interval: Duration) -> Self {
        ProfilerHook {
            frames: Arc::new(Mutex::new(Vec::new())),
            mode,
            interval,
            stop: Arc::new(AtomicBool::new(false)),
            sampler: None,
            inline_samples: Vec::new(),
        }
    }

    /// Start the wall-clock sampler thread (no-op in deterministic mode, or if already
    /// started). The thread loops: sleep `interval`, check the stop flag, snapshot the
    /// published frame stack, push it as a sample — until `finish` sets the flag.
    pub fn start(&mut self) {
        if self.mode != ProfileMode::Wallclock || self.sampler.is_some() {
            return;
        }
        let frames = Arc::clone(&self.frames);
        let stop = Arc::clone(&self.stop);
        let interval = self.interval;
        let handle = std::thread::spawn(move || {
            let mut samples: Vec<Vec<String>> = Vec::new();
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(interval);
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                // Snapshot the current stack. A poisoned lock (a panic on the VM
                // thread mid-publish) is recovered — a stale snapshot is harmless.
                let snap = match frames.lock() {
                    Ok(g) => g.clone(),
                    Err(p) => p.into_inner().clone(),
                };
                if !snap.is_empty() {
                    samples.push(snap);
                }
            }
            samples
        });
        self.sampler = Some(handle);
    }

    /// Record one sample of the CURRENT published stack inline (deterministic mode).
    /// Called by the VM's publish seam at each frame push when the mode is
    /// `Deterministic`. A no-op for an empty stack.
    pub fn record_inline_sample(&mut self) {
        let snap = match self.frames.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        };
        if !snap.is_empty() {
            self.inline_samples.push(snap);
        }
    }

    /// Publish a new frame-name stack snapshot (called by the VM publish seam after a
    /// push/pop). Replaces the shared stack atomically under the lock.
    pub fn publish(&self, stack: Vec<String>) {
        match self.frames.lock() {
            Ok(mut g) => *g = stack,
            Err(p) => *p.into_inner() = stack,
        }
    }

    /// Stop sampling and return all collected samples (each a root → leaf frame-name
    /// path). In wallclock mode this sets the stop flag and JOINS the sampler thread
    /// (so no thread is left running). In deterministic mode it returns the
    /// inline-collected samples.
    pub fn finish(mut self) -> Vec<Vec<String>> {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.sampler.take() {
            return handle.join().unwrap_or_default();
        }
        std::mem::take(&mut self.inline_samples)
    }
}

/// The DX line-coverage table (DX §6.3).
///
/// # The patch-based, hot-loop-untouched design
///
/// Coverage adds NO per-instruction check. At arm time, for each proto, the FIRST
/// bytecode offset of every source line is patched to [`Op::Break`](crate::vm::opcode::Op::Break)
/// (reusing DBG's software-breakpoint trap). The cold `Op::Break` arm in the run loop
/// consults [`trap`](CoverageTable::trap) FIRST: if `(proto_id, fault_ip)` is a coverage
/// trap it marks the line covered, restores the original byte, re-points `ip`, and
/// re-dispatches — so each line traps AT MOST ONCE then runs free (zero steady-state cost
/// and byte-identical program output). If it is NOT a coverage trap the arm falls through
/// to the debugger logic unchanged.
///
/// # Plain owned data (worker-airlock safe)
///
/// Every field is plain owned data (`HashMap`/`HashSet`/`String`/integers) — NO
/// `Rc`/`Value`/`Cc`. The whole table can cross the worker airlock for a parallel-run
/// merge ([`merge`](CoverageTable::merge)).
///
/// v1 records covered/not-covered (a line is either hit or it isn't), not hit COUNTS —
/// because each line un-patches after its first trap, a per-line count would always be 1.
/// The LCOV `DA` count is therefore `1` for a covered line, `0` otherwise (documented).
#[derive(Default, Clone)]
pub struct CoverageTable {
    /// The patch side table: `(proto_id, offset)` → `(original_byte, line)`. Built at
    /// arm time; consulted by the `Op::Break` trap to recognize a coverage trap, recover
    /// the displaced opcode, and learn which source line the offset belongs to. Lines are
    /// 0-based (matching `build_line_starts`); the reporter adds +1 for 1-based display.
    traps: HashMap<ProtoOffset, (u8, u32)>,
    /// `proto_id` → the source file path the proto belongs to (from `chunk.source`).
    proto_path: HashMap<usize, String>,
    /// The instrumented universe: per `proto_id`, every line that COULD be covered (so
    /// the report can list UNCOVERED lines too, not just hit ones).
    instrumented: HashMap<usize, std::collections::HashSet<u32>>,
    /// The covered set: `(proto_id, line0)` that actually trapped (= executed); `line0`
    /// is the 0-based source line. v1 is covered/not, not a count.
    covered: std::collections::HashSet<(usize, u32)>,
}

impl CoverageTable {
    /// A fresh, unarmed table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one patch site at arm time: `(proto_id, offset)` displaced `original`
    /// (the real opcode byte) and belongs to 0-based source `line`. Also folds `line`
    /// into the proto's instrumented universe.
    pub fn record_trap(&mut self, proto_id: usize, offset: usize, original: u8, line: u32) {
        self.traps.insert((proto_id, offset), (original, line));
        self.instrumented.entry(proto_id).or_default().insert(line);
    }

    /// Bind a `proto_id` to its source file path (from `chunk.source.path`).
    pub fn record_path(&mut self, proto_id: usize, path: String) {
        self.proto_path.insert(proto_id, path);
    }

    /// Whether `(proto_id, offset)` is a coverage trap. The `Op::Break` arm checks this
    /// FIRST so a coverage trap is recovered as coverage (not handed to the debugger).
    pub fn trap(&self, proto_id: usize, offset: usize) -> Option<(u8, u32)> {
        self.traps.get(&(proto_id, offset)).copied()
    }

    /// Mark `(proto_id, line)` covered (the trap fired ⇒ the line executed).
    pub fn mark_covered(&mut self, proto_id: usize, line: u32) {
        self.covered.insert((proto_id, line));
    }

    /// The number of live patch sites (test/diagnostic use).
    pub fn trap_count(&self) -> usize {
        self.traps.len()
    }

    /// Whether nothing has been armed (no patch sites). An empty table is inert.
    pub fn is_empty(&self) -> bool {
        self.traps.is_empty()
    }

    /// Merge `other` into `self`: union the trap side table, paths, instrumented universe,
    /// and the covered set. Used to fold per-isolate tables after a `--parallel` run.
    /// Order-independent (set/map unions), so the merged result is identical regardless of
    /// which isolate finished first.
    pub fn merge(&mut self, other: &CoverageTable) {
        for (k, v) in &other.traps {
            self.traps.entry(*k).or_insert(*v);
        }
        for (k, v) in &other.proto_path {
            self.proto_path.entry(*k).or_insert_with(|| v.clone());
        }
        for (pid, lines) in &other.instrumented {
            let entry = self.instrumented.entry(*pid).or_default();
            for l in lines {
                entry.insert(*l);
            }
        }
        for c in &other.covered {
            self.covered.insert(*c);
        }
    }

    /// A by-file, by-line covered/total view, folded across all protos that map to the
    /// same source path. Returns a `Vec<FileCoverage>` sorted by path (stable output).
    /// Each file's `lines` is a sorted `(line_1based, covered)` list — the reporter emits
    /// text/lcov/html from this.
    pub fn by_file(&self) -> Vec<FileCoverage> {
        use std::collections::BTreeMap;
        // path → (line_1based → covered). A line is covered if ANY proto on that path
        // covered it (a line can host instructions in several protos — e.g. a closure
        // literal on the same line as its enclosing statement).
        let mut files: BTreeMap<String, BTreeMap<u32, bool>> = BTreeMap::new();
        for (pid, lines) in &self.instrumented {
            let Some(path) = self.proto_path.get(pid) else {
                continue; // a proto with no bound source — skip cleanly (never panic).
            };
            let file = files.entry(path.clone()).or_default();
            for &line0 in lines {
                let covered = self.covered.contains(&(*pid, line0));
                let entry = file.entry(line0 + 1).or_insert(false);
                *entry = *entry || covered;
            }
        }
        files
            .into_iter()
            .map(|(path, lines)| FileCoverage {
                path,
                lines: lines.into_iter().collect(),
            })
            .collect()
    }
}

/// A by-file coverage view (one entry per source file), produced by
/// [`CoverageTable::by_file`]. Plain owned data.
#[derive(Clone, Debug)]
pub struct FileCoverage {
    /// The source file path.
    pub path: String,
    /// `(line_1based, covered)` for every INSTRUMENTED line on this file, sorted by line.
    pub lines: Vec<(u32, bool)>,
}

impl FileCoverage {
    /// The number of covered lines.
    pub fn covered(&self) -> usize {
        self.lines.iter().filter(|(_, c)| *c).count()
    }

    /// The total number of instrumented lines.
    pub fn total(&self) -> usize {
        self.lines.len()
    }

    /// The coverage percentage (0.0 when there are no instrumented lines).
    pub fn percent(&self) -> f64 {
        if self.lines.is_empty() {
            0.0
        } else {
            100.0 * self.covered() as f64 / self.total() as f64
        }
    }

    /// The sorted list of UNCOVERED 1-based line numbers.
    pub fn uncovered_lines(&self) -> Vec<u32> {
        self.lines
            .iter()
            .filter(|(_, c)| !*c)
            .map(|(l, _)| *l)
            .collect()
    }
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
        // The profiler's cross-thread snapshot type (read by the sampler thread)
        // carries owned `String`s only — proved `Send`/`Sync` at compile time.
        fn _assert_send_sync<T: Send + Sync>() {}
        _assert_send_sync::<FrameStack>();
        _assert_send::<Vec<Vec<String>>>();
    }

    #[test]
    fn deterministic_hook_collects_inline_samples() {
        let mut hook = ProfilerHook::new(ProfileMode::Deterministic, Duration::from_millis(1));
        hook.publish(vec!["<script>".to_string(), "a".to_string()]);
        hook.record_inline_sample();
        hook.publish(vec![
            "<script>".to_string(),
            "a".to_string(),
            "b".to_string(),
        ]);
        hook.record_inline_sample();
        // An empty stack records nothing.
        hook.publish(Vec::new());
        hook.record_inline_sample();
        let samples = hook.finish();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0], vec!["<script>", "a"]);
        assert_eq!(samples[1], vec!["<script>", "a", "b"]);
    }

    #[test]
    fn wallclock_finish_with_no_thread_is_empty() {
        // A wallclock hook never `start`ed (no thread) finishes with no samples and
        // does not hang.
        let hook = ProfilerHook::new(ProfileMode::Wallclock, Duration::from_millis(1));
        assert!(hook.finish().is_empty());
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

    // ---- DX D2 Task 6 — CoverageTable -------------------------------------

    #[test]
    fn coverage_table_is_plain_send() {
        // Plain owned data only — it crosses the worker airlock for a parallel merge.
        fn _assert_send<T: Send>() {}
        _assert_send::<CoverageTable>();
        _assert_send::<FileCoverage>();
    }

    #[test]
    fn coverage_records_and_recognizes_traps() {
        let mut t = CoverageTable::new();
        assert!(t.is_empty());
        // proto 7, offset 3 displaced opcode 0x42, source line 5 (0-based).
        t.record_trap(7, 3, 0x42, 5);
        t.record_path(7, "m.as".to_string());
        assert_eq!(t.trap_count(), 1);
        assert!(!t.is_empty());
        // The trap is recognized at exactly (proto, offset).
        assert_eq!(t.trap(7, 3), Some((0x42, 5)));
        assert_eq!(t.trap(7, 4), None, "a non-trap offset is not recognized");
        assert_eq!(t.trap(8, 3), None, "a different proto is not recognized");
    }

    #[test]
    fn coverage_by_file_covered_and_uncovered() {
        let mut t = CoverageTable::new();
        t.record_path(1, "f.as".to_string());
        // lines 0,1,2 (0-based) instrumented; cover 0 and 2.
        t.record_trap(1, 0, 1, 0);
        t.record_trap(1, 2, 1, 1);
        t.record_trap(1, 4, 1, 2);
        t.mark_covered(1, 0);
        t.mark_covered(1, 2);
        let files = t.by_file();
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.path, "f.as");
        assert_eq!(f.covered(), 2);
        assert_eq!(f.total(), 3);
        // 1-based: lines 1 and 3 covered, line 2 uncovered.
        assert_eq!(f.uncovered_lines(), vec![2]);
    }

    #[test]
    fn coverage_merge_is_order_independent_union() {
        // Two per-isolate tables for the SAME file (different protos), merged either way.
        let mut a = CoverageTable::new();
        a.record_path(1, "g.as".to_string());
        a.record_trap(1, 0, 1, 0); // line 1 (1-based)
        a.mark_covered(1, 0);
        let mut b = CoverageTable::new();
        b.record_path(2, "g.as".to_string());
        b.record_trap(2, 0, 1, 1); // line 2 (1-based)
        // b's line 2 NOT covered.

        let mut ab = a.clone();
        ab.merge(&b);
        let mut ba = b.clone();
        ba.merge(&a);

        let fab = ab.by_file();
        let fba = ba.by_file();
        // Both orders produce the same file view: line 1 covered, line 2 not.
        assert_eq!(fab.len(), 1);
        assert_eq!(fba.len(), 1);
        assert_eq!(fab[0].covered(), 1);
        assert_eq!(fab[0].total(), 2);
        assert_eq!(fab[0].uncovered_lines(), vec![2]);
        assert_eq!(fba[0].covered(), fab[0].covered());
        assert_eq!(fba[0].total(), fab[0].total());
        assert_eq!(fba[0].uncovered_lines(), fab[0].uncovered_lines());
    }

    #[test]
    fn coverage_by_file_skips_proto_without_path() {
        // A proto with instrumented lines but no recorded path is skipped (never panics).
        let mut t = CoverageTable::new();
        t.record_trap(9, 0, 1, 0); // no record_path for proto 9
        assert!(t.by_file().is_empty());
    }

    #[test]
    fn coverage_line_covered_if_any_proto_covers_it() {
        // Two protos host an instruction on the SAME 1-based line; one covers it.
        let mut t = CoverageTable::new();
        t.record_path(1, "h.as".to_string());
        t.record_path(2, "h.as".to_string());
        t.record_trap(1, 0, 1, 0); // proto1, line0 -> 1-based 1
        t.record_trap(2, 0, 1, 0); // proto2, line0 -> 1-based 1
        t.mark_covered(2, 0); // only proto2 executed it
        let files = t.by_file();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].covered(), 1, "line counts as covered (proto2 hit it)");
        assert_eq!(files[0].total(), 1);
    }
}
