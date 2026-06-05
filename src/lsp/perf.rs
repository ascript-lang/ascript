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
/// of a `LARGE_FILE_BYTES`-class document must complete under this in a RELEASE build
/// (the shipped `ascript lsp` binary is release) on CI hardware. Generous but still a
/// hard ceiling that trips if a provider/analysis pass goes super-linear.
///
/// FINDING (recorded per Task 2 Step 2): a `LARGE_FILE_BYTES` rebuild is ~90 ms in a
/// release build but ~2.6 s in an UNOPTIMIZED debug build, the cost dominated by the
/// shared SP10 advisory inference pass (`check::infer`, a recursion-heavy pass that is
/// ~30× slower without optimization). That cost is irreducibly in the shared checker
/// and out of LSP scope to change (it would risk the `vm_differential` / corpus
/// invariants). It is NOT a per-rebuild user-latency problem in practice: Task 4's
/// debounce/coalesce bounds the user-perceived latency to one rebuild per burst, and
/// Task 6's size-class degradation keeps the truly-huge files off the expensive
/// providers. So the budget is asserted at full strength in release and against a
/// generous debug multiple (still catching a genuine O(n²) blowup, which would be
/// tens of seconds, not the steady ~2.6 s constant-factor debug penalty).
pub const REBUILD_BUDGET_MS: u128 = 750;

/// The debug-build multiplier for the rebuild budget. An unoptimized debug build pays
/// a ~30× constant factor (see the `REBUILD_BUDGET_MS` finding); this multiple leaves
/// generous headroom for that constant factor while a true super-linear regression
/// (seconds → tens of seconds) still trips the gate. The full-strength budget is
/// asserted in release.
#[cfg(debug_assertions)]
pub const DEBUG_BUDGET_MULTIPLIER: u128 = 12;

#[cfg(test)]
mod budget_tests {
    use super::*;
    use crate::check::LintConfig;
    use crate::lsp::model::SemanticModel;
    use std::time::Instant;

    /// Build a realistic large AScript source: many small functions, so the parser /
    /// resolver / checker all do real work (not one giant token).
    fn large_source(target_bytes: usize) -> String {
        let unit = "fn f_NUM(a, b) {\n  let s = a + b\n  return s * 2\n}\n";
        let mut out = String::with_capacity(target_bytes + unit.len());
        let mut i = 0usize;
        while out.len() < target_bytes {
            out.push_str(&unit.replace("NUM", &i.to_string()));
            i += 1;
        }
        out
    }

    /// The effective ceiling for the running build profile: the full budget in
    /// release, a generous multiple in an unoptimized debug build (the constant-factor
    /// penalty documented on `REBUILD_BUDGET_MS`). A genuine super-linear regression
    /// trips either ceiling.
    fn effective_budget_ms() -> u128 {
        #[cfg(debug_assertions)]
        {
            REBUILD_BUDGET_MS * DEBUG_BUDGET_MULTIPLIER
        }
        #[cfg(not(debug_assertions))]
        {
            REBUILD_BUDGET_MS
        }
    }

    #[test]
    fn large_file_rebuild_is_under_budget() {
        let src = large_source(LARGE_FILE_BYTES);
        // Warm once (allocator / interner) so the measured run is steady-state.
        let _ = SemanticModel::build(src.clone(), Some(1), &LintConfig::default());

        let start = Instant::now();
        let model = SemanticModel::build(src, Some(2), &LintConfig::default());
        let elapsed = start.elapsed().as_millis();

        let budget = effective_budget_ms();
        // Always print the measurement so the test documents the real cost.
        eprintln!(
            "large-file ({} bytes) model rebuild: {elapsed}ms (budget {budget}ms, \
             release budget {REBUILD_BUDGET_MS}ms)",
            model.text.len()
        );
        assert!(
            elapsed < budget,
            "large-file model rebuild took {elapsed}ms, budget is {budget}ms \
             (a provider/analysis pass likely went super-linear)"
        );
        // Sanity: the build is actually the large class.
        assert!(model.text.len() >= LARGE_FILE_BYTES);
    }
}
