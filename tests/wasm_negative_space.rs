//! WASM §7.3 — negative space: the wasm port changes NOTHING native-observable.
//!
//! These tests pin the invariants that Gate W-1 ("native byte-identical") rests on,
//! read straight from source so they track intent, not a stale copy:
//! - `ASO_FORMAT_VERSION` is NOT bumped on this branch (WASM is no-`.aso`-change);
//! - the compiler / serializer / opcode paths carry ZERO `target_family = "wasm"`
//!   cfg (the wasm portability lives in `platform.rs`, the worker/stack guards, and
//!   the target-dep tables — never in the platform-independent codegen path);
//! - the Cargo target-dep split keeps `tokio`/`rustyline` on the non-wasm row with
//!   today's exact native feature set (so native resolution is unchanged).
use std::fs;

#[test]
fn aso_format_version_unchanged() {
    // Read the constant from source so the pin tracks intent, not a copy. The value
    // seen at branch time is 29 (ELIDE's `Op::CallElided`); bumping it on this branch
    // is a spec violation (WASM §7.3: no `.aso` change).
    let src = fs::read_to_string("src/vm/aso.rs").unwrap();
    let line = src
        .lines()
        .find(|l| l.contains("pub const ASO_FORMAT_VERSION"))
        .expect("ASO_FORMAT_VERSION declaration present");
    let at_branch: u32 = 29;
    assert!(
        line.contains(&at_branch.to_string()),
        "WASM must not bump ASO_FORMAT_VERSION (spec §7.3); saw line: {line}"
    );
}

#[test]
fn no_wasm_cfg_in_chunk_or_serializer() {
    // The compiler/serializer/opcode paths stay platform-independent (spec §7.3):
    // wasm portability never leaks into the codegen path.
    for f in [
        "src/vm/chunk.rs",
        "src/vm/aso.rs",
        "src/vm/opcode.rs",
        "src/compile/mod.rs",
    ] {
        let src = fs::read_to_string(f).unwrap();
        assert!(
            !src.contains("target_family = \"wasm\""),
            "wasm cfg leaked into {f} (spec §7.3: codegen path is platform-independent)"
        );
    }
}

#[test]
fn cargo_target_dep_split_keeps_native_row() {
    // Gate W-1: the target-dep tables must keep `tokio` (with today's exact native
    // feature set incl. `rt-multi-thread`) and `rustyline` on the NON-wasm row so
    // native dependency resolution is byte-identical.
    let toml = fs::read_to_string("Cargo.toml").unwrap();
    assert!(
        toml.contains("[target.'cfg(not(target_family = \"wasm\"))'.dependencies]"),
        "the non-wasm target-dep table must exist (WASM §5.3.2)"
    );
    assert!(
        toml.contains("[target.'cfg(target_family = \"wasm\")'.dependencies]"),
        "the wasm target-dep table must exist (WASM §5.3.2)"
    );
    // The non-wasm tokio row must retain rt-multi-thread (the native feature set is
    // unchanged — only the wasm row drops it as vestigial).
    let non_wasm_table = toml
        .split("[target.'cfg(not(target_family = \"wasm\"))'.dependencies]")
        .nth(1)
        .expect("non-wasm table body");
    let non_wasm_body = non_wasm_table
        .split("\n[target")
        .next()
        .expect("non-wasm table delimited");
    assert!(
        non_wasm_body.contains("rt-multi-thread"),
        "native tokio row must keep rt-multi-thread (Gate W-1: native feature set unchanged)"
    );
    assert!(
        non_wasm_body.contains("rustyline"),
        "rustyline must stay on the non-wasm row (it pulls native fd-lock; WASM §5.3.2)"
    );
}

// ── Task 1.4: the `wasm_run_source` caps entry (cfg-free, native-tested) ──────────

/// WASM §5.4: the wrapper entry applies the caller's CapSet at `Interp` construction.
/// With all five denied, an `env` touch (a cap-gated OS read) raises a recoverable
/// Tier-2 panic that `recover` catches as a `[nil, err]` pair — proving the deny-all
/// is enforced on the EXACT path the wasm wrapper ships.
#[tokio::test]
async fn wasm_entry_denies_all_caps() {
    let mut caps = ascript::stdlib::caps::CapSet::all_granted();
    caps.deny_all_dangerous();
    let (out, _exit) = ascript::wasm_run_source(
        r#"import * as env from "std/env"
let [v, err] = recover(() => env.get("HOME"))
print(err != nil)"#,
        caps,
    )
    .await
    .expect("program runs (the denial is a recoverable Tier-2, not a hard error)");
    assert_eq!(out, "true\n", "a denied env.get must surface a recoverable error");
}

/// WASM §5.4: with `all_granted` the entry adds NOTHING over `vm_run_source` on a
/// pure-compute program — the captured output byte-matches.
#[tokio::test]
async fn wasm_entry_all_granted_matches_vm_run_source() {
    let src = "let total = 0\nfor (i in 1..=10) {\n  total = total + i * i\n}\nprint(total)";
    let (wasm_out, wasm_exit) = ascript::wasm_run_source(
        src,
        ascript::stdlib::caps::CapSet::all_granted(),
    )
    .await
    .expect("compute program runs");
    let (vm_out, vm_exit) = ascript::vm_run_source(src).await.expect("vm path runs");
    assert_eq!(wasm_out, vm_out, "wasm_run_source must byte-match vm_run_source");
    assert_eq!(wasm_exit, vm_exit, "exit codes must match");
}

/// WASM §5.3.7: workers are not available on wasm. On NATIVE the worker still runs
/// (this proves the guard is wasm-only and native worker dispatch is untouched — Gate
/// W-1). The wasm refusal itself is exercised by the Phase-2 Node smoke; here we pin
/// that the guard message wording exists in the source as the single shipped string.
#[test]
fn workers_guard_message_is_the_shipped_string() {
    let isolate = fs::read_to_string("src/worker/isolate.rs").unwrap();
    let dispatch = fs::read_to_string("src/worker/mod.rs").unwrap();
    let msg = "workers are not available on this platform (wasm)";
    assert!(
        isolate.contains(msg),
        "the bootstrap chokepoint must carry the shipped worker-refusal string"
    );
    assert!(
        dispatch.contains(msg),
        "the pooled funnel must surface the SAME shipped worker-refusal string (never silent inline-degradation on wasm)"
    );
}
