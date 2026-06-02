//! Pure static-analysis layer for the LSP.
//!
//! Every function here takes `&str` source and returns owned `lsp_types` data.
//! It reuses the interpreter's `lexer`/`parser` but NEVER runs the interpreter, so
//! it holds no `Rc`/`RefCell`/`Value` and is trivially `Send`. This keeps the
//! tower-lsp `LanguageServer` impl `Send + Sync`-clean.

use crate::ast::{Expr, ExprKind, MatchArm, Param, Pattern, Stmt};
use crate::lexer;
use crate::lsp::line_index::LineIndex;
use crate::parser;
use crate::span::Span;
use crate::token::Tok;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, Diagnostic, DiagnosticSeverity, DocumentSymbol, Hover,
    HoverContents, MarkupContent, MarkupKind, NumberOrString, Range, SymbolKind,
};

/// The AScript keywords offered as completions (KEYWORD kind). Mirrors the lexer's
/// keyword table (`src/lexer.rs`) plus `match` (which the lexer maps to `Tok::Match`).
const KEYWORDS: &[&str] = &[
    "let", "const", "fn", "return", "if", "else", "while", "for", "of", "in", "match", "async",
    "await", "yield", "class", "enum", "import", "export", "nil", "true", "false", "break",
    "continue",
];

/// The global builtins offered as completions (FUNCTION kind). Mirrors `builtin_doc`.
const BUILTINS: &[&str] = &[
    "print", "len", "type", "assert", "range", "Ok", "Err", "recover", "test", "exit",
];

/// The known stdlib module paths offered when completing an `import ... from "..."`
/// string. Hardcoded (rather than derived from `std_module_exports`) so the list is
/// stable regardless of which cargo features are enabled at build time — editors
/// should see every documented module path. Kept in sync with `std_module_exports`
/// in `src/stdlib/mod.rs`.
const STD_MODULE_PATHS: &[&str] = &[
    "std/string",
    "std/array",
    "std/object",
    "std/map",
    "std/math",
    "std/convert",
    "std/json",
    "std/regex",
    "std/encoding",
    "std/bytes",
    "std/uuid",
    "std/csv",
    "std/toml",
    "std/yaml",
    "std/time",
    "std/date",
    "std/intl",
    "std/env",
    "std/fs",
    "std/process",
    "std/crypto",
    "std/compress",
    "std/sqlite",
    "std/net/tcp",
    "std/net/http",
    "std/http/server",
    "std/net/ws",
    "std/tui",
];

/// Produce LSP diagnostics by adapting the shared analysis core
/// (`crate::check::analyze`), so the editor and `ascript check` report the
/// identical diagnostics from ONE analysis path. Core ranges are BYTE offsets;
/// the `LineIndex` is char-based, so we convert byte→char per endpoint.
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
            code: Some(NumberOrString::String(d.code.clone())),
            source: Some("ascript".to_string()),
            message: d.message.clone(),
            ..Diagnostic::default()
        })
        .collect()
}

/// Convert a byte offset to a char offset (the existing `LineIndex` is
/// char-based). Robust against out-of-range and mid-codepoint inputs: clamps to
/// the largest char boundary `<= byte` so the `&str` slice never panics on
/// multi-byte UTF-8.
fn byte_to_char(src: &str, byte: usize) -> usize {
    let mut b = byte.min(src.len());
    while b > 0 && !src.is_char_boundary(b) {
        b -= 1;
    }
    src[..b].chars().count()
}

/// Convert a char-offset `Span` into an LSP `Range` via the `LineIndex`.
fn span_range(span: Span, index: &LineIndex) -> Range {
    Range {
        start: index.position(span.start),
        end: index.position(span.end),
    }
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
        Stmt::Fn {
            name,
            span,
            name_span,
            ..
        } => {
            out.push(symbol(
                name.clone(),
                SymbolKind::FUNCTION,
                span_range(*span, index),
                span_range(*name_span, index),
                None,
            ));
        }
        Stmt::Class {
            name,
            fields,
            methods,
            span,
            name_span,
            ..
        } => {
            let mut children: Vec<DocumentSymbol> = fields
                .iter()
                .map(|fd| {
                    symbol(
                        fd.name.clone(),
                        SymbolKind::PROPERTY,
                        span_range(fd.span, index),
                        span_range(fd.name_span, index),
                        None,
                    )
                })
                .collect();
            children.extend(methods.iter().map(|m| {
                symbol(
                    m.name.clone(),
                    SymbolKind::METHOD,
                    span_range(m.span, index),
                    span_range(m.name_span, index),
                    None,
                )
            }));
            out.push(symbol(
                name.clone(),
                SymbolKind::CLASS,
                span_range(*span, index),
                span_range(*name_span, index),
                Some(children),
            ));
        }
        Stmt::Enum {
            name,
            variants,
            span,
            name_span,
        } => {
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
        Stmt::Let {
            name,
            mutable,
            span,
            name_span,
            ..
        } => {
            let kind = if *mutable {
                SymbolKind::VARIABLE
            } else {
                SymbolKind::CONSTANT
            };
            out.push(symbol(
                name.clone(),
                kind,
                span_range(*span, index),
                span_range(*name_span, index),
                None,
            ));
        }
        Stmt::LetDestructure {
            names,
            rest,
            mutable,
            span,
            name_spans,
            ..
        } => {
            let kind = if *mutable {
                SymbolKind::VARIABLE
            } else {
                SymbolKind::CONSTANT
            };
            for (name, nspan) in names.iter().zip(name_spans.iter()) {
                out.push(symbol(
                    name.clone(),
                    kind,
                    span_range(*span, index),
                    span_range(*nspan, index),
                    None,
                ));
            }
            if let Some((name, nspan)) = rest {
                out.push(symbol(
                    name.clone(),
                    kind,
                    span_range(*span, index),
                    span_range(*nspan, index),
                    None,
                ));
            }
        }
        Stmt::LetDestructureObject {
            bindings,
            rest,
            mutable,
            span,
            ..
        } => {
            let kind = if *mutable {
                SymbolKind::VARIABLE
            } else {
                SymbolKind::CONSTANT
            };
            for b in bindings {
                out.push(symbol(
                    b.binding.clone(),
                    kind,
                    span_range(*span, index),
                    span_range(b.binding_span, index),
                    None,
                ));
            }
            if let Some((name, nspan)) = rest {
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
        Tok::Yield => {
            "`yield` — produce a value from a generator (`fn*`); evaluates to the resume value."
        }
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
        "test" => "```\ntest(name, fn)\n```\nRegister a test for `ascript test`.",
        "exit" => "```\nexit(code?: number)\n```\nTerminate the program with the given exit code (0–255, default 0). Unwinds cleanly — does not skip destructors.",
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

/// A baseline completion item (keyword or builtin).
fn item(label: &str, kind: CompletionItemKind) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(kind),
        ..CompletionItem::default()
    }
}

/// The always-offered baseline completions: every keyword + every global builtin.
fn baseline_completions() -> Vec<CompletionItem> {
    let mut out = Vec::with_capacity(KEYWORDS.len() + BUILTINS.len());
    for kw in KEYWORDS {
        out.push(item(kw, CompletionItemKind::KEYWORD));
    }
    for b in BUILTINS {
        out.push(item(b, CompletionItemKind::FUNCTION));
    }
    out
}

/// Completions at char `offset` in `text`. Pure and robust: never panics, and
/// always returns at least the baseline (keywords + builtins) even on partial or
/// syntactically broken input (completion is requested mid-edit).
///
/// Context detection is done by simple, parser-free scanning of the raw text around
/// the cursor, so it works on documents that do not yet parse:
/// - inside an `import ... from "..."` / `'...'` string → stdlib module paths;
/// - right after `<ident>.` where `<ident>` is a `import * as <ident>` namespace of a
///   known std module → that module's exports.
pub fn completions(text: &str, offset: usize) -> Vec<CompletionItem> {
    let chars: Vec<char> = text.chars().collect();
    let offset = offset.min(chars.len());

    // Context 1: inside an import-from string literal → offer module paths.
    if in_import_path_string(&chars, offset) {
        return STD_MODULE_PATHS
            .iter()
            .map(|p| item(p, CompletionItemKind::MODULE))
            .collect();
    }

    // Context 2: member access `<ident>.` where ident is a namespace import.
    if let Some(alias) = member_access_alias(&chars, offset) {
        if let Some(module) = namespace_import_module(text, &alias) {
            if let Some(exports) = crate::stdlib::std_module_exports(&module) {
                if !exports.is_empty() {
                    return exports
                        .into_iter()
                        .map(|(name, _)| item(&name, CompletionItemKind::FUNCTION))
                        .collect();
                }
            }
        }
    }

    baseline_completions()
}

/// Whether `offset` sits inside the still-open string of a `from "..."` / `from '...'`
/// on the current line. Scans backward from the cursor within the current line for an
/// opening quote with no closing quote before the cursor, then checks the text before
/// that quote ends with `from`.
fn in_import_path_string(chars: &[char], offset: usize) -> bool {
    // Restrict to the current line (imports are single-line).
    let line_start = chars[..offset]
        .iter()
        .rposition(|&c| c == '\n')
        .map_or(0, |p| p + 1);
    let line = &chars[line_start..offset];

    // The cursor is inside a string iff the most recent quote on the line has no
    // matching close before the cursor — i.e. it's the last quote on the line.
    let Some(rel_quote) = line.iter().rposition(|&c| c == '"' || c == '\'') else {
        return false;
    };
    // Check the text before that opening quote ends with `from` (allowing whitespace).
    let before: String = line[..rel_quote].iter().collect();
    before.trim_end().ends_with("from")
}

/// If the text immediately before `offset` is `<ident>.`, return `<ident>`.
fn member_access_alias(chars: &[char], offset: usize) -> Option<String> {
    if offset == 0 {
        return None;
    }
    // The char right before the cursor must be a dot.
    if chars[offset - 1] != '.' {
        return None;
    }
    // Collect the identifier ending just before the dot.
    let dot = offset - 1;
    let mut start = dot;
    while start > 0 && is_ident_char(chars[start - 1]) {
        start -= 1;
    }
    if start == dot {
        return None; // no ident before the dot
    }
    // The first char must be a valid identifier start (not a digit).
    if chars[start].is_ascii_digit() {
        return None;
    }
    Some(chars[start..dot].iter().collect())
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Scan `text` for `import * as <alias> from "std/<mod>"` and return the module path.
/// Parser-free (works on broken docs): a regex-like manual scan per line.
fn namespace_import_module(text: &str, alias: &str) -> Option<String> {
    for line in text.lines() {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix("import") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('*') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix("as") else {
            continue;
        };
        let rest = rest.trim_start();
        // The alias is the next identifier.
        let name: String = rest.chars().take_while(|&c| is_ident_char(c)).collect();
        if name != alias {
            continue;
        }
        let rest = &rest[name.len()..];
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix("from") else {
            continue;
        };
        let rest = rest.trim_start();
        // Extract the quoted module path.
        let mut chars = rest.chars();
        let q = chars.next()?;
        if q != '"' && q != '\'' {
            continue;
        }
        let path: String = chars.take_while(|&c| c != q).collect();
        return Some(path);
    }
    None
}

/// Go-to-definition at char `offset` in `text`. Pure. Identifies the identifier token
/// at the offset and resolves it to a declaration's name span, returned as a `Range`.
///
/// Resolution order (nearest scope first):
/// 1. If the cursor is inside a function/method body, that function's PARAMS, then its
///    local `let`/`const` declarations that appear before the cursor.
/// 2. Otherwise (or if not found in the enclosing fn) the top-level declarations
///    (`fn`/`class`/`enum`/`let`/`const`, including destructured lets and
///    `export`-wrapped decls).
///
/// Within-file only (cross-file deferred). If the cursor is on a declaration's own
/// name, that decl's range is returned (self-location). Unknown identifiers, or any
/// lex/parse failure, yield `None`.
pub fn definition(text: &str, offset: usize) -> Option<Range> {
    let tokens = lexer::lex(text).ok()?;
    // Identify the identifier token under the cursor (same boundary rule as hover).
    let token = tokens
        .iter()
        .find(|t| t.tok != Tok::Eof && offset >= t.span.start && offset < t.span.end)?;
    let Tok::Ident(name) = &token.tok else {
        return None;
    };

    let stmts = parser::parse(&tokens).ok()?;
    let index = LineIndex::new(text);

    // 1. Try the nearest enclosing function's params + local lets (before the cursor).
    if let Some(span) = enclosing_fn_local(&stmts, name, offset) {
        return Some(span_range(span, &index));
    }

    // 2. Fall back to the top-level declarations.
    let mut name_span = None;
    for stmt in &stmts {
        collect_decl_name_span(stmt, name, &mut name_span);
    }
    name_span.map(|s| span_range(s, &index))
}

/// The body of a function (top-level `fn` or a class method): its params and the
/// statements between its name and its closing brace.
struct FnBody<'a> {
    params: &'a [Param],
    body: &'a [Stmt],
    /// Span covering the whole declaration (used to test containment).
    span: Span,
}

/// Walk all top-level fns + class methods and collect their bodies.
fn collect_fn_bodies<'a>(stmts: &'a [Stmt], out: &mut Vec<FnBody<'a>>) {
    for stmt in stmts {
        match stmt {
            Stmt::Export(inner) => collect_fn_bodies(std::slice::from_ref(inner), out),
            Stmt::Fn {
                params, body, span, ..
            } => {
                out.push(FnBody {
                    params,
                    body,
                    span: *span,
                });
            }
            Stmt::Class { methods, .. } => {
                for m in methods {
                    out.push(FnBody {
                        params: &m.params,
                        body: &m.body,
                        span: m.span,
                    });
                }
            }
            _ => {}
        }
    }
}

/// If the cursor at `offset` is inside a function/method whose params or local
/// `let`/`const` (declared before the offset) bind `name`, return that binding's name
/// span. Picks the NEAREST enclosing fn (smallest containing span) so nested fns win.
fn enclosing_fn_local(stmts: &[Stmt], name: &str, offset: usize) -> Option<Span> {
    let mut bodies = Vec::new();
    collect_fn_bodies(stmts, &mut bodies);

    // Candidate fns whose declaration span contains the cursor, nearest (smallest) first.
    let mut candidates: Vec<&FnBody> = bodies
        .iter()
        .filter(|b| offset >= b.span.start && offset < b.span.end)
        .collect();
    candidates.sort_by_key(|b| b.span.end - b.span.start);

    for fnbody in candidates {
        // Params first.
        for p in fnbody.params {
            if p.name == name {
                return Some(p.name_span);
            }
        }
        // Then local lets/consts declared before the cursor.
        if let Some(span) = local_let_before(fnbody.body, name, offset) {
            return Some(span);
        }
        // Then match-arm pattern bindings (the match expr that covers the cursor
        // and whose arm pattern binds `name`).
        if let Some(span) = match_arm_binding_in_body(fnbody.body, name, offset) {
            return Some(span);
        }
    }
    None
}

/// Find a `let`/`const` (or destructured name) declared in `body` that binds `name`
/// and whose name span starts at or before `offset`. Last such binding wins (closest
/// preceding declaration). Direct statements of the body only (pragmatic — no descent
/// into nested blocks/loops).
fn local_let_before(body: &[Stmt], name: &str, offset: usize) -> Option<Span> {
    let mut found = None;
    for stmt in body {
        match stmt {
            Stmt::Let {
                name: n, name_span, ..
            } if n == name && name_span.start <= offset => {
                found = Some(*name_span);
            }
            Stmt::LetDestructure {
                names,
                rest,
                name_spans,
                ..
            } => {
                for (n, s) in names.iter().zip(name_spans.iter()) {
                    if n == name && s.start <= offset {
                        found = Some(*s);
                    }
                }
                if let Some((n, s)) = rest {
                    if n == name && s.start <= offset {
                        found = Some(*s);
                    }
                }
            }
            Stmt::LetDestructureObject { bindings, rest, .. } => {
                for b in bindings {
                    if b.binding == name && b.binding_span.start <= offset {
                        found = Some(b.binding_span);
                    }
                }
                if let Some((n, s)) = rest {
                    if n == name && s.start <= offset {
                        found = Some(*s);
                    }
                }
            }
            _ => {}
        }
    }
    found
}

/// Collect all identifier names introduced by a match-arm `pat` into `out`.
///
/// Conservative (Option-C LSP rule): every bare `Ident(n)`, every array-element
/// binding, every object-shorthand key, every `key: sub-pattern` sub-binding, and
/// every named rest (`...name`) is added. This means a bare ident that at runtime
/// would be a *compare* (because the name is already defined) is also added — but
/// since the name was already defined, this is harmless: the go-to-def resolution
/// still returns the correct declaration for it. The benefit is that genuine
/// *bind* idents are never wrongly flagged as undefined.
fn collect_pattern_bindings(pat: &Pattern, out: &mut Vec<std::rc::Rc<str>>) {
    match pat {
        Pattern::Wildcard | Pattern::Value(_) | Pattern::Range { .. } => {}
        Pattern::Ident(n) => out.push(n.clone()),
        Pattern::Array(pats, rest) => {
            for p in pats {
                collect_pattern_bindings(p, out);
            }
            if let Some(Some(name)) = rest {
                out.push(name.clone());
            }
        }
        Pattern::Object(entries, rest) => {
            for e in entries {
                match &e.pat {
                    // Shorthand `{key}` always binds `key`.
                    None => out.push(e.key.clone()),
                    // `{key: sub}` — recurse into the sub-pattern.
                    Some(sub) => collect_pattern_bindings(sub, out),
                }
            }
            if let Some(Some(name)) = rest {
                out.push(name.clone());
            }
        }
    }
}

/// Walk all `ExprKind::Match` expressions reachable from `expr` and, for any arm
/// whose guard or body span contains `offset`, collect that arm's pattern bindings
/// for `name` and return the match expression's own span as a stand-in "definition
/// site" (there is no individual name-span for a pattern binding, so the pattern
/// token's location is approximated by the match span — still useful for
/// go-to-definition, which resolves to the containing `match`).
///
/// Because match is an expression it can appear anywhere, including deeply nested
/// inside calls, binary ops, etc. We do a full recursive walk of `expr`.
fn match_arm_binding_in_expr(expr: &Expr, name: &str, offset: usize) -> Option<Span> {
    // Recurse into sub-expressions first (the deepest / most-specific match wins).
    let child_result = match &expr.kind {
        ExprKind::Match { subject, arms } => {
            // Try subject first (in case there's a nested match there).
            if let r @ Some(_) = match_arm_binding_in_expr(subject, name, offset) {
                return r;
            }
            // Walk arms.
            for arm in arms {
                // Check guard.
                if let Some(g) = &arm.guard {
                    if let r @ Some(_) = match_arm_binding_in_expr(g, name, offset) {
                        return r;
                    }
                }
                // Check body.
                if let r @ Some(_) = match_arm_binding_in_expr(&arm.body, name, offset) {
                    return r;
                }
            }
            // Now check: is `offset` inside any arm's guard/body, and does that
            // arm's pattern bind `name`?
            arm_binding_for_offset(arms, name, offset, expr.span)
        }
        // Recurse into all expression forms that contain sub-expressions.
        ExprKind::Binary { lhs, rhs, .. } => match_arm_binding_in_expr(lhs, name, offset)
            .or_else(|| match_arm_binding_in_expr(rhs, name, offset)),
        ExprKind::Unary { expr: inner, .. }
        | ExprKind::Await(inner)
        | ExprKind::Try(inner)
        | ExprKind::Unwrap(inner)
        | ExprKind::Paren(inner)
        | ExprKind::Yield(Some(inner)) => match_arm_binding_in_expr(inner, name, offset),
        ExprKind::Assign { target, value } => match_arm_binding_in_expr(target, name, offset)
            .or_else(|| match_arm_binding_in_expr(value, name, offset)),
        ExprKind::Ternary { cond, then, els } => match_arm_binding_in_expr(cond, name, offset)
            .or_else(|| match_arm_binding_in_expr(then, name, offset))
            .or_else(|| match_arm_binding_in_expr(els, name, offset)),
        ExprKind::Call { callee, args } => {
            use crate::ast::CallArg;
            let r = match_arm_binding_in_expr(callee, name, offset);
            if r.is_some() {
                return r;
            }
            for a in args {
                let e = match a {
                    CallArg::Pos(e) | CallArg::Spread(e) => e,
                };
                if let r @ Some(_) = match_arm_binding_in_expr(e, name, offset) {
                    return r;
                }
            }
            None
        }
        ExprKind::Index { object, index } => match_arm_binding_in_expr(object, name, offset)
            .or_else(|| match_arm_binding_in_expr(index, name, offset)),
        ExprKind::Member { object, .. } | ExprKind::OptMember { object, .. } => {
            match_arm_binding_in_expr(object, name, offset)
        }
        ExprKind::Array(items) => {
            use crate::ast::ArrayElem;
            for it in items {
                let e = match it {
                    ArrayElem::Item(e) | ArrayElem::Spread(e) => e,
                };
                if let r @ Some(_) = match_arm_binding_in_expr(e, name, offset) {
                    return r;
                }
            }
            None
        }
        ExprKind::Object(entries) => {
            use crate::ast::ObjEntry;
            for en in entries {
                let e = match en {
                    ObjEntry::KV(_, e) | ObjEntry::Spread(e) => e,
                };
                if let r @ Some(_) = match_arm_binding_in_expr(e, name, offset) {
                    return r;
                }
            }
            None
        }
        ExprKind::Template { parts } => {
            use crate::ast::TemplatePart;
            for p in parts {
                if let TemplatePart::Expr(e) = p {
                    if let r @ Some(_) = match_arm_binding_in_expr(e, name, offset) {
                        return r;
                    }
                }
            }
            None
        }
        ExprKind::Arrow { body, .. } => {
            use crate::ast::ArrowBody;
            match body.as_ref() {
                ArrowBody::Expr(e) => match_arm_binding_in_expr(e, name, offset),
                ArrowBody::Block(stmts) => {
                    for s in stmts {
                        if let r @ Some(_) = match_arm_binding_in_stmt(s, name, offset) {
                            return r;
                        }
                    }
                    None
                }
            }
        }
        // Leaf nodes (no sub-expressions).
        ExprKind::Number(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::Nil
        | ExprKind::Ident(_)
        | ExprKind::Yield(None) => None,
    };
    child_result
}

/// Walk a statement for match-arm bindings (used when descending into arrow blocks).
fn match_arm_binding_in_stmt(stmt: &Stmt, name: &str, offset: usize) -> Option<Span> {
    match stmt {
        Stmt::Expr(e) | Stmt::Return(Some(e)) => match_arm_binding_in_expr(e, name, offset),
        Stmt::Let { value: Some(e), .. } => match_arm_binding_in_expr(e, name, offset),
        Stmt::LetDestructure { value: e, .. } | Stmt::LetDestructureObject { value: e, .. } => {
            match_arm_binding_in_expr(e, name, offset)
        }
        Stmt::If {
            cond,
            then_branch,
            else_branch,
        } => {
            if let r @ Some(_) = match_arm_binding_in_expr(cond, name, offset) {
                return r;
            }
            for s in then_branch {
                if let r @ Some(_) = match_arm_binding_in_stmt(s, name, offset) {
                    return r;
                }
            }
            if let Some(eb) = else_branch {
                for s in eb {
                    if let r @ Some(_) = match_arm_binding_in_stmt(s, name, offset) {
                        return r;
                    }
                }
            }
            None
        }
        Stmt::While { cond, body } => {
            if let r @ Some(_) = match_arm_binding_in_expr(cond, name, offset) {
                return r;
            }
            for s in body {
                if let r @ Some(_) = match_arm_binding_in_stmt(s, name, offset) {
                    return r;
                }
            }
            None
        }
        Stmt::ForRange {
            start, end, body, ..
        } => {
            if let r @ Some(_) = match_arm_binding_in_expr(start, name, offset) {
                return r;
            }
            if let r @ Some(_) = match_arm_binding_in_expr(end, name, offset) {
                return r;
            }
            for s in body {
                if let r @ Some(_) = match_arm_binding_in_stmt(s, name, offset) {
                    return r;
                }
            }
            None
        }
        Stmt::ForOf { iter, body, .. } => {
            if let r @ Some(_) = match_arm_binding_in_expr(iter, name, offset) {
                return r;
            }
            for s in body {
                if let r @ Some(_) = match_arm_binding_in_stmt(s, name, offset) {
                    return r;
                }
            }
            None
        }
        Stmt::Block(stmts) => {
            for s in stmts {
                if let r @ Some(_) = match_arm_binding_in_stmt(s, name, offset) {
                    return r;
                }
            }
            None
        }
        Stmt::Export(inner) => match_arm_binding_in_stmt(inner, name, offset),
        _ => None,
    }
}

/// Given a slice of `MatchArm`s, return the containing `match_span` if `offset`
/// falls inside any arm's guard or body AND that arm's patterns bind `name`.
fn arm_binding_for_offset(
    arms: &[MatchArm],
    name: &str,
    offset: usize,
    match_span: Span,
) -> Option<Span> {
    for arm in arms {
        // Determine if the cursor is inside this arm's "scope" — i.e. inside its
        // guard expression or its body expression.
        let in_guard = arm
            .guard
            .as_ref()
            .is_some_and(|g| offset >= g.span.start && offset < g.span.end);
        let in_body = offset >= arm.body.span.start && offset < arm.body.span.end;
        if !(in_guard || in_body) {
            continue;
        }
        // Cursor is inside this arm — collect pattern bindings.
        let mut bindings: Vec<std::rc::Rc<str>> = Vec::new();
        for pat in &arm.patterns {
            collect_pattern_bindings(pat, &mut bindings);
        }
        if bindings.iter().any(|b| b.as_ref() == name) {
            // Return the match expression's span as a proxy for "the definition
            // site". This is the best we can do without span information on
            // individual pattern tokens.
            return Some(match_span);
        }
    }
    None
}

/// Search `body` for a match-arm that covers `offset` and binds `name`.
/// Returns the match-expression's span as a stand-in for the definition.
fn match_arm_binding_in_body(body: &[Stmt], name: &str, offset: usize) -> Option<Span> {
    for stmt in body {
        if let r @ Some(_) = match_arm_binding_in_stmt(stmt, name, offset) {
            return r;
        }
    }
    None
}

/// If a top-level statement declares `name`, record its name span (first match wins).
fn collect_decl_name_span(stmt: &Stmt, name: &str, out: &mut Option<Span>) {
    if out.is_some() {
        return;
    }
    match stmt {
        Stmt::Export(inner) => collect_decl_name_span(inner, name, out),
        Stmt::Fn {
            name: n, name_span, ..
        }
        | Stmt::Class {
            name: n, name_span, ..
        }
        | Stmt::Enum {
            name: n, name_span, ..
        }
        | Stmt::Let {
            name: n, name_span, ..
        } => {
            if n == name {
                *out = Some(*name_span);
            }
        }
        Stmt::LetDestructure {
            names,
            rest,
            name_spans,
            ..
        } => {
            for (n, s) in names.iter().zip(name_spans.iter()) {
                if n == name {
                    *out = Some(*s);
                    return;
                }
            }
            if let Some((n, s)) = rest {
                if n == name {
                    *out = Some(*s);
                }
            }
        }
        Stmt::LetDestructureObject { bindings, rest, .. } => {
            for b in bindings {
                if b.binding == name {
                    *out = Some(b.binding_span);
                    return;
                }
            }
            if let Some((n, s)) = rest {
                if n == name {
                    *out = Some(*s);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_diagnostics_match_core_count() {
        let src = "let = 1\nlet = 2\n";
        let core = crate::check::analyze(src).diagnostics.len();
        let lsp = diagnostics(src).len();
        assert_eq!(core, lsp, "LSP must mirror the analysis core");
    }

    #[test]
    fn byte_to_char_handles_non_ascii() {
        // "héllo" — 'é' is two bytes (0xC3 0xA9), so byte offsets diverge from
        // char offsets. Mid-codepoint and out-of-range inputs must not panic.
        let src = "héllo";
        assert_eq!(byte_to_char(src, 0), 0);
        assert_eq!(byte_to_char(src, 1), 1); // just before 'é'
        assert_eq!(byte_to_char(src, 2), 1); // mid-'é' → clamps back to boundary 1
        assert_eq!(byte_to_char(src, 3), 2); // just after 'é'
        assert_eq!(byte_to_char(src, 999), src.chars().count()); // out of range
    }

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
        assert!(
            names.contains(&"bar"),
            "exported bar should appear: {names:?}"
        );

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

    // ---- completions ----

    fn labels(items: &[CompletionItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn completions_baseline_has_keywords_and_builtins() {
        // Anywhere (here, end of a trivial doc) the baseline is offered.
        let src = "let x = 1\n";
        let items = completions(src, src.chars().count());
        let ls = labels(&items);
        for expected in ["fn", "let", "match", "print", "Ok"] {
            assert!(
                ls.contains(&expected),
                "baseline should contain {expected:?}: {ls:?}"
            );
        }
        // Kinds are set.
        let fnkw = items.iter().find(|i| i.label == "fn").unwrap();
        assert_eq!(fnkw.kind, Some(CompletionItemKind::KEYWORD));
        let pr = items.iter().find(|i| i.label == "print").unwrap();
        assert_eq!(pr.kind, Some(CompletionItemKind::FUNCTION));
    }

    #[test]
    fn completions_in_import_path_offers_module_paths() {
        let src = "import { x } from \"std/";
        let items = completions(src, src.chars().count());
        let ls = labels(&items);
        for expected in ["std/string", "std/json", "std/net/http"] {
            assert!(
                ls.contains(&expected),
                "import ctx should contain {expected:?}: {ls:?}"
            );
        }
        assert!(items
            .iter()
            .all(|i| i.kind == Some(CompletionItemKind::MODULE)));
    }

    #[test]
    fn completions_in_import_path_single_quote() {
        let src = "import { x } from 'std/ma";
        let items = completions(src, src.chars().count());
        assert!(labels(&items).contains(&"std/math"));
    }

    #[test]
    fn completions_member_access_offers_module_exports() {
        let src = "import * as math from \"std/math\"\nlet y = math.";
        let items = completions(src, src.chars().count());
        let ls = labels(&items);
        for expected in ["sqrt", "abs", "pi"] {
            assert!(
                ls.contains(&expected),
                "math. should contain {expected:?}: {ls:?}"
            );
        }
    }

    #[test]
    fn completions_member_access_unknown_alias_falls_back_to_baseline() {
        // `foo` is not a namespace import → baseline.
        let src = "let foo = 1\nfoo.";
        let items = completions(src, src.chars().count());
        assert!(labels(&items).contains(&"print"));
    }

    #[test]
    fn completions_on_garbage_returns_baseline_no_panic() {
        for src in ["", "@#$%^", "fn fn fn (((", "import * as", "\"unterminated"] {
            let items = completions(src, src.chars().count());
            assert!(
                labels(&items).contains(&"let"),
                "garbage {src:?} should still yield baseline"
            );
        }
        // An out-of-range offset must not panic.
        let _ = completions("let x", 9999);
    }

    // ---- definition ----

    #[test]
    fn definition_resolves_fn_call_to_decl() {
        let src = "fn foo() { return 1 }\nlet x = foo()";
        // Offset of the `foo` call (the second occurrence — the use, not the decl).
        let first = src.find("foo").unwrap();
        let call = src[first + 3..].find("foo").unwrap() + first + 3;
        let r = definition(src, call).expect("should resolve foo call");
        // foo's name_span is on line 0 (`fn foo`).
        assert_eq!(r.start.line, 0);
        assert_eq!(r.start.character, 3); // after "fn "
    }

    #[test]
    fn definition_resolves_class_use_to_decl() {
        let src = "class C {}\nlet c = C()";
        let use_off = src.rfind('C').unwrap();
        let r = definition(src, use_off).expect("should resolve C use");
        assert_eq!(r.start.line, 0);
        assert_eq!(r.start.character, 6); // after "class "
    }

    #[test]
    fn definition_unknown_ident_is_none() {
        let src = "let x = 1\nprint(zzz)";
        let off = src.find("zzz").unwrap();
        assert!(definition(src, off).is_none());
    }

    #[test]
    fn definition_on_non_ident_is_none() {
        let src = "let x = 1";
        // Offset on the `=`.
        let off = src.find('=').unwrap();
        assert!(definition(src, off).is_none());
    }

    #[test]
    fn definition_resolves_const_and_destructured_let() {
        let src = "const K = 1\nlet [a, b] = pair\nprint(K)\nprint(a)";
        let k_use = src.rfind('K').unwrap();
        let rk = definition(src, k_use).expect("K");
        assert_eq!(rk.start.line, 0);
        // The `a` use is in `print(a)` on the last line.
        let a_use = src.rfind("(a)").unwrap() + 1;
        let ra = definition(src, a_use).expect("a");
        assert_eq!(ra.start.line, 1);
    }

    #[test]
    fn definition_resolves_param_used_in_body() {
        // `n` is a parameter; its use inside the body should resolve to the param's
        // name span (the `n` in the parameter list, line 0).
        let src = "fn f(n) {\n  return n + 1\n}";
        let use_off = src.rfind('n').unwrap(); // the `n` in `n + 1`
        let r = definition(src, use_off).expect("should resolve param n");
        assert_eq!(r.start.line, 0);
        // `fn f(` is 5 chars, so the param `n` is at character 5.
        assert_eq!(r.start.character, 5);
    }

    #[test]
    fn definition_resolves_local_let_in_body() {
        let src = "fn f() {\n  let y = 1\n  return y\n}";
        let use_off = src.rfind('y').unwrap(); // `return y`
        let r = definition(src, use_off).expect("should resolve local y");
        assert_eq!(r.start.line, 1); // the `let y` line
    }

    #[test]
    fn definition_top_level_fn_called_from_inside_another_fn() {
        // `helper` is top-level; calling it from inside `main` must resolve to the
        // top-level decl, not be swallowed by param/local resolution.
        let src = "fn helper() { return 1 }\nfn main(x) {\n  return helper()\n}";
        let call = src.rfind("helper").unwrap();
        let r = definition(src, call).expect("should resolve helper call");
        assert_eq!(r.start.line, 0);
        assert_eq!(r.start.character, 3); // after "fn "
    }

    #[test]
    fn definition_param_does_not_shadow_when_name_differs() {
        // The param is `x`; a call to top-level `g` inside the fn still resolves to `g`.
        let src = "fn g() { return 2 }\nfn h(x) {\n  return g() + x\n}";
        let g_call = src.rfind("g()").unwrap();
        let rg = definition(src, g_call).expect("g");
        assert_eq!(rg.start.line, 0);
        // And `x` resolves to the param on line 1.
        let x_use = src.rfind("+ x").unwrap() + 2;
        let rx = definition(src, x_use).expect("x");
        assert_eq!(rx.start.line, 1);
    }

    // ---- Phase 8c: match-arm pattern binding recognition ----

    /// Assert that `definition(src, off)` returns `Some(_)` — i.e. the name at
    /// `off` is recognized as "defined" (go-to-definition succeeds). This is the
    /// LSP invariant we care about: pattern-bound names inside an arm body/guard
    /// must not look "undefined" to the LSP.
    fn assert_defined(src: &str, off: usize, label: &str) {
        assert!(
            definition(src, off).is_some(),
            "expected {label:?} at offset {off} to be defined in:\n{src}"
        );
    }

    #[test]
    fn definition_resolves_bare_ident_pattern_binding() {
        // `other` is a bare-ident binding (Option C). The use inside the arm body
        // (`=> other`) must resolve.
        let src = "fn f(x) { return match x { other => other } }";
        // Find the second `other` (the use in the body, not the pattern).
        let first = src.find("other").unwrap();
        let use_off = src[first + 5..].find("other").unwrap() + first + 5;
        assert_defined(src, use_off, "other");
    }

    #[test]
    fn definition_resolves_array_pattern_binding() {
        // `[x, nil]` — `x` is bound; use inside the arm body must resolve.
        let src = "fn f(p) { return match p { [x, nil] => x } }";
        // Find the `x` after `=> ` (the use in the body).
        let body_x = src.rfind("=> x").unwrap() + 3;
        assert_defined(src, body_x, "x in array pattern body");
    }

    #[test]
    fn definition_resolves_array_rest_binding() {
        // `[first, ...rest]` — both `first` and `rest` are bound in the arm body.
        let src = "fn f(xs) { return match xs { [first, ...rest] => first } }";
        let first_use = src.rfind("=> first").unwrap() + 3;
        assert_defined(src, first_use, "first in rest-array pattern");
    }

    #[test]
    fn definition_resolves_object_shorthand_binding() {
        // `{name}` — shorthand object pattern binds `name`.
        let src = "fn f(u) { return match u { {name} => name } }";
        let name_use = src.rfind("=> name").unwrap() + 3;
        assert_defined(src, name_use, "name in object shorthand pattern");
    }

    #[test]
    fn definition_resolves_object_sub_pattern_binding() {
        // `{role: r}` — sub-pattern binds `r`.
        let src = "fn f(u) { return match u { {role: r} => r } }";
        let r_use = src.rfind("=> r").unwrap() + 3;
        assert_defined(src, r_use, "r in object sub-pattern");
    }

    #[test]
    fn definition_resolves_pattern_binding_in_guard() {
        // `x if x > 0 => x` — `x` is used in BOTH the guard and the body.
        let src = "fn f(n) { return match n { x if x > 0 => x } }";
        // Find `x` in the guard `x > 0` (offset = position of `x` after "if ").
        let guard_x = src.find("if ").unwrap() + 3;
        assert_defined(src, guard_x, "x in guard");
        // Also the body.
        let body_x = src.rfind("=> x").unwrap() + 3;
        assert_defined(src, body_x, "x in body after guard");
    }

    #[test]
    fn definition_resolves_bare_ident_arm_alongside_or_pattern() {
        // Honest naming: this is a single bare-ident catch-all arm (`x => x`), not
        // an or-pattern. Or-pattern alternatives are literals (`"sat" | "sun"`)
        // which bind nothing, so the binding-resolution path is exercised here via
        // the bare-ident fall-through arm. The `|` form itself is covered by the
        // fmt idempotence tests.
        let src = "fn f(d) { return match d { \"a\" | \"b\" => 0, x => x } }";
        let x_use = src.rfind("=> x").unwrap() + 3;
        assert_defined(src, x_use, "x in bare-ident catch-all arm");
    }

    #[test]
    fn definition_wildcard_does_not_bind() {
        // `_` never binds anything — using `_` in the body still works
        // (it's an ident with the special name `_`, not a match binding) but
        // the LSP should NOT resolve `_` through the match-arm path.
        // This test just verifies no panic occurs.
        let src = "fn f(n) { return match n { _ => 0 } }";
        // `_` may or may not resolve (the LSP doesn't bind it). We just don't
        // panic and the program still produces a valid diagnostics response.
        let _ = definition(src, src.find('_').unwrap());
    }

    #[test]
    fn definition_resolves_object_rest_binding() {
        // `{a, ...rest}` — the named rest `rest` is bound in the arm body.
        let src = "fn f(obj) { return match obj { {a, ...rest} => rest } }";
        let rest_use = src.rfind("=> rest").unwrap() + 3;
        assert_defined(src, rest_use, "rest in object-rest pattern");
    }

    #[test]
    fn definition_match_arm_binding_does_not_leak_to_outer_scope() {
        // A name bound inside a match arm must NOT resolve when referenced OUTSIDE
        // that arm. `x` is bound only in arm 1's body (`[x, nil] => x`); the
        // trailing `return x` is in the enclosing function scope where `x` is
        // undefined, so go-to-definition must yield None there.
        let src = "fn f(p) { let r = match p { [x, nil] => x, _ => 0 }\n  return x }";
        // The OUT-OF-SCOPE `x` in `return x` — find the `x` after the last `return `.
        let out_of_scope_x = src.rfind("return x").unwrap() + "return ".len();
        // Sanity: that offset really sits on the `x` ident token (not the keyword).
        assert_eq!(&src[out_of_scope_x..out_of_scope_x + 1], "x");
        assert!(
            definition(src, out_of_scope_x).is_none(),
            "arm binding `x` must NOT resolve outside its arm (in `return x`)"
        );
        // Control: the in-arm use (`=> x`) DOES resolve, proving the difference is
        // scope isolation and not a blanket failure to resolve `x`.
        let in_arm_x = src.find("=> x").unwrap() + 3;
        assert!(
            definition(src, in_arm_x).is_some(),
            "the in-arm `x` use should still resolve"
        );
    }

    // ---- module-path / builtin sync guards ----

    #[test]
    fn std_module_paths_all_resolve_under_default_features() {
        // Every advertised import path must be a real registered module, so the const
        // can't silently drift from `std_module_exports`. (cargo test enables all
        // default features, so every default-gated path resolves.)
        for path in STD_MODULE_PATHS {
            assert!(
                crate::stdlib::std_module_exports(path).is_some(),
                "STD_MODULE_PATHS entry {path:?} is not a known stdlib module"
            );
        }
    }

    #[test]
    fn completions_in_import_path_offers_process() {
        let src = "import { run } from \"std/proc";
        let items = completions(src, src.chars().count());
        assert!(labels(&items).contains(&"std/process"));
    }

    #[test]
    fn hover_on_builtin_test_mentions_test() {
        let src = "test(\"x\", fn() {})";
        let off = 0; // on `test`
        let h = hover(src, off).expect("expected hover on test builtin");
        assert!(markup(&h).contains("test"), "got: {}", markup(&h));
    }

    #[test]
    fn hover_on_yield_keyword_describes_it() {
        let src = "fn* g() { yield 1 }";
        let off = src.find("yield").unwrap();
        let h = hover(src, off).expect("expected hover on yield keyword");
        assert!(
            markup(&h).to_lowercase().contains("generator"),
            "got: {}",
            markup(&h)
        );
    }

    #[test]
    fn completions_baseline_includes_yield_keyword() {
        let items = completions("", 0);
        let y = items
            .iter()
            .find(|i| i.label == "yield")
            .expect("yield keyword in baseline");
        assert_eq!(y.kind, Some(CompletionItemKind::KEYWORD));
    }

    #[test]
    fn document_symbols_lists_generator_fn() {
        // A `fn*` declaration still produces a FUNCTION symbol (the parser flows
        // through the shared AST that the LSP walks).
        let syms = document_symbols("fn* count() { yield 1 }");
        let f = syms
            .iter()
            .find(|s| s.name == "count")
            .expect("count symbol");
        assert_eq!(f.kind, SymbolKind::FUNCTION);
    }

    #[test]
    fn completions_baseline_includes_test_builtin() {
        let items = completions("", 0);
        let t = items
            .iter()
            .find(|i| i.label == "test")
            .expect("test builtin in baseline");
        assert_eq!(t.kind, Some(CompletionItemKind::FUNCTION));
    }
}
