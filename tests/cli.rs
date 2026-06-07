//! End-to-end test: build the binary and run a real `.as` file.

use std::process::Command;

#[test]
fn runs_a_program_file_and_prints_result() {
    let file = std::env::temp_dir().join("ascript_skeleton_hello.as");
    std::fs::write(&file, "print(1 + 2 * 3)\n").unwrap();

    // Cargo sets CARGO_BIN_EXE_<name> for integration tests.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin).arg("run").arg(&file).output().unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "7\n");
}

#[test]
fn runs_factorial_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/factorial.as")
        .output()
        .unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    // 1*2*3*4*5 = 120, which is > 100, then the value itself.
    assert_eq!(String::from_utf8_lossy(&output.stdout), "big\n120\n");
}

#[test]
fn runs_functions_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/functions.as")
        .output()
        .unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    // fib(10) = 55; triple(7) = 21; count of odd fib(0..10) = fib values
    // [0,1,1,2,3,5,8,13,21,34] -> odd ones: 1,1,3,5,13,21 -> 6
    assert_eq!(String::from_utf8_lossy(&output.stdout), "55\n21\n6\n");
}

#[test]
fn runs_async_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/async.as")
        .output()
        .unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    // fetch(21)=42; await 5=5; async arrow g(9)=10; async arrow h(8)=7.
    assert_eq!(String::from_utf8_lossy(&output.stdout), "42\n5\n10\n7\n");
}

#[test]
fn runs_data_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/data.as")
        .output()
        .unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    // ages 36+41+45 = 122; average 122/3 ≈ 40.66...; oldest is Grace
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("sum of ages: 122"));
    assert!(out.contains("Grace"));
}

#[test]
fn runs_result_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/result.as")
        .output()
        .unwrap();
    assert!(output.status.success(), "process failed: {:?}", output);
    let out = String::from_utf8_lossy(&output.stdout);
    // compute(100,5,2): 100/5=20, 20/2=10 -> good[0]=10
    assert!(out.contains("10\n"));
    // compute(100,0,2): first ? propagates -> bad[0]=nil, message "division by zero"
    assert!(out.contains("division by zero"));
    // recover catches the OOB panic
    assert!(out.contains("out of bounds"));
}

#[test]
fn runs_typed_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/typed.as")
        .output()
        .unwrap();
    assert!(output.status.success(), "process failed: {:?}", output);
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("12\n")); // area(3,4)
    assert!(out.contains("hello, Ada")); // greet
    assert!(out.contains("12\n")); // total 3+4+5=12
    assert!(out.contains("type contract violated")); // recovered contract panic
}

#[test]
fn runs_oop_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/oop.as")
        .output()
        .unwrap();
    assert!(output.status.success(), "process failed: {:?}", output);
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("Rex is an animal, specifically a dog"));
    assert!(out.contains("woof"));
    assert!(out.contains("square"));
    assert!(out.contains("other"));
}

#[test]
fn reports_usage_without_args() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin).output().unwrap();
    // clap requires a subcommand; with none it prints usage and exits non-zero.
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage"));
}

#[test]
fn run_tree_walker_flag_works() {
    // `--tree-walker` routes a `.as` file to the legacy tree-walker oracle (the
    // debugging escape hatch). Output must match the default VM run.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let vm = Command::new(bin)
        .args(["run", "examples/hello.as"])
        .output()
        .unwrap();
    let tw = Command::new(bin)
        .args(["run", "--tree-walker", "examples/hello.as"])
        .output()
        .unwrap();
    assert!(vm.status.success() && tw.status.success());
    assert_eq!(
        String::from_utf8_lossy(&vm.stdout),
        String::from_utf8_lossy(&tw.stdout),
        "--tree-walker output must match the default VM run"
    );
    assert_eq!(vm.status.code(), tw.status.code());
}

#[test]
fn run_error_shows_source_caret() {
    let file = std::env::temp_dir().join(format!("ascript_diag_{}.as", std::process::id()));
    std::fs::write(&file, "let x = 1\nprint(missing)\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = Command::new(bin).arg("run").arg(&file).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    // ariadne renders the message and points at the source
    assert!(err.contains("undefined variable 'missing'"));
}

#[test]
fn runs_modules_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/modules/main.as")
        .output()
        .unwrap();
    assert!(output.status.success(), "process failed: {:?}", output);
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("12.56636")); // circleArea(2) = 3.14159*4
    assert!(out.contains("12\n")); // Rect(3,4).area()
    assert!(out.contains("3.14159")); // geo.PI
}

#[test]
fn runs_stdlib_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/stdlib.as")
        .output()
        .unwrap();
    assert!(output.status.success(), "process failed: {:?}", output);
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("brown, fox, quick, the"));
    assert!(out.contains("[1, 4, 9, 16]"));
    assert!(out.contains("\"name\", \"age\"")); // object.keys
    assert!(out.contains("cannot parse 'xyz' as a number")); // Result destructuring
    assert!(out.contains("\n50\n")); // 42 + 8 after parseNumber + destructure
}

#[test]
#[cfg(feature = "data")] // example imports std/json etc.; only valid with the data feature.
fn runs_serialization_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/serialization.as")
        .output()
        .unwrap();
    assert!(output.status.success(), "process failed: {:?}", output);
    let out = String::from_utf8_lossy(&output.stdout);
    // json: parsed fields + stringify
    assert!(out.contains("ascript"));
    assert!(out.contains("lang"));
    assert!(out.contains("{\"ok\":true,\"n\":3}"));
    // toml
    assert!(out.contains("demo"));
    assert!(out.contains("8080"));
    // yaml
    assert!(out.contains("prod"));
    // encoding: base64Encode("hi") + decode round-trip
    assert!(out.contains("aGk="));
    assert!(out.contains("hello"));
    // regex: findAll + find.text
    assert!(out.contains("[\"the\", \"quick\", \"fox\"]"));
    assert!(out.contains("abc"));
    // csv: rows[1][0]
    assert!(out.contains("\n1\n"));
    // bytes: 513 big-endian = [2, 1]
    assert!(out.contains("[2, 1]"));
    // uuid v4 is random; just assert its length (36).
    assert!(out.contains("36"));
}

#[test]
#[cfg(all(feature = "datetime", feature = "intl"))] // imports std/date + std/intl
fn runs_datetime_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/datetime.as")
        .output()
        .unwrap();
    assert!(output.status.success(), "process failed: {:?}", output);
    let out = String::from_utf8_lossy(&output.stdout);
    // time: seconds(3) = 3000 (deterministic; elapsed>=5 line varies, not asserted)
    assert!(out.contains("3000"));
    // date: parse + strftime format, and June 15 + 7 days = the 22nd
    assert!(out.contains("2021/06/15"));
    assert!(out.contains("\n22\n"));
    // intl: locale-aware grouping differs between en-US and de-DE
    assert!(out.contains("1,234,567"));
    assert!(out.contains("1.234.567"));
    // intl: Turkish dotted-capital-I case folding
    assert!(out.contains("İSTANBUL"));
}

#[test]
fn repl_evaluates_and_persists_bindings() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let bin = env!("CARGO_BIN_EXE_ascript");
    let mut child = Command::new(bin)
        .arg("repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"let x = 21\nx * 2\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("42"));
}

#[test]
fn repl_buffers_multiline_class() {
    let input =
        "class P {\n  x: number\n  y: number\n}\nlet p = P.from({x: 3, y: 4})\nprint(p.x + p.y)\n";
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ascript"))
        .arg("repl")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.take().unwrap().write_all(input.as_bytes())?;
            c.wait_with_output()
        })
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains('7'), "expected 7; stdout: {stdout}");
}

/// Feed `input` to `ascript repl` (optionally with `--tree-walker`) over piped
/// stdin and return `(stdout, stderr)`. Used by the WS2 VM-REPL tests below.
fn run_repl_session(input: &str, tree_walker: bool) -> (String, String) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let bin = env!("CARGO_BIN_EXE_ascript");
    let mut cmd = Command::new(bin);
    cmd.arg("repl");
    if tree_walker {
        cmd.arg("--tree-walker");
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn repl_vm_persists_bindings_across_lines() {
    let (out, _) = run_repl_session("let x = 1\nx + 1\n", false);
    assert!(out.contains('2'), "expected 2; stdout: {out}");
}

#[test]
fn repl_vm_persists_fn_and_class_across_lines() {
    let (out, _) = run_repl_session("fn sq(n) { return n * n }\nsq(5)\n", false);
    assert!(out.contains("25"), "fn persistence; stdout: {out}");

    let (out2, _) = run_repl_session(
        "class P {\n  x: number\n  y: number\n}\nlet p = P.from({x: 3, y: 4})\np.x + p.y\n",
        false,
    );
    assert!(out2.contains('7'), "class persistence; stdout: {out2}");
}

#[test]
fn repl_vm_trailing_expr_prints_value_statements_print_nothing() {
    // A bare trailing expression prints its value.
    let (out, _) = run_repl_session("3 + 4\n", false);
    assert_eq!(out.trim(), "7", "trailing expr; stdout: {out:?}");

    // A `let` statement prints nothing.
    let (out2, _) = run_repl_session("let z = 9\n", false);
    assert_eq!(
        out2.trim(),
        "",
        "statement prints nothing; stdout: {out2:?}"
    );

    // A trailing `nil` expression prints nothing.
    let (out3, _) = run_repl_session("nil\n", false);
    assert_eq!(out3.trim(), "", "nil prints nothing; stdout: {out3:?}");
}

#[test]
fn repl_vm_redefinition_across_lines_errors_matching_reference() {
    // The tree-walker REPL rejects a re-`let` of the same name (already defined);
    // the VM REPL must match: report the error AND keep the first binding.
    let (out, err) = run_repl_session("let x = 1\nlet x = 2\nx\n", false);
    let combined = format!("{out}{err}");
    assert!(
        combined.contains("already defined"),
        "expected 'already defined'; out={out:?} err={err:?}"
    );
    // The first binding survives the failed redefinition.
    assert!(out.contains('1'), "first binding survives; stdout: {out:?}");
}

#[test]
fn repl_vm_const_reassignment_across_lines_errors_and_keeps_value() {
    // An immutable top-level `const` defined on one REPL line and reassigned on a
    // LATER, separately-compiled line: the VM REPL must error `cannot assign to
    // immutable binding` (the cross-chunk runtime SET_GLOBAL check) AND keep the
    // first value — byte-identical to the tree-walker REPL.
    let session = "const k = 1\nk = 2\nk\n";
    let (vm_out, vm_err) = run_repl_session(session, false);
    let (tw_out, tw_err) = run_repl_session(session, true);
    let vm = format!("{vm_out}{vm_err}");
    let tw = format!("{tw_out}{tw_err}");
    assert!(
        vm.contains("cannot assign to immutable binding 'k'"),
        "VM must reject const reassignment; out={vm_out:?} err={vm_err:?}"
    );
    assert!(
        tw.contains("cannot assign to immutable binding 'k'"),
        "TW must reject const reassignment; out={tw_out:?} err={tw_err:?}"
    );
    // The const keeps its first value on BOTH engines (the failed assignment did not
    // mutate it): the final `k` line prints 1, not 2.
    assert!(vm_out.contains('1'), "VM keeps k=1; stdout: {vm_out:?}");
    assert!(tw_out.contains('1'), "TW keeps k=1; stdout: {tw_out:?}");
    assert!(
        !vm_out.contains('2'),
        "VM must not print 2; stdout: {vm_out:?}"
    );
}

#[test]
fn repl_vm_fn_and_class_reassignment_across_lines_error() {
    // fn / class names are immutable top-level bindings: reassigning one on a later
    // REPL line errors on BOTH engines (cross-chunk runtime check).
    for session in ["fn f() { return 1 }\nf = 5\n", "class C {}\nC = 9\n"] {
        let (vm_out, vm_err) = run_repl_session(session, false);
        let (tw_out, tw_err) = run_repl_session(session, true);
        let vm = format!("{vm_out}{vm_err}");
        let tw = format!("{tw_out}{tw_err}");
        assert!(
            vm.contains("cannot assign to immutable binding"),
            "VM must reject for {session:?}; out={vm_out:?} err={vm_err:?}"
        );
        assert!(
            tw.contains("cannot assign to immutable binding"),
            "TW must reject for {session:?}; out={tw_out:?} err={tw_err:?}"
        );
    }
}

#[test]
fn repl_vm_mutable_let_reassignment_across_lines_succeeds() {
    // A mutable top-level `let` reassigned on a later line works (the runtime check
    // must NOT over-trigger) — both engines print the new value.
    let session = "let m = 1\nm = 2\nm\n";
    let (vm_out, _) = run_repl_session(session, false);
    let (tw_out, _) = run_repl_session(session, true);
    assert!(
        vm_out.contains('2'),
        "VM let reassignment; stdout: {vm_out:?}"
    );
    assert!(
        tw_out.contains('2'),
        "TW let reassignment; stdout: {tw_out:?}"
    );
}

#[test]
fn file_mode_imported_const_reassignment_errors_on_both_engines() {
    // A main module that imports an immutable `const` from another module and then
    // reassigns it: BOTH engines reject `cannot assign to immutable binding 'K'` (in
    // file mode the import + reassignment are in the same entry chunk, so the runtime
    // SET_GLOBAL check fires identically to the tree-walker).
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = std::env::temp_dir().join("ascript_imp_reassign_test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.as"), "export const K = 1\n").unwrap();
    let main = dir.join("main.as");
    std::fs::write(&main, "import { K } from \"./m\"\nK = 5\nprint(K)\n").unwrap();

    let vm = Command::new(bin).arg("run").arg(&main).output().unwrap();
    let tw = Command::new(bin)
        .args(["run", "--tree-walker"])
        .arg(&main)
        .output()
        .unwrap();
    let vm_err = String::from_utf8_lossy(&vm.stderr);
    let tw_err = String::from_utf8_lossy(&tw.stderr);
    assert!(
        vm_err.contains("cannot assign to immutable binding 'K'"),
        "VM must reject imported-const reassignment; stderr={vm_err:?}"
    );
    assert!(
        tw_err.contains("cannot assign to immutable binding 'K'"),
        "TW must reject imported-const reassignment; stderr={tw_err:?}"
    );
    assert!(!vm.status.success(), "VM must exit non-zero");
    assert!(!tw.status.success(), "TW must exit non-zero");
}

#[test]
fn repl_vm_dead_const_reassignment_across_lines_runs_fine() {
    // A const reassignment in an UN-ENTERED branch on a later line never executes,
    // so it does NOT error (runtime timing) — both engines keep k=1 and print it.
    let session = "const k = 1\nif (false) { k = 2 }\nk\n";
    let (vm_out, vm_err) = run_repl_session(session, false);
    let (tw_out, _) = run_repl_session(session, true);
    assert!(
        !vm_err.contains("immutable"),
        "dead const reassign must not error; err={vm_err:?}"
    );
    assert!(vm_out.contains('1'), "VM keeps k=1; stdout: {vm_out:?}");
    assert!(tw_out.contains('1'), "TW keeps k=1; stdout: {tw_out:?}");
}

#[test]
fn repl_vm_panic_recovers_and_keeps_prior_bindings() {
    // A bad line (undefined var) reports an error but the loop continues with the
    // prior bindings intact.
    let (out, err) = run_repl_session("let a = 10\nb\na + 1\n", false);
    let combined = format!("{out}{err}");
    assert!(
        combined.contains("undefined variable 'b'"),
        "expected the panic diagnostic; out={out:?} err={err:?}"
    );
    assert!(
        out.contains("11"),
        "prior binding survives the panic; stdout: {out:?}"
    );
}

#[test]
fn repl_vm_exit_ends_the_session() {
    // `exit()` ends the REPL: the line after it is never evaluated.
    let (out, _) = run_repl_session("let z = 1\nexit()\nz + 100\n", false);
    assert!(
        !out.contains("101"),
        "exit() must end the session before the next line; stdout: {out:?}"
    );
}

#[test]
fn repl_vm_builtins_and_print_work() {
    let (out, _) = run_repl_session("print(1 + 2)\n", false);
    assert!(out.contains('3'), "print works; stdout: {out:?}");

    // A bare builtin reference is first-class (yields the builtin value).
    let (out2, _) = run_repl_session("let p = print\np(\"hi\")\n", false);
    assert!(out2.contains("hi"), "first-class builtin; stdout: {out2:?}");
}

#[test]
fn repl_vm_buffers_multiline_input() {
    // Multi-line buffering (unclosed delimiters) still works on the VM REPL.
    let input = "fn add(a, b) {\n  return a + b\n}\nadd(2, 3)\n";
    let (out, _) = run_repl_session(input, false);
    assert!(out.contains('5'), "multiline buffering; stdout: {out:?}");
}

/// Regression: a `worker fn` defined across multiple buffered lines (brace-delimited
/// — `worker` is a contextual keyword, so `is_incomplete` sees only the `{`/`}` tokens
/// and buffers correctly) must persist in the session and be callable on a later REPL
/// input.  This guards against `worker_source` not being set in the REPL, which would
/// produce "program source is unavailable" at call time.
#[test]
fn repl_accepts_multiline_worker_fn_and_calls_it() {
    // Lines 1–3 buffer as one input (open `{`…`}`); line 4 is a separate input.
    let input = "worker fn sq(n) {\n  return n * n\n}\nprint(await sq(6))\n";
    let (out, err) = run_repl_session(input, false);
    assert!(out.contains("36"), "expected 36; stdout: {out:?}  stderr: {err:?}");
}

#[test]
fn repl_tree_walker_flag_still_works() {
    let (out, _) = run_repl_session("let x = 21\nx * 2\n", true);
    assert!(out.contains("42"), "legacy REPL; stdout: {out:?}");
}

#[test]
fn repl_vm_matches_tree_walker_on_a_shared_session() {
    // A session valid in both engines must produce identical stdout.
    let session =
        "let x = 1\nx + 1\nfn sq(n) { return n * n }\nsq(5)\nlet y = x + sq(3)\ny\nexit()\n";
    let (vm_out, _) = run_repl_session(session, false);
    let (tw_out, _) = run_repl_session(session, true);
    assert_eq!(
        vm_out, tw_out,
        "VM-REPL and tree-walker-REPL must agree on stdout"
    );
}

#[test]
fn fmt_subcommand_rewrites_in_place_and_is_idempotent() {
    let file = std::env::temp_dir().join(format!("ascript_fmt_{}.as", std::process::id()));
    // A deliberately messy source: cramped spacing, no spaces around `=`/operators.
    std::fs::write(&file, "let   x=1+2\nfn f(a,b){return a+b}").unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");

    // First format: should succeed and rewrite the file to canonical form.
    let out = Command::new(bin).arg("fmt").arg(&file).output().unwrap();
    assert!(out.status.success(), "fmt failed: {:?}", out);
    assert!(String::from_utf8_lossy(&out.stdout).contains("formatted"));

    let after = std::fs::read_to_string(&file).unwrap();
    // The messy input must have changed to canonical, spaced form.
    assert_ne!(after, "let   x=1+2\nfn f(a,b){return a+b}");
    assert!(after.contains("let x = 1 + 2"), "got: {after:?}");

    // Second format: idempotent — running fmt again leaves the file unchanged.
    let out2 = Command::new(bin).arg("fmt").arg(&file).output().unwrap();
    assert!(out2.status.success(), "second fmt failed: {:?}", out2);
    let after2 = std::fs::read_to_string(&file).unwrap();
    assert_eq!(after, after2, "fmt must be idempotent");

    let _ = std::fs::remove_file(&file);
}

#[test]
fn test_runner_reports_pass_and_fail() {
    let file = std::env::temp_dir().join(format!("ascript_tr_{}.as", std::process::id()));
    std::fs::write(
        &file,
        "test(\"adds\", () => { assert(1 + 1 == 2) })\ntest(\"fails\", () => { assert(false, \"boom\") })",
    )
    .unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = std::process::Command::new(bin)
        .arg("test")
        .arg(&file)
        .output()
        .unwrap();
    let s =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(s.contains("1 passed") || s.contains("passed"));
    assert!(s.contains("fails") && s.contains("boom"));
    assert!(!out.status.success()); // a failing test → non-zero exit
}

#[test]
#[cfg(feature = "crypto")] // program imports std/crypto; only valid with the crypto feature.
fn runs_crypto_sha256_and_password_roundtrip() {
    let file = std::env::temp_dir().join(format!("ascript_crypto_{}.as", std::process::id()));
    std::fs::write(
        &file,
        "import { sha256, hashPassword, verifyPassword } from \"std/crypto\"\n\
         print(sha256(\"abc\"))\n\
         const [phc, err] = hashPassword(\"hunter2\")\n\
         print(err == nil)\n\
         print(verifyPassword(\"hunter2\", phc))\n\
         print(verifyPassword(\"nope\", phc))",
    )
    .unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = std::process::Command::new(bin)
        .arg("run")
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\ntrue\ntrue\nfalse\n"
    );
    let _ = std::fs::remove_file(&file);
}

// The system capstone imports std/fs, std/crypto, std/compress, std/sqlite,
// std/process, std/env and std/encoding, so it only loads with all those
// features. It runs `echo`, so it is additionally gated to unix (mirroring the
// existing process integration tests).
#[test]
#[cfg(all(
    feature = "sys",
    feature = "crypto",
    feature = "compress",
    feature = "sql",
    unix
))]
fn runs_system_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/system.as")
        .output()
        .unwrap();
    assert!(output.status.success(), "process failed: {:?}", output);
    let out = String::from_utf8_lossy(&output.stdout);
    // crypto: sha256("abc")
    assert!(out.contains("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
    // fs.grep: 2 matches, first on line 2 ("beta TODO").
    assert_eq!(out.lines().nth(1), Some("2")); // match count
    assert_eq!(out.lines().nth(2), Some("2")); // first match line number
                                               // compress: gzip -> gunzip round-trip is lossless.
    assert!(out.contains("true"));
    // sqlite: queried row value.
    assert!(out.contains("ada"));
    // process: echo's captured stdout.
    assert!(out.contains("hello-from-subprocess"));
    // env: round-tripped variable.
    assert!(out.contains("demo-value"));
}

// std/net/tcp loopback echo example (examples/net.as). Gated on `net` (the module)
// + `unix` (the loopback-into-backlog sequencing is exercised on unix in CI; the
// example itself is portable, but we keep the gate consistent with other socket
// tests). Asserts the full round-trip: no errors, then "ping" (server read what
// the client wrote) then "pong" (client read the server's echo).
#[test]
#[cfg(all(feature = "net", unix))]
fn runs_net_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/net.as")
        .output()
        .unwrap();
    assert!(output.status.success(), "process failed: {:?}", output);
    let out = String::from_utf8_lossy(&output.stdout);
    // Three nil error slots (listen/connect/accept), then the round-tripped lines.
    assert_eq!(out, "nil\nnil\nnil\nping\npong\n");
}

// std/tui off-screen-buffer demo (examples/tui.as). Gated on `tui` (the module).
// The example draws into a fixed 14×4 off-screen buffer via tui.buffer(w,h) and
// prints dump() — fully deterministic (no real tty, no raw mode, no flush), so we
// assert the exact rendered frame: the box border chars plus the placed text.
#[test]
#[cfg(feature = "tui")]
fn runs_tui_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/tui.as")
        .output()
        .unwrap();
    assert!(output.status.success(), "process failed: {:?}", output);
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("AScript"), "missing AScript text:\n{}", out);
    assert!(out.contains("TUI demo"), "missing TUI demo text:\n{}", out);
    // Box-drawing border chars.
    assert!(out.contains('┌'), "missing top-left corner:\n{}", out);
    assert!(out.contains('│'), "missing vertical border:\n{}", out);
    assert!(out.contains('┘'), "missing bottom-right corner:\n{}", out);
    // Full exact frame (dump trims trailing spaces per row).
    assert!(
        out.contains("┌────────────┐\n│ AScript    │\n│ TUI demo   │\n└────────────┘\n"),
        "frame mismatch:\n{}",
        out
    );
}

#[test]
fn runs_generators_example_terminates_and_prints() {
    // End-to-end: the binary runs the generators example to completion. This goes
    // through the real entry-point exit drain (`local.await`), so a regressed
    // task-based generator (zombie task parked in `yield`) would HANG here. The
    // test reaching its asserts is the proof that consumer-driven generators exit.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = Command::new(bin)
        .arg("run")
        .arg("examples/generators.as")
        .output()
        .unwrap();
    assert!(out.status.success(), "generators.as did not exit cleanly");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("6"), "got: {stdout}");
}

#[test]
fn abandoned_infinite_generator_with_break_terminates() {
    // An infinite generator broken after 3 values: the classic abandoned-generator
    // case that hung before generators became consumer-driven. The binary must
    // print 0 1 2 done and EXIT (the test process would hang if it regressed).
    let dir = std::env::temp_dir();
    let path = dir.join("ascript_gen_break_cli.as");
    std::fs::write(
        &path,
        "fn* g() { let i = 0\nwhile (true) { yield i\ni = i + 1 } }\n\
         for await (x in g()) { print(x)\nif (x >= 2) { break } }\n\
         print(\"done\")\n",
    )
    .unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = Command::new(bin)
        .arg("run")
        .arg(path.to_str().unwrap())
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "abandoned generator program did not exit"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout, "0\n1\n2\ndone\n", "got: {stdout}");
}

#[test]
fn run_streams_output_and_keeps_it_before_panic() {
    // `print("before")` runs, then `len(1, 2, 3)` is a runtime (Tier-2) panic
    // because its first arg is a number. Under OutputSink::Live the "before"
    // output is streamed to stdout immediately, so it must survive the panic
    // even though `run_file` returns `Err`.
    let path = std::env::temp_dir().join("ascript_stream_before_panic.as");
    std::fs::write(&path, "print(\"before\")\nlen(1, 2, 3)\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = Command::new(bin)
        .arg("run")
        .arg(path.to_str().unwrap())
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "program should have panicked at len(1, 2, 3); stdout: {stdout:?}"
    );
    assert!(stdout.contains("before"), "stdout was: {stdout:?}");
}

#[test]
fn runs_object_destructuring_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/object_destructuring.as")
        .output()
        .unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Ada\nadmin\n42\nnil\n7\n"
    );
}

#[test]
fn runs_spread_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/spread.as")
        .output()
        .unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "[0, 1, 2, 3, 4]\n{host: \"local\", port: 443}\n60\n"
    );
}

#[test]
fn runs_rest_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/rest.as")
        .output()
        .unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "10\n0\nnums\n[1, 2]\n10\n[20, 30]\n7\n{role: \"admin\", active: true}\n18\n"
    );
}

#[test]
#[cfg(feature = "log")]
fn runs_logging_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/logging.as")
        .output()
        .unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    // Logs go to STDERR, never STDOUT.
    assert!(
        output.stdout.is_empty(),
        "logs must not go to stdout; stdout was: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("[DEBUG] starting pid=42"),
        "stderr was: {err:?}"
    );
    assert!(err.contains("[INFO] request"), "stderr was: {err:?}");
    assert!(err.contains("[WARN] slow query"), "stderr was: {err:?}");
    assert!(
        err.contains("[ERROR] upstream failed"),
        "stderr was: {err:?}"
    );
    assert!(err.contains("\"msg\":\"saved\""), "stderr was: {err:?}");
}

// ─── exit() builtin tests ─────────────────────────────────────────────────────

#[test]
fn exit_code_2_returns_process_exit_2() {
    let path = std::env::temp_dir().join(format!("ascript_exit2_{}.as", std::process::id()));
    std::fs::write(&path, "exit(2)\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = Command::new(bin).arg("run").arg(&path).output().unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit code 2, got {:?}",
        out.status.code()
    );
}

#[test]
fn exit_code_0_returns_success() {
    let path = std::env::temp_dir().join(format!("ascript_exit0_{}.as", std::process::id()));
    std::fs::write(&path, "print(\"hi\")\nexit(0)\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = Command::new(bin).arg("run").arg(&path).output().unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected exit code 0, got {:?}",
        out.status.code()
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("hi"), "stdout: {stdout}");
}

#[test]
fn exit_no_arg_defaults_to_0() {
    let path = std::env::temp_dir().join(format!("ascript_exitnoarg_{}.as", std::process::id()));
    std::fs::write(&path, "exit()\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = Command::new(bin).arg("run").arg(&path).output().unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected exit code 0 for exit(), got {:?}",
        out.status.code()
    );
}

#[test]
fn exit_during_test_run_is_a_clear_failure_not_fake_pass() {
    // `exit()` inside a test file must NOT report an empty all-pass summary (exit 0).
    // It is a hard error for `ascript test`: non-zero exit + a clear message.
    let path = std::env::temp_dir().join(format!("ascript_test_exit_{}.as", std::process::id()));
    std::fs::write(&path, "test(\"calls exit\", () => { exit(0) })\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = Command::new(bin).arg("test").arg(&path).output().unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        !out.status.success(),
        "exit() in a test must fail the run, got status {:?}",
        out.status.code()
    );
    let combined =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        combined.contains("exit() called during test run"),
        "expected a clear message; got: {combined}"
    );
}

#[test]
fn exit_out_of_range_panics() {
    // exit(300) is out of 0..=255 — must be a Tier-2 panic → exit code 1 from the diagnostic path.
    let path = std::env::temp_dir().join(format!("ascript_exit_oor_{}.as", std::process::id()));
    std::fs::write(&path, "exit(300)\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = Command::new(bin).arg("run").arg(&path).output().unwrap();
    let _ = std::fs::remove_file(&path);
    // The panic surfaces as a diagnostic on stderr, process exits 1.
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit code 1 for out-of-range exit(300), got {:?}",
        out.status.code()
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("0..=255"),
        "expected 0..=255 in error; stderr: {stderr}"
    );
}

// ---- std/io stdin tests ----

#[test]
#[cfg(feature = "sys")]
fn io_readline_returns_first_line() {
    use std::io::Write;
    use std::process::Stdio;
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join(format!("ascript_io_rl_{}.as", std::process::id()));
    std::fs::write(
        &file,
        "import * as io from \"std/io\"\nprint(await io.readLine())\n",
    )
    .unwrap();
    let mut child = Command::new(bin)
        .arg("run")
        .arg(&file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"hello\nworld\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello\n");
}

#[test]
#[cfg(feature = "sys")]
fn io_readline_multiple_calls_buffers_correctly() {
    use std::io::Write;
    use std::process::Stdio;
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join(format!("ascript_io_rl2_{}.as", std::process::id()));
    std::fs::write(
        &file,
        "import * as io from \"std/io\"\nprint(await io.readLine())\nprint(await io.readLine())\n",
    )
    .unwrap();
    let mut child = Command::new(bin)
        .arg("run")
        .arg(&file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"hello\nworld\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello\nworld\n");
}

#[test]
#[cfg(feature = "sys")]
fn io_readall_returns_full_stdin() {
    use std::io::Write;
    use std::process::Stdio;
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join(format!("ascript_io_ra_{}.as", std::process::id()));
    std::fs::write(
        &file,
        "import * as io from \"std/io\"\nprint(await io.readAll())\n",
    )
    .unwrap();
    let mut child = Command::new(bin)
        .arg("run")
        .arg(&file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"abc").unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "abc\n");
}

#[test]
#[cfg(feature = "sys")]
fn io_readall_is_utf8_lossy_on_invalid_bytes() {
    use std::io::Write;
    use std::process::Stdio;
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join(format!("ascript_io_lossy_{}.as", std::process::id()));
    // readAll must NOT error on invalid UTF-8; it returns a string (with the
    // U+FFFD replacement char for the bad byte).
    std::fs::write(
        &file,
        "import * as io from \"std/io\"\nlet s = await io.readAll()\nprint(type(s))\nprint(len(s) > 0)\n",
    )
    .unwrap();
    let mut child = Command::new(bin)
        .arg("run")
        .arg(&file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // 0xff is an invalid lone byte; followed by valid "hi".
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&[0xff, b'h', b'i'])
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        out.status.success(),
        "readAll must not error on invalid UTF-8; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "string\ntrue\n");
}

#[test]
#[cfg(feature = "sys")]
fn io_readline_eof_returns_nil() {
    use std::process::Stdio;
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join(format!("ascript_io_eof_{}.as", std::process::id()));
    std::fs::write(
        &file,
        "import * as io from \"std/io\"\nprint(await io.readLine())\n",
    )
    .unwrap();
    // No stdin written — immediate EOF
    let mut child = Command::new(bin)
        .arg("run")
        .arg(&file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Drop stdin immediately to signal EOF
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "nil\n");
}

#[test]
#[cfg(feature = "sys")]
fn io_readlines_returns_all_lines() {
    use std::io::Write;
    use std::process::Stdio;
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join(format!("ascript_io_rls_{}.as", std::process::id()));
    std::fs::write(
        &file,
        "import * as io from \"std/io\"\nlet lines = await io.readLines()\nfor (l in lines) { print(l) }\n",
    )
    .unwrap();
    let mut child = Command::new(bin)
        .arg("run")
        .arg(&file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"alpha\nbeta\ngamma\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "alpha\nbeta\ngamma\n");
}

// ---- env.args() tests ----

#[test]
#[cfg(feature = "sys")] // `std/env` is sys-gated; the binary lacks it under --no-default-features.
fn env_args_returns_script_args() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join("ascript_env_args_test.as");
    std::fs::write(&file, "import { args } from \"std/env\"\nprint(args())\n").unwrap();
    let out = Command::new(bin)
        .arg("run")
        .arg(&file)
        .arg("a")
        .arg("b")
        .arg("--x")
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(out.status.success(), "process failed: {:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // AScript array Display quotes strings: ["a", "b", "--x"]
    assert!(
        stdout.contains("[\"a\", \"b\", \"--x\"]"),
        "expected [\"a\", \"b\", \"--x\"] in stdout; got: {stdout}"
    );
}

#[test]
#[cfg(feature = "sys")] // `std/env` is sys-gated; the binary lacks it under --no-default-features.
fn env_args_no_args_returns_empty_array() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join("ascript_env_args_empty.as");
    std::fs::write(&file, "import { args } from \"std/env\"\nprint(args())\n").unwrap();
    let out = Command::new(bin).arg("run").arg(&file).output().unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(out.status.success(), "process failed: {:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[]"),
        "expected [] in stdout; got: {stdout}"
    );
}

#[test]
fn vm_stepped_for_range_iterates() {
    // PHASE 3: `step` is fully wired on the VM. A stepped for-range strides by the
    // (signed) step — never silently the unstepped `1..10`.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join("ascript_stepped_for_range.as");
    std::fs::write(&file, "for (i in 1..10 step 2) { print(i) }\n").unwrap();
    let out = Command::new(bin).arg("run").arg(&file).output().unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        out.status.success(),
        "stepped for-range must run: {:?}",
        out
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n3\n5\n7\n9\n");
}

#[test]
fn vm_stepped_value_range_materializes() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join("ascript_stepped_value_range.as");
    std::fs::write(&file, "print(1..10 step 2)\n").unwrap();
    let out = Command::new(bin).arg("run").arg(&file).output().unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        out.status.success(),
        "stepped value range must run: {:?}",
        out
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[1, 3, 5, 7, 9]\n");
}

#[test]
fn vm_step_stays_usable_as_identifier() {
    // The contextual `step` keyword must not break ordinary identifier use on the
    // default (VM) engine.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join("ascript_step_identifier.as");
    std::fs::write(&file, "let step = 7\nprint(step)\n").unwrap();
    let out = Command::new(bin).arg("run").arg(&file).output().unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(out.status.success(), "process failed: {:?}", out);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "7\n");
}

// ===== RANGES FEATURE, Phase 2: inclusive `..=` ranges (both engines) =========

/// Run `src` as a `.as` program on a chosen engine, returning stdout. Panics if
/// the process fails (so callers assert on successful output).
fn run_range_src(src: &str, tree_walker: bool, tag: &str) -> String {
    let file = std::env::temp_dir().join(format!("ascript_range_{tag}.as"));
    std::fs::write(&file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let mut cmd = Command::new(bin);
    cmd.arg("run");
    if tree_walker {
        cmd.arg("--tree-walker");
    }
    let out = cmd.arg(&file).output().unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        out.status.success(),
        "process failed ({}): {:?}",
        if tree_walker { "tree-walker" } else { "vm" },
        out
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn inclusive_range_iteration_and_value_both_engines() {
    // The four target cases must be byte-identical on the default VM AND the
    // tree-walker: inclusive for-range, inclusive value materialization, the
    // single-element `5..=5`, and (Phase 4) the descending inclusive `10..=1`
    // counting down.
    let cases = [
        ("for (i in 1..=4) { print(i) }", "1\n2\n3\n4\n"),
        ("print(1..=5)", "[1, 2, 3, 4, 5]\n"),
        ("print(5..=5)", "[5]\n"),
        ("print(10..=1)", "[10, 9, 8, 7, 6, 5, 4, 3, 2, 1]\n"),
    ];
    for (i, (src, expected)) in cases.iter().enumerate() {
        let vm = run_range_src(src, false, &format!("vm_{i}"));
        let tw = run_range_src(src, true, &format!("tw_{i}"));
        assert_eq!(vm, *expected, "VM output wrong for `{src}`");
        assert_eq!(tw, *expected, "tree-walker output wrong for `{src}`");
        assert_eq!(vm, tw, "VM and tree-walker diverged for `{src}`");
    }
}

#[test]
fn exclusive_range_unchanged_both_engines() {
    // Exclusive `..` behavior is unchanged by Phase 2.
    let cases = [
        ("for (i in 1..4) { print(i) }", "1\n2\n3\n"),
        ("print(1..5)", "[1, 2, 3, 4]\n"),
        ("print(5..5)", "[]\n"),
    ];
    for (i, (src, expected)) in cases.iter().enumerate() {
        let vm = run_range_src(src, false, &format!("excl_vm_{i}"));
        let tw = run_range_src(src, true, &format!("excl_tw_{i}"));
        assert_eq!(vm, *expected, "VM output wrong for `{src}`");
        assert_eq!(tw, *expected, "tree-walker output wrong for `{src}`");
        assert_eq!(vm, tw, "VM and tree-walker diverged for `{src}`");
    }
}

#[test]
fn inclusive_match_pattern_still_works_both_engines() {
    // `match n { 1..=10 => "in", _ => "out" }` with n=5 → "in" (pre-existing).
    let src = "let n = 5\nprint(match n { 1..=10 => \"in\", _ => \"out\" })";
    let vm = run_range_src(src, false, "match_vm");
    let tw = run_range_src(src, true, "match_tw");
    assert_eq!(vm, "in\n");
    assert_eq!(tw, "in\n");
}

#[test]
fn stepped_ranges_iterate_both_engines() {
    // RANGES FEATURE, Phase 3: `step` iteration evaluates on BOTH engines, sign
    // honored as direction, byte-identical between the VM and the tree-walker.
    let cases = [
        ("for (i in 1..10 step 2) { print(i) }", "1\n3\n5\n7\n9\n"),
        ("for (i in 10..1 step -2) { print(i) }", "10\n8\n6\n4\n2\n"),
        ("print(1..=10 step 2)", "[1, 3, 5, 7, 9]\n"),
        ("print(0..=1 step 0.25)", "[0, 0.25, 0.5, 0.75, 1]\n"),
        // omitted-step descending: present-step rows above are unaffected by
        // Phase 4 (which only changes the OMITTED-step default direction).
        ("print(10..1 step -2)", "[10, 8, 6, 4, 2]\n"),
    ];
    for (i, (src, expected)) in cases.iter().enumerate() {
        let vm = run_range_src(src, false, &format!("step_vm_{i}"));
        let tw = run_range_src(src, true, &format!("step_tw_{i}"));
        assert_eq!(vm, *expected, "VM output wrong for `{src}`");
        assert_eq!(tw, *expected, "tree-walker output wrong for `{src}`");
        assert_eq!(vm, tw, "VM and tree-walker diverged for `{src}`");
    }
}

#[test]
fn stepped_class_field_default_both_engines() {
    // RANGES FEATURE: a STEPPED range used as a class FIELD DEFAULT must
    // materialize identically on the VM and the tree-walker. This is the path
    // that was guarded out of the VM's `cst_default_expr` field-default lowering
    // (the non-stepped `1..=4` default already worked byte-identically). Covers
    // ascending, descending, and inclusive stepped defaults.
    let cases = [
        (
            "class Box { vals: array<number> = 1..10 step 2 }\nlet b = Box()\nprint(b.vals)",
            "[1, 3, 5, 7, 9]\n",
        ),
        (
            "class Box { vals: array<number> = 10..1 step -2 }\nlet b = Box()\nprint(b.vals)",
            "[10, 8, 6, 4, 2]\n",
        ),
        (
            "class Box { vals: array<number> = 0..=10 step 5 }\nlet b = Box()\nprint(b.vals)",
            "[0, 5, 10]\n",
        ),
    ];
    for (i, (src, expected)) in cases.iter().enumerate() {
        let vm = run_range_src(src, false, &format!("fieldstep_vm_{i}"));
        let tw = run_range_src(src, true, &format!("fieldstep_tw_{i}"));
        assert_eq!(vm, *expected, "VM output wrong for `{src}`");
        assert_eq!(tw, *expected, "tree-walker output wrong for `{src}`");
        assert_eq!(vm, tw, "VM and tree-walker diverged for `{src}`");
    }
}

#[test]
fn descending_bare_range_counts_down_both_engines() {
    // RANGES FEATURE, Phase 4: when `step` is OMITTED the direction is inferred
    // from the bounds, so a bare descending range counts DOWN (was empty). Both
    // engines must agree byte-for-byte. Empty/single-element and ascending cases
    // are pinned as regression guards alongside.
    let cases = [
        // descending bare ranges now count down (THE Phase 4 behavior change).
        ("for (i in 5..1) { print(i) }", "5\n4\n3\n2\n"),
        ("for (i in 5..=1) { print(i) }", "5\n4\n3\n2\n1\n"),
        ("print(10..1)", "[10, 9, 8, 7, 6, 5, 4, 3, 2]\n"),
        ("print(10..=1)", "[10, 9, 8, 7, 6, 5, 4, 3, 2, 1]\n"),
        // UNCHANGED: ascending bare ranges.
        ("print(1..5)", "[1, 2, 3, 4]\n"),
        ("print(1..=5)", "[1, 2, 3, 4, 5]\n"),
        // UNCHANGED: equal bounds are empty (exclusive) / single (inclusive).
        ("print(5..5)", "[]\n"),
        ("print(5..=5)", "[5]\n"),
    ];
    for (i, (src, expected)) in cases.iter().enumerate() {
        let vm = run_range_src(src, false, &format!("down_vm_{i}"));
        let tw = run_range_src(src, true, &format!("down_tw_{i}"));
        assert_eq!(vm, *expected, "VM output wrong for `{src}`");
        assert_eq!(tw, *expected, "tree-walker output wrong for `{src}`");
        assert_eq!(vm, tw, "VM and tree-walker diverged for `{src}`");
    }
}

#[test]
fn stepped_ranges_validation_panics_both_engines() {
    // RANGES FEATURE, Phase 3: zero/non-finite step and direction mismatch are
    // Tier-2 panics (exit 1) with byte-identical messages across engines.
    for (i, (src, msg)) in [
        (
            "for (i in 1..10 step 0) {}",
            "step must be a finite, non-zero number",
        ),
        (
            "for (i in 1..10 step -2) {}",
            "step -2 moves away from end (10); range can never progress",
        ),
        (
            "for (i in 10..1 step 2) {}",
            "step 2 moves away from end (1); range can never progress",
        ),
    ]
    .iter()
    .enumerate()
    {
        let file = std::env::temp_dir().join(format!("ascript_step_panic_{i}.as"));
        std::fs::write(&file, src).unwrap();
        let bin = env!("CARGO_BIN_EXE_ascript");
        for tw in [false, true] {
            let mut cmd = Command::new(bin);
            cmd.arg("run");
            if tw {
                cmd.arg("--tree-walker");
            }
            let out = cmd.arg(&file).output().unwrap();
            assert!(
                !out.status.success(),
                "{} should PANIC on `{src}`, got {:?}",
                if tw { "tree-walker" } else { "vm" },
                out
            );
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                stderr.contains(msg),
                "{} stderr for `{src}` should contain {msg:?}, got: {stderr}",
                if tw { "tree-walker" } else { "vm" }
            );
        }
        let _ = std::fs::remove_file(&file);
    }
}

#[test]
fn stdlib_stream_range_unified_model_both_engines() {
    // RANGES FEATURE, Phase 6: `stream.range` follows the SAME unified model as
    // the `..` syntax. Omitted step infers direction from the bounds (counts down
    // for `range(10, 1)`); the three documented examples are unchanged; both
    // engines agree byte-for-byte.
    let cases = [
        // CHANGED: omitted step infers descending (was `[]`).
        (
            "import { range, collect } from \"std/stream\"\nprint(await collect(range(10, 1)))",
            "[10, 9, 8, 7, 6, 5, 4, 3, 2]\n",
        ),
        // UNCHANGED documented examples.
        (
            "import { range, collect } from \"std/stream\"\nprint(await collect(range(0, 5)))",
            "[0, 1, 2, 3, 4]\n",
        ),
        (
            "import { range, collect } from \"std/stream\"\nprint(await collect(range(0, 10, 2)))",
            "[0, 2, 4, 6, 8]\n",
        ),
        (
            "import { range, collect } from \"std/stream\"\nprint(await collect(range(10, 0, -3)))",
            "[10, 7, 4, 1]\n",
        ),
    ];
    for (i, (src, expected)) in cases.iter().enumerate() {
        let vm = run_range_src(src, false, &format!("streamrange_vm_{i}"));
        let tw = run_range_src(src, true, &format!("streamrange_tw_{i}"));
        assert_eq!(vm, *expected, "VM output wrong for `{src}`");
        assert_eq!(tw, *expected, "tree-walker output wrong for `{src}`");
        assert_eq!(vm, tw, "VM and tree-walker diverged for `{src}`");
    }
}

#[test]
fn stdlib_stream_range_validation_panics_both_engines() {
    // Phase 6: `stream.range` validation panics are byte-identical to the range
    // SYNTAX panics (shared `resolve_step`): sign-mismatch and zero step.
    for (i, (src, msg)) in [
        (
            "import { range, collect } from \"std/stream\"\nawait collect(range(1, 10, -2))",
            "step -2 moves away from end (10); range can never progress",
        ),
        (
            "import { range, collect } from \"std/stream\"\nawait collect(range(1, 10, 0))",
            "step must be a finite, non-zero number",
        ),
    ]
    .iter()
    .enumerate()
    {
        let file = std::env::temp_dir().join(format!("ascript_streamrange_panic_{i}.as"));
        std::fs::write(&file, src).unwrap();
        let bin = env!("CARGO_BIN_EXE_ascript");
        for tw in [false, true] {
            let mut cmd = Command::new(bin);
            cmd.arg("run");
            if tw {
                cmd.arg("--tree-walker");
            }
            let out = cmd.arg(&file).output().unwrap();
            assert!(
                !out.status.success(),
                "{} should PANIC on `{src}`, got {:?}",
                if tw { "tree-walker" } else { "vm" },
                out
            );
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                stderr.contains(msg),
                "{} stderr for `{src}` should contain {msg:?}, got: {stderr}",
                if tw { "tree-walker" } else { "vm" }
            );
        }
        let _ = std::fs::remove_file(&file);
    }
}

#[test]
fn stepped_match_patterns_both_engines() {
    // RANGES FEATURE, Phase 5: `step` in a match-range pattern = strided
    // membership (spec §3.7), byte-identical on the VM and the tree-walker.
    // Anchor is `start`, so parity/offset depends on where the range begins.
    let cases = [
        // 3 ∈ {1,3,5,7,9}
        (
            "print(match 3 { 1..=10 step 2 => \"in\", _ => \"out\" })",
            "in\n",
        ),
        // 4 ∉ {1,3,5,7,9}
        (
            "print(match 4 { 1..=10 step 2 => \"in\", _ => \"out\" })",
            "out\n",
        ),
        // 4 ∈ {0,2,4,6,8,10} (anchor 0)
        (
            "print(match 4 { 0..=10 step 2 => \"in\", _ => \"out\" })",
            "in\n",
        ),
        // 11 out of bounds
        (
            "print(match 11 { 1..=10 step 2 => \"in\", _ => \"out\" })",
            "out\n",
        ),
        // exclusive end: 10 not in {0,2,4,6,8}
        (
            "print(match 10 { 0..10 step 2 => \"in\", _ => \"out\" })",
            "out\n",
        ),
        // inclusive end: 10 ∈ {0,2,...,10}
        (
            "print(match 10 { 0..=10 step 2 => \"in\", _ => \"out\" })",
            "in\n",
        ),
        // descending stepped pattern: 8 ∈ {10,8,6,4,2}
        (
            "print(match 8 { 10..=2 step -2 => \"in\", _ => \"out\" })",
            "in\n",
        ),
        // plain (no-step) pattern is UNCHANGED.
        ("print(match 5 { 1..=10 => \"in\", _ => \"out\" })", "in\n"),
    ];
    for (i, (src, expected)) in cases.iter().enumerate() {
        let vm = run_range_src(src, false, &format!("patstep_vm_{i}"));
        let tw = run_range_src(src, true, &format!("patstep_tw_{i}"));
        assert_eq!(vm, *expected, "VM output wrong for `{src}`");
        assert_eq!(tw, *expected, "tree-walker output wrong for `{src}`");
        assert_eq!(vm, tw, "VM and tree-walker diverged for `{src}`");
    }
}

#[test]
fn stepped_match_pattern_validation_panics_both_engines() {
    // RANGES FEATURE, Phase 5: a stepped pattern runs the SAME shared validator
    // as iteration, so a `step 0` / direction-mismatch pattern PANICS with the
    // byte-identical message on both engines.
    for (i, (src, msg)) in [
        (
            "print(match 5 { 1..=10 step 0 => 1, _ => 0 })",
            "step must be a finite, non-zero number",
        ),
        (
            "print(match 5 { 1..=10 step -2 => 1, _ => 0 })",
            "step -2 moves away from end (10); range can never progress",
        ),
    ]
    .iter()
    .enumerate()
    {
        let file = std::env::temp_dir().join(format!("ascript_patstep_panic_{i}.as"));
        std::fs::write(&file, src).unwrap();
        let bin = env!("CARGO_BIN_EXE_ascript");
        for tw in [false, true] {
            let mut cmd = Command::new(bin);
            cmd.arg("run");
            if tw {
                cmd.arg("--tree-walker");
            }
            let out = cmd.arg(&file).output().unwrap();
            assert!(
                !out.status.success(),
                "{} should PANIC on `{src}`, got {:?}",
                if tw { "tree-walker" } else { "vm" },
                out
            );
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                stderr.contains(msg),
                "{} stderr for `{src}` should contain {msg:?}, got: {stderr}",
                if tw { "tree-walker" } else { "vm" }
            );
        }
        let _ = std::fs::remove_file(&file);
    }
}

/// Strip ANSI SGR escape sequences so the rendered ariadne caret can be compared
/// as plain text.
fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Skip until the terminating 'm' of the SGR sequence.
            for d in chars.by_ref() {
                if d == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn stepped_value_range_panic_underlines_step_clause_both_engines() {
    // Cleanup A: the legacy parser's `ExprKind::Range` span now covers through
    // the END of the `step` clause, so a panic on a stepped VALUE range
    // underlines the whole `1..10 step 0` on BOTH engines (previously the
    // tree-walker underlined only `1..10`). The ariadne caret must:
    //   (a) include the `step 0` text in the underlined region, and
    //   (b) be byte-identical between the VM and the tree-walker (modulo the
    //       file-path header line, which can differ by a `/private` symlink).
    let src = "print(1..10 step 0)";
    let file = std::env::temp_dir().join("ascript_step_span.as");
    std::fs::write(&file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");

    let render = |tw: bool| -> String {
        let mut cmd = Command::new(bin);
        cmd.arg("run");
        if tw {
            cmd.arg("--tree-walker");
        }
        let out = cmd.arg(&file).output().unwrap();
        assert!(!out.status.success(), "should panic ({tw})");
        // Drop the `╭─[ <path>:1:7 ]` header line (path differs by /private);
        // keep the source + caret rows, which carry the span.
        strip_ansi(&String::from_utf8_lossy(&out.stderr))
            .lines()
            .filter(|l| !l.contains(".as:"))
            .map(|l| l.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };

    let vm = render(false);
    let tw = render(true);
    let _ = std::fs::remove_file(&file);

    // (a) the caret region reaches the `step 0` clause: the underline row (the
    // one with the `┬` tick) must be at least as wide as the start column of
    // `step` (the legacy bug truncated it at `1..10`, before `step`).
    let underline = vm
        .lines()
        .find(|l| l.contains('┬'))
        .expect("expected a caret/underline row");
    // `1..10 step 0` starts at col 7 (1-based); the `step` keyword begins 6
    // chars later. The full clause is 12 chars wide. Count the run of `─`/`┬`.
    let span_width = underline.chars().filter(|&c| c == '─' || c == '┬').count();
    assert!(
        span_width >= 12,
        "underline should span the full `1..10 step 0` (>=12), got {span_width} in:\n{vm}"
    );

    // (b) byte-identical carets across engines (this is the real divergence fix).
    assert_eq!(
        vm, tw,
        "VM and tree-walker rendered different carets:\n--- vm ---\n{vm}\n--- tw ---\n{tw}"
    );
}

#[test]
fn check_range_step_lint_and_allow_suppression() {
    // Phase 7: the `range-step` static lint surfaces a statically-detectable bad
    // range (here `step 0`) at author-time, matching the runtime panic text. It is
    // configurable: `--allow range-step` suppresses it.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file =
        std::env::temp_dir().join(format!("ascript_range_step_lint_{}.as", std::process::id()));
    std::fs::write(&file, "for (i in 1..10 step 0){}\n").unwrap();

    // Default: the diagnostic appears (JSON output for a robust assertion).
    let out = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg(&file)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"code\":\"range-step\""),
        "expected a range-step diagnostic, got:\n{stdout}"
    );
    assert!(
        stdout.contains("step must be a finite, non-zero number"),
        "expected the runtime-matching message, got:\n{stdout}"
    );

    // `--allow range-step` suppresses it (and the code is a known/configurable rule).
    let allowed = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg("--allow")
        .arg("range-step")
        .arg(&file)
        .output()
        .unwrap();
    let allowed_out = String::from_utf8_lossy(&allowed.stdout);
    assert!(
        !allowed_out.contains("\"code\":\"range-step\""),
        "--allow range-step should suppress the diagnostic, got:\n{allowed_out}"
    );

    let _ = std::fs::remove_file(&file);
}

#[test]
fn check_call_arity_lint_and_allow_suppression() {
    // The `call-arity` static lint flags a call to a uniquely-resolved file-local
    // function with the wrong number of positional args, mirroring the runtime
    // arity panic. Configurable: `--allow call-arity` suppresses it.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file =
        std::env::temp_dir().join(format!("ascript_call_arity_lint_{}.as", std::process::id()));
    std::fs::write(&file, "fn f(a,b){ return a }\nf(1,2,3)\n").unwrap();

    // Default: the diagnostic appears (JSON output for a robust assertion).
    let out = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg(&file)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"code\":\"call-arity\""),
        "expected a call-arity diagnostic, got:\n{stdout}"
    );

    // `--allow call-arity` suppresses it (and the code is a known/configurable rule).
    let allowed = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg("--allow")
        .arg("call-arity")
        .arg(&file)
        .output()
        .unwrap();
    let allowed_out = String::from_utf8_lossy(&allowed.stdout);
    assert!(
        !allowed_out.contains("\"code\":\"call-arity\""),
        "--allow call-arity should suppress the diagnostic, got:\n{allowed_out}"
    );

    let _ = std::fs::remove_file(&file);
}

#[test]
fn check_unknown_enum_variant_lint_and_allow_suppression() {
    // The `unknown-enum-variant` static lint flags access of a non-existent variant
    // on a statically-known enum, mirroring the runtime `enum E has no variant 'V'`
    // panic. Configurable: `--allow unknown-enum-variant` suppresses it.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join(format!(
        "ascript_unknown_enum_variant_{}.as",
        std::process::id()
    ));
    std::fs::write(&file, "enum Color { Red, Green }\nprint(Color.Reddd)\n").unwrap();

    // Default: the diagnostic appears (JSON output for a robust assertion).
    let out = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg(&file)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"code\":\"unknown-enum-variant\""),
        "expected an unknown-enum-variant diagnostic, got:\n{stdout}"
    );
    assert!(
        stdout.contains("enum Color has no variant 'Reddd'"),
        "expected the runtime-matching message, got:\n{stdout}"
    );

    // `--allow unknown-enum-variant` suppresses it.
    let allowed = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg("--allow")
        .arg("unknown-enum-variant")
        .arg(&file)
        .output()
        .unwrap();
    let allowed_out = String::from_utf8_lossy(&allowed.stdout);
    assert!(
        !allowed_out.contains("\"code\":\"unknown-enum-variant\""),
        "--allow unknown-enum-variant should suppress the diagnostic, got:\n{allowed_out}"
    );

    let _ = std::fs::remove_file(&file);
}

#[test]
fn check_duplicate_member_lint_and_allow_suppression() {
    // The `duplicate-member` static lint flags two members with the same name in
    // one class body (a silent shadow at runtime). Configurable: `--allow
    // duplicate-member` suppresses it.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join(format!(
        "ascript_duplicate_member_{}.as",
        std::process::id()
    ));
    std::fs::write(&file, "class C {\n  x: number\n  x: string\n}\n").unwrap();

    // Default: the diagnostic appears (JSON output for a robust assertion).
    let out = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg(&file)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"code\":\"duplicate-member\""),
        "expected a duplicate-member diagnostic, got:\n{stdout}"
    );
    assert!(
        stdout.contains("duplicate member 'x' in class C"),
        "expected the descriptive message, got:\n{stdout}"
    );

    // `--allow duplicate-member` suppresses it.
    let allowed = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg("--allow")
        .arg("duplicate-member")
        .arg(&file)
        .output()
        .unwrap();
    let allowed_out = String::from_utf8_lossy(&allowed.stdout);
    assert!(
        !allowed_out.contains("\"code\":\"duplicate-member\""),
        "--allow duplicate-member should suppress the diagnostic, got:\n{allowed_out}"
    );

    let _ = std::fs::remove_file(&file);
}

#[test]
fn check_super_misuse_lint_and_allow_suppression() {
    // The `super-misuse` static lint flags `super` used in a class with no
    // superclass (a guaranteed runtime panic). Configurable: `--allow
    // super-misuse` suppresses it.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join(format!("ascript_super_misuse_{}.as", std::process::id()));
    std::fs::write(&file, "class A {\n  fn init() { super.init() }\n}\n").unwrap();

    // Default: the diagnostic appears (JSON output for a robust assertion).
    let out = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg(&file)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"code\":\"super-misuse\""),
        "expected a super-misuse diagnostic, got:\n{stdout}"
    );
    assert!(
        stdout.contains("which has no superclass"),
        "expected the descriptive message, got:\n{stdout}"
    );

    // `--allow super-misuse` suppresses it.
    let allowed = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg("--allow")
        .arg("super-misuse")
        .arg(&file)
        .output()
        .unwrap();
    let allowed_out = String::from_utf8_lossy(&allowed.stdout);
    assert!(
        !allowed_out.contains("\"code\":\"super-misuse\""),
        "--allow super-misuse should suppress the diagnostic, got:\n{allowed_out}"
    );

    let _ = std::fs::remove_file(&file);
}

#[test]
fn check_field_default_type_lint_and_allow_suppression() {
    // The `field-default-type` static lint flags a class field whose literal
    // default contradicts its declared type (a guaranteed runtime panic at
    // construction). Configurable: `--allow field-default-type` suppresses it.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join(format!(
        "ascript_field_default_type_{}.as",
        std::process::id()
    ));
    std::fs::write(&file, "class P { n: number = \"x\" }\n").unwrap();

    // Default: the diagnostic appears (JSON output for a robust assertion).
    let out = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg(&file)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"code\":\"field-default-type\""),
        "expected a field-default-type diagnostic, got:\n{stdout}"
    );
    assert!(
        stdout.contains("field 'n' default is string"),
        "expected the descriptive message, got:\n{stdout}"
    );

    // `--allow field-default-type` suppresses it.
    let allowed = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg("--allow")
        .arg("field-default-type")
        .arg(&file)
        .output()
        .unwrap();
    let allowed_out = String::from_utf8_lossy(&allowed.stdout);
    assert!(
        !allowed_out.contains("\"code\":\"field-default-type\""),
        "--allow field-default-type should suppress the diagnostic, got:\n{allowed_out}"
    );

    let _ = std::fs::remove_file(&file);
}

#[test]
fn check_invalid_propagate_lint_and_allow_suppression() {
    // The `invalid-propagate` static lint flags a postfix `?` inside a function
    // whose declared return type is not a `Result`. Configurable: `--allow
    // invalid-propagate` suppresses it.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join(format!(
        "ascript_invalid_propagate_{}.as",
        std::process::id()
    ));
    std::fs::write(
        &file,
        "fn g(): Result<number> { return [1, nil] }\nfn f(): number { return g()? }\n",
    )
    .unwrap();

    let out = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg(&file)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"code\":\"invalid-propagate\""),
        "expected an invalid-propagate diagnostic, got:\n{stdout}"
    );

    let allowed = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg("--allow")
        .arg("invalid-propagate")
        .arg(&file)
        .output()
        .unwrap();
    let allowed_out = String::from_utf8_lossy(&allowed.stdout);
    assert!(
        !allowed_out.contains("\"code\":\"invalid-propagate\""),
        "--allow invalid-propagate should suppress the diagnostic, got:\n{allowed_out}"
    );

    let _ = std::fs::remove_file(&file);
}

#[test]
fn check_unresolved_import_lint_and_allow_suppression() {
    // The `unresolved-import` static lint flags a `std/*` import whose specifier
    // is not a known std module (here a `std/maths` typo). Configurable:
    // `--allow unresolved-import` suppresses it.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join(format!(
        "ascript_unresolved_import_{}.as",
        std::process::id()
    ));
    std::fs::write(&file, "import { abs } from \"std/maths\"\nprint(1)\n").unwrap();

    let out = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg(&file)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"code\":\"unresolved-import\""),
        "expected an unresolved-import diagnostic, got:\n{stdout}"
    );

    let allowed = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg("--allow")
        .arg("unresolved-import")
        .arg(&file)
        .output()
        .unwrap();
    let allowed_out = String::from_utf8_lossy(&allowed.stdout);
    assert!(
        !allowed_out.contains("\"code\":\"unresolved-import\""),
        "--allow unresolved-import should suppress the diagnostic, got:\n{allowed_out}"
    );

    let _ = std::fs::remove_file(&file);
}

#[test]
fn check_fix_removes_unused_import_and_exits_zero() {
    // `--fix` removes an unused import in place and (since that was the only
    // issue) exits 0; the rest of the file is intact.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = std::env::temp_dir().join(format!("ascript_fix_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("f.as");
    std::fs::write(&file, "import { abs } from \"std/math\"\nprint(1)\n").unwrap();

    let out = Command::new(bin)
        .arg("check")
        .arg("--fix")
        .arg(&file)
        .output()
        .unwrap();
    assert!(out.status.success(), "expected exit 0, got {:?}", out.status);
    let after = std::fs::read_to_string(&file).unwrap();
    assert_eq!(after, "print(1)\n", "file after --fix: {after:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_fix_multi_name_import_keeps_used_clause() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = std::env::temp_dir().join(format!("ascript_fix_multi_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("g.as");
    std::fs::write(&file, "import { abs, max } from \"std/math\"\nprint(max(1, 2))\n").unwrap();

    let out = Command::new(bin)
        .arg("check")
        .arg("--fix")
        .arg(&file)
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let after = std::fs::read_to_string(&file).unwrap();
    assert_eq!(
        after, "import { max } from \"std/math\"\nprint(max(1, 2))\n",
        "file after --fix: {after:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_fix_dry_run_prints_diff_and_leaves_file_unchanged() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = std::env::temp_dir().join(format!("ascript_fix_dry_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("h.as");
    let original = "import { abs } from \"std/math\"\nprint(1)\n";
    std::fs::write(&file, original).unwrap();

    let out = Command::new(bin)
        .arg("check")
        .arg("--fix-dry-run")
        .arg(&file)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--- a/"), "expected a diff header, got:\n{stdout}");
    assert!(
        stdout.lines().any(|l| l.starts_with('-') && l.contains("import")),
        "expected a removed import line in the diff, got:\n{stdout}"
    );
    // The file is byte-identical (dry-run never writes).
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_fix_is_idempotent() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = std::env::temp_dir().join(format!("ascript_fix_idem_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("i.as");
    std::fs::write(&file, "import { abs } from \"std/math\"\nprint(1)\n").unwrap();

    let run = || {
        Command::new(bin)
            .arg("check")
            .arg("--fix")
            .arg(&file)
            .output()
            .unwrap();
    };
    run();
    let after_one = std::fs::read_to_string(&file).unwrap();
    run();
    let after_two = std::fs::read_to_string(&file).unwrap();
    assert_eq!(after_one, after_two, "second --fix changed the file");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_fix_and_fix_dry_run_together_is_usage_error() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = std::env::temp_dir().join(format!("ascript_fix_both_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("j.as");
    std::fs::write(&file, "print(1)\n").unwrap();

    let out = Command::new(bin)
        .arg("check")
        .arg("--fix")
        .arg("--fix-dry-run")
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "expected usage exit 2");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_module_panic_renders_caret_in_defining_module() {
    // SP4 §3: a runtime panic raised in module A (defining `boom`) but invoked
    // from module B must render its caret IN A's file (a.as), not B's — the span
    // belongs to A and is bound to A's source at raise time.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = std::env::temp_dir().join(format!("ascript_prov_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("a.as"),
        "export fn boom() {\n  let x = nil\n  return x.field\n}\n",
    )
    .unwrap();
    let b = dir.join("b.as");
    std::fs::write(&b, "import { boom } from \"./a\"\nprint(\"before\")\nboom()\n").unwrap();

    let out = Command::new(bin).arg("run").arg(&b).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The report must name a.as (the span's own module), NOT b.as.
    assert!(
        stderr.contains("a.as:3"),
        "expected the caret in a.as line 3, got stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("b.as:"),
        "the caret must not be rendered against b.as, got stderr:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn single_module_panic_provenance_unchanged() {
    // SP4 §3 regression: a panic in a STANDALONE file still renders its caret in
    // that file (the span-source fix is additive — single-module errors set the
    // span source to the same file, so the caret is byte-identical to before).
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = std::env::temp_dir().join(format!("ascript_single_prov_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("solo.as");
    std::fs::write(&f, "let x = nil\nprint(x.field)\n").unwrap();

    let out = Command::new(bin).arg("run").arg(&f).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("solo.as:2"),
        "expected the caret in solo.as line 2, got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("cannot read property 'field' of nil"),
        "expected the property panic message, got stderr:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Workers Spec A (Task 8): isolate pool + worker fn dispatch over both engines.
// ---------------------------------------------------------------------------

/// Write `src` to a unique temp `.as` file and run it via `ascript run`, optionally
/// on the tree-walker and/or with extra env vars. Returns trimmed stdout; asserts the
/// process succeeded.
fn run_worker_program(src: &str, tree_walker: bool, env: &[(&str, &str)]) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let bin = env!("CARGO_BIN_EXE_ascript");
    let tag = if tree_walker { "tw" } else { "vm" };
    // A process-unique, monotonically-increasing counter makes the temp path
    // collision-proof across PARALLEL test threads (a nanosecond timestamp can
    // collide, letting one test remove a file another's subprocess is still reading).
    let file = std::env::temp_dir().join(format!(
        "ascript_worker_{}_{}_{}.as",
        tag,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::write(&file, src).unwrap();
    let mut cmd = Command::new(bin);
    cmd.arg("run");
    if tree_walker {
        cmd.arg("--tree-walker");
    }
    cmd.arg(&file);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        out.status.success(),
        "process failed ({tag}): stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn worker_parallel_map_runs() {
    let src = r#"
        import * as array from "std/array"
        import { gather } from "std/task"
        worker fn sq(n) { return n * n }
        let fs = array.map([1, 2, 3, 4], sq)
        let r = await gather(fs)
        print(r)
    "#;
    // Both engines must agree (byte-identical behavior).
    assert_eq!(run_worker_program(src, false, &[]), "[1, 4, 9, 16]");
    assert_eq!(run_worker_program(src, true, &[]), "[1, 4, 9, 16]");
}

#[test]
fn no_worker_program_starts_no_pool() {
    // A program with zero worker fns must still run normally (lazy-pool: the unit
    // proof lives in src/worker/pool.rs; this confirms no regression).
    let src = "print(1 + 1)";
    assert_eq!(run_worker_program(src, false, &[]), "2");
    assert_eq!(run_worker_program(src, true, &[]), "2");
}

#[test]
fn worker_single_await_returns_value() {
    let src = r#"
        worker fn sq(n) { return n * n }
        print(await sq(9))
    "#;
    assert_eq!(run_worker_program(src, false, &[]), "81");
    assert_eq!(run_worker_program(src, true, &[]), "81");
}

#[test]
fn worker_uses_transitive_top_level_deps() {
    // The shipped code slice must include `helper` and const `K` that `sq` uses.
    let src = r#"
        const K = 100
        fn helper(x) { return x + K }
        worker fn sq(n) { return helper(n * n) }
        print(await sq(3))
    "#;
    assert_eq!(run_worker_program(src, false, &[]), "109");
    assert_eq!(run_worker_program(src, true, &[]), "109");
}

#[test]
fn oversubscription_completes_via_queue() {
    // More calls than the pool cap; all must complete (queues drain).
    let src = r#"
        import * as array from "std/array"
        import * as math from "std/math"
        import { gather } from "std/task"
        worker fn sq(n) { return n * n }
        let nums = []
        for n in 1..=20 { array.push(nums, n) }
        let fs = array.map(nums, sq)
        print(math.sum(await gather(fs)))
    "#;
    // 1^2 + .. + 20^2 = 2870
    assert_eq!(
        run_worker_program(src, false, &[("ASCRIPT_WORKERS", "2")]),
        "2870"
    );
}

#[test]
fn nested_worker_runs_inline_no_deadlock() {
    let src = r#"
        worker fn inner(n) { return n + 1 }
        worker fn outer(n) { return await inner(n) * 2 }
        print(await outer(10))
    "#;
    // (10+1)*2 = 22, no deadlock even at pool size 1 (nested runs inline).
    assert_eq!(
        run_worker_program(src, false, &[("ASCRIPT_WORKERS", "1")]),
        "22"
    );
}

#[test]
fn static_worker_method_parallel_map_runs() {
    // Spec A `static worker fn`: the static method body is shipped as a standalone
    // entry fn (no `self`) + its transitive top-level deps. Mirrors
    // worker_parallel_map_runs. ASCRIPT_WORKERS=2 keeps virtual-memory pressure low.
    let src = r#"
        import * as array from "std/array"
        import { gather } from "std/task"
        class Img { static worker fn sq(n) { return n * n } }
        let fs = array.map([1, 2, 3, 4], Img.sq)
        print(await gather(fs))
    "#;
    assert_eq!(
        run_worker_program(src, false, &[("ASCRIPT_WORKERS", "2")]),
        "[1, 4, 9, 16]"
    );
    assert_eq!(
        run_worker_program(src, true, &[("ASCRIPT_WORKERS", "2")]),
        "[1, 4, 9, 16]"
    );
}

// Workers Spec A (Task 9): cancel-on-drop, panic propagation, result pairs, sendability.
// ---------------------------------------------------------------------------

#[test]
fn worker_panic_is_recoverable_on_caller() {
    // A worker fn that panic()s must produce a recoverable error on the caller,
    // catchable via recover(). The result is an error pair [nil, err].
    let src = r#"
        worker fn boom(n) { panic("kaboom " + n) }
        let r = recover(() => await boom(7))
        print(r[1] != nil)
    "#;
    assert_eq!(run_worker_program(src, false, &[]), "true");
}

#[test]
fn worker_result_pair_crosses_as_data() {
    // A worker fn that returns [value, nil] ships the pair as ordinary data
    // through WorkerReply::Ok; `?` propagation works on the awaited result.
    // Uses the builtin `len` function (not a method — s.len() doesn't exist).
    let src = r#"
        worker fn parse(s) { return [len(s), nil] }
        let r = await parse("abcd")?
        print(r)
    "#;
    assert_eq!(run_worker_program(src, false, &[]), "4");
}

#[test]
fn sendability_violation_reports_field_path() {
    // Passing a non-sendable value (a function) nested in an object must produce
    // a recoverable panic whose message contains the field path.
    let src = r#"
        worker fn f(o) { return 1 }
        let cb = () => 1
        let obj = {cb: cb}
        let r = recover(() => await f(obj))
        print(r[1].message)
    "#;
    let out = run_worker_program(src, false, &[]);
    assert!(
        out.contains("cannot be sent to a worker at"),
        "expected sendability message, got: {out}"
    );
}
