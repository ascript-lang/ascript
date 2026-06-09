//! Lint configuration: per-code severity overrides and warning promotion.
//!
//! `LintConfig`'s severity-override surface (`deny`/`warn`/`allow`/`effective`)
//! is wired into `analyze_with_config` and the `ascript check` CLI flags
//! `--deny`/`--warn`/`--allow <rule>` (repeatable). `deny_warnings` additionally
//! promotes all surviving warnings for exit-code purposes
//! (`ascript check --deny-warnings`). The `ascript.toml` `[lint]` table
//! (discovered + parsed in the `check::config_toml` module) also
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
    "call-arity",
    "range-step",
    "invalid-propagate",
    "unresolved-import",
    "unknown-enum-variant",
    // ADT — algebraic enums + exhaustive match.
    "non-exhaustive-match",
    "enum-variant-binding-shadow",
    "duplicate-member",
    "super-misuse",
    "field-default-type",
    // SP10 — the advisory gradual type checker (all default Warning).
    "type-mismatch",
    "type-error",
    "possibly-nil",
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
        self.overrides
            .insert(code.to_string(), Some(Severity::Error));
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
    ///
    /// **TYPE soundness opt-out (the blocking default is opt-out, not opt-in).**
    /// The `default` argument is the rule's *emitted* severity, which since TYPE is
    /// `Severity::Error` for a `type-mismatch`/`type-error` against a syntactically
    /// **annotated** slot (an explicit `let x: T`, a typed param, a declared return,
    /// a typed field default) and `Severity::Warning` everywhere else (an inferred
    /// slot, `possibly-nil`). This method composes that with the project config with
    /// no special-casing: a `[lint] warn = ["type-mismatch"]` (`self.warn(...)`)
    /// override returns `Some(Warning)` — **downgrading the blocking annotated error
    /// back to an advisory warning** (the explicit, documented opt-out for teams
    /// mid-migration) — while *no* override passes the blocking `Error` through
    /// (the soundness default stays blocking; backward-compat is not a constraint).
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
        assert_eq!(
            cfg.effective("x", Severity::Warning),
            Some(Severity::Warning)
        );

        cfg.allow("suppressed");
        assert_eq!(cfg.effective("suppressed", Severity::Error), None);

        cfg.deny("strict");
        assert_eq!(
            cfg.effective("strict", Severity::Warning),
            Some(Severity::Error)
        );

        cfg.warn("relaxed");
        assert_eq!(
            cfg.effective("relaxed", Severity::Error),
            Some(Severity::Warning)
        );
    }

    #[test]
    fn type_blocking_default_is_opt_out_via_warn() {
        // TYPE Task 2: the annotated-slot `type-mismatch` is emitted at the BLOCKING
        // `Severity::Error` default (the `default` argument). With NO override it
        // passes through as a blocking Error (the soundness default).
        let cfg = LintConfig::default();
        assert_eq!(
            cfg.effective("type-mismatch", Severity::Error),
            Some(Severity::Error),
            "default (no override) keeps the annotated type-mismatch blocking",
        );

        // `[lint] warn = ["type-mismatch"]` (→ `warn(...)`) DOWNGRADES the blocking
        // Error back to a Warning — the explicit opt-out, composed purely through
        // `effective` (no special-casing).
        let mut warned = LintConfig::default();
        warned.warn("type-mismatch");
        assert_eq!(
            warned.effective("type-mismatch", Severity::Error),
            Some(Severity::Warning),
            "warn override downgrades the blocking annotated Error to a Warning",
        );

        // An advisory (`possibly-nil`/inferred) `type-mismatch` defaults to Warning
        // and is unaffected by the absence of an override.
        assert_eq!(
            cfg.effective("possibly-nil", Severity::Warning),
            Some(Severity::Warning),
        );
    }
}
