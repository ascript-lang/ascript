//! Integration tests for the WARM compile cache (Task 2).
//!
//! Tests that require the wired `run` path (Task 3) are marked with
//! `// TODO(Task 3): enable when run wiring is complete` and are either skipped
//! or structured so that the store-level behaviour (publish, lookup, validate) is
//! already exercised here.

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
// Tests that require Task 3 (wired run path) — deferred
// ─────────────────────────────────────────────────────────────────────────────

/// TODO(Task 3): enable when run wiring is complete.
///
/// When Task 3 wires `ascript run` to check the compile cache, this test should:
///   1. Run the program cold (cold run → cache miss → compile + publish).
///   2. Run again (warm run → cache hit → skip compile).
///   3. Assert stdout/stderr/exit_code are byte-identical across both runs.
///   4. Assert that a compile-key slot exists in compiled/.
#[test]
#[ignore = "requires Task 3: ascript run cache wiring not yet complete"]
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

/// TODO(Task 3): enable when run wiring is complete.
///
/// When a slot's `program.aso` is bit-flipped, the verifier rejects it, the run falls
/// through to recompile, and the slot is repaired — the output is still correct.
#[test]
#[ignore = "requires Task 3: ascript run cache wiring not yet complete"]
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

/// TODO(Task 3): enable when run wiring is complete.
///
/// N concurrent cold runs of the same program must all produce correct output and
/// the slot must be valid afterwards (atomic last-writer-wins).
#[test]
#[ignore = "requires Task 3: ascript run cache wiring not yet complete"]
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
