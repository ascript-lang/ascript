//! Integration tests for `compile_archive` (self-contained-bundles Phase 1, Task 1.3):
//! walking a program's import graph and compiling each reachable module into a
//! [`ModuleArchive`]. The archive is the container codec covered by the unit tests in
//! `src/vm/archive.rs`; here we assert the GRAPH WALK: dedup, transitive reach, the
//! machine-independent logical-key convention, cycle termination, and that every stored
//! chunk re-verifies.

use ascript::compile_archive;
use ascript::vm::archive::ModuleArchive;
use ascript::vm::chunk::Chunk;
use std::path::Path;

/// The multi-module example pair (`bundle_multimodule.as` imports `./bundle_util`)
/// archives BOTH modules, the entry index points at the entry, both logical keys are
/// present under the relative-to-entry-dir convention, and every chunk re-verifies.
#[test]
fn multimodule_archive_has_both_modules() {
    let arch = compile_archive(Path::new("examples/bundle_multimodule.as"), false)
        .expect("compile_archive succeeds for the multi-module example");

    // Both modules are present, keyed by their entry-dir-relative logical path.
    assert!(
        arch.get("bundle_multimodule.as").is_some(),
        "entry module must be archived under its logical key; keys = {:?}",
        keys(&arch)
    );
    assert!(
        arch.get("bundle_util.as").is_some(),
        "imported sibling must be archived under its logical key; keys = {:?}",
        keys(&arch)
    );
    assert_eq!(arch.modules.len(), 2, "exactly the two reachable modules");

    // The `entry` field indexes the entry module.
    let (entry_key, entry_bytes) = &arch.modules[arch.entry as usize];
    assert_eq!(entry_key, "bundle_multimodule.as");

    // Every embedded chunk re-verifies through the SAME trust boundary the runtime uses.
    for (key, bytes) in &arch.modules {
        Chunk::from_bytes_verified(bytes)
            .unwrap_or_else(|e| panic!("module {key} chunk failed verification: {e:?}"));
    }
    // (entry bytes specifically verify — they are the program's start chunk)
    Chunk::from_bytes_verified(entry_bytes).expect("entry chunk verifies");
}

/// The logical keys must be MACHINE-INDEPENDENT: no absolute build-machine path leaks
/// in (spec §3.3 — "store-relative logical id … so a bundle built on one machine
/// resolves on another"). They are paths relative to the entry's directory. A `..`
/// segment is allowed (it stays relative-to-entry-dir and machine-independent — see
/// `parent_directory_import_keys_with_dotdot`); only ABSOLUTE / machine-specific data is
/// forbidden.
#[test]
fn logical_keys_are_machine_independent() {
    let arch = compile_archive(Path::new("examples/bundle_multimodule.as"), false).expect("archives");
    let build_dir = env!("CARGO_MANIFEST_DIR");
    for (key, _) in &arch.modules {
        assert!(
            !Path::new(key).is_absolute(),
            "logical key {key:?} is absolute — leaks the build machine's layout"
        );
        // No leading separator, drive prefix (`:`), or backslash — keys use `/`.
        assert!(
            !key.starts_with('/') && !key.contains('\\') && !key.contains(':'),
            "logical key {key:?} carries a non-portable separator/prefix"
        );
        // No canonicalized build-machine path component leaked through (the absolute
        // crate-root path must never appear in a key).
        assert!(
            !key.contains(build_dir),
            "logical key {key:?} leaks the build directory {build_dir:?}"
        );
    }
}

/// A module in a SUBDIRECTORY importing `../shared` keys the dependency as `../shared.as`
/// — the `..` is PRESERVED VERBATIM (it is still relative to the entry dir, hence
/// machine-independent and reproducible). This proves the preserve behavior is
/// intentional, not an accident, and pins the exact key Task 1.4's loader must compute.
#[test]
fn parent_directory_import_keys_with_dotdot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sub = dir.path().join("app");
    std::fs::create_dir(&sub).expect("mkdir app");
    let entry = sub.join("main.as");
    let shared = dir.path().join("shared.as");
    // The entry lives in `app/`, the dependency one level up in the root.
    std::fs::write(
        &entry,
        "import { ping } from \"../shared\"\nprint(ping())\n",
    )
    .expect("write entry");
    std::fs::write(
        &shared,
        "export fn ping(): number { return 7 }\n",
    )
    .expect("write shared");

    let arch = compile_archive(&entry, false).expect("archives a parent-dir import");
    assert_eq!(arch.modules.len(), 2);
    // Entry keys to its file name at the logical root.
    assert_eq!(&arch.modules[arch.entry as usize].0, "main.as");
    // The dependency escapes the entry dir → the `..` is preserved verbatim.
    assert!(
        arch.get("../shared.as").is_some(),
        "parent import must key as `../shared.as`; keys = {:?}",
        keys(&arch)
    );
    // And it is STILL machine-independent: no absolute prefix leaked.
    for (key, _) in &arch.modules {
        assert!(
            !Path::new(key).is_absolute() && !key.contains(dir.path().to_str().unwrap_or("")),
            "key {key:?} leaked an absolute build path"
        );
    }
}

/// A DIAMOND import graph (entry → A, entry → B, A → util, B → util) archives `util`
/// EXACTLY ONCE — dedup by canonical path collapses the two edges. This exercises the
/// non-cycle dedup path (two distinct importers reaching the same module), the central
/// correctness claim of the walk.
#[test]
fn diamond_import_dedups_shared_module() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entry = dir.path().join("main.as");
    let a = dir.path().join("a.as");
    let b = dir.path().join("b.as");
    let util = dir.path().join("util.as");
    std::fs::write(
        &entry,
        "import { fa } from \"./a\"\nimport { fb } from \"./b\"\nprint(fa() + fb())\n",
    )
    .expect("write entry");
    std::fs::write(
        &a,
        "import { shared } from \"./util\"\nexport fn fa(): number { return shared() }\n",
    )
    .expect("write a");
    std::fs::write(
        &b,
        "import { shared } from \"./util\"\nexport fn fb(): number { return shared() }\n",
    )
    .expect("write b");
    std::fs::write(&util, "export fn shared(): number { return 1 }\n").expect("write util");

    let arch = compile_archive(&entry, false).expect("archives the diamond");
    assert_eq!(
        arch.modules.len(),
        4,
        "entry + a + b + util (util deduped once); keys = {:?}",
        keys(&arch)
    );
    // `util` appears exactly once.
    let util_count = arch.modules.iter().filter(|(k, _)| k == "util.as").count();
    assert_eq!(util_count, 1, "util must be archived exactly once, not per-importer");
    for k in ["main.as", "a.as", "b.as", "util.as"] {
        assert!(arch.get(k).is_some(), "{k} archived; keys = {:?}", keys(&arch));
    }
}

/// A CIRCULAR import (A imports B imports A) must TERMINATE (dedup by logical key before
/// recursing) and archive BOTH modules.
#[test]
fn circular_import_terminates_and_archives_both() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a.as");
    let b = dir.path().join("b.as");
    // a imports b, b imports a — a cycle. Each names an export the other binds so the
    // import is well-formed.
    std::fs::write(
        &a,
        "import { fromB } from \"./b\"\nexport fn fromA(): number { return 1 }\n",
    )
    .expect("write a");
    std::fs::write(
        &b,
        "import { fromA } from \"./a\"\nexport fn fromB(): number { return 2 }\n",
    )
    .expect("write b");

    let arch = compile_archive(&a, false).expect("compile_archive terminates on a cycle");
    assert_eq!(arch.modules.len(), 2, "both cycle members archived once each");
    assert!(arch.get("a.as").is_some(), "a archived; keys={:?}", keys(&arch));
    assert!(arch.get("b.as").is_some(), "b archived; keys={:?}", keys(&arch));
    assert_eq!(&arch.modules[arch.entry as usize].0, "a.as");
}

/// An import of a package that is not installed → a clean `AsError`, never a panic.
#[test]
fn unknown_package_is_a_clean_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entry = dir.path().join("main.as");
    std::fs::write(
        &entry,
        "import { x } from \"definitely_not_a_real_package\"\nprint(x)\n",
    )
    .expect("write");
    let err = compile_archive(&entry, false).expect_err("unknown package must error");
    assert!(
        err.message.contains("definitely_not_a_real_package")
            || err.message.contains("unknown package"),
        "error should name the unknown package: {}",
        err.message
    );
}

/// A missing imported file → a clean `AsError`, never a panic.
#[test]
fn missing_import_is_a_clean_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entry = dir.path().join("main.as");
    std::fs::write(&entry, "import { y } from \"./does_not_exist\"\nprint(y)\n").expect("write");
    let err = compile_archive(&entry, false).expect_err("missing module must error");
    assert!(!err.message.is_empty(), "missing-module error is non-empty");
}

/// A zero-import program still archives as a single-module archive (entry only).
#[test]
fn single_module_archive_has_one_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entry = dir.path().join("solo.as");
    std::fs::write(&entry, "print(1 + 1)\n").expect("write");
    let arch = compile_archive(&entry, false).expect("archives a zero-import program");
    assert_eq!(arch.modules.len(), 1);
    assert_eq!(arch.entry, 0);
    assert_eq!(&arch.modules[0].0, "solo.as");
}

/// A `std/*` import is NEVER archived (native Rust, linked into the runtime). An entry
/// importing both `std/math` and a relative sibling archives ONLY the entry + sibling.
#[test]
fn std_imports_are_not_archived() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entry = dir.path().join("main.as");
    let helper = dir.path().join("helper.as");
    std::fs::write(
        &entry,
        "import { abs } from \"std/math\"\nimport { greet } from \"./helper\"\n\
         print(abs(-3))\nprint(greet(\"x\"))\n",
    )
    .expect("write entry");
    std::fs::write(
        &helper,
        "export fn greet(n: string): string { return `hi ${n}` }\n",
    )
    .expect("write helper");

    let arch = compile_archive(&entry, false).expect("archives, skipping std");
    assert_eq!(arch.modules.len(), 2, "only entry + helper; std is not archived");
    assert!(arch.get("main.as").is_some());
    assert!(arch.get("helper.as").is_some());
    // No std module leaked in under any key.
    for (key, _) in &arch.modules {
        assert!(
            !key.starts_with("std/") && !key.contains("math"),
            "std module leaked into the archive under key {key:?}"
        );
    }
}

fn keys(arch: &ascript::vm::archive::ModuleArchive) -> Vec<&str> {
    arch.modules.iter().map(|(k, _)| k.as_str()).collect()
}

// ===========================================================================================
// Task 1.4 — the RUNTIME LOADER: consult the in-memory archive before disk. These prove that
// `load_file_module` reproduces the EXACT logical key `compile_archive` (1.3) stored, by
// running a program PURELY from an archive with the SOURCE TREE ABSENT.
// ===========================================================================================

/// THE HEADLINE TEST. Build an archive from the multi-module example, then run the entry
/// module from the archive with the SOURCE FILES INACCESSIBLE — the imported module's
/// function works only if the embedded module was found via its logical key (NOT disk).
/// Assert the output matches a disk run of the same program.
#[tokio::test]
async fn runs_purely_from_archive_with_sources_absent() {
    // 1. The archive is built from the real example (the only place the sources are read).
    let arch = compile_archive(Path::new("examples/bundle_multimodule.as"), false)
        .expect("compile_archive succeeds");

    // 2. Make the sources INACCESSIBLE: copy the archive into a process where the loader's
    //    `module_dir` cannot reach the example dir. `run_archive` installs the archive on a
    //    fresh VM whose module_dir is the cwd; the import `./bundle_util` would resolve on
    //    disk to `<cwd>/bundle_util.as` (which does NOT exist — the real file is under
    //    `examples/`). So a disk hit is impossible; only an archive hit can satisfy it.
    let (out, code) = ascript::run_archive(arch)
        .await
        .expect("program runs from the archive with no source tree");

    // 3. The program's output is exactly what the disk run produces:
    //    greet("world") → "Hello, world!" ; shout("bundled") → "bundled!!!"
    assert_eq!(out, "Hello, world!\nbundled!!!\n", "archive run output");
    assert_eq!(code, None, "clean exit");
}

/// Belt-and-braces on "sources absent": archive a program in a TEMP dir, DELETE the entire
/// source tree, then run from the archive. The loader physically cannot reach any `.as`.
#[tokio::test]
async fn archive_run_survives_deleted_source_tree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entry = dir.path().join("main.as");
    let util = dir.path().join("util.as");
    std::fs::write(
        &entry,
        "import { val } from \"./util\"\nprint(val() + 40)\n",
    )
    .expect("write entry");
    std::fs::write(&util, "export fn val(): number { return 2 }\n").expect("write util");

    let arch = compile_archive(&entry, false).expect("archives");
    // Re-encode/decode to prove the archive is fully self-contained bytes (no borrowed
    // path state), then DELETE the sources entirely.
    let bytes = arch.encode();
    drop(dir); // removes main.as + util.as
    let arch = ModuleArchive::decode(&bytes).expect("decodes");

    let (out, code) = ascript::run_archive(arch)
        .await
        .expect("runs from archive after the source tree is gone");
    assert_eq!(out, "42\n");
    assert_eq!(code, None);
}

/// A CIRCULAR import archive runs without an infinite loop or a double side-effect: each
/// module's top-level body runs AT MOST ONCE (the in-progress cache entry, keyed on the
/// logical key, terminates the cycle), exactly as the disk loader does.
///
/// Structure mirrors the disk loader's proven cycle handling: `a` and `b` import each
/// other via a DEFERRED namespace import (`import * as`, accessed only inside a fn body,
/// never at top-level bind time), so the cycle resolves to the in-progress entry rather
/// than reading a not-yet-populated export. The entry sits OUTSIDE the tight cycle so it
/// can read `a`'s export after both bodies have settled. This is byte-identical to the
/// disk run of the same three files (verified separately).
#[tokio::test]
async fn circular_archive_runs_once_no_infinite_loop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let main = dir.path().join("main.as");
    let a = dir.path().join("a.as");
    let b = dir.path().join("b.as");
    std::fs::write(&main, "import { fromA } from \"./a\"\nprint(\"main: \" + fromA())\n")
        .expect("write main");
    std::fs::write(
        &a,
        "import * as b from \"./b\"\nprint(\"a-body\")\nexport fn fromA(): string { return \"A\" }\n",
    )
    .expect("write a");
    std::fs::write(
        &b,
        "import * as a from \"./a\"\nprint(\"b-body\")\nexport fn fromB(): string { return \"B\" }\n",
    )
    .expect("write b");

    let arch = compile_archive(&main, false).expect("archives the cycle");
    let bytes = arch.encode();
    drop(dir);
    let arch = ModuleArchive::decode(&bytes).expect("decodes");

    let (out, code) = ascript::run_archive(arch)
        .await
        .expect("circular archive terminates");
    // Each module body printed exactly once — no double side effect, no infinite loop.
    assert_eq!(out.matches("a-body").count(), 1, "a body ran once; out={out:?}");
    assert_eq!(out.matches("b-body").count(), 1, "b body ran once; out={out:?}");
    assert!(out.contains("main: A"), "entry read a's export; out={out:?}");
    assert_eq!(code, None);
}

/// A SUBDIRECTORY import + a `..`-escaping parent import both resolve from the archive by
/// the SAME `..`-preserving key the builder stored — the load-bearing key convention.
#[tokio::test]
async fn nested_and_parent_dir_imports_resolve_from_archive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = dir.path().join("app");
    std::fs::create_dir(&app).expect("mkdir app");
    // Entry lives in app/; imports a sibling in app/ AND a module one level up.
    let entry = app.join("main.as");
    let sibling = app.join("helper.as");
    let shared = dir.path().join("shared.as");
    std::fs::write(
        &entry,
        "import { h } from \"./helper\"\nimport { s } from \"../shared\"\nprint(h() + s())\n",
    )
    .expect("write entry");
    std::fs::write(&sibling, "export fn h(): number { return 10 }\n").expect("write helper");
    std::fs::write(&shared, "export fn s(): number { return 5 }\n").expect("write shared");

    let arch = compile_archive(&entry, false).expect("archives nested + parent imports");
    // Sanity: the parent import keyed with a verbatim `..`.
    assert!(
        arch.get("../shared.as").is_some(),
        "parent import keyed as ../shared.as; keys={:?}",
        keys(&arch)
    );
    let bytes = arch.encode();
    drop(dir);
    let arch = ModuleArchive::decode(&bytes).expect("decodes");

    let (out, code) = ascript::run_archive(arch)
        .await
        .expect("nested + parent imports resolve from archive");
    assert_eq!(out, "15\n");
    assert_eq!(code, None);
}

/// A corrupt EMBEDDED chunk → a clean error (the SAME trust boundary as a corrupt `.aso`),
/// never a panic.
#[tokio::test]
async fn corrupt_embedded_entry_chunk_is_clean_error() {
    let arch = ModuleArchive::new(
        0,
        ascript::stdlib::caps::CapSet::default(),
        [0u8; 32],
        // Garbage bytes where a verified chunk should be.
        vec![("main.as".to_string(), vec![0xDE, 0xAD, 0xBE, 0xEF])],
    );
    let err = ascript::run_archive(arch)
        .await
        .expect_err("a corrupt embedded chunk must be a clean error");
    assert!(!err.message.is_empty(), "non-empty load error: {err:?}");
}

/// NO-REGRESSION: a normal multi-file program (NO archive installed) still loads its
/// imports from DISK. This is the default `module_archive == None` path — it must behave
/// exactly as before. (`vm_run_source` runs with no archive; we point its import at a temp
/// disk module and confirm it resolves on disk.)
#[tokio::test]
async fn non_archive_multifile_still_loads_from_disk() {
    // Run the real on-disk example via the CLI-less VM entry, with module_dir at examples/.
    // `run_archive` is NOT used here; the loader's archive is None, so this exercises the
    // unchanged disk path.
    let dir = tempfile::tempdir().expect("tempdir");
    let entry = dir.path().join("prog.as");
    let lib = dir.path().join("lib.as");
    std::fs::write(
        &entry,
        "import { twice } from \"./lib\"\nprint(twice(21))\n",
    )
    .expect("write entry");
    std::fs::write(&lib, "export fn twice(n: number): number { return n * 2 }\n")
        .expect("write lib");

    // Run the entry file on the VM the normal way (disk imports, no archive).
    let code = ascript::run_file_on_vm(&entry, &[])
        .await
        .expect("disk multi-file program runs");
    assert_eq!(code, 0, "disk run exits cleanly");
}
