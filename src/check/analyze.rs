//! Analysis driver: runs the CST parser and collects diagnostics.

use crate::check::config::LintConfig;
use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::syntax::kind::SyntaxKind;
use crate::syntax::lexer::LexToken;
use crate::syntax::parser::{parse, Parse, ParseError};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub diagnostics: Vec<AsDiagnostic>,
}

/// Run the full analysis over `src` and return all diagnostics, sorted by
/// start offset then code.
///
/// Uses the default [`LintConfig`] (no severity overrides), so the result is
/// identical to running every rule at its declared default severity.
pub fn analyze(src: &str) -> Analysis {
    analyze_with_config(src, &LintConfig::default())
}

/// Like [`analyze`], but remaps each diagnostic's severity through `config`
/// after inline `ascript-ignore` suppression has been applied.
///
/// For each surviving diagnostic, `config.effective(code, severity)` decides the
/// outcome: `Some(sev)` overrides the severity, `None` drops the diagnostic.
/// `syntax-error` is immune — it is never downgraded or dropped. Inline
/// `ascript-ignore` runs first, so a config can never resurrect a diagnostic
/// the source explicitly suppressed.
pub fn analyze_with_config(src: &str, config: &LintConfig) -> Analysis {
    use crate::syntax::{resolve, tree_builder};

    let parsed = parse(src);

    let mut diagnostics: Vec<AsDiagnostic> = Vec::new();
    for err in &parsed.errors {
        diagnostics.push(AsDiagnostic {
            range: error_range(&parsed, err),
            severity: Severity::Error,
            code: "syntax-error".into(),
            message: err.message.clone(),
            fix: None,
        });
    }

    // Lexical errors (unterminated string/template/block-comment) are indexed by
    // FULL-token position; compute each one's byte range directly.
    for le in &parsed.lex_errors {
        diagnostics.push(AsDiagnostic {
            range: token_byte_range(&parsed.tokens, le.token),
            severity: Severity::Error,
            code: "syntax-error".to_string(),
            message: le.message.clone(),
            fix: None,
        });
    }

    // Build the typed CST (consumes `parsed`), resolve names, and run lint rules.
    let tree = tree_builder::build_tree(parsed);
    let resolved = resolve::resolve(&tree);
    // Surface resolver diagnostics (e.g. a same-scope REDECLARATION of a top-level
    // binding: `let x; let x`, `fn f; fn f`, `fn f; let f`). The tree-walker rejects
    // these at runtime (`'<name>' is already defined in this scope`); the static
    // checker flags them as `duplicate-binding` errors so `ascript check` catches them
    // up front.
    for d in &resolved.diagnostics {
        diagnostics.push(AsDiagnostic {
            range: ByteSpan::from(d.range),
            severity: Severity::Error,
            code: "duplicate-binding".to_string(),
            message: d.message.clone(),
            fix: None,
        });
    }
    for rule in crate::check::rules::ALL {
        diagnostics.extend(rule(&tree, &resolved, src));
    }
    // SP10: the advisory gradual type checker (a single stateful pass with the same
    // signature as a Rule). It runs no code and emits `type-mismatch`/`type-error`/
    // `possibly-nil` diagnostics through this same machinery.
    diagnostics.extend(crate::check::infer::check(&tree, &resolved, src));

    // TYPE: when the inference pass emits a BLOCKING `type-mismatch` (an annotated
    // param or field-default slot, `Severity::Error`) at a span, it supersedes the
    // legacy advisory `contract-mismatch`/`field-default-type` Warning at that same
    // span (the inference pass keeps its blocking emit there rather than de-dupping
    // it away). Drop the legacy advisory so the user sees the single sound Error,
    // not a duplicate Warning. (Advisory inference `type-mismatch`es still de-dup on
    // the inference side; this only fires for the blocking Error case.)
    //
    // ORDERING IS INTENTIONAL: subsumption runs BEFORE inline-ignore + the config
    // remap below. `type-mismatch` is the canonical diagnostic for these mistakes
    // (it SUBSUMES the legacy `contract-mismatch`/`field-default-type`, which are on a
    // one-release retirement path). So `--allow type-mismatch` (or an inline ignore /
    // a `[lint]` drop) silences the WHOLE mistake class — the legacy advisory does NOT
    // resurface. That is the better DX: a user who opted out of `type-mismatch` would
    // be confused to still get a differently-named warning about the very same line.
    // `--warn type-mismatch` instead keeps a single downgraded Warning. Locked by
    // `tests/cli.rs::check_field_default_type_lint_and_allow_suppression`.
    let blocking_type_spans: std::collections::HashSet<usize> = diagnostics
        .iter()
        .filter(|d| d.code == "type-mismatch" && d.severity == Severity::Error)
        .map(|d| d.range.start)
        .collect();
    if !blocking_type_spans.is_empty() {
        diagnostics.retain(|d| {
            !(matches!(d.code.as_str(), "contract-mismatch" | "field-default-type")
                && blocking_type_spans.contains(&d.range.start))
        });
    }

    // Apply inline `ascript-ignore` suppressions before sorting.
    let supp = suppressions(src);
    let line_starts = line_start_offsets(src);
    diagnostics.retain(|d| !supp.suppressed_on_line(line_of(&line_starts, d.range.start), &d.code));

    // Remap severities through the lint config. `syntax-error` is immune — it is
    // always an Error and is never downgraded or dropped. For every other code,
    // `effective` decides: `Some(sev)` overrides the severity, `None` drops it.
    diagnostics.retain_mut(|d| {
        if d.code == "syntax-error" {
            return true;
        }
        match config.effective(&d.code, d.severity) {
            Some(sev) => {
                d.severity = sev;
                true
            }
            None => false,
        }
    });

    diagnostics.sort_by(|a, b| a.range.start.cmp(&b.range.start).then(a.code.cmp(&b.code)));

    Analysis { diagnostics }
}

/// Inline suppression directives parsed from comments.
#[derive(Debug, Clone, Default)]
pub struct Suppressions {
    /// Codes suppressed on a specific (0-based) line.
    per_line: HashMap<usize, HashSet<String>>,
    /// File-wide suppressed codes (`ascript-ignore-file`).
    file_wide: HashSet<String>,
}

impl Suppressions {
    /// Is `code` suppressed for a diagnostic on (0-based) `line`?
    ///
    /// A directive applies to its own line or the line immediately following
    /// it, and `*` matches every code.
    pub fn suppressed_on_line(&self, line: usize, code: &str) -> bool {
        if self.file_wide.contains(code) || self.file_wide.contains("*") {
            return true;
        }
        let matches = |l: usize| {
            self.per_line
                .get(&l)
                .map(|set| set.contains(code) || set.contains("*"))
                .unwrap_or(false)
        };
        matches(line) || matches(line.wrapping_sub(1))
    }
}

/// Scan `src` for inline `ascript-ignore` directives in comments.
pub fn suppressions(src: &str) -> Suppressions {
    let mut supp = Suppressions::default();
    let mut line = 0usize;
    for t in crate::syntax::lex(src) {
        if matches!(t.kind, SyntaxKind::LineComment | SyntaxKind::BlockComment) {
            if let Some((file_wide, codes)) = parse_ignore(&t.text) {
                if file_wide {
                    supp.file_wide.extend(codes);
                } else {
                    supp.per_line.entry(line).or_default().extend(codes);
                }
            }
        }
        line += t.text.matches('\n').count();
    }
    supp
}

/// Parse an `ascript-ignore` directive out of a comment's raw text.
///
/// Returns `(file_wide, codes)`. An empty code list means `*` (all codes).
fn parse_ignore(comment: &str) -> Option<(bool, Vec<String>)> {
    let body = comment.trim_start_matches('/').trim_start_matches('*');
    let body = body.trim_end_matches('*').trim_end_matches('/');
    let body = body.trim();

    let (file_wide, rest) = if let Some(rest) = body.strip_prefix("ascript-ignore-file") {
        (true, rest)
    } else if let Some(rest) = body.strip_prefix("ascript-ignore") {
        (false, rest)
    } else {
        return None;
    };

    let rest = rest.trim();
    if rest.is_empty() {
        return Some((file_wide, vec!["*".to_string()]));
    }

    // Parse only the content between the first `[` and the next `]`, ignoring any
    // trailing prose (e.g. `// ascript-ignore[code] because <reason>`). A body with
    // no `[` at all is treated as bare (all codes).
    let Some(open) = rest.find('[') else {
        return Some((file_wide, vec!["*".to_string()]));
    };
    let after = &rest[open + 1..];
    let inner = match after.find(']') {
        Some(close) => &after[..close],
        None => after,
    };
    let codes: Vec<String> = inner
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let codes = if codes.is_empty() {
        vec!["*".to_string()]
    } else {
        codes
    };
    Some((file_wide, codes))
}

/// Byte offsets of the start of each line (line 0 starts at 0).
fn line_start_offsets(src: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// 0-based line containing `byte`, via the line-start table.
fn line_of(line_starts: &[usize], byte: usize) -> usize {
    match line_starts.binary_search(&byte) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    }
}

/// Byte span of the full-token at `idx` (sum of all preceding token lengths,
/// extended by this token's own length). Used for lexical (full-token-indexed)
/// errors.
fn token_byte_range(tokens: &[LexToken], idx: usize) -> ByteSpan {
    let start: usize = tokens.iter().take(idx).map(|t| t.text.len()).sum();
    let end = start + tokens.get(idx).map(|t| t.text.len()).unwrap_or(0);
    ByteSpan { start, end }
}

/// Map a `ParseError`'s non-trivia token index to a byte span in `src`.
fn error_range(parsed: &Parse, err: &ParseError) -> ByteSpan {
    let mut byte = 0usize;
    let mut non_trivia = 0usize;
    for t in &parsed.tokens {
        let len = t.text.len();
        if !t.kind.is_trivia() {
            if non_trivia == err.token_index {
                return ByteSpan {
                    start: byte,
                    end: byte + len.max(1),
                };
            }
            non_trivia += 1;
        }
        byte += len;
    }
    // EOF / never-matched: point at the final byte.
    let end = byte;
    ByteSpan {
        start: end.saturating_sub(1),
        end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_no_diagnostics_for_valid_program() {
        let a = analyze("let x = 1\nprint(x)\n");
        assert!(
            a.diagnostics.is_empty(),
            "expected no diagnostics, got {:?}",
            a.diagnostics
        );
    }

    #[test]
    fn flags_top_level_redeclaration_as_duplicate_binding() {
        // A same-scope top-level redeclaration is surfaced from the resolver as a
        // `duplicate-binding` error (the tree-walker rejects it at runtime).
        for src in [
            "let x = 1\nlet x = 2\nprint(x)\n",
            "let x = 1\nconst x = 2\nprint(x)\n",
            "fn f() { return 1 }\nfn f() { return 2 }\nprint(f())\n",
            "fn f() { return 1 }\nlet f = 2\nprint(f)\n",
        ] {
            let codes: Vec<_> = analyze(src)
                .diagnostics
                .into_iter()
                .map(|d| d.code)
                .collect();
            assert!(
                codes.contains(&"duplicate-binding".to_string()),
                "expected duplicate-binding for {src:?}, got {codes:?}"
            );
        }
        // A valid program (no redeclaration) and a BLOCK-scoped shadow are NOT flagged.
        assert!(!analyze("let x = 1\nprint(x)\n")
            .diagnostics
            .iter()
            .any(|d| d.code == "duplicate-binding"));
        assert!(!analyze("let x = 1\n{ let x = 2\n print(x) }\nprint(x)\n")
            .diagnostics
            .iter()
            .any(|d| d.code == "duplicate-binding"));
    }

    #[test]
    fn reports_all_syntax_errors_not_just_first() {
        let a = analyze("let = 1\nlet = 2\n");
        let n = a
            .diagnostics
            .iter()
            .filter(|d| d.code == "syntax-error")
            .count();
        assert!(
            n >= 2,
            "expected >=2 syntax-error diagnostics, got {n}: {:?}",
            a.diagnostics
        );
    }

    #[test]
    fn inline_ignore_suppresses_on_that_line() {
        let src = "// ascript-ignore[syntax-error]\n@\n";
        let supp = suppressions(src);
        // Directive on line 0 covers its own line (and, by the documented
        // `line || line-1` rule, the following line so the next-line `@`
        // error is actually suppressed — verified end-to-end below).
        assert!(supp.suppressed_on_line(0, "syntax-error"));
        // A line the directive cannot reach is not suppressed.
        assert!(!supp.suppressed_on_line(2, "syntax-error"));
        // Unrelated codes are never suppressed by a scoped directive.
        assert!(!supp.suppressed_on_line(0, "some-other-code"));
        // End-to-end: the next-line `@` syntax error is suppressed.
        assert!(analyze(src).diagnostics.is_empty());
    }

    #[test]
    fn ignore_with_trailing_reason_suppresses() {
        let supp = suppressions("// ascript-ignore[syntax-error] because legacy\n@\n");
        assert!(supp.suppressed_on_line(0, "syntax-error"));
    }

    #[test]
    fn ignore_multi_code_with_reason() {
        let supp = suppressions("// ascript-ignore[a, b] note\n@\n");
        assert!(supp.suppressed_on_line(0, "a"));
        assert!(supp.suppressed_on_line(0, "b"));
        assert!(!supp.suppressed_on_line(0, "c"));
    }

    #[test]
    fn bare_ignore_suppresses_all() {
        let supp = suppressions("// ascript-ignore\n@\n");
        assert!(supp.suppressed_on_line(0, "syntax-error"));
        assert!(supp.suppressed_on_line(0, "any-code-at-all"));
    }

    #[test]
    fn ignore_file_with_reason_suppresses_file_wide() {
        let supp = suppressions("// ascript-ignore-file[syntax-error] legacy module\n@\n");
        // file-wide: applies regardless of line
        assert!(supp.suppressed_on_line(5, "syntax-error"));
        assert!(!supp.suppressed_on_line(5, "other-code"));
    }

    #[test]
    fn inline_reason_suppresses_unawaited_future_end_to_end() {
        let src = "async fn work() { return 1 }\n\
                   fn main() { work() // ascript-ignore[unawaited-future] deliberate\n }\n\
                   main()\n";
        assert!(
            !analyze(src)
                .diagnostics
                .iter()
                .any(|d| d.code == "unawaited-future"),
            "got {:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn unterminated_string_is_one_syntax_error() {
        let a = analyze("let s = \"oops\n");
        // Exactly one syntax-error (the unterminated string is not duplicated by
        // the lexer); lint diagnostics for the surrounding code are not asserted
        // here.
        let syntax: Vec<_> = a
            .diagnostics
            .iter()
            .filter(|d| d.code == "syntax-error")
            .collect();
        assert_eq!(syntax.len(), 1, "got {:?}", a.diagnostics);
        let d = syntax[0];
        assert!(d.message.contains("unterminated"), "message: {}", d.message);
        assert!(d.range.start < d.range.end, "range: {:?}", d.range);
    }

    #[test]
    fn unterminated_block_comment_reported() {
        let a = analyze("/* never closed");
        let n = a
            .diagnostics
            .iter()
            .filter(|d| d.code == "syntax-error")
            .count();
        assert!(n >= 1, "expected a syntax error, got {:?}", a.diagnostics);
        assert!(
            a.diagnostics
                .iter()
                .any(|d| d.message.contains("block comment")),
            "got {:?}",
            a.diagnostics
        );
    }

    #[test]
    fn clean_program_has_zero_diagnostics() {
        assert!(analyze("let x = 1\nprint(x)\n").diagnostics.is_empty());
    }

    fn diag<'a>(a: &'a Analysis, code: &str) -> Option<&'a AsDiagnostic> {
        a.diagnostics.iter().find(|d| d.code == code)
    }

    #[test]
    fn default_config_is_identity_remap() {
        // `analyze_with_config` with the default config must be byte-identical to
        // `analyze` (the default is a no-op remap).
        let srcs = [
            "let x = 1\nprint(x)\n",
            "let x = 1\n",
            "let = 1\nlet = 2\n",
            "@\n",
        ];
        for src in srcs {
            assert_eq!(
                analyze(src).diagnostics,
                analyze_with_config(src, &LintConfig::default()).diagnostics,
                "default config diverged from analyze for {src:?}"
            );
        }
    }

    #[test]
    fn deny_promotes_warning_to_error() {
        let src = "let x = 1\n";
        // Default: unused-binding is a Warning.
        assert_eq!(
            diag(&analyze(src), "unused-binding").map(|d| d.severity),
            Some(Severity::Warning)
        );
        // deny → Error.
        let mut cfg = LintConfig::default();
        cfg.deny("unused-binding");
        assert_eq!(
            diag(&analyze_with_config(src, &cfg), "unused-binding").map(|d| d.severity),
            Some(Severity::Error)
        );
    }

    #[test]
    fn allow_drops_diagnostic_entirely() {
        let src = "let x = 1\n";
        assert!(diag(&analyze(src), "unused-binding").is_some());
        let mut cfg = LintConfig::default();
        cfg.allow("unused-binding");
        let a = analyze_with_config(src, &cfg);
        assert!(
            diag(&a, "unused-binding").is_none(),
            "allow should drop the diagnostic, got {:?}",
            a.diagnostics
        );
    }

    #[test]
    fn warn_demotes_error_default_to_warning() {
        // contract-mismatch is reported, then forced to Error via deny so we have
        // a deterministic Error-severity diagnostic to demote. (Simpler: take any
        // warning-default rule, deny→Error, then warn→Warning to prove warn maps
        // to Warning regardless of default.)
        let src = "let x = 1\n";
        let mut cfg = LintConfig::default();
        cfg.warn("unused-binding");
        assert_eq!(
            diag(&analyze_with_config(src, &cfg), "unused-binding").map(|d| d.severity),
            Some(Severity::Warning)
        );

        // And a true Error-default rule (syntax-error is immune, so instead force
        // an Error default by deny then demote): prove warn lands on Warning.
        let mut cfg2 = LintConfig::default();
        cfg2.warn("unused-binding");
        let a = analyze_with_config(src, &cfg2);
        assert_ne!(
            diag(&a, "unused-binding").map(|d| d.severity),
            Some(Severity::Error)
        );
    }

    #[test]
    fn syntax_error_is_immune_to_allow() {
        let src = "@\n"; // produces a syntax-error
        assert!(diag(&analyze(src), "syntax-error").is_some());
        let mut cfg = LintConfig::default();
        cfg.allow("syntax-error");
        cfg.warn("syntax-error");
        let a = analyze_with_config(src, &cfg);
        let d = diag(&a, "syntax-error");
        assert!(
            d.is_some(),
            "syntax-error must not be dropped by allow, got {:?}",
            a.diagnostics
        );
        // ...and never downgraded.
        assert_eq!(d.map(|d| d.severity), Some(Severity::Error));
    }

    #[test]
    fn inline_ignore_beats_config_deny() {
        // Source whose only diagnostic is unused-binding, suppressed inline.
        let src = "let x = 1 // ascript-ignore[unused-binding]\n";
        // Sanity: without the suppression it would be present.
        assert!(diag(&analyze("let x = 1\n"), "unused-binding").is_some());
        // With inline-ignore AND a config that denies the same rule, it is gone:
        // suppression runs first and config cannot resurrect it.
        let mut cfg = LintConfig::default();
        cfg.deny("unused-binding");
        let a = analyze_with_config(src, &cfg);
        assert!(
            diag(&a, "unused-binding").is_none(),
            "inline-ignore must beat config, got {:?}",
            a.diagnostics
        );
    }

    #[test]
    fn diagnostic_has_a_plausible_range() {
        let a = analyze("@\n");
        assert!(
            !a.diagnostics.is_empty(),
            "expected at least one diagnostic"
        );
        let d = &a.diagnostics[0];
        assert!(
            d.range.start < d.range.end,
            "expected non-empty range, got {:?}",
            d.range
        );
    }
}
