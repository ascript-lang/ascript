//! Hand-written recursive-descent parser. Operates over the NON-trivia tokens
//! (trivia is skipped for grammar decisions and re-inserted by the tree builder)
//! and emits a `Vec<Event>` plus a list of `ParseError`s. Never aborts: on error
//! it emits an `Error` event and recovers, so it always yields a tree.

use crate::syntax::event::{Event, TOMBSTONE};
use crate::syntax::kind::SyntaxKind;
use crate::syntax::lexer::{lex, LexToken};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    /// Index into the *non-trivia* token list where the error occurred.
    pub token_index: usize,
}

pub struct Parse {
    pub events: Vec<Event>,
    pub errors: Vec<ParseError>,
    /// The full token stream (incl. trivia), needed by the tree builder.
    pub tokens: Vec<LexToken>,
}

struct Parser {
    tokens: Vec<LexToken>,
    /// Indices (into `tokens`) of the non-trivia tokens, in order.
    nontrivia: Vec<usize>,
    /// Cursor into `nontrivia`.
    pos: usize,
    events: Vec<Event>,
    errors: Vec<ParseError>,
}

/// A pending open node. `complete` sets its kind; if dropped uncompleted it
/// stays a Tombstone and is skipped.
struct Marker {
    pos: usize, // index into events of the Start
    completed: bool,
}

struct CompletedMarker {
    pos: usize, // index into events of the Start
}

impl Parser {
    fn new(src: &str) -> Self {
        let tokens = lex(src);
        let nontrivia = tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.kind.is_trivia())
            .map(|(i, _)| i)
            .collect();
        Parser { tokens, nontrivia, pos: 0, events: Vec::new(), errors: Vec::new() }
    }

    fn current(&self) -> SyntaxKind {
        match self.nontrivia.get(self.pos) {
            Some(&ti) => self.tokens[ti].kind,
            None => SyntaxKind::Error,
        }
    }

    fn at(&self, kind: SyntaxKind) -> bool {
        self.current() == kind
    }

    fn at_end(&self) -> bool {
        self.pos >= self.nontrivia.len()
    }

    fn start(&mut self) -> Marker {
        let pos = self.events.len();
        self.events.push(Event::Start { kind: TOMBSTONE, forward_parent: None });
        Marker { pos, completed: false }
    }

    fn bump(&mut self) {
        let kind = self.current();
        if !self.at_end() {
            self.events.push(Event::Token { kind });
            self.pos += 1;
        }
    }

    fn complete(&mut self, mut m: Marker, kind: SyntaxKind) -> CompletedMarker {
        m.completed = true;
        if let Event::Start { kind: slot, .. } = &mut self.events[m.pos] {
            *slot = kind;
        }
        self.events.push(Event::Finish);
        CompletedMarker { pos: m.pos }
    }

    fn precede(&mut self, cm: &CompletedMarker) -> Marker {
        let new_pos = self.events.len();
        self.events.push(Event::Start { kind: TOMBSTONE, forward_parent: None });
        if let Event::Start { forward_parent, .. } = &mut self.events[cm.pos] {
            *forward_parent = Some(new_pos);
        }
        Marker { pos: new_pos, completed: false }
    }

    fn error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.errors.push(ParseError { message: message.clone(), token_index: self.pos });
        self.events.push(Event::Error { message });
    }
}

/// Parse `src` into events + errors + the token stream.
pub fn parse(src: &str) -> Parse {
    let mut p = Parser::new(src);
    let m = p.start();
    while !p.at_end() {
        let before = p.pos;
        stmt(&mut p);
        if p.pos == before {
            p.error("unexpected token");
            p.bump();
        }
    }
    p.complete(m, SyntaxKind::SourceFile);
    Parse { events: p.events, errors: p.errors, tokens: p.tokens }
}

fn stmt(p: &mut Parser) {
    use SyntaxKind::*;
    match p.current() {
        LetKw | ConstKw => let_stmt(p),
        IfKw => if_stmt(p),
        WhileKw => while_stmt(p),
        ReturnKw => return_stmt(p),
        FnKw => fn_decl(p),
        LBrace => {
            block(p);
        }
        _ => expr_stmt(p),
    }
}

fn expr_stmt(p: &mut Parser) {
    let m = p.start();
    let lhs_cm = expr_returning(p);
    if p.at(SyntaxKind::Eq) {
        let am = p.precede(&lhs_cm);
        p.bump(); // =
        expr(p);
        p.complete(am, SyntaxKind::AssignExpr);
    }
    p.complete(m, SyntaxKind::ExprStmt);
}

/// Like `expr` but returns the CompletedMarker so callers can wrap it (assignment).
fn expr_returning(p: &mut Parser) -> CompletedMarker {
    let cm = lhs(p);
    let mut lhs_cm = cm;
    loop {
        let op = p.current();
        let Some((_l_bp, r_bp)) = infix_binding_power(op) else { break };
        let m = p.precede(&lhs_cm);
        p.bump();
        expr_bp(p, r_bp);
        lhs_cm = p.complete(m, SyntaxKind::BinaryExpr);
    }
    // Ternary tail: cond ? then : els  (right-assoc; then/els are full exprs).
    if p.at(SyntaxKind::Question) && ternary_ahead(p) {
        let m = p.precede(&lhs_cm);
        p.bump(); // ?
        expr(p); // then
        if p.at(SyntaxKind::Colon) {
            p.bump();
            expr(p); // els
        } else {
            p.error("expected ':' in ternary");
        }
        lhs_cm = p.complete(m, SyntaxKind::TernaryExpr);
    }
    lhs_cm
}

fn let_stmt(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // let/const
    if p.at(Ident) {
        p.bump();
    } else {
        p.error("expected a name after let/const");
    }
    if p.at(Eq) {
        p.bump();
        expr(p);
    }
    p.complete(m, LetStmt);
}

fn block(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // {
    while !p.at(RBrace) && !p.at_end() {
        let before = p.pos;
        stmt(p);
        if p.pos == before {
            p.error("unexpected token in block");
            p.bump();
        }
    }
    if p.at(RBrace) {
        p.bump();
    } else {
        p.error("expected '}' to close block");
    }
    p.complete(m, Block)
}

fn if_stmt(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // if
    // AScript requires parentheses around the condition: `if (cond) { ... }`
    if p.at(LParen) {
        p.bump(); // (
        expr(p); // condition
        if p.at(RParen) {
            p.bump(); // )
        } else {
            p.error("expected ')' after if condition");
        }
    } else {
        p.error("expected '(' before if condition");
        expr(p); // recover by parsing expr anyway
    }
    if p.at(LBrace) {
        block(p);
    } else {
        p.error("expected '{' after if condition");
    }
    if p.at(ElseKw) {
        p.bump();
        if p.at(IfKw) {
            if_stmt(p); // else if
        } else if p.at(LBrace) {
            block(p);
        } else {
            p.error("expected '{' or 'if' after else");
        }
    }
    p.complete(m, IfStmt);
}

fn while_stmt(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // while
    // AScript requires parentheses around the condition: `while (cond) { ... }`
    if p.at(LParen) {
        p.bump(); // (
        expr(p); // condition
        if p.at(RParen) {
            p.bump(); // )
        } else {
            p.error("expected ')' after while condition");
        }
    } else {
        p.error("expected '(' before while condition");
        expr(p); // recover by parsing expr anyway
    }
    if p.at(LBrace) {
        block(p);
    } else {
        p.error("expected '{' after while condition");
    }
    p.complete(m, WhileStmt);
}

fn return_stmt(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // return
    if !p.at(RBrace) && !p.at_end() {
        expr(p);
    }
    p.complete(m, ReturnStmt);
}

fn fn_decl(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // fn
    if p.at(Ident) {
        p.bump();
    } else {
        p.error("expected function name");
    }
    if p.at(LParen) {
        param_list(p);
    } else {
        p.error("expected '(' after function name");
    }
    if p.at(LBrace) {
        block(p);
    } else {
        p.error("expected '{' for function body");
    }
    p.complete(m, FnDecl);
}

fn param_list(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // (
    while !p.at(RParen) && !p.at_end() {
        let pm = p.start();
        if p.at(Ident) {
            p.bump();
        } else {
            p.error("expected parameter name");
        }
        p.complete(pm, Param);
        if p.at(Comma) {
            p.bump();
        } else {
            break;
        }
    }
    if p.at(RParen) {
        p.bump();
    } else {
        p.error("expected ')' to close parameters");
    }
    p.complete(m, ParamList);
}

/// Infix binding powers (left, right). Higher binds tighter.
fn infix_binding_power(kind: SyntaxKind) -> Option<(u8, u8)> {
    use SyntaxKind::*;
    Some(match kind {
        PipePipe => (1, 2),
        AmpAmp => (3, 4),
        EqEq | BangEq => (5, 6),
        Lt | Le | Gt | Ge => (7, 8),
        Plus | Minus => (9, 10),
        Star | Slash | Percent => (11, 12),
        StarStar => (16, 15), // right-assoc
        _ => return None,
    })
}

fn expr(p: &mut Parser) {
    let _ = expr_returning(p);
}

fn expr_bp(p: &mut Parser, min_bp: u8) {
    let mut lhs = lhs(p);
    loop {
        let op = p.current();
        let Some((l_bp, r_bp)) = infix_binding_power(op) else { break };
        if l_bp < min_bp {
            break;
        }
        let m = p.precede(&lhs);
        p.bump(); // operator
        expr_bp(p, r_bp);
        lhs = p.complete(m, SyntaxKind::BinaryExpr);
    }
}

/// True if the current token can begin an expression (for optional operands
/// like `yield`).
fn can_start_expr(p: &Parser) -> bool {
    use SyntaxKind::*;
    matches!(
        p.current(),
        Number | Str | TrueKw | FalseKw | NilKw | Ident | LParen | LBracket
            | LBrace | Minus | Bang | TemplateStr | TemplateStart | AwaitKw | YieldKw
    )
}

/// Unary/primary layer: prefix `-`/`!x`, `await`, `yield`, then a primary
/// with its tight postfix chain (call/member/index/?.).
fn unary(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    match p.current() {
        Minus | Bang => {
            let m = p.start();
            p.bump();
            let _operand = unary(p);
            p.complete(m, UnaryExpr)
        }
        AwaitKw => {
            let m = p.start();
            p.bump(); // await
            let _operand = unary(p);
            p.complete(m, AwaitExpr)
        }
        YieldKw => {
            let m = p.start();
            p.bump(); // yield
            if can_start_expr(p) {
                let _ = unary(p);
            }
            p.complete(m, YieldExpr)
        }
        _ => primary(p),
    }
}

/// True if the `?` at the cursor is a ternary `?` (a `:` follows at bracket-depth
/// 0 before the statement ends), false if it is a postfix propagate `?`.
fn ternary_ahead(p: &Parser) -> bool {
    use SyntaxKind::*;
    let mut depth = 0i32;
    let mut i = p.pos + 1; // scan AFTER the `?`
    while i < p.nontrivia.len() {
        match p.tokens[p.nontrivia[i]].kind {
            LParen | LBracket | LBrace => depth += 1,
            RParen | RBracket => depth -= 1,
            RBrace => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            Semicolon if depth == 0 => return false,
            Colon if depth == 0 => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// The unwrap tier — looser than unary, tighter than binary. Applies postfix
/// propagate `?` (when NOT a ternary) and force-unwrap `!` over the whole unary
/// expression, so `await x!` parses as `(await x)!`.
fn unwrap_tier(p: &mut Parser, mut cm: CompletedMarker) -> CompletedMarker {
    use SyntaxKind::*;
    loop {
        match p.current() {
            Question if !ternary_ahead(p) => {
                let m = p.precede(&cm);
                p.bump(); // ?
                cm = p.complete(m, TryExpr);
            }
            Bang => {
                let m = p.precede(&cm);
                p.bump(); // !
                cm = p.complete(m, UnwrapExpr);
            }
            _ => break,
        }
    }
    cm
}

/// Operand of the binary precedence-climb: unary, then the unwrap tier.
fn lhs(p: &mut Parser) -> CompletedMarker {
    let u = unary(p);
    unwrap_tier(p, u)
}

fn primary(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let cm = match p.current() {
        Number | Str | TrueKw | FalseKw | NilKw => {
            let m = p.start();
            p.bump();
            p.complete(m, Literal)
        }
        Ident => {
            let m = p.start();
            p.bump();
            p.complete(m, NameRef)
        }
        LParen if is_arrow_ahead(p) => {
            let m = p.start();
            param_list(p);
            p.bump(); // =>  (guaranteed by is_arrow_ahead)
            if p.at(LBrace) {
                block(p);
            } else {
                expr(p);
            }
            p.complete(m, ArrowExpr)
        }
        LParen => {
            let m = p.start();
            p.bump(); // (
            expr(p);
            if p.at(RParen) {
                p.bump();
            } else {
                p.error("expected ')'");
            }
            p.complete(m, ParenExpr)
        }
        LBracket => array_expr(p),
        LBrace => object_expr(p),
        TemplateStr => {
            let m = p.start();
            p.bump();
            p.complete(m, TemplateExpr)
        }
        TemplateStart => template_expr(p),
        _ => {
            let m = p.start();
            p.error("expected expression");
            p.complete(m, Error)
        }
    };
    postfix(p, cm)
}

fn postfix(p: &mut Parser, mut cm: CompletedMarker) -> CompletedMarker {
    use SyntaxKind::*;
    loop {
        match p.current() {
            LParen => {
                let m = p.precede(&cm);
                arg_list(p);
                cm = p.complete(m, CallExpr);
            }
            Dot => {
                let m = p.precede(&cm);
                p.bump(); // .
                if p.at(Ident) {
                    p.bump();
                } else {
                    p.error("expected property name after '.'");
                }
                cm = p.complete(m, MemberExpr);
            }
            LBracket => {
                let m = p.precede(&cm);
                p.bump(); // [
                expr(p);
                if p.at(RBracket) {
                    p.bump();
                } else {
                    p.error("expected ']'");
                }
                cm = p.complete(m, IndexExpr);
            }
            QuestionDot => {
                let m = p.precede(&cm);
                p.bump(); // ?.
                if p.at(Ident) {
                    p.bump();
                } else {
                    p.error("expected property name after '?.'");
                }
                cm = p.complete(m, OptMemberExpr);
            }
            _ => break,
        }
    }
    cm
}

/// True if the `(` at the cursor begins an arrow parameter list, i.e. the
/// matching `)` is immediately followed by `=>`.
fn is_arrow_ahead(p: &Parser) -> bool {
    use SyntaxKind::*;
    let mut depth = 0i32;
    let mut i = p.pos;
    while i < p.nontrivia.len() {
        match p.tokens[p.nontrivia[i]].kind {
            LParen => depth += 1,
            RParen => {
                depth -= 1;
                if depth == 0 {
                    return matches!(
                        p.nontrivia.get(i + 1).map(|&ti| p.tokens[ti].kind),
                        Some(FatArrow)
                    );
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

fn spread_elem(p: &mut Parser) {
    let m = p.start();
    p.bump(); // ...
    expr(p);
    p.complete(m, SyntaxKind::SpreadElem);
}

fn array_expr(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // [
    while !p.at(RBracket) && !p.at_end() {
        if p.at(DotDotDot) {
            spread_elem(p);
        } else {
            expr(p);
        }
        if p.at(Comma) {
            p.bump();
        } else {
            break;
        }
    }
    if p.at(RBracket) {
        p.bump();
    } else {
        p.error("expected ']' to close array");
    }
    p.complete(m, ArrayExpr)
}

fn object_expr(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // {
    while !p.at(RBrace) && !p.at_end() {
        if p.at(DotDotDot) {
            spread_elem(p);
        } else {
            let fm = p.start();
            if p.at(Ident) || p.at(Str) {
                p.bump();
            } else {
                p.error("expected object key");
            }
            if p.at(Colon) {
                p.bump();
                expr(p);
            } else {
                p.error("expected ':' after object key");
            }
            p.complete(fm, ObjectField);
        }
        if p.at(Comma) {
            p.bump();
        } else {
            break;
        }
    }
    if p.at(RBrace) {
        p.bump();
    } else {
        p.error("expected '}' to close object");
    }
    p.complete(m, ObjectExpr)
}

/// Parse an interpolated template: TemplateStart (expr TemplateMiddle)* expr
/// TemplateEnd. Each `${...}` slot holds a full expression.
fn template_expr(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // TemplateStart  (`...${)
    loop {
        expr(p); // interpolated expression
        if p.at(TemplateMiddle) {
            p.bump(); // }...${  → another interpolation follows
            continue;
        }
        if p.at(TemplateEnd) {
            p.bump(); // }...`
            break;
        }
        p.error("unterminated template interpolation");
        break;
    }
    p.complete(m, TemplateExpr)
}

fn arg_list(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // (
    while !p.at(RParen) && !p.at_end() {
        if p.at(DotDotDot) {
            spread_elem(p);
        } else {
            expr(p);
        }
        if p.at(Comma) {
            p.bump();
        } else {
            break;
        }
    }
    if p.at(RParen) {
        p.bump();
    } else {
        p.error("expected ')' to close arguments");
    }
    p.complete(m, ArgList);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_kinds(src: &str) -> Vec<SyntaxKind> {
        parse(src)
            .events
            .into_iter()
            .filter_map(|e| match e {
                Event::Start { kind, .. } if kind != crate::syntax::event::TOMBSTONE => Some(kind),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn parses_a_number_statement() {
        assert_eq!(
            node_kinds("42"),
            vec![SyntaxKind::SourceFile, SyntaxKind::ExprStmt, SyntaxKind::Literal]
        );
        assert!(parse("42").errors.is_empty());
    }

    #[test]
    fn unexpected_token_recovers_not_panics() {
        let p = parse("+");
        assert!(!p.errors.is_empty(), "should record an error");
        assert!(matches!(p.events.first(), Some(Event::Start { kind: SyntaxKind::SourceFile, .. })));
    }

    fn tree_shape(src: &str) -> Vec<SyntaxKind> {
        use crate::syntax::cst::ResolvedNode;
        fn walk(n: &ResolvedNode, out: &mut Vec<SyntaxKind>) {
            out.push(n.kind());
            for c in n.children() {
                walk(c, out);
            }
        }
        let node = crate::syntax::tree_builder::build_tree(parse(src));
        let mut out = Vec::new();
        walk(&node, &mut out);
        out
    }

    #[test]
    fn precedence_groups_multiply_under_add() {
        // 1 + 2 * 3 => Binary(+) { Literal, Binary(*) { Literal, Literal } }
        let shape = tree_shape("1 + 2 * 3");
        assert_eq!(
            shape,
            vec![
                SyntaxKind::SourceFile, SyntaxKind::ExprStmt,
                SyntaxKind::BinaryExpr,
                SyntaxKind::Literal,
                SyntaxKind::BinaryExpr,
                SyntaxKind::Literal, SyntaxKind::Literal,
            ]
        );
        assert!(parse("1 + 2 * 3").errors.is_empty());
    }

    #[test]
    fn unary_and_paren() {
        assert_eq!(
            tree_shape("-(1)"),
            vec![
                SyntaxKind::SourceFile, SyntaxKind::ExprStmt,
                SyntaxKind::UnaryExpr, SyntaxKind::ParenExpr, SyntaxKind::Literal,
            ]
        );
    }

    #[test]
    fn name_reference() {
        assert_eq!(
            tree_shape("x"),
            vec![SyntaxKind::SourceFile, SyntaxKind::ExprStmt, SyntaxKind::NameRef]
        );
    }

    #[test]
    fn let_statement() {
        assert_eq!(
            tree_shape("let x = 1"),
            vec![SyntaxKind::SourceFile, SyntaxKind::LetStmt, SyntaxKind::Literal]
        );
        assert!(parse("let x = 1").errors.is_empty());
    }

    #[test]
    fn if_else_with_block() {
        // AScript requires parentheses around the condition: `if (cond) { ... }`
        let p = parse("if (x) { return 1 } else { return 2 }");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let shape = tree_shape("if (x) { return 1 } else { return 2 }");
        assert!(shape.contains(&SyntaxKind::IfStmt));
        assert!(shape.contains(&SyntaxKind::Block));
        assert!(shape.contains(&SyntaxKind::ReturnStmt));
    }

    #[test]
    fn while_loop() {
        // AScript requires parentheses around the condition: `while (cond) { ... }`
        assert!(parse("while (x) { x = 0 }").errors.is_empty());
        assert!(tree_shape("while (x) { x = 0 }").contains(&SyntaxKind::WhileStmt));
    }

    #[test]
    fn assignment_is_a_statement() {
        assert!(tree_shape("x = 5").contains(&SyntaxKind::AssignExpr));
    }

    #[test]
    fn fn_declaration() {
        let p = parse("fn add(a, b) { return a + b }");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let shape = tree_shape("fn add(a, b) { return a + b }");
        assert!(shape.contains(&SyntaxKind::FnDecl));
        assert!(shape.contains(&SyntaxKind::ParamList));
        assert!(shape.contains(&SyntaxKind::Param));
    }

    #[test]
    fn arrow_expression() {
        assert!(parse("let f = (x) => x + 1").errors.is_empty());
        assert!(tree_shape("let f = (x) => x + 1").contains(&SyntaxKind::ArrowExpr));
    }

    #[test]
    fn array_literal_with_spread() {
        let shape = tree_shape("[1, ...xs, 2]");
        assert!(shape.contains(&SyntaxKind::ArrayExpr));
        assert!(shape.contains(&SyntaxKind::SpreadElem));
        assert!(parse("[1, ...xs, 2]").errors.is_empty());
    }

    #[test]
    fn object_literal_with_spread() {
        let shape = tree_shape(r#"let o = { a: 1, "k": 2, ...rest }"#);
        assert!(shape.contains(&SyntaxKind::ObjectExpr));
        assert!(shape.contains(&SyntaxKind::ObjectField));
        assert!(shape.contains(&SyntaxKind::SpreadElem));
        assert!(parse(r#"let o = { a: 1, "k": 2, ...rest }"#).errors.is_empty());
    }

    #[test]
    fn plain_template() {
        let shape = tree_shape("`hello`");
        assert!(shape.contains(&SyntaxKind::TemplateExpr));
        assert!(parse("`hello`").errors.is_empty());
    }

    #[test]
    fn interpolated_template() {
        let p = parse("`a${x}b${y}c`");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        assert!(tree_shape("`a${x}b${y}c`").contains(&SyntaxKind::TemplateExpr));
    }

    #[test]
    fn nested_template() {
        let p = parse("`a${ `b${z}` }c`");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn optional_member() {
        let shape = tree_shape("a?.b");
        assert!(shape.contains(&SyntaxKind::OptMemberExpr));
        assert!(parse("a?.b").errors.is_empty());
    }

    #[test]
    fn try_and_unwrap_postfix() {
        assert!(tree_shape("f()?").contains(&SyntaxKind::TryExpr));
        assert!(tree_shape("g()!").contains(&SyntaxKind::UnwrapExpr));
        assert!(parse("f()?").errors.is_empty());
        assert!(parse("g()!").errors.is_empty());
    }

    #[test]
    fn ternary_basic() {
        let shape = tree_shape("a ? b : c");
        assert!(shape.contains(&SyntaxKind::TernaryExpr));
        assert!(parse("a ? b : c").errors.is_empty());
    }

    #[test]
    fn ternary_vs_propagate_disambiguation() {
        // `f()? - 1` is propagate-then-subtract (NOT a ternary): no `:` follows.
        let p = parse("f()? - 1");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        assert!(tree_shape("f()? - 1").contains(&SyntaxKind::TryExpr));
        assert!(!tree_shape("f()? - 1").contains(&SyntaxKind::TernaryExpr));
        // `a ? -b : c` IS a ternary.
        assert!(tree_shape("a ? -b : c").contains(&SyntaxKind::TernaryExpr));
    }

    #[test]
    fn await_expression() {
        assert!(tree_shape("await f()").contains(&SyntaxKind::AwaitExpr));
        assert!(parse("await f()").errors.is_empty());
        // The unwrap tier is looser than unary, so `await x?` = `(await x)?`:
        // in pre-order the TryExpr must appear BEFORE (wrap) the AwaitExpr.
        let shape = tree_shape("await x?");
        let try_idx = shape.iter().position(|k| *k == SyntaxKind::TryExpr)
            .expect("TryExpr present");
        let await_idx = shape.iter().position(|k| *k == SyntaxKind::AwaitExpr)
            .expect("AwaitExpr present");
        assert!(try_idx < await_idx, "expected (await x)? — TryExpr should wrap AwaitExpr");
    }

    #[test]
    fn yield_expression() {
        assert!(tree_shape("yield x").contains(&SyntaxKind::YieldExpr));
        assert!(tree_shape("yield").contains(&SyntaxKind::YieldExpr));
    }
}
