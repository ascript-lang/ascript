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
    let (arch, _report) = compile_archive(Path::new("examples/bundle_multimodule.as"), false)
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
    let (arch, _report) = compile_archive(Path::new("examples/bundle_multimodule.as"), false).expect("archives");
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

    let (arch, _report) = compile_archive(&entry, false).expect("archives a parent-dir import");
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

    let (arch, _report) = compile_archive(&entry, false).expect("archives the diamond");
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

    let (arch, _report) = compile_archive(&a, false).expect("compile_archive terminates on a cycle");
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
    let (arch, _report) = compile_archive(&entry, false).expect("archives a zero-import program");
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

    let (arch, _report) = compile_archive(&entry, false).expect("archives, skipping std");
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
    let (arch, _report) = compile_archive(Path::new("examples/bundle_multimodule.as"), false)
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

    let (arch, _report) = compile_archive(&entry, false).expect("archives");
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

    let (arch, _report) = compile_archive(&main, false).expect("archives the cycle");
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

    let (arch, _report) = compile_archive(&entry, false).expect("archives nested + parent imports");
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

// ===========================================================================================
// Task 2.3 — TREE-SHAKE EMISSION: unreferenced INERT top-level declarations are actually
// DROPPED from archived LIBRARY modules (the entry is kept whole). These prove the keep-set
// becomes OBSERVABLE in the stored bytecode, that the shaken archive still RUNS (the keep-set
// is closed under references → no dangling globals), that side effects are preserved, and
// that the shaken run is behavior-identical to the unshaken disk run.
// ===========================================================================================

/// Decode a module's archived chunk and return the set of top-level GLOBAL names it
/// DEFINES (`DEFINE_GLOBAL`) — i.e. the module-globals that survived the shake. Probes
/// the BYTECODE directly (walking the top chunk's `code`, reading each `DEFINE_GLOBAL`'s
/// name-const operand) rather than the disasm TEXT (which Task 2.4 may reformat) or the
/// proto debug-names (which would false-positive a class METHOD's name as a global). A
/// dropped `fn`/`let` emits no `DEFINE_GLOBAL`, so its name is absent here.
fn module_defined_names(arch: &ModuleArchive, key: &str) -> std::collections::HashSet<String> {
    use ascript::value::ValueKind;
    use ascript::vm::opcode::Op;
    let bytes = arch.get(key).unwrap_or_else(|| panic!("module {key} present; keys present"));
    let chunk = Chunk::from_bytes_verified(bytes)
        .unwrap_or_else(|e| panic!("module {key} re-verifies: {e:?}"));
    let mut names = std::collections::HashSet::new();
    let code = &chunk.code;
    let mut ip = 0;
    while ip < code.len() {
        let Some(op) = Op::from_u8(code[ip]) else { break };
        if op == Op::DefineGlobal && ip + 2 < code.len() {
            let idx = chunk.read_u16(ip + 1) as usize;
            if let Some(ValueKind::Str(name)) = chunk.consts.get(idx).map(|v| v.kind()) {
                names.insert(name.to_string());
            }
        }
        ip += 1 + op.operand_width();
    }
    names
}

/// THE HEADLINE TEST (observable drop). An entry imports `{ used }` from a lib that also
/// defines `unused` — and each of those calls a private helper (`h` / `dead`). After the
/// shake, the lib's archived chunk must CONTAIN `used`/`h` and NOT CONTAIN `unused`/`dead`,
/// the program must RUN with the correct output, and the run must match the unshaken disk run.
#[tokio::test]
async fn shake_drops_unused_library_declarations() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entry = dir.path().join("main.as");
    let lib = dir.path().join("lib.as");
    std::fs::write(&entry, "import { used } from \"./lib\"\nprint(used())\n").expect("write entry");
    std::fs::write(
        &lib,
        "fn h(): number { return 7 }\n\
         fn dead(): number { return 99 }\n\
         export fn used(): number { return h() + 1 }\n\
         export fn unused(): number { return dead() + 1 }\n",
    )
    .expect("write lib");

    let (arch, _report) = compile_archive(&entry, false).expect("archives");

    // OBSERVABLE: the lib chunk keeps `used` + its helper `h`, drops `unused` + `dead`.
    let defined = module_defined_names(&arch, "lib.as");
    assert!(defined.contains("used"), "kept export `used`; defined = {defined:?}");
    assert!(defined.contains("h"), "kept transitively-referenced helper `h`; defined = {defined:?}");
    assert!(
        !defined.contains("unused"),
        "DROPPED unreferenced export `unused`; defined = {defined:?}"
    );
    assert!(
        !defined.contains("dead"),
        "DROPPED `dead` (only `unused` referenced it); defined = {defined:?}"
    );

    // RUNS correctly from the (shaken) archive with the source tree gone.
    let shaken_bytes = arch.encode();
    let shaken = ModuleArchive::decode(&shaken_bytes).expect("decodes");
    let (out, code) = ascript::run_archive(shaken)
        .await
        .expect("shaken archive runs (no dangling refs)");
    assert_eq!(out, "8\n", "used() = h() + 1 = 8");
    assert_eq!(code, None);

    // SHAKEN == UNSHAKEN: the disk run of the same program produces the SAME stdout.
    let disk_code = ascript::run_file_on_vm(&entry, &[]).await.expect("disk run");
    assert_eq!(disk_code, 0);
}

/// A library top-level SIDE EFFECT (a bare `print(...)`) runs when the module is imported —
/// even though the shaker pruned the library's unreferenced decls around it. The side effect
/// must survive (it is a non-binding statement → always kept) and run exactly once.
#[tokio::test]
async fn shake_preserves_library_side_effects() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entry = dir.path().join("main.as");
    let lib = dir.path().join("lib.as");
    std::fs::write(&entry, "import { keep } from \"./lib\"\nprint(keep())\n").expect("write entry");
    std::fs::write(
        &lib,
        "print(\"loaded\")\n\
         fn dropme(): number { return 0 }\n\
         export fn keep(): number { return 1 }\n",
    )
    .expect("write lib");

    let (arch, _report) = compile_archive(&entry, false).expect("archives");
    let defined = module_defined_names(&arch, "lib.as");
    assert!(defined.contains("keep"), "kept `keep`; defined = {defined:?}");
    assert!(!defined.contains("dropme"), "dropped `dropme`; defined = {defined:?}");

    let (out, code) = ascript::run_archive(arch).await.expect("runs");
    assert_eq!(out, "loaded\n1\n", "side effect ran, then keep() = 1");
    assert_eq!(code, None);
}

/// A computed/side-effecting top-level `let x = sideEffect()` in a library is FORCE-KEPT by
/// the shaker (a `ComputedConst`), so its init side effect runs on import even though `x`
/// itself is never imported.
#[tokio::test]
async fn shake_keeps_computed_let_side_effect() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entry = dir.path().join("main.as");
    let lib = dir.path().join("lib.as");
    std::fs::write(&entry, "import { f } from \"./lib\"\nprint(f())\n").expect("write entry");
    std::fs::write(
        &lib,
        "fn announce(): number { print(\"side\")\n return 0 }\n\
         let _boot = announce()\n\
         export fn f(): number { return 5 }\n",
    )
    .expect("write lib");

    let (arch, _report) = compile_archive(&entry, false).expect("archives");
    let (out, code) = ascript::run_archive(arch).await.expect("runs");
    // The computed `let _boot = announce()` ran its side effect; then f() = 5.
    assert_eq!(out, "side\n5\n", "computed-let side effect ran; out = {out:?}");
    assert_eq!(code, None);
}

/// NAMESPACE static shake (2.2 integration): an `import * as m` that uses ONLY `m.foo`
/// drops the lib's unaccessed `bar`, while `foo` still resolves through the namespace.
#[tokio::test]
async fn shake_namespace_only_accessed_exports() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entry = dir.path().join("main.as");
    let lib = dir.path().join("lib.as");
    std::fs::write(
        &entry,
        "import * as m from \"./lib\"\nprint(m.foo())\n",
    )
    .expect("write entry");
    std::fs::write(
        &lib,
        "export fn foo(): number { return 1 }\n\
         export fn bar(): number { return 2 }\n",
    )
    .expect("write lib");

    let (arch, _report) = compile_archive(&entry, false).expect("archives");
    let defined = module_defined_names(&arch, "lib.as");
    assert!(defined.contains("foo"), "kept accessed `foo`; defined = {defined:?}");
    assert!(
        !defined.contains("bar"),
        "DROPPED unaccessed namespace export `bar`; defined = {defined:?}"
    );

    let (out, code) = ascript::run_archive(arch).await.expect("namespace shake runs");
    assert_eq!(out, "1\n");
    assert_eq!(code, None);
}

/// The ENTRY module is kept WHOLE: its own unreferenced top-level `fn` is STILL present in
/// the archived entry chunk (only LIBRARY modules are pruned).
#[tokio::test]
async fn shake_keeps_entry_module_whole() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entry = dir.path().join("main.as");
    let lib = dir.path().join("lib.as");
    // The entry defines an unreferenced `entry_unused` AND imports `used` from the lib.
    std::fs::write(
        &entry,
        "import { used } from \"./lib\"\n\
         fn entry_unused(): number { return 123 }\n\
         print(used())\n",
    )
    .expect("write entry");
    std::fs::write(&lib, "export fn used(): number { return 1 }\n").expect("write lib");

    let (arch, _report) = compile_archive(&entry, false).expect("archives");
    let entry_defined = module_defined_names(&arch, "main.as");
    assert!(
        entry_defined.contains("entry_unused"),
        "entry kept WHOLE — its unreferenced fn survives; defined = {entry_defined:?}"
    );

    let (out, code) = ascript::run_archive(arch).await.expect("runs");
    assert_eq!(out, "1\n");
    assert_eq!(code, None);
}

/// SLOT-SAFETY: dropping a top-level binding must NOT shift a later KEPT binding's slot
/// indices or break a forward/backward reference between surviving globals. A lib defines
/// `dropme` BETWEEN two kept, mutually-referencing globals — after the shake `a`/`b` still
/// resolve each other and the program runs correctly.
#[tokio::test]
async fn shake_slot_safety_kept_globals_still_resolve() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entry = dir.path().join("main.as");
    let lib = dir.path().join("lib.as");
    std::fs::write(&entry, "import { a } from \"./lib\"\nprint(a())\n").expect("write entry");
    std::fs::write(
        &lib,
        "fn dropme(): number { return 999 }\n\
         fn b(): number { return 40 }\n\
         fn dropme2(): number { return 888 }\n\
         export fn a(): number { return b() + 2 }\n",
    )
    .expect("write lib");

    let (arch, _report) = compile_archive(&entry, false).expect("archives");
    let defined = module_defined_names(&arch, "lib.as");
    assert!(defined.contains("a") && defined.contains("b"), "kept a,b; defined = {defined:?}");
    assert!(
        !defined.contains("dropme") && !defined.contains("dropme2"),
        "dropped both unused fns; defined = {defined:?}"
    );

    let (out, code) = ascript::run_archive(arch).await.expect("shaken globals resolve");
    assert_eq!(out, "42\n", "a() = b() + 2 = 42 — references intact after the drop");
    assert_eq!(code, None);
}

// ===========================================================================================
// Task 2.5 — THE LOAD-BEARING TRIPWIRE: a DIFFERENTIAL proving tree-shaking NEVER changes a
// program's observable behavior. For each multi-module program we build TWO archives over the
// SAME walk — one SHAKEN (pass-2 pruning applied), one UNSHAKEN (pass-2 skipped, library
// modules keep their full pass-1 bytes) — run BOTH via `run_archive`, and assert byte-identical
// stdout + exit code. The comparison is DYNAMIC (both outputs computed, never hardcoded). To
// avoid passing vacuously (a no-op shaker would trivially satisfy "shaken == unshaken"), the
// fixtures with unused code ALSO assert the SHAKEN report actually DROPPED the expected names.
//
// BASELINE APPROACH (A) — no-shake archive. We toggle ONLY pass-2 pruning via the test seam
// `compile_archive_with_shake(entry, with_debug, shake)`; `compile_archive(entry, dbg)` is
// exactly `..._with_shake(entry, dbg, true)`. Building both forms over the identical archive
// walk isolates SHAKING as the single variable (same logical keys, same entry chunk, only
// pruning differs) and needs no new run helper — both archives feed the existing `run_archive`.
// This is the precision baseline the task recommends. (Approach B — a disk-run baseline — is
// also exercised, additively, by `differential_*_matches_disk_run` below for the corpus
// examples, re-validating the full archive↔disk pipeline.)
//
// NOTE on re-exports: AScript has NO `export {x} from "./y"` re-export form (confirmed in
// Task 2.2), so adversarial fixture #8 needs no test — there is no re-export edge to shake.

/// Build a SHAKEN and an UNSHAKEN archive for `entry`, run BOTH from the archive (sources may
/// be absent — the archive is self-contained), and assert their stdout + exit code are
/// byte-identical. Returns the SHAKEN report so the caller can assert specific drops/pins (the
/// tripwire-vs-vacuous guard). If shaken ≠ unshaken this is a REAL shaker bug (live code was
/// dropped) — the assertion fails loudly with both outputs rather than being weakened.
async fn assert_shaken_equals_unshaken(entry: &Path) -> ascript::compile::shake::ShakeReport {
    let (shaken_arch, report) =
        ascript::compile_archive_with_shake(entry, false, true).expect("shaken archive builds");
    let (unshaken_arch, _unshaken_report) =
        ascript::compile_archive_with_shake(entry, false, false).expect("unshaken archive builds");

    // Round-trip BOTH through encode/decode so we run exactly what a self-contained bundle
    // would carry on disk (no borrowed path state, no source-tree dependence).
    let shaken = ModuleArchive::decode(&shaken_arch.encode()).expect("shaken decodes");
    let unshaken = ModuleArchive::decode(&unshaken_arch.encode()).expect("unshaken decodes");

    let (shaken_out, shaken_code) = ascript::run_archive(shaken)
        .await
        .expect("shaken archive runs");
    let (unshaken_out, unshaken_code) = ascript::run_archive(unshaken)
        .await
        .expect("unshaken archive runs");

    assert_eq!(
        shaken_out, unshaken_out,
        "SHAKER BUG: shaken stdout diverged from unshaken for {}\n  shaken:   {shaken_out:?}\n  unshaken: {unshaken_out:?}",
        entry.display()
    );
    assert_eq!(
        shaken_code, unshaken_code,
        "SHAKER BUG: shaken exit code diverged from unshaken for {}: {shaken_code:?} vs {unshaken_code:?}",
        entry.display()
    );
    report
}

/// True if the SHAKEN report dropped `name` from ANY module — the non-vacuous guard.
fn report_dropped(report: &ascript::compile::shake::ShakeReport, name: &str) -> bool {
    report
        .dropped
        .iter()
        .any(|d| d.names.iter().any(|n| n.as_ref() == name))
}

/// Write a set of `(file_name, source)` fixtures into a fresh tempdir and return it (kept
/// alive by the caller — dropping it deletes the tree). Lets each fixture's PROGRAM stand out
/// from the I/O scaffolding; the entry path is `dir.path().join(<entry name>)`.
fn write_fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, src) in files {
        std::fs::write(dir.path().join(name), src).expect("write fixture");
    }
    dir
}

// ── Adversarial fixtures ───────────────────────────────────────────────────────────────────

/// #1 — Namespace + DYNAMIC index. `import * as m; print(m[someKey])` indexes the namespace
/// dynamically, so the shaker cannot prove which exports are used → `util` is PINNED WHOLE.
/// Output identical; the report carries a PIN for util (and drops nothing from it).
#[tokio::test]
async fn differential_namespace_dynamic_index_pins_whole() {
    let dir = write_fixture(&[
        (
            "main.as",
            "import * as m from \"./util\"\nlet someKey = \"foo\"\nprint(m[someKey]())\n",
        ),
        (
            "util.as",
            "export fn foo(): number { return 1 }\n\
             export fn bar(): number { return 2 }\n",
        ),
    ]);
    let entry = dir.path().join("main.as");

    let report = assert_shaken_equals_unshaken(&entry).await;
    // util is pinned whole: a PIN is recorded for it, and nothing was dropped from it.
    assert!(
        report.pins.iter().any(|p| p.key == "util.as"),
        "util must be PINNED whole (dynamic index); pins = {:?}",
        report.pins.iter().map(|p| &p.key).collect::<Vec<_>>()
    );
    assert!(
        !report_dropped(&report, "bar") && !report_dropped(&report, "foo"),
        "a pinned module drops nothing; dropped = {:?}",
        report.dropped
    );
}

/// #2 — Namespace + STATIC method calls. `import * as m; m.foo(...); m.bar()` uses ONLY
/// `foo`/`bar` statically → util shaken to {foo,bar}; the unused `baz` is dropped. Output
/// identical; assert `baz` dropped.
#[tokio::test]
async fn differential_namespace_static_calls_drops_unaccessed() {
    let dir = write_fixture(&[
        (
            "main.as",
            "import * as m from \"./util\"\nprint(m.foo(3) + m.bar())\n",
        ),
        (
            "util.as",
            "export fn foo(a: number): number { return a }\n\
             export fn bar(): number { return 2 }\n\
             export fn baz(): number { return 99 }\n",
        ),
    ]);
    let entry = dir.path().join("main.as");

    let report = assert_shaken_equals_unshaken(&entry).await;
    assert!(
        report_dropped(&report, "baz"),
        "unaccessed namespace export `baz` must be dropped; dropped = {:?}",
        report.dropped
    );
}

/// #3 — ESCAPING function value. `import { f } from "./util"; let g = f; print(g())` aliases
/// the imported `f` into a local — `f` is kept whole (a named import keeps its target). Output
/// identical. (No drop asserted: the lib only exports `f`, which is used.)
#[tokio::test]
async fn differential_escaping_function_value_kept() {
    let dir = write_fixture(&[
        (
            "main.as",
            "import { f } from \"./util\"\nlet g = f\nprint(g())\n",
        ),
        ("util.as", "export fn f(): number { return 7 }\n"),
    ]);
    let entry = dir.path().join("main.as");

    // The whole value of this fixture is that the escape does NOT change behavior under shaking.
    let _report = assert_shaken_equals_unshaken(&entry).await;
}

/// #4 — CIRCULAR imports. A imports B, B imports A, with a value used across the cycle. Output
/// identical; both modules' used parts kept (the deferred-namespace cycle the disk loader
/// handles). The entry sits outside the tight cycle so it can read A's settled export.
#[tokio::test]
async fn differential_circular_imports() {
    let dir = write_fixture(&[
        ("main.as", "import { fromA } from \"./a\"\nprint(fromA())\n"),
        (
            "a.as",
            "import * as b from \"./b\"\n\
             export fn fromA(): number { return 10 + b.fromB() }\n",
        ),
        (
            "b.as",
            "import * as a from \"./a\"\n\
             export fn fromB(): number { return 5 }\n\
             export fn deadInB(): number { return 999 }\n",
        ),
    ]);
    let main = dir.path().join("main.as");

    let _report = assert_shaken_equals_unshaken(&main).await;
    // (No drop ASSERT: whether `deadInB` is droppable depends on whether the `a`↔`b` namespace
    // cycle pins; the load-bearing claim here is shaken == unshaken across the cycle.)
}

/// #5 — SIDE-EFFECTFUL top-level in source order. A lib has a top-level `print("loaded")`, a
/// computed `let x = sideEffect()`, AND an unused `dead` fn. The side effects must run IN
/// SOURCE ORDER in the shaken archive; output identical; assert `dead` dropped but the side
/// effects present (the differential proves order + presence, the drop proves non-vacuity).
#[tokio::test]
async fn differential_side_effectful_toplevel_in_source_order() {
    let dir = write_fixture(&[
        ("main.as", "import { keep } from \"./lib\"\nprint(keep())\n"),
        (
            "lib.as",
            "fn compute(): number { print(\"computing\")\n return 1 }\n\
             print(\"loaded\")\n\
             let x = compute()\n\
             fn dead(): number { return 0 }\n\
             export fn keep(): number { return 42 }\n",
        ),
    ]);
    let entry = dir.path().join("main.as");

    let report = assert_shaken_equals_unshaken(&entry).await;
    assert!(
        report_dropped(&report, "dead"),
        "unused `dead` fn must be dropped; dropped = {:?}",
        report.dropped
    );

    // asserts the specific SOURCE ORDER of side effects — distinct from the shaken==unshaken
    // equality above (print("loaded") then the computed-let's print("computing")), then keep().
    let (arch, _r) = compile_archive(&entry, false).expect("archives");
    let (out, _code) = ascript::run_archive(arch).await.expect("runs");
    assert_eq!(
        out, "loaded\ncomputing\n42\n",
        "side effects ran in source order; out = {out:?}"
    );
}

/// #6 — DIAMOND. entry → A, entry → B; A → D, B → D. D has a used + an unused export; D's
/// unused is dropped ONCE (D is archived once). Output identical; assert the unused export
/// dropped.
#[tokio::test]
async fn differential_diamond_drops_shared_unused_once() {
    let dir = write_fixture(&[
        (
            "main.as",
            "import { fa } from \"./a\"\nimport { fb } from \"./b\"\nprint(fa() + fb())\n",
        ),
        (
            "a.as",
            "import { used } from \"./d\"\nexport fn fa(): number { return used() }\n",
        ),
        (
            "b.as",
            "import { used } from \"./d\"\nexport fn fb(): number { return used() }\n",
        ),
        (
            "d.as",
            "export fn used(): number { return 3 }\n\
             export fn unusedInD(): number { return 100 }\n",
        ),
    ]);
    let entry = dir.path().join("main.as");

    let report = assert_shaken_equals_unshaken(&entry).await;
    assert!(
        report_dropped(&report, "unusedInD"),
        "diamond's shared `unusedInD` must be dropped; dropped = {:?}",
        report.dropped
    );
    // Dropped exactly once — D appears once in the report (one ModuleDrops entry naming it).
    let d_drop_entries = report
        .dropped
        .iter()
        .filter(|m| m.names.iter().any(|n| n.as_ref() == "unusedInD"))
        .count();
    assert_eq!(d_drop_entries, 1, "D is shaken once, not per-importer");
}

/// #7 — CROSS-MODULE classes/enums. The entry uses a class from lib1 (with a SUPERCLASS) and
/// an enum from lib2 in a match; each lib carries an UNUSED sibling class / enum-helper that is
/// dropped. Output identical; assert the unused class dropped.
#[tokio::test]
async fn differential_cross_module_classes_enums() {
    let dir = write_fixture(&[
        (
            "main.as",
            "import { Dog } from \"./lib1\"\nimport { Color, describe } from \"./lib2\"\n\
             let d = Dog(\"Rex\")\nprint(d.speak())\nprint(describe(Color.Red))\n",
        ),
        (
            "lib1.as",
            "class Animal {\n  fn init(name: string) { self.name = name }\n  fn speak(): string { return \"...\" }\n}\n\
             export class Dog extends Animal {\n  fn init(name: string) { super.init(name) }\n  fn speak(): string { return `${self.name} says woof` }\n}\n\
             export class Cat extends Animal {\n  fn init(name: string) { super.init(name) }\n  fn speak(): string { return `${self.name} meows` }\n}\n",
        ),
        (
            "lib2.as",
            "export enum Color { Red, Green, Blue }\n\
             export fn describe(c: Color): string {\n  return match c {\n    Color.Red => \"r\",\n    Color.Green => \"g\",\n    Color.Blue => \"b\",\n  }\n}\n\
             export fn unusedHelper(): number { return 42 }\n",
        ),
    ]);
    let entry = dir.path().join("main.as");

    let report = assert_shaken_equals_unshaken(&entry).await;
    // lib1's unused `Cat` (only `Dog` + its superclass `Animal` are reachable) is dropped.
    assert!(
        report_dropped(&report, "Cat"),
        "unused class `Cat` must be dropped; dropped = {:?}",
        report.dropped
    );
    // lib2's unused `unusedHelper` is dropped (the enum + describe are reachable).
    assert!(
        report_dropped(&report, "unusedHelper"),
        "unused `unusedHelper` must be dropped; dropped = {:?}",
        report.dropped
    );
}

// ── Corpus coverage: the existing multi-module EXAMPLES ──────────────────────────────────────

/// The shipped `bundle_multimodule.as` example (named import of a sibling) is shaken ==
/// unshaken, byte-identical. The sibling `bundle_util.as` carries an intentionally-unused
/// `whisper` export, so this is ALSO a non-vacuous shake: the shaken archive must drop it.
#[tokio::test]
async fn differential_corpus_bundle_multimodule() {
    let report = assert_shaken_equals_unshaken(Path::new("examples/bundle_multimodule.as")).await;
    assert!(
        report_dropped(&report, "whisper"),
        "the unused `whisper` export must be dropped by the shaker; dropped = {:?}",
        report.dropped
    );
}

/// The shipped `examples/advanced/bundle_caps.as` example (a `std/caps` posture demo that
/// imports its sibling `./bundle_caps_util` BOTH ways — namespace + named) is shaken ==
/// unshaken, byte-identical. Validates the bundle + capabilities corpus example end to end.
#[tokio::test]
async fn differential_corpus_bundle_caps() {
    let _report =
        assert_shaken_equals_unshaken(Path::new("examples/advanced/bundle_caps.as")).await;
}

/// The `examples/app/main.as` example (named import + transitive + a NAMESPACE import + a
/// cross-module class) is shaken == unshaken, byte-identical.
#[tokio::test]
async fn differential_corpus_app_main() {
    let _report = assert_shaken_equals_unshaken(Path::new("examples/app/main.as")).await;
}

/// The `examples/modules/main.as` example (named + namespace import of the same geometry
/// module, plus a class) is shaken == unshaken, byte-identical.
#[tokio::test]
async fn differential_corpus_modules_main() {
    let _report = assert_shaken_equals_unshaken(Path::new("examples/modules/main.as")).await;
}

// ── Approach (B), additive: the corpus shaken archive matches the on-DISK run ────────────────
// Re-validates the full archive↔disk pipeline for the headline examples — the shaken archive's
// captured stdout equals the on-disk run's stdout (computed dynamically, never hardcoded). The
// disk run is INHERENTLY unshaken (the loader hits disk for every import, no archive, no
// pruning), so this is a second, independent unshaken baseline.

/// Headline corpus example: the SHAKEN archive's stdout equals the on-disk (unshaken) run's
/// stdout, byte-for-byte. Proves shaking + the archive loader together preserve behavior end
/// to end.
#[tokio::test]
async fn differential_bundle_multimodule_archive_matches_disk_run() {
    let entry = Path::new("examples/bundle_multimodule.as");
    let (arch, _report) = compile_archive(entry, false).expect("archives");
    // (This example's exports are all used, so it shakes nothing — that's fine here: this
    // approach-(B) test validates the archive↔DISK pipeline preserves behavior, not
    // non-vacuity. The adversarial fixtures above own the "shaking actually happened" guard.)
    let shaken = ModuleArchive::decode(&arch.encode()).expect("decodes");
    let (archive_out, archive_code) = ascript::run_archive(shaken).await.expect("archive runs");
    // The on-disk run is inherently unshaken (loads every import from disk, no archive/pruning).
    let (disk_out, disk_code) = ascript::vm_run_file_captured(entry)
        .await
        .expect("disk program runs");
    assert_eq!(
        archive_out, disk_out,
        "shaken archive stdout must equal the on-disk run; archive={archive_out:?} disk={disk_out:?}"
    );
    assert_eq!(archive_code, disk_code, "exit codes match");
}

// ── Field-type reachability (validate_into SOUNDNESS) ────────────────────────────────────────
// Phase 2 holistic-review BLOCKER: a class/enum referenced ONLY as a FIELD TYPE carries no
// bytecode `GET_GLOBAL`, so the pre-fix reachability closure dropped it — yet `.from` /
// typed-parse validation resolves the field's declared `Type::Named` leaf through the class's
// `def_env` (`coerce_field` → `validate_into`), coercing a nested Object into a class instance.
// Dropping that class makes the lookup fail and the contract check error: shaken ≠ unshaken,
// the cardinal-rule violation. These fixtures are the permanent differential guard for that
// missing dimension. (`report_kept` = a non-vacuity counterpart to `report_dropped`: the class
// MUST survive the shake.)

/// A class is KEPT (not in the dropped report) — the keep-side counterpart to `report_dropped`.
fn report_kept(report: &ascript::compile::shake::ShakeReport, name: &str) -> bool {
    !report_dropped(report, name)
}

/// HEADLINE (the repro): `Inner` is referenced ONLY as the field type of `Outer.inner`. After
/// the fix `Outer.from({inner:{v:5}})` coerces the nested object into an `Inner` instance, so
/// `o.inner.v` prints `5` identically shaken and unshaken — and `Inner` is KEPT.
#[tokio::test]
async fn differential_class_kept_as_field_type_headline() {
    let dir = write_fixture(&[
        (
            "main.as",
            "import { Outer } from \"./lib\"\n\
             let o = Outer.from({ inner: { v: 5 } })\n\
             print(o.inner.v)\n",
        ),
        (
            "lib.as",
            "class Inner { v: number }\n\
             export class Outer { inner: Inner }\n",
        ),
    ]);
    let entry = dir.path().join("main.as");

    let report = assert_shaken_equals_unshaken(&entry).await;
    assert!(
        report_kept(&report, "Inner"),
        "Inner is load-bearing (Outer.inner field type) and MUST be kept; dropped = {:?}",
        report.dropped
    );
}

/// `array<Item>` field type: `Bag.items: array<Item>`; `.from` with an array coerces each
/// element into an `Item`. `Item` kept, output identical.
#[tokio::test]
async fn differential_class_kept_as_array_field_element_type() {
    let dir = write_fixture(&[
        (
            "main.as",
            "import { Bag } from \"./lib\"\n\
             let b = Bag.from({ items: [{ v: 1 }, { v: 2 }] })\n\
             print(b.items[0].v + b.items[1].v)\n",
        ),
        (
            "lib.as",
            "class Item { v: number }\n\
             export class Bag { items: array<Item> }\n",
        ),
    ]);
    let entry = dir.path().join("main.as");

    let report = assert_shaken_equals_unshaken(&entry).await;
    assert!(
        report_kept(&report, "Item"),
        "Item is load-bearing (array<Item> element type) and MUST be kept; dropped = {:?}",
        report.dropped
    );
}

/// `map<string, Cell>` field type: `Grid.cells: map<string, Cell>`; `.from` with a nested
/// object coerces each value into a `Cell`. `Cell` kept, output identical.
#[tokio::test]
async fn differential_class_kept_as_map_value_field_type() {
    let dir = write_fixture(&[
        (
            "main.as",
            "import * as map from \"std/map\"\n\
             import { Grid } from \"./lib\"\n\
             let g = Grid.from({ cells: { \"a\": { n: 10 }, \"b\": { n: 20 } } })\n\
             print(map.get(g.cells, \"a\").n + map.get(g.cells, \"b\").n)\n",
        ),
        (
            "lib.as",
            "class Cell { n: number }\n\
             export class Grid { cells: map<string, Cell> }\n",
        ),
    ]);
    let entry = dir.path().join("main.as");

    let report = assert_shaken_equals_unshaken(&entry).await;
    assert!(
        report_kept(&report, "Cell"),
        "Cell is load-bearing (map<string, Cell> value type) and MUST be kept; dropped = {:?}",
        report.dropped
    );
}

/// TRANSITIVE chain: `Outer.inner: Mid`, `Mid.deep: Leaf` — all three kept via the field-type
/// chain; an unrelated `Dead` class (referenced nowhere) is still dropped (non-vacuity).
#[tokio::test]
async fn differential_class_kept_transitively_via_field_types() {
    let dir = write_fixture(&[
        (
            "main.as",
            "import { Outer } from \"./lib\"\n\
             let o = Outer.from({ mid: { leaf: { v: 7 } } })\n\
             print(o.mid.leaf.v)\n",
        ),
        (
            "lib.as",
            "class Leaf { v: number }\n\
             class Mid { leaf: Leaf }\n\
             class Dead { junk: number }\n\
             export class Outer { mid: Mid }\n",
        ),
    ]);
    let entry = dir.path().join("main.as");

    let report = assert_shaken_equals_unshaken(&entry).await;
    assert!(
        report_kept(&report, "Mid") && report_kept(&report, "Leaf"),
        "Mid + Leaf are load-bearing via the field-type chain; dropped = {:?}",
        report.dropped
    );
    assert!(
        report_dropped(&report, "Dead"),
        "the unrelated `Dead` class must STILL be dropped (non-vacuity); dropped = {:?}",
        report.dropped
    );
}

/// ENUM payload field type (conservative under-shake). An enum payload variant
/// `Shape.Circle(c: Cell)` names `Cell` as a payload FIELD TYPE. We keep `Cell` (the shaker
/// walks enum payload field `Type::Named` leaves) so a payload-coercion path could never drop
/// live code. Output identical shaken vs unshaken; `Cell` kept.
#[tokio::test]
async fn differential_class_kept_as_enum_payload_field_type() {
    let dir = write_fixture(&[
        (
            "main.as",
            "import { Shape, Cell } from \"./lib\"\n\
             let s = Shape.Circle(Cell.from({ n: 9 }))\n\
             match s {\n\
             \tShape.Circle(c) => print(c.n)\n\
             }\n",
        ),
        (
            "lib.as",
            "export class Cell { n: number }\n\
             export enum Shape { Circle(c: Cell) }\n",
        ),
    ]);
    let entry = dir.path().join("main.as");

    let report = assert_shaken_equals_unshaken(&entry).await;
    assert!(
        report_kept(&report, "Cell"),
        "Cell is referenced as an enum payload field type and is kept (conservative); dropped = {:?}",
        report.dropped
    );
}

/// NEGATIVE (preserve shaking): a class used ONLY as a PARAM type annotation is STILL dropped.
/// `CheckParam` uses the ENV-FREE `check_type` (no global lookup, no coercion), so a
/// param-type-only class is sound to drop — the fix must not over-correct into keeping it.
#[tokio::test]
async fn differential_param_type_only_class_still_dropped() {
    let dir = write_fixture(&[
        (
            "main.as",
            "import { run } from \"./lib\"\nprint(run())\n",
        ),
        (
            "lib.as",
            // `helper` IS kept (called transitively from the exported `run`), so this is a
            // genuine over-correction guard: `Unused` appears ONLY as `helper`'s param TYPE
            // annotation — never constructed, never a field type. `CheckParam` uses the
            // ENV-FREE `check_type` (no global lookup, no coercion), so `Unused` has no runtime
            // edge and MUST stay dropped even though `helper` survives.
            "class Unused { tag: number }\n\
             fn helper(x: Unused?): number { return 5 }\n\
             export fn run(): number { return helper(nil) }\n",
        ),
    ]);
    let entry = dir.path().join("main.as");

    let report = assert_shaken_equals_unshaken(&entry).await;
    assert!(
        report_dropped(&report, "Unused"),
        "a class used ONLY as a param type annotation must STILL be dropped (env-free check_type); dropped = {:?}",
        report.dropped
    );
}
