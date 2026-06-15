//! ELIDE — semantics pins + (later) elision batteries.
//!
//! The probes in this file document the exact runtime/checker behaviors the
//! ELIDE spec's proof predicate is shaped around (spec §0 / §3.4).  They must
//! pass TODAY against the unmodified binary and must remain green after every
//! ELIDE task so that any regression is caught immediately.
//!
//! Output routing (observed 2026-06-15):
//!   `ascript check` diagnostics → **stdout** (ariadne reports go to stdout)
//!   `ascript run` runtime errors → **stderr**
//!
//! Observed exact messages (2026-06-15, recorded here so later phases can rely
//! on them — assert via `.contains(prefix)` to survive appended `, got {type}`
//! tails per the DX message guide):
//!
//!   probe 1 check stdout: "[type-mismatch] Error: argument 1 of `f` expects `string`, found `int`"
//!   probe 1 run  stderr : "type contract violated: expected string, got int (1)"
//!   probe 3 run  stderr : "type contract violated: expected int, got string (s)"
//!   probe 4 run  stderr : "type contract violated: expected object, got instance (<C instance>)"

use std::process::Command;

/// Spawn `ascript` with `args` and return `(stdout, stderr, exit_code)`.
///
/// Mirrors the pattern used in `tests/cli.rs` / `tests/lsp.rs`: no shell, no
/// intermediary — direct `Command` spawn, UTF-8-lossy capture.
fn run_cli(args: &[&str]) -> (String, String, i32) {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn {bin}: {e}"));
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let code = output.status.code().unwrap_or(-1);
    (stdout, stderr, code)
}

/// Write `src` to a uniquely-named temp file (keyed by test name + PID so
/// parallel test runs never collide) and return the path.
fn temp_as(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "ascript_elide_{}_{}.as",
        tag,
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

// ---------------------------------------------------------------------------
// Probe 1 — `ascript run` has no static type gate
// ---------------------------------------------------------------------------
//
// Spec §5.3: `ascript check` blocks (exits != 0) on a provable `type-mismatch`
// Error, but `ascript run` does NOT run the type checker — it executes the
// program and only RUNTIME-panics when a contract is actually violated.
// ELIDE must not change this (the probe is re-checked after each ELIDE task).
//
// Program: fn f(p: string){} f(1)
//   ascript check  → exit 1, stdout contains "type-mismatch" / "expects"
//                    (ariadne check diagnostics go to stdout, not stderr)
//   ascript run    → exit 1, stderr contains "type contract violated: expected string, got int"
//
// Observed (2026-06-15):
//   check stdout: "[type-mismatch] Error: argument 1 of `f` expects `string`, found `int`"
//   run   stderr: "Error: type contract violated: expected string, got int (1)"
#[test]
fn probe_run_has_no_static_type_gate() {
    let src = "fn f(p: string){} f(1)\n";
    let file = temp_as("probe1", src);

    // --- ascript check must block (exit != 0) ---
    // Note: `ascript check` diagnostics go to STDOUT (ariadne rendering), not stderr.
    let (check_out, _, check_code) = run_cli(&["check", file.to_str().unwrap()]);
    assert_ne!(
        check_code, 0,
        "ascript check must exit != 0 for a provable type error; got exit {check_code}\nstdout: {check_out}"
    );
    // Observed stdout: "[type-mismatch] Error: argument 1 of `f` expects `string`, found `int`"
    assert!(
        check_out.contains("type-mismatch") || check_out.contains("expects"),
        "expected a type-mismatch diagnostic in check stdout:\n{check_out}"
    );

    // --- ascript run must execute and RUNTIME-panic (not be blocked statically) ---
    // Note: `ascript run` runtime errors go to STDERR.
    let (_, run_err, run_code) = run_cli(&["run", file.to_str().unwrap()]);
    assert_ne!(
        run_code, 0,
        "ascript run must exit != 0 due to the runtime contract panic; got exit {run_code}\nstderr: {run_err}"
    );
    // Observed: "type contract violated: expected string, got int (1)"
    assert!(
        run_err.contains("type contract violated: expected string"),
        "expected runtime contract panic 'type contract violated: expected string' in run stderr:\n{run_err}"
    );

    let _ = std::fs::remove_file(&file);
}

// ---------------------------------------------------------------------------
// Probe 2 — reassignment to an annotated let is NOT contract-checked
// ---------------------------------------------------------------------------
//
// `let x: int = 5; x = "s"; print(x)` — the assignment `x = "s"` is never
// checked against the annotation.  Both engines run clean (exit 0) and print "s".
//
// This is the spec §0 #2 landmine: the runtime does not contract-check later
// assignments (src/interp.rs documents it), and the checker does not flow-update
// annotated bindings on assignment.
//
// Observed (2026-06-15):
//   VM output:          "s\n", exit 0
//   tree-walker output: "s\n", exit 0
#[test]
fn probe_reassignment_is_not_contract_checked() {
    let src = "let x: int = 5\nx = \"s\"\nprint(x)\n";
    let file = temp_as("probe2", src);

    // Default VM
    let (vm_out, vm_err, vm_code) = run_cli(&["run", file.to_str().unwrap()]);
    assert_eq!(
        vm_code, 0,
        "VM: expected clean exit (no contract check on reassignment); exit {vm_code}\nstderr: {vm_err}"
    );
    assert_eq!(
        vm_out.trim(),
        "s",
        "VM: expected print output 's'; got: {vm_out:?}"
    );

    // Tree-walker (flag goes after the file per CLAUDE.md)
    let (tw_out, tw_err, tw_code) =
        run_cli(&["run", file.to_str().unwrap(), "--tree-walker"]);
    assert_eq!(
        tw_code, 0,
        "tree-walker: expected clean exit (no contract check on reassignment); exit {tw_code}\nstderr: {tw_err}"
    );
    assert_eq!(
        tw_out.trim(),
        "s",
        "tree-walker: expected print output 's'; got: {tw_out:?}"
    );

    let _ = std::fs::remove_file(&file);
}

// ---------------------------------------------------------------------------
// Probe 3 — a mutated annotated binding is NOT a runtime guarantee
// ---------------------------------------------------------------------------
//
// THE ELIDE landmine (spec §0 #2, §3.4 probe 3):
//
//   fn f(p: int){ return p }
//   let x: int = 5
//   x = "s"       ← annotation does NOT guard reassignment
//   f(x)          ← checker still believes x : int → check exits 0
//                   runtime sees x == "s" → contract panic
//
// Under ELIDE this site MUST remain un-elided because `x` is mutated
// (not Anchored per §2.3).  This test is reasserted in Task 3.x to verify
// that property after elision is wired.
//
// Observed (2026-06-15):
//   ascript check exit:  0  (checker believes x : int, emits nothing)
//   ascript run  exit:   1  "type contract violated: expected int, got string (s)"
//   ascript run --tree-walker exit: 1, same message
#[test]
fn probe_mutated_binding_yes_is_not_a_runtime_guarantee() {
    let src = "fn f(p: int){ return p }\nlet x: int = 5\nx = \"s\"\nf(x)\n";
    let file = temp_as("probe3", src);

    // --- ascript check exits 0 (the checker's `Yes` is stale — annotated but mutated) ---
    // Note: check diagnostics go to stdout; check_out is empty when no diagnostics.
    let (check_out, _, check_code) = run_cli(&["check", file.to_str().unwrap()]);
    assert_eq!(
        check_code, 0,
        "ascript check must exit 0 (checker does not track reassignment); got exit {check_code}\nstdout: {check_out}"
    );

    // --- VM: runtime panic because x holds "s" at the call site ---
    let (_, run_err, run_code) = run_cli(&["run", file.to_str().unwrap()]);
    assert_ne!(
        run_code, 0,
        "VM: expected runtime panic at f(x) after x was reassigned to a string; got exit {run_code}\nstderr: {run_err}"
    );
    // Observed: "type contract violated: expected int, got string (s)"
    assert!(
        run_err.contains("type contract violated: expected int"),
        "VM: expected 'type contract violated: expected int' in run stderr:\n{run_err}"
    );

    // --- tree-walker: same panic ---
    let (_, tw_err, tw_code) =
        run_cli(&["run", file.to_str().unwrap(), "--tree-walker"]);
    assert_ne!(
        tw_code, 0,
        "tree-walker: expected runtime panic at f(x); got exit {tw_code}\nstderr: {tw_err}"
    );
    assert!(
        tw_err.contains("type contract violated: expected int"),
        "tree-walker: expected 'type contract violated: expected int' in stderr:\n{tw_err}"
    );

    let _ = std::fs::remove_file(&file);
}

// ---------------------------------------------------------------------------
// Probe 4 — `object` contract rejects class instances at runtime
// ---------------------------------------------------------------------------
//
// Spec §0 #3 / §6.6: the checker's rule 6 (`assignable(Class(_), Object) = Yes`)
// contradicts the runtime (`check_type` for `object` rejects instances).
// ELIDE fixes rule 6 → Unknown (Task 1.1) and excludes `object` from the
// ElideSafe form list.  This probe pins the runtime behavior that makes that
// exclusion necessary.
//
// Program: class C{} fn f(p: object){} f(C())
//   ascript check → exit 0 (checker today says Yes; after Task 1.1 still exit 0
//                           because Unknown → no diagnostic)
//   ascript run   → exit 1, "type contract violated: expected object, got instance"
//   ascript run --tree-walker → exit 1, same message
//
// Observed (2026-06-15):
//   check exit:  0
//   run   exit:  1  "type contract violated: expected object, got instance (<C instance>)"
//   tree-walker: 1  same
#[test]
fn probe_object_contract_rejects_instances() {
    let src = "class C{}\nfn f(p: object){}\nf(C())\n";
    let file = temp_as("probe4", src);

    // checker is currently silent (rule 6 Yes → will stay silent as Unknown after Task 1.1)
    // Note: check diagnostics go to stdout; check_out would be empty if silent.
    let (check_out, _, check_code) = run_cli(&["check", file.to_str().unwrap()]);
    assert_eq!(
        check_code, 0,
        "ascript check must exit 0 for class→object (checker rule 6 is Yes/Unknown, not No); got exit {check_code}\nstdout: {check_out}"
    );

    // VM runtime rejects the instance
    let (_, run_err, run_code) = run_cli(&["run", file.to_str().unwrap()]);
    assert_ne!(
        run_code, 0,
        "VM: expected runtime panic (object contract rejects instances); got exit {run_code}\nstderr: {run_err}"
    );
    // Observed: "type contract violated: expected object, got instance (<C instance>)"
    assert!(
        run_err.contains("type contract violated: expected object, got instance"),
        "VM: expected 'type contract violated: expected object, got instance' in stderr:\n{run_err}"
    );

    // tree-walker runtime rejects the instance
    let (_, tw_err, tw_code) =
        run_cli(&["run", file.to_str().unwrap(), "--tree-walker"]);
    assert_ne!(
        tw_code, 0,
        "tree-walker: expected runtime panic (object contract rejects instances); got exit {tw_code}\nstderr: {tw_err}"
    );
    assert!(
        tw_err.contains("type contract violated: expected object, got instance"),
        "tree-walker: expected 'type contract violated: expected object, got instance' in stderr:\n{tw_err}"
    );

    let _ = std::fs::remove_file(&file);
}
