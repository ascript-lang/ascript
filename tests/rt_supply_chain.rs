//! RT §5.1–§5.3 / §10.1 — the supply-chain battery: signed manifest verification,
//! fail-closed fetch, and the content-addressed stub cache, PROVEN (not asserted).
//!
//! Hermetic: every test sets `$ASCRIPT_CACHE` to a unique tempdir under the
//! `pkg::cache::TEST_ENV_LOCK` discipline (the same process-wide lock the `pkg`
//! tests use, so the env var is never mutated by two tests at once). Manifest +
//! stub fixtures are written to disk and served via a `file://…` base URL through
//! the `ASCRIPT_RT_BASE_URL`-equivalent `FetchOpts.base_url` seam — NO real network.
//! The TEST ed25519 keypair is injected via a `#[doc(hidden)]` `pubkey` seam on
//! `FetchOpts`, NEVER an env var (fail-closed allows no insecure knob).
//!
//! Every integrity check has a NEGATIVE test demonstrating REFUSAL and that nothing
//! is published to `rt/` on refusal.

#![cfg(feature = "rt-fetch")]

use ascript::rtstub::cache;
use ascript::rtstub::fetch::{self, FetchError, FetchOpts};
use ascript::rtstub::manifest::{self, RtManifest};
use ascript::rtstub::tiers::Tier;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

// ----------------------------------------------------------------------------
// Test scaffolding
// ----------------------------------------------------------------------------

const TEST_TARGET: &str = "x86_64-unknown-linux-musl";

fn sha256_hex(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// A deterministic test signing key (fixed 32-byte seed — no rand needed).
fn test_signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

/// A DIFFERENT key, for the bad-signature case.
fn other_signing_key() -> SigningKey {
    SigningKey::from_bytes(&[42u8; 32])
}

/// Build a canonical manifest JSON for one stub entry. `version` lets the
/// version-mismatch case lie about the toolchain version.
fn manifest_json(version: &str, stub_bytes: &[u8], filename: &str) -> String {
    let sha = sha256_hex(stub_bytes);
    let size = stub_bytes.len();
    format!(
        r#"{{"schema":1,"ascript":"{version}","created":"1970-01-01T00:00:00Z",
"stubs":[{{"target":"{TEST_TARGET}","tier":"rt-net",
"features":["shared","bundle-zstd","data"],
"sha256":"{sha}","size":{size},"filename":"{filename}"}}]}}"#
    )
}

/// A served release directory laid out the way the fetcher expects:
/// `{base}/v{version}/rt-manifest.json` + `.sig` + the stub blob.
struct ServedRelease {
    // Kept so the served release's root path stays addressable; not read directly.
    #[allow(dead_code)]
    base_dir: PathBuf,
    base_url: String,
    stub_bytes: Vec<u8>,
    stub_sha: String,
}

/// Lay out a signed release on disk. `sign_with` is the key the manifest is signed
/// with; `manifest_version` is what the manifest claims (use the real toolchain
/// version for happy paths). `tamper_stub` lets a case corrupt the served blob
/// AFTER the manifest pins the original bytes.
fn serve_release(
    tag: &str,
    manifest_version: &str,
    sign_with: &SigningKey,
    sign: bool,
    tamper_stub: impl FnOnce(&mut Vec<u8>),
) -> ServedRelease {
    let base_dir = unique_dir(tag);
    let toolchain_version = env!("CARGO_PKG_VERSION");
    let vdir = base_dir.join(format!("v{toolchain_version}"));
    std::fs::create_dir_all(&vdir).unwrap();

    let filename = format!("ascript-rt-{toolchain_version}-{TEST_TARGET}-rt-net");
    // The honest stub bytes the manifest pins.
    let mut stub_bytes = b"FAKE-STUB-PAYLOAD-bytes-for-the-test-0123456789".to_vec();
    let stub_sha = sha256_hex(&stub_bytes);

    let manifest = manifest_json(manifest_version, &stub_bytes, &filename);
    let sig = sign_with.sign(manifest.as_bytes());

    std::fs::write(vdir.join("rt-manifest.json"), manifest.as_bytes()).unwrap();
    if sign {
        std::fs::write(vdir.join("rt-manifest.json.sig"), sig.to_bytes()).unwrap();
    } else {
        // unsigned: empty signature file
        std::fs::write(vdir.join("rt-manifest.json.sig"), b"").unwrap();
    }

    // Possibly tamper the served blob (checksum/truncation cases) AFTER pinning.
    tamper_stub(&mut stub_bytes);
    std::fs::write(vdir.join(&filename), &stub_bytes).unwrap();

    let base_url = format!("file://{}", base_dir.display());
    ServedRelease { base_dir, base_url, stub_bytes, stub_sha }
}

fn unique_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "rt-supply-{}-{}-{:?}",
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

/// The `rt/` cache subtree must be EMPTY (no entries published).
fn rt_dir_is_empty(cache_root: &Path) -> bool {
    let rt = cache_root.join("rt");
    if !rt.exists() {
        return true;
    }
    std::fs::read_dir(&rt).map(|mut it| it.next().is_none()).unwrap_or(true)
}

/// Run `body` with `$ASCRIPT_CACHE` pinned to a fresh tempdir, under the env-lock.
fn with_cache<R>(tag: &str, body: impl FnOnce(&Path) -> R) -> R {
    let guard = cache::test_env_lock();
    let cache_dir = unique_dir(&format!("cache-{tag}"));
    let prev = std::env::var_os("ASCRIPT_CACHE");
    std::env::set_var("ASCRIPT_CACHE", &cache_dir);

    let result = body(&cache_dir);

    match prev {
        Some(v) => std::env::set_var("ASCRIPT_CACHE", v),
        None => std::env::remove_var("ASCRIPT_CACHE"),
    }
    let _ = std::fs::remove_dir_all(&cache_dir);
    drop(guard);
    result
}

/// Build `FetchOpts` that point at a served release and verify with the TEST key.
fn opts_for(served: &ServedRelease) -> FetchOpts {
    FetchOpts {
        base_url: Some(served.base_url.clone()),
        no_fetch: false,
        pubkey: Some(test_signing_key().verifying_key().to_bytes()),
    }
}

fn tokio_block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

// ----------------------------------------------------------------------------
// Happy path
// ----------------------------------------------------------------------------

#[test]
fn happy_path_fetch_verifies_publishes_and_rehashes_on_load() {
    let version = env!("CARGO_PKG_VERSION");
    let served = serve_release("happy", version, &test_signing_key(), true, |_| {});
    with_cache("happy", |cache_root| {
        let opts = opts_for(&served);
        let path = tokio_block(fetch::fetch_stub(TEST_TARGET, Tier::RtNet, &opts))
            .expect("happy path must fetch + verify + publish");

        // Published under rt/sha256-<hex>/...
        assert!(path.exists(), "published stub must exist on disk");
        assert!(
            path.to_string_lossy().contains(&format!("sha256-{}", served.stub_sha)),
            "publish path is content-addressed: {}",
            path.display()
        );
        // The published bytes equal the honest stub bytes.
        let got = std::fs::read(&path).unwrap();
        assert_eq!(got, served.stub_bytes);

        // load() re-hashes and returns the same path.
        let loaded = cache::load(&served.stub_sha).expect("cache hit re-hashes and returns");
        assert_eq!(loaded, path);
        let _ = cache_root;
    });
}

// ----------------------------------------------------------------------------
// Integrity refusals — every one proves REFUSAL + nothing published
// ----------------------------------------------------------------------------

#[test]
fn wrong_checksum_refused_nothing_published() {
    let version = env!("CARGO_PKG_VERSION");
    // Serve a blob whose bytes differ from the manifest pin.
    let served = serve_release("badsum", version, &test_signing_key(), true, |b| {
        b.push(0xFF); // append a byte → sha + size both differ from the pin
    });
    with_cache("badsum", |cache_root| {
        let err = tokio_block(fetch::fetch_stub(TEST_TARGET, Tier::RtNet, &opts_for(&served)))
            .expect_err("checksum mismatch must be refused");
        assert!(matches!(err, FetchError::Integrity(_)), "must be an INTEGRITY failure: {err:?}");
        let msg = err.to_string();
        assert!(msg.contains("sha256") || msg.contains("checksum") || msg.contains("size"), "{msg}");
        assert!(rt_dir_is_empty(cache_root), "NOTHING may be published on a checksum mismatch");
    });
}

#[test]
fn bad_signature_refused() {
    let version = env!("CARGO_PKG_VERSION");
    // Signed by a DIFFERENT key than the verifier's pubkey.
    let served = serve_release("badsig", version, &other_signing_key(), true, |_| {});
    with_cache("badsig", |cache_root| {
        let err = tokio_block(fetch::fetch_stub(TEST_TARGET, Tier::RtNet, &opts_for(&served)))
            .expect_err("a manifest signed by another key must be refused");
        assert!(matches!(err, FetchError::Integrity(_)), "{err:?}");
        assert!(err.to_string().contains("signature"), "{err}");
        assert!(rt_dir_is_empty(cache_root), "nothing published on bad signature");
    });
}

#[test]
fn unsigned_manifest_refused() {
    let version = env!("CARGO_PKG_VERSION");
    let served = serve_release("unsigned", version, &test_signing_key(), false, |_| {});
    with_cache("unsigned", |cache_root| {
        let err = tokio_block(fetch::fetch_stub(TEST_TARGET, Tier::RtNet, &opts_for(&served)))
            .expect_err("an unsigned/empty-signature manifest must be refused");
        assert!(matches!(err, FetchError::Integrity(_)), "{err:?}");
        assert!(err.to_string().contains("signature"), "{err}");
        assert!(rt_dir_is_empty(cache_root), "nothing published on unsigned manifest");
    });
}

#[test]
fn version_mismatch_refused() {
    // The manifest claims a DIFFERENT version (downgrade/replay defense).
    let served = serve_release("vermismatch", "0.0.1-downgrade", &test_signing_key(), true, |_| {});
    with_cache("vermismatch", |cache_root| {
        let err = tokio_block(fetch::fetch_stub(TEST_TARGET, Tier::RtNet, &opts_for(&served)))
            .expect_err("a version-mismatched manifest must be refused");
        assert!(matches!(err, FetchError::Integrity(_)), "{err:?}");
        assert!(err.to_string().contains("version"), "{err}");
        assert!(rt_dir_is_empty(cache_root), "nothing published on version mismatch");
    });
}

#[test]
fn truncated_stub_refused() {
    let version = env!("CARGO_PKG_VERSION");
    // Truncate the served blob to a few bytes (size + sha both differ from pin).
    let served = serve_release("trunc", version, &test_signing_key(), true, |b| {
        b.truncate(3);
    });
    with_cache("trunc", |cache_root| {
        let err = tokio_block(fetch::fetch_stub(TEST_TARGET, Tier::RtNet, &opts_for(&served)))
            .expect_err("a truncated stub must be refused");
        assert!(matches!(err, FetchError::Integrity(_)), "{err:?}");
        assert!(rt_dir_is_empty(cache_root), "nothing published on truncation");
    });
}

// ----------------------------------------------------------------------------
// Verify-on-load: a corrupt cache entry is evicted and refetched
// ----------------------------------------------------------------------------

#[test]
fn corrupt_cache_entry_evicted_and_refetched() {
    let version = env!("CARGO_PKG_VERSION");
    let served = serve_release("corrupt", version, &test_signing_key(), true, |_| {});
    with_cache("corrupt", |_cache_root| {
        let opts = opts_for(&served);
        // First fetch publishes a good entry.
        let path = tokio_block(fetch::fetch_stub(TEST_TARGET, Tier::RtNet, &opts)).unwrap();

        // Bit-flip the cached file.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();

        // load() must re-hash, detect the mismatch, EVICT, and return None.
        assert!(
            cache::load(&served.stub_sha).is_none(),
            "a bit-flipped cache entry must NOT be trusted by path — re-hash evicts it"
        );
        // The slot is gone.
        assert!(!path.exists(), "the corrupt entry must be evicted");

        // Refetch repairs it (verify-on-load returns a good entry again).
        let path2 = tokio_block(fetch::fetch_stub(TEST_TARGET, Tier::RtNet, &opts)).unwrap();
        assert_eq!(std::fs::read(&path2).unwrap(), served.stub_bytes);
        assert!(cache::load(&served.stub_sha).is_some(), "refetch republishes a trustworthy entry");
    });
}

// ----------------------------------------------------------------------------
// --no-fetch skips the network entirely
// ----------------------------------------------------------------------------

#[test]
fn no_fetch_flag_skips_network_entirely() {
    let version = env!("CARGO_PKG_VERSION");
    let served = serve_release("nofetch", version, &test_signing_key(), true, |_| {});
    with_cache("nofetch", |cache_root| {
        let probe_before = fetch::fetch_attempts();
        let mut opts = opts_for(&served);
        opts.no_fetch = true;
        let err = tokio_block(fetch::fetch_stub(TEST_TARGET, Tier::RtNet, &opts))
            .expect_err("--no-fetch must not fetch");
        // It is an AVAILABILITY failure (the ladder falls through), not integrity.
        assert!(matches!(err, FetchError::Unavailable(_)), "{err:?}");
        // The fetcher was NOT called (probe seam): no network/file read attempted.
        assert_eq!(
            fetch::fetch_attempts(),
            probe_before,
            "no fetch attempt may occur under --no-fetch"
        );
        assert!(rt_dir_is_empty(cache_root), "nothing published under --no-fetch");
    });
}

// ----------------------------------------------------------------------------
// Atomic publish — a pre-created read-only slot is a clean error, no partial state
// ----------------------------------------------------------------------------

#[test]
fn cache_publish_is_atomic() {
    with_cache("atomic", |cache_root| {
        let bytes = b"some-stub-bytes-to-publish".to_vec();
        let sha = sha256_hex(&bytes);

        // Pre-create the destination slot directory as READ-ONLY so the rename
        // into it fails — publish must surface a clean Err and leave NO partial state.
        let slot = cache_root.join("rt").join(format!("sha256-{sha}"));
        std::fs::create_dir_all(&slot).unwrap();
        // Place an existing file and make the slot dir read-only (best-effort on unix).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Put a conflicting file where the binary should land and lock the dir.
            std::fs::write(slot.join(cache::stub_filename()), b"squatter").unwrap();
            let mut perms = std::fs::metadata(&slot).unwrap().permissions();
            perms.set_mode(0o500); // r-x, no write → rename target replace fails
            std::fs::set_permissions(&slot, perms).unwrap();
        }

        let result = cache::publish(&bytes, &sha);

        #[cfg(unix)]
        {
            // Restore perms so the dir can be cleaned up.
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&slot).unwrap().permissions();
            perms.set_mode(0o700);
            let _ = std::fs::set_permissions(&slot, perms);

            assert!(result.is_err(), "publish into a read-only slot must be a clean Err");
            // No staging cruft left behind in tmp/.
            let tmp = cache_root.join("tmp");
            if tmp.exists() {
                let leftover: Vec<_> = std::fs::read_dir(&tmp)
                    .unwrap()
                    .filter_map(|e| e.ok())
                    .collect();
                assert!(leftover.is_empty(), "no partial staging files may remain: {leftover:?}");
            }
        }
        #[cfg(not(unix))]
        {
            // On non-unix, just assert publish either succeeds cleanly or errors cleanly.
            let _ = result;
        }
    });
}

// ----------------------------------------------------------------------------
// Manifest parser unit coverage (also exercised under --no-default-features in
// the in-crate unit tests; here we sanity-check the public parse API).
// ----------------------------------------------------------------------------

#[test]
fn manifest_parse_round_trip() {
    let version = env!("CARGO_PKG_VERSION");
    let stub = b"x".to_vec();
    let json = manifest_json(version, &stub, "ascript-rt-test");
    let m: RtManifest = manifest::parse_manifest(json.as_bytes()).expect("parse");
    assert_eq!(m.schema, 1);
    assert_eq!(m.ascript, version);
    assert_eq!(m.stubs.len(), 1);
    let e = &m.stubs[0];
    assert_eq!(e.target, TEST_TARGET);
    assert_eq!(e.tier, "rt-net");
    assert_eq!(e.size, stub.len() as u64);
    assert!(e.features.contains(&"shared".to_string()));
}

// ----------------------------------------------------------------------------
// RT §5.1 / Task 11 — the in-tree manifest GENERATOR, hermetically tested.
// Gated on `rt-release` (the ed25519 SIGNING half — a runtime stub never links it).
// The whole point: what `generate_manifest` + `sign_manifest` produce is EXACTLY what
// Task 6's `verify_manifest` accepts, proven by a round-trip against a TEST key.
// ----------------------------------------------------------------------------

#[cfg(feature = "rt-release")]
mod generator {
    use super::*;
    use ascript::rtstub::manifest::{
        entry_filename, generate_manifest, generate_keypair, load_signing_key_hex,
        parse_entries, sign_manifest, verify_manifest, StubEntry,
    };

    fn sample_entries(version: &str) -> Vec<StubEntry> {
        let triples = [
            ("x86_64-apple-darwin", "rt-core"),
            ("x86_64-unknown-linux-musl", "rt-net"),
            ("aarch64-pc-windows-msvc", "rt-full"),
        ];
        triples
            .iter()
            .enumerate()
            .map(|(i, (target, tier))| {
                let bytes = format!("stub-{target}-{tier}").into_bytes();
                StubEntry {
                    target: target.to_string(),
                    tier: tier.to_string(),
                    features: vec!["shared".into(), "bundle-zstd".into()],
                    sha256: sha256_hex(&bytes),
                    size: (1000 + i) as u64,
                    filename: entry_filename(version, target, tier),
                }
            })
            .collect()
    }

    #[test]
    fn generated_manifest_round_trips_through_the_task6_verifier() {
        let version = env!("CARGO_PKG_VERSION");
        let entries = sample_entries(version);
        let bytes = generate_manifest(version, "1970-01-01T00:00:00Z", &entries);

        // Sign with the TEST key and verify against its PUBLIC key — the exact path the
        // production builder takes against PRODUCTION_PUBKEY.
        let sk = test_signing_key();
        let sig = sign_manifest(&bytes, &sk);
        let pubkey = sk.verifying_key().to_bytes();

        let m = verify_manifest(&bytes, &sig, &pubkey)
            .expect("the generated+signed manifest must verify against the test pubkey");
        assert_eq!(m.ascript, version);
        assert_eq!(m.stubs.len(), entries.len());
        // The parsed entries equal the inputs (order + every field preserved).
        assert_eq!(m.stubs, entries);
    }

    #[test]
    fn entry_filenames_follow_the_spec_convention() {
        let v = "0.6.0";
        assert_eq!(
            entry_filename(v, "x86_64-unknown-linux-musl", "rt-net"),
            "ascript-rt-0.6.0-x86_64-unknown-linux-musl-rt-net"
        );
        // Windows targets keep the .exe extension.
        assert_eq!(
            entry_filename(v, "aarch64-pc-windows-msvc", "rt-full"),
            "ascript-rt-0.6.0-aarch64-pc-windows-msvc-rt-full.exe"
        );
    }

    #[test]
    fn double_generate_is_byte_identical() {
        let version = env!("CARGO_PKG_VERSION");
        let entries = sample_entries(version);
        let a = generate_manifest(version, "1970-01-01T00:00:00Z", &entries);
        let b = generate_manifest(version, "1970-01-01T00:00:00Z", &entries);
        assert_eq!(a, b, "the generator must be deterministic (no now(), fixed key order)");
    }

    #[test]
    fn tampered_manifest_fails_verify() {
        let version = env!("CARGO_PKG_VERSION");
        let entries = sample_entries(version);
        let mut bytes = generate_manifest(version, "1970-01-01T00:00:00Z", &entries);
        let sk = test_signing_key();
        let sig = sign_manifest(&bytes, &sk);
        let pubkey = sk.verifying_key().to_bytes();

        // Flip a byte AFTER signing — the signature no longer covers these bytes.
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0x20;
        let err = verify_manifest(&bytes, &sig, &pubkey)
            .expect_err("a tampered manifest must fail signature verification");
        assert!(err.contains("signature"), "{err}");
    }

    #[test]
    fn entries_file_round_trips_through_parse_entries() {
        // The release script writes an entries array; parse_entries reads it back. A
        // generated manifest's stubs must equal the entries parsed from the same JSON.
        let version = env!("CARGO_PKG_VERSION");
        let entries = sample_entries(version);
        // Build an entries-array JSON the same way the release script would.
        let mut arr = String::from("[");
        for (i, e) in entries.iter().enumerate() {
            if i > 0 {
                arr.push(',');
            }
            let feats: Vec<String> =
                e.features.iter().map(|f| format!("\"{f}\"")).collect();
            arr.push_str(&format!(
                r#"{{"target":"{}","tier":"{}","features":[{}],"sha256":"{}","size":{},"filename":"{}"}}"#,
                e.target,
                e.tier,
                feats.join(","),
                e.sha256,
                e.size,
                e.filename
            ));
        }
        arr.push(']');
        let parsed = parse_entries(arr.as_bytes()).expect("parse entries");
        assert_eq!(parsed, entries);
    }

    #[test]
    fn generated_keypair_signs_and_verifies() {
        // generate_keypair → load the private seed → sign → verify against the public.
        let (seed_hex, pub_hex) = generate_keypair();
        let sk = load_signing_key_hex(&seed_hex).expect("load minted seed");
        let version = env!("CARGO_PKG_VERSION");
        let entries = sample_entries(version);
        let bytes = generate_manifest(version, "1970-01-01T00:00:00Z", &entries);
        let sig = sign_manifest(&bytes, &sk);

        // Reconstruct the 32-byte pubkey from the printed hex.
        let mut pk = [0u8; 32];
        for (i, b) in pk.iter_mut().enumerate() {
            *b = u8::from_str_radix(&pub_hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        assert!(verify_manifest(&bytes, &sig, &pk).is_ok());
    }

    #[test]
    fn load_signing_key_rejects_malformed_seed() {
        assert!(load_signing_key_hex("tooshort").is_err());
        assert!(load_signing_key_hex(&"z".repeat(64)).is_err());
        assert!(load_signing_key_hex(&"a".repeat(64)).is_ok());
    }
}
