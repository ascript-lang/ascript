//! End-to-end tests for `ascript build` (→ `.aso`), running a `.aso` on the VM,
//! and FILE-module import resolution (`.as`/`.aso`) — VM plan V12-T4.
//!
//! Each test runs the real built binary (`CARGO_BIN_EXE_ascript`) in its OWN unique
//! temp directory so the module cache and `.aso` files of one test never leak into
//! another. The central invariant proven throughout: `run <stem>.aso` (VM path)
//! produces byte-identical stdout + exit code to `run <stem>.as` (tree-walker).

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ascript")
}

/// A fresh, unique temp directory for one test (created; caller writes files into it).
fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ascript_aso_{tag}_{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, name: &str, src: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, src).unwrap();
    p
}

/// Run the binary with `args` (cwd = `dir`), returning (stdout, exit_code).
fn run(dir: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(bin())
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

/// `ascript build <file>` must succeed and produce the expected `.aso`.
fn build(dir: &Path, file: &str) {
    let out = Command::new(bin())
        .arg("build")
        .arg(file)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "build {file} failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// build + run .aso == run .as
// ---------------------------------------------------------------------------

#[test]
fn build_then_run_aso_matches_tree_walker_simple() {
    let dir = unique_dir("simple");
    write(&dir, "p.as", "let x = 2\nprint(x + 3 * 4)\n");
    build(&dir, "p.as");
    assert!(dir.join("p.aso").exists(), "p.aso should exist");

    let (aso_out, aso_code) = run(&dir, &["run", "p.aso"]);
    let (as_out, as_code) = run(&dir, &["run", "p.as"]);
    assert_eq!(aso_out, "14\n");
    assert_eq!(aso_out, as_out, "aso stdout must match tree-walker");
    assert_eq!(aso_code, as_code);
}

#[test]
fn build_then_run_aso_matches_object_rest() {
    // Proves the V12-T2 object-rest serialization fix: `let {a, ...rest} = obj`
    // emits an Op::ObjectRest with a const Array-of-Str key list, which must now
    // serialize into the `.aso` and run byte-identically.
    let dir = unique_dir("objrest");
    write(
        &dir,
        "r.as",
        "let {a, ...rest} = {a: 1, b: 2, c: 3}\nprint(a)\nprint(rest)\n",
    );
    build(&dir, "r.as");

    let (aso_out, _) = run(&dir, &["run", "r.aso"]);
    let (as_out, _) = run(&dir, &["run", "r.as"]);
    assert_eq!(aso_out, "1\n{b: 2, c: 3}\n");
    assert_eq!(aso_out, as_out);
}

#[test]
fn build_then_run_aso_matches_classes() {
    let dir = unique_dir("class");
    write(
        &dir,
        "c.as",
        "class Point {\n  x: number\n  y: number\n  fn init(x, y) { self.x = x\n    self.y = y }\n  fn sum() { return self.x + self.y }\n}\nlet p = Point(3, 4)\nprint(p.sum())\n",
    );
    build(&dir, "c.as");

    let (aso_out, _) = run(&dir, &["run", "c.aso"]);
    let (as_out, _) = run(&dir, &["run", "c.as"]);
    assert_eq!(aso_out, "7\n");
    assert_eq!(aso_out, as_out);
}

// ---------------------------------------------------------------------------
// FILE-module import resolution (named + namespace)
// ---------------------------------------------------------------------------

#[test]
fn named_file_import_on_vm_matches_tree_walker() {
    let dir = unique_dir("named");
    write(
        &dir,
        "mathmod.as",
        "export fn add(x, y) { return x + y }\nexport const PI = 3\n",
    );
    write(
        &dir,
        "main.as",
        "import { add, PI } from \"./mathmod\"\nprint(add(2, 3))\nprint(PI)\n",
    );
    build(&dir, "mathmod.as");
    build(&dir, "main.as");

    let (aso_out, _) = run(&dir, &["run", "main.aso"]);
    let (as_out, _) = run(&dir, &["run", "main.as"]);
    assert_eq!(aso_out, "5\n3\n");
    assert_eq!(aso_out, as_out);
}

#[test]
fn namespace_file_import_on_vm_matches_tree_walker() {
    let dir = unique_dir("ns");
    write(
        &dir,
        "mathmod.as",
        "export fn add(x, y) { return x + y }\nexport const PI = 3\n",
    );
    write(
        &dir,
        "nsmain.as",
        "import * as m from \"./mathmod\"\nprint(m.add(10, 20))\nprint(m.PI)\n",
    );
    build(&dir, "mathmod.as");
    build(&dir, "nsmain.as");

    let (aso_out, _) = run(&dir, &["run", "nsmain.aso"]);
    let (as_out, _) = run(&dir, &["run", "nsmain.as"]);
    assert_eq!(aso_out, "30\n3\n");
    assert_eq!(aso_out, as_out);
}

#[test]
fn transitive_file_imports_on_vm() {
    // a imports b imports c.
    let dir = unique_dir("transitive");
    write(&dir, "c.as", "export const C = 100\n");
    write(
        &dir,
        "b.as",
        "import { C } from \"./c\"\nexport fn bfn() { return C + 1 }\n",
    );
    write(
        &dir,
        "a.as",
        "import { bfn } from \"./b\"\nprint(bfn())\n",
    );
    build(&dir, "a.as");

    let (aso_out, _) = run(&dir, &["run", "a.aso"]);
    let (as_out, _) = run(&dir, &["run", "a.as"]);
    assert_eq!(aso_out, "101\n");
    assert_eq!(aso_out, as_out);
}

// ---------------------------------------------------------------------------
// .aso / .as precedence rules
// ---------------------------------------------------------------------------

#[test]
fn aso_only_dependency_runs_without_source() {
    // Build the dependency to .aso, then remove its source; importing it must
    // still work (prefers the .aso when no source is present).
    let dir = unique_dir("asoonly");
    write(&dir, "dep.as", "export const V = 42\n");
    write(
        &dir,
        "use.as",
        "import { V } from \"./dep\"\nprint(V)\n",
    );
    build(&dir, "dep.as");
    build(&dir, "use.as");
    std::fs::remove_file(dir.join("dep.as")).unwrap();

    let (out, code) = run(&dir, &["run", "use.aso"]);
    assert_eq!(out, "42\n");
    assert_eq!(code, 0);
}

#[test]
fn stale_aso_recompiles_from_newer_source() {
    // A dependency whose source is NEWER than its .aso must be recompiled from
    // source (Python's rule: prefer .aso only when aso_mtime >= src_mtime).
    let dir = unique_dir("stale");
    write(&dir, "dep.as", "export const V = \"old\"\n");
    write(
        &dir,
        "use.as",
        "import { V } from \"./dep\"\nprint(V)\n",
    );
    build(&dir, "dep.as");
    build(&dir, "use.as");

    // First run uses the built dep.aso → "old".
    let (out1, _) = run(&dir, &["run", "use.aso"]);
    assert_eq!(out1, "old\n");

    // Make the source newer than the .aso and change its value.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write(&dir, "dep.as", "export const V = \"new\"\n");

    // Now the importer must pick up the recompiled source → "new".
    let (out2, _) = run(&dir, &["run", "use.aso"]);
    assert_eq!(out2, "new\n");
}

#[test]
fn corrupt_aso_dep_recompiles_when_source_present() {
    // A present-but-unloadable (corrupt header) dependency `.aso` falls back to
    // recompiling the source when source is present.
    let dir = unique_dir("corrupt");
    write(&dir, "dep.as", "export const V = \"src\"\n");
    write(
        &dir,
        "use.as",
        "import { V } from \"./dep\"\nprint(V)\n",
    );
    build(&dir, "use.as");
    // Corrupt the dep.aso but keep it NEWER than dep.as so it would be preferred.
    write(&dir, "dep.aso", "GARBAGE-NOT-A-VALID-ASO");
    // Make sure dep.as is older.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(dir.join("dep.aso"), b"GARBAGE-NOT-A-VALID-ASO").unwrap();

    let (out, code) = run(&dir, &["run", "use.aso"]);
    assert_eq!(out, "src\n", "should recompile from source on corrupt .aso");
    assert_eq!(code, 0);
}

#[test]
fn corrupt_aso_entry_errors_clearly() {
    // Directly running a corrupt `.aso` (no recompile fallback for the ENTRY file)
    // must fail with a non-zero exit and a clear error.
    let dir = unique_dir("entrycorrupt");
    std::fs::write(dir.join("bad.aso"), b"NOT-A-VALID-ASO-FILE").unwrap();
    let out = Command::new(bin())
        .arg("run")
        .arg("bad.aso")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success(), "corrupt .aso entry should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot load") || stderr.contains("bad.aso"),
        "expected a clear load error, got: {stderr}"
    );
}

#[test]
fn build_refuses_compile_error() {
    let dir = unique_dir("builderr");
    write(&dir, "bad.as", "fn f(x x) {}\n"); // malformed param list — compile error
    let out = Command::new(bin())
        .arg("build")
        .arg("bad.as")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success(), "build of a bad program should fail");
    assert!(
        !dir.join("bad.aso").exists(),
        "no .aso should be written on compile error"
    );
}
