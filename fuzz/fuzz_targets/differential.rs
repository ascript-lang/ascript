//! FUZZ Task 7 — the differential program fuzzer (the headline target).
//!
//! `arbitrary` bytes → the grammar-aware generator (`ascript::fuzzgen`) → a VALID,
//! deterministic, run-to-completion AScript program → run on ALL FIVE engine modes and assert
//! they agree on the deterministic projection. This is the SAME five-way differential
//! `tests/vm_differential.rs` enforces over a fixed corpus, turned into a continuous
//! generator-driven oracle:
//!
//!   `run(tree-walker, P) == run(specialized-VM, P) == run(generic-VM, P) == run(lane-off, P) == run(no-call-fast, P)`
//!
//! compared on `(captured stdout, exit code)` on success or the Tier-2 panic MESSAGE on
//! failure (the SP1 caret-column offset between front-ends is excluded — message only). ANY
//! disagreement is a GUARANTEED bug: a compiler / VM-opcode / specialization-guard
//! (generic ≠ specialized) / lane divergence / oracle bug. A divergence is a libFuzzer crash
//! (we `panic!` with the program + both outcomes so the saved input is a ready reproducer).
//!
//! LANE §6.1: the lane-off projection (`vm_run_source_no_sync_lane`) is the fourth axis
//! added by Gate 15 — it proves that the sync-lane burst driver produces byte-identical
//! results across the entire fuzz input space.
//!
//! The generator is reached via `ascript::fuzzgen` — the `fuzz/` crate enables the
//! `fuzzgen` feature on its `ascript` dep, which exposes `pub mod fuzzgen` (the SAME single
//! generator definition the in-tree proptest reaches under `cfg(test)`; one definition, no
//! drift — there is NO `fuzz/`→shared-crate fork). cargo-fuzz also sets `--cfg fuzzing`,
//! which independently admits the module's crate-level gate.
//!
//! The engines are `!Send` current-thread tokio with deep recursion, so each run is wrapped
//! on `run_on_worker_stack` (the 512 MB worker stack, CLAUDE.md SP3/SP9) — `data` is copied
//! into an owned `String` so the `Send` worker closure borrows no libFuzzer buffer.
//!
//! The in-suite proof (no cargo-fuzz needed) lives in the NORMAL suite:
//! `tests/property.rs::three_way_differential_over_generated_programs` (+ the fixed-seed
//! battery + the SABOTEUR self-test proving the differential CAN detect a divergence). This
//! target is the coverage-guided extension that runs in CI from
//! `fuzz/corpus/differential/`.
//!
//! Determinism (spec §6): the generator is deterministic-by-construction (no clock/RNG/race/
//! unsorted-iteration output), so a divergence is a real bug, never a nondeterministic flake;
//! every finding is a saved input file, replayable verbatim.

#![no_main]

use libfuzzer_sys::fuzz_target;

/// The deterministic projection of a program run, compared across engines — stdout + exit on
/// success, or the Tier-2 panic MESSAGE on failure (span/caret excluded per SP1).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Ok { stdout: String, exit: Option<i32> },
    Panic { message: String },
}

fn project(r: Result<(String, Option<i32>), ascript::error::AsError>) -> Outcome {
    match r {
        Ok((stdout, exit)) => Outcome::Ok { stdout, exit },
        Err(e) => Outcome::Panic { message: e.message },
    }
}

// LeakSanitizer: this target RUNS the managed runtime, whose process-lifetime state (interned
// `Rc<str>`, `lazy_static`/`once_cell` globals, the tokio runtime, gcmodule's collector,
// thread-locals) is NOT a leak. Disable LSan for THIS target's binary — the libFuzzer
// `-detect_leaks=0` FLAG does NOT stop the shutdown LSan check; only this compiled-in default
// does. (`aso_roundtrip` is decode-only and keeps leak detection: a leak there is a real bug.)
#[no_mangle]
pub extern "C" fn __lsan_default_options() -> *const std::os::raw::c_char {
    c"detect_leaks=0".as_ptr()
}

fuzz_target!(|data: &[u8]| {
    // Bytes → a valid, deterministic, run-to-completion program. `gen_program_from_bytes`
    // never fails (it falls back to a trivial-but-valid program when bytes are exhausted).
    let prog = ascript::fuzzgen::gen_program_from_bytes(data);
    let src = prog.source;

    // Run all five engine modes on the 512 MB worker stack and project each outcome. The
    // owned `String` is moved into the `Send` closure (no borrow of the libFuzzer buffer
    // crosses). LANE §6.1: the lane-off projection is the fourth axis (Gate 15).
    // CALL §8.1: the no-call-fast projection is the fifth axis (Gate 15).
    let (tw, vm, gen, nolane, nocf) = ascript::run_on_worker_stack({
        let src = src.clone();
        move || async move {
            let tw = project(ascript::run_source_exit(&src).await);
            let vm = project(ascript::vm_run_source(&src).await);
            let gen = project(ascript::vm_run_source_generic(&src).await);
            // LANE §6.1: lane-off must be byte-identical to all other modes.
            let nolane = project(ascript::vm_run_source_no_sync_lane(&src).await);
            // CALL §8.1: no-call-fast must be byte-identical to all other modes.
            let nocf = project(ascript::vm_run_source_no_call_fast(&src).await);
            (tw, vm, gen, nolane, nocf)
        }
    });

    // THE ORACLE: all five must agree. A panic here is a libFuzzer crash carrying a ready
    // reproducer. Fix the ENGINE, never relax this assertion (Gate 0).
    assert_eq!(
        tw, vm,
        "specialized-VM diverged from tree-walker\n--- program ---\n{src}\n--- tw: {tw:?}\n--- vm: {vm:?}"
    );
    assert_eq!(
        tw, gen,
        "generic-VM diverged from tree-walker (a wrong specialization guard)\n--- program ---\n{src}\n--- tw: {tw:?}\n--- gen: {gen:?}"
    );
    // LANE §6.1 / Gate 15: lane-off must be byte-identical to tree-walker.
    assert_eq!(
        tw, nolane,
        "lane-off VM diverged from tree-walker\n--- program ---\n{src}\n--- tw: {tw:?}\n--- nolane: {nolane:?}"
    );
    // CALL §8.1 / Gate 15: no-call-fast must be byte-identical to tree-walker.
    assert_eq!(
        tw, nocf,
        "no-call-fast VM diverged from tree-walker\n--- program ---\n{src}\n--- tw: {tw:?}\n--- nocf: {nocf:?}"
    );
});
