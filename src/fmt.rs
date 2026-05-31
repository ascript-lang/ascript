//! Canonical AST pretty-printer.
//!
//! `format_source` lexes and parses `src`, then renders the resulting AST back
//! to canonical, idempotent source: 2-space indentation, one statement per
//! line, no trailing semicolons, spaced binary operators. The output always
//! re-parses to an equivalent program, and `format(format(x)) == format(x)`.

use crate::ast::{
    ArrowBody, BinOp, EnumVariantDecl, Expr, ExprKind, ImportNames, MatchArm, MethodDecl, Param,
    Stmt, TemplatePart, Type, UnOp,
};
use crate::error::AsError;

/// Lex → parse → render canonical source.
pub fn format_source(src: &str) -> Result<String, AsError> {
    let tokens = crate::lexer::lex(src)?;
    let program = crate::parser::parse(&tokens)?;
    let mut out = String::new();
    for stmt in &program {
        write_stmt(&mut out, stmt, 0);
    }
    Ok(out)
}

/// Two spaces per indent level.
fn indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

/// Render a `{ … }` block of statements at `level` (the brace sits at the end
/// of the current line; the body is indented one deeper; the closing brace is
/// at `level`).
fn write_block(out: &mut String, body: &[Stmt], level: usize) {
    out.push_str("{\n");
    for stmt in body {
        write_stmt(out, stmt, level + 1);
    }
    indent(out, level);
    out.push('}');
}

fn write_params(out: &mut String, params: &[Param]) {
    out.push('(');
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&p.name);
        if let Some(ty) = &p.ty {
            out.push_str(": ");
            out.push_str(&render_type(ty));
        }
    }
    out.push(')');
}

fn render_type(ty: &Type) -> String {
    // The `Type` Display impl already produces canonical type syntax.
    ty.to_string()
}

fn write_stmt(out: &mut String, stmt: &Stmt, level: usize) {
    match stmt {
        Stmt::Expr(e) => {
            indent(out, level);
            write_expr(out, e, 0);
            out.push('\n');
        }
        Stmt::Let { name, ty, value, mutable, .. } => {
            indent(out, level);
            out.push_str(if *mutable { "let " } else { "const " });
            out.push_str(name);
            if let Some(ty) = ty {
                out.push_str(": ");
                out.push_str(&render_type(ty));
            }
            // `let x` / `let x: T` may have no initializer.
            if let Some(value) = value {
                out.push_str(" = ");
                write_expr(out, value, 0);
            }
            out.push('\n');
        }
        Stmt::LetDestructure { names, value, mutable, .. } => {
            indent(out, level);
            out.push_str(if *mutable { "let " } else { "const " });
            out.push('[');
            out.push_str(&names.join(", "));
            out.push_str("] = ");
            write_expr(out, value, 0);
            out.push('\n');
        }
        Stmt::Block(body) => {
            indent(out, level);
            write_block(out, body, level);
            out.push('\n');
        }
        Stmt::If { cond, then_branch, else_branch } => {
            indent(out, level);
            out.push_str("if (");
            write_expr(out, cond, 0);
            out.push_str(") ");
            write_block(out, then_branch, level);
            if let Some(else_branch) = else_branch {
                out.push_str(" else ");
                // `else if` chains: a single nested If renders inline rather
                // than wrapped in a block.
                if let [Stmt::If { .. }] = else_branch.as_slice() {
                    let mut inner = String::new();
                    write_stmt(&mut inner, &else_branch[0], 0);
                    out.push_str(inner.trim_end_matches('\n'));
                } else {
                    write_block(out, else_branch, level);
                }
            }
            out.push('\n');
        }
        Stmt::While { cond, body } => {
            indent(out, level);
            out.push_str("while (");
            write_expr(out, cond, 0);
            out.push_str(") ");
            write_block(out, body, level);
            out.push('\n');
        }
        Stmt::ForRange { var, start, end, body } => {
            indent(out, level);
            out.push_str("for (");
            out.push_str(var);
            out.push_str(" in ");
            write_expr(out, start, 0);
            out.push_str("..");
            write_expr(out, end, 0);
            out.push_str(") ");
            write_block(out, body, level);
            out.push('\n');
        }
        Stmt::ForOf { var, iter, body, for_await } => {
            indent(out, level);
            // `for await (x in e)` for async iteration; plain `for (x of e)`
            // otherwise. The async form uses `in` (matching the parser, which
            // accepts both `in`/`of` and only distinguishes by the `await`).
            if *for_await {
                out.push_str("for await (");
                out.push_str(var);
                out.push_str(" in ");
            } else {
                out.push_str("for (");
                out.push_str(var);
                out.push_str(" of ");
            }
            write_expr(out, iter, 0);
            out.push_str(") ");
            write_block(out, body, level);
            out.push('\n');
        }
        Stmt::Return(value) => {
            indent(out, level);
            out.push_str("return");
            if let Some(v) = value {
                out.push(' ');
                write_expr(out, v, 0);
            }
            out.push('\n');
        }
        Stmt::Break => {
            indent(out, level);
            out.push_str("break\n");
        }
        Stmt::Continue => {
            indent(out, level);
            out.push_str("continue\n");
        }
        Stmt::Fn { name, params, ret, body, is_async, is_generator, .. } => {
            indent(out, level);
            if *is_async {
                out.push_str("async ");
            }
            out.push_str(if *is_generator { "fn* " } else { "fn " });
            out.push_str(name);
            write_params(out, params);
            if let Some(ret) = ret {
                out.push_str(": ");
                out.push_str(&render_type(ret));
            }
            out.push(' ');
            write_block(out, body, level);
            out.push('\n');
        }
        Stmt::Enum { name, variants, .. } => {
            indent(out, level);
            out.push_str("enum ");
            out.push_str(name);
            out.push_str(" {\n");
            for v in variants {
                write_enum_variant(out, v, level + 1);
            }
            indent(out, level);
            out.push_str("}\n");
        }
        Stmt::Class { name, superclass, methods, .. } => {
            indent(out, level);
            out.push_str("class ");
            out.push_str(name);
            if let Some(sup) = superclass {
                out.push_str(" extends ");
                out.push_str(sup);
            }
            out.push_str(" {\n");
            for m in methods {
                write_method(out, m, level + 1);
            }
            indent(out, level);
            out.push_str("}\n");
        }
        Stmt::Import { names, source } => {
            indent(out, level);
            out.push_str("import ");
            match names {
                ImportNames::Named(names) => {
                    out.push_str("{ ");
                    out.push_str(&names.join(", "));
                    out.push_str(" }");
                }
                ImportNames::Namespace(alias) => {
                    out.push_str("* as ");
                    out.push_str(alias);
                }
            }
            out.push_str(" from \"");
            out.push_str(source);
            out.push_str("\"\n");
        }
        Stmt::Export(inner) => {
            // Render the inner declaration, then prepend `export ` to its first
            // (indented) line.
            let mut inner_str = String::new();
            write_stmt(&mut inner_str, inner, level);
            let pad = "  ".repeat(level);
            indent(out, level);
            out.push_str("export ");
            out.push_str(inner_str.trim_start_matches(pad.as_str()));
        }
    }
}

fn write_enum_variant(out: &mut String, v: &EnumVariantDecl, level: usize) {
    indent(out, level);
    out.push_str(&v.name);
    if let Some(value) = &v.value {
        out.push_str(" = ");
        write_expr(out, value, 0);
    }
    out.push_str(",\n");
}

fn write_method(out: &mut String, m: &MethodDecl, level: usize) {
    indent(out, level);
    if m.is_async {
        out.push_str("async ");
    }
    out.push_str(if m.is_generator { "fn* " } else { "fn " });
    out.push_str(&m.name);
    write_params(out, &m.params);
    if let Some(ret) = &m.ret {
        out.push_str(": ");
        out.push_str(&render_type(ret));
    }
    out.push(' ');
    write_block(out, &m.body, level);
    out.push('\n');
}

// Precedence levels, lowest binds loosest. Used to decide when a binary
// sub-expression needs parentheses so the output re-parses with the same
// structure.
fn bin_prec(op: BinOp) -> u8 {
    match op {
        BinOp::Or => 1,
        BinOp::And => 2,
        BinOp::Coalesce => 3,
        BinOp::Eq | BinOp::Ne => 4,
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 5,
        // Range binds looser than additive but tighter than comparison
        // (grammar PREC.range = 7, between compare = 6 and add = 8).
        BinOp::Range => 6,
        BinOp::Add | BinOp::Sub => 7,
        BinOp::Mul | BinOp::Div | BinOp::Mod => 8,
        BinOp::Pow => 9,
    }
}

// A precedence floor for a context. An expression whose own precedence is below
// `min_prec` must be parenthesized.
const PREC_ASSIGN: u8 = 0;
const PREC_UNARY: u8 = 10;
const PREC_POSTFIX: u8 = 11;
const PREC_ATOM: u8 = 12;

/// The natural precedence of an expression (how tightly it binds as a whole).
fn expr_prec(e: &Expr) -> u8 {
    match &e.kind {
        ExprKind::Assign { .. } => PREC_ASSIGN,
        // The ternary binds just above assignment (looser than every binary op),
        // so it shares the lowest tier here.
        ExprKind::Ternary { .. } => PREC_ASSIGN,
        ExprKind::Arrow { .. } => PREC_ASSIGN,
        ExprKind::Binary { op, .. } => bin_prec(*op),
        ExprKind::Unary { .. } => PREC_UNARY,
        ExprKind::Await(_) => PREC_UNARY,
        // `yield` binds as loosely as assignment (it captures a full expression).
        ExprKind::Yield(_) => PREC_ASSIGN,
        ExprKind::Call { .. }
        | ExprKind::Index { .. }
        | ExprKind::Member { .. }
        | ExprKind::OptMember { .. }
        | ExprKind::Try(_) => PREC_POSTFIX,
        // A `Paren` node already emits exactly one set of parens in
        // `write_expr_inner`, so it must be treated as an atom here to prevent
        // `write_expr` from wrapping it in a second, redundant set (which would
        // accumulate `Paren(Paren(...))` layers on every `fmt` pass).
        ExprKind::Paren(_) => PREC_ATOM,
        _ => PREC_ATOM,
    }
}

/// Write `e`, wrapping it in parentheses if its precedence is below `min_prec`.
fn write_expr(out: &mut String, e: &Expr, min_prec: u8) {
    if expr_prec(e) < min_prec {
        out.push('(');
        write_expr_inner(out, e);
        out.push(')');
    } else {
        write_expr_inner(out, e);
    }
}

fn write_expr_inner(out: &mut String, e: &Expr) {
    match &e.kind {
        ExprKind::Number(n) => out.push_str(&format_number(*n)),
        ExprKind::Str(s) => {
            out.push('"');
            out.push_str(&escape_str_lit(s));
            out.push('"');
        }
        ExprKind::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        ExprKind::Nil => out.push_str("nil"),
        ExprKind::Ident(name) => out.push_str(name),
        ExprKind::Unary { op, expr } => {
            out.push_str(&op_str(*op));
            // Operand binds at least as tight as a unary operand.
            write_expr(out, expr, PREC_UNARY);
        }
        ExprKind::Await(expr) => {
            out.push_str("await ");
            // Operand binds at least as tight as a unary operand.
            write_expr(out, expr, PREC_UNARY);
        }
        ExprKind::Yield(operand) => match operand {
            Some(e) => {
                out.push_str("yield ");
                write_expr(out, e, PREC_ASSIGN);
            }
            None => out.push_str("yield"),
        },
        ExprKind::Binary { op, lhs, rhs } => {
            let p = bin_prec(*op);
            // Left-associative for all ops except Pow (right-associative): a
            // safe, idempotent rule is to require the left side to bind at `p`
            // and the right side at `p + 1` (so same-precedence right operands
            // are parenthesized). Slightly over-parenthesizes right-nested
            // chains but stays correct and idempotent.
            let (lp, rp) = match op {
                BinOp::Pow => (p + 1, p),
                _ => (p, p + 1),
            };
            write_expr(out, lhs, lp);
            // Range uses the no-space idiom `a..b`; all other binary operators
            // are spaced.
            if let BinOp::Range = op {
                out.push_str("..");
            } else {
                out.push(' ');
                out.push_str(&op_str_bin(*op));
                out.push(' ');
            }
            write_expr(out, rhs, rp);
        }
        ExprKind::Call { callee, args } => {
            write_expr(out, callee, PREC_POSTFIX);
            out.push('(');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_expr(out, a, PREC_ASSIGN);
            }
            out.push(')');
        }
        ExprKind::Assign { target, value } => {
            write_expr(out, target, PREC_POSTFIX);
            out.push_str(" = ");
            write_expr(out, value, PREC_ASSIGN);
        }
        ExprKind::Arrow { params, body, is_async, .. } => {
            if *is_async {
                out.push_str("async ");
            }
            // Single un-annotated param renders without parens (`x => …`);
            // anything else uses the parenthesized form.
            if params.len() == 1 && params[0].ty.is_none() {
                out.push_str(&params[0].name);
            } else {
                write_params(out, params);
            }
            out.push_str(" => ");
            match body.as_ref() {
                ArrowBody::Expr(e) => write_expr(out, e, PREC_ASSIGN),
                ArrowBody::Block(body) => write_block(out, body, 0),
            }
        }
        ExprKind::Array(items) => {
            out.push('[');
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_expr(out, it, PREC_ASSIGN);
            }
            out.push(']');
        }
        ExprKind::Index { object, index } => {
            write_expr(out, object, PREC_POSTFIX);
            out.push('[');
            write_expr(out, index, PREC_ASSIGN);
            out.push(']');
        }
        ExprKind::Object(entries) => {
            if entries.is_empty() {
                out.push_str("{}");
            } else {
                out.push_str("{ ");
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&object_key(k));
                    out.push_str(": ");
                    write_expr(out, v, PREC_ASSIGN);
                }
                out.push_str(" }");
            }
        }
        ExprKind::Member { object, name } => {
            write_expr(out, object, PREC_POSTFIX);
            out.push('.');
            out.push_str(name);
        }
        ExprKind::OptMember { object, name } => {
            write_expr(out, object, PREC_POSTFIX);
            out.push_str("?.");
            out.push_str(name);
        }
        ExprKind::Try(inner) => {
            write_expr(out, inner, PREC_POSTFIX);
            out.push('?');
        }
        ExprKind::Ternary { cond, then, els } => {
            // `cond` and `then` bind one tier above the ternary so a nested
            // ternary there is parenthesized; the right-associative `els` does
            // not need parentheses for a chained ternary.
            write_expr(out, cond, PREC_ASSIGN + 1);
            out.push_str(" ? ");
            write_expr(out, then, PREC_ASSIGN + 1);
            out.push_str(" : ");
            write_expr(out, els, PREC_ASSIGN);
        }
        ExprKind::Template { parts } => {
            out.push('`');
            for part in parts {
                match part {
                    TemplatePart::Lit(s) => out.push_str(&escape_template_lit(s)),
                    TemplatePart::Expr(e) => {
                        out.push_str("${");
                        write_expr(out, e, PREC_ASSIGN);
                        out.push('}');
                    }
                }
            }
            out.push('`');
        }
        ExprKind::Match { subject, arms } => {
            out.push_str("match ");
            write_expr(out, subject, PREC_ASSIGN);
            out.push_str(" { ");
            for (i, arm) in arms.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_match_arm(out, arm);
            }
            out.push_str(" }");
        }
        ExprKind::Paren(inner) => {
            // The parser keeps explicit parens only to break optional chains.
            // Re-emit them so semantics (and idempotence) are preserved.
            out.push('(');
            write_expr_inner(out, inner);
            out.push(')');
        }
    }
}

fn write_match_arm(out: &mut String, arm: &MatchArm) {
    match &arm.patterns {
        None => out.push('_'),
        Some(pats) => {
            for (i, p) in pats.iter().enumerate() {
                if i > 0 {
                    out.push_str(" | ");
                }
                write_expr(out, p, PREC_ASSIGN);
            }
        }
    }
    out.push_str(" => ");
    write_expr(out, &arm.body, PREC_ASSIGN);
}

fn op_str(op: UnOp) -> String {
    op.to_string()
}

fn op_str_bin(op: BinOp) -> String {
    op.to_string()
}

/// Object keys that are valid identifiers stay bare; others are quoted.
fn object_key(k: &str) -> String {
    let is_ident = !k.is_empty()
        && k.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false)
        && k.chars().all(|c| c.is_alphanumeric() || c == '_');
    if is_ident {
        k.to_string()
    } else {
        format!("\"{}\"", k)
    }
}

/// Render an `f64` the way the lexer would tokenize it back to the same value:
/// integers without a decimal point, others via Rust's shortest round-trip.
fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.is_finite() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

/// Re-escape template literal text so it round-trips through the lexer (which
/// unescapes `\` `` ` `` `$` `\n` `\t`).
/// Re-escape a string value for emission inside a double-quoted literal, so it
/// round-trips through the lexer's escape handling (see `lexer::escape_char`).
fn escape_str_lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_template_lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_is_idempotent() {
        let src = "let   x=1+2\nfn f(a,b){return a+b}\nif(x>2){print(\"big\")}else{print(\"small\")}";
        let once = format_source(src).unwrap();
        let twice = format_source(&once).unwrap();
        assert_eq!(once, twice, "fmt must be idempotent");
        // re-parses to an equivalent program (no parse error)
        assert!(crate::parser::parse(&crate::lexer::lex(&once).unwrap()).is_ok());
    }

    #[test]
    fn paren_in_operand_is_idempotent() {
        for src in [
            "(a + b) * c",
            "a * (b + c)",
            "-(a + b)",
            "((1 + 2)) * 3",
            "(a?.b).c",
            "f((a + b))",
        ] {
            let once = format_source(src).unwrap();
            let twice = format_source(&once).unwrap();
            assert_eq!(once, twice, "not idempotent for: {src}");
            // and it still re-parses
            assert!(crate::parser::parse(&crate::lexer::lex(&once).unwrap()).is_ok());
        }
        // a single explicit paren group renders as exactly one set
        assert_eq!(format_source("(a + b) * c").unwrap(), "(a + b) * c\n");
    }

    #[test]
    fn formats_canonically() {
        let out = format_source("let x=1").unwrap();
        assert_eq!(out, "let x = 1\n");
    }

    #[test]
    fn optional_type_round_trips() {
        // `T?` survives a format pass unchanged in let/param/return positions.
        let src = "let x: number? = nil\n";
        assert_eq!(format_source(src).unwrap(), src);
        let src2 = "fn f(a: string?): number? {\n  return nil\n}\n";
        assert_eq!(format_source(src2).unwrap(), src2);
    }

    #[test]
    fn formats_ternary() {
        // Canonical spacing.
        assert_eq!(format_source("let x=a?b:c").unwrap(), "let x = a ? b : c\n");
        // Right-associative chain renders without redundant parentheses…
        assert_eq!(
            format_source("let x = a ? b : c ? d : e").unwrap(),
            "let x = a ? b : c ? d : e\n"
        );
        // …but a ternary used as the condition keeps its parentheses.
        assert_eq!(
            format_source("let x = (a ? b : c) ? d : e").unwrap(),
            "let x = (a ? b : c) ? d : e\n"
        );
        // Idempotent + still parses for several shapes (incl. a postfix Try nearby).
        for src in [
            "a ? b : c",
            "cond ? -1 : 1",
            "a ? b : c ? d : e",
            "f(x > 0 ? \"pos\" : \"neg\")",
            "let v = ok ? data : fallback",
        ] {
            let once = format_source(src).unwrap();
            let twice = format_source(&once).unwrap();
            assert_eq!(once, twice, "fmt not idempotent for: {src}");
            assert!(crate::parser::parse(&crate::lexer::lex(&once).unwrap()).is_ok());
        }
    }

    #[test]
    fn re_escapes_string_literals() {
        // A string literal carrying special chars must be emitted with escapes
        // so the formatted output re-lexes to the same value (round-trips).
        let out = format_source("print(\"a\\\"b\\tc\\nd\\\\e\")").unwrap();
        assert_eq!(out, "print(\"a\\\"b\\tc\\nd\\\\e\")\n");
        // idempotent and re-parses
        let twice = format_source(&out).unwrap();
        assert_eq!(out, twice);
    }

    #[test]
    fn formats_array_destructuring() {
        let out = format_source("let [a, b] = pair").unwrap();
        assert_eq!(out, "let [a, b] = pair\n");
    }

    #[test]
    fn formats_future_type_annotation() {
        // A `future<T>` binding annotation round-trips through the formatter.
        // (Space before `=` so the lexer does not read `>=` as a single token.)
        assert_eq!(
            format_source("let x:future<number> = f()").unwrap(),
            "let x: future<number> = f()\n"
        );
        // Nested generic and idempotence.
        let once = format_source("let y: future<array<number>> = g()").unwrap();
        assert_eq!(once, "let y: future<array<number>> = g()\n");
        assert_eq!(format_source(&once).unwrap(), once);
    }

    #[test]
    fn formats_generators_yield_and_for_await() {
        // fn* / async fn* / yield / yield <expr> / for await all render canonically.
        assert_eq!(
            format_source("fn*count(){yield 1\nyield 2}").unwrap(),
            "fn* count() {\n  yield 1\n  yield 2\n}\n"
        );
        assert_eq!(
            format_source("async fn* g(){yield x}").unwrap(),
            "async fn* g() {\n  yield x\n}\n"
        );
        assert_eq!(
            format_source("fn* e(){let a=yield\nlet b=yield 5}").unwrap(),
            "fn* e() {\n  let a = yield\n  let b = yield 5\n}\n"
        );
        assert_eq!(
            format_source("for await(x in gen()){print(x)}").unwrap(),
            "for await (x in gen()) {\n  print(x)\n}\n"
        );
        // Generator method.
        assert_eq!(
            format_source("class C{fn* g(){yield 1}}").unwrap(),
            "class C {\n  fn* g() {\n    yield 1\n  }\n}\n"
        );
        // Idempotence + re-parse for every shape.
        for src in [
            "fn* count() { yield 1 }",
            "async fn* g() { yield x * 2 }",
            "for await (x in g) { yield x }",
            "fn* e() { let a = yield 1 }",
            "class C { fn* g() { yield 1 } }",
        ] {
            let once = format_source(src).unwrap();
            let twice = format_source(&once).unwrap();
            assert_eq!(once, twice, "fmt not idempotent for: {src}");
            assert!(crate::parser::parse(&crate::lexer::lex(&once).unwrap()).is_ok());
        }
    }

    /// Every committed example must format idempotently and the formatted
    /// output must re-parse — proving all AST variants are handled.
    #[test]
    fn all_examples_format_idempotently_and_reparse() {
        let mut paths: Vec<std::path::PathBuf> = Vec::new();
        for dir in ["examples", "examples/modules"] {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().and_then(|e| e.to_str()) == Some("as") {
                    paths.push(path);
                }
            }
        }
        assert!(!paths.is_empty(), "no example files found");
        for path in paths {
            let src = std::fs::read_to_string(&path).unwrap();
            let once = format_source(&src)
                .unwrap_or_else(|e| panic!("fmt failed on {:?}: {}", path, e));
            let twice = format_source(&once)
                .unwrap_or_else(|e| panic!("re-fmt failed on {:?}: {}", path, e));
            assert_eq!(once, twice, "fmt not idempotent on {:?}", path);
            // formatted output re-parses without error
            let tokens = crate::lexer::lex(&once)
                .unwrap_or_else(|e| panic!("lex failed on formatted {:?}: {}", path, e));
            assert!(
                crate::parser::parse(&tokens).is_ok(),
                "formatted {:?} does not re-parse",
                path
            );
        }
    }
}
