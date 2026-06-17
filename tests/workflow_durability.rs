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

// ─── Task 10 (WARM C §4.3/§4.4): group appender — write-at-record-time, crc,
//     torn-tail prefix repair, seq-discontinuity. ──────────────────────────────

/// A side-file path unique per test, removed if present.
fn temp_marker(name: &str) -> String {
    let p = std::env::temp_dir().join(format!(
        "ascript_wfm_{name}_{}_{}_{:?}.txt",
        std::process::id(),
        std::thread::current().name().unwrap_or("t"),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&p);
    p.to_string_lossy().into_owned()
}

/// Count `ActivityCompleted` lines in a log file's text.
fn count_activity_lines(text: &str) -> usize {
    text.lines()
        .filter(|l| l.contains("\"kind\":\"ActivityCompleted\"") || l.contains("\"kind\": \"ActivityCompleted\""))
        .count()
}

/// §4.3 write-at-record-time: under "group", a 3-activity workflow where activity
/// k asserts (via std/fs, inside the activity) the log already holds k-1
/// ActivityCompleted lines. Proves each record is appended (and in the OS page cache)
/// the moment it is recorded — the kill-9 guarantee's mechanism.
#[tokio::test]
async fn group_appends_each_event_as_it_is_recorded() {
    let log = temp_log("group_record_time");
    // Each activity reads the log and counts ActivityCompleted lines BEFORE its own
    // record is written; activity index i (0-based) must see exactly i prior lines.
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
import {{ read, exists }} from "std/fs"
import {{ split, contains }} from "std/string"

fn count_lines() {{
    if (exists("{log}") == false) {{ return 0 }}
    let [text, _] = read("{log}")
    if (text == nil) {{ return 0 }}
    let n = 0
    for (line in split(text, "\n")) {{
        if (contains(line, "ActivityCompleted")) {{ n = n + 1 }}
    }}
    return n
}}

let a0 = activity("a0", () => count_lines())
let a1 = activity("a1", () => count_lines())
let a2 = activity("a2", () => count_lines())

fn flow(ctx, _) {{
    let r0 = ctx.call(a0)
    let r1 = ctx.call(a1)
    let r2 = ctx.call(a2)
    return [r0, r1, r2]
}}
let r = await run(flow, nil, {{ log: "{log}", durability: "group" }})
print(r)
"#
    );
    let (out, err) = tw_run(&src).await;
    assert!(err.is_none(), "group write-at-record-time errored: {:?}", err);
    // Each activity sees the count of PRIOR ActivityCompleted records: [0, 1, 2].
    assert_eq!(out.trim(), "[0, 1, 2]", "activities must observe incremental appends: {out}");
}

/// §4.4 crc framing + idempotent resume: after a group run, every appended line
/// parses, carries "crc", and the crc verifies; resume() returns the recorded result.
#[tokio::test]
async fn group_records_carry_crc_and_resume_replays_them() {
    let log = temp_log("group_crc");
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let add = activity("add", (n) => n + 100)
fn flow(ctx, input) {{
    let a = ctx.call(add, input)
    let b = ctx.call(add, a)
    return b
}}
let r = await run(flow, 1, {{ log: "{log}", durability: "group" }})
print(r)
"#
    );
    let (out, err) = tw_run(&src).await;
    assert!(err.is_none(), "group run errored: {:?}", err);
    assert_eq!(out.trim(), "201", "1 + 100 + 100 = 201: {out}");

    // Every line of the produced log parses as JSON and carries a "crc" field.
    let text = std::fs::read_to_string(&log).expect("group log must exist");
    assert!(!text.is_empty(), "group log must be non-empty");
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("group line must be valid JSON: {e}\nline: {line}"));
        assert!(
            v.get("crc").is_some(),
            "every appended group record must carry a crc: {line}"
        );
        // The crc must verify: recompute over the record bytes sans crc.
        assert!(
            crc_line_verifies(line),
            "crc must verify for line: {line}"
        );
    }

    // Resume on the COMPLETED log is idempotent — returns the recorded result without
    // re-running the workflow.
    let resume_src = format!(
        r#"
import {{ resume, activity }} from "std/workflow"
let add = activity("add", (n) => n + 100)
fn flow(ctx, input) {{
    let a = ctx.call(add, input)
    let b = ctx.call(add, a)
    return b
}}
let r = await resume(flow, 1, {{ log: "{log}", durability: "group" }})
print(r)
"#
    );
    let (rout, rerr) = tw_run(&resume_src).await;
    assert!(rerr.is_none(), "resume errored: {:?}", rerr);
    assert_eq!(rout.trim(), "201", "resume must replay the recorded result: {rout}");
}

/// Recompute the crc over a group log line's JSON object (sans the "crc" field) and
/// compare to the carried crc. Mirrors the production hand-rolled CRC32 over the
/// canonical record bytes. Uses the SAME serialization the appender uses: the record
/// JSON object with no "crc" key, serialized via serde_json (compact).
fn crc_line_verifies(line: &str) -> bool {
    let v: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let obj = match v.as_object() {
        Some(o) => o,
        None => return false,
    };
    let carried = match obj.get("crc").and_then(|c| c.as_u64()) {
        Some(c) => c as u32,
        None => return false,
    };
    // Rebuild the object WITHOUT the crc field, in the same key order the producer
    // used (the producer computes crc over the record before adding crc, then adds crc
    // LAST — so removing crc and re-serializing reproduces the exact bytes).
    let mut without = obj.clone();
    without.remove("crc");
    let rebuilt = serde_json::Value::Object(without).to_string();
    crc32_ref(rebuilt.as_bytes()) == carried
}

/// Reference CRC32 (IEEE, reflected) — the same hand-rolled algorithm the production
/// appender uses. Bitwise form (no table) for an independent cross-check.
fn crc32_ref(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Build the 5-activity marker workflow source for a given verb ("run"/"resume").
/// Each activity appends "<name>\n" to the marker file (the side effect), and returns
/// its index. The workflow sums the indices.
fn marker_flow_src(verb: &str, log: &str, marker: &str) -> String {
    format!(
        r#"
import {{ {verb}, activity }} from "std/workflow"
import {{ append }} from "std/fs"

fn mk(name, idx) {{
    return activity(name, () => {{
        append("{marker}", name + "\n")
        return idx
    }})
}}
let a0 = mk("a0", 0)
let a1 = mk("a1", 1)
let a2 = mk("a2", 2)
let a3 = mk("a3", 3)
let a4 = mk("a4", 4)

fn flow(ctx, _) {{
    let s = 0
    s = s + ctx.call(a0)
    s = s + ctx.call(a1)
    s = s + ctx.call(a2)
    s = s + ctx.call(a3)
    s = s + ctx.call(a4)
    return s
}}
let r = await {verb}(flow, nil, {{ log: "{log}", durability: "group" }})
print(r)
"#
    )
}

/// §4.4 PROPERTY BATTERY: a valid 5-activity group log, truncated at EVERY byte
/// offset, must repair (prefix-truncation) + resume to the correct final result; the
/// repaired prefix is valid; activities lost to truncation re-execute (markers grow),
/// replayed ones do NOT double.
#[tokio::test]
async fn torn_tail_is_repaired_by_prefix_truncation() {
    // 1) Produce a valid group log + the baseline marker count (5, one per activity).
    let base_log = temp_log("torn_base");
    let base_marker = temp_marker("torn_base");
    let run_src = marker_flow_src("run", &base_log, &base_marker);
    let (out, err) = tw_run(&run_src).await;
    assert!(err.is_none(), "baseline run errored: {:?}", err);
    assert_eq!(out.trim(), "10", "0+1+2+3+4 = 10: {out}");
    let base_markers = std::fs::read_to_string(&base_marker).unwrap();
    assert_eq!(base_markers.lines().count(), 5, "baseline must run 5 activities");

    let full = std::fs::read(&base_log).expect("base log");
    let len = full.len();
    assert!(len > 0);

    // 2) For EVERY truncation offset, copy the prefix into a fresh log, resume, and
    //    assert: completes with result 10; repaired log valid; total markers == 5
    //    (lost activities re-execute exactly once, replayed ones do not double).
    for t in 0..=len {
        let log = temp_log(&format!("torn_t{t}"));
        let marker = temp_marker(&format!("torn_t{t}"));
        std::fs::write(&log, &full[..t]).unwrap();

        let resume_src = marker_flow_src("resume", &log, &marker);
        let (rout, rerr) = tw_run(&resume_src).await;
        assert!(
            rerr.is_none(),
            "resume at truncation offset {t}/{len} errored: {:?}",
            rerr
        );
        assert_eq!(
            rout.trim(),
            "10",
            "resume at offset {t}/{len} must complete with the correct result"
        );

        // The total markers across the (lost) original run + this resume must be 5:
        // replayed activities did NOT re-execute (no marker), lost ones DID (one
        // marker). Since base run already wrote some markers to base_marker (not this
        // marker file), here we only see the RE-EXECUTED activities of THIS resume.
        // The completed log after resume must hold all 5 activities + completion.
        let final_text = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            count_activity_lines(&final_text),
            5,
            "offset {t}/{len}: the completed log must hold all 5 ActivityCompleted records"
        );
        assert!(
            final_text.lines().any(|l| l.contains("WorkflowCompleted")),
            "offset {t}/{len}: resumed workflow must end with WorkflowCompleted"
        );
        // The repaired prefix is valid: every line parses as JSON (the torn tail was
        // truncated away, never left half-written).
        for line in final_text.lines().filter(|l| !l.trim().is_empty()) {
            assert!(
                serde_json::from_str::<serde_json::Value>(line).is_ok(),
                "offset {t}/{len}: repaired log line must be valid JSON: {line}"
            );
        }

        // Markers re-executed THIS resume = number of activities AFTER the repaired
        // prefix's recorded activities. Replayed activities did not double: the marker
        // count for this resume must equal (5 - replayed_count), and replayed_count is
        // the number of complete ActivityCompleted records in the repaired prefix.
        let prefix_text = String::from_utf8_lossy(&full[..t]);
        let replayed = count_recorded_prefix_activities(&prefix_text);
        let this_markers = std::fs::read_to_string(&marker)
            .map(|s| s.lines().count())
            .unwrap_or(0);
        assert_eq!(
            this_markers,
            5 - replayed,
            "offset {t}/{len}: re-executed activities ({this_markers}) must be 5 minus the \
             replayed prefix activities ({replayed}) — replayed ones must NOT double"
        );

        let _ = std::fs::remove_file(&log);
        let _ = std::fs::remove_file(&marker);
    }
    let _ = std::fs::remove_file(&base_log);
    let _ = std::fs::remove_file(&base_marker);
}

/// Count the ActivityCompleted records in a truncated prefix that the repair logic
/// would KEEP — i.e. records in lines that are newline-terminated, valid JSON, crc-OK,
/// and seq-contiguous. This mirrors the production repair so the property test can
/// predict the replayed count independently.
fn count_recorded_prefix_activities(prefix: &str) -> usize {
    let mut kept = 0usize;
    let mut expect_seq = 0i64;
    // Split keeping only newline-terminated lines (a final line without '\n' is torn).
    let mut rest = prefix;
    while let Some(nl) = rest.find('\n') {
        let line = &rest[..nl];
        rest = &rest[nl + 1..];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            break;
        };
        // crc must verify if present.
        if v.get("crc").is_some() && !crc_line_verifies(trimmed) {
            break;
        }
        // seq must be contiguous if present (WorkflowCompleted carries no seq).
        if let Some(seq) = v.get("seq").and_then(|s| s.as_i64()) {
            if seq != expect_seq {
                break;
            }
            expect_seq += 1;
        }
        if v.get("kind").and_then(|k| k.as_str()) == Some("ActivityCompleted") {
            kept += 1;
        }
    }
    kept
}

// ─── Task 11 (WARM C §5): the `kill -9` crash-recovery battery ────────────────
//
// These spawn the REAL binary (the `tests/cli.rs` precedent), drive a workflow to a
// known point, `kill -9` it mid-run, then `resume` in a fresh process and assert the
// EXACT §4.5 loss-window contract:
//
//   * "fsync" (the shipped default): a crash mid-run persists NOTHING from the
//     in-flight run (the whole log is a single temp+rename snapshot written only at
//     finish), so `resume` re-executes EVERY activity — at-least-once, markers for the
//     already-run activities DOUBLE.
//   * "group" (WARM C, Task 10): each event is appended at record-time and lives in the
//     OS page cache the moment the recording call returns, so a `kill -9` loses NOTHING
//     committed. `resume` REPLAYS the persisted prefix (no re-execution, markers do NOT
//     double), executes only the suffix, and a SECOND resume is idempotent.
//   * "group" mid-activity edge: the in-flight activity's `ActivityCompleted` is never
//     recorded (the kill lands inside it), so `resume` re-executes EXACTLY that one
//     activity — the at-least-once boundary the model guarantees.
//
// Unix-gated: `Child::kill()` sends SIGKILL on Unix (uncatchable, kernel-delivered even
// while the debuggee busy-loops). No `libc`/`nix` dependency is introduced.

/// The 5-activity marker workflow, spawned as a real process. `verb` is "run"
/// (the first, killed invocation) or "resume" (the recovery invocation). `kill_point`
/// selects how the process is held after activity 3:
///   * `"between"` — a `spin` activity is inserted BETWEEN activity a2 and a3 ONLY on the
///     run phase; it writes `ready.txt` and busy-loops forever. Events 0..2 (a0,a1,a2) are
///     fully recorded before the spin; the kill lands cleanly between activities.
///   * `"mid"` — activity a3 itself writes `ready.txt` and busy-loops BEFORE appending its
///     marker, so the kill lands MID-activity and a3's `ActivityCompleted` is never
///     recorded.
///
/// Each non-spin activity appends "<name>\n" to `markers.txt` (the at-least-once side
/// effect) and returns its index; the workflow sums the indices (0+1+2+3+4 = 10).
#[cfg(unix)]
fn kill9_program(kill_point: &str) -> String {
    // The activity bodies differ by kill_point: in "mid" mode a3 carries the
    // signal+spin; in "between" mode a separate `spin` activity carries it.
    let (a3_body, spin_step) = match kill_point {
        "mid" => (
            // a3: on the run phase, signal + busy-loop BEFORE recording its marker.
            r#"if (verb == "run") { write(ready, "ready\n"); let i = 0; while (true) { i = i + 1 } }
        append(marker, "a3\n")
        return 3"#,
            // no separate spin step in "between" position
            "",
        ),
        _ => (
            // a3: an ordinary activity.
            r#"append(marker, "a3\n")
        return 3"#,
            // a `spin` activity inserted between a2 and a3, run-phase only.
            r#"if (verb == "run") { ctx.call(spin) }"#,
        ),
    };
    format!(
        r#"
import {{ run, resume, activity }} from "std/workflow"
import {{ append, write }} from "std/fs"
import {{ args }} from "std/env"

let dir = args()[0]
let dur = args()[1]
let verb = args()[2]
let marker = dir + "/markers.txt"
let ready = dir + "/ready.txt"
let log = dir + "/wf.log"

fn mk(name, idx) {{
    return activity(name, () => {{
        append(marker, name + "\n")
        return idx
    }})
}}
let a0 = mk("a0", 0)
let a1 = mk("a1", 1)
let a2 = mk("a2", 2)
let a4 = mk("a4", 4)
let a3 = activity("a3", () => {{
        {a3_body}
}})
// The `spin` activity (used only in "between" kill_point, run phase) signals ready and
// busy-loops forever so the parent can kill it; on resume it is never reached.
let spin = activity("spin", () => {{
    write(ready, "ready\n")
    let i = 0
    while (true) {{ i = i + 1 }}
    return 0
}})

fn flow(ctx, _) {{
    let s = 0
    s = s + ctx.call(a0)
    s = s + ctx.call(a1)
    s = s + ctx.call(a2)
    {spin_step}
    s = s + ctx.call(a3)
    s = s + ctx.call(a4)
    return s
}}

if (verb == "run") {{
    let r = await run(flow, nil, {{ log: log, durability: dur }})
    print(r)
}} else {{
    let r = await resume(flow, nil, {{ log: log, durability: dur }})
    print(r)
}}
"#
    )
}

/// A unique temp DIRECTORY for one kill-9 case (program file + markers + ready + log all
/// live inside it). Created fresh; the caller cleans it up.
#[cfg(unix)]
fn kill9_dir(name: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "ascript_kill9_{name}_{}_{:?}_{n}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create kill9 temp dir");
    dir
}

/// Spawn `ascript run <prog> -- <dir> <durability> run`, poll for `ready.txt` (bounded,
/// 30s), then `kill -9` (SIGKILL via `Child::kill` on Unix) and reap. Panics if the
/// child never signals readiness (a hang is a real bug, never silently swallowed).
#[cfg(unix)]
fn run_until_ready_then_kill9(dir: &std::path::Path, prog: &std::path::Path, durability: &str) {
    use std::process::Command;
    use std::time::{Duration, Instant};

    let bin = env!("CARGO_BIN_EXE_ascript");
    let ready = dir.join("ready.txt");
    let mut child = Command::new(bin)
        .arg("run")
        .arg(prog)
        .arg("--")
        .arg(dir)
        .arg(durability)
        .arg("run")
        // Discard child stdout/stderr — we observe via the filesystem (markers/log).
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn ascript run");

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if ready.exists() {
            break;
        }
        // If the child already exited (e.g. a compile error) before signalling, surface
        // it loudly instead of polling to the timeout.
        if let Ok(Some(status)) = child.try_wait() {
            // Reap is done; the missing ready.txt is the real failure.
            panic!(
                "child exited (status {status:?}) before writing ready.txt — \
                 ready={} markers={:?}",
                ready.display(),
                std::fs::read_to_string(dir.join("markers.txt")).ok()
            );
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "timed out (30s) waiting for ready.txt at {} — markers={:?}",
                ready.display(),
                std::fs::read_to_string(dir.join("markers.txt")).ok()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // SIGKILL (uncatchable, kernel-delivered even while the debuggee busy-loops).
    child.kill().expect("kill -9 the running workflow");
    let _ = child.wait();
}

/// Spawn `ascript run <prog> -- <dir> <durability> resume` to completion and return its
/// trimmed stdout. The recovery invocation skips the spin (verb != "run").
#[cfg(unix)]
fn resume_to_completion(
    dir: &std::path::Path,
    prog: &std::path::Path,
    durability: &str,
) -> String {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = Command::new(bin)
        .arg("run")
        .arg(prog)
        .arg("--")
        .arg(dir)
        .arg(durability)
        .arg("resume")
        .output()
        .expect("spawn ascript run (resume)");
    assert!(
        out.status.success(),
        "resume must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Read the per-activity marker counts from `markers.txt` as a sorted `(name, count)`
/// vector (deterministic ordering for assertions).
#[cfg(unix)]
fn marker_counts(dir: &std::path::Path) -> std::collections::BTreeMap<String, usize> {
    let mut m = std::collections::BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(dir.join("markers.txt")) {
        for line in text.lines() {
            let l = line.trim();
            if !l.is_empty() {
                *m.entry(l.to_string()).or_insert(0) += 1;
            }
        }
    }
    m
}

/// §5 / §4.5 — `"fsync"` mode (the shipped default): a `kill -9` mid-run persists nothing
/// from the in-flight run, so `resume` re-executes EVERY activity. The already-run
/// activities (a0,a1,a2) appear TWICE in markers.txt (at-least-once — today's contract,
/// now pinned end-to-end); a3,a4 once. The final result is correct.
#[cfg(unix)]
#[test]
fn kill9_fsync_mode_loses_in_flight_run_and_reexecutes_all() {
    let dir = kill9_dir("fsync");
    let prog = dir.join("wf.as");
    std::fs::write(&prog, kill9_program("between")).unwrap();

    run_until_ready_then_kill9(&dir, &prog, "fsync");

    // After the kill: the run wrote markers a0,a1,a2 (3), and — because fsync only
    // snapshots at finish — the log is absent (no mid-run persistence).
    let after_kill = marker_counts(&dir);
    assert_eq!(
        after_kill.get("a0").copied().unwrap_or(0),
        1,
        "fsync: a0 ran once before the kill"
    );
    assert_eq!(after_kill.get("a2").copied().unwrap_or(0), 1, "fsync: a2 ran once");
    assert!(
        !dir.join("wf.log").exists()
            || std::fs::read_to_string(dir.join("wf.log")).unwrap().trim().is_empty(),
        "fsync mid-run must NOT have persisted the log (whole-log snapshot only at finish)"
    );

    // Resume completes and re-executes everything.
    let out = resume_to_completion(&dir, &prog, "fsync");
    assert_eq!(out, "10", "fsync resume must complete with 0+1+2+3+4 = 10");

    let counts = marker_counts(&dir);
    assert_eq!(counts.get("a0").copied(), Some(2), "fsync: a0 re-executed (doubled): {counts:?}");
    assert_eq!(counts.get("a1").copied(), Some(2), "fsync: a1 re-executed (doubled): {counts:?}");
    assert_eq!(counts.get("a2").copied(), Some(2), "fsync: a2 re-executed (doubled): {counts:?}");
    assert_eq!(counts.get("a3").copied(), Some(1), "fsync: a3 ran once: {counts:?}");
    assert_eq!(counts.get("a4").copied(), Some(1), "fsync: a4 ran once: {counts:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// §5 / §4.5 — `"group"` mode: a `kill -9` mid-run loses NOTHING committed (records are in
/// the OS page cache the moment the recording call returns). After the kill the log holds
/// events 0..2; `resume` REPLAYS that prefix (markers a0,a1,a2 do NOT double), executes
/// a3,a4, completes with the correct result, and the log ends with `WorkflowCompleted`. A
/// SECOND resume is idempotent: it returns the recorded result, markers unchanged.
#[cfg(unix)]
#[test]
fn kill9_group_mode_loses_nothing_and_replays_the_prefix() {
    let dir = kill9_dir("group");
    let prog = dir.join("wf.as");
    std::fs::write(&prog, kill9_program("between")).unwrap();

    run_until_ready_then_kill9(&dir, &prog, "group");

    // After the kill: markers a0,a1,a2 (3, each once), and the log holds the persisted
    // prefix (events 0,1,2) — page cache survives process death.
    let after_kill = marker_counts(&dir);
    assert_eq!(after_kill.get("a0").copied(), Some(1), "group: a0 once pre-kill: {after_kill:?}");
    assert_eq!(after_kill.get("a1").copied(), Some(1), "group: a1 once pre-kill: {after_kill:?}");
    assert_eq!(after_kill.get("a2").copied(), Some(1), "group: a2 once pre-kill: {after_kill:?}");
    let killed_log = std::fs::read_to_string(dir.join("wf.log"))
        .expect("group: the log must hold the persisted prefix after kill");
    assert_eq!(
        count_activity_lines(&killed_log),
        3,
        "group: the persisted prefix must hold exactly 3 ActivityCompleted records: {killed_log}"
    );
    assert!(
        !killed_log.contains("WorkflowCompleted"),
        "group: a killed mid-run log must NOT carry WorkflowCompleted"
    );

    // First resume: replays the prefix, executes the suffix, completes.
    let out = resume_to_completion(&dir, &prog, "group");
    assert_eq!(out, "10", "group resume must complete with 10");

    let counts = marker_counts(&dir);
    assert_eq!(counts.get("a0").copied(), Some(1), "group: a0 REPLAYED, not re-run: {counts:?}");
    assert_eq!(counts.get("a1").copied(), Some(1), "group: a1 REPLAYED, not re-run: {counts:?}");
    assert_eq!(counts.get("a2").copied(), Some(1), "group: a2 REPLAYED, not re-run: {counts:?}");
    assert_eq!(counts.get("a3").copied(), Some(1), "group: a3 executed once: {counts:?}");
    assert_eq!(counts.get("a4").copied(), Some(1), "group: a4 executed once: {counts:?}");

    let final_log = std::fs::read_to_string(dir.join("wf.log")).unwrap();
    assert_eq!(count_activity_lines(&final_log), 5, "group: completed log holds all 5: {final_log}");
    assert!(
        final_log.lines().any(|l| l.contains("WorkflowCompleted")),
        "group: completed log must end with WorkflowCompleted: {final_log}"
    );

    // SECOND resume is idempotent — returns the recorded result, markers unchanged.
    let out2 = resume_to_completion(&dir, &prog, "group");
    assert_eq!(out2, "10", "group: a second resume must return the recorded result");
    let counts2 = marker_counts(&dir);
    assert_eq!(counts2, counts, "group: a second resume must NOT re-execute any activity: {counts2:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// §5 / §4.5 — `"group"` mid-activity edge: the kill lands INSIDE activity a3, before its
/// `ActivityCompleted` is recorded, so the log holds only events 0..2. `resume` replays
/// a0,a1,a2 (no marker), re-executes EXACTLY a3 (its event was never recorded), then a4 —
/// the in-flight-activity at-least-once boundary. No activity's marker doubles.
#[cfg(unix)]
#[test]
fn kill9_mid_activity_group_mode_reexecutes_only_that_activity() {
    let dir = kill9_dir("midact");
    let prog = dir.join("wf.as");
    std::fs::write(&prog, kill9_program("mid")).unwrap();

    run_until_ready_then_kill9(&dir, &prog, "group");

    // After the kill: a0,a1,a2 ran (3 markers); a3 wrote ready + busy-looped BEFORE its
    // marker, so it has NO marker and NO ActivityCompleted record.
    let after_kill = marker_counts(&dir);
    assert_eq!(after_kill.get("a0").copied(), Some(1), "midact: a0 once: {after_kill:?}");
    assert_eq!(after_kill.get("a1").copied(), Some(1), "midact: a1 once: {after_kill:?}");
    assert_eq!(after_kill.get("a2").copied(), Some(1), "midact: a2 once: {after_kill:?}");
    assert_eq!(after_kill.get("a3").copied(), None, "midact: a3 left no marker (killed mid-activity): {after_kill:?}");
    let killed_log = std::fs::read_to_string(dir.join("wf.log"))
        .expect("midact: the log must hold the prefix after kill");
    assert_eq!(
        count_activity_lines(&killed_log),
        3,
        "midact: only a0..a2 were recorded (a3's event never landed): {killed_log}"
    );

    // Resume: replays a0..a2, re-executes a3 exactly once, then a4.
    let out = resume_to_completion(&dir, &prog, "group");
    assert_eq!(out, "10", "midact resume must complete with 10");

    let counts = marker_counts(&dir);
    assert_eq!(counts.get("a0").copied(), Some(1), "midact: a0 replayed: {counts:?}");
    assert_eq!(counts.get("a1").copied(), Some(1), "midact: a1 replayed: {counts:?}");
    assert_eq!(counts.get("a2").copied(), Some(1), "midact: a2 replayed: {counts:?}");
    assert_eq!(counts.get("a3").copied(), Some(1), "midact: a3 RE-EXECUTED exactly once: {counts:?}");
    assert_eq!(counts.get("a4").copied(), Some(1), "midact: a4 executed once: {counts:?}");

    let final_log = std::fs::read_to_string(dir.join("wf.log")).unwrap();
    assert!(
        final_log.lines().any(|l| l.contains("WorkflowCompleted")),
        "midact: completed log must end with WorkflowCompleted"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// §4.4 seq-discontinuity: hand-edit a mid-file line's seq so it is non-contiguous;
/// the repair truncates from there (the contiguous-prefix rule) and the suffix
/// re-executes on resume — the final result is still correct.
#[tokio::test]
async fn seq_discontinuity_stops_the_prefix() {
    // Produce a valid 5-activity group log.
    let base_log = temp_log("seq_base");
    let base_marker = temp_marker("seq_base");
    let (out, err) = tw_run(&marker_flow_src("run", &base_log, &base_marker)).await;
    assert!(err.is_none(), "{:?}", err);
    assert_eq!(out.trim(), "10");

    let text = std::fs::read_to_string(&base_log).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.len() >= 4, "need several lines to corrupt a mid one");

    // Corrupt the seq of the 3rd line (index 2): set seq to a wildly wrong value.
    // The repair must truncate from this line (discontinuity) → only lines 0,1 kept.
    let mut corrupted = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i == 2 {
            // Rewrite the seq field to a discontinuous value (999) while keeping JSON
            // valid; the crc will then NOT match — but seq-discontinuity alone is the
            // tested stop condition, so rebuild with a CORRECT crc over the new bytes
            // to isolate the seq rule.
            let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
            if let Some(o) = v.as_object_mut() {
                o.insert("seq".to_string(), serde_json::json!(999));
                // Recompute a valid crc so ONLY the seq rule triggers the truncation.
                o.remove("crc");
                let sans = serde_json::Value::Object(o.clone()).to_string();
                let crc = crc32_ref(sans.as_bytes());
                o.insert("crc".to_string(), serde_json::json!(crc));
            }
            corrupted.push_str(&v.to_string());
        } else {
            corrupted.push_str(line);
        }
        corrupted.push('\n');
    }

    let log = temp_log("seq_corrupt");
    let marker = temp_marker("seq_corrupt");
    std::fs::write(&log, &corrupted).unwrap();

    let (rout, rerr) = tw_run(&marker_flow_src("resume", &log, &marker)).await;
    assert!(rerr.is_none(), "resume after seq corruption errored: {:?}", rerr);
    assert_eq!(rout.trim(), "10", "result still correct after seq-discontinuity repair");

    // Lines 0,1 (seq 0,1) are the contiguous prefix → activities a0,a1 replayed (no
    // marker); a2,a3,a4 re-executed → 3 markers in THIS resume.
    let this_markers = std::fs::read_to_string(&marker)
        .map(|s| s.lines().count())
        .unwrap_or(0);
    assert_eq!(
        this_markers, 3,
        "seq-discontinuity at line 2 → 2 replayed, 3 re-executed; got {this_markers} markers"
    );
    // The completed log holds all 5 activities again.
    let final_text = std::fs::read_to_string(&log).unwrap();
    assert_eq!(count_activity_lines(&final_text), 5);

    let _ = std::fs::remove_file(&base_log);
    let _ = std::fs::remove_file(&base_marker);
    let _ = std::fs::remove_file(&log);
    let _ = std::fs::remove_file(&marker);
}
