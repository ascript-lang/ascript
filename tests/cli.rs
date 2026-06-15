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

/// DX D4 §5.1 multi-error reporting: a file with MULTIPLE parse errors renders
/// ALL of them at once (the error-tolerant CST parser records every one), not just
/// the first. Both malformed `let` statements must appear in stderr.
#[test]
fn run_reports_all_parse_errors_at_once() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    // Two distinct malformed statements — each records its own parse error.
    let file = std::env::temp_dir().join(format!("ascript_multierr_{}.as", std::process::id()));
    std::fs::write(&file, "let = 1\nlet = 2\n").unwrap();
    let out = Command::new(bin).arg("run").arg(&file).output().unwrap();
    assert!(
        !out.status.success(),
        "expected a parse failure, but the run succeeded"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    // Both error sites are reported. ariadne prints one report per error, each with
    // an "Error:" header and a `path:line:col` location gutter. With two reports the
    // header appears at least twice and BOTH source lines (`:1:` and `:2:`) are cited.
    let report_count = err.matches("Error:").count();
    assert!(
        report_count >= 2,
        "expected >=2 error reports, got {report_count}:\n{err}"
    );
    // ariadne colorizes the source text char-by-char, so match on the un-colorized
    // location gutter (`…multierr_<pid>.as:1:` / `:2:`) instead of the raw line text.
    assert!(err.contains(".as:1:"), "missing first error location:\n{err}");
    assert!(err.contains(".as:2:"), "missing second error location:\n{err}");
}

#[test]
fn nested_run_in_worker_with_caps_is_refused() {
    // FFI Unit-B review (NB#2): a `run_in_worker({caps})` called from INSIDE a worker
    // cannot honor the cap reduction (an inline nested run shares the enclosing isolate's
    // Interp, so there is no separate cap boundary). Silently ignoring an explicit
    // `{caps}` would be a security footgun — it must be REFUSED loudly.
    // The refusal is a LOUD Tier-2 panic (a programmer error — an unsupported API
    // combination), so the program FAILS with the message rather than silently degrading.
    let file = std::env::temp_dir().join("ascript_nested_caps_refusal.as");
    std::fs::write(
        &file,
        "import * as task from \"std/task\"\n\
         worker fn inner(x) { return x * 2 }\n\
         worker fn outer(x) {\n  \
           return run_in_worker(inner, x, {caps: {deny: [\"ffi\"]}})\n\
         }\n\
         fn main() {\n  let rs = await task.gather([outer(5)])\n  print(rs[0])\n}\n\
         await main()\n",
    )
    .unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin).arg("run").arg(&file).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.status.success() && combined.contains("not supported"),
        "nested run_in_worker with caps must FAIL loudly (not silently ignore the caps); \
         exit={:?} output: {combined}",
        output.status.code()
    );
    let _ = std::fs::remove_file(&file);
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

/// An anonymous `fn(){...}` EXPRESSION is not a language feature (the spec's
/// only anonymous function is the arrow `() => ...`). It must be rejected as a
/// clean SYNTAX error on BOTH engines — never the legacy "unexpected token Fn"
/// on the tree-walker while the VM emits a confusing internal
/// "compiler bug" message (the regression this guards). The arrow equivalent
/// must keep working.
#[test]
fn anon_fn_expression_is_a_syntax_error_on_both_engines() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    // `fn(){...}` in several expression positions: direct call argument, RHS of
    // a `let`, nested call arg, and an immediately-invoked form.
    let cases = [
        "let r = recover(fn() { return 5 })\nprint(r[0])\n",
        "let f = fn() { return 5 }\nprint(f())\n",
        "let xs = array.map([1, 2, 3], fn(x) { return x * 2 })\nprint(xs)\n",
        "let v = (fn() { return 7 })()\nprint(v)\n",
    ];
    for src in cases {
        let file = std::env::temp_dir().join(format!(
            "ascript_anonfn_{}_{}.as",
            std::process::id(),
            src.len()
        ));
        std::fs::write(&file, src).unwrap();
        for engine_args in [vec!["run"], vec!["run", "--tree-walker"]] {
            let out = Command::new(bin)
                .args(&engine_args)
                .arg(&file)
                .output()
                .unwrap();
            assert!(
                !out.status.success(),
                "expected `{src}` to FAIL on {engine_args:?}, but it succeeded"
            );
            let err = String::from_utf8_lossy(&out.stderr);
            // The defining regression: the VM must NOT surface the internal
            // "compiler bug" message for this user-syntax mistake.
            assert!(
                !err.contains("compiler bug"),
                "`{src}` on {engine_args:?} leaked an internal compiler-bug error:\n{err}"
            );
        }
    }
}

/// The arrow equivalent (`() => ...`) — the real anonymous-function syntax — must
/// keep working as a direct call argument on BOTH engines (the working path we
/// must not regress).
#[test]
fn arrow_in_call_argument_works_on_both_engines() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let src = "let r = recover(() => 5)\nprint(r[0])\n";
    let file = std::env::temp_dir().join(format!("ascript_arrowarg_{}.as", std::process::id()));
    std::fs::write(&file, src).unwrap();
    for engine_args in [vec!["run"], vec!["run", "--tree-walker"]] {
        let out = Command::new(bin)
            .args(&engine_args)
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "expected `{src}` to succeed on {engine_args:?}: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "5\n");
    }
}

/// Regression: `string.repeat(s, 1/0)` / `string.repeat(s, 1e18)` and
/// `string.padStart(s, 1/0)` used to cast `Inf`/huge to `usize::MAX` and abort
/// the host with `capacity overflow`, bypassing `recover`. They must now be
/// CLEAN, recoverable Tier-2 panics — the subprocess exits 0 (no abort) and
/// `recover` catches the error pair. Running the real binary proves there is no
/// host abort (an abort would make `out.status.success()` false with a non-zero
/// / signal exit). (The reader `.read(n)` sites share the same `want_count`
/// guard but need live OS resources, so they are covered by the stdlib unit
/// tests rather than here.)
#[test]
fn string_count_guards_are_recoverable_not_abort() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let src = r#"
import * as string from "std/string"
// Infinity (1.0/0.0) — would previously abort via `f64::INFINITY as usize`.
let [v1, e1] = recover(() => string.repeat("x", 1.0 / 0.0))
print(e1 != nil)
print(v1 == nil)
// Huge finite count — would previously attempt a 10^18-byte allocation (OOM abort).
let [v2, e2] = recover(() => string.repeat("x", 1e18))
print(e2 != nil)
// padStart width guard.
let [v3, e3] = recover(() => string.padStart("x", 1.0 / 0.0, "-"))
print(e3 != nil)
// A normal repeat still works after the recovered panics.
print(string.repeat("ab", 3))
"#;
    let file = std::env::temp_dir().join(format!("ascript_repeatguard_{}.as", std::process::id()));
    std::fs::write(&file, src).unwrap();
    for engine_args in [vec!["run"], vec!["run", "--tree-walker"]] {
        let out = Command::new(bin)
            .args(&engine_args)
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "expected NO host abort on {engine_args:?}; status {:?}, stderr: {:?}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "true\ntrue\ntrue\ntrue\nababab\n",
            "recover must catch the count-guard panics on {engine_args:?}"
        );
    }
    let _ = std::fs::remove_file(&file);
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
    // NUM §4: `math.pow` returns a float, so the squares print with decimals.
    assert!(out.contains("[1.0, 4.0, 9.0, 16.0]"));
    assert!(out.contains("\"name\", \"age\"")); // object.keys
    assert!(out.contains("cannot parse 'xyz' as a number")); // Result destructuring
    // NUM §4: `convert.parseNumber` yields a float, so `42 + 8` prints "50.0".
    assert!(out.contains("\n50.0\n")); // 42 + 8 after parseNumber + destructure
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
    // date: parse + strftime format, and June 15 + 7 days = the 22nd.
    // NUM §4: date components print as floats (the date module returns floats).
    assert!(out.contains("2021/06/15"));
    assert!(out.contains("\n22.0\n"));
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
fn repl_persists_interface_across_lines_for_instanceof() {
    // IFACE Task 11 (REPL regression): an `interface` defined on one line binds the
    // descriptor on the persistent session scope; a LATER line's `instanceof` against
    // it sees the binding. The brace-delimited body relies on the existing
    // delimiter-depth `is_incomplete` buffering for multi-line entry — here we keep it
    // on one line. Asserted on BOTH engines (byte-identity).
    let session = "interface R { fn read(b): int }\n\
class F { fn read(b): int { return 0 } }\n\
F() instanceof R\n";
    let (vm_out, _) = run_repl_session(session, false);
    assert!(
        vm_out.contains("true"),
        "VM: interface must persist across lines for instanceof; stdout: {vm_out}"
    );
    let (tw_out, _) = run_repl_session(session, true);
    assert!(
        tw_out.contains("true"),
        "tree-walker: interface must persist across lines for instanceof; stdout: {tw_out}"
    );

    // A multi-line interface body (exercises delimiter-depth buffering), then used.
    let multiline = "interface W {\n  fn write(b): int\n}\n\
class G { fn write(b): int { return 1 } }\n\
G() instanceof W\n";
    let (ml_out, _) = run_repl_session(multiline, false);
    assert!(
        ml_out.contains("true"),
        "multi-line interface body must buffer + persist; stdout: {ml_out}"
    );
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
fn repl_adt_payload_enum_persists_construct_match_value() {
    // ADT Task 14: a payload enum declared across lines (the `{ … }` buffers via the
    // delimiter-depth `is_incomplete`) persists in the session scope; construction,
    // a `match` (multi-line, buffered too), and `.value` reflection all work on a
    // LATER, separately-compiled line. Byte-identical on the VM and the tree-walker.
    let session = "\
enum Shape {
  Circle(radius: float),
  Pair(int, int),
  Point,
}
let c = Shape.Circle(2.0)
let p = Shape.Pair(3, 4)
c.radius
p.value
match c {
  Circle(r) => r * 2.0,
  Pair(a, b) => float(a + b),
  Shape.Point => 0.0,
}
c == Shape.Circle(2.0)
";
    let (vm_out, vm_err) = run_repl_session(session, false);
    let (tw_out, tw_err) = run_repl_session(session, true);
    // `c.radius` → 2.0; `p.value` → [3, 4]; match → 4.0; equality → true.
    for (out, err, eng) in [(&vm_out, &vm_err, "VM"), (&tw_out, &tw_err, "TW")] {
        assert!(
            out.contains("2.0"),
            "{eng}: c.radius should print 2.0; out={out:?} err={err:?}"
        );
        assert!(
            out.contains("[3, 4]"),
            "{eng}: p.value should print [3, 4]; out={out:?} err={err:?}"
        );
        assert!(
            out.contains("4.0"),
            "{eng}: match should yield 4.0; out={out:?} err={err:?}"
        );
        assert!(
            out.contains("true"),
            "{eng}: structural equality should print true; out={out:?} err={err:?}"
        );
    }
    // The two engines produce byte-identical session output.
    assert_eq!(vm_out, tw_out, "VM/TW REPL output must match for ADT session");
}

/// An or-pattern alternative that BINDS a name (`Circle(r) | Square(r)`) must make
/// that binding visible in the arm body on the VM — the CST resolver was dropping
/// the bindings inside an `OrPat` (no `OrPat` arm in `resolve_pattern`), so the name
/// fell through to a `Global` fallback and failed at runtime with `undefined
/// variable`. The legacy tree-walker oracle already binds these correctly, so the
/// fix is asserted as VM == tree-walker byte-identity.
#[test]
fn match_or_pattern_binds_names_on_both_engines() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let src = "\
enum Shape {
  Circle(radius: int),
  Square(side: int),
  Empty,
}
let c = Shape.Circle(2)
let out = match c {
  Shape.Circle(r) | Shape.Square(r) => r,
  Shape.Empty => 0,
}
print(out)
";
    let file =
        std::env::temp_dir().join(format!("ascript_orpat_{}.as", std::process::id()));
    std::fs::write(&file, src).unwrap();
    let mut outputs = Vec::new();
    for engine_args in [vec!["run"], vec!["run", "--tree-walker"]] {
        let out = Command::new(bin)
            .args(&engine_args)
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "expected or-pattern binding to succeed on {engine_args:?}: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert_eq!(
            stdout, "2\n",
            "or-pattern binding must print 2 on {engine_args:?}; got {stdout:?}"
        );
        outputs.push(stdout);
    }
    assert_eq!(outputs[0], outputs[1], "VM and tree-walker output must match");
}

/// An or-pattern whose alternatives bind DIFFERENT name sets
/// (`Shape.Circle(r) | Shape.Empty => r` — `r` bound in one alternative, absent in
/// the other) is a STATIC compile error, rejected BYTE-IDENTICALLY before running on
/// the VM and the tree-walker, and by `ascript check` — Rust-style "variable `r` is
/// not bound in all patterns". This resolves the earlier runtime divergence (VM:
/// "expected int, got nil" vs tree-walker: "undefined variable 'r'").
#[test]
fn match_or_pattern_mismatched_names_is_static_error_on_all_paths() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let src = "\
enum Shape { Circle(radius: int), Empty }
fn f(s: Shape): int {
  return match s {
    Shape.Circle(r) | Shape.Empty => r,
  }
}
print(f(Shape.Empty))
";
    let file =
        std::env::temp_dir().join(format!("ascript_orpat_bad_{}.as", std::process::id()));
    std::fs::write(&file, src).unwrap();
    let expected = "variable 'r' is not bound in all alternatives of the or-pattern";
    // Each invocation must FAIL with the SAME message. Strip ANSI so the substring
    // match is robust to ariadne's per-char colorization.
    for args in [
        vec!["run"],
        vec!["run", "--tree-walker"],
        vec!["check"],
    ] {
        let out = Command::new(bin).args(&args).arg(&file).output().unwrap();
        assert!(
            !out.status.success(),
            "expected `{args:?}` to FAIL on the mismatched-name or-pattern, but it succeeded"
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let plain = strip_ansi(&combined);
        assert!(
            plain.contains(expected),
            "`{args:?}` must report {expected:?}; got:\n{plain}"
        );
    }
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

/// Regression (Spec B Task 10): a `worker class` defined across multiple buffered REPL
/// lines (brace-delimited — the `worker` keyword is contextual, so `is_incomplete` only
/// sees `{`/`}` tokens and buffers correctly) must persist in the session and be usable
/// on subsequent REPL inputs: `spawn`, method calls, and `close` all round-trip through
/// the same session-accumulated `worker_source`.  Guards against the actor dispatch not
/// seeing the class definition when it recompiles the source slice in a fresh isolate.
#[test]
fn repl_accepts_multiline_worker_class_and_calls_method() {
    // Lines 1–4 buffer as one input (open `{`…`}`); subsequent lines are separate inputs.
    let input = concat!(
        "worker class Counter {\n",
        "  n: number = 0\n",
        "  fn inc(): number { self.n = self.n + 1; return self.n }\n",
        "}\n",
        "let c = await Counter.spawn()\n",
        "print(await c.inc())\n",
        "print(await c.inc())\n",
        "c.close()\n",
    );
    let (out, err) = run_repl_session(input, false);
    assert!(
        out.contains("1\n2"),
        "expected incremented counter; stdout: {out:?}  stderr: {err:?}"
    );
}

/// Regression (Spec B Task 10): a `worker fn*` streaming generator defined across multiple
/// buffered REPL lines must persist in the session scope and stream values via `for await`
/// on a subsequent REPL input.  Mirrors the `worker fn` regression from Plan A Task 13 but
/// exercises the streaming path.
#[test]
fn repl_accepts_multiline_worker_gen_and_streams_it() {
    // Lines 1–3 buffer as one input (open `{`…`}`); lines 4–5 are separate inputs.
    let input = concat!(
        "worker fn* count(n) {\n",
        "  for (i in 1..=n) { yield i * 10 }\n",
        "}\n",
        "let g = count(3)\n",
        "for await (x in g) { print(x) }\n",
    );
    let (out, err) = run_repl_session(input, false);
    assert!(
        out.contains("10") && out.contains("20") && out.contains("30"),
        "expected 10/20/30 from stream; stdout: {out:?}  stderr: {err:?}"
    );
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

/// DX D2 Task 5 — write a small multi-file test corpus (mixed pass/fail across files) to
/// a fresh temp dir and return the file paths in a stable order.
fn write_parallel_test_corpus(tag: &str) -> (std::path::PathBuf, Vec<std::path::PathBuf>) {
    let dir = std::env::temp_dir().join(format!("ascript_par_{}_{}", tag, std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let files = [
        // a.as — both pass
        (
            "a.as",
            "test(\"a_one\", () => { assert(1 + 1 == 2) })\n\
             test(\"a_two\", () => { assert(true) })\n",
        ),
        // b.as — one fails (with a distinct message), one passes
        (
            "b.as",
            "test(\"b_one\", () => { assert(false, \"b boom\") })\n\
             test(\"b_two\", () => { assert(2 * 2 == 4) })\n",
        ),
        // c.as — a fail, attributing the right name across the boundary
        (
            "c.as",
            "test(\"c_one\", () => { assert(true) })\n\
             test(\"c_two\", () => { assert(false, \"c boom\") })\n",
        ),
    ];
    let mut paths = Vec::new();
    for (name, body) in files {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        paths.push(p);
    }
    (dir, paths)
}

/// THE §7 determinism contract: a multi-file corpus run at `--parallel=1` and
/// `--parallel=4` produces BYTE-IDENTICAL stdout and the SAME exit code, regardless of
/// isolate completion order. (`--parallel=1` degrades to the serial path; the parallel
/// aggregation must reproduce that exact output.)
#[test]
fn parallel_test_dispatch_is_deterministic() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let (dir, paths) = write_parallel_test_corpus("det");
    let args: Vec<&std::ffi::OsStr> = paths.iter().map(|p| p.as_os_str()).collect();

    let run = |parallel_flag: &str| {
        std::process::Command::new(bin)
            .arg("test")
            .arg(parallel_flag)
            .args(&args)
            .output()
            .unwrap()
    };

    let one = run("--parallel=1");
    let four = run("--parallel=4");

    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        String::from_utf8_lossy(&one.stdout),
        String::from_utf8_lossy(&four.stdout),
        "stdout must be byte-identical between --parallel=1 and --parallel=4"
    );
    assert_eq!(
        one.status.code(),
        four.status.code(),
        "exit code must match between --parallel=1 and --parallel=4"
    );
    // Sanity: the run reports a non-zero exit (b.as + c.as each have one failure) and the
    // failures are attributed by name regardless of order.
    let s = String::from_utf8_lossy(&four.stdout);
    assert!(s.contains("b boom"), "missing b failure: {s}");
    assert!(s.contains("c boom"), "missing c failure: {s}");
    assert_eq!(four.status.code(), Some(1), "failures → exit 1");
}

/// A SINGLE file with `--parallel` degrades to the serial path: identical output to a
/// plain serial run (no isolate path taken for one file).
#[test]
fn parallel_single_file_degrades_to_serial() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = std::env::temp_dir().join(format!("ascript_par_single_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("solo.as");
    std::fs::write(
        &file,
        "test(\"ok\", () => { assert(true) })\n\
         test(\"bad\", () => { assert(false, \"solo boom\") })\n",
    )
    .unwrap();

    let serial = std::process::Command::new(bin)
        .arg("test")
        .arg(&file)
        .output()
        .unwrap();
    let parallel = std::process::Command::new(bin)
        .arg("test")
        .arg("--parallel=4")
        .arg(&file)
        .output()
        .unwrap();

    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        String::from_utf8_lossy(&serial.stdout),
        String::from_utf8_lossy(&parallel.stdout),
        "a single file with --parallel must match the serial output exactly"
    );
    assert_eq!(serial.status.code(), parallel.status.code());
}

/// A test file that itself dispatches a `worker fn` runs it without deadlock and gets the
/// correct result under `--parallel` (the file behaves like a normal top-level program in
/// its test isolate — its workers take the pool path, no pool-reservation deadlock).
#[test]
fn parallel_test_file_with_nested_worker_takes_pool_path() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = std::env::temp_dir().join(format!("ascript_par_nested_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let a = dir.join("worker_a.as");
    let b = dir.join("plain_b.as");
    std::fs::write(
        &a,
        "import * as task from \"std/task\"\n\
         worker fn double(x) { return x * 2 }\n\
         test(\"nested worker\", () => {\n  \
           let rs = await task.gather([double(21)])\n  \
           assert(rs[0] == 42)\n\
         })\n",
    )
    .unwrap();
    std::fs::write(&b, "test(\"plain\", () => { assert(true) })\n").unwrap();

    let out = std::process::Command::new(bin)
        .arg("test")
        .arg("--parallel=4")
        .arg(&a)
        .arg(&b)
        .output()
        .unwrap();

    let _ = std::fs::remove_dir_all(&dir);

    let s =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "nested-worker test must pass (ran via the pool path, no deadlock); output: {s}"
    );
    assert!(s.contains("2 passed"), "expected 2 passed; got: {s}");
}

// ---------------------------------------------------------------------------------------
// DX D2 Task 10 — `--filter PATTERN` (substring or `/regex/`) prunes which tests run; a
// skipped test is reported as "filtered", never pass/fail. Composes with `--parallel`
// deterministically; a bad regex is a clean error.
// ---------------------------------------------------------------------------------------

/// Write a small mixed-name test corpus (one file, several tests with distinguishable
/// names) and return its path.
fn write_filter_corpus(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("ascript_filter_{}_{}", tag, std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("suite.as");
    std::fs::write(
        &file,
        "test(\"adds numbers\", () => { assert(1 + 1 == 2) })\n\
         test(\"adds strings\", () => { assert(\"a\" + \"b\" == \"ab\") })\n\
         test(\"subtracts numbers\", () => { assert(3 - 1 == 2) })\n\
         test(\"multiplies numbers\", () => { assert(2 * 3 == 6) })\n",
    )
    .unwrap();
    (dir, file)
}

/// A substring `--filter` runs only the matching tests; the rest are reported as
/// "filtered", not passed/failed.
#[test]
fn filter_substring_prunes_tests_and_reports_filtered_count() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let (dir, file) = write_filter_corpus("sub");
    let out = std::process::Command::new(bin)
        .arg("test")
        .arg("--filter")
        .arg("adds")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    // 2 tests contain "adds"; the other 2 ("subtracts numbers", "multiplies numbers") are
    // filtered. ("subtracts" contains "...tracts", NOT "adds".)
    assert!(
        s.contains("2 passed; 0 failed; 2 filtered"),
        "expected 2 passed / 2 filtered; got: {s}"
    );
    assert!(out.status.success(), "all run tests pass → exit 0; got: {s}");
}

/// A `/regex/` `--filter` matches by regular expression against the test name.
#[test]
fn filter_regex_prunes_tests() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let (dir, file) = write_filter_corpus("re");
    let out = std::process::Command::new(bin)
        .arg("test")
        .arg("--filter")
        .arg("/^adds/") // anchored: names STARTING with "adds"
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    // "adds numbers" + "adds strings" start with "adds"; the other two do not.
    assert!(
        s.contains("2 passed; 0 failed; 2 filtered"),
        "expected 2 passed / 2 filtered for /^adds/; got: {s}"
    );
}

/// A malformed `/regex/` is a CLEAN error (non-zero exit, a readable message), never a
/// panic.
#[test]
fn filter_bad_regex_is_a_clean_error() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let (dir, file) = write_filter_corpus("bad");
    let out = std::process::Command::new(bin)
        .arg("test")
        .arg("--filter")
        .arg("/(unclosed/")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    let s =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a bad regex must exit non-zero");
    assert!(
        s.contains("invalid --filter regex"),
        "expected a clean regex error; got: {s}"
    );
    // No panic / abort.
    assert_ne!(out.status.code(), Some(134), "must not abort/panic: {s}");
}

/// THE §7 contract for filtering: `--filter` + `--parallel=1` and `--filter` + `--parallel=N`
/// over a multi-FILE corpus produce BYTE-IDENTICAL output (the filter is applied identically
/// inside each isolate, independent of completion order).
#[test]
fn filter_with_parallel_is_deterministic() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = std::env::temp_dir().join(format!("ascript_filter_par_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let a = dir.join("a.as");
    let b = dir.join("b.as");
    std::fs::write(
        &a,
        "test(\"keep a1\", () => { assert(true) })\n\
         test(\"drop a2\", () => { assert(true) })\n",
    )
    .unwrap();
    std::fs::write(
        &b,
        "test(\"keep b1\", () => { assert(true) })\n\
         test(\"drop b2\", () => { assert(true) })\n",
    )
    .unwrap();

    let run = |parallel_flag: &str| {
        std::process::Command::new(bin)
            .arg("test")
            .arg(parallel_flag)
            .arg("--filter")
            .arg("keep")
            .arg(&a)
            .arg(&b)
            .output()
            .unwrap()
    };
    let one = run("--parallel=1");
    let four = run("--parallel=4");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        String::from_utf8_lossy(&one.stdout),
        String::from_utf8_lossy(&four.stdout),
        "filtered output must be byte-identical across --parallel=1 and =4"
    );
    let s = String::from_utf8_lossy(&four.stdout);
    // 2 "keep" tests run, 2 "drop" tests filtered.
    assert!(
        s.contains("2 passed; 0 failed; 2 filtered"),
        "expected 2 passed / 2 filtered; got: {s}"
    );
}

/// No `--filter` → the tally is byte-identical to the historical output (no "filtered"
/// clause), so existing consumers are unaffected.
#[test]
fn no_filter_keeps_the_legacy_tally_shape() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let (dir, file) = write_filter_corpus("none");
    let out = std::process::Command::new(bin)
        .arg("test")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(s.contains("4 passed; 0 failed"), "got: {s}");
    assert!(!s.contains("filtered"), "no filtered clause expected; got: {s}");
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

// ─── §3.3 exit() skips defers ─────────────────────────────────────────────────
//
// Per spec §3.3 (the Go `os.Exit` rule): `exit()` is process termination, NOT a
// frame exit — defers registered in the current function / at top-level are NOT
// drained. This is tested via a subprocess for both engines (VM default + tree-walker)
// so we observe real process exit code and captured stdout.

#[test]
fn defer_exit_skips_defers_vm() {
    // Program: register a defer that would print, then call exit(0).
    // If defers ran, "SHOULD_NOT_APPEAR" would be in stdout.
    // Positive control: "REGISTERED" is printed BEFORE exit(), proving the
    // defer was registered before the exit() call — the test is meaningful.
    let src = r#"
fn main() {
    defer print("SHOULD_NOT_APPEAR")
    print("REGISTERED")
    exit(0)
}
main()
"#;
    let path = std::env::temp_dir().join(format!(
        "ascript_defer_exit_vm_{}.as",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = Command::new(bin).arg("run").arg(&path).output().unwrap();
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "VM: expected exit code 0; got {:?}; stdout: {stdout:?}",
        out.status.code()
    );
    assert!(
        stdout.contains("REGISTERED"),
        "VM: positive control 'REGISTERED' must appear (defer was registered before exit); stdout: {stdout:?}"
    );
    assert!(
        !stdout.contains("SHOULD_NOT_APPEAR"),
        "VM: §3.3 exit() must NOT run defers — 'SHOULD_NOT_APPEAR' must be absent; stdout: {stdout:?}"
    );
}

#[test]
fn defer_exit_skips_defers_tree_walker() {
    // Same program as above but run on the tree-walker oracle — §3.3 must hold
    // on BOTH engines.
    let src = r#"
fn main() {
    defer print("SHOULD_NOT_APPEAR")
    print("REGISTERED")
    exit(0)
}
main()
"#;
    let path = std::env::temp_dir().join(format!(
        "ascript_defer_exit_tw_{}.as",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = Command::new(bin)
        .args(["run", "--tree-walker"])
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "TW: expected exit code 0; got {:?}; stdout: {stdout:?}",
        out.status.code()
    );
    assert!(
        stdout.contains("REGISTERED"),
        "TW: positive control 'REGISTERED' must appear (defer was registered before exit); stdout: {stdout:?}"
    );
    assert!(
        !stdout.contains("SHOULD_NOT_APPEAR"),
        "TW: §3.3 exit() must NOT run defers — 'SHOULD_NOT_APPEAR' must be absent; stdout: {stdout:?}"
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
/// the process fails (so callers assert on successful output). General-purpose
/// "run source on both engines and compare" helper — the `tag` namespaces the
/// temp file, so callers outside the RANGES section pass their own prefix.
fn run_range_src(src: &str, tree_walker: bool, tag: &str) -> String {
    let file = std::env::temp_dir().join(format!("ascript_{tag}.as"));
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
fn negative_integer_enum_backing_both_engines() {
    // NUM-split regression: a NEGATIVE INTEGER enum backing value (`A = -1`) must
    // compile + run byte-identically on the default VM and the tree-walker. Before
    // the fix, `const_eval_enum_backing`'s unary-minus arm only handled
    // `Value::float`, so an integer literal (now `Value::int`) fell through to
    // "enum variant backing value must be a number or string literal" — the VM
    // rejected legal code the tree-walker accepted.
    let cases = [
        (
            "enum E { A = -1, B = 2 }\nprint(E.A.value)\nprint(E.B.value)",
            "-1\n2\n",
        ),
        (
            // Negative float backing still works (the original arm).
            "enum F { Lo = -2.5, Hi = 3 }\nprint(F.Lo.value)",
            "-2.5\n",
        ),
        (
            // A large in-range negative int literal.
            "enum G { Min = -9223372036854775807 }\nprint(G.Min.value)",
            "-9223372036854775807\n",
        ),
    ];
    for (i, (src, expected)) in cases.iter().enumerate() {
        // `run_range_src` is a general-purpose "run on both engines" helper (it
        // lives under the RANGES section but is engine-agnostic).
        let vm = run_range_src(src, false, &format!("neg_enum_vm_{i}"));
        let tw = run_range_src(src, true, &format!("neg_enum_tw_{i}"));
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
        ("print(0..=1 step 0.25)", "[0.0, 0.25, 0.5, 0.75, 1.0]\n"),
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
            "step -2.0 moves away from end (10.0); range can never progress",
        ),
        (
            "for (i in 10..1 step 2) {}",
            "step 2.0 moves away from end (1.0); range can never progress",
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
            "step -2.0 moves away from end (10.0); range can never progress",
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
            "step -2.0 moves away from end (10.0); range can never progress",
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
    // A class field whose literal default contradicts its declared type is a
    // guaranteed runtime panic at construction. Since TYPE this is a BLOCKING
    // `type-mismatch` Error (the typed field default is an annotated slot — the
    // legacy `field-default-type` advisory is SUBSUMED by the sound checker).
    // Configurable: `--allow type-mismatch` suppresses it.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let file = std::env::temp_dir().join(format!(
        "ascript_field_default_type_{}.as",
        std::process::id()
    ));
    std::fs::write(&file, "class P { n: number = \"x\" }\n").unwrap();

    // Default: the diagnostic appears as a blocking `type-mismatch` Error.
    let out = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg(&file)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"code\":\"type-mismatch\""),
        "expected a blocking type-mismatch diagnostic, got:\n{stdout}"
    );
    assert!(
        stdout.contains("\"severity\":\"error\""),
        "the annotated field-default mismatch must be a blocking Error, got:\n{stdout}"
    );
    assert!(
        stdout.contains("field 'n' default is"),
        "expected the descriptive message, got:\n{stdout}"
    );
    // The legacy advisory is subsumed (dropped at this span).
    assert!(
        !stdout.contains("\"code\":\"field-default-type\""),
        "the legacy field-default-type advisory should be subsumed, got:\n{stdout}"
    );

    // `--allow type-mismatch` suppresses it.
    let allowed = Command::new(bin)
        .arg("check")
        .arg("--json")
        .arg("--allow")
        .arg("type-mismatch")
        .arg(&file)
        .output()
        .unwrap();
    let allowed_out = String::from_utf8_lossy(&allowed.stdout);
    assert!(
        !allowed_out.contains("\"code\":\"type-mismatch\""),
        "--allow type-mismatch should suppress the diagnostic, got:\n{allowed_out}"
    );
    // INTENTIONAL (review-locked): `type-mismatch` subsumes the legacy advisory, so
    // `--allow type-mismatch` silences the WHOLE mistake class — the legacy
    // `field-default-type` Warning must NOT resurface (subsumption runs before the
    // allow filter; see analyze.rs). A future reordering that resurfaced it would be a
    // DX regression this assertion catches.
    assert!(
        !allowed_out.contains("\"code\":\"field-default-type\""),
        "--allow type-mismatch must not resurface the subsumed legacy advisory, got:\n{allowed_out}"
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
        for (n in 1..=20) { array.push(nums, n) }
        let fs = array.map(nums, sq)
        print(math.sum(await gather(fs)))
    "#;
    // 1^2 + .. + 20^2 = 2870 (math.sum returns a float → "2870.0")
    assert_eq!(
        run_worker_program(src, false, &[("ASCRIPT_WORKERS", "2")]),
        "2870.0"
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

// ---------------------------------------------------------------------------
// Spec A: `worker async fn` and `worker fn*` are invalid modifier combos
// and must be rejected (not silently dropped) on BOTH engines.
// ---------------------------------------------------------------------------

/// Run `src` on the given engine and return (success, combined output).
/// Used to check programs that must FAIL.
fn run_worker_program_raw(src: &str, tree_walker: bool) -> (bool, String) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ2: AtomicU64 = AtomicU64::new(0);
    let bin = env!("CARGO_BIN_EXE_ascript");
    let tag = if tree_walker { "tw" } else { "vm" };
    let file = std::env::temp_dir().join(format!(
        "ascript_worker_invalid_{}_{}_{}.as",
        tag,
        std::process::id(),
        SEQ2.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::write(&file, src).unwrap();
    let mut cmd = Command::new(bin);
    cmd.arg("run");
    if tree_walker {
        cmd.arg("--tree-walker");
    }
    cmd.arg(&file);
    let out = cmd.output().unwrap();
    let _ = std::fs::remove_file(&file);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

#[test]
fn worker_async_fn_is_rejected_on_both_engines() {
    // `worker async fn` must fail on BOTH engines with the expected message.
    // The parser accepts both flags (permissive parsing); the semantic layer rejects.
    let src = "worker async fn g() { return 2 }\nprint(await g())\n";
    let expected = "worker functions cannot be async";

    let (ok_vm, out_vm) = run_worker_program_raw(src, false);
    assert!(
        !ok_vm,
        "VM must reject `worker async fn` but it succeeded; output: {out_vm}"
    );
    assert!(
        out_vm.contains(expected),
        "VM error must contain expected message; output: {out_vm}"
    );

    let (ok_tw, out_tw) = run_worker_program_raw(src, true);
    assert!(
        !ok_tw,
        "tree-walker must reject `worker async fn` but it succeeded; output: {out_tw}"
    );
    assert!(
        out_tw.contains(expected),
        "tree-walker error must contain expected message; output: {out_tw}"
    );
}

#[test]
fn worker_generator_fn_streams_on_both_engines() {
    // Spec B Task 6: `worker fn*` is a VALID streaming generator running in a
    // dedicated isolate. It yields its ordered sequence transparently via `for await`,
    // byte-identically on BOTH engines.
    let src = "worker fn* records(n) { for (i in 1..=n) { yield i * 10 } }\n\
               async fn main() { for await (r in records(3)) { print(r) } }\n\
               await main()\n";

    let (ok_vm, out_vm) = run_worker_program_raw(src, false);
    assert!(ok_vm, "VM must stream `worker fn*`; output: {out_vm}");
    assert_eq!(out_vm, "10\n20\n30\n", "VM streamed sequence");

    let (ok_tw, out_tw) = run_worker_program_raw(src, true);
    assert!(
        ok_tw,
        "tree-walker must stream `worker fn*`; output: {out_tw}"
    );
    assert_eq!(out_tw, "10\n20\n30\n", "tree-walker streamed sequence");
}

// ─────────────────────────── FFI capability CLI flags ────────────────────────
// FFI §4.2/§4.5: `--deny`/`--sandbox` opt-out flags, the `[capabilities]` manifest
// table, and their composition. Hermetic (no network — the denied call is caught
// by `recover`, and ambient `os.platform()` is asserted to still work).

fn run_with_args(src: &str, name: &str, extra: &[&str]) -> (bool, String, String) {
    let file = std::env::temp_dir().join(name);
    std::fs::write(&file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let mut cmd = Command::new(bin);
    cmd.arg("run");
    for a in extra {
        cmd.arg(a);
    }
    cmd.arg(&file);
    let output = cmd.output().unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

#[cfg(feature = "sys")] // program imports std/env; only valid with the sys feature.
#[test]
fn deny_flag_denies_env_capability_recoverably() {
    // `--deny env` → env.get raises `capability 'env' denied`, caught by recover.
    let src = "import * as env from \"std/env\"\n\
               let r = recover(() => env.get(\"PATH\"))\n\
               print(r[0])\n\
               print(r[1].message)\n";
    let (ok, out, err) = run_with_args(src, "caps_deny_env.as", &["--deny", "env"]);
    assert!(ok, "program should run (denial is recoverable); stderr: {err}");
    assert_eq!(out, "nil\ncapability 'env' denied\n");
}

#[test]
fn deny_flag_comma_list_denies_multiple() {
    // `--deny ffi,process` denies both; `caps.has` reflects it.
    let src = "import * as caps from \"std/caps\"\n\
               print(caps.has(\"ffi\"))\n\
               print(caps.has(\"process\"))\n\
               print(caps.has(\"net\"))\n";
    let (ok, out, err) = run_with_args(src, "caps_deny_multi.as", &["--deny", "ffi,process"]);
    assert!(ok, "stderr: {err}");
    assert_eq!(out, "false\nfalse\ntrue\n");
}

#[cfg(feature = "sys")] // program imports std/os; only valid with the sys feature.
#[test]
fn sandbox_flag_denies_all_but_keeps_ambient_os() {
    // `--sandbox` denies all five, but ambient os introspection still works.
    let src = "import * as caps from \"std/caps\"\n\
               import * as os from \"std/os\"\n\
               print(caps.list())\n\
               print(type(os.platform()))\n";
    let (ok, out, err) = run_with_args(src, "caps_sandbox.as", &["--sandbox"]);
    assert!(ok, "stderr: {err}");
    // Every dangerous cap denied → empty list; os.platform (ambient) still works.
    assert_eq!(out, "[]\nstring\n");
}

#[test]
fn unknown_deny_cap_is_clean_cli_error() {
    // A bogus cap name to `--deny` is a clean error (non-zero exit, no panic).
    let src = "print(1)\n";
    let (ok, _out, err) = run_with_args(src, "caps_bad.as", &["--deny", "bogus"]);
    assert!(!ok, "unknown cap should fail the CLI");
    assert!(
        err.contains("unknown capability") && err.contains("bogus"),
        "stderr: {err}"
    );
}

#[test]
fn no_deny_flags_is_byte_identical_default() {
    // No flags → all granted → existing behavior unchanged.
    let src = "import * as caps from \"std/caps\"\nprint(caps.list())\n";
    let (ok, out, err) = run_with_args(src, "caps_default.as", &[]);
    assert!(ok, "stderr: {err}");
    assert_eq!(out, "[\"fs\", \"net\", \"process\", \"ffi\", \"env\"]\n");
}

// Manifest [capabilities] composition only happens under the `pkg` feature
// (manifest loading is pkg-side). Caps deny is core, but the manifest floor isn't.
#[cfg(feature = "pkg")]
#[test]
fn manifest_capabilities_table_denies_and_composes_with_cli() {
    // A project dir with an [capabilities] manifest denying ffi; the CLI adds env.
    let dir = std::env::temp_dir().join("ascript_caps_manifest_test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ascript.toml"),
        "[capabilities]\ndeny = [\"ffi\"]\n",
    )
    .unwrap();
    let prog = dir.join("app.as");
    std::fs::write(
        &prog,
        "import * as caps from \"std/caps\"\n\
         print(caps.has(\"ffi\"))\n\
         print(caps.has(\"env\"))\n\
         print(caps.has(\"net\"))\n",
    )
    .unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    // Manifest denies ffi; CLI denies env; union → both gone, net remains.
    let output = Command::new(bin)
        .arg("run")
        .arg("--deny")
        .arg("env")
        .arg(&prog)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(out, "false\nfalse\ntrue\n", "manifest+CLI denials union");
    std::fs::remove_dir_all(&dir).ok();
}

// ── DX D1: `ascript doc` golden + smoke tests ──────────────────────────────

/// A pinned Markdown golden over a documented module: signatures + `///` bodies +
/// member rendering. `ascript doc --format md <file>` streams Markdown to stdout.
#[test]
fn doc_markdown_golden() {
    let file = std::env::temp_dir().join("ascript_doc_golden.as");
    std::fs::write(
        &file,
        "//! A tiny calculator.\n\
         \n\
         /// Adds two numbers.\n\
         export fn add(a: number, b: number): number { return a + b }\n\
         \n\
         /// A 2D point.\n\
         export class Point {\n  \
           /// the x coordinate\n  \
           x: number = 0\n  \
           fn init(x) { self.x = x }\n\
         }\n\
         \n\
         /// The shapes.\n\
         export enum Shape { Circle(r: float), Square }\n\
         \n\
         fn hidden() { return 1 }\n",
    )
    .unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("doc")
        .arg("--format")
        .arg("md")
        .arg(&file)
        .output()
        .unwrap();
    assert!(output.status.success(), "doc failed: {:?}", output);
    let md = String::from_utf8_lossy(&output.stdout);

    // Module heading + module doc.
    assert!(md.contains("# ascript_doc_golden"), "module heading: {md}");
    assert!(md.contains("A tiny calculator."), "module doc: {md}");
    // Function signature + doc.
    assert!(md.contains("## fn `add`"), "fn heading: {md}");
    assert!(
        md.contains("fn add(a: number, b: number): number"),
        "fn sig: {md}"
    );
    assert!(md.contains("Adds two numbers."), "fn doc: {md}");
    // Class + field + method.
    assert!(md.contains("## class `Point`"), "class heading: {md}");
    assert!(md.contains("`x: number = 0`"), "field sig: {md}");
    assert!(md.contains("the x coordinate"), "field doc: {md}");
    assert!(md.contains("#### method `init`"), "method heading: {md}");
    // Enum variants.
    assert!(md.contains("## enum `Shape`"), "enum heading: {md}");
    assert!(md.contains("Circle(r: float)"), "variant sig: {md}");
    // Private `hidden` is NOT in the public output.
    assert!(!md.contains("hidden"), "private fn must be excluded: {md}");

    std::fs::remove_file(&file).ok();
}

/// `--private` includes non-exported declarations.
#[test]
fn doc_private_flag_includes_unexported() {
    let file = std::env::temp_dir().join("ascript_doc_private.as");
    std::fs::write(
        &file,
        "export fn pub_one() {}\nfn priv_one() {}\n",
    )
    .unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = Command::new(bin)
        .arg("doc")
        .arg("--format")
        .arg("md")
        .arg("--private")
        .arg(&file)
        .output()
        .unwrap();
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(md.contains("priv_one"), "--private must include unexported: {md}");
    std::fs::remove_file(&file).ok();
}

/// `--format html` writes a self-contained tree: `index.html`, `style.css`, one
/// page per module. Check the index structure links the module page.
#[test]
fn doc_html_index_structure() {
    let file = std::env::temp_dir().join("ascript_doc_html.as");
    std::fs::write(
        &file,
        "//! HTML module.\n/// A function.\nexport fn f() {}\n",
    )
    .unwrap();
    let dir = std::env::temp_dir().join("ascript_doc_html_out");
    std::fs::remove_dir_all(&dir).ok();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let status = Command::new(bin)
        .arg("doc")
        .arg("--format")
        .arg("html")
        .arg("--out")
        .arg(&dir)
        .arg(&file)
        .output()
        .unwrap();
    assert!(status.status.success(), "doc html failed: {:?}", status);

    let index = std::fs::read_to_string(dir.join("index.html")).unwrap();
    assert!(index.contains("<title>API documentation</title>"), "index title");
    assert!(
        index.contains("href=\"ascript_doc_html.html\""),
        "index links module page: {index}"
    );
    assert!(
        index.contains("<link rel=\"stylesheet\" href=\"style.css\">"),
        "self-contained stylesheet link"
    );
    // The stylesheet exists (self-contained tree).
    assert!(dir.join("style.css").exists(), "style.css written");
    // The module page exists with the item.
    let page = std::fs::read_to_string(dir.join("ascript_doc_html.html")).unwrap();
    assert!(page.contains("fn f()"), "module page has the fn: {page}");

    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_file(&file).ok();
}

/// `--check` exits non-zero on a deliberately-undocumented public symbol and zero
/// when all public symbols are documented.
#[test]
fn doc_check_exits_nonzero_on_undocumented() {
    let bin = env!("CARGO_BIN_EXE_ascript");

    // Undocumented public `bad` → non-zero, reports it.
    let undoc = std::env::temp_dir().join("ascript_doc_check_undoc.as");
    std::fs::write(&undoc, "/// ok\nexport fn good() {}\nexport fn bad() {}\n").unwrap();
    let out = Command::new(bin).arg("doc").arg("--check").arg(&undoc).output().unwrap();
    assert!(!out.status.success(), "must fail on undocumented symbol");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("bad"), "reports the undocumented symbol: {combined}");

    // Fully documented → zero.
    let doc = std::env::temp_dir().join("ascript_doc_check_ok.as");
    std::fs::write(&doc, "/// a\nexport fn a() {}\n/// b\nexport fn b() {}\n").unwrap();
    let out = Command::new(bin).arg("doc").arg("--check").arg(&doc).output().unwrap();
    assert!(out.status.success(), "must pass when all documented: {:?}", out);

    std::fs::remove_file(&undoc).ok();
    std::fs::remove_file(&doc).ok();
}

/// Review finding 1: two `.as` files with the same STEM in different directories
/// must produce two DISTINCT output files (no silent overwrite), and the HTML
/// index must link both with distinct hrefs.
#[test]
fn doc_same_stem_files_do_not_collide() {
    let base = std::env::temp_dir().join(format!("ascript_doc_collide_{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    std::fs::create_dir_all(base.join("a")).unwrap();
    std::fs::create_dir_all(base.join("b")).unwrap();
    std::fs::write(base.join("a/util.as"), "/// From A.\nexport fn fa() {}\n").unwrap();
    std::fs::write(base.join("b/util.as"), "/// From B.\nexport fn fb() {}\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_ascript");
    let outdir = base.join("out");

    // HTML: two distinct module pages + distinct index links.
    let status = Command::new(bin)
        .arg("doc")
        .arg("--format")
        .arg("html")
        .arg("--out")
        .arg(&outdir)
        .arg(&base)
        .output()
        .unwrap();
    assert!(status.status.success(), "doc html failed: {:?}", status);
    let index = std::fs::read_to_string(outdir.join("index.html")).unwrap();
    // Both modules present in the index, with DISTINCT hrefs (slug a_util / b_util).
    assert!(index.contains("a_util.html"), "a/util page linked: {index}");
    assert!(index.contains("b_util.html"), "b/util page linked: {index}");
    // Both module pages exist on disk (neither overwrote the other).
    assert!(outdir.join("a_util.html").exists(), "a_util.html written");
    assert!(outdir.join("b_util.html").exists(), "b_util.html written");
    let a_page = std::fs::read_to_string(outdir.join("a_util.html")).unwrap();
    let b_page = std::fs::read_to_string(outdir.join("b_util.html")).unwrap();
    assert!(a_page.contains("From A."), "a page has its own doc: {a_page}");
    assert!(b_page.contains("From B."), "b page has its own doc: {b_page}");

    // Markdown: two distinct .md files.
    let mddir = base.join("md");
    let status = Command::new(bin)
        .arg("doc")
        .arg("--format")
        .arg("md")
        .arg("--out")
        .arg(&mddir)
        .arg(&base)
        .output()
        .unwrap();
    assert!(status.status.success(), "doc md failed: {:?}", status);
    assert!(mddir.join("a_util.md").exists(), "a_util.md written");
    assert!(mddir.join("b_util.md").exists(), "b_util.md written");

    std::fs::remove_dir_all(&base).ok();
}

/// Review finding 2 (e2e): an HTML doc body's Markdown (fence, inline code, link,
/// bold) renders as real HTML, not literal characters.
#[test]
fn doc_html_renders_markdown_body() {
    let file = std::env::temp_dir().join(format!("ascript_doc_md_{}.as", std::process::id()));
    std::fs::write(
        &file,
        "/// Uses `len(x)`, is **important**, see [guide](https://ex.com).\n///\n/// ```ascript\n/// let y = 2\n/// ```\nexport fn f() {}\n",
    )
    .unwrap();
    let dir = std::env::temp_dir().join(format!("ascript_doc_md_out_{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let status = Command::new(bin)
        .arg("doc")
        .arg("--format")
        .arg("html")
        .arg("--out")
        .arg(&dir)
        .arg(&file)
        .output()
        .unwrap();
    assert!(status.status.success(), "doc failed: {:?}", status);
    let page_name = format!(
        "{}.html",
        file.file_stem().unwrap().to_string_lossy()
    );
    let page = std::fs::read_to_string(dir.join(&page_name)).unwrap();
    assert!(page.contains("<code>len(x)</code>"), "inline code: {page}");
    assert!(page.contains("<strong>important</strong>"), "bold: {page}");
    assert!(page.contains("<a href=\"https://ex.com\""), "link: {page}");
    assert!(page.contains("<pre><code>let y = 2"), "fence: {page}");
    assert!(!page.contains("**important**"), "no literal bold markers: {page}");

    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_file(&file).ok();
}

/// Review finding 6 (e2e): a bare `///` with empty body does NOT satisfy `--check`.
#[test]
fn doc_check_rejects_empty_doc_body() {
    let file = std::env::temp_dir().join(format!("ascript_doc_empty_{}.as", std::process::id()));
    std::fs::write(&file, "///\nexport fn a() {}\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = Command::new(bin).arg("doc").arg("--check").arg(&file).output().unwrap();
    assert!(!out.status.success(), "empty /// must NOT pass --check");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("a"), "reports the undocumented symbol: {combined}");
    std::fs::remove_file(&file).ok();
}

// ── DX Task 20: the `examples/advanced/documented_library.as` dogfooding artifact ──
//
// One fully-documented production-shaped module that simultaneously is a clean
// `ascript doc` source, exercises the parallel test runner + coverage path, and
// stays runnable. These tests pin all three surfaces over the committed example.
// Assertions are robust `contains`/set checks (not brittle full-file goldens) so
// ordinary edits to the example don't spuriously break them — only a real
// regression in run/doc/coverage does.
mod documented_library_artifact {
    use std::process::Command;

    const EXAMPLE: &str = "examples/advanced/documented_library.as";

    /// (1) The artifact runs to completion on the default VM with exit 0 and emits
    /// its deterministic driver output. (It is intentionally NOT in `EXAMPLE_SKIPS`,
    /// so the `vm_differential` corpus also runs it three-way; this is a focused
    /// CLI-level sanity check of the same property.)
    #[test]
    fn runs_to_completion_deterministically() {
        let bin = env!("CARGO_BIN_EXE_ascript");
        let out = Command::new(bin).arg("run").arg(EXAMPLE).output().unwrap();
        assert!(out.status.success(), "run must exit 0: {:?}", out);
        let s = String::from_utf8_lossy(&out.stdout);
        // The driver's deterministic lines (formatting, ledger posting, sum).
        assert!(s.contains("15.25 USD"), "money add+format: {s}");
        assert!(s.contains("-12.50 USD"), "negate+format: {s}");
        assert!(s.contains("1500 JPY"), "scale-1 currency format: {s}");
        assert!(s.contains("posted, balance 20.00 USD"), "ledger Posted: {s}");
        assert!(s.contains("rejected: would breach floor"), "ledger Rejected: {s}");
        assert!(s.contains("16.25 USD"), "sumAmounts fold: {s}");
    }

    /// (2a) `ascript doc --format md` renders the module: the `//!` module header,
    /// every public symbol's signature, and the `///` body text. Robust `contains`
    /// assertions (mirroring `doc_markdown_golden`'s style), not a byte golden.
    #[test]
    fn doc_markdown_contains_public_surface() {
        let bin = env!("CARGO_BIN_EXE_ascript");
        let out = Command::new(bin)
            .arg("doc")
            .arg("--format")
            .arg("md")
            .arg(EXAMPLE)
            .output()
            .unwrap();
        assert!(out.status.success(), "doc md failed: {:?}", out);
        let md = String::from_utf8_lossy(&out.stdout);

        // Module heading + module-doc (`//!`) body.
        assert!(md.contains("# documented_library"), "module heading: {md}");
        assert!(
            md.contains("money ledger") && md.contains("integer minor units"),
            "module doc body: {md}"
        );
        // Enum + its variants.
        assert!(md.contains("## enum `Currency`"), "enum heading: {md}");
        assert!(md.contains("`USD`") && md.contains("`JPY`"), "enum variants: {md}");
        // Functions: signature + a slice of the `///` body.
        assert!(
            md.contains("fn scaleOf(c: Currency): int"),
            "scaleOf signature: {md}"
        );
        assert!(
            md.contains("number of minor units in one major unit"),
            "scaleOf doc body: {md}"
        );
        assert!(
            md.contains("fn sumAmounts(amounts: array<Money>)"),
            "sumAmounts signature: {md}"
        );
        // Class: heading + a documented field + a method signature.
        assert!(md.contains("## class `Money`"), "class heading: {md}");
        assert!(md.contains("minor: int = 0"), "documented field sig: {md}");
        assert!(
            md.contains("static fn fromUnits(major: int, minor: int, currency: Currency): Money"),
            "static method signature: {md}"
        );
        // The payload-carrying ADT enum and its payload variant.
        assert!(md.contains("## enum `PostResult`"), "PostResult heading: {md}");
        assert!(md.contains("Posted(balance: Money)"), "payload variant sig: {md}");
    }

    /// (2b) Every public symbol is documented → `ascript doc --check` exits 0.
    #[test]
    fn doc_check_passes() {
        let bin = env!("CARGO_BIN_EXE_ascript");
        let out = Command::new(bin)
            .arg("doc")
            .arg("--check")
            .arg(EXAMPLE)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "every public symbol must be documented: {:?}",
            out
        );
    }

    /// (3) `ascript test --coverage` runs the in-file suite (all passing) and reports
    /// line coverage over the artifact. Assert the tally, the per-file + TOTAL lines,
    /// and that a healthy majority of lines are covered — not an exact ratio (which a
    /// future edit would break).
    #[test]
    fn test_coverage_runs_suite_and_reports_lines() {
        let bin = env!("CARGO_BIN_EXE_ascript");
        let out = Command::new(bin)
            .arg("test")
            .arg("--coverage")
            .arg(EXAMPLE)
            .output()
            .unwrap();
        assert!(out.status.success(), "all in-file tests must pass: {:?}", out);
        let s = String::from_utf8_lossy(&out.stdout);
        // The suite has six registered tests; all pass.
        assert!(s.contains("6 passed"), "tally (6 tests): {s}");
        assert!(s.contains("0 failed"), "no failures: {s}");
        // Coverage report names the file and a TOTAL.
        assert!(s.contains("documented_library.as:"), "per-file coverage line: {s}");
        assert!(s.contains("TOTAL:"), "total coverage line: {s}");
        // A healthy majority of instrumented lines are hit (the suite drives the public
        // surface). Parse the per-file "<hit>/<found>" ratio robustly.
        let ratio = s
            .lines()
            .find(|l| l.contains("documented_library.as:"))
            .and_then(|l| {
                // The ratio token is the `<hit>/<found>` where BOTH sides parse as
                // integers (skip the file path, which also contains `/`).
                l.split_whitespace().find_map(|w| {
                    let (h, f) = w.split_once('/')?;
                    Some((h.trim().parse::<u32>().ok()?, f.trim().parse::<u32>().ok()?))
                })
            });
        let (hit, found) = ratio.unwrap_or_else(|| panic!("could not parse coverage ratio: {s}"));
        assert!(found > 0, "instrumented some lines: {s}");
        assert!(
            hit * 4 >= found * 3,
            "at least 75% of lines covered ({hit}/{found}): {s}"
        );
    }

    /// (3b) The LCOV form is also well-formed over the artifact (a machine-readable
    /// coverage consumer's path).
    #[test]
    fn test_coverage_lcov_well_formed() {
        let bin = env!("CARGO_BIN_EXE_ascript");
        let out = Command::new(bin)
            .arg("test")
            .arg("--coverage=lcov")
            .arg(EXAMPLE)
            .output()
            .unwrap();
        assert!(out.status.success(), "{:?}", out);
        let s = String::from_utf8_lossy(&out.stdout);
        assert!(s.contains("SF:"), "LCOV source record: {s}");
        assert!(s.contains("DA:"), "LCOV line records: {s}");
        assert!(s.contains("LF:") && s.contains("LH:"), "LCOV found/hit: {s}");
        assert!(s.contains("end_of_record"), "LCOV terminator: {s}");
    }
}

// ── DX D2 Task 8: snapshot completion (--update-snapshots + orphan detection) ──
//
// These spawn `ascript test` with `current_dir` set to a fresh temp dir so the
// cwd-relative `__snapshots__/` store is hermetic and self-contained. All snapshot
// state lives under that dir and is cleaned up at the end.

#[cfg(all(feature = "sys", feature = "data"))]
mod snapshot_cli {
    use std::process::Command;

    /// A fresh, empty temp dir for a hermetic snapshot run; removed first so a stale
    /// `__snapshots__/` from a previous run never leaks in.
    fn fresh_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ascript_snapcli_{}_{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_test(dir: &std::path::Path, file: &std::path::Path, update: bool) -> std::process::Output {
        let bin = env!("CARGO_BIN_EXE_ascript");
        let mut cmd = Command::new(bin);
        cmd.arg("test").arg(file).current_dir(dir);
        if update {
            cmd.arg("--update-snapshots");
        }
        cmd.output().unwrap()
    }

    fn combined(out: &std::process::Output) -> String {
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }

    /// First run WRITES the snapshot (file appears + passes); a second run with the SAME
    /// value PASSES; a MUTATED value FAILS and the failure carries the structural diff.
    #[test]
    fn write_pass_then_mismatch_shows_diff() {
        let dir = fresh_dir("write_pass");
        let file = dir.join("t.as");
        let v1 = r#"
import * as assert from "std/assert"
test("snap", () => { assert.snapshot("user", {name: "ada", age: 36}) })
"#;
        std::fs::write(&file, v1).unwrap();

        // First run: writes + passes.
        let out1 = run_test(&dir, &file, false);
        assert!(out1.status.success(), "first run should pass: {}", combined(&out1));
        let snap = dir.join("__snapshots__").join("user.snap");
        assert!(snap.exists(), "snapshot file created");
        let stored1 = std::fs::read_to_string(&snap).unwrap();
        assert!(stored1.contains("ada"), "stored content: {stored1}");

        // Second run, same value: passes, file unchanged.
        let out2 = run_test(&dir, &file, false);
        assert!(out2.status.success(), "matching second run passes: {}", combined(&out2));
        assert_eq!(std::fs::read_to_string(&snap).unwrap(), stored1, "file unchanged");

        // Mutate the value → mismatch → FAIL with structural diff.
        let v2 = r#"
import * as assert from "std/assert"
test("snap", () => { assert.snapshot("user", {name: "ada", age: 99}) })
"#;
        std::fs::write(&file, v2).unwrap();
        let out3 = run_test(&dir, &file, false);
        assert!(!out3.status.success(), "mismatch must fail: {}", combined(&out3));
        let c = combined(&out3);
        assert!(c.contains(".age: 36 → 99"), "structural diff line: {c}");
        // File NOT rewritten without --update-snapshots.
        assert_eq!(std::fs::read_to_string(&snap).unwrap(), stored1, "no rewrite on a normal run");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--update-snapshots` re-baselines a CHANGED value: it re-writes the stored snapshot
    /// AND passes (and the on-disk content updates).
    #[test]
    fn update_snapshots_rebaselines_changed_value() {
        let dir = fresh_dir("rebaseline");
        let file = dir.join("t.as");
        std::fs::write(
            &file,
            "import * as assert from \"std/assert\"\ntest(\"s\", () => { assert.snapshot(\"k\", 1) })\n",
        )
        .unwrap();
        // Establish the baseline.
        assert!(run_test(&dir, &file, false).status.success());
        let snap = dir.join("__snapshots__").join("k.snap");
        let before = std::fs::read_to_string(&snap).unwrap();

        // Change the value: without the flag it FAILS.
        std::fs::write(
            &file,
            "import * as assert from \"std/assert\"\ntest(\"s\", () => { assert.snapshot(\"k\", 2) })\n",
        )
        .unwrap();
        let out_fail = run_test(&dir, &file, false);
        assert!(!out_fail.status.success(), "changed value fails without the flag");

        // WITH --update-snapshots: re-baselines (passes) and the file updates on disk.
        let out_upd = run_test(&dir, &file, true);
        assert!(
            out_upd.status.success(),
            "--update-snapshots re-baselines + passes: {}",
            combined(&out_upd)
        );
        let after = std::fs::read_to_string(&snap).unwrap();
        assert_ne!(before, after, "on-disk content changed");
        assert!(after.contains('2'), "now stores the new value: {after}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An ORPHANED snapshot (a stored `.snap` no assertion touches this run) is REPORTED
    /// on a normal run and REMOVED under `--update-snapshots`.
    #[test]
    fn orphan_reported_then_removed_with_update() {
        let dir = fresh_dir("orphan");
        let file = dir.join("t.as");
        // Run 1: two snapshot assertions → two .snap files.
        std::fs::write(
            &file,
            "import * as assert from \"std/assert\"\n\
             test(\"s\", () => {\n  assert.snapshot(\"alpha\", 1)\n  assert.snapshot(\"beta\", 2)\n})\n",
        )
        .unwrap();
        assert!(run_test(&dir, &file, false).status.success());
        let alpha = dir.join("__snapshots__").join("alpha.snap");
        let beta = dir.join("__snapshots__").join("beta.snap");
        assert!(alpha.exists() && beta.exists(), "both snapshots written");

        // Run 2: the program no longer makes the `beta` assertion → beta is an ORPHAN.
        std::fs::write(
            &file,
            "import * as assert from \"std/assert\"\n\
             test(\"s\", () => {\n  assert.snapshot(\"alpha\", 1)\n})\n",
        )
        .unwrap();
        // Without --update-snapshots: REPORTED (stderr) but NOT removed.
        let out_report = run_test(&dir, &file, false);
        assert!(out_report.status.success(), "passing tests: orphan is a notice, not a failure");
        let c = combined(&out_report);
        assert!(c.contains("orphan"), "reports an orphan: {c}");
        assert!(c.contains("beta"), "names the orphan file: {c}");
        assert!(beta.exists(), "orphan NOT removed without the flag");

        // With --update-snapshots: orphan REMOVED from disk.
        let out_remove = run_test(&dir, &file, true);
        assert!(out_remove.status.success(), "update run passes: {}", combined(&out_remove));
        assert!(!beta.exists(), "orphan removed under --update-snapshots");
        assert!(alpha.exists(), "the touched snapshot survives");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// DX D2 Task 6 — `ascript test --coverage` (VM line coverage on the Op::Break seam).
mod coverage_cli {
    use std::process::Command;

    fn fresh_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ascript_covcli_{}_{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn stdout(out: &std::process::Output) -> String {
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    // A program with a never-taken branch: line `return "nonpos"` is uncovered.
    const SRC: &str = "import * as assert from \"std/assert\"\n\
        \n\
        fn classify(n) {\n\
        \x20 if (n > 0) {\n\
        \x20   return \"pos\"\n\
        \x20 }\n\
        \x20 return \"nonpos\"\n\
        }\n\
        \n\
        test(\"pos\", () => {\n\
        \x20 print(classify(5))\n\
        \x20 assert.eq(classify(5), \"pos\")\n\
        })\n";

    #[test]
    fn coverage_text_reports_covered_and_uncovered() {
        let dir = fresh_dir("text");
        let file = dir.join("cov.as");
        std::fs::write(&file, SRC).unwrap();

        let bin = env!("CARGO_BIN_EXE_ascript");
        let out = Command::new(bin)
            .arg("test")
            .arg("--coverage")
            .arg(&file)
            .output()
            .unwrap();
        assert!(out.status.success(), "passing tests succeed: {:?}", out);
        let s = stdout(&out);
        // The tally survives.
        assert!(s.contains("1 passed"), "tally present: {s}");
        // The coverage report names the file and a TOTAL.
        assert!(s.contains("cov.as:"), "per-file line: {s}");
        assert!(s.contains("TOTAL:"), "total line: {s}");
        // Line 7 (`return \"nonpos\"`) is the never-taken branch — must be uncovered.
        assert!(s.contains("uncovered:"), "lists uncovered lines: {s}");
        assert!(s.contains('7'), "the dead branch line is listed: {s}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn coverage_lcov_format() {
        let dir = fresh_dir("lcov");
        let file = dir.join("cov.as");
        std::fs::write(&file, SRC).unwrap();

        let bin = env!("CARGO_BIN_EXE_ascript");
        let out = Command::new(bin)
            .arg("test")
            .arg("--coverage=lcov")
            .arg(&file)
            .output()
            .unwrap();
        assert!(out.status.success(), "{:?}", out);
        let s = stdout(&out);
        assert!(s.contains("SF:"), "LCOV source record: {s}");
        assert!(s.contains("DA:"), "LCOV line records: {s}");
        assert!(s.contains("LF:"), "LCOV lines-found: {s}");
        assert!(s.contains("LH:"), "LCOV lines-hit: {s}");
        assert!(s.contains("end_of_record"), "LCOV terminator: {s}");
        // The dead branch line 7 records DA:7,0 (instrumented, not hit).
        assert!(s.contains("DA:7,0"), "dead branch is DA:<line>,0: {s}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn coverage_observation_only_program_output_identical() {
        // A --coverage run's PROGRAM/test output is byte-identical to a plain `ascript
        // test` run — the coverage trap re-dispatches the SAME op, so the program result
        // and the test tally are unchanged (the coverage report is the only extra text,
        // appended after the tally). Under `ascript test`, `print` inside a test body is
        // CAPTURED (not streamed), so the program-output portion is the tally itself; the
        // coverage run reproduces it verbatim as its prefix.
        let dir = fresh_dir("obs");
        let file = dir.join("cov.as");
        std::fs::write(&file, SRC).unwrap();
        let bin = env!("CARGO_BIN_EXE_ascript");

        let plain = Command::new(bin).arg("test").arg(&file).output().unwrap();
        let cov = Command::new(bin)
            .arg("test")
            .arg("--coverage")
            .arg(&file)
            .output()
            .unwrap();
        assert!(plain.status.success() && cov.status.success());
        let ps = stdout(&plain);
        let cs = stdout(&cov);
        // The coverage run reproduces the plain run's output VERBATIM as its prefix, then
        // appends the coverage report. The observable program/test result is unchanged.
        assert!(
            cs.starts_with(&ps),
            "coverage stdout starts with the plain stdout unchanged\nplain:\n{ps}\ncov:\n{cs}"
        );
        // And the appended text is exactly the coverage report (nothing else changed).
        let appended = &cs[ps.len()..];
        assert!(
            appended.starts_with("coverage:"),
            "the only added output is the coverage report: {appended:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn coverage_unknown_format_is_a_clean_error() {
        let dir = fresh_dir("bad");
        let file = dir.join("cov.as");
        std::fs::write(&file, SRC).unwrap();
        let bin = env!("CARGO_BIN_EXE_ascript");
        let out = Command::new(bin)
            .arg("test")
            .arg("--coverage=bogus")
            .arg(&file)
            .output()
            .unwrap();
        assert!(!out.status.success(), "an unknown format fails cleanly");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("unknown coverage format"), "clear error: {err}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------------------------------
// DX diag polish: multibyte-UTF-8 caret correctness.
//
// AScript `Span`s are CHAR offsets (the documented invariant + ariadne's
// char-mode default). For pure-ASCII source byte==char so the bug is invisible,
// but ANY multibyte char before a span desynchronizes the byte-native CST
// front-end from the char-mode renderers: the VM run-time panic frame VANISHES,
// the tree-walker frame shifts a column, parse-error and lint carets blank out.
// These tests pin the FIXED behavior.
// ---------------------------------------------------------------------------

/// A VM run-time panic over source containing a multibyte char (`π`, 2 bytes)
/// BEFORE the failing span must render a FULL ariadne caret frame, byte-identical
/// to the tree-walker. (Pre-fix: the VM frame vanished entirely because the byte
/// span overran the char-mode source and ariadne dropped the report.)
#[test]
fn multibyte_runtime_panic_renders_caret_frame_both_engines() {
    // `let π = 0` then `1 / π` → integer division by zero. π is 2 UTF-8 bytes, so
    // every span after it is byte!=char.
    let src = "let \u{3c0} = 0\nprint(1 / \u{3c0})\n";
    let file = std::env::temp_dir().join(format!("ascript_mb_panic_{}.as", std::process::id()));
    std::fs::write(&file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");

    let render = |tw: bool| -> String {
        let mut cmd = Command::new(bin);
        cmd.arg("run");
        if tw {
            cmd.arg("--tree-walker");
        }
        let out = cmd.arg(&file).output().unwrap();
        assert!(!out.status.success(), "should panic (tw={tw})");
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

    // A real caret frame is present on the VM path (the regression that lost it).
    assert!(
        vm.contains('┬') || vm.contains('─'),
        "VM panic must render a caret/underline row, got:\n{vm}"
    );
    // The underlined source line is the SECOND line and is NOT blank — it shows the
    // `1 / π)` expression text.
    assert!(
        vm.contains("1 / \u{3c0}"),
        "the rendered source row must contain `1 / \u{3c0}`, got:\n{vm}"
    );
    // Byte-identical carets across engines (the core fix).
    assert_eq!(
        vm, tw,
        "VM and tree-walker rendered different carets:\n--- vm ---\n{vm}\n--- tw ---\n{tw}"
    );
}

/// A parse error AFTER a multibyte char must still render a caret frame (not a bare
/// `Error:` line) via BOTH `ascript run` and `ascript check`.
#[test]
fn multibyte_parse_error_renders_caret_frame() {
    // `let y = π +` is an incomplete expression (`+` has no RHS) → "expected
    // expression". π on the first line pushes every later byte offset past char.
    let src = "let \u{3c0} = 3\nlet y = \u{3c0} +\n";
    let file = std::env::temp_dir().join(format!("ascript_mb_parse_{}.as", std::process::id()));
    std::fs::write(&file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");

    for sub in ["run", "check"] {
        let out = Command::new(bin).arg(sub).arg(&file).output().unwrap();
        let err = strip_ansi(&String::from_utf8_lossy(&out.stderr))
            + &strip_ansi(&String::from_utf8_lossy(&out.stdout));
        assert!(
            err.contains('╭') && (err.contains('┬') || err.contains('─')),
            "`{sub}` over multibyte source must render a caret frame, got:\n{err}"
        );
    }
    let _ = std::fs::remove_file(&file);
}

/// A lint (unused-binding) over source where a multibyte char PRECEDES the flagged
/// identifier on the same line must underline the IDENTIFIER, not a column shifted
/// by the multibyte byte-count, and the source row must not be blank.
#[test]
fn multibyte_check_lint_underlines_correctly() {
    // A `check` lint (unused-binding) over source with a multibyte char PRECEDING the
    // flagged statement on the same line must render IDENTICALLY (caret rows + 1-based
    // char column) to the same source with the multibyte char swapped for an ASCII one
    // — i.e. no byte/char skew. The pre-fix bug produced a BLANK source row and a
    // column shifted by the multibyte byte count.
    let bin = env!("CARGO_BIN_EXE_ascript");

    // `<X> = 3; let unused = 5` — same shape, `<X>` is either `π` (2 bytes) or `p`.
    let render = |marker: &str| -> String {
        let src = format!("let {marker} = 3; let unused = 5\nprint({marker})\n");
        let file =
            std::env::temp_dir().join(format!("ascript_mb_lint_{}_{marker}.as", std::process::id()));
        std::fs::write(&file, &src).unwrap();
        let out = Command::new(bin).arg("check").arg(&file).output().unwrap();
        let r = strip_ansi(&String::from_utf8_lossy(&out.stdout))
            + &strip_ansi(&String::from_utf8_lossy(&out.stderr));
        let _ = std::fs::remove_file(&file);
        // Drop the location-gutter line (path differs) but KEEP its `:line:col` suffix
        // by re-extracting it below; here keep the source + caret rows.
        r.lines()
            .filter(|l| !l.contains(".as:"))
            .map(|l| l.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mb = render("\u{3c0}");
    let ascii = render("p");

    // The lint fired and the source row is NOT blank (the regression blanked it).
    assert!(
        mb.contains("unused-binding") && mb.contains("let \u{3c0} = 3; let unused = 5"),
        "multibyte lint must render the real source row, got:\n{mb}"
    );
    // The caret geometry is byte-identical to the ASCII variant (the skew fix): both
    // underline the SAME statement at the SAME column.
    let caret = |s: &str| -> String {
        s.lines()
            .filter(|l| l.contains('┬') || l.contains('╰'))
            .map(|l| l.replace('\u{3c0}', "p")) // normalize the marker char itself
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        caret(&mb),
        caret(&ascii),
        "multibyte and ASCII carets must be identical:\n-- mb --\n{mb}\n-- ascii --\n{ascii}"
    );

    // And the reported 1-based char column matches the ASCII baseline (col 11).
    let col_of = |marker: &str| -> String {
        let src = format!("let {marker} = 3; let unused = 5\nprint({marker})\n");
        let file = std::env::temp_dir()
            .join(format!("ascript_mb_lintc_{}_{marker}.as", std::process::id()));
        std::fs::write(&file, &src).unwrap();
        let out = Command::new(bin).arg("check").arg(&file).output().unwrap();
        let r = strip_ansi(&String::from_utf8_lossy(&out.stdout));
        let _ = std::fs::remove_file(&file);
        // Extract the `:1:NN` column from the gutter line.
        r.lines()
            .find_map(|l| l.split(".as:1:").nth(1).map(|s| {
                s.chars().take_while(|c| c.is_ascii_digit()).collect::<String>()
            }))
            .unwrap_or_default()
    };
    assert_eq!(
        col_of("\u{3c0}"),
        col_of("p"),
        "multibyte and ASCII lint columns must match"
    );
    assert_eq!(col_of("\u{3c0}"), "11", "lint column must be char col 11 (1-based)");
}


/// SELF-CONTAINED-BUNDLES (Task 1.5) — `ascript build` of a MULTI-module program emits an
/// `ASCRIPTA` archive embedding the whole import graph, and `ascript run out.aso` works from
/// a directory that does NOT contain the sources. A SINGLE-module program still emits a bare
/// `ASO\0` chunk (back-compat — byte-identical to the pre-archive artifact).
#[test]
fn build_multimodule_emits_archive_and_runs_without_sources() {
    let bin = env!("CARGO_BIN_EXE_ascript");

    // A temp dir with NO `.as` sources — the run must be entirely self-contained.
    let dir = std::env::temp_dir().join(format!("ascript_bundle15_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out_aso = dir.join("out.aso");

    // The example pair lives under `examples/` (the build reads sources from there); the
    // produced `.aso` is self-contained and is run from `dir`, which has none of them.
    let entry = std::path::Path::new("examples/bundle_multimodule.as");
    let build = Command::new(bin)
        .args(["build"])
        .arg(entry)
        .arg("-o")
        .arg(&out_aso)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    // The artifact leads with the ARCHIVE magic (`ASCRIPTA`), not a bare `ASO\0` chunk.
    let bytes = std::fs::read(&out_aso).unwrap();
    assert_eq!(
        &bytes[..8],
        b"ASCRIPTA",
        "a multi-module build must emit an ASCRIPTA archive"
    );

    // Run the archive from a directory WITHOUT the sources — output must match the source run.
    let run = Command::new(bin)
        .arg("run")
        .arg(&out_aso)
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "running the archive failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let reference = Command::new(bin).arg("run").arg(entry).output().unwrap();
    assert_eq!(
        run.stdout, reference.stdout,
        "archive run stdout must match the source run"
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "Hello, world!\nbundled!!!\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Compat: a SINGLE-module `ascript build` still emits a bare `ASO\0` chunk (NOT an archive),
/// so existing `.aso` artifacts/goldens stay byte-identical to today.
#[test]
fn build_single_module_emits_bare_aso_chunk() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = std::env::temp_dir().join(format!("ascript_bundle15_single_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out_aso = dir.join("hello.aso");

    let build = Command::new(bin)
        .args(["build"])
        .arg("examples/hello.as")
        .arg("-o")
        .arg(&out_aso)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let bytes = std::fs::read(&out_aso).unwrap();
    assert_eq!(
        &bytes[..4],
        b"ASO\0",
        "a single-module build must emit a bare ASO\\0 chunk (compat)"
    );

    // And it still runs.
    let run = Command::new(bin).arg("run").arg(&out_aso).output().unwrap();
    assert!(run.status.success());

    let _ = std::fs::remove_dir_all(&dir);
}

// ── SELF-CONTAINED-BUNDLES (Task 2.4) — tree-shake build report + reproducible digest ──

/// A fresh, empty scratch dir under the system temp, named for this test + process.
fn bundle_scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ascript_shake24_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A NAMESPACE import whose alias is used DYNAMICALLY (`m[key]`) pins the whole target —
/// the build's stderr names the pinned module, the alias, the reason, and a `key:line:col`
/// LOCATION rendered against the importer's source. The `bundled`/`compiled` line stays on
/// stdout; the shake summary is stderr-only.
#[test]
fn build_report_pin_lists_namespace_with_location() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = bundle_scratch("pin");

    // `util.as` exports two fns; `app.as` imports the whole namespace then indexes it
    // dynamically → the shaker cannot prove which exports are live → pin whole.
    std::fs::write(
        dir.join("util.as"),
        "export fn alpha(): int { return 1 }\nexport fn beta(): int { return 2 }\n",
    )
    .unwrap();
    // The dynamic `m[k]` sits on a known line/col so we can assert a concrete location.
    std::fs::write(
        dir.join("app.as"),
        "import * as m from \"./util\"\nlet k = \"alpha\"\nlet r = m[k]\nprint(r)\n",
    )
    .unwrap();

    let out_aso = dir.join("app.aso");
    let build = Command::new(bin)
        .args(["build"])
        .arg(dir.join("app.as"))
        .arg("-o")
        .arg(&out_aso)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("util.as"),
        "pin line must name the pinned module 'util.as'; stderr=\n{stderr}"
    );
    assert!(
        stderr.contains("namespace 'm'"),
        "pin line must name the offending alias 'm'; stderr=\n{stderr}"
    );
    assert!(
        stderr.contains("indexed/escapes"),
        "pin line must state the reason; stderr=\n{stderr}"
    );
    // The rendered location is `<importer key>:line:col` — `app.as:3:9` (1-based) for the
    // `m[k]` use on line 3. We assert the location is importer-relative and anchored to the
    // `m[k]` line (line 3), robustly to the exact column.
    assert!(
        stderr.contains("at app.as:3:"),
        "pin line must carry an importer-relative `key:line:col` location; stderr=\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A `{ used }` named import from a library with an unused `dead` fn drops `dead`; the
/// build's stderr summary names the dropped declaration and a drop count.
#[test]
fn build_report_lists_dropped_declarations() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = bundle_scratch("drop");

    std::fs::write(
        dir.join("lib.as"),
        "export fn used(): int { return 1 }\nexport fn dead(): int { return 2 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.as"),
        "import { used } from \"./lib\"\nprint(used())\n",
    )
    .unwrap();

    let out_aso = dir.join("main.aso");
    let build = Command::new(bin)
        .args(["build"])
        .arg(dir.join("main.as"))
        .arg("-o")
        .arg(&out_aso)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("tree-shaking: dropped"),
        "stderr must carry the tree-shaking summary line; stderr=\n{stderr}"
    );
    assert!(
        stderr.contains("dead"),
        "summary must name the dropped 'dead' fn; stderr=\n{stderr}"
    );
    assert!(
        stderr.contains("lib.as"),
        "summary must name the dropping module 'lib.as'; stderr=\n{stderr}"
    );
    // `used` is KEPT → only one declaration is dropped from lib.as.
    assert!(
        !stderr.contains("dropped 2"),
        "only 'dead' is dropped, not 'used'; stderr=\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Building the SAME multi-module program twice (to two different outputs) produces a
/// byte-identical, NON-ZERO `shake_digest` in the archive manifest — the digest is a
/// reproducible function of the source (no machine-specific data, no HashMap iteration).
#[test]
fn build_shake_digest_is_reproducible_and_nonzero() {
    use ascript::vm::archive::ModuleArchive;
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = bundle_scratch("digest");

    std::fs::write(
        dir.join("lib.as"),
        "export fn used(): int { return 1 }\nexport fn dead(): int { return 2 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.as"),
        "import { used } from \"./lib\"\nprint(used())\n",
    )
    .unwrap();

    let build_to = |name: &str| -> [u8; 32] {
        let out = dir.join(name);
        let b = Command::new(bin)
            .args(["build"])
            .arg(dir.join("main.as"))
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(
            b.status.success(),
            "build failed: {}",
            String::from_utf8_lossy(&b.stderr)
        );
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(&bytes[..8], b"ASCRIPTA", "expected a multi-module archive");
        let arch = ModuleArchive::decode(&bytes).expect("archive decodes");
        arch.shake_digest
    };

    let d1 = build_to("a.aso");
    let d2 = build_to("b.aso");
    assert_eq!(d1, d2, "the shake digest must be reproducible across builds");
    assert_ne!(d1, [0u8; 32], "a program with drops must have a non-zero digest");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The shake report goes to STDERR only: `build`'s stdout carries just the `compiled …`
/// line, and the bundled program's own stdout (when later run) is unchanged by shaking.
#[test]
fn build_report_does_not_pollute_stdout() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let dir = bundle_scratch("stdout");

    std::fs::write(
        dir.join("lib.as"),
        "export fn used(): int { return 7 }\nexport fn dead(): int { return 2 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.as"),
        "import { used } from \"./lib\"\nprint(used())\n",
    )
    .unwrap();

    let out_aso = dir.join("main.aso");
    let build = Command::new(bin)
        .args(["build"])
        .arg(dir.join("main.as"))
        .arg("-o")
        .arg(&out_aso)
        .output()
        .unwrap();
    assert!(build.status.success());

    // build's STDOUT must contain ONLY the `compiled … -> …` line — no shake report.
    let stdout = String::from_utf8_lossy(&build.stdout);
    assert!(
        stdout.starts_with("compiled "),
        "build stdout must lead with the `compiled …` line; stdout=\n{stdout}"
    );
    assert!(
        !stdout.contains("tree-shaking"),
        "the tree-shaking report must NOT be on stdout; stdout=\n{stdout}"
    );
    // The shake summary IS on stderr.
    assert!(
        String::from_utf8_lossy(&build.stderr).contains("tree-shaking"),
        "the tree-shaking report must be on stderr"
    );

    // Running the bundled program emits exactly the program's own stdout, unaffected.
    let run = Command::new(bin)
        .arg("run")
        .arg(&out_aso)
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(run.status.success());
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "7\n",
        "the program's own stdout must be unchanged by shaking"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// DEFER Task 4.3 (REPL): a top-level `defer print(...)` in a REPL submission
/// runs at the END of that submission (§2.3). The REPL treats each newline-
/// delimited line as its own program, so `defer print("deferred")` in
/// submission 1 fires at the end of that submission — BEFORE submission 2's
/// `print("before")`. This pins the correct REPL defer semantics.
#[test]
fn repl_defer_top_level_runs_at_submission_end() {
    // Submission 1: `defer print("deferred")` — registers the defer, body
    // completes, defer fires → "deferred" is printed.
    // Submission 2: `print("before")` → "before" is printed.
    // Expected: "deferred" appears BEFORE "before" in the combined output.
    let (out, _) =
        run_repl_session("defer print(\"deferred\")\nprint(\"before\")\n", false);
    assert!(out.contains("deferred"), "'deferred' missing from output: {out:?}");
    assert!(out.contains("before"), "'before' missing from output: {out:?}");
    let deferred_pos = out.find("deferred").unwrap();
    let before_pos = out.find("before").unwrap();
    assert!(
        deferred_pos < before_pos,
        "top-level defer fires at end of its submission (before the next submission); got: {out:?}"
    );
}

/// DEFER Task 4.3 (REPL): `defer` inside a REPL-defined fn fires when that fn
/// returns — not at submission end.
#[test]
fn repl_defer_inside_fn_runs_on_fn_return() {
    // Define a fn with a defer, then call it.
    let session =
        "fn cleanup() { defer print(\"done\") \n print(\"body\") }\ncleanup()\n";
    let (out, _) = run_repl_session(session, false);
    let body_pos = out.find("body").expect("'body' missing from output");
    let done_pos = out.find("done").expect("'done' missing from output");
    assert!(
        body_pos < done_pos,
        "defer inside fn must fire after fn body; got: {out:?}"
    );
}
