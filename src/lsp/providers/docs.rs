//! Shared documentation tables + cursor/token helpers for the LSP providers.
//!
//! Moved out of the legacy `analysis.rs` and re-homed on the cached
//! [`SemanticModel`]: token-at-cursor uses the model's cached `LexToken`s (no
//! re-lex), and declaration docs reuse the CST-based document-symbol walk.

use crate::lsp::model::SemanticModel;
use crate::syntax::kind::SyntaxKind;
use tower_lsp::lsp_types::{Position, SymbolKind};

/// The byte offset under an LSP `Position`: char offset via the model's
/// `LineIndex`, then char→byte (the CST/infer layer speaks bytes).
pub fn byte_offset_at(model: &SemanticModel, position: Position) -> usize {
    let char_off = model.line_index.offset(position);
    crate::lsp::convert::char_to_byte(&model.text, char_off)
}

/// A doc string for the token under `byte_offset`, if any: a keyword's
/// description, a builtin's signature/doc, or a top-level declaration's kind.
/// Returns `None` when the cursor is on trivia / whitespace / an unknown token.
pub fn doc_at(model: &SemanticModel, byte_offset: usize) -> Option<String> {
    let (kind, text) = token_at(model, byte_offset)?;
    match kind {
        SyntaxKind::Ident => ident_doc(&text, model),
        other => keyword_doc(other).map(str::to_string),
    }
}

/// The `(kind, text)` of the cached `LexToken` whose half-open byte span
/// `[start, end)` contains `byte_offset`. Token byte spans are derived by
/// cumulative `text.len()` over the stream (`LexToken` has no position field).
/// An offset at the boundary of two tokens prefers the token that STARTS there.
fn token_at(model: &SemanticModel, byte_offset: usize) -> Option<(SyntaxKind, String)> {
    let mut pos = 0usize;
    for tok in &model.tokens {
        let start = pos;
        let end = pos + tok.text.len();
        if byte_offset >= start && byte_offset < end {
            return Some((tok.kind, tok.text.clone()));
        }
        pos = end;
    }
    None
}

/// Documentation for a keyword `SyntaxKind`, if it is one. Mirrors the legacy
/// `analysis.rs` keyword table.
pub fn keyword_doc(kind: SyntaxKind) -> Option<&'static str> {
    use SyntaxKind::*;
    let s = match kind {
        LetKw => "`let` — declare a mutable variable binding.",
        ConstKw => "`const` — declare an immutable (constant) binding.",
        FnKw => "`fn` — declare a function.",
        ReturnKw => "`return` — return a value from the enclosing function.",
        IfKw => "`if` — conditional execution.",
        ElseKw => "`else` — the alternative branch of an `if`.",
        WhileKw => "`while` — loop while a condition holds.",
        ForKw => "`for` — iterate with `for (x of xs)` or `for (i in a..b)`.",
        OfKw => "`of` — iterate over the elements of a collection in a `for` loop.",
        InKw => "`in` — iterate over a range in a `for` loop.",
        MatchKw => "`match` — pattern-match a value against arms.",
        AsyncKw => "`async` — declare an asynchronous function returning a future.",
        AwaitKw => "`await` — suspend until an async value resolves.",
        YieldKw => {
            "`yield` — produce a value from a generator (`fn*`); evaluates to the resume value."
        }
        ClassKw => "`class` — declare a class with methods.",
        EnumKw => "`enum` — declare an enumeration of named variants.",
        ImportKw => "`import` — import names from another module.",
        ExportKw => "`export` — make a declaration available to importers.",
        NilKw => "`nil` — the absence of a value.",
        TrueKw => "`true` — the boolean true literal.",
        FalseKw => "`false` — the boolean false literal.",
        BreakKw => "`break` — exit the enclosing loop.",
        ContinueKw => "`continue` — skip to the next loop iteration.",
        _ => return None,
    };
    Some(s)
}

/// Documentation for an identifier: a global builtin (static table), else a
/// top-level declaration in the model, else `None`.
fn ident_doc(name: &str, model: &SemanticModel) -> Option<String> {
    if let Some(b) = builtin_doc(name) {
        return Some(b.to_string());
    }
    decl_doc(name, model)
}

/// Signature + one-line doc for a global builtin function, if `name` is one.
/// Mirrors the legacy `analysis.rs` builtin table.
pub fn builtin_doc(name: &str) -> Option<&'static str> {
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

/// If `name` is a top-level declaration in the model, describe its kind (e.g.
/// "fn `foo`"). Reuses the CST document-symbol walk so the table stays in one
/// place.
fn decl_doc(name: &str, model: &SemanticModel) -> Option<String> {
    let syms = crate::lsp::providers::symbols::document_symbols(model);
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
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn doc_at_keyword_describes_it() {
        let m = model("fn foo() {}\n");
        let doc = doc_at(&m, 0).expect("hover on fn"); // on `fn`
        assert!(doc.to_lowercase().contains("function"), "got: {doc}");
    }

    #[test]
    fn doc_at_builtin_print() {
        let m = model("print(1)\n");
        let off = m.text.find("print").unwrap();
        let doc = doc_at(&m, off).expect("hover on print");
        assert!(doc.contains("print"), "got: {doc}");
    }

    #[test]
    fn doc_at_user_fn_use_says_fn() {
        let m = model("fn foo() {}\nfoo()\n");
        let off = m.text.rfind("foo").unwrap(); // the use
        let doc = doc_at(&m, off).expect("hover on foo use");
        assert!(doc.contains("foo") && doc.contains("fn"), "got: {doc}");
    }

    #[test]
    fn doc_at_whitespace_is_none() {
        let m = model("let x = 1\n");
        // Byte 3 is the space between `let` and `x`.
        assert!(doc_at(&m, 3).is_none());
    }

    #[test]
    fn doc_at_unknown_ident_is_none() {
        let m = model("zzz\n");
        assert!(doc_at(&m, 0).is_none());
    }

    #[test]
    fn byte_offset_at_maps_position() {
        let m = model("let x = 1\nprint(x)\n");
        // Line 1, char 0 is the `p` of print at byte 10.
        let off = byte_offset_at(&m, Position::new(1, 0));
        assert_eq!(off, 10);
    }
}
