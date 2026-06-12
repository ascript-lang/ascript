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
use std::sync::Mutex;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ascript")
}

/// Serializes the disk-heavy native-bundle tests. Each bundle is the WHOLE runtime (~123 MB)
/// written to a temp dir, and it lingers for the test's run/assert phase before its [`TmpDir`]
/// guard cleans it up. Under default parallelism several such bundles coexist, so on a
/// space-constrained volume the peak overruns free space ("No space left on device"). A test
/// that builds a bundle takes this guard FIRST (see [`serial_native`]) so at most ONE bundle
/// exists at a time (the single-threaded peak, which fits); paired with [`TmpDir`] cleanup
/// (which frees each bundle before the next), the whole file stays within a single bundle's
/// footprint regardless of `--test-threads`.
static BUILD_LOCK: Mutex<()> = Mutex::new(());

/// Acquire the [`BUILD_LOCK`] for the lifetime of a native-bundle test. Bind it as the FIRST
/// statement of any test that calls [`build_native`] (`let _serial = serial_native();`). Held
/// across the whole test (not just the build) because the 123 MB bundle persists until the
/// test's `TmpDir` drops; releasing after the build alone would let bundles pile up. A poisoned
/// lock (a panicking test) is recovered — serialization, not data, is what it guards.
fn serial_native() -> std::sync::MutexGuard<'static, ()> {
    BUILD_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A unique temp dir for one test, removed when the returned guard drops (so bundles — each a
/// full runtime copy — do not accumulate in the tmpfs across the run). Derefs to `&Path`, so
/// call sites use it exactly like a `PathBuf` (`dir.join(...)`, `&dir` into `&Path` params).
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

/// A unique temp dir for one test (avoids cross-test collisions in parallel runs). The tag
/// must be unique per test; the PID disambiguates concurrent test-runner processes.
fn tmp_dir(tag: &str) -> TmpDir {
    let d = std::env::temp_dir().join(format!("ascript_native_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    TmpDir(d)
}

/// `ascript build --native <src> -o <out>` — asserts success. The CALLER must hold
/// [`serial_native`] (so concurrent builds don't overrun a space-constrained volume); this
/// function does not lock itself, because some tests build twice and a self-lock would deadlock.
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
    run_bundle_env(bundle, cwd, args, &[])
}

/// Like [`run_bundle`] but with extra environment variables (e.g. `ASCRIPT_WORKERS`).
fn run_bundle_env(bundle: &Path, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(bundle);
    cmd.args(args)
        .current_dir(cwd)
        .env("PATH", ""); // genuinely scrubbed — nothing to fall back to
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
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

/// Reference: `ascript run <src> [args]` with `cwd` set to `dir`. Needed when a worker
/// isolate re-runs the program's relative imports — they resolve against the cwd, so the
/// source reference must run from the module dir to match the bundled (archive-carrying) run.
fn run_ref_in(src: &Path, dir: &Path, args: &[&str]) -> Output {
    run_ref_in_env(src, dir, args, &[])
}

/// Like [`run_ref_in`] but with extra environment variables (e.g. `ASCRIPT_WORKERS`).
fn run_ref_in_env(src: &Path, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(bin());
    cmd.arg("run").arg(src).args(args).current_dir(dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
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
    let _serial = serial_native();
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
    let _serial = serial_native();
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
    let _serial = serial_native();
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
    let _serial = serial_native();
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
    let _serial = serial_native();
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

/// N5 — once the `ASCRIPTB` magic is confirmed, an embedded-payload READ failure must be a
/// REPORTED error (exit 1 with a clear message), NOT a silent fall-through to clap's confusing
/// "missing subcommand" usage error. We can't easily inject an EINTR mid-read, so we exercise
/// the closest observable: a bundle whose footer passes `validate_footer` but whose payload is
/// garbage `from_bytes_verified` rejects — the binary must emit a "cannot load" error and the
/// program must NOT run, and the error must NOT be a clap usage error. The actual I/O-error
/// branch (e.g. EINTR on `read_exact`) isn't unit-testable without fault injection; this test
/// exercises the closest observable proxy.
#[test]
fn native_confirmed_bundle_reports_load_error_not_clap() {
    let _serial = serial_native();
    let dir = tmp_dir("n5");
    let src = write(&dir, "p.as", "print(\"should not run\")\n");
    let app = dir.join("n5_app");
    build_native(&src, &app);

    // Replace the WHOLE payload region with garbage that still satisfies the footer bounds
    // (same length), so `validate_footer` passes but `from_bytes_verified` rejects.
    let off = footer_payload_offset(&app) as usize;
    let mut bytes = std::fs::read(&app).unwrap();
    let payload_end = bytes.len() - 32;
    for b in &mut bytes[off..payload_end] {
        *b = 0x00;
    }
    std::fs::write(&app, &bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let empty = tmp_dir("n5_cwd");
    let out = run_bundle(&app, &empty, &[]);
    assert!(!out.status.success(), "confirmed-bundle load failure must be non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot load") || stderr.contains("failed to read embedded program"),
        "expected a reported embedded-load error, NOT a clap usage error, got: {stderr}"
    );
    assert!(
        !stderr.contains("Usage:") && !stderr.contains("subcommand"),
        "a confirmed bundle must NOT fall through to clap, got: {stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("should not run"),
        "the program must NOT execute"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// N6 — building a native bundle whose STUB is itself already a bundle (a double-bundle) must
/// strip the existing overlay first, so the output is NOT double-sized and still runs. We can't
/// invoke a bundled `ascript` as a builder (it runs its embedded program), so we synthesize the
/// scenario at the codec level via the public `bundle` API: a synthetic `stub || payload ||
/// footer` is parsed by `read_bundle_footer`, and the recovered prefix is exactly the clean stub
/// (offset 0..payload_offset) with NO trailing footer. This is the exact slice `build_native`
/// uses to strip a double-bundle.
#[test]
fn native_double_bundle_strip_recovers_clean_stub() {
    use ascript::bundle::{read_bundle_footer, write_footer, FOOTER_SIZE, MIN_STUB_SIZE};

    let clean_stub: Vec<u8> = (0..(MIN_STUB_SIZE as usize + 256))
        .map(|i| (i % 251) as u8)
        .collect();
    let payload = b"embedded .aso payload";

    // Build a synthetic bundle: clean_stub || payload || footer.
    let mut bundle = clean_stub.clone();
    bundle.extend_from_slice(payload);
    bundle.extend_from_slice(&write_footer(
        clean_stub.len() as u64,
        payload.len() as u64,
        26,
    ));

    // The strip logic: read the footer, take everything before payload_offset.
    let (offset, _len) = read_bundle_footer(&bundle).expect("synthetic bundle is valid");
    let stripped = &bundle[..offset];

    assert_eq!(stripped, clean_stub.as_slice(), "strip must recover the clean stub bytes");
    // The recovered stub is itself a CLEAN runtime: no trailing footer.
    assert!(
        read_bundle_footer(stripped).is_none(),
        "the stripped stub must NOT carry a bundle footer"
    );
    // And it is exactly `bundle.len() - payload - footer` — not double-sized.
    assert_eq!(stripped.len(), bundle.len() - payload.len() - FOOTER_SIZE);
}

/// N6 (end-to-end) — a REAL bundle, when its overlay is stripped via the same `read_bundle_footer`
/// slice `build_native` uses, recovers a footer-free runtime byte-for-byte equal to the original
/// clean `ascript` binary. This proves the strip a double-bundle build performs yields a clean
/// stub identical to building from a never-bundled `ascript` (so the second build is not
/// double-sized).
#[test]
fn native_real_bundle_strips_back_to_clean_runtime() {
    let _serial = serial_native();
    use ascript::bundle::read_bundle_footer;

    let dir = tmp_dir("n6e2e");
    let src = write(&dir, "p.as", "print(\"x\")\n");
    let app = dir.join("n6e2e_app");
    build_native(&src, &app);

    let bundle = std::fs::read(&app).unwrap();
    let (offset, _len) = read_bundle_footer(&bundle).expect("a real bundle must carry a footer");
    let stripped = &bundle[..offset];

    // The recovered stub is footer-free (a clean runtime).
    assert!(
        read_bundle_footer(stripped).is_none(),
        "the stripped real bundle must NOT carry a footer"
    );

    // On macOS the stub is re-signed during the build (so its `__LINKEDIT` differs from the
    // original `ascript` on disk); on other Unix the clean stub equals the original runtime.
    // Either way the strip recovers a SMALLER, single-payload, footer-free image — assert the
    // size relationship that guarantees "not double-sized".
    let original = std::fs::read(bin()).unwrap();
    assert!(
        stripped.len() >= original.len() / 2,
        "stripped stub should be ~runtime-sized, got {} vs original {}",
        stripped.len(),
        original.len()
    );
    assert!(
        stripped.len() < bundle.len(),
        "stripped stub must be strictly smaller than the full bundle"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// N7 — the output binary is produced via temp-then-rename: on SUCCESS no `*.tmp` sibling is
/// left behind and the final binary runs. (A TOCTOU race is impractical to simulate; we assert
/// the atomic-output contract: a clean final artifact and no leftover temp.)
#[test]
fn native_output_is_atomic_no_temp_leftover() {
    let _serial = serial_native();
    let dir = tmp_dir("n7");
    let src = write(&dir, "p.as", "print(\"atomic\")\n");
    let app = dir.join("n7_app");
    build_native(&src, &app);

    // The final binary runs.
    let empty = tmp_dir("n7_cwd");
    let out = run_bundle(&app, &empty, &[]);
    assert!(out.status.success(), "atomic-built bundle must run: {:?}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "atomic\n");

    // No leftover temp file in the output directory.
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp") || n.ends_with("~"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no temp/partial output must remain after a successful build, found: {leftovers:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// SELF-CONTAINED-BUNDLES (Task 1.5) — a MULTI-module `build --native` embeds the whole import
/// graph as an `ASCRIPTA` archive in the payload; the produced binary runs from an EMPTY cwd
/// (no `.as` sources anywhere) and prints the right output. This proves build-time graph
/// embedding + run-time archive decode/install across the native-bundle boundary.
#[test]
fn native_multimodule_bundle_runs_from_empty_dir() {
    let _serial = serial_native();
    let dir = tmp_dir("mm_native");
    // Two sibling modules; the entry imports the util by a relative specifier.
    write(
        &dir,
        "util.as",
        "export fn greet(name: string): string { return `Hello, ${name}!` }\n",
    );
    let entry = write(
        &dir,
        "app.as",
        "import { greet } from \"./util\"\nprint(greet(\"bundle\"))\n",
    );
    let app = dir.join("mm_app");
    build_native(&entry, &app);

    // Run from an EMPTY dir with a scrubbed PATH — nothing on disk to fall back to.
    let empty = tmp_dir("mm_native_cwd");
    let b = run_bundle(&app, &empty, &[]);
    assert!(
        b.status.success(),
        "multi-module native bundle failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&b.stdout),
        String::from_utf8_lossy(&b.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&b.stdout),
        "Hello, bundle!\n",
        "the bundled multi-module program must produce the imported function's output"
    );
    // Equivalence with `ascript run` of the source.
    let r = run_ref(&entry, &[]);
    assert_eq!(b.stdout, r.stdout, "native multimodule stdout differs from source run");
    assert_eq!(b.status.code(), r.status.code());

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// SELF-CONTAINED-BUNDLES (Task 1.6) — HEADLINE: a POOLED `worker fn` in a bundled
/// MULTI-module program calls into an IMPORTED module. The worker isolate builds a fresh,
/// archive-less `Vm` and re-runs the program's top-level imports on itself, so without the
/// archive shipped to the isolate the re-run `Op::Import` finds no archive and no source
/// tree on disk → the worker fails. Shipping + installing the whole `ModuleArchive` on each
/// isolate before any user worker code runs fixes it. Bundled, run from an EMPTY cwd.
#[test]
fn native_worker_imports_module_from_bundle() {
    let _serial = serial_native();
    let dir = tmp_dir("mm_worker_native");
    write(
        &dir,
        "util.as",
        "export fn dbl(n: number): number { return n * 2 }\n",
    );
    let entry = write(
        &dir,
        "app.as",
        "import { dbl } from \"./util\"\n\
         import * as task from \"std/task\"\n\
         worker fn w(n: number): number { return dbl(n) }\n\
         fn main() {\n\
         \x20 let rs = await task.gather([w(1), w(2), w(3), w(4)])\n\
         \x20 print(rs)\n\
         }\n\
         await main()\n",
    );
    let app = dir.join("mm_worker_app");
    build_native(&entry, &app);

    // Reference: `ascript run` of the source FROM the module dir (so the worker isolate's
    // re-run imports resolve `./util` on disk — the bundled case must match this output).
    let r = run_ref_in(&entry, &dir, &[]);
    assert!(
        r.status.success(),
        "multimodule-worker reference failed: {:?}",
        String::from_utf8_lossy(&r.stderr)
    );

    // Run from an EMPTY dir with a scrubbed PATH — nothing on disk for the worker isolate's
    // re-run imports to fall back to. The archive must travel to the isolate.
    let empty = tmp_dir("mm_worker_cwd");
    let b = run_bundle(&app, &empty, &[]);
    assert!(
        b.status.success(),
        "bundled worker→imported-module failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&b.stdout),
        String::from_utf8_lossy(&b.stderr)
    );
    assert_eq!(b.stdout, r.stdout, "bundled worker→import stdout differs from source run");
    assert_eq!(b.stderr, r.stderr, "bundled worker→import stderr differs from source run");
    assert_eq!(b.status.code(), r.status.code());

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// WORKER POOL-CACHE OPTIMIZATION — the KEY correctness guard for shipping each worker's
/// slice + the bundled module archive AT MOST ONCE per isolate. A pooled `worker fn` that
/// calls into an IMPORTED module is invoked MANY times (50). Each pool isolate is reused
/// across many of those calls (cap = num_cpus ≪ 50), so calls 2..N to a reused isolate
/// arrive with `slice_bytes`/`archive_bytes` SUPPRESSED by the pool's per-isolate cache
/// mirror. Every result must still be correct — proving the isolate's own cache
/// (`archive_installed` + `loaded`) carries the imports/slice across the cleared requests.
/// Forced bottleneck: `ASCRIPT_WORKERS=2` so 50 calls share just two reused isolates,
/// maximizing the suppressed-bytes path. Bundled, run from an EMPTY cwd.
#[test]
fn native_worker_many_calls_with_suppressed_bytes() {
    let _serial = serial_native();
    let dir = tmp_dir("mm_worker_many_native");
    write(
        &dir,
        "util.as",
        "export fn dbl(n: number): number { return n * 2 }\n",
    );
    let entry = write(
        &dir,
        "app.as",
        "import { dbl } from \"./util\"\n\
         import * as task from \"std/task\"\n\
         worker fn w(n: number): number { return dbl(n) }\n\
         fn main() {\n\
         \x20 let jobs = []\n\
         \x20 for (i in 0..50) { jobs = [...jobs, w(i)] }\n\
         \x20 let rs = await task.gather(jobs)\n\
         \x20 print(rs)\n\
         }\n\
         await main()\n",
    );
    let app = dir.join("mm_worker_many_app");
    build_native(&entry, &app);

    // Force a SMALL pool (2 isolates) so 50 calls heavily reuse isolates — the suppressed
    // re-send path (calls 2..N on a reused isolate) is exercised on both the reference and
    // the bundle. Reference: `ascript run` from the module dir (imports resolve on disk).
    let workers = [("ASCRIPT_WORKERS", "2")];
    let r = run_ref_in_env(&entry, &dir, &[], &workers);
    assert!(
        r.status.success(),
        "many-calls worker reference failed: {:?}",
        String::from_utf8_lossy(&r.stderr)
    );

    // Run from an EMPTY dir with a scrubbed PATH — nothing on disk for the worker isolate's
    // re-run imports. The archive must travel to (and be cached on) each isolate ONCE.
    let empty = tmp_dir("mm_worker_many_cwd");
    let b = run_bundle_env(&app, &empty, &[], &workers);
    assert!(
        b.status.success(),
        "bundled many-calls worker failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&b.stdout),
        String::from_utf8_lossy(&b.stderr)
    );
    assert_eq!(
        b.stdout, r.stdout,
        "bundled many-calls worker stdout differs from source run"
    );
    // Spot-check the actual values: w(i) = i*2 for i in 0..50.
    let expected: Vec<String> = (0..50).map(|i| (i * 2).to_string()).collect();
    let out = String::from_utf8_lossy(&b.stdout);
    assert_eq!(
        out.trim(),
        format!("[{}]", expected.join(", ")),
        "every one of the 50 suppressed-bytes worker calls must return i*2"
    );
    assert_eq!(b.status.code(), r.status.code());

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// SELF-CONTAINED-BUNDLES (Task 1.6) — a DEDICATED `run_in_worker(fn, input, {...})` body in
/// a bundled multi-module program calls an imported binding. The dedicated isolate gets the
/// archive captured into its `Send` `make_loop` closure and installs it before the slice
/// loads. Bundled, run from an EMPTY cwd.
#[test]
fn native_run_in_worker_imports_module_from_bundle() {
    let _serial = serial_native();
    let dir = tmp_dir("mm_riw_native");
    write(
        &dir,
        "util.as",
        "export fn trip(n: number): number { return n * 3 }\n",
    );
    let entry = write(
        &dir,
        "app.as",
        "import { trip } from \"./util\"\n\
         worker fn plugin(n: number): number { return trip(n) }\n\
         fn main() {\n\
         \x20 let r = await run_in_worker(plugin, 7, {caps: {deny: [\"ffi\"]}})\n\
         \x20 print(r)\n\
         }\n\
         await main()\n",
    );
    let app = dir.join("mm_riw_app");
    build_native(&entry, &app);

    let r = run_ref_in(&entry, &dir, &[]);
    assert!(
        r.status.success(),
        "run_in_worker multimodule reference failed: {:?}",
        String::from_utf8_lossy(&r.stderr)
    );

    let empty = tmp_dir("mm_riw_cwd");
    let b = run_bundle(&app, &empty, &[]);
    assert!(
        b.status.success(),
        "bundled run_in_worker→imported-module failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&b.stdout),
        String::from_utf8_lossy(&b.stderr)
    );
    assert_eq!(b.stdout, r.stdout, "bundled run_in_worker→import stdout differs");
    assert_eq!(b.status.code(), r.status.code());

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// SELF-CONTAINED-BUNDLES (Task 1.6) — an ACTOR (`worker class`) method calls an imported
/// fn, AND a streaming `worker fn*` uses an imported fn, in a bundled multi-module program.
/// Both dedicated isolates get the archive captured into their spawn closures and install it
/// before the class/producer slice loads. Bundled, run from an EMPTY cwd.
#[test]
fn native_actor_and_stream_import_module_from_bundle() {
    let _serial = serial_native();
    let dir = tmp_dir("mm_as_native");
    write(
        &dir,
        "util.as",
        "export fn inc(n: number): number { return n + 1 }\n",
    );
    let entry = write(
        &dir,
        "app.as",
        "import { inc } from \"./util\"\n\
         worker class Counter {\n\
         \x20 count: number = 0\n\
         \x20 async fn bump(): number { self.count = inc(self.count); return self.count }\n\
         }\n\
         worker fn* squares(n: number) {\n\
         \x20 let i = 0\n\
         \x20 while (i < n) { yield inc(i * i - 1); i = i + 1 }\n\
         }\n\
         fn main() {\n\
         \x20 let c = await Counter.spawn()\n\
         \x20 let a = await c.bump()\n\
         \x20 let b = await c.bump()\n\
         \x20 print([a, b])\n\
         \x20 let acc = []\n\
         \x20 for await (v in squares(3)) { acc = [...acc, v] }\n\
         \x20 print(acc)\n\
         }\n\
         await main()\n",
    );
    let app = dir.join("mm_as_app");
    build_native(&entry, &app);

    let r = run_ref_in(&entry, &dir, &[]);
    assert!(
        r.status.success(),
        "actor/stream multimodule reference failed: {:?}",
        String::from_utf8_lossy(&r.stderr)
    );

    let empty = tmp_dir("mm_as_cwd");
    let b = run_bundle(&app, &empty, &[]);
    assert!(
        b.status.success(),
        "bundled actor/stream→imported-module failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&b.stdout),
        String::from_utf8_lossy(&b.stderr)
    );
    assert_eq!(b.stdout, r.stdout, "bundled actor/stream→import stdout differs");
    assert_eq!(b.status.code(), r.status.code());

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// SELF-CONTAINED-BUNDLES (Phase 2, validate_into SOUNDNESS) — WORKER PARITY: a `worker fn`
/// whose body calls `Outer.from({inner:{v}})` validates a NESTED-class field whose type
/// (`Inner`) is referenced ONLY as that field type. The worker code-slice's reachability
/// closure shares `collect_def_refs` with the bundle tree-shaker, so the field-type fix keeps
/// `Inner` in the worker fragment too — without it the isolate's `validate_into` would fail
/// with `type contract violated at outer.inner: expected Inner, got object`. Both classes are
/// defined TOP-LEVEL in the entry module (so they ship via the class-slice machinery, not via
/// import re-run), exercising the shared shaker fix on the worker path. Bundled, EMPTY cwd.
#[test]
fn native_worker_returns_class_with_nested_field_from_bundle() {
    let _serial = serial_native();
    let dir = tmp_dir("mm_worker_field_native");
    // A sibling module supplies the worker's input scalar, so this is a genuine multi-module
    // bundle (the worker isolate re-runs the import); the load-bearing classes live in `app`.
    write(
        &dir,
        "util.as",
        "export fn seed(): number { return 5 }\n",
    );
    let entry = write(
        &dir,
        "app.as",
        "import { seed } from \"./util\"\n\
         import * as task from \"std/task\"\n\
         class Inner { v: number }\n\
         class Outer { inner: Inner }\n\
         worker fn build(n: number): number {\n\
         \x20 let o = Outer.from({ inner: { v: n } })\n\
         \x20 return o.inner.v\n\
         }\n\
         fn main() {\n\
         \x20 let r = await task.gather([build(seed())])\n\
         \x20 print(r)\n\
         }\n\
         await main()\n",
    );
    let app = dir.join("mm_worker_field_app");
    build_native(&entry, &app);

    let r = run_ref_in(&entry, &dir, &[]);
    assert!(
        r.status.success(),
        "worker→nested-field reference failed: {:?}",
        String::from_utf8_lossy(&r.stderr)
    );

    let empty = tmp_dir("mm_worker_field_cwd");
    let b = run_bundle(&app, &empty, &[]);
    assert!(
        b.status.success(),
        "bundled worker→nested-field validate_into failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&b.stdout),
        String::from_utf8_lossy(&b.stderr)
    );
    assert_eq!(
        b.stdout, r.stdout,
        "bundled worker→nested-field stdout differs from source run"
    );
    assert_eq!(b.status.code(), r.status.code());

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

// ── SELF-CONTAINED-BUNDLES Task 3.1 — embed the composed CapSet in the archive ──────────────

use ascript::stdlib::caps::{Cap, CapSet};
use ascript::vm::archive::ModuleArchive;

/// `ascript build <ARGS> <src> -o <out>` (arbitrary extra flags, e.g. `--native --deny net`).
/// Asserts success. The CALLER must hold [`serial_native`] when building a `--native` bundle.
fn build_with(extra: &[&str], src: &Path, out: &Path) {
    let o = Command::new(bin())
        .arg("build")
        .args(extra)
        .arg(src)
        .arg("-o")
        .arg(out)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "build {:?} failed: stdout={:?} stderr={:?}",
        extra,
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(out.exists(), "artifact not written to {}", out.display());
}

/// Write a 2-module program (entry imports a sibling util) so `build` produces an `ASCRIPTA`
/// archive (a single-module program would emit a bare `ASO\0` chunk with no manifest). Returns
/// the entry path.
fn write_multimodule(dir: &Path) -> PathBuf {
    write(
        dir,
        "util.as",
        "export fn greet(name: string): string { return `Hi, ${name}!` }\n",
    );
    write(
        dir,
        "app.as",
        "import { greet } from \"./util\"\nprint(greet(\"caps\"))\n",
    )
}

/// Decode the `ModuleArchive` embedded in a `--native` bundle: read the footer to find the
/// payload region, slice it, and `ModuleArchive::decode`. The payload is the EXACT archive bytes
/// a plain `build` writes (BIN: bundling, not AOT), so the same decode applies to both.
fn decode_bundle_archive(bundle: &Path) -> ModuleArchive {
    let bytes = std::fs::read(bundle).unwrap();
    assert!(bytes.len() >= 32, "bundle too small for a footer");
    let footer = &bytes[bytes.len() - 32..];
    assert_eq!(&footer[24..32], b"ASCRIPTB", "footer magic not at the tail");
    let off = u64::from_le_bytes(footer[0..8].try_into().unwrap()) as usize;
    let len = u64::from_le_bytes(footer[8..16].try_into().unwrap()) as usize;
    // Bounds-check the raw footer values before slicing so a malformed footer (or a
    // future build regression) is a CLEAN test failure, not an opaque OOB panic. The
    // payload region must lie strictly before the trailing footer.
    assert!(
        off.checked_add(len)
            .is_some_and(|end| end <= bytes.len().saturating_sub(ascript::bundle::FOOTER_SIZE)),
        "footer payload region [{off}..{off}+{len}] out of bounds for image size {}",
        bytes.len()
    );
    let payload = &bytes[off..off + len];
    ModuleArchive::decode(payload).expect("embedded payload decodes as a ModuleArchive")
}

/// Assert a `CapSet` denies exactly `denied` and grants every other cap.
fn assert_only_denied(caps: &CapSet, denied: Cap) {
    for cap in Cap::ALL {
        if cap == denied {
            assert!(!caps.has(cap), "expected {} DENIED", cap.name());
        } else {
            assert!(caps.has(cap), "expected {} granted", cap.name());
        }
    }
}

/// HEADLINE (Task 3.1) — `ascript build --native --deny net <multimodule>` embeds a `CapSet`
/// in the archive manifest with `net` DENIED and every other cap granted.
#[test]
fn native_build_embeds_denied_net_in_archive() {
    let _serial = serial_native();
    let dir = tmp_dir("caps_native_net");
    let entry = write_multimodule(&dir);
    let app = dir.join("caps_net_app");
    build_with(&["--native", "--deny", "net"], &entry, &app);

    let archive = decode_bundle_archive(&app);
    assert_only_denied(&archive.caps, Cap::Net);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Plain `build` (no `--native`) embeds caps IDENTICALLY: `--deny fs` over a multi-module program
/// produces an `.aso` ARCHIVE whose manifest denies `fs`. (Confirms `build` and `--native` agree.)
#[test]
fn plain_build_embeds_denied_fs_in_archive() {
    let dir = tmp_dir("caps_plain_fs");
    let entry = write_multimodule(&dir);
    let out = dir.join("out.aso");
    build_with(&["--deny", "fs"], &entry, &out);

    let bytes = std::fs::read(&out).unwrap();
    let archive = ModuleArchive::decode(&bytes).expect("multi-module build is an ASCRIPTA archive");
    assert_only_denied(&archive.caps, Cap::Fs);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Default — a build with NO cap flags embeds an ALL-GRANTED `CapSet` (the placeholder behavior,
/// now explicit and verified). Plain `build` over a multi-module program.
#[test]
fn build_without_cap_flags_embeds_all_granted() {
    let dir = tmp_dir("caps_default");
    let entry = write_multimodule(&dir);
    let out = dir.join("out.aso");
    build_with(&[], &entry, &out);

    let bytes = std::fs::read(&out).unwrap();
    let archive = ModuleArchive::decode(&bytes).expect("multi-module build is an ASCRIPTA archive");
    assert_eq!(
        archive.caps,
        CapSet::all_granted(),
        "a build with no cap flags must embed an all-granted CapSet"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ── SELF-CONTAINED-BUNDLES Task 3.2 — ENFORCE the embedded CapSet at runtime (N4) ────────────

/// `ascript run <ARGS> <artifact>` (run-time flags before the artifact path), cwd = `dir`.
/// Used to drive a built `.aso`/bundle with run-time `--deny` flags for the composition tests.
fn run_artifact_in(extra: &[&str], artifact: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .arg("run")
        .args(extra)
        .arg(artifact)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

// ── SELF-CONTAINED-BUNDLES Task 3.3 — `ASCRIPT_DENY` monotone launch-time subtraction ─────────
//
// `ASCRIPT_DENY=fs ./app` further restricts an already-built bundle at launch. It can ONLY
// subtract caps (never re-grant); an invalid name is a clean STARTUP error (the program never
// runs). It is the ONLY launch-time restriction knob for a native `./app` (no CLI `--deny`
// reaches the shim, which runs before clap), and it ALSO covers `ascript run x.aso`.

/// Run a native bundle (like [`run_bundle`]) but with `ASCRIPT_DENY` set to `deny` on the
/// spawned process. The shim reads it before clap; argv (`args`) is still forwarded intact.
fn run_bundle_deny(bundle: &Path, cwd: &Path, deny: &str, args: &[&str]) -> Output {
    Command::new(bundle)
        .args(args)
        .current_dir(cwd)
        .env("PATH", "") // genuinely scrubbed — nothing to fall back to
        .env("ASCRIPT_DENY", deny)
        .output()
        .unwrap()
}

/// Run an `.aso`/archive artifact via `ascript run` (like [`run_artifact_in`]) with
/// `ASCRIPT_DENY` set on the spawned process.
#[cfg_attr(not(feature = "sys"), allow(dead_code))]
fn run_artifact_deny(artifact: &Path, dir: &Path, deny: &str, args: &[&str]) -> Output {
    Command::new(bin())
        .arg("run")
        .arg(artifact)
        .args(args)
        .current_dir(dir)
        .env("ASCRIPT_DENY", deny)
        .output()
        .unwrap()
}

/// Write a 2-module program that prints a NON-net line (from a sibling util), then makes a NET
/// call (`net.lookup`) that is NOT recovered — so the line proves a non-net path runs while the
/// net call's verdict decides exit/stderr. Returns the entry path.
fn write_multimodule_net(dir: &Path) -> PathBuf {
    write(
        dir,
        "util.as",
        "export fn banner(): string { return \"non-net-ok\" }\n",
    );
    write(
        dir,
        "netapp.as",
        "import { banner } from \"./util\"\n\
         import * as net from \"std/net\"\n\
         print(banner())\n\
         let r = net.lookup(\"example.com\")\n\
         print(\"reached-net\")\n",
    )
}

/// THE N4 FIX (test 1) — a `--native --deny net` bundle ENFORCES the embedded floor at runtime:
/// the net call is denied (non-zero exit, `capability 'net' denied` on stderr) while the non-net
/// line still prints. Built WITHOUT `--deny`, the SAME program reaches the net call — proving
/// enforcement (not some build artifact difference) is the sole cause.
#[test]
fn native_deny_net_enforced_at_runtime() {
    let _serial = serial_native();
    let dir = tmp_dir("enforce_net");
    let entry = write_multimodule_net(&dir);
    let empty = tmp_dir("enforce_net_cwd");

    // (a) Built WITH --deny net → net denied at runtime.
    let denied = dir.join("denied_app");
    build_with(&["--native", "--deny", "net"], &entry, &denied);
    let b = run_bundle(&denied, &empty, &[]);
    assert!(!b.status.success(), "a --deny net bundle must fail at the net call");
    let out = String::from_utf8_lossy(&b.stdout);
    assert!(out.contains("non-net-ok"), "the non-net line must still print: {out}");
    assert!(!out.contains("reached-net"), "the net call must NOT proceed: {out}");
    let err = String::from_utf8_lossy(&b.stderr);
    assert!(err.contains("capability 'net' denied"), "denial message expected: {err}");

    // (b) Built WITHOUT --deny → the SAME program reaches the net call (no denial). The lookup
    // itself may or may not resolve under a scrubbed env, but it is NOT a capability denial.
    let granted = dir.join("granted_app");
    build_native(&entry, &granted);
    let g = run_bundle(&granted, &empty, &[]);
    let gerr = String::from_utf8_lossy(&g.stderr);
    assert!(
        !gerr.contains("capability 'net' denied"),
        "an unrestricted build must NOT deny net: {gerr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// GRANTED still works (test 2) — a multi-module `--native` bundle built with NO cap flags runs
/// the net path with all caps granted (no capability denial). Pairs with test 1(b) but asserts a
/// FULL multi-module bundle is unrestricted by default.
#[test]
fn native_no_cap_flags_grants_net() {
    let _serial = serial_native();
    let dir = tmp_dir("grant_net");
    let entry = write_multimodule_net(&dir);
    let empty = tmp_dir("grant_net_cwd");

    let app = dir.join("grant_app");
    build_native(&entry, &app);
    let b = run_bundle(&app, &empty, &[]);
    let out = String::from_utf8_lossy(&b.stdout);
    let err = String::from_utf8_lossy(&b.stderr);
    assert!(out.contains("non-net-ok"), "non-net line prints: {out}");
    assert!(
        !err.contains("capability 'net' denied"),
        "all-granted bundle must not deny net: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// SINGLE-MODULE restricted now enforces (test 3, validates the artifact-format rule) — a
/// `--native --deny net` build of a SINGLE-module program now emits an `ASCRIPTA` archive
/// payload (not a bare `ASO\0` chunk), and the bundle denies net at runtime.
#[test]
fn native_single_module_deny_net_is_archive_and_enforces() {
    let _serial = serial_native();
    let dir = tmp_dir("single_net");
    // A SINGLE-module program (no imports) that makes a net call.
    let entry = write(
        &dir,
        "single.as",
        "import * as net from \"std/net\"\n\
         print(\"single-start\")\n\
         let r = net.lookup(\"example.com\")\n\
         print(\"single-reached-net\")\n",
    );
    let empty = tmp_dir("single_net_cwd");

    let app = dir.join("single_app");
    build_with(&["--native", "--deny", "net"], &entry, &app);

    // The payload is now an ASCRIPTA archive (the caps floor has a home) — decode confirms it,
    // with net denied in the embedded manifest.
    let archive = decode_bundle_archive(&app);
    assert_only_denied(&archive.caps, Cap::Net);
    assert_eq!(archive.modules.len(), 1, "a single-module archive carries exactly one module");

    // …and it ENFORCES at runtime.
    let b = run_bundle(&app, &empty, &[]);
    assert!(!b.status.success(), "single-module --deny net bundle must deny net");
    let err = String::from_utf8_lossy(&b.stderr);
    assert!(err.contains("capability 'net' denied"), "denial message expected: {err}");
    let out = String::from_utf8_lossy(&b.stdout);
    assert!(out.contains("single-start") && !out.contains("single-reached-net"), "{out}");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// COMPOSITION / NO RE-GRANT (test 4) — an `.aso` built `--deny net`, then `run` with a run-time
/// `--deny fs`, denies BOTH net AND fs (monotone intersection). And run with NO run-time flag,
/// net is STILL denied (the embedded floor cannot be re-granted). Uses the non-native `.aso`
/// path (cheap, no full-runtime bundle) — the same `run_verified_archive` composition seam.
#[test]
fn aso_caps_compose_and_never_regrant() {
    let dir = tmp_dir("compose");
    // A program that (1) makes an fs call and (2) a net call, neither recovered, with markers so
    // we can tell WHICH cap denied. fs is checked first in source order.
    write(&dir, "cmput.as", "export fn tag(): string { return \"util\" }\n");
    let entry = write(
        &dir,
        "compose.as",
        "import { tag } from \"./cmput\"\n\
         import * as net from \"std/net\"\n\
         print(`start-${tag()}`)\n\
         let n = net.lookup(\"example.com\")\n\
         print(\"reached\")\n",
    );
    let out = dir.join("compose.aso");
    // Build with the embedded floor denying net (multi-module → ASCRIPTA).
    build_with(&["--deny", "net"], &entry, &out);
    let arch = ModuleArchive::decode(&std::fs::read(&out).unwrap()).unwrap();
    assert_only_denied(&arch.caps, Cap::Net);

    // (a) No run-time flag → net STILL denied (the floor is enforced, not re-granted).
    let b = run_artifact_in(&[], &out, &dir, &[]);
    assert!(!b.status.success(), "embedded net-deny must be enforced with no run-time flag");
    let berr = String::from_utf8_lossy(&b.stderr);
    assert!(berr.contains("capability 'net' denied"), "net denied by the floor: {berr}");

    // (b) Run-time --deny fs → BOTH net and fs denied. We assert the EFFECTIVE set via a probe
    // program built into the same archive shape that prints caps.list(). Reuse the same artifact:
    // run it with --deny fs; the program hits the net call first → still net denied, proving the
    // floor survives AND the run-time flag composed (it cannot loosen net). The fs denial is
    // proven by a dedicated caps.list probe below.
    let probe = write(
        &dir,
        "probe.as",
        "import { tag } from \"./cmput\"\n\
         import * as caps from \"std/caps\"\n\
         print(tag())\n\
         print(caps.list())\n",
    );
    let pout = dir.join("probe.aso");
    build_with(&["--deny", "net"], &probe, &pout);
    // Run-time also denies fs → the effective list must exclude BOTH net and fs.
    let p = run_artifact_in(&["--deny", "fs"], &pout, &dir, &[]);
    assert!(p.status.success(), "probe should run: {}", String::from_utf8_lossy(&p.stderr));
    let plist = String::from_utf8_lossy(&p.stdout);
    assert!(
        !plist.contains("net") && !plist.contains("fs"),
        "effective caps must deny BOTH net (floor) and fs (run-time): {plist}"
    );
    assert!(
        plist.contains("process") && plist.contains("ffi") && plist.contains("env"),
        "the other caps stay granted: {plist}"
    );

    // (c) No run-time flag → the probe lists net DENIED but fs GRANTED (only the floor applies).
    let p2 = run_artifact_in(&[], &pout, &dir, &[]);
    let plist2 = String::from_utf8_lossy(&p2.stdout);
    assert!(!plist2.contains("net"), "net denied by the floor: {plist2}");
    assert!(plist2.contains("fs"), "fs still granted with no run-time flag: {plist2}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// UNRESTRICTED single-module stays a bare `ASO\0` chunk (test 5) — `build <single>.as -o out.aso`
/// with NO cap flags is byte-identical to today: a bare chunk, NOT an archive. (The default path
/// is preserved → four-mode differential unaffected.)
#[test]
fn unrestricted_single_module_stays_bare_chunk() {
    let dir = tmp_dir("bare_chunk");
    let entry = write(&dir, "single.as", "print(6 * 7)\n");
    let out = dir.join("out.aso");
    build_with(&[], &entry, &out);

    let bytes = std::fs::read(&out).unwrap();
    // A bare chunk starts with the ASO magic; it is NOT an ASCRIPTA archive.
    assert!(bytes.starts_with(b"ASO\0"), "unrestricted single-module build must be a bare ASO\\0 chunk");
    assert!(
        ModuleArchive::decode(&bytes).is_err(),
        "a bare chunk must NOT decode as a ModuleArchive (it has no manifest)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Build a 2-module program — `import` + a sibling util to force an `ASCRIPTA` archive — whose
/// entry calls a representative gated `<expr>` (NOT recovered) — to an `.aso` with `--deny <cap>`,
/// run it, and assert the EMBEDDED floor denies that cap at runtime: non-zero exit and the
/// `capability '<cap>' denied` Tier-2 message on stderr, while the pre-call line still prints.
/// Confirms enforcement of the embedded floor on the `.aso` run path (`run_verified_archive`).
#[cfg_attr(not(any(feature = "sys", feature = "ffi")), allow(dead_code))]
fn assert_embedded_denial(dir: &Path, tag: &str, import: &str, expr: &str, cap: &str) {
    write(dir, "audit_util.as", "export fn ok(): string { return \"pre-call-ok\" }\n");
    let src = format!(
        "import {{ ok }} from \"./audit_util\"\n\
         {import}\n\
         print(ok())\n\
         let r = {expr}\n\
         print(\"reached\")\n"
    );
    let entry = write(dir, &format!("audit_{tag}.as"), &src);
    let out = dir.join(format!("audit_{tag}.aso"));
    build_with(&["--deny", cap], &entry, &out);
    // Sanity: the embedded floor denies exactly this cap.
    let arch = ModuleArchive::decode(&std::fs::read(&out).unwrap()).unwrap();
    assert_only_denied(&arch.caps, ascript::stdlib::caps::cap_name(cap).unwrap());

    let r = run_artifact_in(&[], &out, dir, &[]);
    assert!(!r.status.success(), "{tag}: embedded --deny {cap} must deny at runtime");
    let out_s = String::from_utf8_lossy(&r.stdout);
    assert!(out_s.contains("pre-call-ok"), "{tag}: the pre-call line must print: {out_s}");
    assert!(!out_s.contains("reached"), "{tag}: the gated call must NOT proceed: {out_s}");
    let err = String::from_utf8_lossy(&r.stderr);
    assert!(
        err.contains(&format!("capability '{cap}' denied")),
        "{tag}: expected `capability '{cap}' denied`: {err}"
    );
}

/// PER-CAP EMBEDDED-DENIAL AUDIT (3.2 review) — locks in embedded-path parity with
/// `cap_audit`'s in-process matrix: each remaining OS-surface cap, embedded as an `.aso` floor
/// via `--deny <cap>`, is enforced by `run_verified_archive` at runtime. Uses the `.aso` path
/// (not `--native`) to stay cheap/hermetic. `net` is covered by the dedicated bundle tests above;
/// here we audit `fs` / `process` / `env` (the `sys` feature) and `ffi` (the `ffi` feature).
#[test]
#[cfg(any(feature = "sys", feature = "ffi"))]
fn embedded_caps_denied_per_cap_audit() {
    let dir = tmp_dir("embedded_audit");

    // fs → fs.read (the canonical read surface, same as cap_audit).
    #[cfg(feature = "sys")]
    assert_embedded_denial(
        &dir, "fs", "import * as fs from \"std/fs\"", "fs.read(\"/etc/hosts\")", "fs",
    );
    // process → process.run (subprocess spawn).
    #[cfg(feature = "sys")]
    assert_embedded_denial(
        &dir, "proc", "import * as process from \"std/process\"", "process.run(\"true\", [])", "process",
    );
    // env → env.get (env-var read).
    #[cfg(feature = "sys")]
    assert_embedded_denial(
        &dir, "env", "import * as env from \"std/env\"", "env.get(\"PATH\")", "env",
    );
    // ffi → ffi.open (native library load; the gate fires before the open is attempted, so the
    // library name need not exist on the test host).
    #[cfg(feature = "ffi")]
    assert_embedded_denial(
        &dir, "ffi", "import * as ffi from \"std/ffi\"", "ffi.open(\"libm.so.6\")", "ffi",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Write a 2-module program (forces an `ASCRIPTA` archive) that prints `caps.list()` after a
/// non-cap marker line. The effective capability set is OBSERVABLE on stdout — used to prove
/// `ASCRIPT_DENY` subtracts (and never re-grants) without needing any OS-surface feature.
fn write_caps_probe(dir: &Path) -> PathBuf {
    write(dir, "probe_util.as", "export fn tag(): string { return \"probe-ok\" }\n");
    write(
        dir,
        "probe.as",
        "import { tag } from \"./probe_util\"\n\
         import * as caps from \"std/caps\"\n\
         print(tag())\n\
         print(caps.list())\n",
    )
}

/// SUBTRACT FROM ALL-GRANTED (Task 3.3, test 1) — a multi-module bundle built with NO cap flags
/// (all-granted) is FURTHER restricted at launch by `ASCRIPT_DENY=fs ./app`: an `fs.*` call is
/// denied (`capability 'fs' denied`, non-zero exit) while a non-fs path still runs. Proves
/// launch-time subtraction over a bundle that itself granted everything.
#[test]
#[cfg(feature = "sys")]
fn ascript_deny_subtracts_from_all_granted_bundle() {
    let _serial = serial_native();
    let dir = tmp_dir("deny_subtract");
    write(&dir, "subutil.as", "export fn ok(): string { return \"non-fs-ok\" }\n");
    let entry = write(
        &dir,
        "subapp.as",
        "import { ok } from \"./subutil\"\n\
         import * as fs from \"std/fs\"\n\
         print(ok())\n\
         let r = fs.read(\"/etc/hosts\")\n\
         print(\"reached-fs\")\n",
    );
    let empty = tmp_dir("deny_subtract_cwd");

    // Built all-granted (no cap flags).
    let app = dir.join("subtract_app");
    build_native(&entry, &app);

    // Launch with ASCRIPT_DENY=fs → the fs call is denied, the non-fs line still prints.
    let b = run_bundle_deny(&app, &empty, "fs", &[]);
    assert!(!b.status.success(), "ASCRIPT_DENY=fs must deny the fs call");
    let out = String::from_utf8_lossy(&b.stdout);
    assert!(out.contains("non-fs-ok"), "the non-fs line must still print: {out}");
    assert!(!out.contains("reached-fs"), "the fs call must NOT proceed: {out}");
    let err = String::from_utf8_lossy(&b.stderr);
    assert!(err.contains("capability 'fs' denied"), "fs denial expected: {err}");

    // Sanity: the SAME bundle without ASCRIPT_DENY does NOT deny fs (proves the env var is the
    // cause, not a build artifact).
    let g = run_bundle(&app, &empty, &[]);
    let gerr = String::from_utf8_lossy(&g.stderr);
    assert!(
        !gerr.contains("capability 'fs' denied"),
        "an all-granted bundle with no ASCRIPT_DENY must not deny fs: {gerr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// CANNOT RE-GRANT (Task 3.3, test 2) — a bundle built `--deny net` keeps `net` denied no matter
/// what `ASCRIPT_DENY` is: `ASCRIPT_DENY` has NO grant syntax. With `ASCRIPT_DENY=fs` the embedded
/// `net` floor survives (now BOTH net and fs are gone via the `caps.list()` probe); with
/// `ASCRIPT_DENY` UNSET the embedded floor is unchanged (net denied, fs still granted). Uses the
/// CORE `caps.list()` probe so no OS-surface feature is needed.
#[test]
fn ascript_deny_cannot_regrant_embedded_floor() {
    let _serial = serial_native();
    let dir = tmp_dir("deny_no_regrant");
    let entry = write_caps_probe(&dir);
    let empty = tmp_dir("deny_no_regrant_cwd");

    // Built with the embedded floor denying net.
    let app = dir.join("noregrant_app");
    build_with(&["--native", "--deny", "net"], &entry, &app);
    let arch = decode_bundle_archive(&app);
    assert_only_denied(&arch.caps, Cap::Net);

    // (a) ASCRIPT_DENY=fs → net STILL denied (floor cannot be re-granted) AND fs now denied too.
    let b = run_bundle_deny(&app, &empty, "fs", &[]);
    assert!(b.status.success(), "probe should run: {}", String::from_utf8_lossy(&b.stderr));
    let list = String::from_utf8_lossy(&b.stdout);
    assert!(list.contains("probe-ok"), "marker line prints: {list}");
    assert!(
        !list.contains("net") && !list.contains("fs"),
        "net (floor, never re-granted) AND fs (ASCRIPT_DENY) must both be denied: {list}"
    );

    // (b) ASCRIPT_DENY UNSET → embedded floor unchanged: net denied, fs still granted.
    let g = run_bundle(&app, &empty, &[]);
    let glist = String::from_utf8_lossy(&g.stdout);
    assert!(!glist.contains("net"), "net denied by the embedded floor: {glist}");
    assert!(glist.contains("fs"), "fs still granted with no ASCRIPT_DENY: {glist}");

    // (c) ASCRIPT_DENY="" (empty) → no further restriction; floor unchanged (net denied, fs granted).
    let e = run_bundle_deny(&app, &empty, "", &[]);
    let elist = String::from_utf8_lossy(&e.stdout);
    assert!(!elist.contains("net"), "empty ASCRIPT_DENY: net still denied by the floor: {elist}");
    assert!(elist.contains("fs"), "empty ASCRIPT_DENY adds no denial: fs still granted: {elist}");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// STACKING (Task 3.3, test 3) — a bundle built `--deny net`, launched with `ASCRIPT_DENY=fs`,
/// has BOTH net (embedded floor) AND fs (`ASCRIPT_DENY`) denied while the other three caps stay
/// granted. The `caps.list()` probe makes the effective set directly observable.
#[test]
fn ascript_deny_stacks_with_embedded_floor() {
    let _serial = serial_native();
    let dir = tmp_dir("deny_stack");
    let entry = write_caps_probe(&dir);
    let empty = tmp_dir("deny_stack_cwd");

    let app = dir.join("stack_app");
    build_with(&["--native", "--deny", "net"], &entry, &app);

    let b = run_bundle_deny(&app, &empty, "fs", &[]);
    assert!(b.status.success(), "probe should run: {}", String::from_utf8_lossy(&b.stderr));
    let list = String::from_utf8_lossy(&b.stdout);
    assert!(
        !list.contains("net") && !list.contains("fs"),
        "stacking must deny BOTH net (floor) and fs (ASCRIPT_DENY): {list}"
    );
    assert!(
        list.contains("process") && list.contains("ffi") && list.contains("env"),
        "the other three caps stay granted: {list}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// INVALID NAME → STARTUP ERROR (Task 3.3, test 4) — `ASCRIPT_DENY=bogus ./app` is a clean
/// startup error mentioning `bogus` and the expected names; the exit is non-zero and the
/// program's OWN output never appears (it never ran — the `?` aborts before any code runs).
#[test]
fn ascript_deny_invalid_name_is_startup_error() {
    let _serial = serial_native();
    let dir = tmp_dir("deny_invalid");
    // A program whose stdout is a distinctive marker — it must NOT appear (program never runs).
    write(&dir, "invutil.as", "export fn m(): string { return \"PROGRAM-RAN-MARKER\" }\n");
    let entry = write(
        &dir,
        "invapp.as",
        "import { m } from \"./invutil\"\nprint(m())\n",
    );
    let empty = tmp_dir("deny_invalid_cwd");

    let app = dir.join("invalid_app");
    build_native(&entry, &app);

    let b = run_bundle_deny(&app, &empty, "bogus", &[]);
    assert!(!b.status.success(), "an invalid ASCRIPT_DENY name must be a non-zero startup error");
    let out = String::from_utf8_lossy(&b.stdout);
    assert!(
        !out.contains("PROGRAM-RAN-MARKER"),
        "the program must NEVER run on an invalid ASCRIPT_DENY: {out}"
    );
    let err = String::from_utf8_lossy(&b.stderr);
    assert!(err.contains("ASCRIPT_DENY"), "the error must mention ASCRIPT_DENY: {err}");
    assert!(err.contains("bogus"), "the error must name the bad cap 'bogus': {err}");
    assert!(
        err.contains("fs, net, process, ffi, env"),
        "the error must list the expected cap names: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// ARGV PRESERVED (Task 3.3, test 5) — `ASCRIPT_DENY=fs ./app a b` still forwards `a`, `b` to the
/// program. `ASCRIPT_DENY` affects ONLY the `CapSet`; argv is untouched.
#[test]
fn ascript_deny_preserves_argv() {
    let _serial = serial_native();
    let dir = tmp_dir("deny_argv");
    write(&dir, "argutil.as", "export fn n(): string { return \"args\" }\n");
    let entry = write(
        &dir,
        "argapp.as",
        "import { n } from \"./argutil\"\n\
         import * as env from \"std/env\"\n\
         print(n())\n\
         for (a in env.args()) { print(`arg:${a}`) }\n",
    );
    let empty = tmp_dir("deny_argv_cwd");

    let app = dir.join("argv_app");
    build_native(&entry, &app);

    // ASCRIPT_DENY=net (does not touch env.args, which is gated on `env`) so the program runs and
    // prints its argv. `a`, `b` must both reach the program.
    let b = run_bundle_deny(&app, &empty, "net", &["a", "b"]);
    assert!(b.status.success(), "program should run: {}", String::from_utf8_lossy(&b.stderr));
    let out = String::from_utf8_lossy(&b.stdout);
    assert!(out.contains("arg:a"), "argv 'a' must reach the program: {out}");
    assert!(out.contains("arg:b"), "argv 'b' must reach the program: {out}");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// `ascript run x.aso` HONORS ASCRIPT_DENY TOO (Task 3.3, test 6) — the OTHER launch path. An
/// `.aso` built all-granted, run via `ascript run out.aso` with `ASCRIPT_DENY=fs`, denies fs.
/// Proves BOTH cap-install points (`run_verified_archive` here) honor the env var.
#[test]
#[cfg(feature = "sys")]
fn ascript_deny_honored_on_aso_run_path() {
    let dir = tmp_dir("deny_aso_run");
    write(&dir, "rutil.as", "export fn ok(): string { return \"aso-non-fs-ok\" }\n");
    let entry = write(
        &dir,
        "rapp.as",
        "import { ok } from \"./rutil\"\n\
         import * as fs from \"std/fs\"\n\
         print(ok())\n\
         let r = fs.read(\"/etc/hosts\")\n\
         print(\"aso-reached-fs\")\n",
    );
    // Built all-granted (multi-module → ASCRIPTA archive on the .aso path).
    let out = dir.join("run.aso");
    build_with(&[], &entry, &out);

    let b = run_artifact_deny(&out, &dir, "fs", &[]);
    assert!(!b.status.success(), "ASCRIPT_DENY=fs on `ascript run x.aso` must deny fs");
    let so = String::from_utf8_lossy(&b.stdout);
    assert!(so.contains("aso-non-fs-ok"), "the non-fs line must still print: {so}");
    assert!(!so.contains("aso-reached-fs"), "the fs call must NOT proceed: {so}");
    let err = String::from_utf8_lossy(&b.stderr);
    assert!(err.contains("capability 'fs' denied"), "fs denial expected: {err}");

    // Without ASCRIPT_DENY, the same .aso reaches the fs call (no capability denial).
    let g = run_artifact_in(&[], &out, &dir, &[]);
    let gerr = String::from_utf8_lossy(&g.stderr);
    assert!(
        !gerr.contains("capability 'fs' denied"),
        "an all-granted .aso without ASCRIPT_DENY must not deny fs: {gerr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
