//! Canonical AST pretty-printer.
//!
//! `format_source` lexes and parses `src`, then renders the resulting AST back
//! to canonical, idempotent source: 2-space indentation, one statement per
//! line, no trailing semicolons, spaced binary operators. The output always
//! re-parses to an equivalent program, and `format(format(x)) == format(x)`.

use crate::ast::{
    ArrayElem, ArrowBody, BinOp, CallArg, EnumVariantDecl, Expr, ExprKind, FieldDecl, ImportNames,
    MatchArm, MethodDecl, ObjEntry, Param, Pattern, Stmt, TemplatePart, Type, UnOp,
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
        if p.rest {
            out.push_str("...");
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
        Stmt::Let {
            name,
            ty,
            value,
            mutable,
            ..
        } => {
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
        Stmt::LetDestructure {
            names,
            rest,
            value,
            mutable,
            ..
        } => {
            indent(out, level);
            out.push_str(if *mutable { "let " } else { "const " });
            out.push('[');
            let mut parts: Vec<String> = names.clone();
            if let Some((rest_name, _)) = rest {
                parts.push(format!("...{rest_name}"));
            }
            out.push_str(&parts.join(", "));
            out.push_str("] = ");
            write_expr(out, value, 0);
            out.push('\n');
        }
        Stmt::LetDestructureObject {
            bindings,
            rest,
            value,
            mutable,
            ..
        } => {
            indent(out, level);
            out.push_str(if *mutable { "let " } else { "const " });
            out.push('{');
            let mut parts: Vec<String> = bindings
                .iter()
                .map(|b| {
                    let key = object_key(&b.key);
                    if b.binding == b.key {
                        key
                    } else {
                        format!("{key} as {}", b.binding)
                    }
                })
                .collect();
            if let Some((rest_name, _)) = rest {
                parts.push(format!("...{rest_name}"));
            }
            out.push_str(&parts.join(", "));
            out.push_str("} = ");
            write_expr(out, value, 0);
            out.push('\n');
        }
        Stmt::Block(body) => {
            indent(out, level);
            write_block(out, body, level);
            out.push('\n');
        }
        Stmt::If {
            cond,
            then_branch,
            else_branch,
        } => {
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
        Stmt::ForRange {
            var,
            start,
            end,
            inclusive,
            step,
            body,
        } => {
            // RANGES FEATURE, Phase 1. The parser DOES set `inclusive`/`step`
            // (since commit a91dfdc); this render emits the `..=`/`step` surface
            // faithfully (parsed now; evaluated in a later phase).
            indent(out, level);
            out.push_str("for (");
            out.push_str(var);
            out.push_str(" in ");
            write_expr(out, start, 0);
            out.push_str(if *inclusive { "..=" } else { ".." });
            write_expr(out, end, 0);
            if let Some(k) = step {
                out.push_str(" step ");
                write_expr(out, k, 0);
            }
            out.push_str(") ");
            write_block(out, body, level);
            out.push('\n');
        }
        Stmt::ForOf {
            var,
            iter,
            body,
            for_await,
        } => {
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
        Stmt::Fn {
            name,
            params,
            ret,
            body,
            is_async,
            is_generator,
            is_worker,
            ..
        } => {
            indent(out, level);
            if *is_worker {
                out.push_str("worker ");
            }
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
        Stmt::Class {
            name,
            superclass,
            fields,
            methods,
            is_worker,
            ..
        } => {
            indent(out, level);
            if *is_worker {
                out.push_str("worker ");
            }
            out.push_str("class ");
            out.push_str(name);
            if let Some(sup) = superclass {
                out.push_str(" extends ");
                out.push_str(sup);
            }
            out.push_str(" {\n");
            for fd in fields {
                write_field(out, fd, level + 1);
            }
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
    // ADT: a payload variant renders its declared field list — `Circle(radius: float)`
    // (named) or `Pair(int, int)` (positional). Mutually exclusive with `= value`.
    if !v.payload.is_empty() {
        out.push('(');
        for (i, field) in v.payload.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            if let Some(name) = &field.name {
                out.push_str(name);
                out.push_str(": ");
            }
            out.push_str(&render_type(&field.ty));
        }
        out.push(')');
    }
    out.push_str(",\n");
}

fn write_field(out: &mut String, fd: &FieldDecl, level: usize) {
    indent(out, level);
    out.push_str(&fd.name);
    out.push_str(": ");
    out.push_str(&render_type(&fd.ty)); // Type::Optional renders as `T?` (canonical)
    if let Some(def) = &fd.default {
        out.push_str(" = ");
        write_expr(out, def, PREC_ASSIGN);
    }
    out.push('\n');
}

fn write_method(out: &mut String, m: &MethodDecl, level: usize) {
    indent(out, level);
    // Canonical modifier order: `static? worker? async? fn` (SP1 §3, Spec A workers).
    if m.is_static {
        out.push_str("static ");
    }
    if m.is_worker {
        out.push_str("worker ");
    }
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
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::InstanceOf => 5,
        // Bitwise-OR tier (NUM §3.4): tighter than comparison, looser than range.
        BinOp::BitOr | BinOp::BitXor => 6,
        // Range binds looser than additive but tighter than bitor
        // (grammar PREC.range, between bitor and add).
        BinOp::Range => 7,
        // Additive: `+ -` and wrapping `+% -%`.
        BinOp::Add | BinOp::Sub | BinOp::WrapAdd | BinOp::WrapSub => 8,
        // Multiplicative: `* / %`, wrapping `*%`, shifts `<< >>`, bitwise `&`.
        BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::WrapMul | BinOp::Shl | BinOp::Shr
        | BinOp::BitAnd => 9,
        BinOp::Pow => 10,
    }
}

// A precedence floor for a context. An expression whose own precedence is below
// `min_prec` must be parenthesized.
const PREC_ASSIGN: u8 = 0;
const PREC_UNARY: u8 = 11;
const PREC_POSTFIX: u8 = 12;
const PREC_ATOM: u8 = 13;

/// The natural precedence of an expression (how tightly it binds as a whole).
fn expr_prec(e: &Expr) -> u8 {
    match &e.kind {
        ExprKind::Assign { .. } => PREC_ASSIGN,
        // The ternary binds just above assignment (looser than every binary op),
        // so it shares the lowest tier here.
        ExprKind::Ternary { .. } => PREC_ASSIGN,
        ExprKind::Arrow { .. } => PREC_ASSIGN,
        ExprKind::Binary { op, .. } => bin_prec(*op),
        // `ExprKind::Range` (emitted by the parser since commit a91dfdc) mirrors
        // the legacy `BinOp::Range` precedence.
        ExprKind::Range { .. } => bin_prec(BinOp::Range),
        ExprKind::Unary { .. } => PREC_UNARY,
        ExprKind::Await(_) => PREC_UNARY,
        // `yield` binds as loosely as assignment (it captures a full expression).
        ExprKind::Yield(_) => PREC_ASSIGN,
        ExprKind::Call { .. }
        | ExprKind::Index { .. }
        | ExprKind::Member { .. }
        | ExprKind::OptMember { .. }
        | ExprKind::Try(_)
        | ExprKind::Unwrap(_) => PREC_POSTFIX,
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
        ExprKind::Int(n) => out.push_str(&n.to_string()),
        ExprKind::Float(n) => out.push_str(&format_float_literal(*n)),
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
                match a {
                    CallArg::Pos(x) => write_expr(out, x, PREC_ASSIGN),
                    CallArg::Spread(x) => {
                        out.push_str("...");
                        write_expr(out, x, PREC_ASSIGN);
                    }
                    CallArg::Named { name, value } => {
                        out.push_str(name);
                        out.push_str(": ");
                        write_expr(out, value, PREC_ASSIGN);
                    }
                }
            }
            out.push(')');
        }
        ExprKind::Assign { target, value } => {
            write_expr(out, target, PREC_POSTFIX);
            out.push_str(" = ");
            write_expr(out, value, PREC_ASSIGN);
        }
        ExprKind::Arrow {
            params,
            body,
            is_async,
            ..
        } => {
            if *is_async {
                out.push_str("async ");
            }
            // Single un-annotated param renders without parens (`x => …`);
            // anything else uses the parenthesized form.
            if params.len() == 1 && params[0].ty.is_none() && !params[0].rest {
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
                match it {
                    ArrayElem::Item(x) => write_expr(out, x, PREC_ASSIGN),
                    ArrayElem::Spread(x) => {
                        out.push_str("...");
                        write_expr(out, x, PREC_ASSIGN);
                    }
                }
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
                for (i, e) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    match e {
                        ObjEntry::KV(k, v) => {
                            out.push_str(&object_key(k));
                            out.push_str(": ");
                            write_expr(out, v, PREC_ASSIGN);
                        }
                        ObjEntry::Spread(x) => {
                            out.push_str("...");
                            write_expr(out, x, PREC_ASSIGN);
                        }
                    }
                }
                out.push_str(" }");
            }
        }
        ExprKind::Map(entries) => {
            if entries.is_empty() {
                out.push_str("#{}");
            } else {
                out.push_str("#{ ");
                for (i, e) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    // The map-key is an arbitrary expression (NOT the object-key
                    // quoting logic): format it as an expression.
                    write_expr(out, &e.key, PREC_ASSIGN);
                    out.push_str(": ");
                    write_expr(out, &e.value, PREC_ASSIGN);
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
            write_expr(out, inner, PREC_UNARY);
            out.push('?');
        }
        ExprKind::Unwrap(inner) => {
            write_expr(out, inner, PREC_UNARY);
            out.push('!');
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
        // RANGES FEATURE, Phase 1. The parser DOES emit `ExprKind::Range` (since
        // commit a91dfdc), so this is the live value-range render path. It mirrors
        // the legacy `BinOp::Range` operand precedence and adds the `..=`/`step`
        // surface (parsed now; evaluated in a later phase).
        ExprKind::Range {
            start,
            end,
            inclusive,
            step,
        } => {
            let p = bin_prec(BinOp::Range);
            write_expr(out, start, p);
            out.push_str(if *inclusive { "..=" } else { ".." });
            write_expr(out, end, p + 1);
            if let Some(k) = step {
                out.push_str(" step ");
                write_expr(out, k, PREC_ASSIGN);
            }
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
    // Minimal-but-source-faithful pattern render (Phase 8a). Value/Range sub-
    // expressions go through the formatter's own `write_expr` (NOT the AST debug
    // `Display`) so the output reparses. A fully idempotent pass is Phase 8c.
    for (i, p) in arm.patterns.iter().enumerate() {
        if i > 0 {
            out.push_str(" | ");
        }
        write_pattern(out, p);
    }
    if let Some(guard) = &arm.guard {
        out.push_str(" if ");
        write_expr(out, guard, PREC_ASSIGN);
    }
    out.push_str(" => ");
    write_expr(out, &arm.body, PREC_ASSIGN);
}

fn write_pattern(out: &mut String, pat: &Pattern) {
    match pat {
        Pattern::Wildcard => out.push('_'),
        Pattern::Ident(n) => out.push_str(n),
        Pattern::Value(e) => write_expr(out, e, PREC_ASSIGN),
        Pattern::Range {
            start,
            end,
            inclusive,
            step,
        } => {
            // Renders the inclusive boundary (`..=`) and the optional `step`
            // clause, both of which the parser now populates (a `step`-less
            // pattern simply leaves `step` `None`).
            write_expr(out, start, PREC_ASSIGN);
            out.push_str(if *inclusive { "..=" } else { ".." });
            write_expr(out, end, PREC_ASSIGN);
            if let Some(k) = step {
                out.push_str(" step ");
                write_expr(out, k, PREC_ASSIGN);
            }
        }
        Pattern::Array(pats, rest) => {
            out.push('[');
            for (i, p) in pats.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_pattern(out, p);
            }
            write_pattern_rest(out, rest, !pats.is_empty());
            out.push(']');
        }
        Pattern::Object(entries, rest) => {
            out.push('{');
            for (i, e) in entries.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&object_key(e.key.as_ref()));
                if let Some(p) = &e.pat {
                    out.push_str(": ");
                    write_pattern(out, p);
                }
            }
            write_pattern_rest(out, rest, !entries.is_empty());
            out.push('}');
        }
        Pattern::Variant {
            enum_name,
            variant,
            fields,
        } => {
            if let Some(en) = enum_name {
                out.push_str(en);
                out.push('.');
            }
            out.push_str(variant);
            out.push('(');
            match fields {
                crate::ast::VariantPatFields::Positional(pats) => {
                    for (i, p) in pats.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        write_pattern(out, p);
                    }
                }
                crate::ast::VariantPatFields::Named(entries) => {
                    for (i, (k, p)) in entries.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        out.push_str(k);
                        if let Some(p) = p {
                            out.push_str(": ");
                            write_pattern(out, p);
                        }
                    }
                }
            }
            out.push(')');
        }
    }
}

fn write_pattern_rest(
    out: &mut String,
    rest: &Option<Option<std::rc::Rc<str>>>,
    needs_comma: bool,
) {
    match rest {
        None => {}
        Some(name) => {
            if needs_comma {
                out.push_str(", ");
            }
            out.push_str("...");
            if let Some(n) = name {
                out.push_str(n);
            }
        }
    }
}

fn op_str(op: UnOp) -> String {
    op.to_string()
}

fn op_str_bin(op: BinOp) -> String {
    op.to_string()
}

/// Object keys that are valid identifiers stay bare; others are quoted.
fn object_key(k: &str) -> String {
    if crate::token::is_ident_like(k) {
        k.to_string()
    } else {
        format!("\"{}\"", escape_str_lit(k))
    }
}

/// Render an `f64` the way the lexer would tokenize it back to the same value:
/// integers without a decimal point, others via Rust's shortest round-trip.
/// Render a `float` literal so it round-trips back through the lexer AS A FLOAT
/// (NUM §3.1/§4): an integral float must keep a fractional part (`5.0`, not `5`),
/// otherwise re-lexing it would produce an `int`. Non-finite values keep Rust's
/// `inf`/`-inf`/`NaN` rendering (they are not literals, but the formatter never
/// emits them in practice — a literal is always finite).
fn format_float_literal(n: f64) -> String {
    if n.fract() == 0.0 && n.is_finite() {
        format!("{}.0", n)
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
        let src =
            "let   x=1+2\nfn f(a,b){return a+b}\nif(x>2){print(\"big\")}else{print(\"small\")}";
        let once = format_source(src).unwrap();
        let twice = format_source(&once).unwrap();
        assert_eq!(once, twice, "fmt must be idempotent");
        // re-parses to an equivalent program (no parse error)
        assert!(crate::parser::parse(&crate::lexer::lex(&once).unwrap()).is_ok());
    }

    #[test]
    fn bitwise_and_wrapping_operators_round_trip() {
        // NUM §3.2/§3.4: the legacy formatter renders the new operators and is
        // idempotent, and the precedence renumbering keeps `(a&b)==c` parenthesis-free.
        for src in [
            "let a = 0xFF & 0b1010\n",
            "let b = (1 << 16) | 256\n",
            "let c = ~0\n",
            "let d = 5 +% 3\n",
            "let e = x -% y *% z\n",
            "let f = a ^ b >> 1\n",
            // Go precedence: `(a & b) == c` needs NO parens (bitwise binds tighter).
            "a & b == c\n",
            "a | b == c\n",
        ] {
            let once = format_source(src).unwrap();
            let twice = format_source(&once).unwrap();
            assert_eq!(once, twice, "fmt must be idempotent for {src:?}: {once:?}");
            // Re-parses cleanly on the legacy front-end.
            assert!(
                crate::parser::parse(&crate::lexer::lex(&once).unwrap()).is_ok(),
                "re-format must re-parse for {src:?}: {once:?}"
            );
        }
        // The bitwise-vs-equality footgun must NOT gain parentheses (Go binding).
        assert_eq!(format_source("a & b == c").unwrap(), "a & b == c\n");
        assert_eq!(format_source("a | b == c").unwrap(), "a | b == c\n");
    }

    #[test]
    fn int_and_float_literals_round_trip() {
        // NUM §3.1/§4: an int renders without a decimal; a float ALWAYS keeps a
        // fractional part so it re-lexes as a float (not an int).
        assert_eq!(format_source("5").unwrap(), "5\n");
        assert_eq!(format_source("5.0").unwrap(), "5.0\n");
        assert_eq!(format_source("1.5").unwrap(), "1.5\n");
        // An integral float must NOT collapse to an int literal on re-format.
        let once = format_source("let x = 5.0").unwrap();
        assert!(once.contains("5.0"), "float must keep its .0, got: {once}");
        let twice = format_source(&once).unwrap();
        assert_eq!(once, twice, "float formatting must be idempotent");
        // And the re-formatted float re-lexes as a Float, not an Int.
        use crate::token::Tok;
        let toks = crate::lexer::lex("5.0").unwrap();
        assert!(matches!(toks[0].tok, Tok::Float(_)));
        let toks2 = crate::lexer::lex(&format_source("5.0").unwrap()).unwrap();
        assert!(matches!(toks2[0].tok, Tok::Float(_)));
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
    fn fmt_spread_roundtrips() {
        // Arrays and calls round-trip exactly; object literals canonicalize to
        // the spaced `{ ... }` form (matching all other object output).
        let cases = [
            ("let a = [...x, 1]\n", "let a = [...x, 1]\n"),
            ("let o = {...x, k: 1}\n", "let o = { ...x, k: 1 }\n"),
            ("f(...args, 2)\n", "f(...args, 2)\n"),
        ];
        for (src, expected) in cases {
            let out = format_source(src).unwrap();
            assert_eq!(out, expected, "fmt mismatch for: {src}");
            // and idempotent
            assert_eq!(format_source(&out).unwrap(), expected);
        }
    }

    #[test]
    fn formats_canonically() {
        let out = format_source("let x=1").unwrap();
        assert_eq!(out, "let x = 1\n");
    }

    #[test]
    fn unwrap_and_await_format_without_parens() {
        // `await x!` and `await x?` are canonical (no parens) — `!`/`?` are
        // looser than await, so the grouping is implicit. A binary inner still
        // keeps its parens. `format_source` emits the statement-expression plus
        // a trailing newline, so we compare against that.
        assert_eq!(format_source("await f()!").unwrap(), "await f()!\n");
        assert_eq!(format_source("await f()?").unwrap(), "await f()?\n");
        assert_eq!(format_source("a! + b").unwrap(), "a! + b\n");
        assert_eq!(format_source("(a + b)!").unwrap(), "(a + b)!\n");
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
    fn class_fields_format_canonically() {
        // `name?: T` normalizes to `name: T?`; fields print before methods.
        let src = "class U {\n  id: number\n  nick?: string\n  role: string = \"guest\"\n  fn init() {}\n}\n";
        let want = "class U {\n  id: number\n  nick: string?\n  role: string = \"guest\"\n  fn init() {\n  }\n}\n";
        assert_eq!(format_source(src).unwrap(), want);
        // idempotent
        let once = format_source(src).unwrap();
        assert_eq!(format_source(&once).unwrap(), once);
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
    fn fmt_object_destructuring_escapes_quotes_in_key() {
        // A non-identifier object key containing a `"` must be emitted with the
        // quote escaped so the formatted output re-lexes to the same key.
        let src = "let {\"a\\\"b\" as x} = obj\n";
        assert_eq!(format_source(src).unwrap(), src);
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
            let once =
                format_source(&src).unwrap_or_else(|e| panic!("fmt failed on {:?}: {}", path, e));
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

    #[test]
    fn array_rest_destructuring_round_trips() {
        let src = "let [a, ...rest] = xs\n";
        assert_eq!(format_source(src).unwrap(), src);
    }

    #[test]
    fn object_rest_destructuring_round_trips() {
        let src = "let {a, ...rest} = obj\n";
        assert_eq!(format_source(src).unwrap(), src);
    }

    #[test]
    fn rest_param_round_trips() {
        let src = "fn f(a, ...rest) {\n  return rest\n}\n";
        assert_eq!(format_source(src).unwrap(), src);
        let src2 = "fn f(...rest: array<number>) {\n  return rest\n}\n";
        assert_eq!(format_source(src2).unwrap(), src2);
        let src3 = "let f = (a, ...rest) => rest\n";
        assert_eq!(format_source(src3).unwrap(), src3);
    }

    // ---- Phase 8c: match-pattern idempotence tests ----

    /// Helper: assert `format_source(src)` produces `expected`, is idempotent,
    /// and the output re-parses without error.
    fn assert_fmt_idempotent(src: &str, expected: &str) {
        let once = format_source(src).unwrap_or_else(|e| panic!("fmt failed on {:?}: {e}", src));
        assert_eq!(once, expected, "wrong output for: {src}");
        let twice =
            format_source(&once).unwrap_or_else(|e| panic!("re-fmt failed on {:?}: {e}", &once));
        assert_eq!(once, twice, "fmt not idempotent for: {src}");
        // formatted output re-parses
        let tokens = crate::lexer::lex(&once)
            .unwrap_or_else(|e| panic!("lex failed on formatted {:?}: {e}", &once));
        assert!(
            crate::parser::parse(&tokens).is_ok(),
            "formatted output does not re-parse: {:?}",
            &once
        );
    }

    #[test]
    fn match_wildcard_and_value_are_idempotent() {
        // Wildcard `_` and bare value patterns format canonically and re-parse.
        assert_fmt_idempotent("match n { _ => 0 }", "match n { _ => 0 }\n");
        assert_fmt_idempotent(
            "match n { 0 => \"zero\", 1 => \"one\", _ => \"other\" }",
            "match n { 0 => \"zero\", 1 => \"one\", _ => \"other\" }\n",
        );
    }

    #[test]
    fn match_bare_ident_binding_is_idempotent() {
        // A bare-ident pattern (Option-C binding) formats as just the identifier.
        assert_fmt_idempotent("match x { other => other }", "match x { other => other }\n");
    }

    #[test]
    fn match_range_patterns_are_idempotent() {
        // Inclusive `..=` and exclusive `..` range patterns.
        assert_fmt_idempotent(
            "match n { 1..=9 => \"single\", 10..100 => \"double\", _ => \"big\" }",
            "match n { 1..=9 => \"single\", 10..100 => \"double\", _ => \"big\" }\n",
        );
    }

    #[test]
    fn match_array_patterns_are_idempotent() {
        // Fixed-arity, rest, binding mixed with value pattern.
        assert_fmt_idempotent(
            "match xs { [] => \"empty\", [x] => x, [first, ...rest] => first }",
            "match xs { [] => \"empty\", [x] => x, [first, ...rest] => first }\n",
        );
        // Explicit nil value inside array pattern.
        assert_fmt_idempotent(
            "match pair { [u, nil] => u, [_, e] => e }",
            "match pair { [u, nil] => u, [_, e] => e }\n",
        );
        // Ignore-rest `...` with no name.
        assert_fmt_idempotent(
            "match xs { [h, ...] => h, _ => nil }",
            "match xs { [h, ...] => h, _ => nil }\n",
        );
    }

    #[test]
    fn match_object_patterns_are_idempotent() {
        // Shorthand binding `{key}` and sub-pattern `{key: pat}`.
        assert_fmt_idempotent(
            "match req { {method, path} => method, _ => \"?\" }",
            "match req { {method, path} => method, _ => \"?\" }\n",
        );
        assert_fmt_idempotent(
            "match user { {role: \"admin\"} => true, {role: r} => false }",
            "match user { {role: \"admin\"} => true, {role: r} => false }\n",
        );
        // Object rest `...name`.
        assert_fmt_idempotent(
            "match obj { {a, ...rest} => rest, _ => nil }",
            "match obj { {a, ...rest} => rest, _ => nil }\n",
        );
        // Object ignore-rest `...`.
        assert_fmt_idempotent(
            "match obj { {x, ...} => x, _ => nil }",
            "match obj { {x, ...} => x, _ => nil }\n",
        );
    }

    #[test]
    fn match_guard_is_idempotent() {
        // `_ if <guard>` — guard expression survives formatting and round-trips.
        assert_fmt_idempotent(
            "match n { _ if n < 0 => \"neg\", _ => \"pos\" }",
            "match n { _ if n < 0 => \"neg\", _ => \"pos\" }\n",
        );
        // Guard on a binding pattern.
        assert_fmt_idempotent(
            "match n { x if x > 10 => x, _ => 0 }",
            "match n { x if x > 10 => x, _ => 0 }\n",
        );
    }

    #[test]
    fn match_or_patterns_are_idempotent() {
        // `|` alternatives in a single arm.
        assert_fmt_idempotent(
            "match day { \"sat\" | \"sun\" => true, _ => false }",
            "match day { \"sat\" | \"sun\" => true, _ => false }\n",
        );
    }

    #[test]
    fn match_nested_in_fn_is_idempotent() {
        // A full match expression inside a function body — verifies the fmt pass
        // handles statement+block nesting alongside match.
        let src = "fn classify(n) {\n  return match n {\n    _ if n < 0 => \"negative\",\n    0 => \"zero\",\n    1..=9 => \"single digit\",\n    10..100 => \"double digit\",\n    _ => \"big\",\n  }\n}\n";
        // Canonical output (the example file's shape).
        let once = format_source(src).unwrap();
        let twice = format_source(&once).unwrap();
        assert_eq!(once, twice, "fmt not idempotent for match-in-fn");
        assert!(crate::parser::parse(&crate::lexer::lex(&once).unwrap()).is_ok());
    }

    #[test]
    fn match_pattern_matching_example_is_idempotent() {
        // The committed pattern_matching.as example must round-trip through the
        // formatter (the `all_examples_format_idempotently_and_reparse` test covers
        // this too, but having it here makes the failure message explicit).
        let src = std::fs::read_to_string("examples/pattern_matching.as")
            .expect("examples/pattern_matching.as should exist");
        let once = format_source(&src).unwrap();
        let twice = format_source(&once).unwrap();
        assert_eq!(once, twice, "pattern_matching.as fmt is not idempotent");
        assert!(crate::parser::parse(&crate::lexer::lex(&once).unwrap()).is_ok());
    }

    #[test]
    fn formats_worker_modifier_canonical_order() {
        // Free worker fn: canonical order is `worker fn`.
        assert_eq!(
            format_source("worker fn   f( )  { return 1 }").unwrap(),
            "worker fn f() {\n  return 1\n}\n"
        );
        // Static worker method in a class: canonical order is `static worker fn`.
        assert_eq!(
            format_source("class C { static   worker   fn h(x){return x} }").unwrap(),
            "class C {\n  static worker fn h(x) {\n    return x\n  }\n}\n"
        );
    }

    #[test]
    fn worker_fmt_is_idempotent() {
        let once = format_source("worker fn f() { return 1 }").unwrap();
        assert_eq!(format_source(&once).unwrap(), once);
    }

    // ---- Spec B Task 3: worker class + worker fn* (legacy AST formatter) ----

    #[test]
    fn formats_worker_class_canonical() {
        // `worker` prefix is preserved and extra whitespace normalised.
        assert_eq!(
            format_source("worker  class  Db{fn f(){return 1}}").unwrap(),
            "worker class Db {\n  fn f() {\n    return 1\n  }\n}\n"
        );
    }

    #[test]
    fn worker_class_fmt_is_idempotent() {
        let once = format_source("worker  class  Db{fn f(){return 1}}").unwrap();
        assert_eq!(format_source(&once).unwrap(), once, "worker class not idempotent");
    }

    #[test]
    fn formats_worker_fn_star_canonical() {
        // `worker fn*` — both modifiers preserved; `*` attaches to `fn`.
        assert_eq!(
            format_source("worker fn*  g(){yield 1}").unwrap(),
            "worker fn* g() {\n  yield 1\n}\n"
        );
    }

    #[test]
    fn worker_fn_star_fmt_is_idempotent() {
        let once = format_source("worker fn*  g(){yield 1}").unwrap();
        assert_eq!(format_source(&once).unwrap(), once, "worker fn* not idempotent");
    }

    #[test]
    fn formats_worker_method_in_worker_class() {
        // A `worker class` body with a `worker fn` method inside — both modifiers.
        let src = "worker class Db { worker fn  run(x){return x} }";
        let out = format_source(src).unwrap();
        assert!(out.starts_with("worker class Db {"), "missing 'worker class Db': {out}");
        assert!(out.contains("worker fn run"), "missing 'worker fn run': {out}");
        assert_eq!(format_source(&out).unwrap(), out, "not idempotent");
    }
}
