//! SP6 package-manager CLI integration tests (hermetic — path + local `file://`
//! git fixtures only; NO network).
//!
//! The headline invariant: a bare-specifier import program runs BYTE-IDENTICAL
//! under `ascript run` (VM) and `ascript run --tree-walker` (the oracle). These
//! spawn the built binary like `tests/cli.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ascript")
}

/// A unique scratch dir under the system tempdir.
fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "pkg-it-{}-{}-{}",
        std::process::id(),
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(dir: &Path, rel: &str, contents: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, contents).unwrap();
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `git` in `dir` with deterministic identity; panics on failure.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run `ascript run [--tree-walker] [--locked] <file>` with an isolated cache.
fn run_in(file: &Path, cache: &Path, tree_walker: bool, locked: bool) -> Output {
    let mut cmd = Command::new(bin());
    cmd.arg("run");
    if tree_walker {
        cmd.arg("--tree-walker");
    }
    if locked {
        cmd.arg("--locked");
    }
    cmd.arg(file);
    cmd.env("ASCRIPT_CACHE", cache);
    cmd.output().expect("spawn ascript")
}

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pkg")
        .join(rel)
}

/// Run `ascript run [--tree-walker] <file>` and capture the output.
fn run_engine(file: &PathBuf, tree_walker: bool) -> Output {
    let mut cmd = Command::new(bin());
    cmd.arg("run");
    if tree_walker {
        cmd.arg("--tree-walker");
    }
    cmd.arg(file);
    cmd.output().expect("spawn ascript")
}

/// Assert the VM and tree-walker produce byte-identical program STDOUT + exit
/// code. (The two engines render error DIAGNOSTICS via different formatters — the
/// VM's ariadne caret vs the tree-walker's plain `error:` — a known, accepted SP1
/// cosmetic asymmetry; the error MESSAGE content is asserted separately. The
/// observable contract for a *running* program — its stdout + exit — is what must
/// be byte-identical.) Returns the VM output for further message assertions.
fn assert_both_engines_identical(file: &PathBuf) -> (Output, Output) {
    let vm = run_engine(file, false);
    let tw = run_engine(file, true);
    assert_eq!(
        vm.stdout, tw.stdout,
        "stdout differs between VM and tree-walker\nVM: {}\nTW: {}",
        String::from_utf8_lossy(&vm.stdout),
        String::from_utf8_lossy(&tw.stdout)
    );
    assert_eq!(
        vm.status.code(),
        tw.status.code(),
        "exit code differs between VM and tree-walker"
    );
    (vm, tw)
}

// Path-dep resolution requires the `pkg` feature (the CLI builds the resolver
// map). Under `--no-default-features` there is no resolver, so a bare import is
// "unknown package" on both engines (that parity is covered by
// `unknown_package_errors_identically_both_engines`, which runs in BOTH configs).
#[cfg(feature = "pkg")]
#[test]
fn path_dep_bare_import_runs_byte_identical_both_engines() {
    let file = fixture("app/main.as");
    let (vm, _tw) = assert_both_engines_identical(&file);
    assert!(
        vm.status.success(),
        "program failed: {}",
        String::from_utf8_lossy(&vm.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&vm.stdout),
        "hello world!\nloud?!\nscoped-x\n"
    );
}

#[test]
fn unknown_package_errors_identically_both_engines() {
    let file = fixture("app/missing.as");
    let (vm, tw) = assert_both_engines_identical(&file);
    assert!(!vm.status.success(), "unknown package must be an error");
    // The error MESSAGE is identical on both engines (the diagnostic FORMATTING
    // differs — accepted SP1 cosmetic asymmetry — so assert the substring on each
    // engine's stderr rather than byte-identical stderr).
    for (engine, out) in [("VM", &vm), ("tree-walker", &tw)] {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("unknown package 'missing'"),
            "[{engine}] expected unknown-package message, got: {stderr}"
        );
        assert!(
            stderr.contains("ascript add"),
            "[{engine}] message should suggest `ascript add`, got: {stderr}"
        );
    }
}

/// A local `file://` git dep, fetched into an isolated cache, locked, and run
/// byte-identical on both engines; a second `--locked` run is offline against the
/// written lock. Hermetic (no network); skips if `git` is absent.
#[cfg(feature = "pkg")]
#[test]
fn git_dep_fetch_lock_run_byte_identical_then_locked() {
    if !git_available() {
        eprintln!("SKIP git_dep_fetch_lock_run_byte_identical_then_locked: `git` not found");
        return;
    }

    // 1. A tagged git source repo for the `greeter` package.
    let repo = scratch("greeter-repo");
    write(&repo, "ascript.toml", "[package]\nname=\"greeter\"\nversion=\"1.0.0\"\nentry=\"main.as\"\n");
    write(&repo, "main.as", "export fn hi() { return \"hi from git\" }\n");
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "v1"]);
    git(&repo, &["tag", "v1.0.0"]);
    let url = format!("file://{}", repo.display());

    // 2. A consumer project depending on it by tag.
    let app = scratch("git-app");
    write(
        &app,
        "ascript.toml",
        &format!("[package]\nname=\"app\"\nversion=\"0.1.0\"\n\n[dependencies]\ngreeter = {{ git = \"{url}\", tag = \"v1.0.0\" }}\n"),
    );
    write(&app, "main.as", "import { hi } from \"greeter\"\nprint(hi())\n");
    let main_as = app.join("main.as");

    // 3. Isolated cache for this test.
    let cache = scratch("git-cache");

    // 4. `run` (VM) fetches, writes the lock, runs.
    let vm = run_in(&main_as, &cache, false, false);
    assert!(
        vm.status.success(),
        "VM run failed: {}",
        String::from_utf8_lossy(&vm.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&vm.stdout), "hi from git\n");

    // The lock was written beside the manifest.
    let lock_path = app.join("ascript.lock");
    assert!(lock_path.is_file(), "ascript.lock must be written");
    let lock = std::fs::read_to_string(&lock_path).unwrap();
    assert!(lock.contains("name = \"greeter\""), "{lock}");
    assert!(lock.contains("source = \"git+"), "{lock}");
    assert!(lock.contains("rev = "), "lock records the exact commit:\n{lock}");
    assert!(lock.contains("integrity = \"asum1-"), "lock records integrity:\n{lock}");

    // 5. Tree-walker run: byte-identical stdout + exit (cache already warm).
    let tw = run_in(&main_as, &cache, true, false);
    assert_eq!(vm.stdout, tw.stdout, "VM vs tree-walker stdout must match");
    assert_eq!(vm.status.code(), tw.status.code());

    // 6. `--locked` is offline-deterministic against the written lock (cache warm).
    let locked = run_in(&main_as, &cache, false, true);
    assert!(
        locked.status.success(),
        "--locked run failed: {}",
        String::from_utf8_lossy(&locked.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&locked.stdout), "hi from git\n");

    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&app);
    let _ = std::fs::remove_dir_all(&cache);
}
