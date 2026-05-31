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
    let path =
        std::env::temp_dir().join(format!("ascript_exit_oor_{}.as", std::process::id()));
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
