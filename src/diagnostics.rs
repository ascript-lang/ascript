//! Pretty diagnostic rendering via ariadne.
use crate::error::AsError;

/// Render an error to stderr — a source-pointing report if span+source are
/// present, otherwise a plain `error: <message>` line.
pub fn report(err: &AsError) {
    use ariadne::{Color, Label, Report, ReportKind, Source};
    // Prefer the span's OWN source (bound at raise time) for the caret, so a span
    // is never rendered against a different module's text (SP4 §3 cross-module
    // provenance). Fall back to the outer/context `source` when no span-source is
    // bound (single-module errors set neither or both to the same `SourceInfo`, so
    // they are byte-identical to before).
    let caret_source = err.span_source.as_ref().or(err.source.as_ref());
    match (caret_source, err.span) {
        (Some(src), Some(span)) => {
            // Spans are CHAR offsets and ariadne 0.6 renders in its default
            // `IndexType::Char` mode, so the range is passed straight through — NO
            // char→byte conversion (the legacy and CST front-ends both produce CHAR
            // spans; converting here would desync the caret on multibyte source).
            let text = &src.text;
            let start = span.start;
            let end = span.end;
            let path = src.path.as_str();
            let _ = Report::build(ReportKind::Error, (path, start..end))
                .with_message(&err.message)
                .with_label(
                    Label::new((path, start..end))
                        .with_message(&err.message)
                        .with_color(Color::Red),
                )
                .finish()
                .eprint((path, Source::from(text.as_str())));
        }
        _ => eprintln!("error: {}", err),
    }
}

/// Render MULTIPLE errors together (DX D4 §5.1 multi-error reporting). Each error
/// becomes its OWN ariadne report (matching [`report`]'s styling), printed in the
/// order given — so a file with several parse errors shows them all at once
/// instead of bailing on the first. Callers batch only recoverable, parse-time
/// diagnostics here; a single fatal runtime panic (a Tier-2 abort) still goes
/// through [`report`] as one report. An empty slice prints nothing.
pub fn report_all(errors: &[AsError]) {
    for err in errors {
        report(err);
    }
}
