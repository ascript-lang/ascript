//! LSP performance bounds and budgets.
//!
//! NOTE ON INCREMENTAL REBUILDS: the AScript front-end is a FULL-REPARSE design.
//! `crate::syntax::parser::parse` re-lexes the whole source (`parser.rs` ~L170 →
//! `Parser::new(src)` re-runs `lex_with_errors` on every call) and
//! `tree_builder::build_tree` allocates a FRESH `cstree` `GreenNodeBuilder` each
//! call (`tree_builder.rs` ~L21: `let mut builder: GreenNodeBuilder<SyntaxKind> =
//! GreenNodeBuilder::new();`). cstree 0.14 interns green nodes WITHIN one build but
//! exposes no cross-build green-node-reuse / `reparse` API (no reusable `NodeCache`
//! threaded across calls). So Phase 7 does NOT do green-node reuse — that would be
//! an out-of-scope, differential-risky front-end change. The responsiveness strategy
//! is instead: DEBOUNCE/COALESCE rapid edits into one rebuild, BOUND each rebuild
//! with a measured budget (see `REBUILD_BUDGET_MS`), and DEGRADE expensive providers
//! above a size threshold (see `model::SizeClass`).

/// At or above this source size, the document is "large": semantic-tokens FULL is
/// downgraded to range-only and inlay hints are skipped (with a logged note).
pub const LARGE_FILE_BYTES: usize = 256 * 1024; // 256 KiB

/// At or above this size, the document is "huge": only diagnostics + navigation run;
/// all token/inlay/folding/color providers return empty rather than risk a stall.
pub const HUGE_FILE_BYTES: usize = 2 * 1024 * 1024; // 2 MiB

/// The per-edit model-rebuild budget asserted by the perf test (Task 2). A rebuild
/// of a `LARGE_FILE_BYTES`-class document must complete under this on CI hardware.
/// Generous (release/debug, shared CI) but still a hard ceiling that trips if a
/// provider accidentally makes `build` super-linear.
pub const REBUILD_BUDGET_MS: u128 = 750;
