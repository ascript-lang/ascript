//! Pure static-analysis layer for the LSP.
//!
//! Every function here takes `&str` source and returns owned `lsp_types` data.
//! It reuses the interpreter's `lexer`/`parser` but NEVER runs the interpreter, so
//! it holds no `Rc`/`RefCell`/`Value` and is trivially `Send`. This keeps the
//! tower-lsp `LanguageServer` impl `Send + Sync`-clean.

use crate::ast::{Param, Stmt};
use crate::error::AsError;
use crate::lexer;
use crate::lsp::line_index::LineIndex;
use crate::parser;
use crate::span::Span;
use crate::token::Tok;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, Diagnostic, DiagnosticSeverity, DocumentSymbol, Hover,
    HoverContents, MarkupContent, MarkupKind, Position, Range, SymbolKind,
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
        Some(span) => Range {
            start: index.position(span.start),
            end: index.position(span.end),
        },
        // No span: point at the start of the document (line 0).
        None => Range {
            start: Position::new(0, 0),
            end: Position::new(0, 0),
        },
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
