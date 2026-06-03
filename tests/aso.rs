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

// ---------------------------------------------------------------------------
// .as cutover: `run <file.as>` now executes on the bytecode VM (tree-walker kept
// as the differential oracle, reachable via ASCRIPT_ENGINE=tree-walker).
// ---------------------------------------------------------------------------

/// Repo root (CARGO_MANIFEST_DIR) so tests can run the real `examples/*.as`.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Run the binary from the repo root (so relative `examples/...` paths resolve),
/// optionally setting `ASCRIPT_ENGINE`. Returns (stdout, exit_code).
fn run_root(args: &[&str], engine: Option<&str>) -> (String, i32) {
    let mut cmd = Command::new(bin());
    cmd.args(args).current_dir(repo_root());
    match engine {
        Some(v) => {
            cmd.env("ASCRIPT_ENGINE", v);
        }
        None => {
            cmd.env_remove("ASCRIPT_ENGINE");
        }
    }
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

/// The cutover gate: `run <file.as>` (now VM) is byte-identical to the same
/// program built + run as `.aso` (VM) AND to the tree-walker (oracle escape hatch),
/// across a hello, an imports example, and a class/compute example.
#[test]
fn run_as_on_vm_matches_aso_and_tree_walker() {
    for example in ["examples/hello.as", "examples/modules/main.as", "examples/oop.as"] {
        // Default (no env): VM path.
        let (vm_out, vm_code) = run_root(&["run", example], None);
        // Oracle escape hatch: tree-walker.
        let (tw_out, tw_code) = run_root(&["run", example], Some("tree-walker"));
        assert_eq!(
            vm_out, tw_out,
            "{example}: VM stdout must match tree-walker oracle"
        );
        assert_eq!(vm_code, tw_code, "{example}: VM exit must match tree-walker");

        // And byte-identical to the built .aso (VM, no compile step at run time).
        let dir = unique_dir("cutover");
        let src = std::fs::read_to_string(repo_root().join(example)).unwrap();
        write(&dir, "prog.as", &src);
        // hello/oop are self-contained; modules/main imports siblings, so copy them.
        if example == "examples/modules/main.as" {
            let moddir = repo_root().join("examples/modules");
            for entry in std::fs::read_dir(&moddir).unwrap() {
                let p = entry.unwrap().path();
                if p.extension().and_then(|e| e.to_str()) == Some("as") {
                    let name = p.file_name().unwrap().to_str().unwrap();
                    if name != "main.as" {
                        std::fs::copy(&p, dir.join(name)).unwrap();
                    }
                }
            }
        }
        build(&dir, "prog.as");
        let (aso_out, aso_code) = run(&dir, &["run", "prog.aso"]);
        assert_eq!(aso_out, vm_out, "{example}: .aso stdout must match VM .as run");
        assert_eq!(aso_code, vm_code, "{example}: .aso exit must match VM .as run");
    }
}

/// WS3 Deliverable 1 — the comprehensive all-features showcase example. The
/// three execution paths must agree byte-for-byte: VM-from-source (`run .as`),
/// the built `.aso` (VM bytecode), and the tree-walker oracle (`--tree-walker`).
///
/// Gated on `data`: the example imports `std/json` to showcase serialization,
/// which is unavailable under `--no-default-features` (the import would error
/// identically on both engines, with no exit-0 reference output to compare). The
/// VM-vs-tree-walker parity for the bare-language constructs in this file is
/// still covered by the whole-corpus differential in `tests/vm_differential.rs`.
#[cfg(feature = "data")]
#[test]
fn all_features_example_aso_vm_and_tree_walker_agree() {
    let example = "examples/all_features.as";

    // VM from source vs the tree-walker oracle.
    let (vm_out, vm_code) = run_root(&["run", example], None);
    let (tw_out, tw_code) = run_root(&["run", example], Some("tree-walker"));
    assert_eq!(vm_out, tw_out, "{example}: VM stdout must match tree-walker");
    assert_eq!(vm_code, tw_code, "{example}: VM exit must match tree-walker");
    assert_eq!(vm_code, 0, "{example}: should exit 0");
    assert!(
        vm_out.ends_with("all_features ok\n"),
        "{example}: should end with the success line, got tail: {:?}",
        &vm_out[vm_out.len().saturating_sub(40)..]
    );

    // Build to .aso and run the bytecode: must match the VM-from-source run.
    let dir = unique_dir("allfeatures");
    let src = std::fs::read_to_string(repo_root().join(example)).unwrap();
    write(&dir, "prog.as", &src);
    build(&dir, "prog.as");
    assert!(dir.join("prog.aso").exists(), "prog.aso should exist");
    let (aso_out, aso_code) = run(&dir, &["run", "prog.aso"]);
    assert_eq!(aso_out, vm_out, "{example}: .aso stdout must match VM .as run");
    assert_eq!(aso_code, vm_code, "{example}: .aso exit must match VM .as run");
}

/// WS3 Deliverable 2 — the multi-file local-import application (`examples/app/`):
/// `main.as` imports `shapes.as` (which TRANSITIVELY imports `util.as`) plus a
/// namespace import of `util.as`. Building the entry point must resolve the local
/// imports, and the resulting `.aso` (VM) must be byte-identical to both the
/// VM-from-source run and the tree-walker oracle.
#[test]
fn local_import_app_example_aso_vm_and_tree_walker_agree() {
    let entry = "examples/app/main.as";

    // VM from source vs the tree-walker oracle (relative ./imports resolve from
    // the repo root because run_root sets cwd to the repo root).
    let (vm_out, vm_code) = run_root(&["run", entry], None);
    let (tw_out, tw_code) = run_root(&["run", entry], Some("tree-walker"));
    assert_eq!(vm_out, tw_out, "{entry}: VM stdout must match tree-walker");
    assert_eq!(vm_code, tw_code, "{entry}: VM exit must match tree-walker");
    assert_eq!(vm_code, 0, "{entry}: should exit 0");
    assert!(
        vm_out.ends_with("app ok\n"),
        "{entry}: should end with the success line, got: {vm_out:?}"
    );

    // Copy the whole app/ module set into a unique temp dir, build each module +
    // the entry, then run the entry `.aso` — mirrors the existing transitive
    // file-import tests. The built `.aso` must match the VM-from-source run.
    let dir = unique_dir("appimport");
    let appdir = repo_root().join("examples/app");
    for name in ["util.as", "shapes.as", "main.as"] {
        std::fs::copy(appdir.join(name), dir.join(name)).unwrap();
    }
    // Build leaf-first so each dependency's `.aso` exists when the next is built;
    // building `main.as` resolves its (already-built) local imports.
    build(&dir, "util.as");
    build(&dir, "shapes.as");
    build(&dir, "main.as");
    assert!(dir.join("main.aso").exists(), "main.aso should exist");
    let (aso_out, aso_code) = run(&dir, &["run", "main.aso"]);
    assert_eq!(aso_out, vm_out, "{entry}: .aso stdout must match VM .as run");
    assert_eq!(aso_code, vm_code, "{entry}: .aso exit must match VM .as run");
}

/// The oracle escape hatch stays CLI-reachable: `ASCRIPT_ENGINE=tree-walker`
/// routes `.as` back to the tree-walker and still produces correct output.
#[test]
fn ascript_engine_tree_walker_escape_hatch_works() {
    let (out, code) = run_root(&["run", "examples/hello.as"], Some("tree-walker"));
    assert_eq!(out, "7\n");
    assert_eq!(code, 0);
}

/// A Tier-2 panic in a `.as` program run on the VM renders a proper diagnostic
/// (source attached: file path + line/col) and exits non-zero.
#[test]
fn run_as_on_vm_panic_shows_diagnostic() {
    let dir = unique_dir("panic");
    write(&dir, "boom.as", "let y = nil\nprint(y.field)\n");
    let out = Command::new(bin())
        .args(["run", "boom.as"])
        .current_dir(&dir)
        .env_remove("ASCRIPT_ENGINE")
        .output()
        .unwrap();
    assert_eq!(out.status.code().unwrap_or(-1), 1, "panic should exit 1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("boom.as") && stderr.contains("cannot read property"),
        "diagnostic should name the file and the error; got: {stderr}"
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
