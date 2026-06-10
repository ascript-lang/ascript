//! DX D2 Task 6 — line-coverage report rendering (text / LCOV / HTML).
//!
//! Pure formatters over a [`CoverageTable`](crate::vm::instrument::CoverageTable)'s
//! by-file view ([`CoverageTable::by_file`]). No interpreter, no I/O here (the CLI owns
//! the file writes for `--coverage=html`); each function returns an owned `String`.

use crate::vm::instrument::{CoverageTable, FileCoverage};

/// The requested coverage output format (`--coverage[=text|lcov|html]`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CoverageFormat {
    /// Per-file `covered/total (pct%)` + the uncovered-line list + a total (default).
    #[default]
    Text,
    /// Standard LCOV (`SF:`/`DA:`/`LF`/`LH`/`end_of_record`).
    Lcov,
    /// A self-contained colored per-file HTML tree under `target/coverage/`.
    Html,
}

impl CoverageFormat {
    /// Parse the `--coverage=<fmt>` value. Returns `None` for an unknown format (the CLI
    /// renders the error).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "text" => Some(CoverageFormat::Text),
            "lcov" => Some(CoverageFormat::Lcov),
            "html" => Some(CoverageFormat::Html),
            _ => None,
        }
    }
}

/// Render the default TEXT report: one line per file (`covered/total (pct%)` + uncovered
/// line numbers), then a TOTAL line. Files are listed in `by_file`'s stable path order.
pub fn render_text(table: &CoverageTable) -> String {
    let files = table.by_file();
    let mut out = String::new();
    out.push_str("coverage:\n");
    let mut total_covered = 0usize;
    let mut total_lines = 0usize;
    for f in &files {
        total_covered += f.covered();
        total_lines += f.total();
        out.push_str(&format!(
            "  {}: {}/{} ({:.1}%)",
            f.path,
            f.covered(),
            f.total(),
            f.percent()
        ));
        let uncovered = f.uncovered_lines();
        if !uncovered.is_empty() {
            let list: Vec<String> = uncovered.iter().map(|l| l.to_string()).collect();
            out.push_str(&format!("  uncovered: {}", list.join(", ")));
        }
        out.push('\n');
    }
    let pct = if total_lines == 0 {
        0.0
    } else {
        100.0 * total_covered as f64 / total_lines as f64
    };
    out.push_str(&format!(
        "  TOTAL: {total_covered}/{total_lines} ({pct:.1}%)\n"
    ));
    out
}

/// Render an LCOV report. v1 records covered/not (each line traps once then un-patches),
/// so `DA:<line>,1` for a covered line and `DA:<line>,0` otherwise — `LF`/`LH` summarize.
pub fn render_lcov(table: &CoverageTable) -> String {
    let files = table.by_file();
    let mut out = String::new();
    for f in &files {
        out.push_str(&format!("SF:{}\n", f.path));
        for (line, covered) in &f.lines {
            let count = u8::from(*covered);
            out.push_str(&format!("DA:{line},{count}\n"));
        }
        out.push_str(&format!("LF:{}\n", f.total()));
        out.push_str(&format!("LH:{}\n", f.covered()));
        out.push_str("end_of_record\n");
    }
    out
}

/// HTML-escape the five markup-significant characters.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// A minimal self-contained stylesheet for the HTML report (no `docs/` NAV dependency).
const HTML_STYLE: &str = r#"
body{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;background:#14110d;color:#efe6d6;margin:0;padding:2rem;}
h1{color:#f0a92b;font-size:1.3rem;}
table{border-collapse:collapse;width:100%;margin:1rem 0;}
td,th{padding:.3rem .6rem;border-bottom:1px solid #2a2419;text-align:left;}
th{color:#9d917b;font-weight:600;}
a{color:#74b6c9;text-decoration:none;}
a:hover{text-decoration:underline;}
.pct{color:#f0a92b;}
.src{background:#1b1712;border:1px solid #2a2419;border-radius:6px;overflow:hidden;margin:1rem 0;}
.ln{display:flex;}
.ln .no{width:4rem;text-align:right;padding:0 .8rem;color:#9d917b;user-select:none;}
.ln .code{flex:1;white-space:pre;padding:0 .8rem;}
.hit{background:rgba(116,182,201,.12);}
.miss{background:rgba(240,80,80,.18);}
"#;

/// Render a single self-contained HTML page (index + every file's source view inlined).
/// `sources` maps a file path → its full source text so missed/hit lines can be colored;
/// a path absent from `sources` renders the line list without source text (degrades
/// cleanly). The result is ONE page (no external assets), written by the CLI to
/// `target/coverage/index.html`.
pub fn render_html(table: &CoverageTable, sources: &[(String, String)]) -> String {
    let files = table.by_file();
    let mut total_covered = 0usize;
    let mut total_lines = 0usize;
    for f in &files {
        total_covered += f.covered();
        total_lines += f.total();
    }
    let total_pct = if total_lines == 0 {
        0.0
    } else {
        100.0 * total_covered as f64 / total_lines as f64
    };

    let mut body = String::new();
    body.push_str(&format!(
        "<h1>Coverage — {total_covered}/{total_lines} ({total_pct:.1}%)</h1>\n"
    ));
    // Summary table.
    body.push_str("<table><tr><th>File</th><th>Covered</th><th>Total</th><th>%</th></tr>\n");
    for (i, f) in files.iter().enumerate() {
        body.push_str(&format!(
            "<tr><td><a href=\"#f{i}\">{}</a></td><td>{}</td><td>{}</td><td class=\"pct\">{:.1}%</td></tr>\n",
            html_escape(&f.path),
            f.covered(),
            f.total(),
            f.percent()
        ));
    }
    body.push_str("</table>\n");

    // Per-file source view.
    for (i, f) in files.iter().enumerate() {
        body.push_str(&format!(
            "<h2 id=\"f{i}\">{}</h2>\n",
            html_escape(&f.path)
        ));
        body.push_str(&render_file_source(f, source_for(sources, &f.path)));
    }

    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Coverage</title>\
         <style>{HTML_STYLE}</style></head><body>{body}</body></html>"
    )
}

fn source_for<'a>(sources: &'a [(String, String)], path: &str) -> Option<&'a str> {
    sources
        .iter()
        .find(|(p, _)| p == path)
        .map(|(_, s)| s.as_str())
}

/// Render one file's colored source block: each instrumented line gets a hit/miss class;
/// non-instrumented lines render plain. Falls back to a bare line list if no source text.
fn render_file_source(f: &FileCoverage, source: Option<&str>) -> String {
    use std::collections::HashMap;
    let status: HashMap<u32, bool> = f.lines.iter().copied().collect();
    let mut out = String::from("<div class=\"src\">\n");
    match source {
        Some(text) => {
            for (idx, line) in text.lines().enumerate() {
                let lineno = idx as u32 + 1;
                let class = match status.get(&lineno) {
                    Some(true) => " hit",
                    Some(false) => " miss",
                    None => "",
                };
                out.push_str(&format!(
                    "<div class=\"ln{class}\"><span class=\"no\">{lineno}</span><span class=\"code\">{}</span></div>\n",
                    html_escape(line)
                ));
            }
        }
        None => {
            for (lineno, covered) in &f.lines {
                let class = if *covered { " hit" } else { " miss" };
                out.push_str(&format!(
                    "<div class=\"ln{class}\"><span class=\"no\">{lineno}</span><span class=\"code\"></span></div>\n"
                ));
            }
        }
    }
    out.push_str("</div>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CoverageTable {
        let mut t = CoverageTable::new();
        let pid = 1usize;
        t.record_path(pid, "a.as".to_string());
        // lines 0,1,2 instrumented (0-based); cover 0 and 2, miss 1.
        t.record_trap(pid, 0, 1, 0);
        t.record_trap(pid, 2, 1, 1);
        t.record_trap(pid, 4, 1, 2);
        t.mark_covered(pid, 0);
        t.mark_covered(pid, 2);
        t
    }

    #[test]
    fn text_lists_uncovered() {
        let out = render_text(&sample());
        // 1-based: covered lines 1 and 3, uncovered line 2.
        assert!(out.contains("a.as: 2/3"), "{out}");
        assert!(out.contains("uncovered: 2"), "{out}");
        assert!(out.contains("TOTAL: 2/3"), "{out}");
    }

    #[test]
    fn lcov_da_records() {
        let out = render_lcov(&sample());
        assert!(out.contains("SF:a.as\n"), "{out}");
        assert!(out.contains("DA:1,1\n"), "{out}");
        assert!(out.contains("DA:2,0\n"), "{out}");
        assert!(out.contains("DA:3,1\n"), "{out}");
        assert!(out.contains("LF:3\n"), "{out}");
        assert!(out.contains("LH:2\n"), "{out}");
        assert!(out.contains("end_of_record\n"), "{out}");
    }

    #[test]
    fn html_is_self_contained() {
        let out = render_html(&sample(), &[("a.as".to_string(), "x\ny\nz\n".to_string())]);
        assert!(out.starts_with("<!doctype html>"));
        assert!(out.contains("<style>"), "inline style, no external asset");
        assert!(out.contains("class=\"ln hit\""));
        assert!(out.contains("class=\"ln miss\""));
    }

    #[test]
    fn html_escapes_source() {
        let out = render_html(
            &sample(),
            &[("a.as".to_string(), "<script>\ny\nz\n".to_string())],
        );
        assert!(out.contains("&lt;script&gt;"));
        assert!(!out.contains("<script>"));
    }

    #[test]
    fn format_parse() {
        assert_eq!(CoverageFormat::parse("text"), Some(CoverageFormat::Text));
        assert_eq!(CoverageFormat::parse("lcov"), Some(CoverageFormat::Lcov));
        assert_eq!(CoverageFormat::parse("html"), Some(CoverageFormat::Html));
        assert_eq!(CoverageFormat::parse("bogus"), None);
    }
}
