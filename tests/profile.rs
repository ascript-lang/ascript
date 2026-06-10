//! DBG Task 7 — the CPU sampling profiler, end-to-end through the built binary.
//!
//! These tests drive the DETERMINISTIC sample clock (`--profile-format
//! deterministic-*`) so the output is a pure function of the program's call structure
//! — byte-stable for a golden, with NO wall-clock flakiness and no timer thread.

#![cfg(feature = "profile")]

use std::process::Command;

/// A small CPU-bound program with a known call structure:
/// `<script>` calls `a`, `a` calls `b` (twice), `b` calls `c` (twice), in a loop.
const PROG: &str = "\
fn c(n) { return n + 1 }
fn b(n) { return c(n) + c(n) }
fn a(n) { return b(n) + b(n) }
let total = 0
for (i in 0..3) { total = total + a(i) }
print(total)
";

fn write_prog(name: &str) -> std::path::PathBuf {
    let file = std::env::temp_dir().join(name);
    std::fs::write(&file, PROG).unwrap();
    file
}

#[test]
fn deterministic_collapsed_golden_is_stable() {
    let file = write_prog("ascript_prof_collapsed.as");
    let out = std::env::temp_dir().join("ascript_prof_collapsed.txt");
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("--profile")
        .arg("cpu")
        .arg("--profile-format")
        .arg("deterministic-collapsed")
        .arg("-o")
        .arg(&out)
        .arg(&file)
        .output()
        .unwrap();
    assert!(output.status.success(), "process failed: {output:?}");
    // The program's own stdout is unchanged (the `print(total)` result).
    assert_eq!(String::from_utf8_lossy(&output.stdout), "24\n");

    let folded = std::fs::read_to_string(&out).unwrap();
    // Deterministic: a sample is recorded at EVERY frame push AND every pop (the pop
    // republishes the now-shallower stack). Over the 3 loop iterations this is a pure
    // function of the call structure, so the counts are byte-stable. The bottom
    // `<script>` line is the 3 returns to root depth (one per iteration).
    assert_eq!(
        folded,
        "<script>;a 9\n<script>;a;b 18\n<script>;a;b;c 12\n<script> 3\n",
        "collapsed folded-stacks golden"
    );
}

#[test]
fn deterministic_speedscope_golden_is_stable() {
    let file = write_prog("ascript_prof_speedscope.as");
    let out = std::env::temp_dir().join("ascript_prof_speedscope.json");
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("--profile")
        .arg("cpu")
        .arg("--profile-format")
        .arg("deterministic-speedscope")
        .arg("-o")
        .arg(&out)
        .arg(&file)
        .output()
        .unwrap();
    assert!(output.status.success(), "process failed: {output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "24\n");

    let json = std::fs::read_to_string(&out).unwrap();
    // Parse-and-assert the shape (the byte-stable ordering is asserted by the frame
    // table + first sample; the full doc is large but deterministic).
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid speedscope JSON");
    assert_eq!(v["$schema"], "https://www.speedscope.app/file-format-schema.json");
    assert_eq!(v["profiles"][0]["type"], "sampled");
    assert_eq!(v["profiles"][0]["name"], "ascript_prof_speedscope.as");
    // Frame table in first-seen order: <script>, a, b, c.
    assert_eq!(v["shared"]["frames"][0]["name"], "<script>");
    assert_eq!(v["shared"]["frames"][1]["name"], "a");
    assert_eq!(v["shared"]["frames"][2]["name"], "b");
    assert_eq!(v["shared"]["frames"][3]["name"], "c");
    // First sample is the first push: <script> -> a => indices [0, 1].
    assert_eq!(v["profiles"][0]["samples"][0], serde_json::json!([0, 1]));
    // 42 deterministic samples (9 + 18 + 12 + 3 over the collapsed paths), and
    // endValue == sample count. Byte-stable for a golden.
    let n = v["profiles"][0]["samples"].as_array().unwrap().len();
    assert_eq!(n, 42);
    assert_eq!(v["profiles"][0]["endValue"], 42);
}

#[test]
fn profiling_is_observation_only_gate_9() {
    // Gate 9: --profile leaves the program's stdout byte-identical to a plain run.
    let file = write_prog("ascript_prof_gate9.as");
    let out = std::env::temp_dir().join("ascript_prof_gate9.json");
    let bin = env!("CARGO_BIN_EXE_ascript");

    let plain = Command::new(bin).arg("run").arg(&file).output().unwrap();
    let profiled = Command::new(bin)
        .arg("run")
        .arg("--profile")
        .arg("cpu")
        .arg("--profile-format")
        .arg("deterministic-collapsed")
        .arg("-o")
        .arg(&out)
        .arg(&file)
        .output()
        .unwrap();
    assert!(plain.status.success());
    assert!(profiled.status.success());
    assert_eq!(
        plain.stdout, profiled.stdout,
        "profiling must not change program stdout"
    );
}

#[test]
fn empty_program_yields_valid_profile() {
    // A program with no user function calls profiles to a trivial (here empty) set —
    // no panic, a valid file written.
    let file = std::env::temp_dir().join("ascript_prof_empty.as");
    std::fs::write(&file, "print(1 + 1)\n").unwrap();
    let out = std::env::temp_dir().join("ascript_prof_empty.json");
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("--profile")
        .arg("cpu")
        .arg("--profile-format")
        .arg("deterministic-speedscope")
        .arg("-o")
        .arg(&out)
        .arg(&file)
        .output()
        .unwrap();
    assert!(output.status.success(), "process failed: {output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "2\n");
    let json = std::fs::read_to_string(&out).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON even when empty");
    // No user calls → no samples, no frames. Still a well-formed speedscope doc.
    assert_eq!(v["profiles"][0]["samples"].as_array().unwrap().len(), 0);
    assert_eq!(v["shared"]["frames"].as_array().unwrap().len(), 0);
}

#[test]
fn wallclock_mode_runs_and_writes_a_file() {
    // The REAL (timer-thread) mode must run to completion, join the sampler, and write
    // a valid file. Sample COUNT is wall-clock-dependent so we assert only validity,
    // not an exact count.
    let file = std::env::temp_dir().join("ascript_prof_wall.as");
    std::fs::write(
        &file,
        "fn work(n) { let s = 0; for (i in 0..n) { s = s + i }; return s }\n\
         let t = 0\n\
         for (j in 0..400) { t = t + work(400) }\n\
         print(t)\n",
    )
    .unwrap();
    let out = std::env::temp_dir().join("ascript_prof_wall.json");
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("--profile")
        .arg("cpu")
        .arg("--profile-hz")
        .arg("2000")
        .arg("-o")
        .arg(&out)
        .arg(&file)
        .output()
        .unwrap();
    assert!(output.status.success(), "process failed: {output:?}");
    let json = std::fs::read_to_string(&out).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid speedscope JSON");
    assert_eq!(v["profiles"][0]["type"], "sampled");
}
