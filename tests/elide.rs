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

// ===========================================================================
// Step 1 (Task 1.3) — `elision_proofs(src)`-level collection tests (spec §2.3).
//
// These assert SET MEMBERSHIP per the §2.3 anchoring table: which call / let /
// fn-return sites the collector proves under the strict (E)∧(Y)∧(A) predicate.
// Membership is checked by computing the CHAR-span key of a known source
// substring (the same `(start_char, end_char)` convention the collector uses,
// `code_range` → byte→char) and probing the corresponding `ElisionSet` field.
// ===========================================================================

mod collect {
    use ascript::check::infer::elide::ElisionSet;
    use ascript::check::infer::elision_proofs;

    fn proofs(src: &str) -> ElisionSet {
        elision_proofs(src)
    }

    /// CHAR `(start, end)` of the FIRST occurrence of `needle` in `src`. Mirrors the
    /// `ByteToCharMap` convention: `start_char` = chars before the byte offset,
    /// `end_char` = chars before the end byte. ASCII fixtures ⇒ byte == char.
    fn char_span(src: &str, needle: &str) -> (u32, u32) {
        let bstart = src.find(needle).unwrap_or_else(|| panic!("`{needle}` not in src"));
        let bend = bstart + needle.len();
        let start = src[..bstart].chars().count() as u32;
        let end = src[..bend].chars().count() as u32;
        (start, end)
    }

    fn has_call(src: &str, set: &ElisionSet, call_text: &str) -> bool {
        set.calls.contains(&char_span(src, call_text))
    }
    fn has_let(src: &str, set: &ElisionSet, init_text: &str) -> bool {
        set.lets.contains(&char_span(src, init_text))
    }
    fn has_fn_ret(src: &str, set: &ElisionSet, fn_name: &str) -> bool {
        set.fn_rets.contains(&char_span(src, fn_name))
    }

    // ---- row 1: literals are anchored -------------------------------------

    #[test]
    fn literals_anchored_call() {
        let src = "fn f(a: int, b: string) {} f(1, \"x\")\n";
        let set = proofs(src);
        assert!(has_call(src, &set, "f(1, \"x\")"), "literal call must be proven");
        assert_eq!(set.calls.len(), 1, "exactly one call key");
        // no annotated let / no ElideSafe-return fn ⇒ no other keys.
        assert!(set.lets.is_empty());
        assert!(set.fn_rets.is_empty());
    }

    // ---- row 2: unmutated annotated binding anchors the let AND the call ---

    #[test]
    fn unmutated_annotated_binding_anchors_let_and_call() {
        let src = "fn f(p: int){}\nlet x: int = 5\nf(x)\n";
        let set = proofs(src);
        assert!(has_let(src, &set, "5"), "annotated let initializer proven");
        assert!(has_call(src, &set, "f(x)"), "unmutated annotated binding anchors the call");
    }

    #[test]
    fn mutated_binding_keeps_let_but_not_call() {
        // x = 7 is provably fine, but a MUTATED binding is NOT anchored (v1) — the
        // call keeps its check. The let key is NOT present either (the let binding is
        // mutated ⇒ not anchored ⇒ not recorded).
        let src = "fn f(p: int){}\nlet x: int = 5\nx = 7\nf(x)\n";
        let set = proofs(src);
        assert!(!has_call(src, &set, "f(x)"), "mutated binding ⇒ call NOT proven");
        // The let SITE: a mutated annotated binding is unanchored, so per the record
        // predicate (annotation ElideSafe + Yes + Anchored-INITIALIZER) the initializer
        // `5` IS still a literal (anchored) and the annotation IS ElideSafe and `5: int`
        // is Yes — BUT the binding's unmutated gate fails, so the let is NOT recorded.
        assert!(!has_let(src, &set, "5"), "mutated annotated let is NOT recorded");
    }

    #[test]
    fn probe3_mutated_to_wrong_type_no_call_key() {
        // THE soundness pin (§0 #2): x is reassigned to a string; even though the
        // checker still believes x: int, the call must NOT be elided.
        let src = "fn f(p: int){}\nlet x: int = 5\nx = \"s\"\nf(x)\n";
        let set = proofs(src);
        assert!(!has_call(src, &set, "f(x)"), "probe-3: mutated binding ⇒ no call key");
    }

    // ---- row 3: declared ElideSafe return + anchored call via it ----------

    #[test]
    fn declared_return_anchors_call_and_fn_ret() {
        let src = "fn g(): int { return 1 }\nfn f(p: int){}\nf(g())\n";
        let set = proofs(src);
        assert!(has_fn_ret(src, &set, "g"), "g's return contract proven");
        assert!(has_call(src, &set, "f(g())"), "call anchored via g's ElideSafe return");
        // g() itself is a call to a 0-arg fn with no typed params ⇒ vacuously proven.
        // (Compute the inner `g()` extent directly so the substring isn't confused with
        // the declaration's empty param list `g():`.)
        let call_off = src.find("f(g())").unwrap() + 2; // the inner `g(` start
        assert_eq!(&src[call_off..call_off + 3], "g()");
        let gkey = (call_off as u32, (call_off + 3) as u32);
        assert!(set.calls.contains(&gkey), "g() (no typed params) is also proven");
    }

    // ---- gradual sources are excluded (rule-1 Yes via anchoring) ----------

    #[test]
    fn unknown_callee_arg_not_anchored() {
        // unknown() resolves to nothing ⇒ Any ⇒ not anchored ⇒ no call key.
        let src = "fn f(p: int){}\nf(unknown())\n";
        let set = proofs(src);
        assert!(!has_call(src, &set, "f(unknown())"), "Any-arg call not proven");
    }

    #[test]
    fn any_typed_source_not_anchored() {
        let src = "fn f(p: int){}\nlet a: any = 5\nf(a)\n";
        let set = proofs(src);
        assert!(!has_call(src, &set, "f(a)"), "any-typed binding ⇒ not anchored");
    }

    // ---- ineligible call shapes -------------------------------------------

    #[test]
    fn spread_named_rest_async_worker_gen_excluded() {
        // spread arg
        let s1 = "fn f(p: int){}\nlet xs = [1]\nf(...xs)\n";
        assert!(proofs(s1).calls.is_empty(), "spread ⇒ no key");
        // named arg
        let s2 = "fn f(p: int){}\nf(p: 1)\n";
        assert!(proofs(s2).calls.is_empty(), "named arg ⇒ no key");
        // rest-param callee
        let s3 = "fn f(...p: array<int>){}\nf(1, 2)\n";
        assert!(proofs(s3).calls.is_empty(), "rest callee ⇒ no key");
        // async callee
        let s4 = "async fn f(p: int){}\nf(1)\n";
        assert!(proofs(s4).calls.is_empty(), "async callee ⇒ no key");
        // worker callee
        let s5 = "worker fn f(p: int) { return p }\nf(1)\n";
        assert!(proofs(s5).calls.is_empty(), "worker callee ⇒ no key");
        // generator callee
        let s6 = "fn* f(p: int) { yield p }\nf(1)\n";
        assert!(proofs(s6).calls.is_empty(), "generator callee ⇒ no key");
    }

    #[test]
    fn arity_mismatch_no_key() {
        // f(1,2,3) on a 2-arity fn: the collector counts arity itself ⇒ not proven.
        let src = "fn f(a: int, b: int) {}\nf(1, 2, 3)\n";
        let set = proofs(src);
        assert!(set.calls.is_empty(), "arity mismatch ⇒ no key");
        // too few:
        let src2 = "fn f(a: int, b: int) {}\nf(1)\n";
        assert!(proofs(src2).calls.is_empty(), "too-few args ⇒ no key");
        // defaulted param: f(1) valid for (a:int, b:int=2)
        let src3 = "fn f(a: int, b: int = 2) {}\nf(1)\n";
        assert!(has_call(src3, &proofs(src3), "f(1)"), "defaulted arity ⇒ proven");
    }

    // ---- narrowing-from-anchored ------------------------------------------

    #[test]
    fn narrowed_anchored_binding_call() {
        // x : int? unmutated, narrowed by `if (x != nil)` ⇒ the call inside is proven
        // (the base binding is anchored via its ElideSafe `int?` annotation, and
        // narrowing preserves anchoring).
        let src = "fn f(p: int?){}\nlet x: int? = 5\nif (x != nil) { f(x) }\n";
        let set = proofs(src);
        assert!(has_call(src, &set, "f(x)"), "narrowed-from-anchored call proven");
    }

    // ---- arithmetic / ternary / unary / comparison composition ------------

    #[test]
    fn arithmetic_composition_anchored() {
        // int literals + arithmetic → int (anchored).
        let src = "fn f(p: int){}\nf(1 + 2)\n";
        assert!(has_call(src, &proofs(src), "f(1 + 2)"), "int+int anchored");
        // mixed int/float → float, into a float param.
        let src2 = "fn g(p: float){}\ng(1 + 2.0)\n";
        assert!(has_call(src2, &proofs(src2), "g(1 + 2.0)"), "int+float anchored as float");
        // `**` is NOT kind-deterministic (int**−1 → float) ⇒ never anchored, so a
        // call fed by it keeps its check (fail-safe — matches the synth's gradual
        // `Number` for `**`).
        let src3 = "fn f(p: int){}\nf(2 ** 3)\n";
        assert!(proofs(src3).calls.is_empty(), "`**` ⇒ not anchored ⇒ no call key");
    }

    #[test]
    fn unary_and_comparison_anchored() {
        // `!cond` → bool, into a bool param.
        let src = "fn f(p: bool){}\nf(!true)\n";
        assert!(has_call(src, &proofs(src), "f(!true)"), "unary ! anchored as bool");
        // comparison → bool.
        let src2 = "fn f(p: bool){}\nf(1 < 2)\n";
        assert!(has_call(src2, &proofs(src2), "f(1 < 2)"), "comparison anchored as bool");
        // negation of an int literal → int.
        let src3 = "fn f(p: int){}\nf(-5)\n";
        assert!(has_call(src3, &proofs(src3), "f(-5)"), "unary - anchored as int");
    }

    #[test]
    fn ternary_both_branches_anchored() {
        let src = "fn f(p: int){}\nf(true ? 1 : 2)\n";
        assert!(has_call(src, &proofs(src), "f(true ? 1 : 2)"), "ternary anchored");
    }

    #[test]
    fn logical_op_not_anchored() {
        // `&&` returns an operand (truthiness), not a fixed kind ⇒ never anchored.
        let src = "fn f(p: bool){}\nlet a: bool = true\nlet b: bool = false\nf(a && b)\n";
        let set = proofs(src);
        assert!(!has_call(src, &set, "f(a && b)"), "logical-op arg ⇒ not anchored");
    }

    // ---- uninitialized annotated let is UNANCHORED (soundness point) ------

    #[test]
    fn uninitialized_annotated_let_unanchored() {
        // `let x: int` (no initializer): the runtime binds nil WITHOUT checking, so
        // the annotation is not a runtime guarantee ⇒ the call must NOT be proven.
        let src = "fn f(p: int){}\nlet x: int\nf(x)\n";
        let set = proofs(src);
        assert!(!has_call(src, &set, "f(x)"), "uninitialized annotated let ⇒ no call key");
        assert!(set.lets.is_empty(), "uninitialized let records no let key");
    }

    // ---- non-always-returning fn with non-nil ret is not a fn_ret key -----

    #[test]
    fn non_total_return_body_not_proven() {
        // g returns int on one path but falls off the end ⇒ implicit nil; nil is NOT
        // Yes against int ⇒ g's return contract is NOT proven ⇒ a call anchored via it
        // is NOT proven.
        let src = "fn g(c: bool): int { if (c) { return 1 } }\nfn f(p: int){}\nf(g(true))\n";
        let set = proofs(src);
        assert!(!has_fn_ret(src, &set, "g"), "non-total int-returning fn ⇒ no fn_ret key");
        assert!(!has_call(src, &set, "f(g(true))"), "call via unproven return ⇒ no key");
    }

    // ---- shadowed fn name fails safe --------------------------------------

    #[test]
    fn shadowed_fn_name_no_call_key() {
        // A genuine SHADOW: a local `f` (a non-fn binding) shadows the global `fn f`,
        // so the call inside `g` resolves to the LOCAL, not the fn — resolve_in_file_fn
        // must bail (not a unique non-shadowed Fn binding) ⇒ no call key for `f(1)`.
        let src = "fn f(p: int) {}\nfn g() { let f = 5\nf(1) }\n";
        let set = proofs(src);
        // The call `f(1)` inside g resolves to the local `f = 5` (not callable / not the
        // fn) — the collector must NOT prove it.
        let off = src.rfind("f(1)").unwrap();
        let key = (off as u32, (off + 4) as u32);
        assert!(!set.calls.contains(&key), "shadowed fn name ⇒ no call key");
    }

    #[test]
    fn captured_mutated_global_not_anchored() {
        // A global mutated inside a nested fn (`mark_mutated_target` reaches the global
        // binding) ⇒ the binding is NOT anchored ⇒ a call using it is not proven.
        let src = "fn f(p: int){}\nlet x: int = 5\nfn bump() { x = 9 }\nf(x)\n";
        let set = proofs(src);
        assert!(!has_call(src, &set, "f(x)"), "globally-mutated binding ⇒ no call key");
    }

    #[test]
    fn forward_mutated_global_not_anchored() {
        // The mutation `x = 9` (inside a fn) is collected order-independently, so even
        // a mutation textually BEFORE the `let x` declaration marks x mutated ⇒ the
        // call is not anchored (robustness of the resolver's mutated-globals set).
        let src = "fn f(p: int){}\nfn bump() { x = 9 }\nlet x: int = 5\nf(x)\n";
        let set = proofs(src);
        assert!(!has_call(src, &set, "f(x)"), "forward-mutated global ⇒ no call key");
    }
}

// ===========================================================================
// Step 3 (Task 1.3) — diagnostic-neutrality gate (spec §6.5).
//
// For EVERY file in examples/** (both intro and advanced), the inference pass's
// diagnostics must be byte-IDENTICAL whether or not elide-collection mode ran —
// the collector is a pure side-accumulator that NEVER changes diagnostics. This
// is the hover-mode invariant, re-proven for elide mode. Runs in BOTH feature
// configs (the inference pass is feature-independent, so `cargo test` and
// `--no-default-features` both exercise this).
// ===========================================================================

mod neutrality {
    use ascript::check::infer::check_with_elision;
    use std::fs;
    use std::path::Path;

    /// The full inference-pass diagnostics for `src` in NORMAL mode — recomputed
    /// here via the same front-end the collection path uses, so the only variable
    /// is the elide flag.
    fn normal_diags(src: &str) -> Vec<String> {
        use ascript::check::infer;
        use ascript::syntax::{resolve, tree_builder};
        let tree = tree_builder::build_tree(ascript::syntax::parser::parse(src));
        let resolved = resolve::resolve(&tree);
        infer::check(&tree, &resolved, src)
            .into_iter()
            .map(fmt_diag)
            .collect()
    }

    fn fmt_diag(d: ascript::check::diagnostic::AsDiagnostic) -> String {
        format!("{}:{}-{}:{}:{}", d.code, d.range.start, d.range.end, d.severity as u8, d.message)
    }

    fn collect_as_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    collect_as_files(&p, out);
                } else if p.extension().is_some_and(|x| x == "as") {
                    out.push(p);
                }
            }
        }
    }

    #[test]
    fn elide_collection_is_diagnostic_neutral_over_examples() {
        let root = env!("CARGO_MANIFEST_DIR");
        let mut files = Vec::new();
        collect_as_files(&Path::new(root).join("examples"), &mut files);
        assert!(files.len() > 10, "expected a real example corpus, found {}", files.len());
        let mut checked = 0usize;
        for f in &files {
            let src = fs::read_to_string(f).unwrap();
            let normal = normal_diags(&src);
            let (elide_diags, _set) = check_with_elision(&src);
            let elide: Vec<String> = elide_diags.into_iter().map(fmt_diag).collect();
            assert_eq!(
                normal, elide,
                "DIAGNOSTIC DRIFT in elide-collection mode for {}",
                f.display()
            );
            checked += 1;
        }
        assert_eq!(checked, files.len(), "every example must be checked");
    }
}

// ===========================================================================
// Task 2.3 — Compiler consumption of ElisionSet (spec §4.2)
//
// These tests verify that `compile_source_with_elision` correctly:
//   (a) skips CHECK_LOCAL for a proven let but NOT for an unproven one;
//   (b) emits CALL_ELIDED at a proven call site, CALL at a gradual one;
//   (c) sets proto.ret = None for a proven fn-return, Some(_) for unproven;
//   (d) is byte-identical to compile_source when elide = None.
//
// Span-match PROOF: if CALL_ELIDED never appears in a typed compile the spans
// don't match — this is an actual failure, not a latent miss.
// ===========================================================================

mod compiler {
    use ascript::check::infer::elision_proofs;
    use ascript::compile::{compile_source, compile_source_with_elision};
    use ascript::vm::disasm::disasm;

    // ---- (d) None elision-set ⇒ byte-identical to compile_source ----------

    #[test]
    fn none_elision_set_is_byte_identical() {
        // A typed program (so an elision COULD fire if wired) — but with None,
        // the output must be identical to compile_source.
        let src = "fn f(p: int): int { return p }\nlet x: int = 5\nf(x)\n";
        let chunk_default = compile_source(src).expect("compile_source ok");
        let chunk_none = compile_source_with_elision(src, None).expect("compile_source_with_elision(None) ok");
        // Compare the serialized disasm strings (fully recursive, includes protos).
        let d_default = disasm(&chunk_default);
        let d_none    = disasm(&chunk_none);
        assert_eq!(
            d_default, d_none,
            "compile_source_with_elision(None) must produce byte-identical output to compile_source"
        );
        // Also compare the raw bytecode bytes of the top chunk.
        assert_eq!(
            chunk_default.code.as_slice(),
            chunk_none.code.as_slice(),
            "top-chunk bytecode bytes must be identical"
        );
    }

    // ---- (a) CHECK_LOCAL skipped for proven let, kept for unproven ----------

    #[test]
    fn check_local_skipped_for_proven_let() {
        // `let x: int = 5` — annotation ElideSafe, literal anchored, Yes ⇒ proven.
        // `let y = 5` — no annotation ⇒ no check to skip.
        // A separate `let z: int = get()` — `get()` is not anchored ⇒ NOT proven,
        // but that would need a more complex setup; use an unannotated `let` as the
        // "unproven" control case.
        let src = "let x: int = 5\nlet y = 5\n";
        let set = elision_proofs(src);
        // `let x: int = 5`: proven ⇒ in set.lets
        assert!(!set.lets.is_empty(), "expected `let x: int = 5` to be in set.lets");

        let chunk_elided = compile_source_with_elision(src, Some(&set))
            .expect("compile with elision ok");
        let chunk_plain  = compile_source(src).expect("compile_source ok");

        let dis_elided = disasm(&chunk_elided);
        let dis_plain  = disasm(&chunk_plain);

        // The elided chunk must NOT contain CHECK_LOCAL (the annotation check was proven).
        assert!(
            !dis_elided.contains("CHECK_LOCAL"),
            "CHECK_LOCAL must be absent from the elided disasm:\n{dis_elided}"
        );
        // The plain chunk MUST contain CHECK_LOCAL (no elision).
        assert!(
            dis_plain.contains("CHECK_LOCAL"),
            "CHECK_LOCAL must be present in the plain disasm:\n{dis_plain}"
        );
    }

    #[test]
    fn check_local_kept_for_unproven_let_mutated() {
        // `let x: int = 5; x = 7` — mutated ⇒ not anchored ⇒ NOT proven. The CHECK_LOCAL
        // must remain even when an elision set is threaded through.
        let src = "let x: int = 5\nx = 7\n";
        let set = elision_proofs(src);
        // The let MUST NOT be in set.lets (mutated binding).
        assert!(set.lets.is_empty(), "mutated annotated let must not be in set.lets; set={set:?}");

        let chunk_elided = compile_source_with_elision(src, Some(&set))
            .expect("compile with elision ok");
        let dis_elided = disasm(&chunk_elided);
        assert!(
            dis_elided.contains("CHECK_LOCAL"),
            "CHECK_LOCAL must remain for an unproven (mutated) let:\n{dis_elided}"
        );
    }

    // ---- (b) CALL_ELIDED at proven call site, CALL at gradual one ----------

    #[test]
    fn call_elided_for_proven_call() {
        // `fn f(p: int){} f(5)` — literal arg to typed param, no spread/named/rest.
        // The collector MUST prove this call and the compiler MUST emit CALL_ELIDED.
        let src = "fn f(p: int){}\nf(5)\n";
        let set = elision_proofs(src);
        assert!(!set.calls.is_empty(), "f(5) must be proven; set.calls={:?}", set.calls);

        let chunk_elided = compile_source_with_elision(src, Some(&set))
            .expect("compile with elision ok");
        let dis_elided = disasm(&chunk_elided);

        assert!(
            dis_elided.contains("CALL_ELIDED"),
            "CALL_ELIDED must appear in the elided disasm for a proven call;\n\
             If it never appears, the compiler's call span does not match the collector's key.\n\
             disasm:\n{dis_elided}\nset.calls: {:?}", set.calls
        );
        // The un-elided (None) compile must NOT have CALL_ELIDED.
        let dis_plain = disasm(&compile_source(src).expect("ok"));
        assert!(
            !dis_plain.contains("CALL_ELIDED"),
            "plain compile must not contain CALL_ELIDED:\n{dis_plain}"
        );
    }

    #[test]
    fn call_kept_for_gradual_call() {
        // Probe-3 pattern: mutated binding ⇒ not anchored ⇒ collector produces no call key.
        // With the set threaded in, the compiler must keep a plain CALL (not CALL_ELIDED).
        let src = "fn f(p: int){}\nlet x: int = 5\nx = \"s\"\nf(x)\n";
        let set = elision_proofs(src);
        assert!(set.calls.is_empty(), "probe-3 call must NOT be proven; set={:?}", set);

        let chunk_elided = compile_source_with_elision(src, Some(&set))
            .expect("compile with elision ok");
        let dis_elided = disasm(&chunk_elided);
        assert!(
            !dis_elided.contains("CALL_ELIDED"),
            "CALL_ELIDED must NOT appear for a gradual (mutated-binding) call:\n{dis_elided}"
        );
        // The CALL opcode must still be present.
        assert!(
            dis_elided.contains("CALL "),
            "plain CALL must remain for a gradual call:\n{dis_elided}"
        );
    }

    // ---- (c) proto.ret = None for proven fn-return -------------------------

    #[test]
    fn proto_ret_none_for_proven_fn_return() {
        // `fn g(): int { return 1 }` — ElideSafe return, literal anchored, always returns.
        // Collector must put `g` in fn_rets; compiler must set proto.ret = None.
        let src = "fn g(): int { return 1 }\n";
        let set = elision_proofs(src);
        assert!(!set.fn_rets.is_empty(), "g's return must be proven; set.fn_rets={:?}", set.fn_rets);

        let chunk_elided = compile_source_with_elision(src, Some(&set))
            .expect("compile with elision ok");
        // The proto for `g` is in chunk.protos (top-level fns compile to protos in
        // the top chunk's proto table, not the const pool).
        let proto = chunk_elided
            .protos
            .first()
            .expect("g's FnProto must be in chunk.protos");
        assert!(
            proto.ret.is_none(),
            "proven fn's proto.ret must be None (elided return contract); got {:?}", proto.ret
        );
    }

    #[test]
    fn proto_ret_some_for_non_proven_fn() {
        // `fn h(c: bool): int { if (c) { return 1 } }` — non-total body, nil not Yes against int.
        // NOT proven ⇒ proto.ret must remain Some(int).
        let src = "fn h(c: bool): int { if (c) { return 1 } }\n";
        let set = elision_proofs(src);
        assert!(set.fn_rets.is_empty(), "non-total fn must NOT be in fn_rets; set={:?}", set);

        let chunk_elided = compile_source_with_elision(src, Some(&set))
            .expect("compile with elision ok");
        let proto = chunk_elided
            .protos
            .first()
            .expect("h's FnProto must be in chunk.protos");
        assert!(
            proto.ret.is_some(),
            "non-proven fn's proto.ret must remain Some(_); got None"
        );
    }

    // ---- consumed_count ⇒ matches len(ElisionSet) when all sites apply -----

    #[test]
    fn consumed_count_matches_elision_set_len() {
        // A fully-typed program with a proven call + proven let + proven fn-ret.
        let src = "fn g(): int { return 1 }\nfn f(p: int){}\nlet x: int = 5\nf(x)\ng()\n";
        let set = elision_proofs(src);
        let set_len = set.len();
        assert!(set_len > 0, "expected at least one proven site");

        let (chunk, consumed) = super::compile_source_with_elision_counted(src, Some(&set))
            .expect("compile ok");
        // consumed must equal the number of proven sites that were actually applied.
        // For a simple single-module program where every key is in-scope, consumed == set_len.
        assert_eq!(
            consumed, set_len,
            "consumed_count ({consumed}) must equal ElisionSet::len ({set_len})"
        );
        // Verify the disasm contains CALL_ELIDED (the call was actually consumed).
        let dis = disasm(&chunk);
        assert!(dis.contains("CALL_ELIDED"), "CALL_ELIDED must appear:\n{dis}");
    }

    // ---- end-to-end: vm_run_source_elided output == vm_run_source ----------

    #[test]
    fn vm_run_source_elided_same_output_typed_program() {
        // A typed program that produces observable output — the elided version must
        // produce byte-identical output to the normal run.
        // `AsError` is `!Send` (holds `Rc<SourceInfo>`); reduce to `String` inside the
        // worker closure so the return type is `Send`.
        let src = "fn add(a: int, b: int): int { return a + b }\nprint(add(3, 4))\n";
        type Summary = Result<(String, Option<i32>), String>;
        let (out_normal, out_elided): (Summary, Summary) = ascript::run_on_worker_stack({
            let src = src.to_string();
            move || async move {
                let summarize = |r: Result<(String, Option<i32>), ascript::error::AsError>| -> Summary {
                    r.map_err(|e| e.message)
                };
                let normal = summarize(ascript::vm_run_source(&src).await);
                let elided = summarize(ascript::vm_run_source_elided(&src).await);
                (normal, elided)
            }
        });
        assert_eq!(
            out_normal, out_elided,
            "vm_run_source_elided must produce byte-identical output:\nnormal: {out_normal:?}\nelided: {out_elided:?}"
        );
    }

    #[test]
    fn vm_run_source_elided_gradual_boundary_still_panics() {
        // Probe-3: mutated binding ⇒ call NOT elided ⇒ must STILL panic.
        // `AsError` is `!Send`; reduce to `Result<_, String>` inside the closure.
        let src = "fn f(p: int){ return p }\nlet x: int = 5\nx = \"s\"\nf(x)\n";
        let result: Result<(String, Option<i32>), String> = ascript::run_on_worker_stack({
            let src = src.to_string();
            move || async move {
                ascript::vm_run_source_elided(&src).await.map_err(|e| e.message)
            }
        });
        assert!(
            result.is_err(),
            "probe-3 program must still panic under vm_run_source_elided (gradual boundary kept)"
        );
    }
}

/// Helper used only in the compiler-consumption tests: compiles with an elision set
/// and also returns the consumed count. Exposed for tests via the `ascript` crate.
fn compile_source_with_elision_counted(
    src: &str,
    elide: Option<&ascript::check::infer::elide::ElisionSet>,
) -> Result<(ascript::vm::chunk::Chunk, usize), ascript::compile::CompileError> {
    ascript::compile::compile_source_with_elision_counted(src, elide)
}
