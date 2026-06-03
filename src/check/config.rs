//! Lint configuration: per-code severity overrides and warning promotion.
//!
//! `LintConfig`'s severity-override surface (`deny`/`warn`/`allow`/`effective`)
//! is wired into `analyze_with_config` and the `ascript check` CLI flags
//! `--deny`/`--warn`/`--allow <rule>` (repeatable). `deny_warnings` additionally
//! promotes all surviving warnings for exit-code purposes
//! (`ascript check --deny-warnings`). The `ascript.toml` `[lint]` table
//! (discovered + parsed in the CLI binary's `lint_config_toml` module) also
//! seeds this same `LintConfig`; the CLI flags are overlaid on top, so the net
//! precedence is: inline `// ascript-ignore[code]` suppression (runs first, in
//! `analyze.rs`, and config cannot resurrect a diagnostic the source suppressed),
//! then CLI flag, then `ascript.toml [lint]`, then rule default. `syntax-error`
//! is accepted as a known code but is immune (always `Error`). [`RULE_CODES`] is
//! the single source of truth for validating rule codes.

use crate::check::diagnostic::Severity;
use std::collections::HashMap;

/// The complete set of lint rule codes the checker can emit. This is the single
/// source of truth for validating `--deny`/`--warn`/`--allow` flags (and the
/// `ascript.toml [lint]` table). It is feature-independent.
///
/// `syntax-error` is included so `--allow syntax-error` is accepted as a *known*
/// code rather than rejected as unknown — but it is a NO-OP: `analyze_with_config`
/// makes `syntax-error` immune (always `Error`, never dropped or downgraded), so
/// configuring it has no effect.
pub const RULE_CODES: &[&str] = &[
    "syntax-error",
    "undefined-variable",
    "unused-binding",
    "unused-import",
    "shadowing",
    "unreachable-code",
    "missing-return",
    "unawaited-future",
    "ignored-result",
    "dead-recover",
    "contract-mismatch",
];

/// Per-code severity overrides plus global warning promotion.
///
/// An override maps a code to `Some(sev)` (force this severity) or `None`
/// (suppress the code entirely).
#[derive(Debug, Clone, Default)]
pub struct LintConfig {
    overrides: HashMap<String, Option<Severity>>,
    pub deny_warnings: bool,
}

impl LintConfig {
    /// Is `code` a known lint rule code? Used by the CLI/config layer to reject
    /// unknown `--deny`/`--warn`/`--allow` values with a clear error.
    pub fn is_known_code(code: &str) -> bool {
        RULE_CODES.contains(&code)
    }

    /// Force `code` to error severity.
    pub fn deny(&mut self, code: &str) {
        self.overrides.insert(code.to_string(), Some(Severity::Error));
    }

    /// Force `code` to warning severity.
    pub fn warn(&mut self, code: &str) {
        self.overrides
            .insert(code.to_string(), Some(Severity::Warning));
    }

    /// Suppress `code` entirely.
    pub fn allow(&mut self, code: &str) {
        self.overrides.insert(code.to_string(), None);
    }

    /// Resolve the effective severity for `code` given its `default`.
    ///
    /// - explicit `Some(sev)` override → that severity
    /// - explicit suppression (`None`) → `None` (drop the diagnostic)
    /// - no override → the default severity
    pub fn effective(&self, code: &str, default: Severity) -> Option<Severity> {
        match self.overrides.get(code) {
            Some(Some(sev)) => Some(*sev),
            Some(None) => None,
            None => Some(default),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_suppresses_warn_promotes() {
        let mut cfg = LintConfig::default();
        // No override → default passes through.
        assert_eq!(cfg.effective("x", Severity::Warning), Some(Severity::Warning));

        cfg.allow("suppressed");
        assert_eq!(cfg.effective("suppressed", Severity::Error), None);

        cfg.deny("strict");
        assert_eq!(cfg.effective("strict", Severity::Warning), Some(Severity::Error));

        cfg.warn("relaxed");
        assert_eq!(cfg.effective("relaxed", Severity::Error), Some(Severity::Warning));
    }
}
