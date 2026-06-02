//! Lint configuration: per-code severity overrides and warning promotion.
//!
//! NOTE: `LintConfig`'s severity-override surface (`deny`/`warn`/`allow`/
//! `effective`) is implemented and unit-tested but not yet wired into `analyze`
//! or the CLI — it is staged for the future `--deny <rule>` / `--warn <rule>` /
//! `--allow <rule>` / `ascript.toml` config flags (a recorded follow-up
//! sub-project). Today only `deny_warnings` is consulted (by
//! `ascript check --deny-warnings`, for exit-code purposes). Inline
//! `// ascript-ignore[code]` suppression is fully active (see `analyze.rs`).

use crate::check::diagnostic::Severity;
use std::collections::HashMap;

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
