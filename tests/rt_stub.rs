//! RT §10.2 — end-to-end battery for `ascript-rt` (the runtime-only stub).
//!
//! All tests here are gated on the `ASCRIPT_RT_BIN` environment variable pointing
//! to a built `ascript-rt` binary. When the variable is unset the tests self-skip
//! (they print a reason to stderr and return), so a plain `cargo test` run with no
//! rt build stays GREEN.
//!
//! # To run locally
//!
//! ```bash
//! scripts/build-rt.sh rt-full
//! ASCRIPT_RT_BIN=$PWD/target/release/ascript-rt cargo test --test rt_stub
//! ```
//!
//! # RUN-NOW vs IGNORE cases
//!
//! **RUN-NOW** (pass when `ASCRIPT_RT_BIN` is set, skip when not set):
//!   - `stub_rt_info_schema`
//!   - `stub_runs_aso_by_path`
//!   - `stub_bad_argv_is_clean_usage_error`
//!
//! **IGNORE until Task 7** (`--stub` flag not yet implemented; `#[ignore]` guards them):
//!   - `stub_bundle_matches_ascript_run_output`
//!   - `stub_bundle_multi_module_archive_runs_from_empty_dir`
//!   - `stub_bundle_worker_parity`
//!   - `stub_bundle_caps_floor_and_ascript_deny`
//!   - `stub_missing_module_error_names_the_toolchain`
//!   - `stub_panic_diagnostics_render_from_embedded_source`

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Return the path to the normal `ascript` toolchain binary (via the Cargo-set env).
fn toolchain_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ascript")
}

/// Return the path to the `ascript-rt` stub, or `None` if `ASCRIPT_RT_BIN` is not set.
/// Tests call this first and skip (with an eprintln) when it returns `None`.
fn rt_bin() -> Option<String> {
    std::env::var("ASCRIPT_RT_BIN").ok()
}

/// A unique temp dir for one test (avoids cross-test collisions in parallel runs).
/// Removed when dropped.
struct TmpDir(PathBuf);

impl std::ops::Deref for TmpDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TmpDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Create a unique temp dir for one test. The `tag` must be unique per test; the PID
/// disambiguates concurrent test-runner processes.
fn tmp_dir(tag: &str) -> TmpDir {
    let d = std::env::temp_dir().join(format!(
        "ascript_rt_stub_{}_{}_{}",
        tag,
        std::process::id(),
        // Add nonce to avoid collisions if test is rerun quickly
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    TmpDir(d)
}

/// Write a file to `dir/name` with `body` and return its path.
fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

/// Build a `.as` source to a `.aso` artifact using the TOOLCHAIN `ascript build`.
/// Returns the path to the `.aso` file. Asserts success.
fn build_aso(src: &Path, out: &Path) {
    let o = Command::new(toolchain_bin())
        .args(["build"])
        .arg(src)
        .arg("-o")
        .arg(out)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "ascript build failed:\n  src={}\n  stdout={}\n  stderr={}",
        src.display(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(out.exists(), "build did not produce {}", out.display());
}

/// Run the rt stub binary with `args`, from `cwd`, with a SCRUBBED PATH.
/// Returns the full `Output`.
fn run_rt(rt: &str, cwd: &Path, args: &[&str]) -> Output {
    Command::new(rt)
        .args(args)
        .current_dir(cwd)
        .env("PATH", "") // scrubbed: toolchain not reachable
        .output()
        .unwrap()
}

/// Run `ascript run <src>` with the toolchain for a reference output.
fn run_ref(src: &Path, args: &[&str]) -> Output {
    Command::new(toolchain_bin())
        .arg("run")
        .arg(src)
        .args(args)
        .output()
        .unwrap()
}

// ── RUN-NOW tests ─────────────────────────────────────────────────────────────

/// RT §10.2 / RUN-NOW — `$ASCRIPT_RT_BIN --rt-info` emits exactly one JSON line
/// with all required fields at their expected values.
///
/// Asserted fields:
///   - `name` == "ascript-rt"
///   - `version` is non-empty
///   - `target` is non-empty
///   - `tier` is present (non-empty)
///   - `features` is a JSON array containing at least "shared" (all tiers include it)
///   - `aso_format_version` == 29
///   - `archive_version` == 1
#[test]
fn stub_rt_info_schema() {
    let rt = match rt_bin() {
        Some(b) => b,
        None => {
            eprintln!("[rt_stub] SKIP stub_rt_info_schema — ASCRIPT_RT_BIN not set");
            return;
        }
    };

    let dir = tmp_dir("rt_info");
    let out = run_rt(&rt, &dir, &["--rt-info"]);

    assert!(
        out.status.success(),
        "--rt-info must succeed (exit 0); got exit={:?}\nstdout={}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let raw = String::from_utf8_lossy(&out.stdout);
    let line = raw.trim();
    assert!(
        !line.is_empty(),
        "--rt-info must print at least one line; got empty output"
    );
    // Must be exactly ONE line (no trailing newlines beyond the first).
    assert_eq!(
        raw.lines().count(),
        1,
        "--rt-info must emit exactly one JSON line; got {} lines:\n{raw}",
        raw.lines().count()
    );

    // ── Parse the JSON line with a hand-rolled scanner (no serde in this test) ──
    // We look for the quoted key-value pairs we care about.
    fn extract_str<'a>(json: &'a str, key: &str) -> Option<&'a str> {
        let needle = format!("\"{}\":\"", key);
        let start = json.find(&needle)? + needle.len();
        let end = json[start..].find('"')? + start;
        Some(&json[start..end])
    }
    fn extract_num(json: &str, key: &str) -> Option<u64> {
        let needle = format!("\"{}\":", key);
        let start = json.find(&needle)? + needle.len();
        let tail = json[start..].trim_start();
        let end = tail
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(tail.len());
        tail[..end].parse().ok()
    }
    fn has_array_element(json: &str, key: &str, elem: &str) -> bool {
        // Find `"key":[...]` and check `elem` appears inside the brackets.
        let needle = format!("\"{}\":[", key);
        if let Some(start) = json.find(&needle) {
            let tail = &json[start + needle.len()..];
            if let Some(end) = tail.find(']') {
                let arr = &tail[..end];
                return arr.contains(&format!("\"{}\"", elem));
            }
        }
        false
    }

    assert_eq!(
        extract_str(line, "name"),
        Some("ascript-rt"),
        "name field must be \"ascript-rt\"; full JSON: {line}"
    );

    let version = extract_str(line, "version").unwrap_or("");
    assert!(
        !version.is_empty(),
        "version field must be non-empty; full JSON: {line}"
    );

    let target = extract_str(line, "target").unwrap_or("");
    assert!(
        !target.is_empty(),
        "target field must be non-empty; full JSON: {line}"
    );

    let tier = extract_str(line, "tier").unwrap_or("");
    assert!(
        !tier.is_empty(),
        "tier field must be non-empty; full JSON: {line}"
    );

    assert!(
        has_array_element(line, "features", "shared"),
        "features array must contain \"shared\"; full JSON: {line}"
    );

    assert_eq!(
        extract_num(line, "aso_format_version"),
        Some(29),
        "aso_format_version must be 29; full JSON: {line}"
    );

    assert_eq!(
        extract_num(line, "archive_version"),
        Some(1),
        "archive_version must be 1; full JSON: {line}"
    );
}

/// RT §10.2 / RUN-NOW — the rt stub can run a pre-built `.aso` file by path.
///
/// Two programs: (a) a trivial hello-world single-liner, (b) a slightly more substantive
/// multi-statement program with arithmetic and string interpolation. Both are built with
/// the TOOLCHAIN `ascript build` and then executed via `$ASCRIPT_RT_BIN <path>.aso`.
#[test]
fn stub_runs_aso_by_path() {
    let rt = match rt_bin() {
        Some(b) => b,
        None => {
            eprintln!("[rt_stub] SKIP stub_runs_aso_by_path — ASCRIPT_RT_BIN not set");
            return;
        }
    };

    let dir = tmp_dir("rt_aso_path");
    let empty_cwd = tmp_dir("rt_aso_path_cwd");

    // (a) Trivial hello-world.
    let hello_src = write(&dir, "hello.as", "print(\"hello from rt stub\")\n");
    let hello_aso = dir.join("hello.aso");
    build_aso(&hello_src, &hello_aso);

    let out_a = run_rt(&rt, &empty_cwd, &[hello_aso.to_str().unwrap()]);
    assert!(
        out_a.status.success(),
        "rt stub failed to run hello.aso:\n  exit={:?}\n  stdout={}\n  stderr={}",
        out_a.status.code(),
        String::from_utf8_lossy(&out_a.stdout),
        String::from_utf8_lossy(&out_a.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out_a.stdout),
        "hello from rt stub\n",
        "hello.aso stdout mismatch"
    );

    // (b) Multi-statement program — arithmetic, string interpolation, loop.
    let prog = write(
        &dir,
        "multi.as",
        "let x = 6 * 7\n\
         let name = \"world\"\n\
         print(`answer=${x}`)\n\
         let acc = 0\n\
         for (i in 0..5) { acc = acc + i }\n\
         print(`sum=${acc}`)\n\
         print(`hello ${name}!`)\n",
    );
    let prog_aso = dir.join("multi.aso");
    build_aso(&prog, &prog_aso);

    let out_b = run_rt(&rt, &empty_cwd, &[prog_aso.to_str().unwrap()]);
    assert!(
        out_b.status.success(),
        "rt stub failed to run multi.aso:\n  exit={:?}\n  stdout={}\n  stderr={}",
        out_b.status.code(),
        String::from_utf8_lossy(&out_b.stdout),
        String::from_utf8_lossy(&out_b.stderr)
    );
    let multi_out = String::from_utf8_lossy(&out_b.stdout);
    assert!(
        multi_out.contains("answer=42"),
        "multi.aso must print answer=42; got: {multi_out}"
    );
    assert!(
        multi_out.contains("sum=10"),
        "multi.aso must print sum=10 (0+1+2+3+4); got: {multi_out}"
    );
    assert!(
        multi_out.contains("hello world!"),
        "multi.aso must print hello world!; got: {multi_out}"
    );

    // (c) Equivalence with `ascript run` — the rt stub must produce identical stdout.
    let ref_out = run_ref(&prog, &[]);
    assert!(
        ref_out.status.success(),
        "ascript run of multi.as must succeed (reference): stderr={}",
        String::from_utf8_lossy(&ref_out.stderr)
    );
    assert_eq!(
        out_b.stdout, ref_out.stdout,
        "rt stub stdout must match ascript run stdout for multi.as"
    );
}

/// RT §10.2 / RUN-NOW — unknown/unsupported argv produces a clean usage error
/// (exit 2, non-empty stderr) with NO panic.
///
/// Specifically: `$ASCRIPT_RT_BIN --bogus` must exit 2, write something to stderr,
/// and stderr must NOT contain "panicked" (a Rust panic message).
#[test]
fn stub_bad_argv_is_clean_usage_error() {
    let rt = match rt_bin() {
        Some(b) => b,
        None => {
            eprintln!(
                "[rt_stub] SKIP stub_bad_argv_is_clean_usage_error — ASCRIPT_RT_BIN not set"
            );
            return;
        }
    };

    let dir = tmp_dir("rt_bad_argv");

    // (a) An unrecognized flag.
    let out_flag = run_rt(&rt, &dir, &["--bogus"]);
    assert_eq!(
        out_flag.status.code(),
        Some(2),
        "--bogus must exit 2; got exit={:?}\nstdout={}\nstderr={}",
        out_flag.status.code(),
        String::from_utf8_lossy(&out_flag.stdout),
        String::from_utf8_lossy(&out_flag.stderr)
    );
    let flag_stderr = String::from_utf8_lossy(&out_flag.stderr);
    assert!(
        !flag_stderr.is_empty(),
        "--bogus must write something to stderr; got empty stderr"
    );
    assert!(
        !flag_stderr.contains("panicked"),
        "--bogus must NOT trigger a Rust panic; stderr contains 'panicked':\n{flag_stderr}"
    );

    // (b) No arguments at all (also a usage error).
    let out_empty = run_rt(&rt, &dir, &[]);
    assert_eq!(
        out_empty.status.code(),
        Some(2),
        "no args must exit 2; got exit={:?}\nstderr={}",
        out_empty.status.code(),
        String::from_utf8_lossy(&out_empty.stderr)
    );
    let empty_stderr = String::from_utf8_lossy(&out_empty.stderr);
    assert!(
        !empty_stderr.is_empty(),
        "no args must write usage to stderr"
    );
    assert!(
        !empty_stderr.contains("panicked"),
        "no args must NOT trigger a Rust panic; stderr: {empty_stderr}"
    );

    // (c) Multiple unknown args.
    let out_multi = run_rt(&rt, &dir, &["--foo", "--bar"]);
    assert_eq!(
        out_multi.status.code(),
        Some(2),
        "multiple unknown args must exit 2; got exit={:?}",
        out_multi.status.code()
    );
    assert!(
        !String::from_utf8_lossy(&out_multi.stderr)
            .to_lowercase()
            .contains("panicked"),
        "multiple unknown args must not panic"
    );
}

// ── IGNORE until Task 7 (`--stub` flag) ───────────────────────────────────────
//
// These tests require `ascript build --native prog.as --stub $ASCRIPT_RT_BIN -o out`,
// which is not yet implemented. Task 7 will wire the `--stub` flag and remove the
// `#[ignore]` annotations.

/// RT §10.2 / IGNORE (Task 7) — a program bundled ONTO the rt stub (via `--stub`) produces
/// the same stdout/exit as `ascript run prog.as`. Tests hello-world and argv forwarding.
#[test]
#[ignore = "unblocked by Task 7 (--stub wiring)"]
fn stub_bundle_matches_ascript_run_output() {
    let rt = match rt_bin() {
        Some(b) => b,
        None => return,
    };
    let dir = tmp_dir("rt_bundle_equiv");
    let src = write(&dir, "prog.as", "print(\"bundled onto rt stub\")\n");
    let out_path = dir.join("prog_rt");

    // Requires: ascript build --native prog.as --stub $rt -o out_path
    let build_out = Command::new(toolchain_bin())
        .args(["build", "--native"])
        .arg(&src)
        .arg("--stub")
        .arg(&rt)
        .arg("-o")
        .arg(&out_path)
        .output()
        .unwrap();
    assert!(
        build_out.status.success(),
        "build --native --stub failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr)
    );

    let empty_cwd = tmp_dir("rt_bundle_equiv_cwd");
    let b = Command::new(&out_path)
        .current_dir(&empty_cwd)
        .env("PATH", "")
        .output()
        .unwrap();
    let r = run_ref(&src, &[]);

    assert_eq!(b.stdout, r.stdout, "bundled-onto-rt stdout differs from ascript run");
    assert_eq!(
        b.status.code(),
        r.status.code(),
        "bundled-onto-rt exit differs from ascript run"
    );
}

/// RT §10.2 / IGNORE (Task 7) — a multi-module program bundled onto the rt stub runs from an
/// EMPTY cwd (the import graph is embedded in the archive; nothing on disk at run time).
#[test]
#[ignore = "unblocked by Task 7 (--stub wiring)"]
fn stub_bundle_multi_module_archive_runs_from_empty_dir() {
    let rt = match rt_bin() {
        Some(b) => b,
        None => return,
    };
    let dir = tmp_dir("rt_bundle_mm");
    write(
        &dir,
        "util.as",
        "export fn greet(name: string): string { return `Hello, ${name}!` }\n",
    );
    let entry = write(
        &dir,
        "app.as",
        "import { greet } from \"./util\"\nprint(greet(\"rt stub\"))\n",
    );
    let out_path = dir.join("mm_rt");

    let build_out = Command::new(toolchain_bin())
        .args(["build", "--native"])
        .arg(&entry)
        .arg("--stub")
        .arg(&rt)
        .arg("-o")
        .arg(&out_path)
        .output()
        .unwrap();
    assert!(
        build_out.status.success(),
        "multi-module --stub build failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr)
    );

    let empty_cwd = tmp_dir("rt_bundle_mm_cwd");
    let b = Command::new(&out_path)
        .current_dir(&empty_cwd)
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(
        b.status.success(),
        "multi-module rt bundle failed from empty dir:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&b.stdout),
        String::from_utf8_lossy(&b.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&b.stdout),
        "Hello, rt stub!\n",
        "multi-module rt bundle stdout mismatch"
    );
}

/// RT §10.2 / IGNORE (Task 7) — a `worker fn` pool program bundled onto the rt stub produces
/// the same stdout as `ascript run` (chunk-shipping path exercises worker serialization).
#[test]
#[ignore = "unblocked by Task 7 (--stub wiring)"]
fn stub_bundle_worker_parity() {
    let rt = match rt_bin() {
        Some(b) => b,
        None => return,
    };
    let dir = tmp_dir("rt_bundle_worker");
    let src = write(
        &dir,
        "worker_prog.as",
        "import * as task from \"std/task\"\n\
         worker fn double(n: number): number { return n * 2 }\n\
         fn main() {\n\
         \x20 let rs = await task.gather([double(1), double(2), double(3)])\n\
         \x20 print(rs)\n\
         }\n\
         await main()\n",
    );
    let out_path = dir.join("worker_rt");

    let build_out = Command::new(toolchain_bin())
        .args(["build", "--native"])
        .arg(&src)
        .arg("--stub")
        .arg(&rt)
        .arg("-o")
        .arg(&out_path)
        .output()
        .unwrap();
    assert!(
        build_out.status.success(),
        "worker --stub build failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr)
    );

    let r = run_ref(&src, &[]);
    assert!(
        r.status.success(),
        "worker reference run failed: stderr={}",
        String::from_utf8_lossy(&r.stderr)
    );

    let empty_cwd = tmp_dir("rt_bundle_worker_cwd");
    let b = Command::new(&out_path)
        .current_dir(&empty_cwd)
        .env("PATH", "")
        .output()
        .unwrap();
    assert_eq!(b.stdout, r.stdout, "worker rt bundle stdout differs from ascript run");
    assert_eq!(b.status.code(), r.status.code(), "worker rt bundle exit differs");
}

/// RT §10.2 / IGNORE (Task 7) — caps are enforced: a bundle built with `--deny net` denies net
/// at runtime, and `ASCRIPT_DENY=fs` further restricts an unrestricted bundle at launch.
#[test]
#[ignore = "unblocked by Task 7 (--stub wiring)"]
fn stub_bundle_caps_floor_and_ascript_deny() {
    let rt = match rt_bin() {
        Some(b) => b,
        None => return,
    };
    let dir = tmp_dir("rt_bundle_caps");
    let src = write(
        &dir,
        "netprog.as",
        "import * as net from \"std/net\"\n\
         let r = net.lookup(\"example.com\")\n\
         print(\"reached-net\")\n",
    );
    let out_path = dir.join("net_denied_rt");

    // Build with --deny net onto the rt stub.
    let build_out = Command::new(toolchain_bin())
        .args(["build", "--native", "--deny", "net"])
        .arg(&src)
        .arg("--stub")
        .arg(&rt)
        .arg("-o")
        .arg(&out_path)
        .output()
        .unwrap();
    assert!(
        build_out.status.success(),
        "--deny net --stub build failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr)
    );

    let empty_cwd = tmp_dir("rt_bundle_caps_cwd");
    let b = Command::new(&out_path)
        .current_dir(&empty_cwd)
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!b.status.success(), "net-denied rt bundle must fail at the net call");
    let stderr = String::from_utf8_lossy(&b.stderr);
    assert!(
        stderr.contains("capability 'net' denied"),
        "expected capability denial message; got stderr: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&b.stdout);
    assert!(
        !stdout.contains("reached-net"),
        "net-denied rt bundle must NOT reach the net call: {stdout}"
    );

    // ASCRIPT_DENY=fs further restricts an all-granted bundle.
    let fs_src = write(
        &dir,
        "fsprog.as",
        "import * as fs from \"std/fs\"\n\
         let _ = fs.readFile(\"no_such_file\")\n\
         print(\"reached-fs\")\n",
    );
    let fs_out = dir.join("fs_denied_rt");
    let _ = Command::new(toolchain_bin())
        .args(["build", "--native"])
        .arg(&fs_src)
        .arg("--stub")
        .arg(&rt)
        .arg("-o")
        .arg(&fs_out)
        .output();

    let b2 = Command::new(&fs_out)
        .current_dir(&empty_cwd)
        .env("PATH", "")
        .env("ASCRIPT_DENY", "fs")
        .output()
        .unwrap();
    assert!(!b2.status.success(), "ASCRIPT_DENY=fs must deny the fs call");
    let stderr2 = String::from_utf8_lossy(&b2.stderr);
    assert!(
        stderr2.contains("capability 'fs' denied"),
        "expected fs capability denial message; got: {stderr2}"
    );
}

/// RT §10.2 / IGNORE (Task 7) — when a bundle is missing a module (compiled WITHOUT the file),
/// running it produces the exact §2.3(a) compile-refusal error naming the toolchain.
///
/// Expected stderr contains:
///   - "cannot compile module"
///   - "this runtime has no compiler"
///   - "rebuild with the ascript toolchain"
#[test]
#[ignore = "unblocked by Task 7 (--stub wiring)"]
fn stub_missing_module_error_names_the_toolchain() {
    let rt = match rt_bin() {
        Some(b) => b,
        None => return,
    };
    let dir = tmp_dir("rt_bundle_missing_mod");
    // A program that imports a sibling module.
    write(
        &dir,
        "helper.as",
        "export fn noop(): number { return 42 }\n",
    );
    let entry = write(
        &dir,
        "app.as",
        "import { noop } from \"./helper\"\nprint(noop())\n",
    );
    let out_path = dir.join("missing_mod_rt");

    // Build (embeds helper.as in the archive normally).
    let build_out = Command::new(toolchain_bin())
        .args(["build", "--native"])
        .arg(&entry)
        .arg("--stub")
        .arg(&rt)
        .arg("-o")
        .arg(&out_path)
        .output()
        .unwrap();
    assert!(
        build_out.status.success(),
        "missing-mod --stub build failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr)
    );

    // NOW place a DIFFERENT helper.as in the cwd so the bundle can't use the embedded one
    // and the rt stub tries to compile it — but has no compiler.
    let run_dir = tmp_dir("rt_bundle_missing_mod_cwd");
    write(
        &run_dir,
        "helper.as",
        "export fn noop(): number { return 99 }\n", // different, triggers re-compile
    );

    let b = Command::new(&out_path)
        .current_dir(&run_dir)
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!b.status.success(), "bundle with missing module must fail");
    let stderr = String::from_utf8_lossy(&b.stderr);
    assert!(
        stderr.contains("cannot compile module"),
        "expected 'cannot compile module' in stderr; got: {stderr}"
    );
    assert!(
        stderr.contains("this runtime has no compiler"),
        "expected 'this runtime has no compiler' in stderr; got: {stderr}"
    );
    assert!(
        stderr.contains("rebuild with the ascript toolchain"),
        "expected 'rebuild with the ascript toolchain' in stderr; got: {stderr}"
    );
}

/// RT §10.2 / IGNORE (Task 7) — a panicking program bundled with a debug section renders the
/// source caret; `build --strip` produces a message-only diagnostic (no source lines).
#[test]
#[ignore = "unblocked by Task 7 (--stub wiring)"]
fn stub_panic_diagnostics_render_from_embedded_source() {
    let rt = match rt_bin() {
        Some(b) => b,
        None => return,
    };
    let dir = tmp_dir("rt_bundle_diag");
    // A program that always panics.
    let src = write(
        &dir,
        "panic_prog.as",
        "fn bad(): number { return nil + 1 }\nbad()\n",
    );
    let out_debug = dir.join("panic_debug_rt");
    let out_stripped = dir.join("panic_strip_rt");

    // Build WITH debug section (default).
    let bd = Command::new(toolchain_bin())
        .args(["build", "--native"])
        .arg(&src)
        .arg("--stub")
        .arg(&rt)
        .arg("-o")
        .arg(&out_debug)
        .output()
        .unwrap();
    assert!(
        bd.status.success(),
        "debug --stub build failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&bd.stdout),
        String::from_utf8_lossy(&bd.stderr)
    );

    // Build WITH --strip.
    let bs = Command::new(toolchain_bin())
        .args(["build", "--native", "--strip"])
        .arg(&src)
        .arg("--stub")
        .arg(&rt)
        .arg("-o")
        .arg(&out_stripped)
        .output()
        .unwrap();
    assert!(
        bs.status.success(),
        "strip --stub build failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&bs.stdout),
        String::from_utf8_lossy(&bs.stderr)
    );

    let empty_cwd = tmp_dir("rt_bundle_diag_cwd");

    // Debug run: stderr must contain source context (a caret or the source line).
    let b_debug = Command::new(&out_debug)
        .current_dir(&empty_cwd)
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!b_debug.status.success(), "panicking program must exit non-zero");
    let debug_stderr = String::from_utf8_lossy(&b_debug.stderr);
    assert!(
        debug_stderr.contains('^') || debug_stderr.contains("nil + 1"),
        "debug build must render source context (caret or source line); stderr:\n{debug_stderr}"
    );

    // Stripped run: stderr must contain the panic message but no source line/caret.
    let b_strip = Command::new(&out_stripped)
        .current_dir(&empty_cwd)
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!b_strip.status.success(), "panicking stripped program must exit non-zero");
    let strip_stderr = String::from_utf8_lossy(&b_strip.stderr);
    assert!(
        !strip_stderr.contains('^'),
        "stripped build must NOT render a caret; stderr:\n{strip_stderr}"
    );
    // The error message itself must still appear.
    assert!(
        !strip_stderr.trim().is_empty(),
        "stripped build must still write a panic message to stderr"
    );
}
