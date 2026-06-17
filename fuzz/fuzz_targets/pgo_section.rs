//! WARM Task 5 — the PGO section codec byte-fuzz target (WARM spec §3.2).
//!
//! [`PgoSection::decode`] (`src/vm/pgo.rs`) parses UNTRUSTED bytes from a trailing section
//! of a module archive — an adversarially-controlled input if the archive file is tampered.
//! This libFuzzer target feeds ARBITRARY bytes to the decoder and asserts the load-bearing
//! invariant:
//!
//!   `PgoSection::decode(any_bytes)` ALWAYS returns `Some(PgoSection)` / `None` —
//!   and NEVER panics, reads out of bounds, allocates unboundedly, or hangs.
//!
//! The in-suite proof that the panic-detection fires lives in the normal test suite
//! (`src/vm/pgo.rs` `truncation_at_every_offset_returns_none_never_panics` and the
//! count-bomb tests). This libFuzzer campaign is the coverage-guided extension.
//!
//! Two invariants on every input:
//!   (a) DECODE no-panic — calling `decode` is the assertion (libFuzzer catches
//!       panic/abort/OOM/OOB/hang).
//!   (b) DECODE-STABILITY — if `decode(data)` is `Some(s)`, then encoding `s` and
//!       decoding the payload again must yield `Some(s2)` with `s == s2` (idempotent
//!       codec).
//!
//! Seed corpus: `fuzz/corpus/pgo_section/` — a minimal but representative encoded
//! PGO section payload (see `ex_pgo_section` below), plus `bad_*` hostile inputs
//! (count bombs, truncated, out-of-range indices, invalid arith kind byte).

#![no_main]

use libfuzzer_sys::fuzz_target;

use ascript::vm::pgo::PgoSection;

fuzz_target!(|data: &[u8]| {
    // (a) DECODE-only path — the security-critical core. The contract: NEVER panic.
    // Both `Some` and `None` are valid outcomes for arbitrary bytes.
    if let Some(section) = PgoSection::decode(data) {
        // (b) DECODE-STABILITY — a successful decode must re-encode idempotently.
        // The encode() produces the full section FRAME (magic·version·len·payload);
        // we extract just the payload (bytes 14..) for round-trip decode.
        let frame = section.encode();
        if frame.len() >= 14 {
            let payload = &frame[14..];
            let s2 = PgoSection::decode(payload)
                .expect("re-encoding a decoded PgoSection must itself decode");
            assert_eq!(section, s2, "PgoSection encode∘decode is not idempotent");
        }
    }
});
