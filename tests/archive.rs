//! Integration tests for `compile_archive` (self-contained-bundles Phase 1, Task 1.3):
//! walking a program's import graph and compiling each reachable module into a
//! [`ModuleArchive`]. The archive is the container codec covered by the unit tests in
//! `src/vm/archive.rs`; here we assert the GRAPH WALK: dedup, transitive reach, the
//! machine-independent logical-key convention, cycle termination, and that every stored
//! chunk re-verifies.

use ascript::compile_archive;
use ascript::vm::chunk::Chunk;
use std::path::Path;

/// The multi-module example pair (`bundle_multimodule.as` imports `./bundle_util`)
/// archives BOTH modules, the entry index points at the entry, both logical keys are
/// present under the relative-to-entry-dir convention, and every chunk re-verifies.
#[test]
fn multimodule_archive_has_both_modules() {
    let arch = compile_archive(Path::new("examples/bundle_multimodule.as"))
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
    let arch = compile_archive(Path::new("examples/bundle_multimodule.as")).expect("archives");
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

    let arch = compile_archive(&entry).expect("archives a parent-dir import");
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

    let arch = compile_archive(&entry).expect("archives the diamond");
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

    let arch = compile_archive(&a).expect("compile_archive terminates on a cycle");
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
    let err = compile_archive(&entry).expect_err("unknown package must error");
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
    let err = compile_archive(&entry).expect_err("missing module must error");
    assert!(!err.message.is_empty(), "missing-module error is non-empty");
}

/// A zero-import program still archives as a single-module archive (entry only).
#[test]
fn single_module_archive_has_one_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entry = dir.path().join("solo.as");
    std::fs::write(&entry, "print(1 + 1)\n").expect("write");
    let arch = compile_archive(&entry).expect("archives a zero-import program");
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

    let arch = compile_archive(&entry).expect("archives, skipping std");
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
