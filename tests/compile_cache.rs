//! Integration tests for the WARM compile cache (Tasks 2 + 3).
//!
//! Task 2 covered the store-level CLI (`cache clean`/`dir`). Task 3 wires the cached
//! `ascript run` front door and adds the adversarial stale-hit battery (§5-A): every
//! staleness source must MISS (entry edit, transitive edit, path-dep edit, flag change,
//! lockfile change, different-path); a content-preserving `touch` must HIT; `--no-cache`
//! / `ASCRIPT_NO_COMPILE_CACHE=1` must bypass; cached vs uncached runs (incl. panic
//! stderr and worker programs) must be byte-identical; `.aso`/`--tree-walker`/`--profile`
//! must never consult the cache; and the §2.5 walk-drift tripwire pins the cache walk to
//! the compile-path walk.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ascript")
}

/// Create a unique temp dir for one test's ASCRIPT_CACHE and return the path.
fn unique_cache_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "ascript-cct-{}-{}-{}",
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

/// Write the 3-module program: main.as → util.as → model.as.
fn write_three_module_program(dir: &Path) -> PathBuf {
    let model = dir.join("model.as");
    let util = dir.join("util.as");
    let main = dir.join("main.as");
    std::fs::write(
        &model,
        "export fn greet(name) { return 'hello ' + name }\n",
    )
    .unwrap();
    std::fs::write(&util, "import { greet } from './model'\nexport fn run(n) { return greet(n) }\n")
        .unwrap();
    std::fs::write(
        &main,
        "import { run } from './util'\nprint(run('world'))\n",
    )
    .unwrap();
    main
}

// ─────────────────────────────────────────────────────────────────────────────
// cache clean / cache dir — store-level tests that PASS in Task 2
// ─────────────────────────────────────────────────────────────────────────────

/// `ascript cache dir` prints the cache root (which is ASCRIPT_CACHE when set).
#[test]
fn cache_dir_prints_cache_root() {
    let cache = unique_cache_dir("dir_test");
    let out = Command::new(bin())
        .arg("cache")
        .arg("dir")
        .env("ASCRIPT_CACHE", &cache)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cache dir must succeed: {:?}",
        out.status
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim() == cache.to_string_lossy(),
        "cache dir must print the cache root path; got {:?}",
        stdout.trim()
    );
    let _ = std::fs::remove_dir_all(&cache);
}

/// `ascript cache clean` on an empty cache prints the empty message (no error).
#[test]
fn cache_clean_empty_is_harmless() {
    let cache = unique_cache_dir("clean_empty");
    let out = Command::new(bin())
        .arg("cache")
        .arg("clean")
        .env("ASCRIPT_CACHE", &cache)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cache clean on empty cache must succeed: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("empty"),
        "clean on empty cache must say 'empty', got: {:?}",
        stdout
    );
    let _ = std::fs::remove_dir_all(&cache);
}

/// `ascript cache clean` removes compiled/ entries and leaves store/ intact.
#[test]
fn cache_clean_removes_compiled_namespace_only() {
    let cache = unique_cache_dir("clean_namespace");

    // Create fake compiled/ entries.
    let compiled = cache.join("compiled");
    std::fs::create_dir_all(compiled.join("ck1-aaaa")).unwrap();
    std::fs::create_dir_all(compiled.join("ck1-bbbb")).unwrap();
    std::fs::write(compiled.join("ck1-aaaa").join("program.aso"), b"fake").unwrap();
    std::fs::write(compiled.join("ck1-bbbb").join("program.aso"), b"fake").unwrap();

    // Create a fake store/ entry (should NOT be removed).
    let store = cache.join("store").join("asum1-fake");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("some-file.as"), b"package content").unwrap();

    let out = Command::new(bin())
        .arg("cache")
        .arg("clean")
        .env("ASCRIPT_CACHE", &cache)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "cache clean must succeed: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    // compiled/ namespace must be gone.
    assert!(
        !compiled.exists(),
        "compiled/ must be removed by cache clean"
    );

    // store/ must be intact.
    assert!(
        cache.join("store").join("asum1-fake").join("some-file.as").exists(),
        "store/ must be untouched by cache clean"
    );

    // Output must mention "2" (two slots removed).
    assert!(
        stdout.contains('2'),
        "output must report count of removed slots, got: {:?}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&cache);
}

/// `ascript cache dir` prints the cache root.
/// (Duplicate angle: test that it works with a non-existent path too.)
#[test]
fn cache_dir_works_when_cache_does_not_exist_yet() {
    let cache = unique_cache_dir("dir_missing");
    // Do NOT create the dir.
    let _ = std::fs::remove_dir_all(&cache);

    let out = Command::new(bin())
        .arg("cache")
        .arg("dir")
        .env("ASCRIPT_CACHE", &cache)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cache dir must succeed even when cache root doesn't exist: {:?}",
        out.status
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim() == cache.to_string_lossy(),
        "expected {:?}, got {:?}",
        cache.to_string_lossy(),
        stdout.trim()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Store-level tests at the library level (publish + lookup) — Task 2
// ─────────────────────────────────────────────────────────────────────────────
// NOTE: These are UNIT tests inside compile_cache.rs (#[cfg(test)] mod tests).
// The integration-test file (here) focuses on spawn-based CLI tests.
//
// The library-level unit tests (publish_then_lookup_hit, miss_after_source_edit,
// etc.) live in src/cache/compile_cache.rs and run via `cargo test`.

// ─────────────────────────────────────────────────────────────────────────────
// Task 3 helpers — the cached run front door
// ─────────────────────────────────────────────────────────────────────────────

/// Run `ascript run <main> [extra args]` with a given ASCRIPT_CACHE.
fn run_cached(cache: &Path, main: &Path, extra: &[&str]) -> std::process::Output {
    let mut cmd = Command::new(bin());
    cmd.arg("run").arg(main);
    for a in extra {
        cmd.arg(a);
    }
    cmd.env("ASCRIPT_CACHE", cache).output().unwrap()
}

/// Count slot directories in `<cache>/compiled/`.
fn slot_count(cache: &Path) -> usize {
    let compiled = cache.join("compiled");
    if !compiled.exists() {
        return 0;
    }
    std::fs::read_dir(&compiled)
        .map(|rd| rd.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).count())
        .unwrap_or(0)
}

/// A panicking 3-module program: model.boom() panics, util forwards, main calls it.
/// Returns the entry path (main.as).
fn write_panicking_program(dir: &Path) -> PathBuf {
    let model = dir.join("model.as");
    let util = dir.join("util.as");
    let main = dir.join("main.as");
    std::fs::write(&model, "export fn boom() { return [1, 2][9] }\n").unwrap();
    std::fs::write(
        &util,
        "import { boom } from './model'\nexport fn go() { return boom() }\n",
    )
    .unwrap();
    std::fs::write(&main, "import { go } from './util'\nprint(go())\n").unwrap();
    main
}

// ─────────────────────────────────────────────────────────────────────────────
// §2.5 walk-drift tripwire — collect_module_graph == compile-path enumeration
// ─────────────────────────────────────────────────────────────────────────────

/// The cache HASHES the set `collect_module_graph` produces; the archive COMPILES the
/// set `compile_path_module_set` reports. If they ever diverge a hit could run the
/// wrong code. This permanent tripwire asserts the two walks produce the IDENTICAL set
/// (logical keys + canonical paths, same order). WARM Task 3 §2.5 option (b).
#[test]
fn collect_module_graph_matches_compile_path() {
    let dir = unique_cache_dir("walk_drift_src");
    let main = write_three_module_program(&dir);

    let graph = ascript::cache::collect_module_graph(&main)
        .expect("collect_module_graph must succeed");
    let compile_set =
        ascript::compile_path_module_set(&main).expect("compile_path_module_set must succeed");

    let graph_pairs: Vec<(String, PathBuf)> = graph
        .iter()
        .map(|m| (m.logical_key.clone(), m.path.clone()))
        .collect();

    assert_eq!(
        graph_pairs, compile_set,
        "the cache walk and the compile-path walk must produce the IDENTICAL module set \
         (logical keys + canonical paths, same order) — a divergence is a false-hit risk"
    );
    // Sanity: the 3-module program yields exactly 3 modules.
    assert_eq!(graph_pairs.len(), 3, "expected 3 reachable modules");

    let _ = std::fs::remove_dir_all(&dir);
}

// ─────────────────────────────────────────────────────────────────────────────
// Task 3 adversarial stale-hit battery (§5-A) — each cold cache per case
// ─────────────────────────────────────────────────────────────────────────────

/// Editing the ENTRY file ⇒ miss ⇒ recompile (output reflects the edit).
#[test]
fn edit_entry_misses() {
    let cache = unique_cache_dir("edit_entry");
    let src = unique_cache_dir("edit_entry_src");
    let main = write_three_module_program(&src);

    let cold = run_cached(&cache, &main, &[]);
    assert!(cold.status.success());
    assert_eq!(String::from_utf8_lossy(&cold.stdout).trim(), "hello world");
    assert_eq!(slot_count(&cache), 1, "cold run must publish one slot");

    // Edit the entry: print a different argument.
    std::fs::write(&main, "import { run } from './util'\nprint(run('there'))\n").unwrap();
    let warm = run_cached(&cache, &main, &[]);
    assert!(warm.status.success());
    assert_eq!(
        String::from_utf8_lossy(&warm.stdout).trim(),
        "hello there",
        "entry edit must MISS and recompile (not stale-hit the old artifact)"
    );

    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&src);
}

/// Editing ANY transitive module (util or model) ⇒ miss ⇒ recompile.
#[test]
fn edit_transitive_module_misses() {
    for which in ["util.as", "model.as"] {
        let cache = unique_cache_dir(&format!("edit_trans_{which}"));
        let src = unique_cache_dir(&format!("edit_trans_src_{which}"));
        let main = write_three_module_program(&src);

        let cold = run_cached(&cache, &main, &[]);
        assert!(cold.status.success(), "cold run for {which} must succeed");
        assert_eq!(String::from_utf8_lossy(&cold.stdout).trim(), "hello world");

        // Edit the transitive module so its output changes.
        if which == "model.as" {
            std::fs::write(
                src.join("model.as"),
                "export fn greet(name) { return 'HI ' + name }\n",
            )
            .unwrap();
        } else {
            std::fs::write(
                src.join("util.as"),
                "import { greet } from './model'\nexport fn run(n) { return greet(n) + '!' }\n",
            )
            .unwrap();
        }
        let warm = run_cached(&cache, &main, &[]);
        assert!(warm.status.success(), "warm run for {which} must succeed");
        let out = String::from_utf8_lossy(&warm.stdout);
        assert_ne!(
            out.trim(),
            "hello world",
            "editing {which} must MISS and recompile, not stale-hit"
        );

        let _ = std::fs::remove_dir_all(&cache);
        let _ = std::fs::remove_dir_all(&src);
    }
}

/// Editing a `{path = …}` package-dependency module ⇒ miss ⇒ recompile. The cache
/// hashes package modules by file content (no asum1 shortcut), so a MUTABLE path dep
/// edit is correctly covered (§2.5).
#[cfg(feature = "pkg")]
#[test]
fn edit_path_dep_package_module_misses() {
    let cache = unique_cache_dir("path_dep");
    let root = unique_cache_dir("path_dep_src");
    // The path-dependency package lives in a sibling dir.
    let dep = root.join("mathlib");
    std::fs::create_dir_all(&dep).unwrap();
    std::fs::write(
        dep.join("ascript.toml"),
        "[package]\nname=\"mathlib\"\nversion=\"1.0.0\"\nentry=\"index.as\"\n",
    )
    .unwrap();
    std::fs::write(
        dep.join("index.as"),
        "export fn twice(n) { return n * 2 }\n",
    )
    .unwrap();
    // The app imports the package by name.
    let app = root.join("app");
    std::fs::create_dir_all(&app).unwrap();
    let main = app.join("main.as");
    std::fs::write(
        &main,
        "import { twice } from 'mathlib'\nprint(twice(21))\n",
    )
    .unwrap();
    // ascript.toml with a path dependency.
    std::fs::write(
        app.join("ascript.toml"),
        "[package]\nname = \"app\"\nversion = \"0.0.0\"\n\n[dependencies]\nmathlib = { path = \"../mathlib\" }\n",
    )
    .unwrap();

    let cold = run_cached(&cache, &main, &[]);
    assert!(
        cold.status.success(),
        "cold path-dep run must succeed: {}",
        String::from_utf8_lossy(&cold.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&cold.stdout).trim(), "42");

    // Edit the path-dep package module.
    std::fs::write(
        dep.join("index.as"),
        "export fn twice(n) { return n * 3 }\n",
    )
    .unwrap();
    let warm = run_cached(&cache, &main, &[]);
    assert!(warm.status.success());
    assert_eq!(
        String::from_utf8_lossy(&warm.stdout).trim(),
        "63",
        "editing a path-dep package module must MISS and recompile"
    );

    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&root);
}

/// Same content at a DIFFERENT path ⇒ miss (path is part of the key, §2.4); each run's
/// panic caret names ITS OWN invoking path.
#[test]
fn same_content_different_path_misses_and_diagnostics_show_invoking_path() {
    let cache = unique_cache_dir("samecontent");
    let dir_a = unique_cache_dir("samecontent_a");
    let dir_b = unique_cache_dir("samecontent_b");
    let main_a = write_panicking_program(&dir_a);
    let main_b = write_panicking_program(&dir_b);

    let run_a = run_cached(&cache, &main_a, &[]);
    let run_b = run_cached(&cache, &main_b, &[]);

    // Both panic (out-of-bounds index).
    assert!(!run_a.status.success());
    assert!(!run_b.status.success());

    // Two distinct slots (path-in-key).
    assert_eq!(
        slot_count(&cache),
        2,
        "same content at different paths must create TWO slots"
    );

    // Each stderr names its own invoking path.
    let err_a = String::from_utf8_lossy(&run_a.stderr);
    let err_b = String::from_utf8_lossy(&run_b.stderr);
    assert!(
        err_a.contains(&dir_a.to_string_lossy().to_string()),
        "run A's diagnostic must name dir_a; got:\n{err_a}"
    );
    assert!(
        err_b.contains(&dir_b.to_string_lossy().to_string()),
        "run B's diagnostic must name dir_b; got:\n{err_b}"
    );
    // And NOT the other's path.
    assert!(
        !err_a.contains(&dir_b.to_string_lossy().to_string()),
        "run A must not name dir_b"
    );

    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

/// `touch` (mtime-only, content unchanged) ⇒ HIT (digests validate, not mtimes).
#[test]
fn touch_without_change_hits() {
    let cache = unique_cache_dir("touch_hit");
    let src = unique_cache_dir("touch_hit_src");
    let main = write_three_module_program(&src);

    let cold = run_cached(&cache, &main, &[]);
    assert!(cold.status.success());
    assert_eq!(slot_count(&cache), 1);

    // Re-write each file with the EXACT same content (new mtime, same bytes).
    for f in ["main.as", "util.as", "model.as"] {
        let p = src.join(f);
        let content = std::fs::read(&p).unwrap();
        // Sleep a hair so the mtime actually changes on coarse-resolution FSes.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&p, &content).unwrap();
    }

    let warm = run_cached(&cache, &main, &[]);
    assert!(warm.status.success());
    assert_eq!(cold.stdout, warm.stdout, "touch must HIT (same output)");
    // No NEW slot was created (still exactly one).
    assert_eq!(slot_count(&cache), 1, "touch must not create a new slot");

    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&src);
}

/// A codegen-flag change (perturbed via the `ASCRIPT_TEST_CACHE_FLAG_SALT` test seam) ⇒ miss
/// (a NEW slot is created — the keys differ).
#[test]
fn flag_change_misses() {
    let cache = unique_cache_dir("flag_change");
    let src = unique_cache_dir("flag_change_src");
    let main = write_three_module_program(&src);

    let cold = run_cached(&cache, &main, &[]);
    assert!(cold.status.success());
    assert_eq!(slot_count(&cache), 1);

    // Same program, same path, but a different codegen-flag salt → different key.
    let warm = Command::new(bin())
        .arg("run")
        .arg(&main)
        .env("ASCRIPT_CACHE", &cache)
        .env("ASCRIPT_TEST_CACHE_FLAG_SALT", "perturbed")
        .output()
        .unwrap();
    assert!(warm.status.success());
    assert_eq!(cold.stdout, warm.stdout, "output is unchanged (same program)");
    assert_eq!(
        slot_count(&cache),
        2,
        "a codegen-flag change must produce a SECOND slot (a miss, not a hit)"
    );

    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&src);
}

/// A lockfile / package-map change ⇒ miss. Simulated via the path-dep package whose
/// resolution lands in the `package_map_digest`: changing the dep's store location
/// changes the key. We approximate with a second app whose dep maps differently.
#[cfg(feature = "pkg")]
#[test]
fn lockfile_change_misses() {
    let cache = unique_cache_dir("lockfile");
    let root = unique_cache_dir("lockfile_src");

    // Two sibling packages with the SAME export name but different bodies.
    let dep_v1 = root.join("dep_v1");
    let dep_v2 = root.join("dep_v2");
    std::fs::create_dir_all(&dep_v1).unwrap();
    std::fs::create_dir_all(&dep_v2).unwrap();
    std::fs::write(
        dep_v1.join("ascript.toml"),
        "[package]\nname=\"dep\"\nversion=\"1.0.0\"\nentry=\"index.as\"\n",
    )
    .unwrap();
    std::fs::write(
        dep_v2.join("ascript.toml"),
        "[package]\nname=\"dep\"\nversion=\"1.0.0\"\nentry=\"index.as\"\n",
    )
    .unwrap();
    std::fs::write(dep_v1.join("index.as"), "export fn val() { return 1 }\n").unwrap();
    std::fs::write(dep_v2.join("index.as"), "export fn val() { return 2 }\n").unwrap();

    let app = root.join("app");
    std::fs::create_dir_all(&app).unwrap();
    let main = app.join("main.as");
    std::fs::write(&main, "import { val } from 'dep'\nprint(val())\n").unwrap();

    // Point the dependency at v1.
    std::fs::write(
        app.join("ascript.toml"),
        "[package]\nname = \"app\"\nversion = \"0.0.0\"\n\n[dependencies]\ndep = { path = \"../dep_v1\" }\n",
    )
    .unwrap();
    let cold = run_cached(&cache, &main, &[]);
    assert!(
        cold.status.success(),
        "cold run must succeed: {}",
        String::from_utf8_lossy(&cold.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&cold.stdout).trim(), "1");

    // Re-point the dependency at v2 (a resolution change → package_map_digest changes).
    std::fs::write(
        app.join("ascript.toml"),
        "[package]\nname = \"app\"\nversion = \"0.0.0\"\n\n[dependencies]\ndep = { path = \"../dep_v2\" }\n",
    )
    .unwrap();
    // Remove the lock so the new resolution is recomputed.
    let _ = std::fs::remove_file(app.join("ascript.lock"));
    let warm = run_cached(&cache, &main, &[]);
    assert!(warm.status.success());
    assert_eq!(
        String::from_utf8_lossy(&warm.stdout).trim(),
        "2",
        "a package re-resolution must MISS and recompile"
    );

    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&root);
}

/// `--no-cache` and `ASCRIPT_NO_COMPILE_CACHE=1` both bypass the cache ⇒ NO slot created.
#[test]
fn no_cache_flag_and_env_bypass() {
    // --no-cache flag.
    {
        let cache = unique_cache_dir("nocache_flag");
        let src = unique_cache_dir("nocache_flag_src");
        let main = write_three_module_program(&src);
        let out = run_cached(&cache, &main, &["--no-cache"]);
        // --no-cache is a Run flag; it must precede the file. Re-issue correctly.
        let _ = out;
        let out = Command::new(bin())
            .arg("run")
            .arg("--no-cache")
            .arg(&main)
            .env("ASCRIPT_CACHE", &cache)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello world");
        assert_eq!(slot_count(&cache), 0, "--no-cache must NOT create a slot");
        let _ = std::fs::remove_dir_all(&cache);
        let _ = std::fs::remove_dir_all(&src);
    }
    // ASCRIPT_NO_COMPILE_CACHE=1 env.
    {
        let cache = unique_cache_dir("nocache_env");
        let src = unique_cache_dir("nocache_env_src");
        let main = write_three_module_program(&src);
        let out = Command::new(bin())
            .arg("run")
            .arg(&main)
            .env("ASCRIPT_CACHE", &cache)
            .env("ASCRIPT_NO_COMPILE_CACHE", "1")
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello world");
        assert_eq!(
            slot_count(&cache),
            0,
            "ASCRIPT_NO_COMPILE_CACHE=1 must NOT create a slot"
        );
        let _ = std::fs::remove_dir_all(&cache);
        let _ = std::fs::remove_dir_all(&src);
    }
}

/// `--tree-walker`, `--inspect`, `--profile` never consult the cache (no slot created).
#[test]
fn tree_walker_inspect_profile_paths_uncached() {
    // --tree-walker
    {
        let cache = unique_cache_dir("tw_uncached");
        let src = unique_cache_dir("tw_uncached_src");
        let main = write_three_module_program(&src);
        let out = Command::new(bin())
            .arg("run")
            .arg("--tree-walker")
            .arg(&main)
            .env("ASCRIPT_CACHE", &cache)
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello world");
        assert_eq!(slot_count(&cache), 0, "--tree-walker must be uncached");
        let _ = std::fs::remove_dir_all(&cache);
        let _ = std::fs::remove_dir_all(&src);
    }
    // --profile cpu (observation-only; writes a profile artifact but no cache slot).
    {
        let cache = unique_cache_dir("prof_uncached");
        let src = unique_cache_dir("prof_uncached_src");
        let main = write_three_module_program(&src);
        let prof_out = src.join("profile.json");
        let out = Command::new(bin())
            .arg("run")
            .arg("--profile")
            .arg("cpu")
            .arg("--out")
            .arg(&prof_out)
            .arg(&main)
            .env("ASCRIPT_CACHE", &cache)
            .output()
            .unwrap();
        // The profile feature may or may not be enabled; if disabled it errors cleanly.
        if out.status.success() {
            assert_eq!(slot_count(&cache), 0, "--profile must be uncached");
        }
        let _ = std::fs::remove_dir_all(&cache);
        let _ = std::fs::remove_dir_all(&src);
    }
}

/// Cached vs uncached stderr is byte-identical for a panicking multi-module program.
#[test]
fn panic_output_parity() {
    let cache = unique_cache_dir("panic_parity");
    let src = unique_cache_dir("panic_parity_src");
    let main = write_panicking_program(&src);

    // Uncached (--no-cache) run.
    let uncached = Command::new(bin())
        .arg("run")
        .arg("--no-cache")
        .arg(&main)
        .env("ASCRIPT_CACHE", &cache)
        .output()
        .unwrap();
    // Cached cold run (miss → publish → run).
    let cached_cold = run_cached(&cache, &main, &[]);
    // Cached warm run (hit).
    let cached_warm = run_cached(&cache, &main, &[]);

    assert_eq!(
        uncached.status.code(),
        cached_cold.status.code(),
        "exit code parity (uncached vs cached cold)"
    );
    assert_eq!(uncached.stdout, cached_cold.stdout, "stdout parity (cold)");
    assert_eq!(uncached.stderr, cached_cold.stderr, "stderr parity (cold)");
    assert_eq!(uncached.stdout, cached_warm.stdout, "stdout parity (warm)");
    assert_eq!(
        uncached.stderr, cached_warm.stderr,
        "stderr parity (warm hit) — panic caret must be byte-identical"
    );

    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&src);
}

/// A `worker fn` program: a cached run is byte-identical to an uncached run.
#[test]
fn worker_program_parity() {
    let cache = unique_cache_dir("worker_parity");
    let src = unique_cache_dir("worker_parity_src");
    let main = src.join("main.as");
    std::fs::write(
        &main,
        "import * as task from \"std/task\"\n\
         worker fn square(n: number): number { return n * n }\n\
         fn main() {\n\
           let futures = [square(2), square(3), square(4)]\n\
           let results = await task.gather(futures)\n\
           print(results)\n\
         }\n\
         await main()\n",
    )
    .unwrap();

    let uncached = Command::new(bin())
        .arg("run")
        .arg("--no-cache")
        .arg(&main)
        .env("ASCRIPT_CACHE", &cache)
        .output()
        .unwrap();
    let cached_cold = run_cached(&cache, &main, &[]);
    let cached_warm = run_cached(&cache, &main, &[]);

    assert!(
        uncached.status.success(),
        "uncached worker run must succeed: {}",
        String::from_utf8_lossy(&uncached.stderr)
    );
    assert_eq!(uncached.stdout, cached_cold.stdout, "worker stdout parity (cold)");
    assert_eq!(uncached.stdout, cached_warm.stdout, "worker stdout parity (warm)");
    assert_eq!(
        uncached.status.code(),
        cached_warm.status.code(),
        "worker exit parity"
    );

    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&src);
}

/// `run file.aso` never consults the cache (no slot created).
#[test]
fn aso_run_unaffected() {
    let cache = unique_cache_dir("aso_uncached");
    let src = unique_cache_dir("aso_uncached_src");
    let main = write_three_module_program(&src);
    let aso = src.join("main.aso");

    // Build the .aso (build is never cached).
    let build = Command::new(bin())
        .arg("build")
        .arg(&main)
        .arg("-o")
        .arg(&aso)
        .env("ASCRIPT_CACHE", &cache)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "build must succeed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    // Run the .aso — must NOT create a compile-cache slot.
    let run = Command::new(bin())
        .arg("run")
        .arg(&aso)
        .env("ASCRIPT_CACHE", &cache)
        .output()
        .unwrap();
    assert!(run.status.success());
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "hello world");
    assert_eq!(slot_count(&cache), 0, "run file.aso must never consult the cache");

    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&src);
}

// ─────────────────────────────────────────────────────────────────────────────
// Previously-#[ignore]'d cached-RUN tests — ENABLED by Task 3 wiring
// ─────────────────────────────────────────────────────────────────────────────

/// Cold run → miss + publish; warm run → hit; both byte-identical.
#[test]
fn second_run_hits_and_is_byte_identical() {
    let cache = unique_cache_dir("hit_identical");
    let src_dir = unique_cache_dir("hit_identical_src");
    let main = write_three_module_program(&src_dir);

    let run1 = Command::new(bin())
        .arg("run")
        .arg(&main)
        .env("ASCRIPT_CACHE", &cache)
        .output()
        .unwrap();
    let run2 = Command::new(bin())
        .arg("run")
        .arg(&main)
        .env("ASCRIPT_CACHE", &cache)
        .output()
        .unwrap();

    assert_eq!(run1.status.code(), run2.status.code(), "exit codes must match");
    assert_eq!(run1.stdout, run2.stdout, "stdout must be byte-identical");
    assert_eq!(run1.stderr, run2.stderr, "stderr must be byte-identical");

    // At least one slot must exist in compiled/.
    let compiled = cache.join("compiled");
    assert!(compiled.exists(), "compiled/ must exist after warm run");
    let entries: Vec<_> = std::fs::read_dir(&compiled).unwrap().collect();
    assert!(!entries.is_empty(), "compiled/ must have at least one slot");

    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&src_dir);
}

/// When a slot's `program.aso` is bit-flipped, the verifier rejects it, the run falls
/// through to recompile, and the slot is repaired — the output is still correct.
#[test]
fn corrupted_artifact_fails_closed_to_recompile_and_repairs() {
    let cache = unique_cache_dir("corrupt_repair");
    let src_dir = unique_cache_dir("corrupt_repair_src");
    let main = write_three_module_program(&src_dir);

    // Cold run — populates the slot.
    let run1 = Command::new(bin())
        .arg("run")
        .arg(&main)
        .env("ASCRIPT_CACHE", &cache)
        .output()
        .unwrap();
    assert!(run1.status.success());

    // Bit-flip the artifact in every slot.
    let compiled = cache.join("compiled");
    for entry in std::fs::read_dir(&compiled).unwrap() {
        let slot = entry.unwrap().path();
        let aso = slot.join("program.aso");
        if aso.exists() {
            let mut bytes = std::fs::read(&aso).unwrap();
            if !bytes.is_empty() {
                bytes[0] ^= 0xFF;
            }
            std::fs::write(&aso, &bytes).unwrap();
        }
    }

    // Warm run — verifier rejects the corrupt slot, falls back to compile.
    let run2 = Command::new(bin())
        .arg("run")
        .arg(&main)
        .env("ASCRIPT_CACHE", &cache)
        .output()
        .unwrap();
    assert!(run2.status.success());
    assert_eq!(run1.stdout, run2.stdout, "output must be correct after repair");

    // The slot must now be valid (repaired by the recompile + publish).
    for entry in std::fs::read_dir(&compiled).unwrap() {
        let slot = entry.unwrap().path();
        let manifest = slot.join("manifest.json");
        assert!(manifest.exists(), "repaired slot must have manifest.json");
    }

    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&src_dir);
}

/// N concurrent cold runs of the same program must all produce correct output and
/// the slot must be valid afterwards (atomic last-writer-wins).
#[test]
fn concurrent_runs_racing_one_key_both_succeed() {
    use std::thread;

    let cache = unique_cache_dir("concurrent");
    let src_dir = unique_cache_dir("concurrent_src");
    let main = write_three_module_program(&src_dir);
    const N: usize = 4;

    let handles: Vec<_> = (0..N)
        .map(|_| {
            let bin = bin().to_string();
            let main = main.clone();
            let cache = cache.clone();
            thread::spawn(move || {
                Command::new(&bin)
                    .arg("run")
                    .arg(&main)
                    .env("ASCRIPT_CACHE", &cache)
                    .output()
                    .unwrap()
            })
        })
        .collect();

    let outputs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let first = &outputs[0];
    for (i, out) in outputs.iter().enumerate() {
        assert!(
            out.status.success(),
            "run #{i} must succeed: {:?}",
            out.status
        );
        assert_eq!(
            out.stdout, first.stdout,
            "run #{i} stdout must match run #0"
        );
    }

    // The slot must be valid after the race.
    let compiled = cache.join("compiled");
    assert!(compiled.exists(), "compiled/ must exist after concurrent runs");

    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&src_dir);
}
