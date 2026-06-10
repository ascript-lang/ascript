//! FUZZ Task 5 — the `.aso` deserializer + verifier byte-fuzz target (SECURITY-CRITICAL;
//! gates BIN, spec §2.2 / §4.1).
//!
//! A shipped native binary (BIN) parses attacker-influenceable `.aso` bytes, so the reader
//! is a security surface. This libFuzzer target feeds ARBITRARY bytes to the two public
//! reader entry points and asserts the load-bearing invariant:
//!
//!   `Chunk::from_bytes(any_bytes)` and `Chunk::from_bytes_verified(any_bytes)` ALWAYS
//!   return `Ok(Chunk)` / `Err(AsoError)` / `Err(FromBytesVerifiedError)` — and NEVER
//!   panic, read out of bounds, allocate unboundedly, hang, or exhibit UB.
//!
//! Two reader entry points (per the plan's "find the entry points yourself"):
//!   - `ascript::vm::chunk::Chunk::from_bytes`            (src/vm/aso.rs) — decode only.
//!   - `ascript::vm::chunk::Chunk::from_bytes_verified`   (src/vm/verify.rs) — decode + verify.
//!     This is the variant `run --inspect file.aso` / `ascript::run_aso_file` use.
//!
//! RUNNABLE-ACCEPT (bounded, spec §2.2): when `from_bytes_verified` returns `Ok`, we run the
//! chunk on the VM on the 512 MB worker stack. A *verified* chunk that crashes the VM (a
//! `debug_assert`/panic the verifier failed to rule out) is a `verify.rs` gap — itself a
//! security finding. The run is bounded by libFuzzer's own knobs (the halting-problem bound,
//! spec §9): `-timeout=<s>` flags a hang and `-rss_limit_mb=<n>` flags runaway allocation. We
//! prove accepted chunks don't IMMEDIATELY crash, not that they all halt.
//!
//! Determinism: every libFuzzer finding is a saved input file, replayable verbatim.
//!
//! Seed corpus: `fuzz/corpus/aso_roundtrip/` — real `.aso` built by `ascript::build_file`
//! over `examples/**` (carrying the current `ASO_FORMAT_VERSION`), plus the curated known-bad
//! buffers. The structure-aware seeds let the mutator flip ONE byte deep inside a valid proto
//! tree, reaching the `read_*` arms where the clamp-class bugs live.
//!
//! The in-suite proof that this target's "panic ⇒ crash" detection actually fires lives in the
//! NORMAL test suite: `tests/property.rs::aso_planted_bug_known_bad_bytes_are_clean_err` (the
//! permanent planted-bug guard) — that is the in-session evidence; the libFuzzer campaign is the
//! coverage-guided extension that runs in CI.

#![no_main]

use libfuzzer_sys::fuzz_target;

use ascript::vm::chunk::Chunk;

fuzz_target!(|data: &[u8]| {
    // (1) DECODE-only path — the SECURITY-CRITICAL core, exercised on EVERY input. The contract:
    // NEVER panic — only `Ok(Chunk)`/`Err(AsoError)`. libFuzzer treats any panic/abort/OOM/OOB/hang
    // inside as a crash, so simply CALLING it under the fuzzer is the assertion. Both `Ok` and `Err`
    // are valid outcomes for arbitrary bytes.
    let _ = Chunk::from_bytes(data);

    // (2) DECODE + VERIFY path (the one `run_aso_file` uses) — also on every input, also fast. Same
    // contract: only `Ok`/`Err(FromBytesVerifiedError)`, never a panic. These two reader calls are
    // the BIN-gating invariant (spec §2.2) and are intentionally cheap, so the per-PR + nightly
    // campaign accumulate `read_*` coverage at high throughput without spurious slow-unit timeouts.
    let _ = Chunk::from_bytes_verified(data);

    // (3) RUNNABLE-ACCEPT (bounded, OPT-IN) — the verifier-gap check (spec §2.2): a chunk the
    // verifier ACCEPTS must not crash the VM HOST. This actually RUNS accepted chunks, so a
    // structure-aware mutation can produce a valid-but-slow program that legitimately runs past a
    // per-input `-timeout` — the documented halting-problem boundary (spec §9), a slow VALID
    // program, NOT a soundness bug (no panic/OOB/OOM). To keep the high-throughput decode+verify
    // campaign free of that non-soundness noise, the run is gated behind `ASCRIPT_FUZZ_RUNNABLE`:
    // the nightly verifier-gap pass sets it (with a generous `-timeout`/`-rss_limit_mb`), while the
    // per-PR smoke run leaves it off. A HOST panic in the run IS a finding either way. We run on the
    // 512 MB worker stack so deep-but-legal recursion reaches its guard cleanly (CLAUDE.md SP3/SP9);
    // `data` is copied into an owned `Vec` so the `Send` worker closure borrows no libFuzzer buffer.
    if std::env::var_os("ASCRIPT_FUZZ_RUNNABLE").is_some() {
        let owned = data.to_vec();
        ascript::run_on_worker_stack(move || async move {
            ascript::aso_runnable_accept(&owned).await;
        });
    }
});
