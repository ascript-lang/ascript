//! SP9 §2/§4 — durable-execution (`std/workflow`) oracle.
//!
//! Record → simulate crash (truncate the log mid-activity) → resume → byte-identical
//! result; replay-mismatch (non-determinism) detection; idempotent resume of a
//! completed log; durable `ctx.sleep`; the `--no-default-features` unknown-module
//! symmetry. These run the BUILT binary (which uses the VM by default and the
//! enlarged worker stack) over temp-file logs.
#![cfg(feature = "workflow")]

use std::process::Command;

/// Run a `.as` program through the built binary, returning (stdout, exit code).
fn run_as(src: &str, tag: &str) -> (String, Option<i32>) {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    tag.hash(&mut h);
    let file = std::env::temp_dir().join(format!("ascript_wf_{}_{:x}.as", tag, h.finish()));
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ascript"))
        .arg("run")
        .arg(&file)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        out.status.code(),
    )
}

/// A unique temp log path for a test.
fn temp_log(name: &str) -> String {
    let p = std::env::temp_dir().join(format!("ascript_wf_{name}_{}.log", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p.to_string_lossy().into_owned()
}

#[test]
fn record_then_resume_byte_identical_result() {
    // A workflow with two activities; record, then resume the COMPLETED log →
    // idempotent (returns the recorded result, re-runs nothing). A side-effect file
    // counter proves the activities do NOT re-execute on the idempotent resume.
    let log = temp_log("record_resume");
    let counter = temp_log("record_resume_counter").replace(".log", ".cnt");
    let _ = std::fs::remove_file(&counter);

    let src = format!(
        r#"
import {{ run, resume, activity }} from "std/workflow"
import {{ read, write, exists }} from "std/fs"
import {{ toNumber }} from "std/convert"

fn bumpCounter() {{
  let prev = exists("{counter}") ? toNumber(read("{counter}")[0]) : 0
  write("{counter}", `${{prev + 1}}`)
}}

let bump = activity("bump", (n) => {{
  bumpCounter()
  return n * 2
}})

fn flow(ctx, input) {{
  let a = ctx.call(bump, input)
  let b = ctx.call(bump, a)
  return b
}}

let r1 = await run(flow, 5, {{ log: "{log}" }})
print(r1)
let r2 = await resume(flow, 5, {{ log: "{log}" }})
print(r2)
print(read("{counter}")[0])
"#,
        log = log,
        counter = counter
    );
    let (out, code) = run_as(&src, "record_resume");
    assert_eq!(code, Some(0), "workflow program should exit 0; got: {out}");
    let lines: Vec<&str> = out.lines().collect();
    // r1 == r2 (idempotent), and the counter is 2 (each activity ran exactly once
    // during record; the idempotent resume re-ran nothing).
    assert_eq!(lines[0], "20", "record result");
    assert_eq!(lines[1], "20", "resume result equals record (idempotent)");
    // NUM §4: the counter file holds toNumber()+1, which is a float → "2.0".
    assert_eq!(lines[2], "2.0", "activities ran exactly twice total (not on resume)");
}

#[test]
fn crash_midway_then_resume_replays_first_executes_rest() {
    // Record a workflow but TRUNCATE the log after the first ActivityCompleted
    // (simulating a crash mid-second-activity). Resume: the first activity REPLAYS
    // (its side effect does NOT fire again), the second EXECUTES for real, and the
    // final result matches a clean run. A counter file proves the first activity's
    // side effect fired only once across record+resume.
    let log = temp_log("crash");
    let counter = temp_log("crash_counter").replace(".log", ".cnt");
    let _ = std::fs::remove_file(&counter);

    // Phase 1: record, then truncate the log to just the first ActivityCompleted.
    let record_src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
import {{ read, write, exists }} from "std/fs"
import {{ toNumber }} from "std/convert"
let step = activity("step", (n) => {{
  let prev = exists("{counter}") ? toNumber(read("{counter}")[0]) : 0
  write("{counter}", `${{prev + 1}}`)
  return n + 100
}})
fn flow(ctx, input) {{
  let a = ctx.call(step, input)
  let b = ctx.call(step, a)
  return b
}}
let r = await run(flow, 1, {{ log: "{log}" }})
print(r)
"#,
        log = log,
        counter = counter
    );
    let (out, code) = run_as(&record_src, "crash_record");
    assert_eq!(code, Some(0), "record run: {out}");
    assert_eq!(out.lines().next(), Some("201"), "clean record result 1→101→201");
    let counter_after_record = std::fs::read_to_string(&counter).unwrap();
    // NUM §4: the counter file holds toNumber()+1, a float → "2.0".
    assert_eq!(counter_after_record.trim(), "2.0", "both activities ran in record");

    // Truncate the log to ONLY the first ActivityCompleted line (the crash point).
    let full = std::fs::read_to_string(&log).unwrap();
    let first_line = full.lines().next().unwrap();
    std::fs::write(&log, format!("{first_line}\n")).unwrap();
    // Reset the counter so we can observe what re-executes on resume.
    std::fs::write(&counter, "0").unwrap();

    // Phase 2: resume against the truncated log.
    let resume_src = format!(
        r#"
import {{ resume, activity }} from "std/workflow"
import {{ read, write, exists }} from "std/fs"
import {{ toNumber }} from "std/convert"
let step = activity("step", (n) => {{
  let prev = exists("{counter}") ? toNumber(read("{counter}")[0]) : 0
  write("{counter}", `${{prev + 1}}`)
  return n + 100
}})
fn flow(ctx, input) {{
  let a = ctx.call(step, input)
  let b = ctx.call(step, a)
  return b
}}
let r = await resume(flow, 1, {{ log: "{log}" }})
print(r)
"#,
        log = log,
        counter = counter
    );
    let (out2, code2) = run_as(&resume_src, "crash_resume");
    assert_eq!(code2, Some(0), "resume run: {out2}");
    assert_eq!(out2.lines().next(), Some("201"), "resume result matches clean run");
    // Only the SECOND activity executed on resume (the first replayed from the log).
    let counter_after_resume = std::fs::read_to_string(&counter).unwrap();
    assert_eq!(
        counter_after_resume.trim(),
        // NUM §4: the counter file holds toNumber()+1, a float → "1.0".
        "1.0",
        "exactly one activity executed on resume (first replayed, not re-run)"
    );
}

#[test]
fn replay_mismatch_is_detected() {
    // Record a workflow, then resume a workflow whose activity ORDER changed →
    // a Tier-2 non-determinism panic.
    let log = temp_log("mismatch");
    let record_src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let a = activity("a", (x) => x + 1)
let b = activity("b", (x) => x + 2)
fn flow(ctx, input) {{
  let p = ctx.call(a, input)
  let q = ctx.call(b, p)
  return q
}}
let r = await run(flow, 0, {{ log: "{log}" }})
print(r)
"#,
        log = log
    );
    let (_o, code) = run_as(&record_src, "mismatch_record");
    assert_eq!(code, Some(0));
    // Truncate to force a re-run from the top on resume.
    let full = std::fs::read_to_string(&log).unwrap();
    let first = full.lines().next().unwrap();
    std::fs::write(&log, format!("{first}\n")).unwrap();

    // Resume with the activity order SWAPPED (b before a) → mismatch on the first
    // effect (recorded "a", got "b").
    let resume_src = format!(
        r#"
import {{ resume, activity }} from "std/workflow"
let a = activity("a", (x) => x + 1)
let b = activity("b", (x) => x + 2)
fn flow(ctx, input) {{
  let p = ctx.call(b, input)
  let q = ctx.call(a, p)
  return q
}}
let r = await resume(flow, 0, {{ log: "{log}" }})
print(r)
"#,
        log = log
    );
    let (out, code) = run_as(&resume_src, "mismatch_resume");
    assert_ne!(code, Some(0), "non-deterministic resume must fail");
    assert!(
        out.contains("workflow non-determinism"),
        "expected a non-determinism error, got: {out}"
    );
}

#[test]
fn durable_sleep_advances_clock_without_real_delay() {
    let log = temp_log("sleep");
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let noop = activity("noop", (x) => x)
fn flow(ctx, input) {{
  let t0 = ctx.now()
  ctx.sleep(3600000)
  let t1 = ctx.now()
  return t1 - t0
}}
let r = await run(flow, 0, {{ log: "{log}" }})
print(r)
"#,
        log = log
    );
    let started = std::time::Instant::now();
    let (out, code) = run_as(&src, "sleep");
    assert_eq!(code, Some(0), "{out}");
    // NUM §4: the clock delta comes from time (float) → "3600000.0".
    assert_eq!(out.lines().next(), Some("3600000.0"), "virtual clock advanced 1h");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "durable sleep must not block real time"
    );
}

/// Task 0.19c: a workflow that draws seeded BYTES (`ctx.uuid()` + `uuid.v4` +
/// `crypto.randomBytes` — all three route through `fill_seeded_bytes`) records
/// `BytesRead` events and, on an idempotent resume of the COMPLETED log, reproduces
/// them BYTE-IDENTICALLY from the events. Before 0.19c these draws were seed-reproducible
/// but NOT event-sourced; this asserts the end-to-end faithful path.
#[cfg(all(feature = "data", feature = "crypto"))]
#[test]
fn workflow_seeded_byte_draws_replay_byte_identical() {
    let log = temp_log("byte_draws");
    let src = format!(
        r#"
import {{ run, resume }} from "std/workflow"
import {{ v4 }} from "std/uuid"
import {{ randomBytes }} from "std/crypto"
import {{ hexEncode }} from "std/encoding"

fn flow(ctx, input) {{
  let cid = ctx.uuid()
  let id = v4()
  let rb = hexEncode(randomBytes(8))
  return `${{cid}}|${{id}}|${{rb}}`
}}

let r1 = await run(flow, 0, {{ log: "{log}" }})
print(r1)
let r2 = await resume(flow, 0, {{ log: "{log}" }})
print(r2)
"#,
        log = log
    );
    let (out, code) = run_as(&src, "byte_draws");
    assert_eq!(code, Some(0), "workflow program should exit 0; got: {out}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "expected record + resume output, got: {out}");
    assert_eq!(
        lines[0], lines[1],
        "resume must reproduce the recorded ctx.uuid + uuid.v4 + random bytes byte-identically (from BytesRead events)"
    );
    // The log must actually carry BytesRead events (proves the draws were event-sourced,
    // not bypassed).
    let log_text = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        log_text.contains("\"kind\":\"BytesRead\""),
        "the workflow log must contain BytesRead events; got log:\n{log_text}"
    );
}

#[test]
fn non_serializable_activity_result_is_a_constraint_violation() {
    let log = temp_log("nonser");
    // An activity that returns a function (non-serializable) → constraint violation
    // at record time.
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
let bad = activity("bad", (x) => ((y) => y))
fn flow(ctx, input) {{
  return ctx.call(bad, input)
}}
let r = await run(flow, 0, {{ log: "{log}" }})
print(r)
"#,
        log = log
    );
    let (out, code) = run_as(&src, "nonser");
    assert_ne!(code, Some(0), "non-serializable result must fail");
    assert!(
        out.contains("not serializable"),
        "expected a serialization-constraint error, got: {out}"
    );
}

/// WARM §0.1 / §4.1 baseline pin: the workflow log is written ONCE per `run()` call (at
/// finish), NOT per activity event. A mid-run activity therefore observes NO log file at
/// the log path — a `kill -9` mid-workflow today loses every recorded event. This test
/// documents the shipped contract that Unit C's `"group"` mode improves on; if someone
/// later adds an incremental per-event write to the default `"fsync"` path, this trips
/// and the spec table must be revisited.
#[test]
fn fsync_mode_writes_nothing_until_finish() {
    let log = temp_log("writes_nothing_until_finish");
    // The activity checks whether the log file exists MID-RUN and records "exists" or
    // "not_exists" as its return value. After run() we assert: activity saw "not_exists"
    // (log was not written yet) AND the log file now exists (written at finish).
    let src = format!(
        r#"
import {{ run, activity }} from "std/workflow"
import {{ exists }} from "std/fs"

let check = activity("check_log_mid_run", (_) => {{
  return exists("{log}") ? "exists" : "not_exists"
}})

fn flow(ctx, input) {{
  return ctx.call(check, nil)
}}

let result = await run(flow, nil, {{ log: "{log}" }})
print(result)
"#,
        log = log
    );
    let (out, code) = run_as(&src, "fsync_writes_nothing_until_finish");
    assert_eq!(code, Some(0), "workflow program should exit 0; got: {out}");
    assert_eq!(
        out.trim(),
        "not_exists",
        "WARM §0.1 contract violated: log was written mid-run (before finish_workflow) \
         with default fsync durability; got activity result: {out}"
    );
    // After run() returns, the log MUST exist (finish_workflow wrote it).
    assert!(
        std::path::Path::new(&log).exists(),
        "WARM §0.1: log file must exist after run() returns, but was not found at {log}"
    );
}
