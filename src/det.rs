//! SP9 §3 — determinism seams (`DeterminismContext`).
//!
//! A per-[`crate::interp::Interp`] context that, when present, makes the
//! non-deterministic stdlib seams (the wall/monotonic **clock**, the seeded
//! **RNG**, and — in Phase 3 — recorded I/O **ordering** via `std/workflow`)
//! reproducible. It is **INERT by default**: `Interp.determinism` is `None`, every
//! seam takes its existing real-clock / thread-local-RNG path, and behavior is
//! byte-identical to pre-SP9. It activates only when deterministic mode is entered
//! (today: by `workflow.run`/`resume`; a future `--deterministic --seed N` CLI flag
//! is a small addition once this exists).
//!
//! Core/unconditional: this module uses only plain Rust types (NO `serde`), so it
//! builds under `--no-default-features`. The *persistence* of the event stream to
//! disk is `std/workflow`'s `data`-gated concern (§2.7); the in-memory context here
//! records `DetEvent`s using ordinary Rust values.
//!
//! tokio is NOT replaced. These seams pin *time/RNG values and recorded ordering*,
//! not *task scheduling*. Bit-for-bit reproducible interleaving of arbitrary
//! concurrent tasks remains the one named 2b residual (spec §3.6).
//!
//! Explicitly EXCLUDED seams (real-OS, intentionally NOT routed through a
//! `DeterminismContext`): CNTR §6's inbound `process.on`/`process.off` signal handlers.
//! An OS signal arrives at a wall-clock-driven, externally-triggered moment that has no
//! meaningful record/replay (re-delivering a real SIGTERM on resume is impossible, and
//! the §6.3 emulated default exits the process). The `workflow-determinism` lint flags a
//! `process.*` call (incl. `process.on`) inside a workflow body so the author drives any
//! needed signal-driven effect through an `activity`/`ctx.call` boundary instead.

/// A FIXED, seed-derived virtual-clock start (ms-epoch) for a pure deterministic
/// run. Using a seed-derived base (NOT the real wall clock) is what makes two
/// same-seed runs byte-identical on `time.now`/`date.now` too (the determinism
/// oracle, spec §3.5). `std/workflow` record mode instead captures the REAL time at
/// record so a later resume replays the same recorded timestamps.
pub fn deterministic_start_ms(seed: u64) -> f64 {
    // A stable base epoch (2023-01-01T00:00:00Z) offset by a small, seed-derived
    // amount so distinct seeds also get distinct (but reproducible) start times.
    const BASE_MS: f64 = 1_672_531_200_000.0;
    BASE_MS + (seed % 1_000_000) as f64
}

/// BATT C1 (§10.1) — the deterministic test configuration carried by `ascript test
/// --seed/--frozen-time` from the CLI into the test runner. Two plain `Send` scalars so it
/// rides the parallel airlock as ordinary fields (no new sendable `Value` kind). When
/// present, the runner installs a FRESH `DeterminismContext::record(seed, start_ms)` at the
/// top of EACH test iteration; when absent (`None`), the path is byte-identical to today's
/// inert default.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetTestConfig {
    /// The RNG seed (also the workflow/clock seed). `--frozen-time` alone defaults this to 0.
    pub seed: u64,
    /// The frozen virtual-clock start, ms-epoch. `--seed` alone defaults this to
    /// `deterministic_start_ms(seed)`.
    pub start_ms: f64,
}

impl DetTestConfig {
    /// Build the per-test config from the (already-parsed) optional CLI inputs, applying
    /// the §10.1 defaults: `--frozen-time` alone → seed 0; `--seed` alone → frozen at
    /// `deterministic_start_ms(seed)`; neither → `None` (the inert default — nothing
    /// changes). Both present → exactly those values.
    pub fn from_cli(seed: Option<u64>, frozen_ms: Option<f64>) -> Option<DetTestConfig> {
        match (seed, frozen_ms) {
            (None, None) => None,
            (Some(seed), Some(start_ms)) => Some(DetTestConfig { seed, start_ms }),
            (Some(seed), None) => Some(DetTestConfig {
                seed,
                start_ms: deterministic_start_ms(seed),
            }),
            (None, Some(start_ms)) => Some(DetTestConfig { seed: 0, start_ms }),
        }
    }
}

/// REPLAY §7 — who installed the determinism context, which sets the replay posture.
///
/// `Workflow` is the SHIPPED SP9 semantics (lenient: replay falls through to Record at
/// stream exhaustion — the crash-point model — and best-effort recovers on a kind
/// mismatch; the `std/workflow` detector is the authoritative guard). Every EXISTING
/// constructor (`record`/`replay`) yields `Workflow` so the workflow path is
/// byte-identical. `CliTrace` is the REPLAY flagship's `ascript run --record/--replay`
/// posture (strict: exhaustion and any kind/signature mismatch are LOUD divergences,
/// never a silent fall-through or recovery — §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `std/workflow` installed the context (the shipped lenient crash-point semantics).
    Workflow,
    /// `ascript run --record/--replay` installed the context (the strict REPLAY posture).
    CliTrace,
}

/// REPLAY §7 — a detected strict-replay divergence. Set on
/// [`DeterminismContext::pending_divergence`] when a strict (`CliTrace`) replay hits an
/// unexpected/exhausted event at a seam; the Interp chokepoint reads it via
/// [`DeterminismContext::take_divergence`] and raises the indexed error (the RAISE is a
/// later REPLAY task — Task 1 only PLUMBS the pending divergence). The `at` field is the
/// event-stream cursor index where the divergence was detected (the §7 error's "event N").
/// Kind names are `&'static str` (`DetEvent` variant names) so `det.rs` stays free of any
/// new dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Divergence {
    /// The recorded stream was exhausted while a seam still wanted an event of kind
    /// `wanted` at cursor index `at`.
    Exhausted {
        at: usize,
        wanted: &'static str,
    },
    /// The event at cursor `at` was of kind `got`, but the seam wanted `expected`. The
    /// recorded value is NEVER returned through the seam — only the kind names cross.
    Mismatch {
        at: usize,
        expected: &'static str,
        got: &'static str,
    },
}

/// Whether the context is recording effects (first run) or replaying them from a
/// previously-recorded stream (resume after a crash).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// First run: each effect executes for real, and its result is appended to
    /// `events`.
    Record,
    /// Resume: each effect returns its recorded result from `events` at `cursor`
    /// WITHOUT executing the side effect, asserting the signature matches.
    Replay,
}

/// A recorded non-deterministic effect. The persisted (`std/workflow`) event log is
/// a serialized projection of this stream; the in-memory form here keeps results as
/// plain primitives (`f64`) or as already-serialized JSON strings (activity results)
/// so the determinism core needs no `Value`/`serde` dependency.
#[derive(Debug, Clone, PartialEq)]
pub enum DetEvent {
    /// A wall/`date.now` clock read returned this ms-epoch value.
    ClockRead { value: f64 },
    /// A `math.random`/seeded draw returned this `[0,1)` value.
    RandomRead { value: f64 },
    /// A seeded BYTE draw (`fill_seeded_bytes` — `uuid.v4`/`uuid.v7` tail/
    /// `crypto.randomBytes`/the `crypto.hashPassword`/`bcryptHash` salts) returned
    /// these exact bytes. Recording the DRAWN BYTES (not just the count) makes replay
    /// faithful regardless of the seed or any RNG-stream interleaving, and a wrong-kind
    /// or wrong-length event at this cursor position surfaces a divergence (Task 0.19c).
    ///
    /// SIZE NOTE: the bytes are stored VERBATIM (the persisted workflow log encodes them
    /// as a JSON number array, ~3 bytes/byte). A 16-byte UUID is trivial, but a large
    /// `crypto.randomBytes(n)` inside a workflow body balloons the log (a 1 MiB draw →
    /// ~3 MB entry; the 16 MiB max → ~56 MB). To draw a large key/blob in a workflow,
    /// do it inside an `activity` so only the DERIVED result — not the raw entropy —
    /// enters the event log.
    BytesRead { bytes: Vec<u8> },
    /// A monotonic clock read returned this ms value.
    MonotonicRead { value: f64 },
    /// A durable timer was set to wake at this ms-epoch.
    TimerSet { wake: f64 },
    /// An activity (`ctx.call`) completed. `result_json` is the activity's return
    /// value serialized via `json::to_json_lossy`; `args_hash` pins the call
    /// signature so a workflow-code change that reorders effects is caught as a
    /// non-determinism error rather than silently replaying a wrong value.
    ActivityCompleted {
        name: String,
        args_hash: u64,
        result_json: String,
    },
    /// Workers Spec B (Task 12): one actor method call crossed the isolate boundary.
    /// `result` is the structured-clone-encoded reply bytes (the same `Vec<u8>` the
    /// actor's `oneshot` carried back). On Replay the recorded bytes are decoded on the
    /// caller thread WITHOUT re-crossing the isolate boundary, so the actor's side
    /// effect runs exactly once (at Record). `panic` carries an uncaught Tier-2 panic
    /// message instead of a result, so a recorded failure replays as the same failure.
    ActorCall {
        method: String,
        result: Vec<u8>,
        panic: Option<String>,
    },
    /// FFI Task 10 (§7): one `sym.call` foreign call crossed the determinism boundary.
    /// `ret` is the marshalled return value (a small primitive — int/float/void; a
    /// POINTER return is refused before this is recorded, §7B). `out_params` is the
    /// post-call snapshot of every `ffi.ptr`-typed **`Bytes`** out-param: `(arg_index,
    /// bytes)`. On Replay the recorded `ret` is returned WITHOUT re-invoking C and each
    /// out-param's bytes are written back into the live `Bytes` (§7A) — so a replayed
    /// workflow is deterministic without re-running native code, AND out-params are
    /// faithful (not stale pre-call bytes). A `ForeignPtr` out-param is non-recordable
    /// and is refused (§7B), so `out_params` only ever holds `Bytes` snapshots.
    FfiCall {
        ret: FfiRet,
        out_params: Vec<(usize, Vec<u8>)>,
    },
    /// Workers Spec B (Task 12): one `worker fn*` resume crossed the isolate boundary.
    /// `value` is the structured-clone-encoded yielded value bytes, or `None` when the
    /// producer finished (the resume returned done). On Replay the recorded outcome is
    /// returned WITHOUT re-driving the producer isolate. `panic` carries a producer
    /// panic message instead of a yield.
    GeneratorYield {
        value: Option<Vec<u8>>,
        panic: Option<String>,
    },
    /// REPLAY §2.3: one effectful stdlib call crossed the result boundary at the
    /// `call_stdlib` chokepoint. `args_hash` pins the call signature (the
    /// `ActivityCompleted` discipline) so a code change that reorders effects is a
    /// detected divergence, never a silently wrong value. `outcome` carries airlock
    /// structured-clone bytes (or a panic/propagate marker) — exact `Value` fidelity,
    /// plain `Send` data, no `Value`/serde dependency in `det.rs`.
    StdlibCall {
        module: String,
        func: String,
        args_hash: u64,
        outcome: TraceOutcome,
    },
    /// REPLAY §2.5: one method call on a VIRTUALIZED native handle (e.g. an
    /// `HttpResponse` accessor). `vid` is the trace-scoped virtual handle id (assigned
    /// at birth, in event order); the method + `args_hash` pin the call.
    NativeCall {
        vid: u32,
        method: String,
        args_hash: u64,
        outcome: TraceOutcome,
    },
}

/// REPLAY §2.3 — the recorded outcome of one effectful stdlib / native-handle call.
/// Plain `Send` data (airlock bytes + a message) so `det.rs` stays free of any
/// `Value`/serde dependency (it builds under `--no-default-features`). The encode/decode
/// of the `Value` ⇄ bytes happens at the Interp chokepoint (a later REPLAY task), never
/// here.
#[derive(Debug, Clone, PartialEq)]
pub enum TraceOutcome {
    /// The airlock-encoded result `Value` (the common case; plain data).
    Value(Vec<u8>),
    /// The call raised a recoverable Tier-2 panic; replay re-raises the same message.
    Panic(String),
    /// The call returned `Control::Propagate(v)` (a `?` early return); `v` is airlock-encoded.
    Propagate(Vec<u8>),
}

/// FFI Task 10 (§7): the marshalled RETURN of a recorded foreign call. A pointer
/// return (`ForeignPtr`) is meaningless across runs and is REFUSED before recording
/// (§7B), so the recordable returns are exactly the small primitives the marshaller
/// produces: an integer (every `iN`/`uN`/`size` result, carried as the i64 bit
/// pattern), a float (`f32`/`f64`), or void (`nil`). Plain Rust types keep `det.rs`
/// free of `Value`/serde (it builds under `--no-default-features`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FfiRet {
    /// An `iN`/`uN`/`size` return — the i64 the marshaller produced.
    Int(i64),
    /// An `f32`/`f64` return.
    Float(f64),
    /// A `void` return (`nil`).
    Void,
}

/// The outcome of one recorded cross-isolate boundary interaction, as it crosses
/// back to the caller thread. Used by the actor / generator record-replay helpers so
/// the determinism core stays free of `Value`/serde (only `Send` bytes + a message).
#[derive(Debug, Clone, PartialEq)]
pub enum BoundaryOutcome {
    /// A successful reply / yield: the structured-clone-encoded bytes.
    Bytes(Vec<u8>),
    /// A `worker fn*` resume that returned "done" (no value). Meaningless for actors.
    Done,
    /// An uncaught Tier-2 panic message raised across the boundary.
    Panic(String),
}

/// A virtual ms-epoch clock. In deterministic mode the engine reads it instead of
/// the wall clock; it advances only on a recorded read (Record) / a durable
/// `ctx.sleep` (which moves it forward by the slept duration).
#[derive(Debug, Clone)]
pub struct VirtualClock {
    /// Current virtual time, ms since the Unix epoch.
    now_ms: f64,
    /// Current virtual monotonic time, ms since context start (starts at 0).
    monotonic_ms: f64,
}

impl VirtualClock {
    /// A clock seeded at `start_ms` (ms-epoch). Monotonic starts at 0.
    pub fn new(start_ms: f64) -> Self {
        VirtualClock {
            now_ms: start_ms,
            monotonic_ms: 0.0,
        }
    }
    pub fn now_ms(&self) -> f64 {
        self.now_ms
    }
    pub fn monotonic_ms(&self) -> f64 {
        self.monotonic_ms
    }
    /// Advance both the wall and monotonic clocks by `delta_ms` (a durable sleep).
    pub fn advance(&mut self, delta_ms: f64) {
        if delta_ms > 0.0 {
            self.now_ms += delta_ms;
            self.monotonic_ms += delta_ms;
        }
    }

    /// Set the wall clock to an absolute `wake` ms-epoch (replay of a recorded
    /// durable timer fast-forwards the clock to the recorded wake time).
    pub fn set_now(&mut self, wake: f64) {
        if wake > self.now_ms {
            self.monotonic_ms += wake - self.now_ms;
        }
        self.now_ms = wake;
    }
}

/// A deterministic xorshift64* PRNG — the SAME algorithm as the thread-local one in
/// `src/stdlib/math.rs`, but seeded from an explicit `u64` (not time+stack-addr) so
/// the same seed always yields the same sequence (spec §3.5).
#[derive(Debug, Clone)]
pub struct SeededRng {
    state: u64,
}

impl SeededRng {
    /// A PRNG seeded from `seed` (a zero seed is normalized to 1, matching the
    /// thread-local seam's non-zero invariant).
    pub fn new(seed: u64) -> Self {
        SeededRng {
            state: seed.max(1),
        }
    }

    /// Advance the state and return the next raw `u64` (xorshift64* core, identical
    /// to `math::next_random`'s transform).
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    /// The next `f64` in `[0, 1)` — byte-identical conversion to the thread-local
    /// `math::next_random` (top 53 bits / 2^53), so deterministic mode and the
    /// default path differ ONLY in the seed source, not the math.
    pub fn next_f64(&mut self) -> f64 {
        let r = self.next_u64();
        (r >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Fill `buf` with deterministic bytes (for `uuid.v4` / `crypto.randomBytes`
    /// under deterministic mode).
    pub fn fill_bytes(&mut self, buf: &mut [u8]) {
        let mut i = 0;
        while i < buf.len() {
            let word = self.next_u64().to_le_bytes();
            let take = (buf.len() - i).min(8);
            buf[i..i + take].copy_from_slice(&word[..take]);
            i += take;
        }
    }
}

/// WARM C (§4.3/§4.4) — hand-rolled CRC32 (IEEE 802.3, reflected polynomial
/// `0xEDB88320`). A ~10-line pure-Rust bitwise implementation chosen DELIBERATELY over
/// a new crate dependency (owner-confirmed): it keeps the build graph clean and adds no
/// dependency-resolution decision. The group appender frames each newline-JSON record
/// with the crc of the record's bytes (sans the crc field) so a torn append (partial
/// write on power loss) is detected at open and truncated away. Determinism: a fixed
/// polynomial + fixed init/xor, identical on every platform — the log is portable.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            // Branchless: `mask` is all-ones iff the low bit is set.
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// WARM C (§4.3) — the group-durability appender. Installed on the
/// [`DeterminismContext`] only when `durability: "group"`; absent (`None`) for the
/// `Fsync`/`Buffered` modes, so [`DeterminismContext::pump`] is an inert no-op and
/// those paths stay BIT-IDENTICAL to pre-WARM (the SP9 inert-when-off pattern).
///
/// Holds an open append-mode `File`, the count of already-persisted events
/// (`persisted` — the cursor `pump` serializes from), and the coalesced-fsync window
/// state (`unsynced` records since the last `sync_all`, and the wall-clock `Instant` of
/// the oldest unsynced record). All writes are SYNCHRONOUS inside the recording call —
/// no borrow is ever held across an `.await` (the write happens exactly where the
/// `Vec::push` happens today). `!Send` is fine (the runtime is per-isolate `!Send`).
pub struct GroupAppender {
    /// The open log file, positioned at end (append).
    file: std::fs::File,
    /// How many `events` have already been serialized + written to `file`.
    persisted: usize,
    /// Records written since the last `sync_all` (the `max_events` coalescing bound).
    unsynced: usize,
    /// Wall-clock instant of the oldest unsynced record (the `window_ms` bound). `None`
    /// when everything is synced. NOTE: this is REAL I/O timing for the fsync window,
    /// NOT the VM determinism clock — `Instant::now` is correct here (the SP9 ban is on
    /// the virtual clock, never on physical I/O latency timing).
    oldest_unsynced: Option<std::time::Instant>,
    /// Coalescing window: fsync if the oldest unsynced record is at least this old.
    window_ms: f64,
    /// Coalescing cap: fsync if at least this many unsynced records have accumulated.
    max_events: usize,
    /// Serialize one `(seq, event)` into a crc-framed newline-JSON record line.
    /// Provided by the `workflow` layer (which owns `event_to_json` + `serde_json`) so
    /// `det.rs` stays serde-free and builds under `--no-default-features`. Returns the
    /// FULL line bytes including the trailing `\n`.
    serialize: fn(usize, &DetEvent) -> Vec<u8>,
}

impl std::fmt::Debug for GroupAppender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupAppender")
            .field("persisted", &self.persisted)
            .field("unsynced", &self.unsynced)
            .field("window_ms", &self.window_ms)
            .field("max_events", &self.max_events)
            .finish()
    }
}

impl GroupAppender {
    /// Build an appender over an already-opened append-mode `file`, having already
    /// persisted `persisted` events (0 for a fresh run; the repaired-prefix count for a
    /// resume). `serialize` frames one `(seq, event)` as a crc'd newline-JSON line.
    pub fn new(
        file: std::fs::File,
        persisted: usize,
        window_ms: f64,
        max_events: usize,
        serialize: fn(usize, &DetEvent) -> Vec<u8>,
    ) -> Self {
        GroupAppender {
            file,
            persisted,
            unsynced: 0,
            oldest_unsynced: None,
            window_ms,
            max_events,
            serialize,
        }
    }

    /// Serialize and write every event in `events[self.persisted..]` as ONE batched
    /// `write_all`, advance `persisted`, then `maybe_fsync`. The `seq` of each record is
    /// its absolute index in `events` (so seq is monotone + contiguous across pumps —
    /// the repair's contiguity check depends on it).
    fn pump_from(&mut self, events: &[DetEvent]) -> std::io::Result<()> {
        use std::io::Write;
        if self.persisted >= events.len() {
            return Ok(());
        }
        let now = std::time::Instant::now();
        let mut batch = Vec::new();
        let mut new_records = 0usize;
        for (seq, ev) in events.iter().enumerate().skip(self.persisted) {
            batch.extend_from_slice(&(self.serialize)(seq, ev));
            new_records += 1;
        }
        self.file.write_all(&batch)?;
        self.persisted = events.len();
        if self.oldest_unsynced.is_none() {
            self.oldest_unsynced = Some(now);
        }
        self.unsynced += new_records;
        self.maybe_fsync()
    }

    /// Coalesced fsync (§4.3): `sync_all` only when at least `max_events` unsynced
    /// records have accumulated OR the oldest unsynced record is at least `window_ms`
    /// old; then reset the window. NOT an unconditional fsync — the window/cap is the
    /// durability contract and the bench win.
    fn maybe_fsync(&mut self) -> std::io::Result<()> {
        let due = self.unsynced >= self.max_events
            || self
                .oldest_unsynced
                .map(|t| t.elapsed().as_secs_f64() * 1000.0 >= self.window_ms)
                .unwrap_or(false);
        if due && self.unsynced > 0 {
            self.file.sync_all()?;
            self.unsynced = 0;
            self.oldest_unsynced = None;
        }
        Ok(())
    }

    /// Finish (§4.3): write the already-crc-framed terminal `WorkflowCompleted` line,
    /// then a final DEADLINE-CHECKED `maybe_fsync` (never forced). The file is closed
    /// when the appender drops.
    fn finish(&mut self, terminal_line: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        if !terminal_line.is_empty() {
            self.file.write_all(terminal_line)?;
            if self.oldest_unsynced.is_none() {
                self.oldest_unsynced = Some(std::time::Instant::now());
            }
            self.unsynced += 1;
        }
        self.maybe_fsync()
    }
}

/// The per-`Interp` deterministic-mode context (spec §3.2). When installed
/// (`Interp.determinism = Some(..)`), the clock/RNG seams route through it; when
/// `None` (default), they take their existing real paths.
#[derive(Debug)]
pub struct DeterminismContext {
    pub mode: Mode,
    /// REPLAY §7 — who installed this context. `Workflow` (the default, every existing
    /// constructor) keeps the shipped lenient crash-point semantics; `CliTrace` is the
    /// strict `ascript run --record/--replay` posture.
    pub origin: Origin,
    /// REPLAY §7 — when `true` (only for `CliTrace`), the seam readers set
    /// [`Self::pending_divergence`] on exhaustion / mismatch instead of falling through
    /// to Record / best-effort recovering. The `Workflow` path is `false` (unchanged).
    pub strict: bool,
    /// REPLAY §7 — the first strict-replay divergence detected at a seam, awaiting the
    /// Interp chokepoint to raise it as the indexed error (Task 1 plumbs it; the RAISE is
    /// a later task). Always `None` for `Workflow` contexts.
    pub pending_divergence: Option<Divergence>,
    pub clock: VirtualClock,
    pub rng: SeededRng,
    pub seed: u64,
    /// Replay cursor into `events`.
    pub cursor: usize,
    /// The in-memory recorded effect stream (the persisted workflow log is a
    /// projection of this).
    pub events: Vec<DetEvent>,
    /// WARM C (§4.3): the group-durability appender. `None` for `Fsync`/`Buffered`
    /// (the inert-when-off path — `record_event`'s `pump` is then a no-op, byte-identical
    /// to pre-WARM). `Some` only under `durability: "group"`, installed by `workflow_run`.
    pub group: Option<GroupAppender>,
}

impl Clone for DeterminismContext {
    /// Clone the determinism state WITHOUT the group appender (an open `File` is not
    /// `Clone`). The appender is never cloned in practice — it is installed on the live
    /// context, drained at finish, and dropped; `Clone` exists only for the pre-WARM
    /// test helpers that snapshot record/replay state, none of which install an appender.
    fn clone(&self) -> Self {
        DeterminismContext {
            mode: self.mode,
            origin: self.origin,
            strict: self.strict,
            pending_divergence: self.pending_divergence.clone(),
            clock: self.clock.clone(),
            rng: self.rng.clone(),
            seed: self.seed,
            cursor: self.cursor,
            events: self.events.clone(),
            group: None,
        }
    }
}

impl DeterminismContext {
    /// A fresh Record-mode context seeded by `seed`, with the virtual clock started
    /// at `start_ms`. The event stream is empty (first run).
    pub fn record(seed: u64, start_ms: f64) -> Self {
        DeterminismContext {
            mode: Mode::Record,
            origin: Origin::Workflow,
            strict: false,
            pending_divergence: None,
            clock: VirtualClock::new(start_ms),
            rng: SeededRng::new(seed),
            seed,
            cursor: 0,
            events: Vec::new(),
            group: None,
        }
    }

    /// A Replay-mode context seeded by `seed`, primed with a previously-recorded
    /// `events` stream. The virtual clock is started at `start_ms` (it is overridden
    /// by the recorded `ClockRead`s as they are consumed). The cursor starts at 0.
    pub fn replay(seed: u64, start_ms: f64, events: Vec<DetEvent>) -> Self {
        DeterminismContext {
            mode: Mode::Replay,
            origin: Origin::Workflow,
            strict: false,
            pending_divergence: None,
            clock: VirtualClock::new(start_ms),
            rng: SeededRng::new(seed),
            seed,
            cursor: 0,
            events,
            group: None,
        }
    }

    /// REPLAY §4.1 — a fresh `CliTrace` Record-mode context (`ascript run --record`).
    /// Strict posture (no fall-through-to-Record at replay), seeded clock/RNG. Identical
    /// to [`Self::record`] except `origin: CliTrace, strict: true`.
    pub fn record_trace(seed: u64, start_ms: f64) -> Self {
        DeterminismContext {
            mode: Mode::Record,
            origin: Origin::CliTrace,
            strict: true,
            pending_divergence: None,
            clock: VirtualClock::new(start_ms),
            rng: SeededRng::new(seed),
            seed,
            cursor: 0,
            events: Vec::new(),
            group: None,
        }
    }

    /// REPLAY §4.1/§7 — a strict `CliTrace` Replay-mode context (`ascript run --replay`).
    /// Exhaustion and kind/signature mismatch set [`Self::pending_divergence`] (LOUD)
    /// instead of the lenient fall-through/recover. Identical to [`Self::replay`] except
    /// `origin: CliTrace, strict: true`.
    pub fn replay_trace(seed: u64, start_ms: f64, events: Vec<DetEvent>) -> Self {
        DeterminismContext {
            mode: Mode::Replay,
            origin: Origin::CliTrace,
            strict: true,
            pending_divergence: None,
            clock: VirtualClock::new(start_ms),
            rng: SeededRng::new(seed),
            seed,
            cursor: 0,
            events,
            group: None,
        }
    }

    /// WARM C (§4.3): install the group-durability appender (called by `workflow_run`
    /// only under `durability: "group"`). After this, every [`Self::record_event`] pumps
    /// new events to disk; absent it, `pump` is an inert no-op.
    pub fn set_group_appender(&mut self, appender: GroupAppender) {
        self.group = Some(appender);
    }

    /// REPLAY §7 — take (and clear) the pending strict-replay divergence, if any. The
    /// Interp chokepoint calls this after a Seamed/Recorded dispatch (only when
    /// `trace_active`) to raise the indexed error. Always `None` for `Workflow` contexts
    /// (they never set a divergence). The RAISE happens at the Interp layer (a later
    /// REPLAY task); Task 1 only plumbs the field.
    pub fn take_divergence(&mut self) -> Option<Divergence> {
        self.pending_divergence.take()
    }

    /// Record the FIRST strict-replay divergence (later ones are kept silent — the first
    /// is the meaningful one; the chokepoint raises it before the next seam runs).
    fn set_divergence(&mut self, d: Divergence) {
        if self.pending_divergence.is_none() {
            self.pending_divergence = Some(d);
        }
    }

    /// REPLAY §0.5 — public helper for an OUT-OF-MODULE seam (the bare `time.sleep` site
    /// in `stdlib/mod.rs`) to record a kind-`Mismatch` divergence at `at`. Mirrors the
    /// in-reader strict arms; no-op-safe if a divergence is already pending.
    pub fn set_divergence_public(&mut self, at: usize, expected: &'static str, got: &'static str) {
        self.set_divergence(Divergence::Mismatch { at, expected, got });
    }

    /// REPLAY §0.5 — public helper to record an `Exhausted` divergence at `at`.
    pub fn set_exhausted_public(&mut self, at: usize, wanted: &'static str) {
        self.set_divergence(Divergence::Exhausted { at, wanted });
    }

    /// REPLAY §0.5 — the `&'static str` variant name of a `DetEvent` (public re-export of
    /// the internal kind-name helper for the out-of-module sleep site).
    pub fn kind_name(ev: &DetEvent) -> &'static str {
        Self::event_kind_name(ev)
    }

    /// WARM C (§4.3) — the SINGLE recording chokepoint. Every `DetEvent` is appended
    /// through here (the 11 det-seam sites + the 3 workflow sites + `stdlib/mod.rs`),
    /// so the group appender has exactly one place to observe a new record. For the
    /// `Fsync`/`Buffered` modes (no appender installed) this is `push` + an inert `pump`
    /// — byte-identical to the pre-WARM `events.push`.
    pub fn record_event(&mut self, ev: DetEvent) -> std::io::Result<()> {
        self.events.push(ev);
        self.pump()
    }

    /// WARM C (§4.3): persist any not-yet-written events to the group log (one
    /// `write_all` for the whole new batch), then maybe-fsync per the coalescing policy.
    /// A no-op when no appender is installed (`Fsync`/`Buffered`). A write/fsync I/O
    /// error is surfaced to the caller's recording site (it does NOT silently lie about
    /// durability — §4.5). All synchronous: no borrow spans an `.await`.
    pub fn pump(&mut self) -> std::io::Result<()> {
        // Split the borrow: take the appender out so we can read `events` immutably
        // while writing through the appender, then put it back.
        let Some(mut appender) = self.group.take() else {
            return Ok(());
        };
        let res = appender.pump_from(&self.events);
        self.group = Some(appender);
        res
    }

    /// WARM C (§4.3): the finish-time flush for the group path. Append the terminal
    /// record bytes (already crc-framed by the caller — `WorkflowCompleted`, no seq),
    /// then a final `maybe_fsync` (DEADLINE-CHECKED, never an unconditional fsync — that
    /// would reinstate the per-commit `F_FULLFSYNC` and forfeit the bench win, §4.3).
    /// Drops the appender (closes the file). A no-op when no appender is installed.
    pub fn finish_group(&mut self, terminal_line: &[u8]) -> std::io::Result<()> {
        let Some(mut appender) = self.group.take() else {
            return Ok(());
        };
        let res = appender.finish(terminal_line);
        // Drop the appender (the workflow is done) — do NOT put it back.
        res
    }

    /// Whether a group appender is installed (the group durability path is active).
    pub fn has_group_appender(&self) -> bool {
        self.group.is_some()
    }

    /// Read the wall clock (ms-epoch). Record: draw the virtual clock value, append a
    /// `ClockRead`. Replay: return the next recorded `ClockRead` at `cursor`; if the
    /// stream is exhausted (the crash point), fall through to Record from here.
    pub fn clock_now_ms(&mut self) -> f64 {
        match self.mode {
            Mode::Record => {
                let v = self.clock.now_ms();
                let _ = self.record_event(DetEvent::ClockRead { value: v });
                v
            }
            // REPLAY §7 — strict (CliTrace): a wrong-kind / exhausted event sets a
            // pending divergence (LOUD, raised at the Interp chokepoint) and returns the
            // best-effort virtual-clock value WITHOUT consuming the cursor on a mismatch
            // or switching to Record on exhaustion. The recorded value never leaks.
            Mode::Replay if self.strict => match self.peek_event() {
                Some(DetEvent::ClockRead { value }) => {
                    self.cursor += 1;
                    self.clock.now_ms = value;
                    value
                }
                Some(other) => {
                    let got = Self::event_kind_name(&other);
                    self.set_divergence(Divergence::Mismatch {
                        at: self.cursor,
                        expected: "ClockRead",
                        got,
                    });
                    self.clock.now_ms()
                }
                None => {
                    self.set_divergence(Divergence::Exhausted {
                        at: self.cursor,
                        wanted: "ClockRead",
                    });
                    self.clock.now_ms()
                }
            },
            Mode::Replay => match self.next_event() {
                Some(DetEvent::ClockRead { value }) => {
                    self.clock.now_ms = value;
                    value
                }
                Some(other) => {
                    // Signature mismatch → caller turns this into the non-determinism
                    // panic; we return the clock value to keep going only if it WAS a
                    // clock event. A different kind is a replay mismatch.
                    self.replay_mismatch_recover(other)
                }
                None => self.switch_to_record_clock(),
            },
        }
    }

    /// Read the monotonic clock (ms). Same record/replay discipline as
    /// [`Self::clock_now_ms`] but over `MonotonicRead`.
    pub fn clock_monotonic_ms(&mut self) -> f64 {
        match self.mode {
            Mode::Record => {
                let v = self.clock.monotonic_ms();
                let _ = self.record_event(DetEvent::MonotonicRead { value: v });
                v
            }
            // REPLAY §7 — strict (CliTrace): mismatch/exhaustion → pending divergence +
            // best-effort monotonic value, cursor unmoved on a mismatch.
            Mode::Replay if self.strict => match self.peek_event() {
                Some(DetEvent::MonotonicRead { value }) => {
                    self.cursor += 1;
                    self.clock.monotonic_ms = value;
                    value
                }
                Some(other) => {
                    let got = Self::event_kind_name(&other);
                    self.set_divergence(Divergence::Mismatch {
                        at: self.cursor,
                        expected: "MonotonicRead",
                        got,
                    });
                    self.clock.monotonic_ms()
                }
                None => {
                    self.set_divergence(Divergence::Exhausted {
                        at: self.cursor,
                        wanted: "MonotonicRead",
                    });
                    self.clock.monotonic_ms()
                }
            },
            Mode::Replay => match self.next_event() {
                Some(DetEvent::MonotonicRead { value }) => {
                    self.clock.monotonic_ms = value;
                    value
                }
                // A non-`MonotonicRead` event is a replay mismatch — surface it via
                // `replay_mismatch_recover` (LOUD divergence) rather than silently
                // cross-consuming a `ClockRead` as a monotonic value or bypassing replay.
                // NOTE: `replay_mismatch_recover` returns wall-clock ms — a different unit
                // than a monotonic reader expects. That's acceptable here because the
                // std/workflow non-determinism detector is the authoritative guard (this
                // value is a best-effort keep-going, not a basis for elapsed-time math).
                Some(other) => self.replay_mismatch_recover(other),
                None => {
                    self.mode = Mode::Record;
                    let v = self.clock.monotonic_ms();
                    let _ = self.record_event(DetEvent::MonotonicRead { value: v });
                    v
                }
            },
        }
    }

    /// Draw the next seeded `[0,1)` value. Record: draw + append `RandomRead`.
    /// Replay: return the recorded `RandomRead` at `cursor`, falling through to
    /// Record when the stream is exhausted.
    pub fn next_random_f64(&mut self) -> f64 {
        match self.mode {
            Mode::Record => {
                let v = self.rng.next_f64();
                let _ = self.record_event(DetEvent::RandomRead { value: v });
                v
            }
            // REPLAY §7 — strict (CliTrace): mismatch/exhaustion → pending divergence.
            // Best-effort returns the current virtual-clock value (a keep-going sentinel,
            // NOT a fresh PRNG draw — drawing would shift the stream out of phase and the
            // recorded value must not leak). Cursor unmoved on a mismatch.
            Mode::Replay if self.strict => match self.peek_event() {
                Some(DetEvent::RandomRead { value }) => {
                    self.cursor += 1;
                    value
                }
                Some(other) => {
                    let got = Self::event_kind_name(&other);
                    self.set_divergence(Divergence::Mismatch {
                        at: self.cursor,
                        expected: "RandomRead",
                        got,
                    });
                    self.clock.now_ms()
                }
                None => {
                    self.set_divergence(Divergence::Exhausted {
                        at: self.cursor,
                        wanted: "RandomRead",
                    });
                    self.clock.now_ms()
                }
            },
            Mode::Replay => match self.next_event() {
                Some(DetEvent::RandomRead { value }) => value,
                // A non-`RandomRead` event is a replay mismatch — surface it via
                // `replay_mismatch_recover` (LOUD divergence) rather than silently
                // bypassing replay by drawing a fresh PRNG value, which would shift the
                // whole replay one event out of phase.
                Some(other) => self.replay_mismatch_recover(other),
                None => {
                    self.mode = Mode::Record;
                    let v = self.rng.next_f64();
                    let _ = self.record_event(DetEvent::RandomRead { value: v });
                    v
                }
            },
        }
    }

    /// Task 0.19c: draw `buf.len()` seeded bytes into `buf`, event-sourced so a replay
    /// is FAITHFUL (returns the exact recorded bytes, independent of the seed) AND a
    /// desync is DETECTED. Symmetric with [`Self::next_random_f64`]:
    /// - Record: draw from the seeded PRNG, append a `BytesRead` of the drawn bytes.
    /// - Replay: consume the next event; a `BytesRead` of the RIGHT LENGTH → copy into
    ///   `buf`; a wrong-kind OR wrong-length event is a replay mismatch → surface the
    ///   divergence (`replay_mismatch_recover_bytes`, the byte-path sibling of
    ///   `replay_mismatch_recover`) and fill `buf` from the PRNG as a best-effort
    ///   keep-going (the std/workflow detector is the authoritative guard).
    /// - Exhaustion (None): fall through to Record (draw + append), matching the other
    ///   readers' crash-point behavior.
    pub fn next_seeded_bytes(&mut self, buf: &mut [u8]) {
        match self.mode {
            Mode::Record => {
                self.rng.fill_bytes(buf);
                let _ = self.record_event(DetEvent::BytesRead {
                    bytes: buf.to_vec(),
                });
            }
            // REPLAY §7 — strict (CliTrace): a wrong-kind OR wrong-length event sets a
            // pending divergence; `buf` is filled from the seeded PRNG as a best-effort
            // keep-going (the recorded value never leaks). Cursor unmoved on a mismatch.
            Mode::Replay if self.strict => match self.peek_event() {
                Some(DetEvent::BytesRead { bytes }) if bytes.len() == buf.len() => {
                    self.cursor += 1;
                    buf.copy_from_slice(&bytes);
                }
                Some(other) => {
                    let got = Self::event_kind_name(&other);
                    self.set_divergence(Divergence::Mismatch {
                        at: self.cursor,
                        expected: "BytesRead",
                        got,
                    });
                    self.rng.fill_bytes(buf);
                }
                None => {
                    self.set_divergence(Divergence::Exhausted {
                        at: self.cursor,
                        wanted: "BytesRead",
                    });
                    self.rng.fill_bytes(buf);
                }
            },
            Mode::Replay => match self.next_event() {
                Some(DetEvent::BytesRead { bytes }) if bytes.len() == buf.len() => {
                    buf.copy_from_slice(&bytes);
                }
                // A wrong-kind OR wrong-length event is a replay mismatch — surface it
                // (LOUD divergence) rather than silently drawing fresh PRNG bytes (which
                // would shift the whole replay one event out of phase) or copying a
                // mismatched-length recorded value.
                Some(other) => self.replay_mismatch_recover_bytes(other, buf),
                None => {
                    self.mode = Mode::Record;
                    self.rng.fill_bytes(buf);
                    let _ = self.record_event(DetEvent::BytesRead {
                        bytes: buf.to_vec(),
                    });
                }
            },
        }
    }

    /// Workers Spec B (Task 12): record one actor method-call boundary result.
    /// Record-mode ONLY (the caller checks `mode` first and crosses the isolate
    /// boundary for real before calling this); appends an [`DetEvent::ActorCall`].
    pub fn record_actor_call(&mut self, method: &str, outcome: &BoundaryOutcome) {
        let (result, panic) = match outcome {
            BoundaryOutcome::Bytes(b) => (b.clone(), None),
            // An actor call has no "done" outcome; treat it as empty bytes defensively.
            BoundaryOutcome::Done => (Vec::new(), None),
            BoundaryOutcome::Panic(msg) => (Vec::new(), Some(msg.clone())),
        };
        let _ = self.record_event(DetEvent::ActorCall {
            method: method.to_string(),
            result,
            panic,
        });
    }

    /// Workers Spec B (Task 12): replay one actor method-call boundary result.
    /// Returns the recorded outcome WITHOUT crossing the isolate boundary, or `None`
    /// when the recorded stream is exhausted (the caller then falls through to a real
    /// boundary crossing + a Record append, mirroring the clock/RNG seams). A recorded
    /// event of a different kind — OR a method-name mismatch — is a replay mismatch →
    /// `None` (fall through without consuming the event), detecting a divergent replay
    /// (different method order) at the earliest possible point. The cursor is left
    /// unmoved so the fall-through path can re-record from the correct position.
    pub fn replay_actor_call(&mut self, method: &str) -> Option<BoundaryOutcome> {
        match self.peek_event() {
            Some(DetEvent::ActorCall { method: recorded_method, result, panic }) => {
                if recorded_method != method {
                    // Method-name mismatch: the caller is replaying a different method
                    // than was recorded at this cursor position. Signal divergence by
                    // returning None (cursor stays unmoved — fall through to real call +
                    // Record append).
                    return None;
                }
                self.cursor += 1;
                Some(match panic {
                    Some(msg) => BoundaryOutcome::Panic(msg),
                    None => BoundaryOutcome::Bytes(result),
                })
            }
            _ => None,
        }
    }

    /// Workers Spec B (Task 12): record one `worker fn*` resume boundary outcome.
    /// Record-mode ONLY; appends a [`DetEvent::GeneratorYield`].
    pub fn record_generator_yield(&mut self, outcome: &BoundaryOutcome) {
        let (value, panic) = match outcome {
            BoundaryOutcome::Bytes(b) => (Some(b.clone()), None),
            BoundaryOutcome::Done => (None, None),
            BoundaryOutcome::Panic(msg) => (None, Some(msg.clone())),
        };
        let _ = self.record_event(DetEvent::GeneratorYield { value, panic });
    }

    /// Workers Spec B (Task 12): replay one `worker fn*` resume boundary outcome.
    /// Returns the recorded yield / done / panic WITHOUT re-driving the producer
    /// isolate, or `None` when the stream is exhausted (fall through to a real resume +
    /// a Record append). A different recorded kind → `None` (fall through).
    pub fn replay_generator_yield(&mut self) -> Option<BoundaryOutcome> {
        match self.peek_event() {
            Some(DetEvent::GeneratorYield { value, panic }) => {
                self.cursor += 1;
                Some(match (value, panic) {
                    (_, Some(msg)) => BoundaryOutcome::Panic(msg),
                    (Some(b), None) => BoundaryOutcome::Bytes(b),
                    (None, None) => BoundaryOutcome::Done,
                })
            }
            _ => None,
        }
    }

    /// FFI Task 10 (§7A): record one foreign-call boundary. Record-mode ONLY (the
    /// caller checks `mode` first and invokes the C side for real before calling this).
    /// Appends a [`DetEvent::FfiCall`] carrying the marshalled return + the post-call
    /// snapshot of every `Bytes` out-param.
    pub fn record_ffi_call(&mut self, ret: FfiRet, out_params: Vec<(usize, Vec<u8>)>) {
        let _ = self.record_event(DetEvent::FfiCall { ret, out_params });
    }

    /// FFI Task 10 (§7A): replay one foreign-call boundary. Returns the recorded
    /// `(ret, out_params)` WITHOUT re-invoking the C side, or `None` when the stream is
    /// exhausted (the caller then falls through to a real call + a Record append,
    /// mirroring the clock/RNG/actor seams). A recorded event of a different kind →
    /// `None` (cursor unmoved), detecting a divergent replay at the earliest point. The
    /// caller writes each out-param's recorded bytes back into the live `Bytes`.
    #[allow(clippy::type_complexity)]
    pub fn replay_ffi_call(&mut self) -> Option<(FfiRet, Vec<(usize, Vec<u8>)>)> {
        match self.peek_event() {
            Some(DetEvent::FfiCall { ret, out_params }) => {
                self.cursor += 1;
                Some((ret, out_params))
            }
            _ => None,
        }
    }

    /// REPLAY §2.3 — record one effectful stdlib call at the result boundary. Appends a
    /// [`DetEvent::StdlibCall`] carrying the call signature (`module`/`func`/`args_hash`)
    /// and the encoded `outcome`. Record-mode only (the caller dispatched the real call
    /// first).
    pub fn record_stdlib_call(
        &mut self,
        module: &str,
        func: &str,
        args_hash: u64,
        outcome: TraceOutcome,
    ) {
        let _ = self.record_event(DetEvent::StdlibCall {
            module: module.to_string(),
            func: func.to_string(),
            args_hash,
            outcome,
        });
    }

    /// REPLAY §2.3/§7 — replay one effectful stdlib call. Returns the recorded outcome
    /// WITHOUT dispatching, or `None` on a divergence — a wrong `(module, func, args_hash)`
    /// at the cursor, an exhausted stream, or a wrong event kind. On a divergence the
    /// cursor is left UNMOVED (peek-don't-consume, the FFI/actor discipline) and, for a
    /// strict (`CliTrace`) context, [`Self::pending_divergence`] is set so the Interp
    /// chokepoint raises the indexed error. A `Workflow`-origin context never sets a
    /// divergence (it has no use for `StdlibCall` today, but the discipline is uniform).
    pub fn replay_stdlib_call(
        &mut self,
        module: &str,
        func: &str,
        args_hash: u64,
    ) -> Option<TraceOutcome> {
        match self.peek_event() {
            Some(DetEvent::StdlibCall {
                module: rmod,
                func: rfunc,
                args_hash: rhash,
                outcome,
            }) => {
                if rmod == module && rfunc == func && rhash == args_hash {
                    self.cursor += 1;
                    return Some(outcome);
                }
                // Signature mismatch — peek-don't-consume + (strict) record divergence.
                if self.strict {
                    self.set_divergence(Divergence::Mismatch {
                        at: self.cursor,
                        expected: "StdlibCall",
                        got: "StdlibCall",
                    });
                }
                None
            }
            Some(other) => {
                if self.strict {
                    let got = Self::event_kind_name(&other);
                    self.set_divergence(Divergence::Mismatch {
                        at: self.cursor,
                        expected: "StdlibCall",
                        got,
                    });
                }
                None
            }
            None => {
                if self.strict {
                    self.set_divergence(Divergence::Exhausted {
                        at: self.cursor,
                        wanted: "StdlibCall",
                    });
                }
                None
            }
        }
    }

    /// REPLAY §2.5 — record one method call on a virtualized native handle. Appends a
    /// [`DetEvent::NativeCall`] pinning the `vid` + `method` + `args_hash`. Record-mode only.
    pub fn record_native_call(
        &mut self,
        vid: u32,
        method: &str,
        args_hash: u64,
        outcome: TraceOutcome,
    ) {
        let _ = self.record_event(DetEvent::NativeCall {
            vid,
            method: method.to_string(),
            args_hash,
            outcome,
        });
    }

    /// REPLAY §2.5/§7 — replay one virtual-handle method call. Verifies the `vid` AND the
    /// `method` (and `args_hash`); a mismatch / exhaustion / wrong kind → `None` with the
    /// cursor unmoved (peek-don't-consume) and, when strict, a pending divergence.
    pub fn replay_native_call(
        &mut self,
        vid: u32,
        method: &str,
        args_hash: u64,
    ) -> Option<TraceOutcome> {
        match self.peek_event() {
            Some(DetEvent::NativeCall {
                vid: rvid,
                method: rmethod,
                args_hash: rhash,
                outcome,
            }) => {
                if rvid == vid && rmethod == method && rhash == args_hash {
                    self.cursor += 1;
                    return Some(outcome);
                }
                if self.strict {
                    self.set_divergence(Divergence::Mismatch {
                        at: self.cursor,
                        expected: "NativeCall",
                        got: "NativeCall",
                    });
                }
                None
            }
            Some(other) => {
                if self.strict {
                    let got = Self::event_kind_name(&other);
                    self.set_divergence(Divergence::Mismatch {
                        at: self.cursor,
                        expected: "NativeCall",
                        got,
                    });
                }
                None
            }
            None => {
                if self.strict {
                    self.set_divergence(Divergence::Exhausted {
                        at: self.cursor,
                        wanted: "NativeCall",
                    });
                }
                None
            }
        }
    }

    /// Peek the event at `cursor` WITHOUT advancing (the boundary replay helpers only
    /// advance the cursor when the event kind matches, so a mismatch leaves the cursor
    /// in place for the fall-through path to re-record).
    fn peek_event(&self) -> Option<DetEvent> {
        self.events.get(self.cursor).cloned()
    }

    /// The `&'static str` variant name of a `DetEvent` — used to build the
    /// expected/got kind names in a [`Divergence`] (no recorded value crosses).
    fn event_kind_name(ev: &DetEvent) -> &'static str {
        match ev {
            DetEvent::ClockRead { .. } => "ClockRead",
            DetEvent::RandomRead { .. } => "RandomRead",
            DetEvent::BytesRead { .. } => "BytesRead",
            DetEvent::MonotonicRead { .. } => "MonotonicRead",
            DetEvent::TimerSet { .. } => "TimerSet",
            DetEvent::ActivityCompleted { .. } => "ActivityCompleted",
            DetEvent::ActorCall { .. } => "ActorCall",
            DetEvent::FfiCall { .. } => "FfiCall",
            DetEvent::GeneratorYield { .. } => "GeneratorYield",
            DetEvent::StdlibCall { .. } => "StdlibCall",
            DetEvent::NativeCall { .. } => "NativeCall",
        }
    }

    /// Advance the cursor and return the event it pointed at (Replay helper).
    fn next_event(&mut self) -> Option<DetEvent> {
        let e = self.events.get(self.cursor).cloned();
        if e.is_some() {
            self.cursor += 1;
        }
        e
    }

    /// Any seam reader (clock / RNG) hit an unexpected recorded event kind during
    /// replay — return the current virtual wall-clock value as a best-effort (the
    /// workflow-level detector in `std/workflow` is the authoritative non-determinism
    /// guard for activities; bare clock/RNG seams outside a workflow never reach this in
    /// practice). Callers in non-wall-clock units (monotonic / RNG) treat this purely as
    /// a keep-going sentinel, not a unit-meaningful value.
    fn replay_mismatch_recover(&mut self, _got: DetEvent) -> f64 {
        self.clock.now_ms()
    }

    /// The byte-path sibling of [`Self::replay_mismatch_recover`] (Task 0.19c). A
    /// `next_seeded_bytes` replay hit an unexpected event kind (or a `BytesRead` of the
    /// wrong length). The recorded value must NOT leak through (a wrong-kind value is
    /// meaningless as bytes, and a wrong-length one cannot fill `buf`), so we fill `buf`
    /// from the seeded PRNG as a best-effort keep-going — the std/workflow non-determinism
    /// detector is the authoritative guard that turns the divergence into a clean error.
    fn replay_mismatch_recover_bytes(&mut self, _got: DetEvent, buf: &mut [u8]) {
        self.rng.fill_bytes(buf);
    }

    /// Replay exhausted on a clock read → switch to Record and record from here.
    fn switch_to_record_clock(&mut self) -> f64 {
        self.mode = Mode::Record;
        let v = self.clock.now_ms();
        let _ = self.record_event(DetEvent::ClockRead { value: v });
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // REPLAY Task 1 (spec §2.3/§7/§0.5) — Origin/strict, StdlibCall/NativeCall
    // events + TraceOutcome, divergence plumbing, and the bare-sleep fix support.
    // -----------------------------------------------------------------------

    /// The EXISTING constructors (`record`/`replay`) keep workflow semantics:
    /// `Origin::Workflow`, `strict: false`, no pending divergence. Exhaustion still
    /// falls through to Record; a kind mismatch still best-effort recovers (the
    /// existing tests pin the recover/fall-through behavior — here we add only the
    /// new-field assertions, proving the workflow path is byte-identical).
    #[test]
    fn origin_defaults_keep_workflow_semantics() {
        let rec = DeterminismContext::record(1, 0.0);
        assert_eq!(rec.origin, Origin::Workflow);
        assert!(!rec.strict, "workflow record must NOT be strict");
        assert!(rec.pending_divergence.is_none());

        let rep = DeterminismContext::replay(1, 0.0, vec![]);
        assert_eq!(rep.origin, Origin::Workflow);
        assert!(!rep.strict);

        // Exhaustion under a NON-strict (Workflow) replay still falls through to Record.
        let mut rep = DeterminismContext::replay(5, 0.0, vec![]);
        let _ = rep.clock_now_ms();
        assert_eq!(rep.mode, Mode::Record, "workflow exhaustion falls through to Record");
        assert!(
            rep.take_divergence().is_none(),
            "a Workflow-origin replay must never set a divergence"
        );
    }

    /// A strict `CliTrace` Replay with an EMPTY stream: `clock_now_ms` does NOT switch
    /// to Record; it sets a `Divergence::Exhausted { at: 0, wanted: "ClockRead" }` and
    /// returns the best-effort (current virtual clock) value.
    #[test]
    fn strict_replay_exhaustion_sets_divergence() {
        let mut rep = DeterminismContext::replay_trace(7, 1234.0, vec![]);
        assert_eq!(rep.origin, Origin::CliTrace);
        assert!(rep.strict);
        let v = rep.clock_now_ms();
        assert_eq!(rep.mode, Mode::Replay, "strict replay must NOT fall through to Record");
        assert_eq!(v, 1234.0, "best-effort returns the current virtual clock value");
        assert_eq!(
            rep.take_divergence(),
            Some(Divergence::Exhausted { at: 0, wanted: "ClockRead" }),
            "exhaustion under strict replay sets an indexed Exhausted divergence"
        );
        // take_divergence consumes it.
        assert!(rep.take_divergence().is_none(), "take_divergence clears the pending divergence");
    }

    /// A strict `CliTrace` Replay where the cursor holds a `RandomRead` but the caller
    /// reads the clock: `Divergence::Mismatch { at: 0, expected, got }`; the recorded
    /// value does NOT leak through (the best-effort virtual-clock value is returned).
    #[test]
    fn strict_replay_kind_mismatch_sets_divergence_with_expected_got() {
        let events = vec![DetEvent::RandomRead { value: 0.42 }];
        let mut rep = DeterminismContext::replay_trace(0, 88.0, events);
        let v = rep.clock_now_ms();
        assert_ne!(v, 0.42, "the recorded RandomRead value must NOT leak as a clock value");
        assert_eq!(v, 88.0, "best-effort returns the current virtual clock");
        assert_eq!(
            rep.take_divergence(),
            Some(Divergence::Mismatch {
                at: 0,
                expected: "ClockRead",
                got: "RandomRead",
            }),
            "a wrong-kind event under strict replay sets a Mismatch divergence carrying \
             expected/got kind names"
        );
    }

    /// Record two distinct `StdlibCall`s and replay them in order → identical outcomes.
    /// A wrong func at the cursor → divergence with the cursor UNMOVED (peek-don't-consume);
    /// a wrong args_hash → divergence.
    #[test]
    fn stdlib_call_record_then_replay_round_trips() {
        let mut rec = DeterminismContext::record_trace(1, 0.0);
        rec.record_stdlib_call("fs", "read", 0x11, TraceOutcome::Value(vec![1, 2, 3]));
        rec.record_stdlib_call("env", "get", 0x22, TraceOutcome::Value(vec![9]));
        let events = rec.events.clone();

        let mut rep = DeterminismContext::replay_trace(1, 0.0, events.clone());
        assert_eq!(
            rep.replay_stdlib_call("fs", "read", 0x11),
            Some(TraceOutcome::Value(vec![1, 2, 3])),
        );
        assert_eq!(
            rep.replay_stdlib_call("env", "get", 0x22),
            Some(TraceOutcome::Value(vec![9])),
        );

        // Wrong FUNC at the cursor → divergence, cursor unmoved (peek-don't-consume).
        let mut rep = DeterminismContext::replay_trace(1, 0.0, events.clone());
        let out = rep.replay_stdlib_call("fs", "write", 0x11);
        assert!(out.is_none(), "a wrong func must NOT return a recorded outcome");
        assert_eq!(rep.cursor, 0, "cursor must be unmoved on a func mismatch");
        assert!(matches!(rep.take_divergence(), Some(Divergence::Mismatch { at: 0, .. })));

        // Wrong ARGS_HASH at the cursor → divergence.
        let mut rep = DeterminismContext::replay_trace(1, 0.0, events);
        let out = rep.replay_stdlib_call("fs", "read", 0xDEAD);
        assert!(out.is_none(), "a wrong args_hash must NOT return a recorded outcome");
        assert_eq!(rep.cursor, 0, "cursor must be unmoved on an args_hash mismatch");
        assert!(matches!(rep.take_divergence(), Some(Divergence::Mismatch { at: 0, .. })));
    }

    /// `Panic` and `Propagate` outcomes round-trip through record → replay.
    #[test]
    fn stdlib_call_panic_and_propagate_outcomes_replay() {
        let mut rec = DeterminismContext::record_trace(2, 0.0);
        rec.record_stdlib_call("process", "run", 0x1, TraceOutcome::Panic("boom".into()));
        rec.record_stdlib_call("fs", "read", 0x2, TraceOutcome::Propagate(vec![7, 7]));
        let events = rec.events.clone();

        let mut rep = DeterminismContext::replay_trace(2, 0.0, events);
        assert_eq!(
            rep.replay_stdlib_call("process", "run", 0x1),
            Some(TraceOutcome::Panic("boom".into())),
        );
        assert_eq!(
            rep.replay_stdlib_call("fs", "read", 0x2),
            Some(TraceOutcome::Propagate(vec![7, 7])),
        );
    }

    /// `NativeCall` replay verifies BOTH the vid AND the method name.
    #[test]
    fn native_call_vid_and_method_pinned() {
        let mut rec = DeterminismContext::record_trace(3, 0.0);
        rec.record_native_call(0, "json", 0xAA, TraceOutcome::Value(vec![1]));
        rec.record_native_call(1, "text", 0xBB, TraceOutcome::Value(vec![2]));
        let events = rec.events.clone();

        // Correct (vid, method) in order → outcomes.
        let mut rep = DeterminismContext::replay_trace(3, 0.0, events.clone());
        assert_eq!(
            rep.replay_native_call(0, "json", 0xAA),
            Some(TraceOutcome::Value(vec![1])),
        );
        assert_eq!(
            rep.replay_native_call(1, "text", 0xBB),
            Some(TraceOutcome::Value(vec![2])),
        );

        // Wrong vid at the cursor → divergence (cursor unmoved).
        let mut rep = DeterminismContext::replay_trace(3, 0.0, events.clone());
        assert!(rep.replay_native_call(9, "json", 0xAA).is_none(), "wrong vid → None");
        assert_eq!(rep.cursor, 0, "cursor unmoved on a vid mismatch");
        assert!(matches!(rep.take_divergence(), Some(Divergence::Mismatch { at: 0, .. })));

        // Wrong method at the cursor → divergence.
        let mut rep = DeterminismContext::replay_trace(3, 0.0, events);
        assert!(rep.replay_native_call(0, "bytes", 0xAA).is_none(), "wrong method → None");
        assert_eq!(rep.cursor, 0, "cursor unmoved on a method mismatch");
        assert!(matches!(rep.take_divergence(), Some(Divergence::Mismatch { at: 0, .. })));
    }

    #[test]
    fn seeded_rng_reproducible_for_fixed_seed() {
        let mut a = SeededRng::new(42);
        let mut b = SeededRng::new(42);
        let seq_a: Vec<f64> = (0..16).map(|_| a.next_f64()).collect();
        let seq_b: Vec<f64> = (0..16).map(|_| b.next_f64()).collect();
        assert_eq!(seq_a, seq_b, "same seed must yield the same sequence");
        // All in [0, 1).
        assert!(seq_a.iter().all(|&x| (0.0..1.0).contains(&x)));
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = SeededRng::new(1);
        let mut b = SeededRng::new(2);
        // Overwhelmingly likely to differ within a few draws.
        let differ = (0..8).any(|_| a.next_f64() != b.next_f64());
        assert!(differ, "distinct seeds should produce distinct sequences");
    }

    #[test]
    fn fill_bytes_is_deterministic() {
        let mut a = SeededRng::new(7);
        let mut b = SeededRng::new(7);
        let mut ba = [0u8; 16];
        let mut bb = [0u8; 16];
        a.fill_bytes(&mut ba);
        b.fill_bytes(&mut bb);
        assert_eq!(ba, bb);
    }

    #[test]
    fn record_then_replay_reproduces_values() {
        // Record a clock + two random reads.
        let mut rec = DeterminismContext::record(99, 1_000_000.0);
        let c0 = rec.clock_now_ms();
        let r0 = rec.next_random_f64();
        let r1 = rec.next_random_f64();
        let events = rec.events.clone();

        // Replay against the recorded stream → identical values, no real effect.
        let mut rep = DeterminismContext::replay(99, 0.0, events);
        assert_eq!(rep.clock_now_ms(), c0);
        assert_eq!(rep.next_random_f64(), r0);
        assert_eq!(rep.next_random_f64(), r1);
    }

    #[test]
    fn record_then_replay_actor_message_sequence() {
        // Record a sequence of three actor calls (the encoded replies are opaque
        // `Send` bytes — here just stand-in payloads).
        let mut rec = DeterminismContext::record(7, 0.0);
        rec.record_actor_call("inc", &BoundaryOutcome::Bytes(vec![1]));
        rec.record_actor_call("inc", &BoundaryOutcome::Bytes(vec![2]));
        rec.record_actor_call("get", &BoundaryOutcome::Bytes(vec![2]));
        let events = rec.events.clone();

        // Replay returns the recorded results IN ORDER without re-crossing the boundary.
        let mut rep = DeterminismContext::replay(7, 0.0, events);
        assert_eq!(
            rep.replay_actor_call("inc"),
            Some(BoundaryOutcome::Bytes(vec![1]))
        );
        assert_eq!(
            rep.replay_actor_call("inc"),
            Some(BoundaryOutcome::Bytes(vec![2]))
        );
        assert_eq!(
            rep.replay_actor_call("get"),
            Some(BoundaryOutcome::Bytes(vec![2]))
        );
        // Exhausted → None (caller falls through to a real boundary crossing).
        assert_eq!(rep.replay_actor_call("get"), None);
    }

    #[test]
    fn record_then_replay_actor_panic() {
        let mut rec = DeterminismContext::record(1, 0.0);
        rec.record_actor_call("boom", &BoundaryOutcome::Panic("kaboom".to_string()));
        let events = rec.events.clone();
        let mut rep = DeterminismContext::replay(1, 0.0, events);
        assert_eq!(
            rep.replay_actor_call("boom"),
            Some(BoundaryOutcome::Panic("kaboom".to_string()))
        );
    }

    #[test]
    fn record_then_replay_generator_yield_sequence() {
        // Record a yield sequence ending in Done.
        let mut rec = DeterminismContext::record(3, 0.0);
        rec.record_generator_yield(&BoundaryOutcome::Bytes(vec![10]));
        rec.record_generator_yield(&BoundaryOutcome::Bytes(vec![20]));
        rec.record_generator_yield(&BoundaryOutcome::Bytes(vec![30]));
        rec.record_generator_yield(&BoundaryOutcome::Done);
        let events = rec.events.clone();

        // Replay reproduces the same yields then Done, no producer re-run.
        let mut rep = DeterminismContext::replay(3, 0.0, events);
        assert_eq!(
            rep.replay_generator_yield(),
            Some(BoundaryOutcome::Bytes(vec![10]))
        );
        assert_eq!(
            rep.replay_generator_yield(),
            Some(BoundaryOutcome::Bytes(vec![20]))
        );
        assert_eq!(
            rep.replay_generator_yield(),
            Some(BoundaryOutcome::Bytes(vec![30]))
        );
        assert_eq!(rep.replay_generator_yield(), Some(BoundaryOutcome::Done));
        assert_eq!(rep.replay_generator_yield(), None); // exhausted
    }

    #[test]
    fn boundary_replay_mismatch_falls_through_without_advancing() {
        // A recorded actor event but a generator replay attempt (or vice versa) must
        // NOT consume the event — it returns None so the caller falls through.
        let mut rec = DeterminismContext::record(2, 0.0);
        rec.record_actor_call("m", &BoundaryOutcome::Bytes(vec![9]));
        let events = rec.events.clone();
        let mut rep = DeterminismContext::replay(2, 0.0, events);
        // Wrong kind → None, cursor unmoved.
        assert_eq!(rep.replay_generator_yield(), None);
        // The correct kind still finds the event.
        assert_eq!(
            rep.replay_actor_call("m"),
            Some(BoundaryOutcome::Bytes(vec![9]))
        );
    }

    #[test]
    fn replay_exhaustion_falls_through_to_record() {
        // Record a single random read, then replay but draw TWICE: the second draw
        // has no recorded event → fall through to Record (append a new event).
        let mut rec = DeterminismContext::record(5, 0.0);
        let r0 = rec.next_random_f64();
        let events = rec.events.clone();

        let mut rep = DeterminismContext::replay(5, 0.0, events);
        assert_eq!(rep.next_random_f64(), r0); // replayed (no PRNG draw)
        let r1 = rep.next_random_f64(); // fell through to record
        assert_eq!(rep.mode, Mode::Record);
        assert_eq!(rep.events.len(), 2, "second draw appended a new RandomRead");
        // Replay returns recorded values WITHOUT advancing the PRNG, so the first
        // fall-through-to-record draw is the FIRST value of a fresh seed-5 PRNG
        // (equal to the recorded `r0`, since record drew exactly once before).
        let mut fresh = SeededRng::new(5);
        assert_eq!(r1, fresh.next_f64());
        assert_eq!(r1, r0);
    }

    /// FFI Task 10 (§7A): a recorded foreign call replays its return AND its post-call
    /// `Bytes` out-param contents without re-invoking C.
    #[test]
    fn record_then_replay_ffi_call_with_out_param() {
        let mut rec = DeterminismContext::record(11, 0.0);
        // A C call that returns status int 0 and wrote [1,2,3,4] into out-param arg 1.
        rec.record_ffi_call(FfiRet::Int(0), vec![(1, vec![1, 2, 3, 4])]);
        let events = rec.events.clone();

        let mut rep = DeterminismContext::replay(11, 0.0, events);
        let (ret, outs) = rep.replay_ffi_call().expect("recorded ffi call");
        assert_eq!(ret, FfiRet::Int(0));
        assert_eq!(outs, vec![(1usize, vec![1u8, 2, 3, 4])]);
        // Exhausted → None (caller falls through to a real call).
        assert_eq!(rep.replay_ffi_call(), None);
    }

    /// A wrong-kind recorded event makes `replay_ffi_call` return None without
    /// consuming it (cursor unmoved) — the same divergence discipline as the actor /
    /// generator seams.
    #[test]
    fn replay_ffi_call_wrong_kind_falls_through() {
        let mut rec = DeterminismContext::record(1, 0.0);
        rec.record_actor_call("m", &BoundaryOutcome::Bytes(vec![9]));
        let events = rec.events.clone();
        let mut rep = DeterminismContext::replay(1, 0.0, events);
        assert_eq!(rep.replay_ffi_call(), None);
        assert_eq!(rep.cursor, 0, "cursor unmoved on a kind mismatch");
    }

    /// A method-name mismatch during replay is detected immediately: `replay_actor_call`
    /// returns `None` without consuming the event (cursor unmoved), so the fall-through
    /// path can make a real call and re-record at the correct position. This catches a
    /// divergent replay (different method call order) at the earliest possible point
    /// rather than silently returning a recorded result for the wrong method.
    #[test]
    fn replay_actor_call_method_name_mismatch_is_detected() {
        let mut rec = DeterminismContext::record(42, 0.0);
        rec.record_actor_call("get_count", &BoundaryOutcome::Bytes(vec![7]));
        let events = rec.events.clone();

        let mut rep = DeterminismContext::replay(42, 0.0, events);
        // Replay with the WRONG method name → None (cursor stays at 0).
        assert_eq!(
            rep.replay_actor_call("increment"),
            None,
            "method-name mismatch must return None (divergence detected)"
        );
        assert_eq!(rep.cursor, 0, "cursor must be unmoved on a mismatch");

        // Replay with the CORRECT method name still works (event was not consumed).
        assert_eq!(
            rep.replay_actor_call("get_count"),
            Some(BoundaryOutcome::Bytes(vec![7])),
            "correct method must still find the event"
        );
        assert_eq!(rep.cursor, 1, "cursor must advance after a successful match");
    }

    /// A wrong-kind recorded event under `clock_monotonic_ms` must surface the replay
    /// mismatch via `replay_mismatch_recover` — NOT silently cross-consume a `ClockRead`
    /// as if it were a monotonic value (which would shift the whole replay one event out
    /// of phase). Mirrors `clock_now_ms`'s mismatch discipline.
    fn replay_mismatch_recover_value(start_ms: f64) -> f64 {
        // `replay_mismatch_recover` returns `self.clock.now_ms()`, which a replay clock
        // initializes to `start_ms`. Picking a `start_ms` distinct from any seeded event
        // value makes the mismatch path observable.
        start_ms
    }

    #[test]
    fn clock_monotonic_ms_surfaces_replay_mismatch_on_clock_read() {
        // Seed a ClockRead (a DIFFERENT event kind) at the cursor; replaying a monotonic
        // read against it is a divergence. The recorded value (999.0) must NOT be returned
        // — the mismatch-recover value (the wall clock = start_ms = 42.0) is.
        let events = vec![DetEvent::ClockRead { value: 999.0 }];
        let mut rep = DeterminismContext::replay(0, 42.0, events);
        let got = rep.clock_monotonic_ms();
        assert_eq!(
            got,
            replay_mismatch_recover_value(42.0),
            "a ClockRead under clock_monotonic_ms must surface via replay_mismatch_recover, \
             not be silently cross-consumed as a monotonic value"
        );
        assert_ne!(got, 999.0, "the recorded wrong-kind value must NOT leak through");
    }

    #[test]
    fn clock_monotonic_ms_surfaces_replay_mismatch_on_random_read() {
        // Same divergence with a RandomRead at the cursor.
        let events = vec![DetEvent::RandomRead { value: 0.123 }];
        let mut rep = DeterminismContext::replay(0, 77.0, events);
        let got = rep.clock_monotonic_ms();
        assert_eq!(got, replay_mismatch_recover_value(77.0));
        assert_ne!(got, 0.123);
    }

    /// Blast-radius (Task 0.12): `next_random_f64` had the same silent-mismatch bug —
    /// a wrong-kind event at the cursor silently drew a fresh PRNG value (bypassing
    /// replay, shifting the stream out of phase) instead of surfacing the divergence.
    /// A non-`RandomRead` event must now surface via `replay_mismatch_recover`.
    /// Task 0.19c: a recorded byte draw replays FROM THE EVENT — faithful even when the
    /// seed CHANGES between record and replay (proving the bytes come from the event, not
    /// just a reproducible seed). This is the fidelity guarantee the pre-0.19c path lacked.
    #[test]
    fn record_then_replay_bytes_is_from_event_not_seed() {
        // Record two byte draws of different lengths under seed 123.
        let mut rec = DeterminismContext::record(123, 0.0);
        let mut a0 = [0u8; 16];
        let mut a1 = [0u8; 8];
        rec.next_seeded_bytes(&mut a0);
        rec.next_seeded_bytes(&mut a1);
        let events = rec.events.clone();

        // Replay with a DIFFERENT seed (999). If the bytes were re-drawn from the PRNG
        // they'd diverge; because they come from the recorded event they're identical.
        let mut rep = DeterminismContext::replay(999, 0.0, events);
        let mut b0 = [0u8; 16];
        let mut b1 = [0u8; 8];
        rep.next_seeded_bytes(&mut b0);
        rep.next_seeded_bytes(&mut b1);
        assert_eq!(a0, b0, "byte draw must replay verbatim from the event");
        assert_eq!(a1, b1, "second byte draw must replay verbatim from the event");

        // Sanity: a fresh seed-999 PRNG would have produced DIFFERENT bytes — so the
        // equality above genuinely proves event-fidelity, not seed-coincidence.
        let mut fresh = SeededRng::new(999);
        let mut diff = [0u8; 16];
        fresh.fill_bytes(&mut diff);
        assert_ne!(diff, b0, "seed 999 differs from recorded seed 123 — event was used");
    }

    /// Task 0.19c: a wrong-kind event at a byte-draw position surfaces a divergence — the
    /// recorded value must NOT leak through (mirrors `next_random_f64`'s mismatch test).
    #[test]
    fn next_seeded_bytes_surfaces_replay_mismatch_on_clock_read() {
        let events = vec![DetEvent::ClockRead { value: 999.0 }];
        let mut rep = DeterminismContext::replay(7, 0.0, events);
        let mut buf = [0u8; 8];
        rep.next_seeded_bytes(&mut buf);
        // The mismatch-recover fills from the seeded PRNG (best-effort keep-going). The
        // recorded 999.0 has no byte representation that could leak; assert the buf was
        // filled by the seed-7 PRNG, NOT left as a phase-shifted recorded value.
        let mut expect = SeededRng::new(7);
        let mut want = [0u8; 8];
        expect.fill_bytes(&mut want);
        assert_eq!(buf, want, "a wrong-kind event must surface via the byte mismatch-recover");
    }

    /// Task 0.19c: a `BytesRead` of the WRONG LENGTH is also a mismatch (not a silent
    /// truncate/partial copy) — surfaced via the byte mismatch-recover.
    #[test]
    fn next_seeded_bytes_wrong_length_is_a_mismatch() {
        let events = vec![DetEvent::BytesRead { bytes: vec![1, 2, 3, 4] }];
        let mut rep = DeterminismContext::replay(7, 0.0, events);
        let mut buf = [0u8; 8]; // asks for 8, recorded 4 → length mismatch
        rep.next_seeded_bytes(&mut buf);
        let mut expect = SeededRng::new(7);
        let mut want = [0u8; 8];
        expect.fill_bytes(&mut want);
        assert_eq!(buf, want, "a wrong-length BytesRead must surface as a mismatch");
        assert_eq!(rep.cursor, 1, "wrong-length BytesRead must be consumed");
    }

    /// Task 0.19c: byte-draw replay exhaustion falls through to Record (append a new
    /// `BytesRead`), matching the clock/RNG seams' crash-point behavior.
    #[test]
    fn next_seeded_bytes_exhaustion_falls_through_to_record() {
        let mut rec = DeterminismContext::record(5, 0.0);
        let mut a0 = [0u8; 4];
        rec.next_seeded_bytes(&mut a0);
        let events = rec.events.clone();

        let mut rep = DeterminismContext::replay(5, 0.0, events);
        let mut b0 = [0u8; 4];
        rep.next_seeded_bytes(&mut b0); // replayed
        assert_eq!(a0, b0);
        let mut b1 = [0u8; 4];
        rep.next_seeded_bytes(&mut b1); // fell through to record
        assert_eq!(rep.mode, Mode::Record);
        assert_eq!(rep.events.len(), 2, "second draw appended a new BytesRead");
        // The fall-through draw is the FIRST draw of a fresh seed-5 PRNG (replay returns
        // recorded bytes WITHOUT advancing the PRNG, so the PRNG is still at its start).
        let mut fresh = SeededRng::new(5);
        let mut want = [0u8; 4];
        fresh.fill_bytes(&mut want);
        assert_eq!(b1, want);
    }

    #[test]
    fn next_random_f64_surfaces_replay_mismatch_on_clock_read() {
        let events = vec![DetEvent::ClockRead { value: 999.0 }];
        let mut rep = DeterminismContext::replay(0, 55.0, events);
        let got = rep.next_random_f64();
        assert_eq!(
            got,
            replay_mismatch_recover_value(55.0),
            "a ClockRead under next_random_f64 must surface via replay_mismatch_recover, \
             not silently draw a fresh PRNG value"
        );
        assert_ne!(got, 999.0, "the recorded wrong-kind value must NOT leak through");
    }
}
