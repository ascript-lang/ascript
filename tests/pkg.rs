//! SP6 package-manager CLI integration tests (hermetic — path + local `file://`
//! git fixtures only; NO network).
//!
//! The headline invariant: a bare-specifier import program runs BYTE-IDENTICAL
//! under `ascript run` (VM) and `ascript run --tree-walker` (the oracle). These
//! spawn the built binary like `tests/cli.rs`.

use std::path::PathBuf;
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ascript")
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
