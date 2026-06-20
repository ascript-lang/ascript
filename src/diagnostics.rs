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

/// Render an error to a `String` — the same ariadne report [`report`] writes to
/// stderr, captured into an owned string instead. Used by the EMBED facade
/// (`EmbedError`'s `rendered` field) so a host gets the source-pointing caret
/// report as data, not only on stderr. Mirrors [`report`]'s caret-source selection
/// (span-bound source preferred over the context source) exactly; falls back to the
/// plain `error: <message>` line when no span+source is available.
pub fn render_to_string(err: &AsError) -> String {
    use ariadne::{Color, Label, Report, ReportKind, Source};
    let caret_source = err.span_source.as_ref().or(err.source.as_ref());
    match (caret_source, err.span) {
        (Some(src), Some(span)) => {
            let text = &src.text;
            let start = span.start;
            let end = span.end;
            let path = src.path.as_str();
            let mut buf: Vec<u8> = Vec::new();
            let render = Report::build(ReportKind::Error, (path, start..end))
                .with_message(&err.message)
                .with_label(
                    Label::new((path, start..end))
                        .with_message(&err.message)
                        .with_color(Color::Red),
                )
                .finish()
                .write((path, Source::from(text.as_str())), &mut buf);
            // ariadne writes UTF-8; on the off chance of a write error, fall back to
            // the plain line rather than panicking (this is a diagnostics path).
            match render {
                Ok(()) => String::from_utf8(buf)
                    .unwrap_or_else(|_| format!("error: {}", err)),
                Err(_) => format!("error: {}", err),
            }
        }
        _ => format!("error: {}", err),
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
