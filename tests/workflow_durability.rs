//! WARM C §4.2 — Durability option surface tests.
//!
//! Task 9: `Durability` enum, unknown-value hardening, `"group"` parsing with
//! `groupWindowMs`/`groupMaxEvents` overrides and validation.
//!
//! Both engines must emit identical error messages for the same bad input.
//! Existing `"fsync"`/`"buffered"`/absent behavior must be bit-for-bit unchanged.
#![cfg(feature = "workflow")]

/// Helper: a temp log path unique per test (removed if it already exists).
fn temp_log(name: &str) -> String {
    let p = std::env::temp_dir().join(format!(
        "ascript_wfd_{name}_{}_{}_{:?}.log",
        std::process::id(),
        std::thread::current().name().unwrap_or("t"),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&p);
    p.to_string_lossy().into_owned()
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Run a program (as AScript source) on the tree-walker, returning `(output, error_message)`.
/// On panic, `output` is empty and `error_message` contains the panic message.
async fn tw_run(src: &str) -> (String, Option<String>) {
    match ascript::run_source(src).await {
        Ok(out) => (out, None),
        Err(e) => (String::new(), Some(e.message)),
    }
}

/// Run a program on the VM, returning `(output, error_message)`.
async fn vm_run(src: &str) -> (String, Option<String>) {
    match ascript::vm_run_source(src).await {
        Ok((out, _code)) => (out, None),
        Err(e) => (String::new(), Some(e.message)),
    }
}

// ─── §4.2 Hardening: unknown durability string ───────────────────────────────

/// An unknown durability string must produce a Tier-2 error naming all three
/// valid values, on BOTH engines with identical messages.
#[tokio::test]
async fn unknown_durability_groop_errors_both_engines() {
    let log = temp_log("groop");
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let noop = activity("noop", () => 1)
fn flow(ctx, _) {{ return ctx.call(noop) }}
await run(flow, nil, {{ log: "{log}", durability: "groop" }})
"#
    );
    let (_, tw_err) = tw_run(&src).await;
    let (_, vm_err) = vm_run(&src).await;
    let tw_err = tw_err.expect("tree-walker must Tier-2-panic on unknown durability");
    let vm_err = vm_err.expect("vm must Tier-2-panic on unknown durability");

    // Both messages must mention the three valid values.
    for valid in &["fsync", "group", "buffered"] {
        assert!(
            tw_err.contains(valid),
            "tree-walker error must list '{valid}' in: {tw_err}"
        );
        assert!(
            vm_err.contains(valid),
            "vm error must list '{valid}' in: {vm_err}"
        );
    }
    // Both engines must be identical.
    assert_eq!(tw_err, vm_err, "error message diverged:\n  tw: {tw_err:?}\n  vm: {vm_err:?}");
}

/// "full" is not an alias; it must error naming the valid set (both engines).
#[tokio::test]
async fn unknown_durability_full_errors_both_engines() {
    let log = temp_log("full");
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let noop = activity("noop", () => 1)
fn flow(ctx, _) {{ return ctx.call(noop) }}
await run(flow, nil, {{ log: "{log}", durability: "full" }})
"#
    );
    let (_, tw_err) = tw_run(&src).await;
    let (_, vm_err) = vm_run(&src).await;
    let tw_err = tw_err.expect("tree-walker must error on 'full'");
    let vm_err = vm_err.expect("vm must error on 'full'");

    for valid in &["fsync", "group", "buffered"] {
        assert!(
            tw_err.contains(valid),
            "error must list '{valid}' in: {tw_err}"
        );
    }
    assert_eq!(tw_err, vm_err, "message diverged:\n  tw: {tw_err:?}\n  vm: {vm_err:?}");
}

/// "async" is not an alias (spec §7); must error naming the valid set.
#[tokio::test]
async fn unknown_durability_async_errors_both_engines() {
    let log = temp_log("async");
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let noop = activity("noop", () => 1)
fn flow(ctx, _) {{ return ctx.call(noop) }}
await run(flow, nil, {{ log: "{log}", durability: "async" }})
"#
    );
    let (_, tw_err) = tw_run(&src).await;
    let (_, vm_err) = vm_run(&src).await;
    let tw_err = tw_err.expect("tree-walker must error on 'async'");
    let vm_err = vm_err.expect("vm must error on 'async'");

    for valid in &["fsync", "group", "buffered"] {
        assert!(
            tw_err.contains(valid),
            "error must list '{valid}' in: {tw_err}"
        );
    }
    assert_eq!(tw_err, vm_err, "message diverged:\n  tw: {tw_err:?}\n  vm: {vm_err:?}");
}

// ─── §4.2 Valid values parse without error ────────────────────────────────────

/// `"fsync"` is accepted; the workflow runs and returns normally.
#[tokio::test]
async fn durability_fsync_accepted() {
    let log = temp_log("fsync_ok");
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let noop = activity("noop", () => 42)
fn flow(ctx, _) {{ return ctx.call(noop) }}
let r = await run(flow, nil, {{ log: "{log}", durability: "fsync" }})
print(r)
"#
    );
    let (tw_out, tw_err) = tw_run(&src).await;
    let (vm_out, vm_err) = vm_run(&src).await;
    assert!(tw_err.is_none(), "tree-walker must not error on 'fsync': {:?}", tw_err);
    assert!(vm_err.is_none(), "vm must not error on 'fsync': {:?}", vm_err);
    assert_eq!(tw_out.trim(), "42", "tree-walker output: {tw_out}");
    assert_eq!(vm_out.trim(), "42", "vm output: {vm_out}");
}

/// `"buffered"` is accepted; the workflow runs and returns normally.
#[tokio::test]
async fn durability_buffered_accepted() {
    let log = temp_log("buffered_ok");
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let noop = activity("noop", () => 42)
fn flow(ctx, _) {{ return ctx.call(noop) }}
let r = await run(flow, nil, {{ log: "{log}", durability: "buffered" }})
print(r)
"#
    );
    let (tw_out, tw_err) = tw_run(&src).await;
    let (vm_out, vm_err) = vm_run(&src).await;
    assert!(tw_err.is_none(), "tree-walker must not error on 'buffered': {:?}", tw_err);
    assert!(vm_err.is_none(), "vm must not error on 'buffered': {:?}", vm_err);
    assert_eq!(tw_out.trim(), "42", "tree-walker output: {tw_out}");
    assert_eq!(vm_out.trim(), "42", "vm output: {vm_out}");
}

/// Absent `durability` field (defaults to fsync) — must run cleanly.
#[tokio::test]
async fn durability_absent_defaults_to_fsync() {
    let log = temp_log("absent_ok");
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let noop = activity("noop", () => 99)
fn flow(ctx, _) {{ return ctx.call(noop) }}
let r = await run(flow, nil, {{ log: "{log}" }})
print(r)
"#
    );
    let (tw_out, tw_err) = tw_run(&src).await;
    let (vm_out, vm_err) = vm_run(&src).await;
    assert!(tw_err.is_none(), "tree-walker must not error when durability absent: {:?}", tw_err);
    assert!(vm_err.is_none(), "vm must not error when durability absent: {:?}", vm_err);
    assert_eq!(tw_out.trim(), "99", "tree-walker: {tw_out}");
    assert_eq!(vm_out.trim(), "99", "vm: {vm_out}");
}

// ─── §4.2 "group" parsing + defaults + overrides ─────────────────────────────

/// `"group"` is accepted with no overrides, using defaults (window 50 ms, max 128).
#[tokio::test]
async fn durability_group_accepted_defaults() {
    let log = temp_log("group_default");
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let noop = activity("noop", () => 7)
fn flow(ctx, _) {{ return ctx.call(noop) }}
let r = await run(flow, nil, {{ log: "{log}", durability: "group" }})
print(r)
"#
    );
    let (tw_out, tw_err) = tw_run(&src).await;
    let (vm_out, vm_err) = vm_run(&src).await;
    assert!(tw_err.is_none(), "tree-walker must not error on 'group': {:?}", tw_err);
    assert!(vm_err.is_none(), "vm must not error on 'group': {:?}", vm_err);
    assert_eq!(tw_out.trim(), "7", "tree-walker: {tw_out}");
    assert_eq!(vm_out.trim(), "7", "vm: {vm_out}");
}

/// `"group"` with explicit `groupWindowMs` and `groupMaxEvents` overrides.
#[tokio::test]
async fn durability_group_accepted_with_overrides() {
    let log = temp_log("group_overrides");
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let noop = activity("noop", () => 8)
fn flow(ctx, _) {{ return ctx.call(noop) }}
let r = await run(flow, nil, {{ log: "{log}", durability: "group", groupWindowMs: 100, groupMaxEvents: 32 }})
print(r)
"#
    );
    let (tw_out, tw_err) = tw_run(&src).await;
    let (vm_out, vm_err) = vm_run(&src).await;
    assert!(tw_err.is_none(), "tree-walker: {:?}", tw_err);
    assert!(vm_err.is_none(), "vm: {:?}", vm_err);
    assert_eq!(tw_out.trim(), "8", "tree-walker: {tw_out}");
    assert_eq!(vm_out.trim(), "8", "vm: {vm_out}");
}

// ─── §4.2 "group" parameter validation ───────────────────────────────────────

/// Non-positive `groupWindowMs` must produce a Tier-2 error (both engines, identical).
#[tokio::test]
async fn group_window_ms_zero_errors_both_engines() {
    let log = temp_log("win_zero");
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let noop = activity("noop", () => 1)
fn flow(ctx, _) {{ return ctx.call(noop) }}
await run(flow, nil, {{ log: "{log}", durability: "group", groupWindowMs: 0 }})
"#
    );
    let (_, tw_err) = tw_run(&src).await;
    let (_, vm_err) = vm_run(&src).await;
    let tw_err = tw_err.expect("tw must error on groupWindowMs=0");
    let vm_err = vm_err.expect("vm must error on groupWindowMs=0");
    assert!(
        tw_err.contains("groupWindowMs"),
        "error must mention groupWindowMs: {tw_err}"
    );
    assert_eq!(tw_err, vm_err, "diverged:\n  tw: {tw_err:?}\n  vm: {vm_err:?}");
}

/// Negative `groupWindowMs` must produce a Tier-2 error.
#[tokio::test]
async fn group_window_ms_negative_errors_both_engines() {
    let log = temp_log("win_neg");
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let noop = activity("noop", () => 1)
fn flow(ctx, _) {{ return ctx.call(noop) }}
await run(flow, nil, {{ log: "{log}", durability: "group", groupWindowMs: -5 }})
"#
    );
    let (_, tw_err) = tw_run(&src).await;
    let (_, vm_err) = vm_run(&src).await;
    let tw_err = tw_err.expect("tw must error on groupWindowMs=-5");
    let vm_err = vm_err.expect("vm must error on groupWindowMs=-5");
    assert!(tw_err.contains("groupWindowMs"), "{tw_err}");
    assert_eq!(tw_err, vm_err, "diverged:\n  tw: {tw_err:?}\n  vm: {vm_err:?}");
}

/// Non-finite `groupWindowMs` (NaN) must produce a Tier-2 error.
#[tokio::test]
async fn group_window_ms_nan_errors_both_engines() {
    let log = temp_log("win_nan");
    // AScript: 0.0/0.0 produces NaN (IEEE-754, no integer division).
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let noop = activity("noop", () => 1)
fn flow(ctx, _) {{ return ctx.call(noop) }}
let nan_val = 0.0 / 0.0
await run(flow, nil, {{ log: "{log}", durability: "group", groupWindowMs: nan_val }})
"#
    );
    let (_, tw_err) = tw_run(&src).await;
    let (_, vm_err) = vm_run(&src).await;
    let tw_err = tw_err.expect("tw must error on groupWindowMs=NaN");
    let vm_err = vm_err.expect("vm must error on groupWindowMs=NaN");
    assert!(tw_err.contains("groupWindowMs"), "{tw_err}");
    assert_eq!(tw_err, vm_err, "diverged:\n  tw: {tw_err:?}\n  vm: {vm_err:?}");
}

/// Non-positive `groupMaxEvents` (zero) must produce a Tier-2 error.
#[tokio::test]
async fn group_max_events_zero_errors_both_engines() {
    let log = temp_log("max_zero");
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let noop = activity("noop", () => 1)
fn flow(ctx, _) {{ return ctx.call(noop) }}
await run(flow, nil, {{ log: "{log}", durability: "group", groupMaxEvents: 0 }})
"#
    );
    let (_, tw_err) = tw_run(&src).await;
    let (_, vm_err) = vm_run(&src).await;
    let tw_err = tw_err.expect("tw must error on groupMaxEvents=0");
    let vm_err = vm_err.expect("vm must error on groupMaxEvents=0");
    assert!(
        tw_err.contains("groupMaxEvents"),
        "error must mention groupMaxEvents: {tw_err}"
    );
    assert_eq!(tw_err, vm_err, "diverged:\n  tw: {tw_err:?}\n  vm: {vm_err:?}");
}

/// Negative `groupMaxEvents` must produce a Tier-2 error.
#[tokio::test]
async fn group_max_events_negative_errors_both_engines() {
    let log = temp_log("max_neg");
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let noop = activity("noop", () => 1)
fn flow(ctx, _) {{ return ctx.call(noop) }}
await run(flow, nil, {{ log: "{log}", durability: "group", groupMaxEvents: -1 }})
"#
    );
    let (_, tw_err) = tw_run(&src).await;
    let (_, vm_err) = vm_run(&src).await;
    let tw_err = tw_err.expect("tw must error on groupMaxEvents=-1");
    let vm_err = vm_err.expect("vm must error on groupMaxEvents=-1");
    assert!(tw_err.contains("groupMaxEvents"), "{tw_err}");
    assert_eq!(tw_err, vm_err, "diverged:\n  tw: {tw_err:?}\n  vm: {vm_err:?}");
}

// ─── Byte-identity: fsync/buffered/absent produce identical output across engines

/// `"fsync"` and absent produce the same output (both are the fsync path).
#[tokio::test]
async fn fsync_explicit_and_absent_produce_same_output() {
    let log_a = temp_log("same_fsync");
    let log_b = temp_log("same_absent");
    let body = r#"
import { run, activity } from "std/workflow"
let add = activity("add", (n) => n + 10)
fn flow(ctx, input) { return ctx.call(add, input) }
"#;
    let src_a = format!(
        r#"{}
let r = await run(flow, 5, {{ log: "{log}", durability: "fsync" }})
print(r)
"#,
        body,
        log = log_a
    );
    let src_b = format!(
        r#"{}
let r = await run(flow, 5, {{ log: "{log}" }})
print(r)
"#,
        body,
        log = log_b
    );
    let (tw_a, err_a) = tw_run(&src_a).await;
    let (tw_b, err_b) = tw_run(&src_b).await;
    assert!(err_a.is_none(), "{:?}", err_a);
    assert!(err_b.is_none(), "{:?}", err_b);
    assert_eq!(tw_a, tw_b, "fsync explicit vs absent must produce same tw output");

    let (vm_a, verr_a) = vm_run(&src_a).await;
    let (vm_b, verr_b) = vm_run(&src_b).await;
    assert!(verr_a.is_none(), "{:?}", verr_a);
    assert!(verr_b.is_none(), "{:?}", verr_b);
    assert_eq!(vm_a, vm_b, "fsync explicit vs absent must produce same vm output");
    assert_eq!(vm_a, tw_a, "tw vs vm must produce same output");
}
