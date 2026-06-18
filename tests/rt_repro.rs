//! RT §9 — cross-flag reproducibility battery + `--report-json` schema lock.
//!
//! **Step 1 — double-build battery**: For each artifact form, the SAME program is built
//! TWICE (to different output paths) and:
//!   - The artifact bytes must be **bit-identical** (sha256 equal).
//!   - The `--report-json` must contain NO time/date/timestamp field (§9.1 guarantee).
//!
//! Forms covered:
//!   - `plain`           — bare `--native --stub $RT --no-fetch`
//!   - `compress`        — `--native --compress --stub $RT --no-fetch`
//!   - `target`          — `--native --target <host-triple> --stub $RT --no-fetch`
//!   - `oci`             — `--native --oci --stub $RT --no-fetch`  (`cfg(compress)`)
//!   - `compress_oci`    — `--native --compress --oci --stub $RT --no-fetch` (`cfg(compress)`)
//!
//! All gated on `ASCRIPT_RT_BIN`. When unset the tests self-skip with a printed reason.
//!
//! **Step 2 — schema lock** is in `tests/rt_select.rs` (`report_json_schema_lock`).
//!
//! # To run locally
//!
//! ```bash
//! scripts/build-rt.sh rt-full
//! ASCRIPT_RT_BIN=$PWD/target/release/ascript-rt cargo test --test rt_repro
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn toolchain_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ascript")
}

fn rt_bin() -> Option<String> {
    std::env::var("ASCRIPT_RT_BIN").ok()
}

struct TmpDir(PathBuf);

impl std::ops::Deref for TmpDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tmp_dir(tag: &str) -> TmpDir {
    let d = std::env::temp_dir().join(format!(
        "ascript_rt_repro_{}_{}",
        tag,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    TmpDir(d)
}

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

/// sha256 of `bytes`, returned as lowercase hex.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let d: [u8; 32] = Sha256::digest(bytes).into();
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// The host Rust target triple (same as the `--target` the host toolchain was compiled for).
fn host_triple() -> String {
    // Use rustc to get the host triple — available in any Cargo environment.
    let out = Command::new("rustc")
        .args(["-vV"])
        .output()
        .expect("rustc -vV");
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("host: ") {
            return rest.trim().to_string();
        }
    }
    // Fallback: use the compile-time triple.
    std::env::var("TARGET")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "x86_64-unknown-linux-musl".to_string())
}

// ─── §9.1 helpers ─────────────────────────────────────────────────────────────

/// Assert the JSON string contains no time/date field (§9.1 determinism contract).
/// Any of these key names in the JSON is a determinism violation:
///   `created`, `timestamp`, `time`, `date`, `built_at`, `now`, `built`.
fn assert_no_time_fields(json: &str, label: &str) {
    for forbidden in [
        "\"created\"",
        "\"timestamp\"",
        "\"time\"",
        "\"date\"",
        "\"built_at\"",
        "\"now\"",
        "\"built\"",
    ] {
        assert!(
            !json.contains(forbidden),
            "§9.1 violation [{label}]: report JSON must NOT contain a time field \
             but found {forbidden} in:\n{json}"
        );
    }
}

/// Run `ascript build --native` with the given extra `args`, capturing `--report-json -`
/// on stdout. Returns `(artifact_sha256, report_json_string)`.
///
/// The function always passes:
///   - `--stub <rt>` (pinned stub for determinism)
///   - `--no-fetch` (offline, no network rung)
///   - `--report-json -` (JSON on stdout)
///   - `src` → output at `out`
fn build_and_capture(
    src: &Path,
    out: &Path,
    rt: &str,
    extra_args: &[&str],
) -> (String, String) {
    let mut cmd = Command::new(toolchain_bin());
    cmd.arg("build")
        .arg("--native")
        .arg("--stub")
        .arg(rt)
        .arg("--no-fetch")
        .arg("--report-json")
        .arg("-")
        .arg(src)
        .arg("-o")
        .arg(out)
        .args(extra_args);
    let o = cmd.output().expect("spawn ascript build");
    assert!(
        o.status.success(),
        "build failed (extra_args={extra_args:?}):\n  stdout={}\n  stderr={}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(out.exists(), "build did not produce {}", out.display());

    let artifact_bytes = std::fs::read(out).unwrap();
    let artifact_sha256 = sha256_hex(&artifact_bytes);

    // The JSON line is the line starting with '{' on stdout.
    let stdout = String::from_utf8(o.stdout).unwrap();
    let json_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .map(|l| l.to_string())
        .unwrap_or_else(|| {
            panic!(
                "no JSON line on stdout (extra_args={extra_args:?}):\n{stdout}"
            )
        });

    (artifact_sha256, json_line)
}

/// Core battery assertion: build twice, assert identical artifact sha256 + no time fields.
fn assert_double_build_identical(
    label: &str,
    src: &Path,
    dir: &TmpDir,
    rt: &str,
    extra_args: &[&str],
) {
    let out1 = dir.join(format!("{label}_out1"));
    let out2 = dir.join(format!("{label}_out2"));

    let (sha1, json1) = build_and_capture(src, &out1, rt, extra_args);
    let (sha2, json2) = build_and_capture(src, &out2, rt, extra_args);

    // 1. Artifact bytes must be bit-identical.
    assert_eq!(
        sha1, sha2,
        "§9.1 DETERMINISM FAILURE [{label}]: two builds of the same source produced \
         DIFFERENT artifact bytes.\n  build-1 sha256: {sha1}\n  build-2 sha256: {sha2}\n\
         (This is a real determinism bug — do NOT loosen the assertion; find and fix the \
         non-determinism: a timestamp, unsorted map, or random value.)"
    );

    // 2. The report JSON must contain no time/date fields.
    assert_no_time_fields(&json1, label);
    assert_no_time_fields(&json2, label);

    // 3. The two report JSONs differ only in `source` and `output` path fields.
    //    Blank those two fields and assert the rest is byte-identical.
    let blank_paths = |j: &str| -> String {
        // Replace "source":"<value>", "output":"<value>" with fixed placeholders.
        // We use a simple approach: find each key and replace its string value.
        replace_json_str(
            &replace_json_str(j, "source", "PATH_PLACEHOLDER"),
            "output",
            "PATH_PLACEHOLDER",
        )
    };

    let j1_blanked = blank_paths(&json1);
    let j2_blanked = blank_paths(&json2);
    assert_eq!(
        j1_blanked, j2_blanked,
        "§9.1 DETERMINISM FAILURE [{label}]: the two --report-json documents differ \
         in fields other than 'source'/'output' (which legitimately differ).\n\
         build-1 (paths blanked):\n  {j1_blanked}\n\
         build-2 (paths blanked):\n  {j2_blanked}"
    );
}

/// Replace a JSON string value for key `k` with `replacement` (simple, not full JSON parser).
fn replace_json_str(json: &str, k: &str, replacement: &str) -> String {
    let needle = format!("\"{}\":\"", k);
    if let Some(start) = json.find(&needle) {
        let after_quote = start + needle.len();
        // Find the closing quote (skip escaped quotes).
        let chars: Vec<char> = json[after_quote..].chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '\\' {
                i += 2; // skip escaped char
            } else if chars[i] == '"' {
                break;
            } else {
                i += 1;
            }
        }
        let end_in_tail = json[after_quote..]
            .char_indices()
            .take(i + 1)
            .last()
            .map(|(byte_idx, c)| byte_idx + c.len_utf8())
            .unwrap_or(0);
        let idx = after_quote + end_in_tail;
        // Replace from `start` (opening `"k":`) to `idx` (closing `"`).
        format!(
            "{}\"{}\":\"{}\"{}",
            &json[..start],
            k,
            replacement,
            &json[idx..]
        )
    } else {
        json.to_string()
    }
}

// ── Battery test 1: plain --native ────────────────────────────────────────────

/// §9.1 reproducibility — plain `--native` with an explicit `--stub`: same source built
/// twice must produce bit-identical artifacts and a report with no time fields.
#[test]
fn repro_plain_native_is_deterministic() {
    let rt = match rt_bin() {
        Some(b) => b,
        None => {
            eprintln!(
                "[rt_repro] SKIP repro_plain_native_is_deterministic — ASCRIPT_RT_BIN not set"
            );
            return;
        }
    };
    let dir = tmp_dir("plain");
    let src = write(&dir, "prog.as", "print(\"determinism check\")\n");
    assert_double_build_identical("plain", &src, &dir, &rt, &[]);
}

// ── Battery test 2: --compress ────────────────────────────────────────────────

/// §9.1 reproducibility — `--compress` (zstd payload): same source built twice must
/// produce bit-identical compressed artifacts.
#[test]
fn repro_compress_is_deterministic() {
    let rt = match rt_bin() {
        Some(b) => b,
        None => {
            eprintln!(
                "[rt_repro] SKIP repro_compress_is_deterministic — ASCRIPT_RT_BIN not set"
            );
            return;
        }
    };
    let dir = tmp_dir("compress");
    let src = write(&dir, "prog.as", "print(\"compressed determinism\")\n");
    assert_double_build_identical("compress", &src, &dir, &rt, &["--compress"]);
}

// ── Battery test 3: --target (cross, using host triple) ──────────────────────

/// §9.1 reproducibility — `--target <host-triple>` (same stub, explicit target matching
/// the host): same source built twice must produce bit-identical artifacts.
///
/// Using the host triple means we can use the rt stub directly (no actual cross-compile;
/// the stub bytes are the same as without `--target`). This exercises the target→report
/// path while staying hermetic.
#[test]
fn repro_target_is_deterministic() {
    let rt = match rt_bin() {
        Some(b) => b,
        None => {
            eprintln!(
                "[rt_repro] SKIP repro_target_is_deterministic — ASCRIPT_RT_BIN not set"
            );
            return;
        }
    };
    let triple = host_triple();
    // Only musl and darwin triples appear in SUPPORTED_TARGETS. If the host triple
    // is not in the supported set, skip with a note (we can't force an unsupported target).
    // The point is to exercise the target=Some(...) path — not to cross-compile.
    let dir = tmp_dir("target");
    let src = write(&dir, "prog.as", "print(\"target determinism\")\n");

    // Try with the host triple; if rejected (unsupported), skip gracefully.
    let probe = Command::new(toolchain_bin())
        .arg("build")
        .arg("--native")
        .arg("--stub")
        .arg(&rt)
        .arg("--no-fetch")
        .arg("--target")
        .arg(&triple)
        .arg(&src)
        .arg("-o")
        .arg(dir.join("probe_out"))
        .output()
        .expect("spawn probe build");
    if !probe.status.success() {
        let stderr = String::from_utf8_lossy(&probe.stderr);
        eprintln!(
            "[rt_repro] SKIP repro_target_is_deterministic — host triple '{triple}' \
             not accepted by --target (likely not in SUPPORTED_TARGETS): {stderr}"
        );
        return;
    }

    assert_double_build_identical("target", &src, &dir, &rt, &["--target", &triple]);
}

// ── Battery test 4: --oci (cfg compress) ─────────────────────────────────────

/// §9.1 reproducibility — `--oci`: same source built twice must produce bit-identical
/// OCI tarballs. Extends the existing `oci_double_build_is_deterministic` pattern to
/// include the `--stub`/`--no-fetch` + report-JSON checks.
#[cfg(feature = "compress")]
#[test]
fn repro_oci_is_deterministic() {
    let rt = match rt_bin() {
        Some(b) => b,
        None => {
            eprintln!("[rt_repro] SKIP repro_oci_is_deterministic — ASCRIPT_RT_BIN not set");
            return;
        }
    };
    let dir = tmp_dir("oci");
    let src = write(&dir, "prog.as", "print(\"oci determinism\")\n");
    let out1 = dir.join("prog_oci1.tar");
    let out2 = dir.join("prog_oci2.tar");

    // Build twice with SOURCE_DATE_EPOCH=0 for deterministic OCI timestamps.
    let build_oci = |out: &Path| {
        let o = Command::new(toolchain_bin())
            .arg("build")
            .arg("--native")
            .arg("--oci")
            .arg("--stub")
            .arg(&rt)
            .arg("--no-fetch")
            .arg("--report-json")
            .arg("-")
            .arg(&src)
            .arg("-o")
            .arg(out)
            .env("SOURCE_DATE_EPOCH", "0")
            .output()
            .expect("spawn oci build");
        assert!(
            o.status.success(),
            "oci build failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        let bytes = std::fs::read(out).unwrap();
        let sha = sha256_hex(&bytes);
        let stdout = String::from_utf8(o.stdout).unwrap();
        let json = stdout
            .lines()
            .find(|l| l.trim_start().starts_with('{'))
            .map(|l| l.to_string())
            .unwrap_or_else(|| panic!("no JSON line on stdout: {stdout}"));
        (sha, json)
    };

    let (sha1, json1) = build_oci(&out1);
    let (sha2, json2) = build_oci(&out2);

    // Bit-identical OCI tarball.
    assert_eq!(
        sha1, sha2,
        "§9.1 DETERMINISM FAILURE [oci]: two --oci builds produced DIFFERENT bytes.\n\
         build-1 sha256: {sha1}\nbuild-2 sha256: {sha2}"
    );

    // No time fields in the report JSON.
    assert_no_time_fields(&json1, "oci");
    assert_no_time_fields(&json2, "oci");

    // Report fields other than source/output must be identical.
    let blank_paths = |j: &str| -> String {
        replace_json_str(&replace_json_str(j, "source", "P"), "output", "P")
    };
    assert_eq!(
        blank_paths(&json1),
        blank_paths(&json2),
        "§9.1 [oci]: report JSON differs beyond source/output paths"
    );
}

// ── Battery test 5: --compress --oci ─────────────────────────────────────────

/// §9.1 reproducibility — `--compress --oci` (composed): bit-identical OCI tarballs
/// with a zstd-compressed inner payload.
#[cfg(feature = "compress")]
#[test]
fn repro_compress_oci_is_deterministic() {
    let rt = match rt_bin() {
        Some(b) => b,
        None => {
            eprintln!(
                "[rt_repro] SKIP repro_compress_oci_is_deterministic — ASCRIPT_RT_BIN not set"
            );
            return;
        }
    };
    let dir = tmp_dir("compress_oci");
    let src = write(&dir, "prog.as", "print(\"compress+oci determinism\")\n");
    let out1 = dir.join("prog_coci1.tar");
    let out2 = dir.join("prog_coci2.tar");

    let build_coci = |out: &Path| {
        let o = Command::new(toolchain_bin())
            .arg("build")
            .arg("--native")
            .arg("--compress")
            .arg("--oci")
            .arg("--stub")
            .arg(&rt)
            .arg("--no-fetch")
            .arg("--report-json")
            .arg("-")
            .arg(&src)
            .arg("-o")
            .arg(out)
            .env("SOURCE_DATE_EPOCH", "0")
            .output()
            .expect("spawn compress+oci build");
        assert!(
            o.status.success(),
            "compress+oci build failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        let bytes = std::fs::read(out).unwrap();
        let sha = sha256_hex(&bytes);
        let stdout = String::from_utf8(o.stdout).unwrap();
        let json = stdout
            .lines()
            .find(|l| l.trim_start().starts_with('{'))
            .map(|l| l.to_string())
            .unwrap_or_else(|| panic!("no JSON line on stdout: {stdout}"));
        (sha, json)
    };

    let (sha1, json1) = build_coci(&out1);
    let (sha2, json2) = build_coci(&out2);

    assert_eq!(
        sha1, sha2,
        "§9.1 DETERMINISM FAILURE [compress+oci]: two builds produced DIFFERENT bytes.\n\
         build-1 sha256: {sha1}\nbuild-2 sha256: {sha2}"
    );

    assert_no_time_fields(&json1, "compress+oci");
    assert_no_time_fields(&json2, "compress+oci");

    let blank_paths = |j: &str| -> String {
        replace_json_str(&replace_json_str(j, "source", "P"), "output", "P")
    };
    assert_eq!(
        blank_paths(&json1),
        blank_paths(&json2),
        "§9.1 [compress+oci]: report JSON differs beyond source/output paths"
    );
}
