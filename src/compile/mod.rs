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
use crate::syntax::ast::{AstNode, BinaryExpr, Expr, Literal, ParenExpr, SourceFile, Stmt, UnaryExpr};
use crate::syntax::kind::SyntaxKind;
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

    // Run the resolver so the pipeline is wired end-to-end. V1 has no locals or
    // globals to consult, so its result is intentionally unused for now.
    let _ = resolve(&root);

    let mut compiler = Compiler { chunk: Chunk::new() };

    // V1 supports a program that is a sequence of statements whose meaningful
    // tail is an expression statement. Find the last `ExprStmt` and compile it;
    // any other (leading) statement kinds are not yet supported.
    let stmts: Vec<Stmt> = file.stmts().collect();
    let trailing = stmts.iter().rev().find_map(|s| match s {
        Stmt::ExprStmt(e) => Some(e.clone()),
        _ => None,
    });

    let Some(expr_stmt) = trailing else {
        return Err(CompileError::new(
            "V1 compiler requires a trailing expression statement",
            Span::new(0, src.len()),
        ));
    };

    // Reject any non-expression statement in the file (V1 is expression-only).
    for s in &stmts {
        if !matches!(s, Stmt::ExprStmt(_)) {
            return Err(CompileError::new(
                "statement kind not yet supported in V1 (expression-only)",
                stmt_span(s),
            ));
        }
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
}

impl Compiler {
    fn compile_expr(&mut self, expr: &Expr) -> Result<(), CompileError> {
        match expr {
            Expr::Literal(lit) => self.compile_literal(lit),
            Expr::BinaryExpr(bin) => self.compile_binary(bin),
            Expr::UnaryExpr(un) => self.compile_unary(un),
            Expr::ParenExpr(paren) => self.compile_paren(paren),
            other => Err(CompileError::new(
                "expression kind not yet supported in V1",
                node_span(other),
            )),
        }
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
                let n = parse_number(&text)
                    .ok_or_else(|| CompileError::new(format!("unsupported number literal {text:?} in V1"), span))?;
                Value::Number(n)
            }
            SyntaxKind::Str => Value::Str(Rc::from(unescape_basic(&text).as_str())),
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

/// Parse a V1 number literal: plain decimal integers and floats only. Returns
/// `None` for forms V1 does not support yet (hex/binary/scientific/underscored)
/// so the caller can raise a clear `CompileError`. V2 widens this.
fn parse_number(text: &str) -> Option<f64> {
    // V1 accepts only `[0-9]+` and `[0-9]+.[0-9]+` style decimals — reject any
    // alphabetic radix prefix, exponent, or digit separator for now.
    if text.bytes().any(|b| !(b.is_ascii_digit() || b == b'.')) {
        return None;
    }
    text.parse::<f64>().ok()
}

/// A minimal string-literal unescape for V1: strips the surrounding quotes and
/// handles the common backslash escapes (`\n \t \r \\ \" \' \0`). Templates,
/// `${…}` interpolation, and `\u`/`\x` escapes are V2.
fn unescape_basic(raw: &str) -> String {
    // Strip one matching quote on each side if present.
    let inner = strip_quotes(raw);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some('0') => out.push('\0'),
                // Unknown escape (incl. \u/\x, V2): keep the char verbatim.
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
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
}
