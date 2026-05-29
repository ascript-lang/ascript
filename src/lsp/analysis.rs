//! Pure static-analysis layer for the LSP.
//!
//! Every function here takes `&str` source and returns owned `lsp_types` data.
//! It reuses the interpreter's `lexer`/`parser` but NEVER runs the interpreter, so
//! it holds no `Rc`/`RefCell`/`Value` and is trivially `Send`. This keeps the
//! tower-lsp `LanguageServer` impl `Send + Sync`-clean.

use crate::ast::Stmt;
use crate::error::AsError;
use crate::lexer;
use crate::lsp::line_index::LineIndex;
use crate::parser;
use crate::span::Span;
use crate::token::Tok;
use tower_lsp::lsp_types::{
    Diagnostic, DiagnosticSeverity, DocumentSymbol, Hover, HoverContents, MarkupContent,
    MarkupKind, Position, Range, SymbolKind,
};

/// Lex + parse `text`, reporting the first lex-or-parse error as a single
/// `Diagnostic`. Valid programs produce an empty vec.
pub fn diagnostics(text: &str) -> Vec<Diagnostic> {
    let index = LineIndex::new(text);
    match lexer::lex(text) {
        Err(e) => vec![error_diagnostic(&e, &index)],
        Ok(tokens) => match parser::parse(&tokens) {
            Err(e) => vec![error_diagnostic(&e, &index)],
            Ok(_) => Vec::new(),
        },
    }
}

/// Build an Error-severity diagnostic from an `AsError`, using its span (via the
/// `LineIndex`) for the range, or the whole first line when no span is present.
fn error_diagnostic(error: &AsError, index: &LineIndex) -> Diagnostic {
    let range = match error.span {
        Some(span) => Range { start: index.position(span.start), end: index.position(span.end) },
        // No span: point at the start of the document (line 0).
        None => Range { start: Position::new(0, 0), end: Position::new(0, 0) },
    };
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("ascript".to_string()),
        message: error.message.clone(),
        ..Diagnostic::default()
    }
}

/// Convert a char-offset `Span` into an LSP `Range` via the `LineIndex`.
fn span_range(span: Span, index: &LineIndex) -> Range {
    Range { start: index.position(span.start), end: index.position(span.end) }
}

/// Build a `DocumentSymbol` literal. `#[allow(deprecated)]` is required because
/// `lsp_types::DocumentSymbol` still carries the deprecated `deprecated` field.
#[allow(deprecated)]
fn symbol(
    name: String,
    kind: SymbolKind,
    range: Range,
    selection_range: Range,
    children: Option<Vec<DocumentSymbol>>,
) -> DocumentSymbol {
    DocumentSymbol {
        name,
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children,
    }
}

/// Produce the document-symbol outline for `text`. Pure: lex + parse, and on any
/// lex-or-parse error return an empty vec (diagnostics report the error
/// separately). Walks only top-level declarations, unwrapping `export`.
pub fn document_symbols(text: &str) -> Vec<DocumentSymbol> {
    let Ok(tokens) = lexer::lex(text) else {
        return Vec::new();
    };
    let Ok(stmts) = parser::parse(&tokens) else {
        return Vec::new();
    };
    let index = LineIndex::new(text);
    let mut out = Vec::new();
    for stmt in &stmts {
        symbols_for_stmt(stmt, &index, &mut out);
    }
    out
}

/// Append the symbol(s) for one top-level statement (unwrapping `export`).
fn symbols_for_stmt(stmt: &Stmt, index: &LineIndex, out: &mut Vec<DocumentSymbol>) {
    match stmt {
        Stmt::Export(inner) => symbols_for_stmt(inner, index, out),
        Stmt::Fn { name, span, name_span, .. } => {
            out.push(symbol(
                name.clone(),
                SymbolKind::FUNCTION,
                span_range(*span, index),
                span_range(*name_span, index),
                None,
            ));
        }
        Stmt::Class { name, methods, span, name_span, .. } => {
            let children: Vec<DocumentSymbol> = methods
                .iter()
                .map(|m| {
                    symbol(
                        m.name.clone(),
                        SymbolKind::METHOD,
                        span_range(m.span, index),
                        span_range(m.name_span, index),
                        None,
                    )
                })
                .collect();
            out.push(symbol(
                name.clone(),
                SymbolKind::CLASS,
                span_range(*span, index),
                span_range(*name_span, index),
                Some(children),
            ));
        }
        Stmt::Enum { name, variants, span, name_span } => {
            let children: Vec<DocumentSymbol> = variants
                .iter()
                .map(|v| {
                    symbol(
                        v.name.clone(),
                        SymbolKind::ENUM_MEMBER,
                        span_range(v.name_span, index),
                        span_range(v.name_span, index),
                        None,
                    )
                })
                .collect();
            out.push(symbol(
                name.clone(),
                SymbolKind::ENUM,
                span_range(*span, index),
                span_range(*name_span, index),
                Some(children),
            ));
        }
        Stmt::Let { name, mutable, span, name_span, .. } => {
            let kind = if *mutable { SymbolKind::VARIABLE } else { SymbolKind::CONSTANT };
            out.push(symbol(
                name.clone(),
                kind,
                span_range(*span, index),
                span_range(*name_span, index),
                None,
            ));
        }
        Stmt::LetDestructure { names, mutable, span, name_spans, .. } => {
            let kind = if *mutable { SymbolKind::VARIABLE } else { SymbolKind::CONSTANT };
            for (name, nspan) in names.iter().zip(name_spans.iter()) {
                out.push(symbol(
                    name.clone(),
                    kind,
                    span_range(*span, index),
                    span_range(*nspan, index),
                    None,
                ));
            }
        }
        _ => {}
    }
}

/// Hover at the given char `offset`: locate the token spanning the offset and
/// describe it (keyword / builtin / known top-level declaration), or `None`.
pub fn hover(text: &str, offset: usize) -> Option<Hover> {
    let tokens = lexer::lex(text).ok()?;
    // Find the token whose half-open span [start, end) contains the offset. An
    // offset exactly at the end of one token / start of the next prefers the
    // token that starts there (so hovering the boundary of an identifier works).
    let token = tokens
        .iter()
        .find(|t| t.tok != Tok::Eof && offset >= t.span.start && offset < t.span.end)?;

    let index = LineIndex::new(text);
    let range = span_range(token.span, &index);

    let doc = match &token.tok {
        Tok::Ident(name) => ident_doc(name, text),
        other => keyword_doc(other).map(str::to_string),
    }?;

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: doc,
        }),
        range: Some(range),
    })
}

/// Documentation for a keyword token, if it is one.
fn keyword_doc(tok: &Tok) -> Option<&'static str> {
    let s = match tok {
        Tok::Let => "`let` — declare a mutable variable binding.",
        Tok::Const => "`const` — declare an immutable (constant) binding.",
        Tok::Fn => "`fn` — declare a function.",
        Tok::Return => "`return` — return a value from the enclosing function.",
        Tok::If => "`if` — conditional execution.",
        Tok::Else => "`else` — the alternative branch of an `if`.",
        Tok::While => "`while` — loop while a condition holds.",
        Tok::For => "`for` — iterate with `for (x of xs)` or `for (i in a..b)`.",
        Tok::Of => "`of` — iterate over the elements of a collection in a `for` loop.",
        Tok::In => "`in` — iterate over a range in a `for` loop.",
        Tok::Match => "`match` — pattern-match a value against arms.",
        Tok::Async => "`async` — declare an asynchronous function returning a future.",
        Tok::Await => "`await` — suspend until an async value resolves.",
        Tok::Class => "`class` — declare a class with methods.",
        Tok::Enum => "`enum` — declare an enumeration of named variants.",
        Tok::Import => "`import` — import names from another module.",
        Tok::Export => "`export` — make a declaration available to importers.",
        Tok::Nil => "`nil` — the absence of a value.",
        Tok::True => "`true` — the boolean true literal.",
        Tok::False => "`false` — the boolean false literal.",
        Tok::Break => "`break` — exit the enclosing loop.",
        Tok::Continue => "`continue` — skip to the next loop iteration.",
        _ => return None,
    };
    Some(s)
}

/// Documentation for an identifier: a global builtin (static table), else a
/// top-level declaration in `text`, else `None`.
fn ident_doc(name: &str, text: &str) -> Option<String> {
    if let Some(b) = builtin_doc(name) {
        return Some(b.to_string());
    }
    decl_doc(name, text)
}

/// Signature + one-line doc for a global builtin function, if `name` is one.
fn builtin_doc(name: &str) -> Option<&'static str> {
    let s = match name {
        "print" => "```\nprint(...values)\n```\nWrite the values to standard output, separated by spaces, followed by a newline.",
        "len" => "```\nlen(value): number\n```\nThe length of a string, array, or object.",
        "type" => "```\ntype(value): string\n```\nThe runtime type name of a value.",
        "assert" => "```\nassert(cond, message?)\n```\nPanic with `message` if `cond` is falsy.",
        "range" => "```\nrange(start, end): array<number>\n```\nThe integers from `start` (inclusive) to `end` (exclusive).",
        "Ok" => "```\nOk(value): Result\n```\nWrap a value as a successful `Result`.",
        "Err" => "```\nErr(error): Result\n```\nWrap an error as a failed `Result`.",
        "recover" => "```\nrecover(fn): Result\n```\nRun `fn`, capturing any panic as an `Err` instead of unwinding.",
        _ => return None,
    };
    Some(s)
}

/// If `name` is a top-level declaration in `text`, describe its kind (e.g.
/// "fn `foo`"). Reuses the symbol walk so the table stays in one place.
fn decl_doc(name: &str, text: &str) -> Option<String> {
    let syms = document_symbols(text);
    let sym = syms.iter().find(|s| s.name == name)?;
    let kind = match sym.kind {
        SymbolKind::FUNCTION => "fn",
        SymbolKind::CLASS => "class",
        SymbolKind::ENUM => "enum",
        SymbolKind::CONSTANT => "const",
        SymbolKind::VARIABLE => "let",
        _ => "symbol",
    };
    Some(format!("```\n{kind} {name}\n```"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_program_has_no_diagnostics() {
        let diags = diagnostics("let x = 1\nprint(x)");
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
    }

    #[test]
    fn unterminated_string_is_one_error() {
        let diags = diagnostics("let s = \"oops");
        assert_eq!(diags.len(), 1);
        let d = &diags[0];
        assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(d.source.as_deref(), Some("ascript"));
        assert!(
            d.message.to_lowercase().contains("string"),
            "message should mention string, got: {}",
            d.message
        );
        // A plausible range: start no later than end, within the single line.
        assert_eq!(d.range.start.line, 0);
        assert!(d.range.start.character <= d.range.end.character || d.range.end.line > 0);
    }

    #[test]
    fn parse_error_is_one_error() {
        let diags = diagnostics("let = 5");
        assert_eq!(diags.len(), 1);
        let d = &diags[0];
        assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(d.source.as_deref(), Some("ascript"));
        assert!(!d.message.is_empty());
    }

    #[allow(deprecated)]
    fn find<'a>(syms: &'a [DocumentSymbol], name: &str) -> &'a DocumentSymbol {
        syms.iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no symbol named {name:?} in {syms:#?}"))
    }

    #[test]
    fn document_symbols_lists_decls_with_kinds_and_nesting() {
        let src = "\
fn foo() {}
class C {
  fn init() {}
  fn m() {}
}
enum E { A, B }
const K = 1
let v = 2
export fn bar() {}
";
        let syms = document_symbols(src);

        // Top-level names present.
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"foo"), "names: {names:?}");
        assert!(names.contains(&"C"), "names: {names:?}");
        assert!(names.contains(&"E"), "names: {names:?}");
        assert!(names.contains(&"K"), "names: {names:?}");
        assert!(names.contains(&"v"), "names: {names:?}");
        // The exported decl still appears.
        assert!(names.contains(&"bar"), "exported bar should appear: {names:?}");

        // Kinds.
        assert_eq!(find(&syms, "foo").kind, SymbolKind::FUNCTION);
        assert_eq!(find(&syms, "bar").kind, SymbolKind::FUNCTION);
        assert_eq!(find(&syms, "C").kind, SymbolKind::CLASS);
        assert_eq!(find(&syms, "E").kind, SymbolKind::ENUM);
        assert_eq!(find(&syms, "K").kind, SymbolKind::CONSTANT);
        assert_eq!(find(&syms, "v").kind, SymbolKind::VARIABLE);

        // Class has method children.
        let c = find(&syms, "C");
        let methods = c.children.as_ref().expect("class C should have children");
        let mnames: Vec<&str> = methods.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(mnames, vec!["init", "m"]);
        assert!(methods.iter().all(|m| m.kind == SymbolKind::METHOD));

        // Enum has variant children.
        let e = find(&syms, "E");
        let variants = e.children.as_ref().expect("enum E should have children");
        let vnames: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(vnames, vec!["A", "B"]);
        assert!(variants.iter().all(|v| v.kind == SymbolKind::ENUM_MEMBER));

        // The selection range of `foo` should sit on its name (line 0).
        let foo = find(&syms, "foo");
        assert_eq!(foo.selection_range.start.line, 0);
        assert_eq!(foo.selection_range.start.character, 3); // after "fn "
    }

    #[test]
    fn document_symbols_destructuring_lists_each_name() {
        let syms = document_symbols("let [a, b] = pair");
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"a"), "names: {names:?}");
        assert!(names.contains(&"b"), "names: {names:?}");
        assert!(syms.iter().all(|s| s.kind == SymbolKind::VARIABLE));
    }

    #[test]
    fn document_symbols_on_parse_error_is_empty() {
        assert!(document_symbols("let = 5").is_empty());
    }

    fn markup(h: &Hover) -> &str {
        match &h.contents {
            HoverContents::Markup(m) => &m.value,
            other => panic!("expected markup, got {other:?}"),
        }
    }

    #[test]
    fn hover_on_builtin_print_mentions_print() {
        let src = "print(1)";
        let off = src.find("print").unwrap();
        let h = hover(src, off).expect("expected hover on print");
        assert!(markup(&h).contains("print"), "got: {}", markup(&h));
        assert!(h.range.is_some());
    }

    #[test]
    fn hover_on_keyword_fn_describes_it() {
        let src = "fn foo() {}";
        let off = 0; // on `fn`
        let h = hover(src, off).expect("expected hover on fn keyword");
        let m = markup(&h).to_lowercase();
        assert!(m.contains("function"), "got: {}", markup(&h));
    }

    #[test]
    fn hover_on_user_fn_use_says_fn_name() {
        // Hover on the call site `foo` should resolve to the top-level fn decl.
        let src = "fn foo() {}\nfoo()";
        let off = src.find("foo()").unwrap(); // the use, not the decl
        let h = hover(src, off).expect("expected hover on foo use");
        let m = markup(&h);
        assert!(m.contains("foo"), "got: {m}");
        assert!(m.contains("fn"), "expected fn kind, got: {m}");
    }

    #[test]
    fn hover_on_whitespace_is_none() {
        let src = "let x = 1";
        // Offset 3 is the space between `let` and `x`.
        assert!(hover(src, 3).is_none());
    }

    #[test]
    fn hover_on_unknown_ident_is_none() {
        let src = "zzz";
        assert!(hover(src, 0).is_none());
    }
}
