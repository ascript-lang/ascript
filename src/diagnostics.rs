//! Pretty diagnostic rendering via ariadne.
use crate::error::AsError;

/// Render an error to stderr — a source-pointing report if span+source are
/// present, otherwise a plain `error: <message>` line.
///
/// A one-line wrapper over [`render_to_string`] with color ON (the byte-identical
/// reuse refactor WASM §5.4 asked for — stderr output is unchanged; the only new
/// surface is the `color` toggle the wasm playground passes as `false`). ariadne's
/// `eprint`/`write` share the same renderer, so `eprint!`-ing the captured string
/// is byte-identical to the prior direct `.eprint(...)`. The no-span fallback adds
/// the trailing newline the legacy `eprintln!` emitted (`render_to_string` itself
/// does NOT — its embed/wasm callers want the bare report).
pub fn report(err: &AsError) {
    let s = render_to_string(err, true);
    let has_caret = (err.span_source.as_ref().or(err.source.as_ref())).is_some() && err.span.is_some();
    if has_caret {
        eprint!("{s}");
    } else {
        // The plain `error: <message>` fallback — match the legacy `eprintln!`.
        eprintln!("{s}");
    }
}

/// Render an error to a `String` — the same ariadne report [`report`] writes to
/// stderr, captured into an owned string instead. Used by the EMBED facade
/// (`EmbedError`'s `rendered` field) so a host gets the source-pointing caret
/// report as data, not only on stderr, AND by the WASM playground wrapper
/// (`color = false` → no ANSI escapes in the JS-facing error string, WASM §5.4).
/// Mirrors the caret-source selection (span-bound source preferred over the
/// context source) exactly; falls back to the plain `error: <message>` line when
/// no span+source is available.
///
/// `color` toggles ariadne's `Config` color: `true` reproduces the legacy colored
/// stderr/embed output byte-identically; `false` emits a plain ANSI-free report.
/// The plain (`error: <message>`) fallback is ANSI-free in both cases.
pub fn render_to_string(err: &AsError, color: bool) -> String {
    use ariadne::{Color, Config, Label, Report, ReportKind, Source};
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
            let mut buf: Vec<u8> = Vec::new();
            let render = Report::build(ReportKind::Error, (path, start..end))
                .with_config(Config::default().with_color(color))
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
                Ok(()) => {
                    String::from_utf8(buf).unwrap_or_else(|_| format!("error: {}", err))
                }
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
