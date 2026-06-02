//! The bytecode compiler — walks the CST typed AST (plus the resolver's binding
//! information) and emits a [`Chunk`] for the VM to run.
//!
//! V1 scope: a source file whose meaningful content is a single trailing
//! expression statement (or one expression statement). It compiles literals,
//! arithmetic (`+ - * / % **`), unary `-`/`!`, and parentheses, then emits
//! `RETURN` so the VM yields the expression's value. Statements, locals, control
//! flow, calls, and the richer literal grammar (templates, escapes, hex/binary/
//! scientific numbers) land in V2+.

use crate::span::Span;
use crate::syntax::ast::{
    AstNode, BinaryExpr, CallExpr, Expr, Literal, ParenExpr, SourceFile, Stmt, TemplateExpr,
    UnaryExpr,
};
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{ResolveResult, Resolution};
use crate::syntax::{parse_to_tree, resolve::resolve};
use crate::value::Value;
use crate::vm::chunk::Chunk;
use crate::vm::opcode::Op;
use std::rc::Rc;

/// A compile-time error: a message plus the source span that triggered it. The
/// lib boundary converts this into an [`crate::error::AsError`] for reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    pub message: String,
    pub span: Span,
}

impl CompileError {
    fn new(message: impl Into<String>, span: Span) -> Self {
        CompileError {
            message: message.into(),
            span,
        }
    }
}

/// The span of a CST node, as byte offsets into the original source.
fn node_span(node: &impl AstNode) -> Span {
    range_span(node.syntax())
}

/// The span of a raw CST node, as byte offsets into the original source.
fn range_span(node: &crate::syntax::cst::ResolvedNode) -> Span {
    let range = node.text_range();
    Span::new(usize::from(range.start()), usize::from(range.end()))
}

/// Compile `src` into a top-level [`Chunk`].
///
/// Pipeline: `parse_to_tree` → `SourceFile::cast` → `resolve` (wired so the full
/// front-end runs even though V1 has no locals/globals to bind) → walk the
/// statements, compiling the trailing expression and emitting `RETURN`.
pub fn compile_source(src: &str) -> Result<Chunk, CompileError> {
    let root = parse_to_tree(src);
    let file =
        SourceFile::cast(root.clone()).ok_or_else(|| CompileError::new("expected a source file", Span::new(0, src.len())))?;

    // Run the resolver so the compiler can classify identifier uses (e.g. a bare
    // builtin callee in a `print(...)` call resolves to `Resolution::Global`).
    let resolved = resolve(&root);

    let mut compiler = Compiler {
        chunk: Chunk::new(),
        resolved,
    };

    // V2 supports a program that is a sequence of expression statements whose
    // meaningful tail is an expression. Leading expression statements (e.g.
    // `print(...)` calls) are compiled and their result discarded with `POP`; the
    // trailing expression is left on the stack and `RETURN`ed. Other statement
    // kinds (let/if/while/...) land in V3+.
    let stmts: Vec<Stmt> = file.stmts().collect();
    let trailing = stmts.iter().rev().find_map(|s| match s {
        Stmt::ExprStmt(e) => Some(e.clone()),
        _ => None,
    });

    let Some(expr_stmt) = trailing else {
        return Err(CompileError::new(
            "V2 compiler requires a trailing expression statement",
            Span::new(0, src.len()),
        ));
    };
    let trailing_node = expr_stmt.syntax().clone();

    // Reject any non-expression statement in the file (V2 is expression-only).
    for s in &stmts {
        if !matches!(s, Stmt::ExprStmt(_)) {
            return Err(CompileError::new(
                "statement kind not yet supported in V2 (expression-only)",
                stmt_span(s),
            ));
        }
    }

    // Compile each leading expression statement as a side-effecting expression
    // whose result is discarded; the trailing one is the program's value.
    for s in &stmts {
        let Stmt::ExprStmt(es) = s else { continue };
        let is_trailing = *es.syntax() == trailing_node;
        if is_trailing {
            continue;
        }
        let expr = es
            .expr()
            .ok_or_else(|| CompileError::new("empty expression statement", node_span(es)))?;
        compiler.compile_expr(&expr)?;
        compiler.chunk.emit(Op::Pop, node_span(es));
    }

    let expr = expr_stmt
        .expr()
        .ok_or_else(|| CompileError::new("empty expression statement", node_span(&expr_stmt)))?;
    compiler.compile_expr(&expr)?;
    compiler.chunk.emit(Op::Return, node_span(&expr_stmt));

    Ok(compiler.chunk)
}

/// The span of a `Stmt`, by reading its wrapped CST node (the enum does not
/// expose a single `syntax()` accessor, so we match each variant).
fn stmt_span(stmt: &Stmt) -> Span {
    let node = match stmt {
        Stmt::LetStmt(n) => n.syntax(),
        Stmt::ExprStmt(n) => n.syntax(),
        Stmt::Block(n) => n.syntax(),
        Stmt::IfStmt(n) => n.syntax(),
        Stmt::WhileStmt(n) => n.syntax(),
        Stmt::ReturnStmt(n) => n.syntax(),
        Stmt::FnDecl(n) => n.syntax(),
        Stmt::ForStmt(n) => n.syntax(),
        Stmt::BreakStmt(n) => n.syntax(),
        Stmt::ContinueStmt(n) => n.syntax(),
        Stmt::EnumDecl(n) => n.syntax(),
        Stmt::ClassDecl(n) => n.syntax(),
        Stmt::ImportStmt(n) => n.syntax(),
        Stmt::ExportStmt(n) => n.syntax(),
    };
    range_span(node)
}

struct Compiler {
    chunk: Chunk,
    resolved: ResolveResult,
}

impl Compiler {
    fn compile_expr(&mut self, expr: &Expr) -> Result<(), CompileError> {
        match expr {
            Expr::Literal(lit) => self.compile_literal(lit),
            Expr::BinaryExpr(bin) => self.compile_binary(bin),
            Expr::UnaryExpr(un) => self.compile_unary(un),
            Expr::ParenExpr(paren) => self.compile_paren(paren),
            Expr::CallExpr(call) => self.compile_call(call),
            Expr::TemplateExpr(t) => self.compile_template(t),
            other => Err(CompileError::new(
                "expression kind not yet supported in V2",
                node_span(other),
            )),
        }
    }

    /// Lower a call whose callee is a bare builtin name (`print`, `len`, `type`,
    /// …): `GET_GLOBAL <name>`, then each argument, then `CALL argc`.
    ///
    /// V2 supports only calls to bare builtins. The callee is classified via the
    /// resolver: a `NameRef` whose use resolves to `Resolution::Global(name)`
    /// where `name` is a known builtin. Anything else (method calls, calls to
    /// user functions/locals/upvalues) is a documented V4 deferral.
    fn compile_call(&mut self, call: &CallExpr) -> Result<(), CompileError> {
        let span = node_span(call);
        let callee = call
            .expr()
            .ok_or_else(|| CompileError::new("call expression missing callee", span))?;

        // Only a bare `NameRef` callee is supported in V2.
        let Expr::NameRef(name_ref) = &callee else {
            return Err(CompileError::new(
                "calls to non-builtins not yet supported (V4)",
                node_span(&callee),
            ));
        };

        // Classify the callee via the resolver: it must be a Global builtin.
        let key = name_ref.syntax().text_range();
        let builtin_name = match self.resolved.uses.get(&key) {
            Some(Resolution::Global(name)) if crate::interp::BUILTIN_NAMES.contains(&name.as_str()) => {
                name.clone()
            }
            _ => {
                return Err(CompileError::new(
                    "calls to non-builtins not yet supported (V4)",
                    node_span(&callee),
                ));
            }
        };

        // GET_GLOBAL <name-const>
        let name_idx = self.chunk.add_const(Value::Str(Rc::from(builtin_name.as_str())));
        self.chunk.emit_u16(Op::GetGlobal, name_idx, span);

        // Compile each argument, left to right.
        let mut argc: u8 = 0;
        if let Some(arg_list) = call.arg_list() {
            for arg in arg_list.exprs() {
                self.compile_expr(&arg)?;
                argc = argc.checked_add(1).ok_or_else(|| {
                    CompileError::new("too many call arguments (max 255)", span)
                })?;
            }
        }

        self.chunk.emit_u8(Op::Call, argc, span);
        Ok(())
    }

    fn compile_literal(&mut self, lit: &Literal) -> Result<(), CompileError> {
        let span = node_span(lit);
        let kind = lit
            .op()
            .ok_or_else(|| CompileError::new("malformed literal (no token)", span))?;
        // Read the literal *token*'s text directly — `node.text()` would include
        // leading/trailing trivia (whitespace/comments) attached to the node.
        let text = literal_token_text(lit)
            .ok_or_else(|| CompileError::new("malformed literal (no token text)", span))?;
        let value = match kind {
            SyntaxKind::Number => {
                let n = parse_number(&text).ok_or_else(|| {
                    // The lexer already validated the token, so this is a
                    // compiler bug rather than a user error if it ever fires.
                    CompileError::new(format!("malformed number literal {text:?}"), span)
                })?;
                Value::Number(n)
            }
            SyntaxKind::Str => Value::Str(Rc::from(unescape_string(&text).as_str())),
            SyntaxKind::TrueKw => Value::Bool(true),
            SyntaxKind::FalseKw => Value::Bool(false),
            SyntaxKind::NilKw => Value::Nil,
            other => {
                return Err(CompileError::new(
                    format!("literal token {other:?} not yet supported in V1"),
                    span,
                ))
            }
        };
        let idx = self.chunk.add_const(value);
        self.chunk.emit_u16(Op::Const, idx, span);
        Ok(())
    }

    fn compile_binary(&mut self, bin: &BinaryExpr) -> Result<(), CompileError> {
        let span = node_span(bin);
        let lhs = bin
            .lhs()
            .ok_or_else(|| CompileError::new("binary expression missing left operand", span))?;
        let rhs = bin
            .rhs()
            .ok_or_else(|| CompileError::new("binary expression missing right operand", span))?;
        let op = bin
            .op()
            .ok_or_else(|| CompileError::new("binary expression missing operator", span))?;
        self.compile_expr(&lhs)?;
        self.compile_expr(&rhs)?;
        let bytecode = match op {
            SyntaxKind::Plus => Op::Add,
            SyntaxKind::Minus => Op::Sub,
            SyntaxKind::Star => Op::Mul,
            SyntaxKind::Slash => Op::Div,
            SyntaxKind::Percent => Op::Mod,
            SyntaxKind::StarStar => Op::Pow,
            other => {
                return Err(CompileError::new(
                    format!("binary operator {other:?} not yet supported in V1"),
                    span,
                ))
            }
        };
        self.chunk.emit(bytecode, span);
        Ok(())
    }

    fn compile_unary(&mut self, un: &UnaryExpr) -> Result<(), CompileError> {
        let span = node_span(un);
        let operand = un
            .expr()
            .ok_or_else(|| CompileError::new("unary expression missing operand", span))?;
        let op = un
            .op()
            .ok_or_else(|| CompileError::new("unary expression missing operator", span))?;
        self.compile_expr(&operand)?;
        let bytecode = match op {
            SyntaxKind::Minus => Op::Neg,
            SyntaxKind::Bang => Op::Not,
            other => {
                return Err(CompileError::new(
                    format!("unary operator {other:?} not yet supported in V1"),
                    span,
                ))
            }
        };
        self.chunk.emit(bytecode, span);
        Ok(())
    }

    fn compile_paren(&mut self, paren: &ParenExpr) -> Result<(), CompileError> {
        let inner = paren.expr().ok_or_else(|| {
            CompileError::new("empty parenthesized expression", node_span(paren))
        })?;
        // Parens affect only grouping; no opcode is emitted.
        self.compile_expr(&inner)
    }

    /// Lower a template literal `` `a${e}b` `` into `n` part-pushes followed by
    /// `TEMPLATE n`, where `n` is the total number of parts (literal chunks +
    /// interpolated expressions). The CST `TemplateExpr` node interleaves
    /// template *tokens* (`TemplateStr`/`TemplateStart`/`TemplateMiddle`/
    /// `TemplateEnd`, each carrying its raw delimited source text) with the
    /// interpolated expression *nodes*. We walk `children_with_tokens()` in
    /// source order: every template token contributes a literal string chunk
    /// (delimiters stripped + unescaped, exactly mirroring the tree-walker's
    /// `lex_template_chunk`), and every expression node is compiled in place.
    ///
    /// The tree-walker's `ExprKind::Template` concatenates each chunk and each
    /// interpolated value (coerced via `Value::to_string()`); the VM's
    /// `TEMPLATE n` op performs the identical concatenation/coercion.
    fn compile_template(&mut self, t: &TemplateExpr) -> Result<(), CompileError> {
        let span = node_span(t);
        let mut parts: u16 = 0;
        for child in t.syntax().children_with_tokens() {
            if let Some(tok) = child.as_token() {
                // A template *token* carries a raw, delimited literal chunk
                // (`` `...${ ``, `}...${`, `` }...` ``, or full `` `...` ``).
                match tok.kind() {
                    SyntaxKind::TemplateStr
                    | SyntaxKind::TemplateStart
                    | SyntaxKind::TemplateMiddle
                    | SyntaxKind::TemplateEnd => {
                        let chunk = unescape_template_chunk(tok.text());
                        let idx = self.chunk.add_const(Value::Str(Rc::from(chunk.as_str())));
                        self.chunk.emit_u16(Op::Const, idx, span);
                    }
                    // Trivia (whitespace/comments) never appears between template
                    // delimiters, but skip it defensively (no part emitted).
                    _ => continue,
                }
            } else if let Some(node) = child.as_node() {
                let expr = Expr::cast((*node).clone()).ok_or_else(|| {
                    CompileError::new("template interpolation is not an expression", span)
                })?;
                self.compile_expr(&expr)?;
            } else {
                continue;
            }
            parts = parts
                .checked_add(1)
                .ok_or_else(|| CompileError::new("template has too many parts", span))?;
        }
        self.chunk.emit_u16(Op::Template, parts, span);
        Ok(())
    }
}

/// The text of a `Literal` node's value token (Number/Str/keyword), excluding
/// any trivia. Mirrors the kind set in the generated `Literal::op()`.
fn literal_token_text(lit: &Literal) -> Option<String> {
    lit.syntax()
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| {
            matches!(
                t.kind(),
                SyntaxKind::Number
                    | SyntaxKind::Str
                    | SyntaxKind::TrueKw
                    | SyntaxKind::FalseKw
                    | SyntaxKind::NilKw
            )
        })
        .map(|t| t.text().to_string())
}

/// Parse a numeric literal token into the exact `f64` the tree-walker produces.
///
/// This mirrors the legacy lexer's number scan (`src/lexer.rs`) byte-for-byte:
/// hex (`0x..`/`0X..`) and binary (`0b..`/`0B..`) prefixes parse the digits via
/// `u64::from_str_radix` then cast to `f64`; everything else (plain decimals,
/// floats, scientific `1e9`) is parsed by `f64::parse`. Underscore digit
/// separators are stripped first in all forms. The lexer has already validated
/// the token's shape, so this only fails on a genuine compiler/lexer
/// disagreement (→ `None`, surfaced as a `CompileError`).
fn parse_number(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'0' && matches!(bytes[1], b'x' | b'X' | b'b' | b'B') {
        let radix = if matches!(bytes[1], b'x' | b'X') { 16 } else { 2 };
        let digits: String = text[2..].chars().filter(|&c| c != '_').collect();
        if digits.is_empty() {
            return None;
        }
        return u64::from_str_radix(&digits, radix).ok().map(|n| n as f64);
    }
    let cleaned: String = text.chars().filter(|&c| c != '_').collect();
    cleaned.parse::<f64>().ok()
}

/// Translate the character following a `\` into its escaped value. Mirrors the
/// legacy lexer's `escape_char` (`src/lexer.rs`) EXACTLY: the known escapes plus
/// a lenient passthrough for any other char (`\x` → `x`). AScript has NO
/// `\u`/`\x`/numeric escapes, so they fall through to the passthrough — matching
/// the tree-walker.
fn escape_char(c: char) -> char {
    match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        '0' => '\0',
        '\\' => '\\',
        '"' => '"',
        '\'' => '\'',
        other => other,
    }
}

/// Unescape a `"..."` / `'...'` string-literal token into its runtime value,
/// mirroring the legacy lexer's `lex_quoted`. The raw token text includes the
/// surrounding quotes; they are stripped, then backslash escapes are translated
/// via [`escape_char`]. A trailing lone `\` (cannot occur for a lexer-accepted,
/// terminated token) is kept verbatim, matching the legacy scan's behavior.
fn unescape_string(raw: &str) -> String {
    let inner = strip_quotes(raw);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(e) => out.push(escape_char(e)),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Unescape a template literal *chunk* token (`TemplateStr`/`TemplateStart`/
/// `TemplateMiddle`/`TemplateEnd`) into its literal string contents, mirroring
/// the legacy lexer's `lex_template_chunk`. The raw token text carries its
/// delimiters: a leading `` ` `` or `}`, and a trailing `` ` `` or `${`. We
/// strip those, then apply the template escape set — `` \` `` → `` ` `` and
/// `\$` → `$` are template-specific; everything else shares [`escape_char`].
fn unescape_template_chunk(raw: &str) -> String {
    let inner = strip_template_delims(raw);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('`') => out.push('`'),
                Some('$') => out.push('$'),
                Some(other) => out.push(escape_char(other)),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Strip the leading delimiter (`` ` `` or `}`) and trailing delimiter
/// (`` ` `` or `${`) from a raw template-chunk token, yielding the inner text.
/// Mirrors the lossless slicing the CST lexer's `scan_template_chunk` produces.
fn strip_template_delims(s: &str) -> &str {
    // Leading delimiter is a single byte: `` ` `` or `}`.
    let after_open = s.strip_prefix('`').or_else(|| s.strip_prefix('}')).unwrap_or(s);
    // Trailing delimiter is either `${` (interpolation continues) or `` ` ``.
    if let Some(inner) = after_open.strip_suffix("${") {
        inner
    } else if let Some(inner) = after_open.strip_suffix('`') {
        inner
    } else {
        // Unterminated tail (lexer would have flagged it); use as-is.
        after_open
    }
}

/// Strip one leading and one trailing matching quote (`"` or `'`) if present.
fn strip_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            return &s[1..s.len() - 1];
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::disasm::disasm;

    #[test]
    fn compile_one_plus_two_emits_const_const_add_return() {
        let chunk = compile_source("1 + 2").expect("compiles");
        let text = disasm(&chunk);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.iter().any(|l| l.contains("CONST") && l.ends_with("; 1")), "missing CONST 1 in:\n{text}");
        assert!(lines.iter().any(|l| l.contains("CONST") && l.ends_with("; 2")), "missing CONST 2 in:\n{text}");
        assert!(lines.iter().any(|l| l.contains("ADD")), "missing ADD in:\n{text}");
        assert!(lines.iter().any(|l| l.contains("RETURN")), "missing RETURN in:\n{text}");
    }

    #[test]
    fn rejects_unsupported_binary_operator() {
        let err = compile_source("1 == 2").unwrap_err();
        assert!(err.message.contains("not yet supported"), "got {err:?}");
    }

    #[test]
    fn compile_print_emits_get_global_call() {
        let chunk = compile_source("print(1 + 2)").expect("compiles");
        let text = disasm(&chunk);
        assert!(
            text.contains("GET_GLOBAL") && text.contains("print"),
            "missing GET_GLOBAL print in:\n{text}"
        );
        assert!(text.contains("ADD"), "missing ADD in:\n{text}");
        assert!(text.contains("CALL"), "missing CALL in:\n{text}");
        assert!(text.contains("RETURN"), "missing RETURN in:\n{text}");
    }

    #[test]
    fn leading_print_statement_is_popped() {
        // A non-trailing print(...) compiles a CALL followed by POP; the trailing
        // expression is RETURNed.
        let chunk = compile_source("print(1)\n2").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("CALL"), "missing CALL in:\n{text}");
        assert!(text.contains("POP"), "missing POP in:\n{text}");
        assert!(text.contains("RETURN"), "missing RETURN in:\n{text}");
    }

    #[test]
    fn rejects_call_to_non_builtin() {
        // `foo` is not a builtin; resolver classifies it Global("foo") which is
        // not in BUILTIN_NAMES → documented V4 deferral.
        let err = compile_source("foo(1)").unwrap_err();
        assert!(
            err.message.contains("non-builtins not yet supported (V4)"),
            "got {err:?}"
        );
    }

    #[test]
    fn compiles_string_literal() {
        let chunk = compile_source("\"hi\"").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("; \"hi\""), "missing string const in:\n{text}");
    }

    async fn eval_number(src: &str) -> f64 {
        match crate::vm_eval_source(src).await.expect("evaluates") {
            Value::Number(n) => n,
            other => panic!("expected Number, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn precedence_from_cst() {
        assert_eq!(eval_number("1 + 2 * 3").await, 7.0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unary_negate() {
        assert_eq!(eval_number("-(4)").await, -4.0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn parens_group() {
        assert_eq!(eval_number("(1 + 2) * 4").await, 12.0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn division_is_float() {
        assert_eq!(eval_number("10 / 4").await, 2.5);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn modulo() {
        assert_eq!(eval_number("7 % 3").await, 1.0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn power() {
        assert_eq!(eval_number("2 ** 10").await, 1024.0);
    }

    #[test]
    fn parse_number_all_forms() {
        assert_eq!(parse_number("0xff"), Some(255.0));
        assert_eq!(parse_number("0XFF"), Some(255.0));
        assert_eq!(parse_number("0b1010"), Some(10.0));
        assert_eq!(parse_number("0B1111"), Some(15.0));
        assert_eq!(parse_number("1e3"), Some(1000.0));
        assert_eq!(parse_number("2.5e-2"), Some(0.025));
        assert_eq!(parse_number("1_000"), Some(1000.0));
        assert_eq!(parse_number("0xFF_FF"), Some(65535.0));
        assert_eq!(parse_number("12.5"), Some(12.5));
        assert_eq!(parse_number("0"), Some(0.0));
    }

    #[test]
    fn unescape_string_handles_full_escape_set() {
        assert_eq!(unescape_string(r#""a\nb""#), "a\nb");
        assert_eq!(unescape_string(r#""t\ta""#), "t\ta");
        assert_eq!(unescape_string(r#""r\ra""#), "r\ra");
        assert_eq!(unescape_string(r#""q\"x""#), "q\"x");
        assert_eq!(unescape_string(r#""b\\s""#), "b\\s");
        assert_eq!(unescape_string(r#""n\0e""#), "n\0e");
        assert_eq!(unescape_string(r#"'single'"#), "single");
        assert_eq!(unescape_string(r#"'\'q\''"#), "'q'");
        // Unknown escape passes through leniently (\q -> q).
        assert_eq!(unescape_string(r#""x\qy""#), "xqy");
    }

    #[test]
    fn unescape_template_chunk_strips_delims_and_escapes() {
        // Full template `` `...` ``.
        assert_eq!(unescape_template_chunk("`plain`"), "plain");
        // Start chunk `` `a${ ``.
        assert_eq!(unescape_template_chunk("`a${"), "a");
        // Middle chunk `}b${`.
        assert_eq!(unescape_template_chunk("}b${"), "b");
        // End chunk `` }c` ``.
        assert_eq!(unescape_template_chunk("}c`"), "c");
        // Empty leading/middle chunks.
        assert_eq!(unescape_template_chunk("`${"), "");
        assert_eq!(unescape_template_chunk("}${"), "");
        // Template escapes: \` -> ` and \$ -> $, plus the shared set.
        assert_eq!(unescape_template_chunk("`a\\`b`"), "a`b");
        assert_eq!(unescape_template_chunk("`a\\$b`"), "a$b");
        assert_eq!(unescape_template_chunk("`a\\nb`"), "a\nb");
    }

    #[test]
    fn compiles_template_emits_template_op() {
        let chunk = compile_source("`hi ${1}!`").expect("compiles");
        let text = disasm(&chunk);
        assert!(text.contains("TEMPLATE"), "missing TEMPLATE op in:\n{text}");
    }

    async fn eval_string(src: &str) -> String {
        match crate::vm_eval_source(src).await.expect("evaluates") {
            Value::Str(s) => s.to_string(),
            other => panic!("expected Str, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn template_interpolation_evaluates() {
        assert_eq!(eval_string("`hi ${1+2}!`").await, "hi 3!");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn template_coerces_non_strings() {
        assert_eq!(eval_string("`b=${true} n=${42}`").await, "b=true n=42");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hex_literal_evaluates() {
        assert_eq!(eval_number("0xff").await, 255.0);
    }
}
