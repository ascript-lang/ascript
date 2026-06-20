//! REPLAY §3 — the `ASTRC` trace-container deserializer byte-fuzz target
//! (SECURITY-CRITICAL; gates the record/replay container).
//!
//! A recorded trace (`ascript run --record <trace>`) is an attacker-WRITABLE
//! file that `--replay` parses, so [`ascript::trace::read_trace`] is a security
//! surface — the exact `.aso`-reader discipline (see `aso_roundtrip.rs`). This
//! libFuzzer target feeds ARBITRARY bytes to the reader and asserts the
//! load-bearing invariant:
//!
//!   `read_trace(any_bytes)` ALWAYS returns `Ok((TraceHeader, Vec<DetEvent>))`
//!   / `Err(TraceError)` — and NEVER panics, reads out of bounds, allocates
//!   unboundedly (a count/length bomb), hangs, or exhibits UB.
//!
//! libFuzzer treats any panic/abort/OOM/OOB/hang inside as a crash, so simply
//! CALLING the reader under the fuzzer is the assertion. Both `Ok` and `Err`
//! are valid outcomes for arbitrary bytes.
//!
//! Round-trip stability: when a decode succeeds, re-encoding via `write_trace`'s
//! `encode` path and re-decoding must reproduce the SAME `(header, events)` — a
//! divergence is a codec bug. (`write_trace` does IO; the fuzzer exercises the
//! pure `read_trace` invariant only — the in-suite round-trip tests in
//! `src/trace.rs` cover encode∘decode equality over structured inputs.)
//!
//! Seed corpus: `fuzz/corpus/trace_roundtrip/` — real small recorded traces
//! written by the `tests/property.rs` seed-corpus helper, plus the curated
//! known-bad buffers. The in-suite proof that this target's "panic ⇒ crash"
//! detection actually fires lives in the NORMAL test suite:
//! `tests/property.rs::trace_planted_bug_known_bad_bytes_are_clean_err`.

#![no_main]

use libfuzzer_sys::fuzz_target;

use ascript::trace::read_trace;

fuzz_target!(|data: &[u8]| {
    // The SECURITY-CRITICAL contract: NEVER panic — only `Ok`/`Err(TraceError)`.
    let _ = read_trace(data);
});
