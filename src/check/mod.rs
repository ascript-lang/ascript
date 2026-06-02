//! `ascript check` — the analysis core (feature-independent), CLI rendering,
//! config/suppression, and the LSP-facing entry point.
pub mod analyze;
pub mod config;
pub mod diagnostic;
pub mod render;
pub mod rules;
pub use analyze::{analyze, Analysis};
pub use diagnostic::{AsDiagnostic, ByteSpan, Fix, Severity, TextEdit};
