//! M17 carry-in regression tests: abandoned/un-iterated generators must never
//! hang program exit, and operating on a generator handle without driving it
//! must be graceful. These run the real binary end-to-end, so the program's
//! top-level `LocalSet` drain is exercised — a regression to the old
//! spawned-task generator model would HANG these tests (cargo's harness timeout
//! fails them), rather than silently passing.

use std::process::Command;

fn run_ok(name: &str, src: &str) -> String {
    let file = std::env::temp_dir().join(format!("ascript_m17_{name}.as"));
    std::fs::write(&file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin).arg("run").arg(&file).output().unwrap();
    assert!(output.status.success(), "process failed: {output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn equality_on_uniterated_generator_terminates() {
    // A generator handle is identity-comparable without driving its body. The
    // program must terminate (the top-level drain must not wait on the
    // never-polled generator future).
    let out = run_ok(
        "eq",
        "fn* count() { yield 1\n yield 2 }\nlet g = count()\nprint(g == g)\nprint(\"end\")\n",
    );
    assert!(out.contains("true"), "got: {out:?}");
    assert!(out.contains("end"), "got: {out:?}");
}

#[test]
fn inspecting_uniterated_generator_is_graceful_and_terminates() {
    // Inspecting a generator handle without driving its body (here via the core
    // `type` builtin — kept feature-independent so it also runs under
    // --no-default-features) must be graceful and the program must terminate.
    let out = run_ok(
        "inspect",
        "fn* count() { yield 1 }\n\
         let g = count()\n\
         print(type(g))\n\
         print(\"end\")\n",
    );
    assert!(out.contains("generator"), "expected type generator, got: {out:?}");
    assert!(out.contains("end"), "got: {out:?}");
}

#[test]
fn never_iterated_generator_terminates() {
    // Creating a generator and never iterating it must not hang exit.
    let out = run_ok("never", "fn* g() { yield 1\n yield 2 }\ng()\nprint(\"ok\")\n");
    assert!(out.contains("ok"), "got: {out:?}");
}

#[test]
fn infinite_generator_with_break_terminates_and_prints() {
    // A lazy infinite generator consumed with an early break must print exactly
    // the pulled values and then exit — the canonical "abandoned generator" case.
    let out = run_ok(
        "infbreak",
        "fn* nats() { let i = 0\n while (true) { yield i\n i = i + 1 } }\n\
         for await (x in nats()) { print(x)\n if (x >= 2) { break } }\n\
         print(\"done\")\n",
    );
    assert!(out.contains('0'), "got: {out:?}");
    assert!(out.contains('2'), "got: {out:?}");
    assert!(out.contains("done"), "got: {out:?}");
}
