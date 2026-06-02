# Checker — Analysis Core + `ascript check` CLI + LSP Rewiring (Plan C1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the feature-independent analysis core (`AsDiagnostic`), the `ascript check` CLI (ariadne + `--json`, exit codes, **all** syntax errors not just the first), the config/suppression machinery, and rewire the LSP onto the same core — so a script can be checked from the terminal, CI, and any editor identically.

**Architecture:** A new `src/check/` module (NOT behind any feature flag) drives lex → parse (Plan 2's error-recovering parser) → resolve (Plan 3) → run enabled rules → collect/sort/dedup → `Vec<AsDiagnostic>`. This plan ships **Tier-0 (syntax errors)** as the only rule (the lint tiers are sub-projects #4–#6); the resolver runs so the pipeline is wired, even though no lint consumes it yet. The CLI renders with ariadne; the LSP maps `AsDiagnostic` → `lsp_types::Diagnostic`.

**Tech Stack:** Rust, the Plan 1/2/3 `src/syntax/*` pipeline, `ariadne` (already a dep), `clap`, `serde`/`serde_json` (for `--json`; gate behind the existing `data` feature, fall back to a hand-rolled JSON writer when absent).

**Scope note:** Checker sub-project #3 (spec: `docs/superpowers/specs/2026-06-02-checker-design.md`). Depends on Plans 2 (parser) + 3 (resolver). The lint rules (`undefined-variable`, `unawaited-future`, etc.) are #4–#6, each its own plan. Does not touch the interpreter.

**Key win shipped here:** `ascript check foo.as` reports **every** syntax error with a source-pointing diagnostic and a CI-meaningful exit code — the capability that does not exist today.

---

## File Structure

- Create `src/check/mod.rs` — re-exports; `pub use`.
- Create `src/check/diagnostic.rs` — `AsDiagnostic`, `Severity`, `Fix`, `TextEdit`, `ByteSpan`.
- Create `src/check/analyze.rs` — `analyze(src) -> Analysis`; the driver + syntax-error collection.
- Create `src/check/config.rs` — `LintConfig` (deny/warn/allow), severity resolution, suppression reader.
- Create `src/check/render.rs` — ariadne human output + JSON output.
- Modify `src/lib.rs` — `pub mod check;`.
- Modify `src/main.rs` — `Command::Check { … }`.
- Modify `src/lsp/analysis.rs` — `diagnostics()` becomes an adapter over `check::analyze`.
- Create `tests/check.rs` — CLI integration tests.

---

## Task 1: The diagnostic model

**Files:**
- Create: `src/check/diagnostic.rs`
- Create: `src/check/mod.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Define the types + a test**

Create `src/check/diagnostic.rs`:

```rust
//! Neutral diagnostic model. ariadne (CLI), the LSP, and `--json` all derive
//! from this — the analysis core never depends on LSP types.

/// A half-open byte range into the source (ariadne + cstree are byte-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteSpan {
    pub start: usize,
    pub end: usize,
}

impl From<cstree::text::TextRange> for ByteSpan {
    fn from(r: cstree::text::TextRange) -> Self {
        ByteSpan { start: r.start().into(), end: r.end().into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

#[derive(Debug, Clone)]
pub struct TextEdit {
    pub range: ByteSpan,
    pub replacement: String,
}

#[derive(Debug, Clone)]
pub struct Fix {
    pub title: String,
    pub edits: Vec<TextEdit>,
}

/// One diagnostic. `code` is a stable machine string (e.g. "syntax-error",
/// "unused-binding") used by config/suppression/CI.
#[derive(Debug, Clone)]
pub struct AsDiagnostic {
    pub range: ByteSpan,
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub fix: Option<Fix>,
}

impl AsDiagnostic {
    pub fn error(code: &str, range: ByteSpan, message: impl Into<String>) -> Self {
        AsDiagnostic { range, severity: Severity::Error, code: code.into(), message: message.into(), fix: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostic_constructs() {
        let d = AsDiagnostic::error("syntax-error", ByteSpan { start: 0, end: 3 }, "boom");
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.code, "syntax-error");
    }
}
```

Create `src/check/mod.rs`:

```rust
//! `ascript check` — the analysis core (feature-independent), CLI rendering,
//! config/suppression, and the LSP-facing entry point.

pub mod analyze;
pub mod config;
pub mod diagnostic;
pub mod render;

pub use analyze::{analyze, Analysis};
pub use diagnostic::{AsDiagnostic, ByteSpan, Fix, Severity, TextEdit};
```

- [ ] **Step 2: Register the module**

In `src/lib.rs` add (near the other `pub mod`s):

```rust
pub mod check;
```

> `analyze`/`config`/`render` don't exist yet (Tasks 2–4). To compile Task 1, create minimal stubs for those three modules now (empty `analyze.rs` with `pub struct Analysis; pub fn analyze(_: &str) -> Analysis { Analysis }` etc.), filled in by their tasks. Simplest: create the three files as stubs in Step 1 and flesh them out per task.

- [ ] **Step 3: Run + commit**

Run: `cargo test --lib check::diagnostic 2>&1 | tail -15`
Expected: PASS. (If `TextRange::start().into()` needs `u32`→`usize`, use `usize::from(r.start())`.)

```bash
git add src/check/ src/lib.rs
git commit -m "feat(check): neutral diagnostic model (AsDiagnostic)"
```

---

## Task 2: The analysis driver + syntax-error collection (all errors)

**Files:**
- Modify: `src/check/analyze.rs`

- [ ] **Step 1: Write the all-errors test**

Replace `src/check/analyze.rs` stub with the real types + test:

```rust
//! The analysis driver: parse (error-recovering) → resolve → run rules →
//! collect/sort. This plan implements Tier-0 (syntax errors); lint tiers are
//! sub-projects #4–#6.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::syntax::lexer::LexToken;
use crate::syntax::parser::{parse, Parse, ParseError};

#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub diagnostics: Vec<AsDiagnostic>,
}

/// Analyze a source string. Always returns (never errors) — a malformed program
/// yields syntax diagnostics, not an abort (the run path enforces all-or-nothing
/// separately).
pub fn analyze(src: &str) -> Analysis {
    let parsed = parse(src);
    let mut diagnostics = Vec::new();

    // Tier 0: every syntax error, each with a source range.
    for err in &parsed.errors {
        diagnostics.push(AsDiagnostic {
            range: error_range(&parsed, err),
            severity: Severity::Error,
            code: "syntax-error".to_string(),
            message: err.message.clone(),
            fix: None,
        });
    }

    // (Resolver runs in #4+ when lints consume it; wired here later.)

    // Sort by start, then by code for stable output.
    diagnostics.sort_by(|a, b| a.range.start.cmp(&b.range.start).then(a.code.cmp(&b.code)));
    Analysis { diagnostics }
}

/// Byte range of a ParseError. `token_index` indexes the NON-trivia tokens; walk
/// the full token stream summing byte lengths until the n-th non-trivia token.
fn error_range(parsed: &Parse, err: &ParseError) -> ByteSpan {
    let mut byte = 0usize;
    let mut nontrivia_seen = 0usize;
    for t in &parsed.tokens {
        let len = t.text.len();
        if !t.kind.is_trivia() {
            if nontrivia_seen == err.token_index {
                return ByteSpan { start: byte, end: byte + len.max(1) };
            }
            nontrivia_seen += 1;
        }
        byte += len;
    }
    // Past the end (EOF error): point at the final byte.
    let end = total_len(&parsed.tokens);
    ByteSpan { start: end.saturating_sub(1), end }
}

fn total_len(tokens: &[LexToken]) -> usize {
    tokens.iter().map(|t| t.text.len()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_no_diagnostics_for_valid_program() {
        assert!(analyze("let x = 1\nprint(x)\n").diagnostics.is_empty());
    }

    #[test]
    fn reports_all_syntax_errors_not_just_first() {
        // Two broken statements → at least two syntax-error diagnostics
        // (error-recovering parser keeps going).
        let a = analyze("let = 1\nlet = 2\n");
        let syntax = a.diagnostics.iter().filter(|d| d.code == "syntax-error").count();
        assert!(syntax >= 2, "expected ≥2 syntax errors, got {syntax}: {:?}", a.diagnostics);
    }

    #[test]
    fn diagnostic_has_a_plausible_range() {
        let a = analyze("@\n"); // a stray unrecognized token
        assert!(!a.diagnostics.is_empty());
        let d = &a.diagnostics[0];
        assert!(d.range.start < d.range.end);
    }
}
```

- [ ] **Step 2: Run (expect failure → pass)**

Run: `cargo test --lib check::analyze 2>&1 | tail -20`
Expected: PASS. (`reports_all_syntax_errors_not_just_first` is the headline behavior vs today's first-error-only analysis. If the parser's recovery doesn't yet produce ≥2 errors for that input, the parser recovery — Plan 2 — is the place to strengthen; the test pins the requirement.)

- [ ] **Step 3: Commit**

```bash
git add src/check/analyze.rs
git commit -m "feat(check): analysis driver + all-syntax-errors collection"
```

---

## Task 3: Config + suppression (CST-trivia `// ascript-ignore[code]`)

**Files:**
- Modify: `src/check/config.rs`
- Modify: `src/check/analyze.rs`

- [ ] **Step 1: Tests**

Replace `src/check/config.rs` stub:

```rust
//! Rule configuration (deny/warn/allow) + inline suppression parsing.

use crate::check::diagnostic::Severity;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct LintConfig {
    /// rule code → forced severity (None = rule's default).
    overrides: HashMap<String, Option<Severity>>,
    /// Treat all warnings as errors for exit purposes.
    pub deny_warnings: bool,
}

impl LintConfig {
    pub fn deny(&mut self, code: &str) { self.overrides.insert(code.into(), Some(Severity::Error)); }
    pub fn warn(&mut self, code: &str) { self.overrides.insert(code.into(), Some(Severity::Warning)); }
    pub fn allow(&mut self, code: &str) { self.overrides.insert(code.into(), None); } // None = suppressed

    /// Resolve the effective severity for a rule given its default.
    /// Returns None when the rule is allowed/suppressed entirely.
    pub fn effective(&self, code: &str, default: Severity) -> Option<Severity> {
        match self.overrides.get(code) {
            Some(Some(sev)) => Some(*sev),
            Some(None) => None, // allowed → suppressed
            None => Some(default),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allow_suppresses_warn_promotes() {
        let mut c = LintConfig::default();
        c.allow("unused-binding");
        c.deny("undefined-variable");
        assert_eq!(c.effective("unused-binding", Severity::Warning), None);
        assert_eq!(c.effective("undefined-variable", Severity::Warning), Some(Severity::Error));
        assert_eq!(c.effective("shadowing", Severity::Hint), Some(Severity::Hint));
    }
}
```

Add inline-suppression parsing. In `analyze.rs` add a suppression test + reader:

```rust
    #[test]
    fn inline_ignore_suppresses_on_that_line() {
        // The directive on a line silences the listed rule for diagnostics on
        // that line. (Demonstrated with a synthetic code via the suppression set.)
        let src = "// ascript-ignore[syntax-error]\n@\n";
        let supp = crate::check::analyze::suppressions(src);
        // line 0 (the directive line) suppresses syntax-error; the broken token
        // is on line 1, so this directive does NOT cover it — proves line scoping.
        assert!(supp.suppressed_on_line(0, "syntax-error"));
        assert!(!supp.suppressed_on_line(1, "syntax-error"));
    }
```

Add to `analyze.rs`:

```rust
use std::collections::HashSet;

/// Parsed inline suppression directives, keyed by 0-based line.
#[derive(Debug, Default)]
pub struct Suppressions {
    /// line → set of suppressed rule codes ("*" = all). Applies to the directive
    /// line AND the next line (so `// ascript-ignore[x]` above an item works).
    per_line: std::collections::HashMap<usize, HashSet<String>>,
    file_wide: HashSet<String>,
}

impl Suppressions {
    pub fn suppressed_on_line(&self, line: usize, code: &str) -> bool {
        if self.file_wide.contains(code) || self.file_wide.contains("*") {
            return true;
        }
        // a directive on `line` or the line above covers `line`.
        for l in [line, line.wrapping_sub(1)] {
            if let Some(set) = self.per_line.get(&l) {
                if set.contains(code) || set.contains("*") {
                    return true;
                }
            }
        }
        false
    }
}

/// Scan comment tokens for `// ascript-ignore[a, b]` / `// ascript-ignore-file[..]`
/// (bare = all rules). Uses the lossless token stream (comments included).
pub fn suppressions(src: &str) -> Suppressions {
    use crate::syntax::kind::SyntaxKind;
    let mut s = Suppressions::default();
    let mut line = 0usize;
    for t in crate::syntax::lex(src) {
        let is_comment = matches!(t.kind, SyntaxKind::LineComment | SyntaxKind::BlockComment);
        if is_comment {
            if let Some((file_wide, codes)) = parse_ignore(&t.text) {
                if file_wide {
                    s.file_wide.extend(codes);
                } else {
                    s.per_line.entry(line).or_default().extend(codes);
                }
            }
        }
        // advance the line counter by the newlines in this token's text.
        line += t.text.matches('\n').count();
    }
    s
}

/// Parse an ignore directive's codes. Returns (file_wide, codes); bare directive
/// yields `["*"]`. None if the comment isn't an ignore directive.
fn parse_ignore(comment: &str) -> Option<(bool, Vec<String>)> {
    let body = comment.trim_start_matches('/').trim_start_matches('*').trim();
    let (file_wide, rest) = if let Some(r) = body.strip_prefix("ascript-ignore-file") {
        (true, r)
    } else if let Some(r) = body.strip_prefix("ascript-ignore") {
        (false, r)
    } else {
        return None;
    };
    let rest = rest.trim();
    if rest.is_empty() {
        return Some((file_wide, vec!["*".to_string()]));
    }
    // expect `[a, b, c]`
    let inner = rest.trim_start_matches('[').trim_end_matches(']');
    let codes = inner.split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect();
    Some((file_wide, codes))
}
```

Wire suppression into `analyze`: after collecting diagnostics, drop those suppressed on their line. Add a line-index helper (byte offset → line) and filter:

```rust
    // ... after building `diagnostics`, before sorting:
    let supp = suppressions(src);
    let line_starts = line_start_offsets(src);
    diagnostics.retain(|d| {
        let line = line_of(&line_starts, d.range.start);
        !supp.suppressed_on_line(line, &d.code)
    });
```

```rust
fn line_start_offsets(src: &str) -> Vec<usize> {
    let mut v = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            v.push(i + 1);
        }
    }
    v
}
fn line_of(line_starts: &[usize], byte: usize) -> usize {
    match line_starts.binary_search(&byte) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    }
}
```

- [ ] **Step 2: Run + commit**

Run: `cargo test --lib check 2>&1 | tail -20`
Expected: config + suppression tests PASS.

```bash
git add src/check/config.rs src/check/analyze.rs
git commit -m "feat(check): lint config + inline ascript-ignore suppression"
```

---

## Task 4: Rendering — ariadne human output + `--json`

**Files:**
- Modify: `src/check/render.rs`

- [ ] **Step 1: Tests**

Replace `src/check/render.rs` stub:

```rust
//! Render diagnostics for the CLI: human (ariadne) and machine (`--json`).

use crate::check::diagnostic::{AsDiagnostic, Severity};

/// Render diagnostics as human-readable text using ariadne, pointing at `src`.
/// `path` labels the source. Returns the rendered report (empty if no diags).
pub fn human(path: &str, src: &str, diags: &[AsDiagnostic]) -> String {
    use ariadne::{Label, Report, ReportKind, Source};
    let mut buf = Vec::new();
    for d in diags {
        let kind = match d.severity {
            Severity::Error => ReportKind::Error,
            Severity::Warning => ReportKind::Warning,
            Severity::Info | Severity::Hint => ReportKind::Advice,
        };
        let mut out = Vec::new();
        Report::build(kind, path, d.range.start)
            .with_code(&d.code)
            .with_message(&d.message)
            .with_label(Label::new((path, d.range.start..d.range.end)).with_message(&d.message))
            .finish()
            .write((path, Source::from(src)), &mut out)
            .ok();
        buf.extend_from_slice(&out);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Render diagnostics as a JSON array (hand-written; no serde dependency so it
/// works in every feature config).
pub fn json(path: &str, diags: &[AsDiagnostic]) -> String {
    let mut s = String::from("[");
    for (i, d) in diags.iter().enumerate() {
        if i > 0 { s.push(','); }
        let sev = match d.severity {
            Severity::Error => "error", Severity::Warning => "warning",
            Severity::Info => "info", Severity::Hint => "hint",
        };
        s.push_str(&format!(
            "{{\"path\":{},\"code\":{},\"severity\":\"{}\",\"start\":{},\"end\":{},\"message\":{}}}",
            json_str(path), json_str(&d.code), sev, d.range.start, d.range.end, json_str(&d.message)
        ));
    }
    s.push(']');
    s
}

/// Minimal JSON string escaper.
fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::diagnostic::{AsDiagnostic, ByteSpan};

    #[test]
    fn json_shape() {
        let d = AsDiagnostic::error("syntax-error", ByteSpan { start: 0, end: 1 }, "bad \"x\"");
        let out = json("f.as", std::slice::from_ref(&d));
        assert!(out.starts_with('['));
        assert!(out.contains("\"code\":\"syntax-error\""));
        assert!(out.contains("\"severity\":\"error\""));
        assert!(out.contains("\\\"x\\\""), "message must be JSON-escaped: {out}");
    }

    #[test]
    fn human_points_at_source() {
        let d = AsDiagnostic::error("syntax-error", ByteSpan { start: 0, end: 3 }, "boom");
        let out = human("f.as", "let = 1\n", std::slice::from_ref(&d));
        assert!(out.contains("boom"));
        assert!(out.contains("syntax-error"));
    }
}
```

- [ ] **Step 2: Run + commit**

Run: `cargo test --lib check::render 2>&1 | tail -15`
Expected: PASS. (If ariadne's `Report::build` signature differs by version, adjust the builder call; `ariadne` is already a dependency — match the version in `Cargo.lock`.)

```bash
git add src/check/render.rs
git commit -m "feat(check): ariadne + json rendering"
```

---

## Task 5: The `ascript check` CLI command

**Files:**
- Modify: `src/main.rs`
- Create: `tests/check.rs`

- [ ] **Step 1: Add the `Check` subcommand**

In `src/main.rs`, add to the `Command` enum (after `Fmt`):

```rust
    /// Statically check .as files (syntax + lints)
    Check {
        files: Vec<String>,
        /// Emit machine-readable JSON instead of human output.
        #[arg(long)]
        json: bool,
        /// Treat all warnings as errors (non-zero exit on any warning).
        #[arg(long)]
        deny_warnings: bool,
    },
```

In `main`'s `match cli.command`, add the arm:

```rust
        Command::Check { files, json, deny_warnings } => {
            let mut any_error = false;
            let mut any_warning = false;
            for file in &files {
                let src = match std::fs::read_to_string(file) {
                    Ok(s) => s,
                    Err(e) => { eprintln!("{}: {}", file, e); any_error = true; continue; }
                };
                let analysis = ascript::check::analyze(&src);
                for d in &analysis.diagnostics {
                    match d.severity {
                        ascript::check::Severity::Error => any_error = true,
                        ascript::check::Severity::Warning => any_warning = true,
                        _ => {}
                    }
                }
                if json {
                    println!("{}", ascript::check::render::json(file, &analysis.diagnostics));
                } else {
                    print!("{}", ascript::check::render::human(file, &src, &analysis.diagnostics));
                }
            }
            let fail = any_error || (deny_warnings && any_warning);
            if fail { ExitCode::from(1) } else { ExitCode::SUCCESS }
        }
```

> If no files are given, mirror `Fmt`'s behavior (check `*.as` under cwd). For this plan, requiring explicit files is acceptable; the no-args glob can be added when `Fmt` gets it too.

- [ ] **Step 2: CLI integration tests**

Create `tests/check.rs`:

```rust
//! Integration tests for `ascript check` (spawns the built binary).

use std::process::Command;

fn bin() -> &'static str { env!("CARGO_BIN_EXE_ascript") }

fn write_tmp(name: &str, contents: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("ascript_check_tests");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, contents).unwrap();
    p
}

#[test]
fn clean_file_exits_zero() {
    let p = write_tmp("ok.as", "let x = 1\nprint(x)\n");
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn syntax_error_exits_nonzero_and_reports() {
    let p = write_tmp("bad.as", "let = 1\n");
    let out = Command::new(bin()).arg("check").arg(&p).output().unwrap();
    assert!(!out.status.success(), "should fail on syntax error");
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(combined.contains("syntax-error"), "should name the rule: {combined}");
}

#[test]
fn json_output_is_a_json_array() {
    let p = write_tmp("bad2.as", "let = 1\n");
    let out = Command::new(bin()).arg("check").arg("--json").arg(&p).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.trim_start().starts_with('['), "json output: {stdout}");
    assert!(stdout.contains("\"code\":\"syntax-error\""));
}
```

- [ ] **Step 3: Run + commit**

Run: `cargo test --test check 2>&1 | tail -20`
Expected: all three PASS.

```bash
git add src/main.rs tests/check.rs
git commit -m "feat(cli): ascript check command (human + json + exit codes)"
```

---

## Task 6: Rewire the LSP onto the analysis core

**Files:**
- Modify: `src/lsp/analysis.rs`

- [ ] **Step 1: Inspect the current diagnostics fn**

Run: `sed -n '68,96p' src/lsp/analysis.rs`
Expected: shows `pub fn diagnostics(text: &str) -> Vec<Diagnostic>` doing lex+parse and `error_diagnostic` building one LSP `Diagnostic`.

- [ ] **Step 2: Replace its body with an adapter over `check::analyze`**

Rewrite `diagnostics` to call the core and map each `AsDiagnostic` → `lsp_types::Diagnostic` (the byte range → LSP line/col via the existing `LineIndex`; note `LineIndex` is char-based, so build positions from byte offsets by converting through the source — add a `byte_to_position` helper if `LineIndex` only accepts char offsets):

```rust
pub fn diagnostics(text: &str) -> Vec<Diagnostic> {
    let analysis = crate::check::analyze(text);
    let index = LineIndex::new(text);
    analysis
        .diagnostics
        .iter()
        .map(|d| Diagnostic {
            range: Range {
                start: index.position(byte_to_char(text, d.range.start)),
                end: index.position(byte_to_char(text, d.range.end)),
            },
            severity: Some(match d.severity {
                crate::check::Severity::Error => DiagnosticSeverity::ERROR,
                crate::check::Severity::Warning => DiagnosticSeverity::WARNING,
                crate::check::Severity::Info => DiagnosticSeverity::INFORMATION,
                crate::check::Severity::Hint => DiagnosticSeverity::HINT,
            }),
            code: Some(lsp_types::NumberOrString::String(d.code.clone())),
            source: Some("ascript".to_string()),
            message: d.message.clone(),
            ..Diagnostic::default()
        })
        .collect()
}

/// Convert a byte offset to a char offset (the existing `LineIndex` is char-based).
fn byte_to_char(src: &str, byte: usize) -> usize {
    src[..byte.min(src.len())].chars().count()
}
```

> If `LineIndex::position` already accepts byte offsets, drop `byte_to_char` and pass `d.range.start` directly. The point is: the LSP now produces the **same** diagnostics as `ascript check`, mapped — no separate analysis path. Keep `analysis.rs`'s symbol/hover/completion functions as-is; only `diagnostics` changes.

- [ ] **Step 3: Parity test**

Add to `analysis.rs`'s `tests` mod:

```rust
    #[test]
    fn lsp_diagnostics_match_core_count() {
        // The LSP adapter reports exactly as many diagnostics as the core.
        let src = "let = 1\nlet = 2\n";
        let core = crate::check::analyze(src).diagnostics.len();
        let lsp = diagnostics(src).len();
        assert_eq!(core, lsp, "LSP must mirror the analysis core");
    }
```

- [ ] **Step 4: Run + full suite + clippy both configs**

Run: `cargo test --features lsp lsp 2>&1 | tail -15` (or `cargo test lsp` if `lsp` is in default features)
Expected: parity test + existing LSP tests PASS.
Run: `cargo test 2>&1 | tail -15`
Expected: full suite green.
Run: `cargo clippy --all-targets 2>&1 | tail -5 && cargo clippy --no-default-features --all-targets 2>&1 | tail -5`
Expected: clean both. **Critical:** `--no-default-features` must build — `src/check/` has no feature gate, proving the checker works without `lsp`/stdlib features.

- [ ] **Step 5: Commit**

```bash
git add src/lsp/analysis.rs
git commit -m "refactor(lsp): diagnostics adapt the shared analysis core (no separate path)"
```

---

## Done criteria for Plan C1

- [ ] `cargo test` green; `cargo clippy` clean in both feature configs; **`--no-default-features` builds the checker** (feature-independent core).
- [ ] `ascript check foo.as` reports **all** syntax errors (not just the first), source-pointing, with a CI-meaningful exit code (non-zero on Error; on Warning only under `--deny-warnings`).
- [ ] `--json` emits a valid diagnostics array.
- [ ] Inline `// ascript-ignore[code]` suppression works (line-scoped + file-wide).
- [ ] The LSP produces the **same** diagnostics as the CLI (one analysis path).
- [ ] The interpreter, the formatter, and the runtime are unchanged.

**Next plan:** `checker-scope-control-flow-lints.md` (sub-project #4) — wire the resolver into `analyze` and add the first real lints: `undefined-variable` (resolver `Global` + builtin/import allow-list), `unused-binding`/`unused-import` (with fixes), `shadowing`, `unreachable-code`, `missing-return`. Each as a rule module with table tests (positive + negative) and a corpus zero-false-positive guard.
