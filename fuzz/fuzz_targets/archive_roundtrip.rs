//! FUZZ Task 4.2 — the `ASCRIPTA` module-archive container decoder byte-fuzz target
//! (SECURITY-CRITICAL; self-contained-bundles spec §3).
//!
//! A shipped bundle (`ascript build` / `--native`) parses attacker-influenceable
//! `ASCRIPTA` archive bytes on load — a tampered or corrupted archive — so
//! [`ModuleArchive::decode`] (`src/vm/archive.rs`) is a security surface. This
//! libFuzzer target feeds ARBITRARY bytes to the decoder and asserts the load-bearing
//! invariant:
//!
//!   `ModuleArchive::decode(any_bytes)` ALWAYS returns `Ok(ModuleArchive)` /
//!   `Err(ArchiveError)` — and NEVER panics, reads out of bounds, allocates
//!   unboundedly, or hangs. (Per-module chunk bytes are stored OPAQUE — the container
//!   codec does not verify them, so this target fuzzes the CONTAINER framing only.)
//!
//! Two invariants exercised on every input:
//!   (a) DECODE no-panic — calling `decode` under the fuzzer IS the assertion
//!       (libFuzzer treats any panic/abort/OOM/OOB/hang as a crash). Both `Ok` and
//!       `Err` are valid outcomes for arbitrary bytes.
//!   (b) DECODE-STABILITY — if `decode(data)` is `Ok(a)`, then `decode(a.encode())`
//!       must be `Ok(b)` with `a == b` (the re-encode round-trips idempotently). A
//!       divergence is a codec bug, so this is a hard `assert`.
//!
//! Plus a STRUCTURED `encode∘decode` round-trip: a VALID archive is built
//! deterministically from `data` (a few `(key, opaque-bytes)` modules + a `CapSet` +
//! an in-range `entry`), encoded, and decoded — asserting field equality. This
//! exercises the ENCODE side that the arbitrary-bytes path cannot reach.
//!
//! Determinism: every libFuzzer finding is a saved input file, replayable verbatim.
//!
//! Seed corpus: `fuzz/corpus/archive_roundtrip/` — a real `ASCRIPTA` archive
//! (`ex_multimodule`, built by `ascript build` over `examples/bundle_multimodule.as`,
//! which embeds its `./bundle_util` import) plus the curated `bad_*` known-bad buffers
//! (truncated magic, bogus module count, oversized length prefixes, non-UTF-8 key).
//! The structure-aware `ex_*` seed lets the mutator flip ONE byte deep inside a valid
//! archive, reaching the `Reader` length/UTF-8 arms where clamp-class bugs would live.
//!
//! The in-suite proof that this target's "panic ⇒ crash" detection actually fires
//! lives in the NORMAL test suite:
//! `tests/property.rs::archive_planted_bug_known_bad_bytes_are_clean_err` (the
//! permanent planted-bug guard) + `archive_decode_arbitrary_never_panics` +
//! `archive_structured_round_trip` — that is the in-session evidence; this libFuzzer
//! campaign is the coverage-guided extension that runs in CI.

#![no_main]

use libfuzzer_sys::fuzz_target;

use ascript::stdlib::caps::CapSet;
use ascript::vm::archive::ModuleArchive;

/// Build a VALID `ModuleArchive` deterministically from arbitrary `data` (mirrors the
/// `archive_from_bytes` generator in `tests/property.rs`): 1..=4 modules with unique
/// valid-UTF-8 keys + opaque chunk bytes, a `CapSet`, a 32-byte digest, and an
/// in-range `entry` (`entry < modules.len()`), so the result always encodes+decodes.
fn archive_from_bytes(data: &[u8]) -> ModuleArchive {
    let n = 1 + (data.first().copied().unwrap_or(0) as usize % 4);
    let mut modules: Vec<(String, Vec<u8>)> = Vec::with_capacity(n);
    let mut cursor = 1usize;
    for i in 0..n {
        let key = format!("mod{i}");
        let want = data.get(cursor).copied().unwrap_or(0) as usize % 8;
        cursor = cursor.saturating_add(1);
        let chunk: Vec<u8> = data.iter().cycle().skip(cursor).take(want).copied().collect();
        cursor = cursor.saturating_add(want);
        modules.push((key, chunk));
    }
    let entry = (data.get(1).copied().unwrap_or(0) as u32) % (n as u32);

    let mut shake = [0u8; 32];
    for (i, b) in shake.iter_mut().enumerate() {
        *b = data
            .get(i + 2)
            .copied()
            .unwrap_or((i as u8).wrapping_mul(7).wrapping_add(1));
    }

    let caps = match data.get(2).copied().unwrap_or(0) % 4 {
        0 => CapSet::default(),
        1 => CapSet::from_deny_list(["net"]).unwrap(),
        2 => CapSet::from_deny_list(["fs", "process"]).unwrap(),
        _ => CapSet::from_deny_list(["env"]).unwrap(),
    };

    ModuleArchive::new(entry, caps, shake, modules)
}

fuzz_target!(|data: &[u8]| {
    // (a) DECODE-only path — the SECURITY-CRITICAL core, exercised on EVERY input. The
    // contract: NEVER panic — only `Ok(ModuleArchive)`/`Err(ArchiveError)`. libFuzzer
    // treats any panic/abort/OOM/OOB/hang inside as a crash, so simply CALLING it under
    // the fuzzer is the assertion. Both `Ok` and `Err` are valid for arbitrary bytes.
    if let Ok(a) = ModuleArchive::decode(data) {
        // (b) DECODE-STABILITY — a successful decode must re-encode idempotently. A
        // divergence is a container-codec bug, so this is a hard assertion.
        let re = a.encode();
        let b = ModuleArchive::decode(&re)
            .expect("re-encode of a decoded archive must itself decode");
        assert_eq!(a, b, "decode∘encode is not idempotent (codec bug)");
    }

    // (c) STRUCTURED `encode∘decode` round-trip — fuzz the ENCODE side from structured
    // input: build a VALID archive from `data`, encode it, decode it, assert equality.
    let arch = archive_from_bytes(data);
    let bytes = arch.encode();
    let decoded = ModuleArchive::decode(&bytes)
        .expect("a well-formed archive must decode");
    assert_eq!(decoded, arch, "encode∘decode lost or changed a field");
});
