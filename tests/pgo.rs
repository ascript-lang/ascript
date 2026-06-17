//! WARM B §3.1/§3.6 — end-to-end tests for `ascript build --pgo` (the PGO harvest).
//!
//! Each test spawns the real binary (`CARGO_BIN_EXE_ascript`), writes temp source files,
//! and asserts the produced artifact properties.  The tests follow the `tests/native.rs`
//! spawn-test precedent.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ascript")
}

/// A unique temp dir, cleaned up on drop.
struct TmpDir(PathBuf);

impl TmpDir {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("ascript_pgo_{}_{}", tag, std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        TmpDir(base)
    }
    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A program with a hot int loop (arith specialization), a hot monomorphic `o.x` read
/// (field IC), and a `math.abs` call (global cache).
fn pgo_training_source() -> &'static str {
    r#"
import * as math from "std/math"

// Hot int loop → arith specialization (ArithKind::Int)
let sum = 0
for (i in 1..=20) {
    sum = sum + i
}

// Monomorphic object with field "x" → field IC
// Read o.x 9 times to exceed WARMUP_THRESHOLD=8 and warm the field IC.
let o = { x: 42, y: sum }
let xv = o.x + o.x + o.x + o.x + o.x + o.x + o.x + o.x + o.x

// std/math call → global cache
let v = math.abs(-99)

print(sum)
print(v)
"#
}

// ── Test 1: `build --pgo` produces an ASCRIPTA archive with a valid PGO section ───────────

/// `ascript build prog.as --pgo -o out.aso` on a program with a hot int loop + a hot
/// monomorphic `o.x` read + a `math.abs` import call → the artifact is an `ASCRIPTA`
/// archive (even single-module) whose trailing PGO section decodes with:
///   ≥1 Specialized(Int) arith record (kind byte 0 = Int)
///   ≥1 field record whose key list contains "x"
///   ≥1 global record
/// AND the training run's stdout appeared (a real run, output lives, lines include
/// the loop sum and the abs result).
#[test]
fn build_pgo_produces_archive_with_warmstate() {
    let tmp = TmpDir::new("harvest");
    let src = tmp.join("prog.as");
    let out = tmp.join("prog.aso");

    std::fs::write(&src, pgo_training_source()).unwrap();

    let result = Command::new(bin())
        .args(["build", src.to_str().unwrap(), "--pgo", "-o", out.to_str().unwrap()])
        .output()
        .expect("failed to spawn ascript build --pgo");

    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        result.status.success(),
        "ascript build --pgo failed: stdout={stdout}\nstderr={stderr}"
    );

    // Training run output must be present (a real run happened).
    // The loop computes 1+2+…+20 = 210; math.abs(-99) = 99.
    assert!(
        stdout.contains("210"),
        "training run stdout must show loop sum (210); got: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("99"),
        "training run stdout must show abs result (99); got: {stdout}\nstderr: {stderr}"
    );

    // The artifact must exist.
    assert!(out.exists(), "output .aso was not created");

    let artifact_bytes = std::fs::read(&out).unwrap();

    // Single-module --pgo must emit an ASCRIPTA archive (not bare ASO\0).
    assert!(
        artifact_bytes.starts_with(b"ASCRIPTA"),
        "single-module --pgo must emit ASCRIPTA archive; got {:?}",
        &artifact_bytes[..8.min(artifact_bytes.len())]
    );

    // The archive must decode successfully.
    let archive = ascript::vm::archive::ModuleArchive::decode(&artifact_bytes)
        .expect("produced artifact must decode as ModuleArchive");
    assert!(
        !archive.modules.is_empty(),
        "archive must contain at least one module"
    );

    // Find and decode the PGO trailing section.
    let archive_len = {
        // Re-encode to find the archive end boundary: the section starts after it.
        // We use scan_trailing_sections with the re-encoded archive length as start.
        let canonical = archive.encode();
        canonical.len()
    };
    let pgo = ascript::vm::pgo::find_and_decode_pgo(&artifact_bytes, archive_len)
        .expect("PGO section must be present and decodeable");

    // Must have at least one module entry.
    assert!(!pgo.modules.is_empty(), "PGO section must record at least one module");

    // Collect all records across all modules and protos.
    let mut found_int_arith = false;
    let mut found_x_field = false;
    let mut found_global = false;

    for module in &pgo.modules {
        for proto in &module.protos {
            // Check arith records: kind byte 0 = Int
            for &(_off, kind) in &proto.arith {
                if kind == 0 {
                    found_int_arith = true;
                }
            }
            // Check field records: at least one key list must contain "x"
            for (_off, list_indices) in &proto.fields {
                for &idx in list_indices {
                    if let Some(klist) = pgo.key_lists.get(idx as usize) {
                        if klist.iter().any(|k| k == "x") {
                            found_x_field = true;
                        }
                    }
                }
            }
            // Check global records
            if !proto.globals.is_empty() {
                found_global = true;
            }
        }
    }

    assert!(
        found_int_arith,
        "PGO section must contain ≥1 Specialized(Int) arith record (kind=0); full PGO: {pgo:?}"
    );
    assert!(
        found_x_field,
        "PGO section must contain ≥1 field record with key list containing 'x'; full PGO: {pgo:?}"
    );
    assert!(
        found_global,
        "PGO section must contain ≥1 global record; full PGO: {pgo:?}"
    );
}

// ── Test 2: `build` WITHOUT `--pgo` is byte-identical to pre-WARM (no PGO section) ────────

/// `ascript build prog.as` (no `--pgo`) must emit a bare `ASO\0` chunk for a
/// single-module program — byte-identical to pre-WARM behavior.  The existing
/// native/build tests guard this too; we also verify it here directly.
#[test]
fn build_without_pgo_emits_bare_aso_for_single_module() {
    let tmp = TmpDir::new("no_pgo");
    let src = tmp.join("simple.as");
    let out = tmp.join("simple.aso");

    std::fs::write(&src, "print(42)\n").unwrap();

    let result = Command::new(bin())
        .args(["build", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("failed to spawn ascript build");

    assert!(
        result.status.success(),
        "build without --pgo failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let bytes = std::fs::read(&out).unwrap();

    // Single-module without --pgo MUST be a bare ASO\0, NOT ASCRIPTA.
    assert!(
        bytes.starts_with(b"ASO\0"),
        "build without --pgo must emit bare ASO\\0; got {:?}",
        &bytes[..4.min(bytes.len())]
    );
}

// ── Test 3: PGO artifact runs correctly ──────────────────────────────────────────────────

/// The PGO artifact produced by `build --pgo` must run correctly (same output as the
/// training program).
#[test]
fn pgo_artifact_runs_correctly() {
    let tmp = TmpDir::new("run");
    let src = tmp.join("prog.as");
    let out = tmp.join("prog.aso");

    std::fs::write(&src, pgo_training_source()).unwrap();

    // Build with --pgo
    let build_result = Command::new(bin())
        .args(["build", src.to_str().unwrap(), "--pgo", "-o", out.to_str().unwrap()])
        .output()
        .expect("failed to spawn ascript build --pgo");
    assert!(
        build_result.status.success(),
        "build --pgo failed: {}",
        String::from_utf8_lossy(&build_result.stderr)
    );

    // Run the artifact
    let run_result = Command::new(bin())
        .args(["run", out.to_str().unwrap()])
        .output()
        .expect("failed to run pgo artifact");

    let stdout = String::from_utf8_lossy(&run_result.stdout);
    assert!(
        run_result.status.success(),
        "running pgo artifact failed: {}",
        String::from_utf8_lossy(&run_result.stderr)
    );
    assert!(
        stdout.contains("210"),
        "pgo artifact must produce loop sum 210; got: {stdout}"
    );
    assert!(
        stdout.contains("99"),
        "pgo artifact must produce abs result 99; got: {stdout}"
    );
}

// ── Test 4: training run that panics still embeds a partial section ───────────────────────

/// A training run that panics must still embed a (possibly partial/empty) PGO section —
/// the build does not abort on a training panic. The artifact must be an ASCRIPTA archive
/// with a PGO section (even if empty).
#[test]
fn build_pgo_partial_section_on_training_panic() {
    let tmp = TmpDir::new("panic");
    let src = tmp.join("panicking.as");
    let out = tmp.join("panicking.aso");

    // A program that does some work then panics
    std::fs::write(
        &src,
        r#"
let x = 1 + 1
// Force a panic after some warmup
panic("deliberate training panic")
"#,
    )
    .unwrap();

    let result = Command::new(bin())
        .args(["build", src.to_str().unwrap(), "--pgo", "-o", out.to_str().unwrap()])
        .output()
        .expect("failed to spawn ascript build --pgo");

    // The BUILD itself must succeed (training panic is absorbed).
    assert!(
        result.status.success(),
        "build --pgo must succeed even when training run panics; stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    // The artifact must exist and be an ASCRIPTA archive.
    assert!(out.exists(), "artifact must be created even on training panic");
    let bytes = std::fs::read(&out).unwrap();
    assert!(
        bytes.starts_with(b"ASCRIPTA"),
        "panicking-training artifact must still be ASCRIPTA archive"
    );

    // A PGO section must be present (possibly empty, but present).
    let archive = ascript::vm::archive::ModuleArchive::decode(&bytes).unwrap();
    let archive_len = archive.encode().len();
    // The section is present (find_and_decode_pgo returns Some, even if the section
    // has no records — partial profile from a panicking run is acceptable).
    let _pgo = ascript::vm::pgo::find_and_decode_pgo(&bytes, archive_len)
        .expect("PGO section must be present even after a panicking training run");
}
