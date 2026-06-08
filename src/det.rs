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
    /// Workers Spec B (Task 12): one `worker fn*` resume crossed the isolate boundary.
    /// `value` is the structured-clone-encoded yielded value bytes, or `None` when the
    /// producer finished (the resume returned done). On Replay the recorded outcome is
    /// returned WITHOUT re-driving the producer isolate. `panic` carries a producer
    /// panic message instead of a yield.
    GeneratorYield {
        value: Option<Vec<u8>>,
        panic: Option<String>,
    },
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

/// The per-`Interp` deterministic-mode context (spec §3.2). When installed
/// (`Interp.determinism = Some(..)`), the clock/RNG seams route through it; when
/// `None` (default), they take their existing real paths.
#[derive(Debug, Clone)]
pub struct DeterminismContext {
    pub mode: Mode,
    pub clock: VirtualClock,
    pub rng: SeededRng,
    pub seed: u64,
    /// Replay cursor into `events`.
    pub cursor: usize,
    /// The in-memory recorded effect stream (the persisted workflow log is a
    /// projection of this).
    pub events: Vec<DetEvent>,
}

impl DeterminismContext {
    /// A fresh Record-mode context seeded by `seed`, with the virtual clock started
    /// at `start_ms`. The event stream is empty (first run).
    pub fn record(seed: u64, start_ms: f64) -> Self {
        DeterminismContext {
            mode: Mode::Record,
            clock: VirtualClock::new(start_ms),
            rng: SeededRng::new(seed),
            seed,
            cursor: 0,
            events: Vec::new(),
        }
    }

    /// A Replay-mode context seeded by `seed`, primed with a previously-recorded
    /// `events` stream. The virtual clock is started at `start_ms` (it is overridden
    /// by the recorded `ClockRead`s as they are consumed). The cursor starts at 0.
    pub fn replay(seed: u64, start_ms: f64, events: Vec<DetEvent>) -> Self {
        DeterminismContext {
            mode: Mode::Replay,
            clock: VirtualClock::new(start_ms),
            rng: SeededRng::new(seed),
            seed,
            cursor: 0,
            events,
        }
    }

    /// Read the wall clock (ms-epoch). Record: draw the virtual clock value, append a
    /// `ClockRead`. Replay: return the next recorded `ClockRead` at `cursor`; if the
    /// stream is exhausted (the crash point), fall through to Record from here.
    pub fn clock_now_ms(&mut self) -> f64 {
        match self.mode {
            Mode::Record => {
                let v = self.clock.now_ms();
                self.events.push(DetEvent::ClockRead { value: v });
                v
            }
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
                self.events.push(DetEvent::MonotonicRead { value: v });
                v
            }
            Mode::Replay => match self.next_event() {
                Some(DetEvent::MonotonicRead { value }) => {
                    self.clock.monotonic_ms = value;
                    value
                }
                Some(DetEvent::ClockRead { value }) => value,
                Some(_) => self.clock.monotonic_ms(),
                None => {
                    self.mode = Mode::Record;
                    let v = self.clock.monotonic_ms();
                    self.events.push(DetEvent::MonotonicRead { value: v });
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
                self.events.push(DetEvent::RandomRead { value: v });
                v
            }
            Mode::Replay => match self.next_event() {
                Some(DetEvent::RandomRead { value }) => value,
                Some(_) => {
                    // Wrong kind during replay — advance the rng anyway to keep
                    // determinism for any subsequent fall-through-to-record draws.
                    self.rng.next_f64()
                }
                None => {
                    self.mode = Mode::Record;
                    let v = self.rng.next_f64();
                    self.events.push(DetEvent::RandomRead { value: v });
                    v
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
        self.events.push(DetEvent::ActorCall {
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
        self.events.push(DetEvent::GeneratorYield { value, panic });
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

    /// Peek the event at `cursor` WITHOUT advancing (the boundary replay helpers only
    /// advance the cursor when the event kind matches, so a mismatch leaves the cursor
    /// in place for the fall-through path to re-record).
    fn peek_event(&self) -> Option<DetEvent> {
        self.events.get(self.cursor).cloned()
    }

    /// Advance the cursor and return the event it pointed at (Replay helper).
    fn next_event(&mut self) -> Option<DetEvent> {
        let e = self.events.get(self.cursor).cloned();
        if e.is_some() {
            self.cursor += 1;
        }
        e
    }

    /// A clock read hit a non-clock recorded event during replay — return the
    /// current virtual clock value as a best-effort (the workflow-level detector in
    /// `std/workflow` is the authoritative non-determinism guard for activities;
    /// bare clock/RNG seams outside a workflow never reach this in practice).
    fn replay_mismatch_recover(&mut self, _got: DetEvent) -> f64 {
        self.clock.now_ms()
    }

    /// Replay exhausted on a clock read → switch to Record and record from here.
    fn switch_to_record_clock(&mut self) -> f64 {
        self.mode = Mode::Record;
        let v = self.clock.now_ms();
        self.events.push(DetEvent::ClockRead { value: v });
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
