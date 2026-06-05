//! `ascript check` — the analysis core (feature-independent), CLI rendering,
//! config/suppression, and the LSP-facing entry point.
pub mod analyze;
pub mod config;
pub mod config_toml;
pub mod diagnostic;
pub mod fix;
pub mod infer;
pub mod render;
pub mod rules;
pub mod std_arity;
pub use analyze::{analyze, Analysis};
pub use config::{LintConfig, RULE_CODES};
pub use diagnostic::{AsDiagnostic, ByteSpan, Fix, Severity, TextEdit};
pub use fix::{apply_edits, collect_fixes, FIXABLE_CODES};
