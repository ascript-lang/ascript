//! FUZZ Task 7 — the source-parser fuzz target.
//!
//! Raw libFuzzer bytes → both front-ends, asserting the NO-PANIC contract (spec §2.3 / §3.2):
//!
//!   - CST `ascript::syntax::parser::parse(src)` (`src/syntax/parser.rs`) — degrades to error
//!     nodes via `p.error` + `p.bump` and NEVER panics by contract; this target proves no
//!     input breaks that contract.
//!   - Legacy `ascript::parser::parse(tokens)` — takes TOKENS, so we `ascript::lexer::lex`
//!     first; neither the lexer nor the legacy parser may panic on any input.
//!
//! The two sub-modes (a leading discriminant byte routes between them so one corpus reaches
//! both):
//!   (a) ARBITRARY BYTES (as lossy UTF-8) → both parsers must SURVIVE (no panic). Most random
//!       bytes are rejected with error nodes / an `Err` — that is fine; only a HOST panic /
//!       OOB / hang is a finding.
//!   (b) GENERATOR-PRODUCED VALID SOURCE (`ascript::fuzzgen`) → both front-ends must AGREE on
//!       accept/reject (the front-end-agreement property, generalizing
//!       `tests/frontend_conformance.rs`). The generator is grammar-aware, so a rejection by
//!       EITHER front-end on its output is a parser bug. Caret-column offsets (SP1) don't
//!       enter — we compare accept/reject only.
//!
//! The in-suite proof (no cargo-fuzz needed) lives in the NORMAL suite:
//! `tests/property.rs::both_frontends_accept_generated_source` + `frontend_agreement_fixed_battery`.
//! This target is the coverage-guided extension that runs in CI from `fuzz/corpus/parser/`.
//!
//! Determinism: every finding is a saved input file, replayable verbatim.

#![no_main]

use libfuzzer_sys::fuzz_target;

/// Does the LEGACY front-end (lex → parse) accept `src` without error?
fn legacy_accepts(src: &str) -> bool {
    match ascript::lexer::lex(src) {
        Ok(tokens) => ascript::parser::parse(&tokens).is_ok(),
        Err(_) => false,
    }
}

/// Does the CST front-end accept `src` cleanly (no parse or lex error nodes)?
fn cst_accepts(src: &str) -> bool {
    let parse = ascript::syntax::parser::parse(src);
    parse.errors.is_empty() && parse.lex_errors.is_empty()
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let (mode, rest) = (data[0], &data[1..]);

    if mode & 1 == 0 {
        // (a) ARBITRARY BYTES → both parsers must SURVIVE (no panic). CALLING them under the
        // fuzzer IS the assertion. Lossy UTF-8 so any byte buffer becomes a `&str` input.
        let src = String::from_utf8_lossy(rest);
        // Legacy: lex then (on success) parse — neither may panic.
        if let Ok(tokens) = ascript::lexer::lex(&src) {
            let _ = ascript::parser::parse(&tokens);
        }
        // CST: must never panic (it degrades to error nodes).
        let _ = ascript::syntax::parser::parse(&src);
    } else {
        // (b) GENERATOR-PRODUCED VALID SOURCE → the two front-ends must AGREE on acceptance.
        let prog = ascript::fuzzgen::gen_program_from_bytes(rest);
        let src = &prog.source;
        let legacy = legacy_accepts(src);
        let cst = cst_accepts(src);
        // The generator emits the shared grammar both parsers implement, so BOTH must accept.
        // A disagreement (or a rejection by either) is a parser/generator bug — a ready
        // reproducer is saved.
        assert!(
            legacy && cst,
            "front-end disagreement on generated source (legacy={legacy}, cst={cst})\n--- program ---\n{src}"
        );
    }
});
