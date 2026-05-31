//! Pretty diagnostic rendering via ariadne.
use crate::error::AsError;

/// Render an error to stderr — a source-pointing report if span+source are
/// present, otherwise a plain `error: <message>` line.
pub fn report(err: &AsError) {
    use ariadne::{Color, Label, Report, ReportKind, Source};
    match (&err.source, err.span) {
        (Some(src), Some(span)) => {
            // Spans are CHAR offsets; ariadne wants byte ranges. Convert.
            let text = &src.text;
            let start = char_to_byte(text, span.start);
            let end = char_to_byte(text, span.end);
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

/// Convert a char offset into a byte offset within `text`.
fn char_to_byte(text: &str, char_off: usize) -> usize {
    text.char_indices()
        .nth(char_off)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}
