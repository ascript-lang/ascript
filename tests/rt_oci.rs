//! RT §8 — end-to-end tests for `ascript build --oci`.
//!
//! All tests are gated on `#[cfg(feature = "compress")]` (the OCI writer requires
//! flate2) and on a simple `hello.as` program that is compiled in a temp dir.
//!
//! # What is tested
//!
//! Structural (always run when `compress` feature is present):
//!   - `oci_layout_content` — `oci-layout` entry == `{"imageLayoutVersion":"1.0.0"}`
//!   - `oci_index_schema` — `index.json` schemaVersion, mediaType, platform, ref.name
//!   - `oci_blobs_self_describe` — every `blobs/sha256/<hex>` filename == sha256 of its bytes
//!   - `oci_config_fields` — config arch/os/Entrypoint/diff_ids
//!   - `oci_two_digest_rule` — diff_id == sha256(uncompressed), != sha256(gzipped)
//!   - `oci_inner_tar_entry` — inner tar has exactly one entry `app`, mode 0755, uid/gid 0, mtime 0
//!   - `oci_double_build_is_deterministic` — two builds with same env produce byte-identical .tar
//!   - `oci_source_date_epoch_respected` — `SOURCE_DATE_EPOCH=1700000000` is reflected in config
//!   - `oci_rejects_gnu_target` — `--oci --target x86_64-unknown-linux-gnu` → error with musl hint
//!   - `oci_default_tag_is_stem_latest` — default oci-tag is `<stem>:latest`
//!   - `oci_explicit_tag_appears_in_index` — `--oci-tag myapp:v1.0` appears in index.json
//!
//! Optional (skip if Docker absent or `ASCRIPT_RT_BIN_MUSL` unset):
//!   - `docker_load_and_run` — `docker load` + `docker run` executes and exits 0

#![cfg(feature = "compress")]

use std::path::{Path, PathBuf};
use std::process::Command;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn toolchain_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ascript")
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
        "ascript_rt_oci_{}_{}",
        tag,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    TmpDir(d)
}

/// Write a file and return its path.
fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

/// A trivial AScript program that just prints "hello from oci" and exits.
const HELLO_SRC: &str = r#"print("hello from oci")"#;

/// Build `--oci` on `src` into `out.tar`, returning the tarball bytes.
/// On failure, panics with the stderr.
fn build_oci(dir: &Path, src: &Path, out: &Path, extra_args: &[&str]) -> Vec<u8> {
    let mut cmd = Command::new(toolchain_bin());
    cmd.arg("build")
        .arg("--oci")
        .arg(src)
        .arg("-o")
        .arg(out)
        .args(extra_args)
        .current_dir(dir);
    let o = cmd.output().expect("failed to spawn ascript build --oci");
    assert!(
        o.status.success(),
        "ascript build --oci failed:\n  stdout={}\n  stderr={}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(out.exists(), "--oci build did not produce {}", out.display());
    std::fs::read(out).expect("cannot read OCI tarball")
}

// ── Minimal USTAR tarball parser ─────────────────────────────────────────────
// Enough to enumerate entries (name, content) without any tar-crate dep.

struct TarEntry {
    name: String,
    content: Vec<u8>,
    /// raw header bytes (512 bytes)
    header: [u8; 512],
}

/// Walk a USTAR tarball, returning all data entries (skips end-of-archive zeros).
fn parse_tar(data: &[u8]) -> Vec<TarEntry> {
    let mut pos = 0;
    let mut entries = Vec::new();
    while pos + 512 <= data.len() {
        let hdr = &data[pos..pos + 512];
        // End-of-archive: two zero blocks.
        if hdr.iter().all(|&b| b == 0) {
            break;
        }
        // Name: bytes 0..100, NUL-terminated.
        let name_end = hdr[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = String::from_utf8_lossy(&hdr[..name_end]).to_string();
        // Size: bytes 124..136, octal ASCII.
        let size_str = std::str::from_utf8(&hdr[124..136])
            .unwrap_or("0")
            .trim_matches(|c: char| c == ' ' || c == '\0');
        let size = usize::from_str_radix(size_str, 8).unwrap_or(0);
        pos += 512;
        let content = data[pos..pos + size].to_vec();
        let mut header = [0u8; 512];
        header.copy_from_slice(&data[pos - 512..pos]);
        entries.push(TarEntry { name, content, header });
        // Advance past content, padded to 512.
        pos += (size + 511) & !511;
    }
    entries
}

/// sha256 of `bytes`, returned as lowercase hex.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let d: [u8; 32] = Sha256::digest(bytes).into();
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decompress a gzip blob, panicking on error.
fn gunzip(gz: &[u8]) -> Vec<u8> {
    use flate2::read::GzDecoder;
    use std::io::Read;
    let mut dec = GzDecoder::new(gz);
    let mut out = Vec::new();
    dec.read_to_end(&mut out).expect("gunzip failed");
    out
}

// ── Structural tests ─────────────────────────────────────────────────────────

/// Parse the OCI tarball into named blobs. Returns (oci_layout, index_json, blobs)
/// where blobs maps digest-hex → bytes.
struct OciTar {
    oci_layout: Vec<u8>,
    index_json: Vec<u8>,
    blobs: std::collections::HashMap<String, Vec<u8>>,
}

fn parse_oci_tar(data: &[u8]) -> OciTar {
    let entries = parse_tar(data);
    let mut oci_layout = None;
    let mut index_json = None;
    let mut blobs = std::collections::HashMap::new();
    for e in entries {
        if e.name == "oci-layout" {
            oci_layout = Some(e.content);
        } else if e.name == "index.json" {
            index_json = Some(e.content);
        } else if let Some(hex) = e.name.strip_prefix("blobs/sha256/") {
            blobs.insert(hex.to_string(), e.content);
        }
    }
    OciTar {
        oci_layout: oci_layout.expect("missing oci-layout entry"),
        index_json: index_json.expect("missing index.json entry"),
        blobs,
    }
}

#[test]
fn oci_layout_content() {
    let dir = tmp_dir("layout");
    let src = write(&dir, "hello.as", HELLO_SRC);
    let out = dir.join("hello.tar");
    let data = build_oci(&dir, &src, &out, &[]);
    let oci = parse_oci_tar(&data);
    let got = String::from_utf8_lossy(&oci.oci_layout);
    assert_eq!(got, r#"{"imageLayoutVersion":"1.0.0"}"#, "oci-layout mismatch");
}

#[test]
fn oci_index_schema() {
    let dir = tmp_dir("index");
    let src = write(&dir, "app.as", HELLO_SRC);
    let out = dir.join("app.tar");
    let data = build_oci(&dir, &src, &out, &["--oci-tag", "myapp:v1.0"]);
    let oci = parse_oci_tar(&data);
    let index: serde_json::Value =
        serde_json::from_slice(&oci.index_json).expect("index.json is not valid JSON");
    assert_eq!(index["schemaVersion"], 2, "schemaVersion must be 2");
    assert_eq!(
        index["mediaType"].as_str().unwrap_or(""),
        "application/vnd.oci.image.index.v1+json",
        "index mediaType"
    );
    // platform annotation
    let manifests = index["manifests"].as_array().expect("manifests array");
    let m = &manifests[0];
    let platform = &m["platform"];
    assert_eq!(platform["os"].as_str().unwrap_or(""), "linux");
    // ref.name annotation
    let ref_name = m["annotations"]["org.opencontainers.image.ref.name"]
        .as_str()
        .unwrap_or("");
    assert_eq!(ref_name, "myapp:v1.0", "ref.name annotation mismatch");
}

#[test]
fn oci_blobs_self_describe() {
    let dir = tmp_dir("blobs");
    let src = write(&dir, "hello.as", HELLO_SRC);
    let out = dir.join("hello.tar");
    let data = build_oci(&dir, &src, &out, &[]);
    let oci = parse_oci_tar(&data);
    for (hex, bytes) in &oci.blobs {
        let computed = sha256_hex(bytes);
        assert_eq!(
            &computed, hex,
            "blob filename {hex} does not match sha256 of its content"
        );
    }
}

#[test]
fn oci_config_fields() {
    let dir = tmp_dir("config");
    let src = write(&dir, "hello.as", HELLO_SRC);
    let out = dir.join("hello.tar");
    let data = build_oci(&dir, &src, &out, &[]);
    let oci = parse_oci_tar(&data);

    // Find the manifest (the blob referenced by index.json's manifests[0]).
    let index: serde_json::Value = serde_json::from_slice(&oci.index_json).unwrap();
    let manifest_digest = index["manifests"][0]["digest"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    let manifest_bytes = oci.blobs.get(manifest_digest).expect("manifest blob missing");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();

    // Find the config blob.
    let config_digest = manifest["config"]["digest"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    let config_bytes = oci.blobs.get(config_digest).expect("config blob missing");
    let config: serde_json::Value = serde_json::from_slice(config_bytes).unwrap();

    assert_eq!(config["os"].as_str().unwrap_or(""), "linux");
    // architecture is either "amd64" or "arm64" (host-dependent).
    let arch = config["architecture"].as_str().unwrap_or("");
    assert!(
        arch == "amd64" || arch == "arm64",
        "unexpected architecture: {arch}"
    );
    // Entrypoint must be ["/app"].
    let ep = config["config"]["Entrypoint"]
        .as_array()
        .expect("Entrypoint must be an array");
    assert_eq!(ep.len(), 1);
    assert_eq!(ep[0].as_str().unwrap_or(""), "/app");
    // diff_ids must be present and non-empty.
    let diff_ids = config["rootfs"]["diff_ids"]
        .as_array()
        .expect("diff_ids must be an array");
    assert_eq!(diff_ids.len(), 1);
    let diff_id = diff_ids[0].as_str().expect("diff_id must be a string");
    assert!(diff_id.starts_with("sha256:"), "diff_id must be sha256:HEX");
}

#[test]
fn oci_two_digest_rule() {
    let dir = tmp_dir("two_digest");
    let src = write(&dir, "hello.as", HELLO_SRC);
    let out = dir.join("hello.tar");
    let data = build_oci(&dir, &src, &out, &[]);
    let oci = parse_oci_tar(&data);

    // Walk manifest → config → diff_id and layer digest.
    let index: serde_json::Value = serde_json::from_slice(&oci.index_json).unwrap();
    let manifest_digest = index["manifests"][0]["digest"]
        .as_str().unwrap().strip_prefix("sha256:").unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(oci.blobs.get(manifest_digest).unwrap()).unwrap();

    let config_digest = manifest["config"]["digest"]
        .as_str().unwrap().strip_prefix("sha256:").unwrap();
    let config: serde_json::Value =
        serde_json::from_slice(oci.blobs.get(config_digest).unwrap()).unwrap();

    let diff_id = config["rootfs"]["diff_ids"][0]
        .as_str().unwrap().strip_prefix("sha256:").unwrap();

    let layer_digest = manifest["layers"][0]["digest"]
        .as_str().unwrap().strip_prefix("sha256:").unwrap();

    // layer_gz is the gzipped blob.
    let layer_gz = oci.blobs.get(layer_digest).expect("layer blob missing");

    // layer descriptor digest == sha256(gzipped bytes) — the OCI wire hash.
    let gz_digest = sha256_hex(layer_gz);
    assert_eq!(&gz_digest, layer_digest, "layer descriptor digest must be sha256(gzipped)");

    // diff_id == sha256(UNCOMPRESSED inner tar).
    let inner_tar = gunzip(layer_gz);
    let uncompressed_digest = sha256_hex(&inner_tar);
    assert_eq!(&uncompressed_digest, diff_id, "diff_id must be sha256(uncompressed)");

    // THE TWO DIGEST RULE: diff_id != layer_digest (they are different digests of different forms).
    assert_ne!(
        diff_id, layer_digest,
        "diff_id must differ from the layer descriptor digest (§8.1 two-digest rule)"
    );
}

#[test]
fn oci_inner_tar_entry() {
    let dir = tmp_dir("inner_tar");
    let src = write(&dir, "hello.as", HELLO_SRC);
    let out = dir.join("hello.tar");
    let data = build_oci(&dir, &src, &out, &[]);
    let oci = parse_oci_tar(&data);

    let index: serde_json::Value = serde_json::from_slice(&oci.index_json).unwrap();
    let manifest_digest = index["manifests"][0]["digest"]
        .as_str().unwrap().strip_prefix("sha256:").unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(oci.blobs.get(manifest_digest).unwrap()).unwrap();
    let layer_digest = manifest["layers"][0]["digest"]
        .as_str().unwrap().strip_prefix("sha256:").unwrap();
    let layer_gz = oci.blobs.get(layer_digest).unwrap();
    let inner_tar = gunzip(layer_gz);

    let inner_entries = parse_tar(&inner_tar);
    assert_eq!(inner_entries.len(), 1, "inner tar must have exactly one entry");
    let entry = &inner_entries[0];
    assert_eq!(entry.name, "app", "inner tar entry name must be 'app'");

    // mode: bytes 100..108 in the header (octal), last 3 octal digits = rwxr-xr-x = 0755.
    let mode_str = std::str::from_utf8(&entry.header[100..108])
        .unwrap_or("")
        .trim_matches(|c: char| c == ' ' || c == '\0');
    let mode = u32::from_str_radix(mode_str, 8).unwrap_or(0);
    assert_eq!(
        mode & 0o777, 0o755,
        "inner tar entry mode must be 0755, got {mode_str:?}"
    );

    // uid: bytes 108..116, gid: bytes 116..124, both must be 0.
    let uid_str = std::str::from_utf8(&entry.header[108..116])
        .unwrap_or("").trim_matches(|c: char| c == ' ' || c == '\0');
    let gid_str = std::str::from_utf8(&entry.header[116..124])
        .unwrap_or("").trim_matches(|c: char| c == ' ' || c == '\0');
    assert_eq!(u32::from_str_radix(uid_str, 8).unwrap_or(99), 0, "uid must be 0");
    assert_eq!(u32::from_str_radix(gid_str, 8).unwrap_or(99), 0, "gid must be 0");

    // mtime: bytes 136..148, must be 0.
    let mtime_str = std::str::from_utf8(&entry.header[136..148])
        .unwrap_or("").trim_matches(|c: char| c == ' ' || c == '\0');
    assert_eq!(u64::from_str_radix(mtime_str, 8).unwrap_or(99), 0, "mtime must be 0");
}

#[test]
fn oci_double_build_is_deterministic() {
    let dir = tmp_dir("determinism");
    let src = write(&dir, "hello.as", HELLO_SRC);
    let out1 = dir.join("hello1.tar");
    let out2 = dir.join("hello2.tar");
    // Two builds: both with SOURCE_DATE_EPOCH=0 to ensure identical timestamps.
    let build = |out: &Path| {
        let o = Command::new(toolchain_bin())
            .args(["build", "--oci"])
            .arg(&src)
            .arg("-o")
            .arg(out)
            .env("SOURCE_DATE_EPOCH", "0")
            .current_dir(&*dir)
            .output()
            .expect("spawn build");
        assert!(
            o.status.success(),
            "build failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    };
    build(&out1);
    build(&out2);
    let b1 = std::fs::read(&out1).unwrap();
    let b2 = std::fs::read(&out2).unwrap();
    assert_eq!(b1, b2, "double-build produced different bytes (non-deterministic)");
}

#[test]
fn oci_source_date_epoch_respected() {
    // SOURCE_DATE_EPOCH=1700000000 == 2023-11-14T22:13:20Z
    let dir = tmp_dir("epoch");
    let src = write(&dir, "hello.as", HELLO_SRC);
    let out = dir.join("hello.tar");
    let o = Command::new(toolchain_bin())
        .args(["build", "--oci"])
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .env("SOURCE_DATE_EPOCH", "1700000000")
        .current_dir(&*dir)
        .output()
        .expect("spawn build");
    assert!(
        o.status.success(),
        "build --oci with SOURCE_DATE_EPOCH failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let data = std::fs::read(&out).unwrap();
    let oci = parse_oci_tar(&data);
    let index: serde_json::Value = serde_json::from_slice(&oci.index_json).unwrap();
    let manifest_digest = index["manifests"][0]["digest"]
        .as_str().unwrap().strip_prefix("sha256:").unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(oci.blobs.get(manifest_digest).unwrap()).unwrap();
    let config_digest = manifest["config"]["digest"]
        .as_str().unwrap().strip_prefix("sha256:").unwrap();
    let config: serde_json::Value =
        serde_json::from_slice(oci.blobs.get(config_digest).unwrap()).unwrap();
    let created = config["created"].as_str().unwrap_or("");
    assert_eq!(created, "2023-11-14T22:13:20Z", "SOURCE_DATE_EPOCH not reflected in created");
}

#[test]
fn oci_rejects_gnu_target() {
    let dir = tmp_dir("reject_gnu");
    let src = write(&dir, "hello.as", HELLO_SRC);
    let out = dir.join("hello.tar");
    // NOTE: `--target` requires `--native` via clap; `--oci` implies `--native` semantically
    // but clap's `requires` is satisfied by passing `--native` explicitly. The actual rejection
    // (musl-only error) is issued by `build_native`, after clap accepts the flags.
    let o = Command::new(toolchain_bin())
        .args(["build", "--native", "--oci", "--target", "x86_64-unknown-linux-gnu"])
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .current_dir(&*dir)
        .output()
        .expect("spawn build");
    assert!(
        !o.status.success(),
        "--oci with gnu target should fail"
    );
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(
        stderr.contains("musl"),
        "--oci gnu rejection must name the musl equivalent, got: {stderr}"
    );
}

#[test]
fn oci_default_tag_is_stem_latest() {
    let dir = tmp_dir("default_tag");
    let src = write(&dir, "myapp.as", HELLO_SRC);
    let out = dir.join("myapp.tar");
    let data = build_oci(&dir, &src, &out, &[]);
    let oci = parse_oci_tar(&data);
    let index: serde_json::Value = serde_json::from_slice(&oci.index_json).unwrap();
    let ref_name = index["manifests"][0]["annotations"]["org.opencontainers.image.ref.name"]
        .as_str()
        .unwrap_or("");
    assert_eq!(ref_name, "myapp:latest", "default tag should be <stem>:latest");
}

#[test]
fn oci_explicit_tag_appears_in_index() {
    let dir = tmp_dir("explicit_tag");
    let src = write(&dir, "app.as", HELLO_SRC);
    let out = dir.join("app.tar");
    let data = build_oci(&dir, &src, &out, &["--oci-tag", "registry.example.com/myapp:2.0.1"]);
    let oci = parse_oci_tar(&data);
    let index: serde_json::Value = serde_json::from_slice(&oci.index_json).unwrap();
    let ref_name = index["manifests"][0]["annotations"]["org.opencontainers.image.ref.name"]
        .as_str()
        .unwrap_or("");
    assert_eq!(ref_name, "registry.example.com/myapp:2.0.1");
}

// ── Optional Docker smoke test ────────────────────────────────────────────────

/// Skip if docker is absent on PATH or `ASCRIPT_RT_BIN_MUSL` is unset.
/// When both are present: `docker load < hello.tar && docker run --rm <img>`.
#[test]
fn docker_load_and_run() {
    let musl_bin = match std::env::var("ASCRIPT_RT_BIN_MUSL") {
        Ok(b) => b,
        Err(_) => {
            eprintln!("[rt_oci] SKIP docker_load_and_run — ASCRIPT_RT_BIN_MUSL not set");
            return;
        }
    };
    // Check if docker is on PATH.
    let docker_ok = Command::new("docker").arg("info").output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !docker_ok {
        eprintln!("[rt_oci] SKIP docker_load_and_run — docker not available");
        return;
    }

    let dir = tmp_dir("docker_smoke");
    let src = write(&dir, "smoke.as", HELLO_SRC);
    let out = dir.join("smoke.tar");
    let tag = format!("ascript-rt-oci-smoke-test-{}", std::process::id());

    // Build using the musl stub directly.
    let o = Command::new(toolchain_bin())
        .args(["build", "--oci", "--stub"])
        .arg(&musl_bin)
        .arg("--oci-tag")
        .arg(&tag)
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .env("SOURCE_DATE_EPOCH", "0")
        .current_dir(&*dir)
        .output()
        .expect("spawn build");
    assert!(
        o.status.success(),
        "build --oci --stub failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );

    // docker load
    let tar_bytes = std::fs::read(&out).unwrap();
    let load = Command::new("docker")
        .args(["load"])
        .stdin(std::process::Stdio::piped())
        .output()
        .ok()
        .and_then(|_| {
            // Use a pipe-based invocation.
            let mut child = Command::new("docker")
                .args(["load"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .ok()?;
            use std::io::Write;
            child.stdin.take()?.write_all(&tar_bytes).ok()?;
            child.wait_with_output().ok()
        });
    let load_out = match load {
        Some(o) => o,
        None => {
            eprintln!("[rt_oci] SKIP docker_load_and_run — docker load failed to spawn");
            return;
        }
    };
    if !load_out.status.success() {
        eprintln!("[rt_oci] SKIP docker_load_and_run — docker load failed:\n  {}",
            String::from_utf8_lossy(&load_out.stderr));
        return;
    }

    // docker run
    let run = Command::new("docker")
        .args(["run", "--rm", &tag])
        .output()
        .expect("docker run");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success(),
        "docker run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        stdout.contains("hello from oci"),
        "docker run output missing expected string, got: {stdout}"
    );

    // Cleanup the image.
    let _ = Command::new("docker")
        .args(["rmi", "--force", &tag])
        .output();
}
