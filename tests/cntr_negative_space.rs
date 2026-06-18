//! CNTR Phase 0, Task 0.1 — preflight semantic pins.
//!
//! These tests document the **shipped ground truth** that `std/docker` composes on
//! top of.  Every pin reflects the CURRENT repo state; a failure means the
//! foundation moved → STOP and report before proceeding with CNTR implementation.
//!
//! ## Invariants being pinned
//!
//!  1. `ASO_FORMAT_VERSION` — CNTR is a pure-stdlib addition (like RESIL/PAR/SRV).
//!     No new opcode, no new bytecode layout.  Pre-CNTR baseline is **29**.
//!     *The plan prose said "27" — that was stale (DEFER bumped 27→28, ELIDE bumped
//!     28→29; RT/WARM/RESIL kept 29).  This file pins the real current value.*
//!
//!  2. Op variant count — CNTR must add zero opcodes.  Pinned at **121** (the value
//!     at branch creation: `Op::CallElided` is discriminant 120).
//!
//!  3. `required_cap` verdicts for the two resource-class keys CNTR's `std/docker`
//!     will JOIN (`net_tcp` → `Net`, `process` → `Process`).  These get a mechanical
//!     migration in Phase 1 that adds `"docker"` → `Net`; pinning first proves the
//!     migration preserves the existing entries.
//!
//!  4. No CNTR surface-form yet — `docker` is not yet a keyword, token, AST node,
//!     or grammar rule.  Asserting absence now means the Phase 1 commit that ADDS it
//!     is the first and only place the tripwire changes.
//!
//! ## Gate-16 / baseline note
//!
//! The plan's Task 0.1 prose says to record a Gate-12 A-side baseline "now".
//! That is deferred to Task 7.6 where BOTH main and the branch are built in the
//! SAME session (Gate-16 requires same-session A/B; a Phase-0 baseline taken many
//! commits before the Phase-7 comparison would be stale and violate Gate-16).

use std::path::Path;

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

// ─────────────────────────────────────────────────────────────────────────────
// Pin 1 — `.aso` format version
// ─────────────────────────────────────────────────────────────────────────────

/// CNTR must NOT bump `ASO_FORMAT_VERSION`.
///
/// `std/docker` is a pure-stdlib module: new exported functions dispatch through
/// the existing opcode set (like `std/resilience`, `task.pmap`, `std/shared`).
/// No new `FnProto` field, no new serialised layout, no new bytecode constant.
///
/// History:
///   - DEFER bumped 27 → 28 (DeferPush / DeferPushMethod opcodes).
///   - ELIDE bumped 28 → 29 (Op::CallElided opcode).
///   - RT / WARM / RESIL kept 29.
///   - CNTR bumps NOTHING (pre-CNTR baseline confirmed at 29 on branch creation).
///
/// Note: the plan prose referenced "27" — that was stale prose; the real pre-CNTR
/// value is 29 (verified via `grep ASO_FORMAT_VERSION src/vm/aso.rs` on this branch).
#[test]
fn aso_format_version_unchanged_by_cntr() {
    // Literal pin: trips on ANY bump, forcing a conscious update.
    // When a later spec legitimately bumps the version, update this literal in
    // THAT spec's branch (not here) after confirming the bump is not from CNTR.
    assert_eq!(
        ascript::vm::aso::ASO_FORMAT_VERSION,
        29,
        "ASO_FORMAT_VERSION changed — confirm the bump is NOT from CNTR (std/docker is \
         pure-stdlib, no new opcode/layout); pre-CNTR baseline is 29 (plan said 27 but \
         that was stale: DEFER→28, ELIDE→29). Update this literal in the bumping \
         feature's branch."
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Pin 2 — Op variant count
// ─────────────────────────────────────────────────────────────────────────────

/// CNTR must add zero opcodes.
///
/// `std/docker` container lifecycle calls route through the existing call/import/
/// await opcode set (the same path as every other stdlib module).  No new dispatch
/// byte is needed.
///
/// Pinned at 121 at CNTR branch creation:
///   `Op::CallElided` is discriminant 120 (0-indexed), total count = 121.
///   Added by ELIDE; confirmed by `tests/shape_negative_space.rs` passing.
#[test]
fn op_variant_count_unchanged_by_cntr() {
    const EXPECTED: usize = 121;
    let count = (0u16..=255)
        .filter(|&b| ascript::vm::opcode::Op::from_u8(b as u8).is_some())
        .count();
    assert_eq!(
        count, EXPECTED,
        "Op variant count changed from {EXPECTED} — CNTR (std/docker) must add NO \
         opcodes; container calls are plain stdlib dispatch. Update EXPECTED only \
         when a non-CNTR feature intentionally adds an opcode."
    );
    // Complementary: CallElided is still the last variant.
    assert_eq!(
        ascript::vm::opcode::Op::CallElided as u16 + 1,
        EXPECTED as u16,
        "Op::CallElided is no longer the last variant — a new opcode was inserted \
         after it; update the complementary pin."
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Pin 3 — `required_cap` verdicts for the resource classes CNTR will join
// ─────────────────────────────────────────────────────────────────────────────

/// `required_cap("net_tcp", …)` returns `Some(Cap::Net)`.
///
/// CNTR's `std/docker` communicates with the Docker daemon over a Unix socket or
/// TCP (both in the `net` capability class).  Phase 1 adds `"docker" => Some(Cap::Net)`
/// to `required_cap`, joining the same class as `net_tcp`.  This pin proves the
/// existing `net_tcp` entry is intact so the Phase-1 migration is additive, not a
/// replacement of a broken entry.
///
/// Real module key confirmed by reading `required_cap`'s match arms:
/// `"net" | "net_tcp" | "net_http" | "net_udp" | "net_ws" | "http_server" => Some(Cap::Net)`.
#[cfg(feature = "net")]
#[test]
fn required_cap_net_tcp_is_net() {
    use ascript::stdlib::caps::Cap;
    assert_eq!(
        ascript::stdlib::required_cap("net_tcp", "connect"),
        Some(Cap::Net),
        "required_cap(\"net_tcp\", \"connect\") must be Some(Net); \
         CNTR's std/docker joins this cap class in Phase 1"
    );
}

/// `required_cap("process", "spawn")` returns `Some(Cap::Process)`.
///
/// CNTR's `docker.exec` / `docker.run` may optionally spawn a child process
/// (fallback path when the Docker SDK is unavailable).  Phase 1 adds
/// `"docker" => Some(Cap::Net)` as the PRIMARY gate; the `process` cap entry is
/// the related gate we pin to prove the migration doesn't disturb it.
///
/// Real module key confirmed: `"process" => Some(Cap::Process)` in `required_cap`.
#[cfg(feature = "sys")]
#[test]
fn required_cap_process_spawn_is_process() {
    use ascript::stdlib::caps::Cap;
    assert_eq!(
        ascript::stdlib::required_cap("process", "spawn"),
        Some(Cap::Process),
        "required_cap(\"process\", \"spawn\") must be Some(Process); \
         CNTR's docker.exec uses the process cap for the fallback spawn path"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Pin 4 — No CNTR surface form yet
// ─────────────────────────────────────────────────────────────────────────────

/// The tree-sitter grammar has no `docker` rule or named node.
///
/// `std/docker` is a runtime stdlib call, not surface syntax — `docker.run(…)` is
/// parsed as an ordinary member-call expression (`Expr::Member` + `Expr::Call`).
/// No grammar rule, token, or AST variant is needed.  This pin trips the moment
/// the grammar is modified, forcing a conscious check that the change is intentional.
#[test]
fn tree_sitter_grammar_has_no_docker_syntax_rule() {
    let grammar = std::fs::read_to_string(repo_root().join("tree-sitter-ascript/grammar.js"))
        .expect("read grammar.js");
    // A grammar RULE would appear as `docker_expression:`, `docker_statement:`,
    // `$.docker`, or `field('docker', ...)`.  These must not exist pre-Phase-1.
    for forbidden in [
        "docker_expression",
        "docker_statement",
        "docker_declaration",
        "$.docker",
        "DockerNode",
    ] {
        assert!(
            !grammar.contains(forbidden),
            "tree-sitter grammar.js mentions `{forbidden}` — CNTR must add NO grammar \
             rule; `docker.*` calls are runtime stdlib, parsed as ordinary \
             member-call expressions."
        );
    }
}

/// The Rust front-ends and formatter have no `docker` keyword or AST node.
///
/// A new keyword would appear in `lexer.rs`/`token.rs`; a new AST node in
/// `ast.rs`; a new formatter arm in `fmt.rs`; a new parse rule in `parser.rs` or
/// `src/syntax/`.  None of these should name a CNTR surface form before Phase 1.
#[test]
fn rust_frontends_have_no_docker_keyword_or_node() {
    let surface_files = [
        "src/lexer.rs",
        "src/token.rs",
        "src/ast.rs",
        "src/fmt.rs",
        "src/parser.rs",
    ];
    // We forbid the specific surface tokens that would only exist if CNTR had
    // added syntax.  Plain identifiers like `"docker"` in a string literal or
    // comment are fine — we match the token/AST forms only.
    for rel in surface_files {
        let text = std::fs::read_to_string(repo_root().join(rel))
            .unwrap_or_else(|e| panic!("read {rel}: {e}"));
        for forbidden in [
            "Tok::Docker",
            "Kw::Docker",
            "ExprKind::Docker",
            "Stmt::Docker",
            "DockerExpr",
        ] {
            assert!(
                !text.contains(forbidden),
                "{rel} mentions `{forbidden}` — CNTR must add NO keyword/token/AST \
                 node before Phase 1; `docker.*` calls are runtime stdlib."
            );
        }
    }
}
