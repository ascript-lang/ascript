//! Analysis driver (filled by later tasks).

#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub diagnostics: Vec<crate::check::diagnostic::AsDiagnostic>,
}

pub fn analyze(_: &str) -> Analysis {
    Analysis::default()
}
