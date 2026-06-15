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

// ---------------------------------------------------------------------------
// ELIDE Task 1.2 — the kind-table battery (spec §6.7)
// ---------------------------------------------------------------------------
//
// The anchoring proof (A) relies on `arith_result_kind` MIRRORING the runtime's
// NUM promotion EXACTLY: for every operand-kind pair × operator the table claims
// a definite result kind, the runtime must actually produce a value of that kind.
//
// This battery generates ONE program (a single VM invocation) that, for every
// concrete operand-kind pair × operator the table maps to `Some(kind)`, computes
// the op on representative literals and prints `type(result)`. It then asserts the
// printed runtime kind equals the table's `ResultKind`. Synth-vs-runtime is pinned
// EXHAUSTIVELY over the concrete operand-kind matrix.
//
// `OperandKind::Number` is omitted from the runtime side: `number` is an
// annotation SUPERTYPE with no literal form (a value is always concretely int or
// float at runtime), and the table already returns `None` for every `Number`
// operand — so there is no runtime obligation to pin for it.

use ascript::check::infer::elide::{arith_result_kind, ArithOp, OperandKind, ResultKind};

/// A representative `.as` literal expression for a concrete operand kind, plus a
/// SECOND distinct literal (so e.g. `5 / 2` exercises the truncating int division
/// rather than a trivial `x / x`).
fn operand_literals(k: OperandKind) -> Option<(&'static str, &'static str)> {
    match k {
        OperandKind::Int => Some(("5", "2")),
        OperandKind::Float => Some(("5.0", "2.0")),
        OperandKind::String => Some(("\"a\"", "\"b\"")),
        OperandKind::Bool => Some(("true", "false")),
        OperandKind::Nil => Some(("nil", "nil")),
        // `number` has no literal form (see the module note) — skip on the runtime
        // side; the table already returns None for it.
        OperandKind::Number => None,
    }
}

/// The surface operator(s) covered by an `ArithOp` family. Each concrete operator
/// must, for a kind pair the table maps to `Some`, produce that exact runtime kind.
fn surface_ops(op: ArithOp) -> &'static [&'static str] {
    match op {
        // `+` is its own family (string-concat overload).
        ArithOp::Add => &["+"],
        // strictly-numeric arithmetic (float-accepting), incl. the truncating `/`,
        // `%`, `**`.
        ArithOp::NumericArith => &["-", "*", "/", "%", "**"],
        // explicit overflow-WRAPPING arithmetic — int-only.
        ArithOp::WrappingArith => &["+%", "-%", "*%"],
        // bitwise / shift.
        ArithOp::Bitwise => &["&", "|", "^", "<<", ">>"],
        // comparison / membership (instanceof needs a type RHS, covered separately).
        ArithOp::Comparison => &["==", "!=", "<", "<=", ">", ">="],
        // logical / coalesce — never anchored; covered by a `None` assertion, not a
        // runtime program.
        ArithOp::Logical => &[],
    }
}

/// Whether a concrete surface operator actually PRODUCES a value (vs panics
/// before the site) for the given concrete operand kinds. The spec (§2.3) permits
/// a "panic-before-site" for anchoring (a panic is not a wrong elision), so the
/// battery only pins the cases that genuinely yield a value:
/// - ORDERING comparisons (`< <= > >=`) require two numbers — a cross-type or
///   non-number pair panics (`operator requires two numbers …`).
/// - EQUALITY (`== !=`) works on any operand pair (→ bool).
///
/// All other families' Some-cases already imply value-producing operand kinds.
fn runtime_produces_value(sop: &str, lhs: OperandKind, rhs: OperandKind) -> bool {
    let numeric = |k: OperandKind| matches!(k, OperandKind::Int | OperandKind::Float);
    match sop {
        "<" | "<=" | ">" | ">=" => numeric(lhs) && numeric(rhs),
        _ => true,
    }
}

fn result_kind_name(k: ResultKind) -> &'static str {
    match k {
        ResultKind::Int => "int",
        ResultKind::Float => "float",
        ResultKind::String => "string",
        ResultKind::Bool => "bool",
    }
}

#[test]
fn kind_table_battery_mirrors_runtime_num_promotion() {
    let concrete_kinds = [
        OperandKind::Int,
        OperandKind::Float,
        OperandKind::String,
        OperandKind::Bool,
        OperandKind::Nil,
    ];
    let families = [
        ArithOp::Add,
        ArithOp::NumericArith,
        ArithOp::WrappingArith,
        ArithOp::Bitwise,
        ArithOp::Comparison,
        ArithOp::Logical,
    ];

    // Build one program that prints the runtime kind of each anchored case, and a
    // parallel `expected` vector of the table's claimed kind names.
    let mut program = String::new();
    let mut expected: Vec<&'static str> = Vec::new();
    let mut cases: Vec<String> = Vec::new();

    for &op in &families {
        for &lhs in &concrete_kinds {
            for &rhs in &concrete_kinds {
                let Some(rk) = arith_result_kind(op, lhs, rhs) else {
                    continue; // table says NOT anchored → no runtime obligation.
                };
                let (Some((la, _)), Some((_, rb))) =
                    (operand_literals(lhs), operand_literals(rhs))
                else {
                    continue; // no literal form (e.g. Number) — skip.
                };
                for sop in surface_ops(op) {
                    if !runtime_produces_value(sop, lhs, rhs) {
                        continue; // panic-before-site — not a value-producing case.
                    }
                    let expr = format!("{la} {sop} {rb}");
                    program.push_str(&format!("print(type({expr}))\n"));
                    expected.push(result_kind_name(rk));
                    cases.push(format!("{la} {sop} {rb}  (table: {})", result_kind_name(rk)));
                }
            }
        }
    }

    assert!(
        !expected.is_empty(),
        "battery produced no cases — generator is broken"
    );

    // Run the whole battery in ONE VM invocation on a worker-stack runtime (the
    // `!Send` idiom the differential uses).
    let out = ascript::run_on_worker_stack({
        let src = program.clone();
        move || async move {
            ascript::vm_run_source(&src)
                .await
                .expect("kind-table battery program failed to run")
                .0
        }
    });

    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines.len(),
        expected.len(),
        "expected {} result lines, got {}\nprogram:\n{program}\noutput:\n{out}",
        expected.len(),
        lines.len()
    );

    for (i, (got, want)) in lines.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            got, want,
            "case `{}`: arith_result_kind says `{want}`, runtime `type()` says `{got}`",
            cases[i]
        );
    }

    // Sanity: the battery is non-trivial (covers ~100 micro-cases, the spec's
    // exhaustive-pin requirement).
    assert!(
        expected.len() >= 80,
        "battery should pin ~100 cases, only pinned {}",
        expected.len()
    );
}

#[test]
fn kind_table_logical_ops_never_anchored() {
    // The logical/coalesce trio is NEVER anchored — `arith_result_kind` returns
    // None for EVERY operand pair (pinned here so a future table edit can't
    // silently anchor them).
    let kinds = [
        OperandKind::Int,
        OperandKind::Float,
        OperandKind::Number,
        OperandKind::String,
        OperandKind::Bool,
        OperandKind::Nil,
    ];
    for &lhs in &kinds {
        for &rhs in &kinds {
            assert_eq!(
                arith_result_kind(ArithOp::Logical, lhs, rhs),
                None,
                "logical {lhs:?} ∘ {rhs:?} must never be anchored"
            );
        }
    }
}
