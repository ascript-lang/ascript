//! `EmbedError` — the error bridge for the embedding facade (spec §3.4).
//!
//! The variants and their structured fields are semver contract; the diagnostic
//! *strings* are NOT (wording may improve). `#[non_exhaustive]` keeps the enum
//! extensible under that contract.

use crate::error::AsError;

/// A single compile-time diagnostic (lex/parse/compile): the message, its char-offset
/// span into the evaluated source, and an ariadne-rendered source-pointing report.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EmbedDiagnostic {
    /// The human diagnostic message (NOT semver-stable wording).
    pub message: String,
    /// Start char offset into the evaluated source (`0` if unknown).
    pub start: usize,
    /// End char offset into the evaluated source (`0` if unknown).
    pub end: usize,
    /// The ariadne-rendered source-pointing report (a caret diagram), as a string.
    pub rendered: String,
}

/// A Tier-2 runtime panic surfaced across the boundary: the message, its optional
/// char-offset span, and the ariadne-rendered report.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EmbedPanic {
    /// The panic message (NOT semver-stable wording).
    pub message: String,
    /// The char-offset `(start, end)` span into the source, when the panic carried one.
    pub span: Option<(usize, usize)>,
    /// The ariadne-rendered source-pointing report (a caret diagram), as a string.
    pub rendered: String,
}

/// The error type every fallible `Isolate` operation returns (spec §3.4).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum EmbedError {
    /// Lex/parse/compile diagnostics (message + span + rendered report each). The
    /// session is NOT mutated when this is returned (the REPL compile-error rule).
    Compile(Vec<EmbedDiagnostic>),
    /// A Tier-2 runtime panic: message, optional span, ariadne-rendered report. The
    /// isolate's session survives (the per-eval fiber is discarded; globals persist).
    Panic(EmbedPanic),
    /// The script called `exit(n)`. The isolate stays usable — the *host* decides what
    /// "exit" means (a documented difference from the CLI, which ends the process).
    Exit(i32),
    /// A blocking `eval`/`call`/`load_archive` was invoked from inside an ambient async
    /// runtime context, where `block_on` would panic (spec §4.1). The fix named in the
    /// message is the `*_async` variant.
    NestedRuntime,
    /// A `call` target is not defined / not callable, or a wrong-shaped argument was
    /// supplied. Carries a human description.
    Undefined(String),
    /// Builder/registration misuse (bad host-module name, duplicate registration, …).
    Config(String),
    /// Archive decode/verify failure (the `.aso` trust boundary; the verifier's
    /// message verbatim).
    Archive(String),
}

impl EmbedError {
    /// Build a `Compile` error from a single `AsError` (the shape `compile_source`
    /// failures take): one diagnostic with its span + rendered report.
    ///
    /// Wired into `eval`'s compile step in Task 1.2; `#[allow(dead_code)]` until then.
    #[allow(dead_code)]
    pub(crate) fn from_compile(err: &AsError) -> Self {
        let (start, end) = err.span.map(|s| (s.start, s.end)).unwrap_or((0, 0));
        EmbedError::Compile(vec![EmbedDiagnostic {
            message: err.message.clone(),
            start,
            end,
            rendered: crate::diagnostics::render_to_string(err),
        }])
    }

    /// Build a `Panic` error from a runtime `AsError`.
    pub(crate) fn from_panic(err: &AsError) -> Self {
        EmbedError::Panic(EmbedPanic {
            message: err.message.clone(),
            span: err.span.map(|s| (s.start, s.end)),
            rendered: crate::diagnostics::render_to_string(err),
        })
    }
}

/// `From<AsError>` defaults a raw engine error to the `Panic` tier (a Tier-2 runtime
/// panic). Compile-time diagnostics use [`EmbedError::from_compile`] explicitly at the
/// compile site, where the `Compile` framing is correct.
impl From<AsError> for EmbedError {
    fn from(err: AsError) -> Self {
        EmbedError::from_panic(&err)
    }
}

impl std::fmt::Display for EmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmbedError::Compile(diags) => {
                if diags.len() == 1 {
                    write!(f, "compile error: {}", diags[0].message)
                } else {
                    write!(f, "{} compile errors", diags.len())?;
                    for d in diags {
                        write!(f, "\n  - {}", d.message)?;
                    }
                    Ok(())
                }
            }
            EmbedError::Panic(p) => write!(f, "panic: {}", p.message),
            EmbedError::Exit(code) => write!(f, "script called exit({code})"),
            EmbedError::NestedRuntime => write!(
                f,
                "blocking eval/call invoked from inside an async runtime — use the \
                 async variant (eval_async/call_async/load_archive_async) instead"
            ),
            EmbedError::Undefined(msg) => write!(f, "{msg}"),
            EmbedError::Config(msg) => write!(f, "configuration error: {msg}"),
            EmbedError::Archive(msg) => write!(f, "archive error: {msg}"),
        }
    }
}

impl std::error::Error for EmbedError {}
