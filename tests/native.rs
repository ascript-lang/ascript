//! BIN — end-to-end tests for `ascript build --native` (the self-contained native bundle).
//!
//! Each test builds a real bundle from the cargo-built `ascript` (`CARGO_BIN_EXE_ascript`),
//! runs it with a **scrubbed PATH** and a CWD that has no `.as`/`.aso` (so the bundle is
//! genuinely self-contained — nothing on disk to fall back to), and asserts the bundle's
//! observable behavior (stdout + stderr + exit) matches `ascript run` of the same source.
//!
//! NOTE: a bundle is the WHOLE runtime + the program (tens of MB), so these tests write large
//! temp files; each cleans up. They double as the macOS-arm64 ad-hoc-sign exec smoke (Task 8):
//! on arm64 an unsigned/append-broken Mach-O is `SIGKILL`ed at launch and the run fails.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ascript")
}

/// A unique temp dir for one test (avoids cross-test collisions in parallel runs).
fn tmp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ascript_native_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `ascript build --native <src> -o <out>` — asserts success, returns the bundle path.
fn build_native(src: &Path, out: &Path) {
    let o = Command::new(bin())
        .args(["build", "--native"])
        .arg(src)
        .arg("-o")
        .arg(out)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "build --native failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(out.exists(), "bundle not written to {}", out.display());
}

/// Run a bundle with a SCRUBBED `PATH` (no `ascript` reachable) and `cwd` set to an empty
/// dir, forwarding `args`. The bundle must be entirely self-contained.
fn run_bundle(bundle: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(bundle)
        .args(args)
        .current_dir(cwd)
        .env("PATH", "") // genuinely scrubbed — nothing to fall back to
        .output()
        .unwrap()
}

/// Reference: `ascript run <src> [args]`.
fn run_ref(src: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .arg("run")
        .arg(src)
        .args(args)
        .output()
        .unwrap()
}

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

/// Gate 9 — the bundle's stdout + stderr + exit code all equal `ascript run` of the source,
/// with a scrubbed PATH. Covers a plain program, an argv-reading program, and a
/// stderr-emitting program (a non-vacuous stderr channel).
#[test]
fn native_bundle_equivalence_stdout_stderr_exit() {
    let dir = tmp_dir("equiv");

    // (a) A plain program with deterministic stdout.
    let hello = write(&dir, "hello.as", "print(1 + 2 * 3)\nprint(\"hi\")\n");
    let app = dir.join("hello_app");
    build_native(&hello, &app);
    let empty = tmp_dir("equiv_cwd");
    let b = run_bundle(&app, &empty, &[]);
    let r = run_ref(&hello, &[]);
    assert_eq!(b.stdout, r.stdout, "stdout differs");
    assert_eq!(b.status.code(), r.status.code(), "exit differs");

    // (b) A stderr-emitting program (std/log writes to stderr, deterministically here).
    let logp = write(
        &dir,
        "log.as",
        "import * as log from \"std/log\"\nlog.setLevel(\"info\")\nprint(\"stdout-line\")\nlog.info(\"hello\", {n: 1})\nlog.error(\"boom\", {code: 7})\n",
    );
    let logapp = dir.join("log_app");
    build_native(&logp, &logapp);
    let b = run_bundle(&logapp, &empty, &[]);
    let r = run_ref(&logp, &[]);
    assert_eq!(b.stdout, r.stdout, "log stdout differs");
    assert!(!r.stderr.is_empty(), "the stderr channel must be non-vacuous");
    assert_eq!(b.stderr, r.stderr, "log stderr differs");
    assert_eq!(b.status.code(), r.status.code(), "log exit differs");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// The bundled program receives `env.args()` — `./app a b --c` → `[a, b, --c]`, identical to
/// `ascript run src a b --c`. (`--c` would be a flag to `ascript`; the shim runs BEFORE clap,
/// so it reaches the program.)
#[test]
fn native_bundle_forwards_argv() {
    let dir = tmp_dir("argv");
    let src = write(
        &dir,
        "args.as",
        "import { args } from \"std/env\"\nfor (a of args()) { print(a) }\n",
    );
    let app = dir.join("args_app");
    build_native(&src, &app);
    let empty = tmp_dir("argv_cwd");
    let b = run_bundle(&app, &empty, &["a", "b", "--c"]);
    let r = run_ref(&src, &["a", "b", "--c"]);
    assert_eq!(b.stdout, r.stdout, "argv-forwarded stdout differs");
    assert_eq!(
        String::from_utf8_lossy(&b.stdout),
        "a\nb\n--c\n",
        "the program must see [a, b, --c]"
    );
    assert_eq!(b.status.code(), r.status.code());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// Read the trailing footer's `payload_offset` (first 8 LE bytes of the last 32) so a test
/// can target the embedded `.aso` region precisely.
fn footer_payload_offset(bundle: &Path) -> u64 {
    let bytes = std::fs::read(bundle).unwrap();
    assert!(bytes.len() >= 32);
    let footer = &bytes[bytes.len() - 32..];
    // magic is the last 8 bytes of the footer.
    assert_eq!(&footer[24..32], b"ASCRIPTB", "footer magic not at the tail");
    u64::from_le_bytes(footer[0..8].try_into().unwrap())
}

/// Gate 9 (security) — flipping a byte INSIDE the embedded `.aso` payload makes the binary
/// exit non-zero with a clean load/verify error, NOT a panic/abort/SIGSEGV, NOT silent
/// execution. This is the test that the FUZZ-hardened `from_bytes_verified` is the real gate.
#[test]
fn native_tampered_payload_rejected_cleanly() {
    let dir = tmp_dir("tamper");
    let src = write(&dir, "p.as", "print(\"should not run\")\n");
    let app = dir.join("tamper_app");
    build_native(&src, &app);

    let off = footer_payload_offset(&app) as usize;
    let mut bytes = std::fs::read(&app).unwrap();
    // Flip a byte inside the payload region: [off, len - 32). Aim a few bytes in.
    let target = off + 4;
    assert!(target < bytes.len() - 32, "target lands inside the payload");
    bytes[target] ^= 0xFF;
    std::fs::write(&app, &bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let empty = tmp_dir("tamper_cwd");
    let out = run_bundle(&app, &empty, &[]);
    // Non-zero exit, a CLEAN error (no signal/abort), and the program did NOT run.
    assert!(!out.status.success(), "tampered bundle must not succeed");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert!(
            out.status.signal().is_none(),
            "tampered bundle must not crash with a signal (got {:?})",
            out.status
        );
    }
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("should not run"),
        "the tampered program must NOT execute"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot load"),
        "expected a clean load/verify error, got stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// Gate 9 (security) — corrupting the footer's `payload_offset` to point past EOF makes the
/// footer fail its bounds check, so the binary falls through to the normal `ascript` CLI
/// (clean, no OOB slice / panic). With no subcommand it is a clap usage error (non-zero).
#[test]
fn native_corrupt_footer_offset_falls_through_cleanly() {
    let dir = tmp_dir("footer");
    let src = write(&dir, "p.as", "print(\"x\")\n");
    let app = dir.join("footer_app");
    build_native(&src, &app);

    let mut bytes = std::fs::read(&app).unwrap();
    let len = bytes.len();
    // Overwrite the footer's payload_offset (first 8 bytes of the last 32) with a huge value.
    let off_field = len - 32;
    bytes[off_field..off_field + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    std::fs::write(&app, &bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let empty = tmp_dir("footer_cwd");
    let out = run_bundle(&app, &empty, &[]);
    // It must NOT crash; a bounds-failed footer → fall through to the CLI (usage error here).
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert!(
            out.status.signal().is_none(),
            "a corrupt footer must not crash with a signal (got {:?})",
            out.status
        );
    }
    assert!(!out.status.success(), "no subcommand → non-zero");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// Gate 9 / §7 — a WORKER program bundled and run with a scrubbed PATH produces the same
/// output as `ascript run`. Proves isolates spawn and get their slice from `worker_aso_bytes`
/// in embedded mode (decode-only re-parse of the already-verified payload) — no source, no
/// re-exec, no worker code change.
#[test]
fn native_worker_bundle_parity() {
    let dir = tmp_dir("worker");
    // Use the shipped, deterministic `worker fn` parallel-map+gather example.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/workers_parallel_map.as");
    let r = run_ref(&src, &[]);
    assert!(
        r.status.success(),
        "worker reference failed: {:?}",
        String::from_utf8_lossy(&r.stderr)
    );
    let app = dir.join("worker_app");
    build_native(&src, &app);
    let empty = tmp_dir("worker_cwd");
    let b = run_bundle(&app, &empty, &[]);
    assert_eq!(b.stdout, r.stdout, "worker bundle stdout differs");
    assert_eq!(b.stderr, r.stderr, "worker bundle stderr differs");
    assert_eq!(b.status.code(), r.status.code(), "worker bundle exit differs");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// `--target` is parsed-but-rejected in v1 with a SPECIFIC message naming the triple (not a
/// generic clap error, not a silent ignore — Gate 6/10), and `--target` without `--native`
/// is a usage error.
#[test]
fn native_target_is_rejected_and_requires_native() {
    let dir = tmp_dir("target");
    let src = write(&dir, "p.as", "print(1)\n");

    // --target with --native → specific cross-compile Tier-1 error, exit 1.
    let o = Command::new(bin())
        .args(["build", "--native", "--target", "x86_64-unknown-linux-gnu"])
        .arg(&src)
        .arg("-o")
        .arg(dir.join("x"))
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "cross-compile must exit 1");
    let msg = String::from_utf8_lossy(&o.stderr);
    assert!(
        msg.contains("cross-compilation is not yet supported")
            && msg.contains("x86_64-unknown-linux-gnu"),
        "expected the specific cross-compile error naming the triple, got: {msg}"
    );

    // --target WITHOUT --native → clap usage error (non-zero, not 1).
    let o = Command::new(bin())
        .args(["build", "--target", "x86_64-unknown-linux-gnu"])
        .arg(&src)
        .output()
        .unwrap();
    assert!(!o.status.success(), "--target without --native must fail");
    let _ = std::fs::remove_dir_all(&dir);
}
