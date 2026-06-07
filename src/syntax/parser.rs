//! Hand-written recursive-descent parser. Operates over the NON-trivia tokens
//! (trivia is skipped for grammar decisions and re-inserted by the tree builder)
//! and emits a `Vec<Event>` plus a list of `ParseError`s. Never aborts: on error
//! it emits an `Error` event and recovers, so it always yields a tree.

use crate::syntax::event::{Event, TOMBSTONE};
use crate::syntax::kind::SyntaxKind;
use crate::syntax::lexer::{lex_with_errors, LexError, LexToken};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    /// Index into the *non-trivia* token list where the error occurred.
    pub token_index: usize,
}

pub struct Parse {
    pub events: Vec<Event>,
    pub errors: Vec<ParseError>,
    /// Lexical errors (unterminated string/template/block-comment). Indexed by
    /// FULL-token index (incl. trivia), unlike `errors` which are non-trivia
    /// grammar errors; mapped to byte ranges by `crate::check::analyze`.
    pub lex_errors: Vec<LexError>,
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
    /// Lexical errors surfaced by the lexer (full-token-indexed).
    lex_errors: Vec<LexError>,
    /// When true, a top-level bare `IDENT =>` (or `async IDENT =>` / `async (…) =>`)
    /// is NOT consumed as an arrow — the `=>` belongs to an enclosing construct
    /// (a match arm). Set only while parsing a match GUARD; cleared whenever the
    /// parser descends into a bracketed sub-expression so nested/parenthesized
    /// arrows inside the guard still parse. Mirrors the legacy parser, whose
    /// `try_arrow` is reached only at the top of `assignment()` and so never fires
    /// on a guard operand that precedes the arm's `=>`.
    suppress_arrow: bool,
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
        let (tokens, lex_errors) = lex_with_errors(src);
        let nontrivia = tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.kind.is_trivia())
            .map(|(i, _)| i)
            .collect();
        Parser {
            tokens,
            nontrivia,
            pos: 0,
            events: Vec::new(),
            errors: Vec::new(),
            lex_errors,
            suppress_arrow: false,
        }
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
        self.events.push(Event::Start {
            kind: TOMBSTONE,
            forward_parent: None,
        });
        Marker {
            pos,
            completed: false,
        }
    }

    fn bump(&mut self) {
        let kind = self.current();
        if !self.at_end() {
            self.events.push(Event::Token { kind });
            self.pos += 1;
        }
    }

    /// Consume the current token but record it under a DIFFERENT `SyntaxKind`.
    /// The tree builder pairs the event's kind with the original token's TEXT
    /// (it advances the raw-token cursor positionally), so this stays lossless.
    /// Used for contextual/soft keywords (e.g. remapping a `static` identifier
    /// to `StaticKw` in class-member-modifier position).
    fn bump_remap(&mut self, kind: SyntaxKind) {
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
        self.events.push(Event::Start {
            kind: TOMBSTONE,
            forward_parent: None,
        });
        if let Event::Start { forward_parent, .. } = &mut self.events[cm.pos] {
            *forward_parent = Some(new_pos);
        }
        Marker {
            pos: new_pos,
            completed: false,
        }
    }

    /// True if the current token is an `Ident` whose text equals `kw` (a soft
    /// keyword like `as` / `extends` / `from`, which are not reserved).
    fn at_kw(&self, kw: &str) -> bool {
        match self.nontrivia.get(self.pos) {
            Some(&ti) => self.tokens[ti].kind == SyntaxKind::Ident && self.tokens[ti].text == kw,
            None => false,
        }
    }

    fn error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.errors.push(ParseError {
            message: message.clone(),
            token_index: self.pos,
        });
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
    Parse {
        events: p.events,
        errors: p.errors,
        lex_errors: p.lex_errors,
        tokens: p.tokens,
    }
}

fn stmt(p: &mut Parser) {
    use SyntaxKind::*;
    // Skip bare semicolons — they act as statement separators (no-op statements).
    if p.at(Semicolon) {
        p.bump();
        return;
    }
    match p.current() {
        LetKw | ConstKw => let_stmt(p),
        IfKw => if_stmt(p),
        WhileKw => while_stmt(p),
        ReturnKw => return_stmt(p),
        FnKw => fn_decl(p),
        AsyncKw if is_async_fn(p) => fn_decl(p),
        // `worker fn` / `worker async fn` — contextual; `worker` is an Ident.
        Ident if at_worker_modifier(p) => fn_decl(p),
        LBrace => {
            block(p);
        }
        ForKw => for_stmt(p),
        BreakKw => {
            let m = p.start();
            p.bump();
            p.complete(m, SyntaxKind::BreakStmt);
        }
        ContinueKw => {
            let m = p.start();
            p.bump();
            p.complete(m, SyntaxKind::ContinueStmt);
        }
        EnumKw => enum_decl(p),
        ClassKw => class_decl(p),
        ImportKw => import_stmt(p),
        ExportKw => export_stmt(p),
        _ => expr_stmt(p),
    }
}

/// True if `async fn` starts here (vs an async arrow `async (`).
fn is_async_fn(p: &Parser) -> bool {
    matches!(
        p.nontrivia.get(p.pos + 1).map(|&ti| p.tokens[ti].kind),
        Some(SyntaxKind::FnKw)
    )
}

/// True if the cursor is at the contextual `worker` modifier: an `Ident` whose
/// text is `"worker"` immediately followed by `fn` or `async` (i.e. the start
/// of a `worker fn` or `worker async fn` declaration). The `worker` identifier
/// is never reserved — `let worker = 5` and `worker(x)` keep `worker` as `Ident`.
fn at_worker_modifier(p: &Parser) -> bool {
    if !p.at_kw("worker") {
        return false;
    }
    matches!(
        p.nontrivia.get(p.pos + 1).map(|&ti| p.tokens[ti].kind),
        Some(SyntaxKind::FnKw) | Some(SyntaxKind::AsyncKw)
    )
}

fn expr_stmt(p: &mut Parser) {
    // `expr_returning` now parses assignment (lowest-precedence expression), so a
    // statement-level `x = 5` already yields an `AssignExpr` here.
    let m = p.start();
    expr(p);
    p.complete(m, SyntaxKind::ExprStmt);
}

/// Like `expr` but returns the CompletedMarker so callers can wrap it (assignment).
fn expr_returning(p: &mut Parser) -> CompletedMarker {
    // SP9 §1: the CST parser re-enters `expr_returning` for every bracketed
    // sub-expression (`(`/`[`/`{`/template `${`), so a deeply nested SOURCE
    // expression (`((((…))))`) is native Rust recursion HERE — before compile/eval.
    // Grow the native stack at this funnel so parsing reaches the CST (and then the
    // compiler's `EXPR_NEST_LIMIT` cap) rather than SIGABRTing first. Synchronous,
    // so the cheap probe-and-grow `grow` suffices; inert until the stack runs low.
    crate::vm::stack::grow(|| expr_returning_inner(p))
}

fn expr_returning_inner(p: &mut Parser) -> CompletedMarker {
    let cm = lhs(p);
    let mut lhs_cm = cm;
    loop {
        let op = p.current();
        let Some((_l_bp, r_bp)) = infix_binding_power(op) else {
            break;
        };
        let m = p.precede(&lhs_cm);
        p.bump();
        expr_bp(p, r_bp);
        lhs_cm = p.complete(m, SyntaxKind::BinaryExpr);
    }
    // Range expression: `a..b` or `a..=b` (lower precedence than binary ops),
    // with an optional trailing contextual `step <expr>`.
    if p.at(SyntaxKind::DotDot) || p.at(SyntaxKind::DotDotEq) {
        let m = p.precede(&lhs_cm);
        p.bump(); // .. or ..=
                  // Optional right-hand side (a bare `..` is an open range).
        if can_start_expr(p) {
            expr_bp(p, 1); // parse rhs at lowest precedence
        }
        // Trailing contextual `step <expr>`: only when `step` directly follows a
        // range end (otherwise `step` stays a normal identifier).
        parse_range_step(p);
        lhs_cm = p.complete(m, SyntaxKind::RangeExpr);
    }
    // Ternary tail: cond ? then : els  (right-assoc; then/els are full exprs).
    if p.at(SyntaxKind::Question) && ternary_ahead(p) {
        let m = p.precede(&lhs_cm);
        p.bump(); // ?
                  // Ternary branches are full sub-expressions whose `=>` (if any) cannot be a
                  // match-arm separator, so arrows are allowed even inside a guard.
        with_arrows_allowed(p, expr); // then
        if p.at(SyntaxKind::Colon) {
            p.bump();
            with_arrows_allowed(p, expr); // els
        } else {
            p.error("expected ':' in ternary");
        }
        lhs_cm = p.complete(m, SyntaxKind::TernaryExpr);
    }
    // Assignment tail (lowest precedence, right-assoc): lhs = rhs, lhs += rhs, ...
    // Lower than the ternary tail above (matches the legacy parser, whose
    // assignment() wraps ternary()), so `a ? b : c = d` parses as `(a ? b : c) = d`.
    if matches!(
        p.current(),
        SyntaxKind::Eq
            | SyntaxKind::PlusEq
            | SyntaxKind::MinusEq
            | SyntaxKind::StarEq
            | SyntaxKind::SlashEq
    ) {
        let m = p.precede(&lhs_cm);
        p.bump(); // = / += / -= / *= / /=
        expr(p); // rhs is a full expression (right-assoc via expr -> expr_returning)
        lhs_cm = p.complete(m, SyntaxKind::AssignExpr);
    }
    lhs_cm
}

fn let_stmt(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // let/const
    match p.current() {
        LBracket => array_bind_pat(p),
        LBrace => object_bind_pat(p),
        Ident => p.bump(),
        _ => p.error("expected a name or destructuring pattern after let/const"),
    }
    if p.at(Colon) {
        p.bump();
        type_ann(p);
    }
    if p.at(Eq) {
        p.bump();
        expr(p);
    }
    p.complete(m, LetStmt);
}

fn array_bind_pat(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // [
    while !p.at(RBracket) && !p.at_end() {
        if p.at(DotDotDot) {
            rest_bind(p);
        } else {
            let e = p.start();
            if p.at(Ident) {
                p.bump();
            } else {
                p.error("expected a binding name");
            }
            p.complete(e, BindEntry);
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
        p.error("expected ']' to close destructuring pattern");
    }
    p.complete(m, ArrayBindPat);
}

fn object_bind_pat(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // {
    while !p.at(RBrace) && !p.at_end() {
        if p.at(DotDotDot) {
            rest_bind(p);
        } else {
            let e = p.start();
            if p.at(Ident) || p.at(Str) {
                p.bump();
            } else {
                p.error("expected a key in object pattern");
            }
            if p.at_kw("as") {
                p.bump(); // as
                if p.at(Ident) {
                    p.bump();
                } else {
                    p.error("expected a local name after 'as'");
                }
            }
            p.complete(e, BindEntry);
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
        p.error("expected '}' to close destructuring pattern");
    }
    p.complete(m, ObjectBindPat);
}

fn rest_bind(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // ...
    if p.at(Ident) {
        p.bump();
    } else {
        p.error("expected a name after '...'");
    }
    p.complete(m, RestBind);
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

/// Parse an optional trailing contextual `step <expr>` immediately after a
/// range's end bound. `step` is NOT a reserved word: it is recognized here ONLY
/// when it directly follows a range end, so `let step = 1` / `fn step(n)` keep
/// `step` as a normal identifier. The step expression is attached as a child of
/// the enclosing `RangeExpr` node (the caller has not yet `complete`d it), and
/// the `step` keyword token is consumed so it doesn't leak as a NameRef.
fn parse_range_step(p: &mut Parser) {
    if p.at_kw("step") {
        p.bump(); // contextual `step` keyword (an Ident token)
        expr_bp(p, 1); // step expression at the range precedence boundary
    }
}

fn for_stmt(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // for
    if p.at(AwaitKw) {
        p.bump(); // await
    }
    if p.at(LParen) {
        p.bump();
    } else {
        p.error("expected '(' after for");
    }
    if p.at(Ident) {
        p.bump(); // loop variable
    } else {
        p.error("expected loop variable");
    }
    if p.at(InKw) || p.at(OfKw) {
        p.bump();
    } else {
        p.error("expected 'in' or 'of' in for");
    }
    // iterable, or a range a..b / a..=b. Parse a (possibly binary) expression,
    // then a trailing .. / ..= makes it a RangeExpr.
    let iter = lhs(p);
    let mut iter_cm = iter;
    loop {
        let op = p.current();
        let Some((_l, r_bp)) = infix_binding_power(op) else {
            break;
        };
        let bm = p.precede(&iter_cm);
        p.bump();
        expr_bp(p, r_bp);
        iter_cm = p.complete(bm, BinaryExpr);
    }
    if p.at(DotDot) || p.at(DotDotEq) {
        let rm = p.precede(&iter_cm);
        p.bump(); // .. or ..=
        expr(p); // range end
                 // Trailing contextual `step <expr>` after the range end.
        parse_range_step(p);
        p.complete(rm, RangeExpr);
    }
    if p.at(RParen) {
        p.bump();
    } else {
        p.error("expected ')' to close for header");
    }
    if p.at(LBrace) {
        block(p);
    } else {
        p.error("expected '{' for loop body");
    }
    p.complete(m, ForStmt);
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
    // `worker fn` / `worker async fn` — remap the contextual identifier to WorkerKw.
    if at_worker_modifier(p) {
        p.bump_remap(WorkerKw);
    }
    if p.at(AsyncKw) {
        p.bump();
    }
    if p.at(FnKw) {
        p.bump();
    } else {
        p.error("expected 'fn'");
    }
    if p.at(Star) {
        p.bump();
    } // generator
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
    if p.at(Colon) {
        ret_type(p);
    }
    if p.at(LBrace) {
        block(p);
    } else {
        p.error("expected '{' for function body");
    }
    p.complete(m, FnDecl);
}

fn ret_type(p: &mut Parser) {
    let m = p.start();
    p.bump(); // :
    type_ann(p);
    p.complete(m, SyntaxKind::RetType);
}

fn param_list(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // (
    while !p.at(RParen) && !p.at_end() {
        let pm = p.start();
        if p.at(DotDotDot) {
            p.bump();
        } // rest
        if p.at(Ident) {
            p.bump();
        } else {
            p.error("expected parameter name");
        }
        if p.at(Colon) {
            p.bump();
            type_ann(p);
        }
        // Optional default value: `= <expr>`. The expression becomes an `Expr`
        // child of the `Param` node (no wrapper); the compiler finds it via
        // `Expr::cast`.
        if p.at(Eq) {
            p.bump();
            expr(p);
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
///
/// Precedence matches the legacy `src/parser.rs` chain (loosest → tightest):
///   `??` < `||` < `&&` < equality < comparison < add/sub < mul/div/rem < `**`
/// The legacy parser has `coalesce` call `logic_or`, confirming `??` is looser than `||`
/// (verified by: `sexpr("a || b ?? c") == "(?? (|| a b) c)"`).
fn infix_binding_power(kind: SyntaxKind) -> Option<(u8, u8)> {
    use SyntaxKind::*;
    Some(match kind {
        QuestionQuestion => (1, 2), // loosest binary; looser than ||
        PipePipe => (3, 4),
        AmpAmp => (5, 6),
        EqEq | BangEq => (7, 8),
        Lt | Le | Gt | Ge | InstanceofKw => (9, 10),
        Plus | Minus => (11, 12),
        Star | Slash | Percent => (13, 14),
        StarStar => (18, 17), // right-assoc
        _ => return None,
    })
}

fn expr(p: &mut Parser) {
    let _ = expr_returning(p);
}

/// Run `f` with top-level arrow suppression temporarily disabled, restoring the
/// previous setting afterwards. Used wherever the parser descends into a
/// bracketed sub-expression (parens, call args, array/object/index, template
/// interpolation, ternary branches) so that nested/parenthesized arrows inside a
/// match guard still parse — only the guard's OWN top level suppresses the bare
/// `IDENT =>` form.
fn with_arrows_allowed<R>(p: &mut Parser, f: impl FnOnce(&mut Parser) -> R) -> R {
    let saved = p.suppress_arrow;
    p.suppress_arrow = false;
    let r = f(p);
    p.suppress_arrow = saved;
    r
}

fn expr_bp(p: &mut Parser, min_bp: u8) {
    let mut lhs = lhs(p);
    loop {
        let op = p.current();
        let Some((l_bp, r_bp)) = infix_binding_power(op) else {
            break;
        };
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
        Number
            | Str
            | TrueKw
            | FalseKw
            | NilKw
            | Ident
            | LParen
            | LBracket
            | LBrace
            | HashLBrace
            | Minus
            | Bang
            | TemplateStr
            | TemplateStart
            | AwaitKw
            | YieldKw
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
                      // `yield`'s operand is parsed at the LOWEST (assignment/expression)
                      // precedence, matching the tree-walker's `assignment()` (spec §7, like
                      // JS `yield`): `yield a + b` is `yield (a + b)` and `yield x = 1` is
                      // `yield (x = 1)`. (Contrast `await`, which is a UNARY-precedence prefix
                      // — `await x` then a binary tail: `await a + b` is `(await a) + b`.)
            if can_start_expr(p) {
                let _ = expr_returning(p);
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
        Ident if !p.suppress_arrow && is_bare_arrow_ahead(p) => {
            // Single-parameter arrow without parentheses: `x => expr`.
            let m = p.start();
            let pm = p.start();
            let param_m = p.start();
            p.bump(); // param name
            p.complete(param_m, Param);
            p.complete(pm, ParamList);
            p.bump(); // =>
            if p.at(LBrace) {
                block(p);
            } else {
                expr(p);
            }
            p.complete(m, ArrowExpr)
        }
        Ident => {
            let m = p.start();
            p.bump();
            p.complete(m, NameRef)
        }
        MatchKw => match_expr(p),
        AsyncKw if !p.suppress_arrow && is_async_arrow_ahead(p) => {
            let m = p.start();
            p.bump(); // async
            if p.at(Ident) {
                // Bare single-param: `async x => ...`
                let pm = p.start();
                let param_m = p.start();
                p.bump(); // param name
                p.complete(param_m, Param);
                p.complete(pm, ParamList);
            } else {
                param_list(p); // `async (params) => ...`
            }
            p.bump(); // =>
            if p.at(LBrace) {
                block(p);
            } else {
                expr(p);
            }
            p.complete(m, ArrowExpr)
        }
        LParen if is_arrow_ahead(p) => {
            let m = p.start();
            param_list(p);
            p.bump(); // =>  (guaranteed by is_arrow_ahead)
            with_arrows_allowed(p, |p| {
                if p.at(LBrace) {
                    block(p);
                } else {
                    expr(p);
                }
            });
            p.complete(m, ArrowExpr)
        }
        LParen => {
            let m = p.start();
            p.bump(); // (
            with_arrows_allowed(p, expr);
            if p.at(RParen) {
                p.bump();
            } else {
                p.error("expected ')'");
            }
            p.complete(m, ParenExpr)
        }
        LBracket => array_expr(p),
        LBrace => object_expr(p),
        HashLBrace => map_expr(p),
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
                with_arrows_allowed(p, expr);
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

/// True if the current token is an `Ident` immediately followed by `=>`,
/// making it a bare single-parameter arrow `x => ...`.
fn is_bare_arrow_ahead(p: &Parser) -> bool {
    matches!(
        p.nontrivia.get(p.pos + 1).map(|&ti| p.tokens[ti].kind),
        Some(SyntaxKind::FatArrow)
    )
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

/// True if `async (` ... `) =>` OR `async ident =>` starts here (an async arrow).
fn is_async_arrow_ahead(p: &Parser) -> bool {
    use SyntaxKind::*;
    match p.nontrivia.get(p.pos + 1).map(|&ti| p.tokens[ti].kind) {
        Some(Ident) => {
            // `async ident =>` — bare single-param async arrow.
            matches!(
                p.nontrivia.get(p.pos + 2).map(|&ti| p.tokens[ti].kind),
                Some(FatArrow)
            )
        }
        Some(LParen) => {
            // `async (params) =>` — parenthesized async arrow.
            let mut depth = 0i32;
            let mut i = p.pos + 1;
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
        _ => false,
    }
}

fn spread_elem(p: &mut Parser) {
    let m = p.start();
    p.bump(); // ...
    with_arrows_allowed(p, expr);
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
            with_arrows_allowed(p, expr);
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
                with_arrows_allowed(p, expr);
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

/// Parse a `#{ keyExpr: valueExpr, … }` map literal. Unlike `object_expr`, the
/// KEY is an arbitrary expression (so the map keys by the key's VALUE). D4:
/// a spread `...` element is a clean parse error (out of scope for SP2).
fn map_expr(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // #{
    while !p.at(RBrace) && !p.at_end() {
        if p.at(DotDotDot) {
            // Spread is not allowed in a map literal (D4) — report and bail so
            // the resulting tree carries an error node (both front-ends reject).
            p.error("spread is not allowed in a map literal");
            break;
        }
        let em = p.start();
        with_arrows_allowed(p, expr);
        if p.at(Colon) {
            p.bump();
            with_arrows_allowed(p, expr);
        } else {
            p.error("expected ':' after map key");
        }
        p.complete(em, MapEntry);
        if p.at(Comma) {
            p.bump();
        } else {
            break;
        }
    }
    if p.at(RBrace) {
        p.bump();
    } else {
        p.error("expected '}' to close map");
    }
    p.complete(m, MapExpr)
}

/// Parse an interpolated template: TemplateStart (expr TemplateMiddle)* expr
/// TemplateEnd. Each `${...}` slot holds a full expression.
fn template_expr(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // TemplateStart  (`...${)
    loop {
        with_arrows_allowed(p, expr); // interpolated expression
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

/// Parse a type annotation. union (`|`), then postfix-`?` optional, then a
/// primary (named/generic/tuple). Generics: `name<T, ...>`; tuples: `[T, ...]`.
fn type_ann(p: &mut Parser) {
    let cm = type_optional(p);
    if p.at(SyntaxKind::Pipe) {
        let m = p.precede(&cm);
        while p.at(SyntaxKind::Pipe) {
            p.bump(); // |
            type_optional(p);
        }
        p.complete(m, SyntaxKind::UnionType);
    }
}

fn type_optional(p: &mut Parser) -> CompletedMarker {
    let cm = type_primary(p);
    if p.at(SyntaxKind::Question) {
        let m = p.precede(&cm);
        p.bump(); // ?
        return p.complete(m, SyntaxKind::OptionalType);
    }
    cm
}

fn type_primary(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    match p.current() {
        Ident => {
            let m = p.start();
            p.bump(); // type name
            if p.at(Lt) {
                let args = p.start();
                p.bump(); // <
                while !p.at(Gt) && !p.at_end() {
                    type_ann(p);
                    if p.at(Comma) {
                        p.bump();
                    } else {
                        break;
                    }
                }
                if p.at(Gt) {
                    p.bump();
                } else {
                    p.error("expected '>' to close type arguments");
                }
                p.complete(args, TypeArgs);
                return p.complete(m, GenericType);
            }
            p.complete(m, NamedType)
        }
        // `nil` lexes as its own keyword (not an `Ident`) but is a valid type
        // (`Type::Nil` in the spec; accepted by the legacy parser and the
        // tree-sitter grammar). Wrap it in a `NamedType` so `cst_type` and the
        // formatter — both keyed on the node's first non-trivia token text —
        // treat it uniformly with `number`/`string`/etc. Note: only `nil` is a
        // keyword-lexed type here; literal keywords `true`/`false` and number/
        // string literals are deliberately NOT types (matching the legacy parser).
        NilKw => {
            let m = p.start();
            p.bump(); // nil
            p.complete(m, NamedType)
        }
        // `fn` is the function-type keyword (`Type::Fn` in the spec). Like `nil`
        // it lexes as its own keyword (not an `Ident`), so it needs its own arm;
        // wrap it in a `NamedType` (keyed on the node's first non-trivia token
        // text) so `cst_type` (FnKw → Type::Fn) and the formatter treat it
        // uniformly. Accepted by the legacy parser (`Tok::Fn => Type::Fn`) and the
        // tree-sitter grammar (`primitive_type` lists `'fn'`). `nil` and `fn` are
        // the ONLY two keyword-lexed types; all others (number/string/bool/any/
        // object/error/array/map/...) lex as `Ident`.
        FnKw => {
            let m = p.start();
            p.bump(); // fn
            p.complete(m, NamedType)
        }
        LBracket => {
            let m = p.start();
            p.bump(); // [
            while !p.at(RBracket) && !p.at_end() {
                type_ann(p);
                if p.at(Comma) {
                    p.bump();
                } else {
                    break;
                }
            }
            if p.at(RBracket) {
                p.bump();
            } else {
                p.error("expected ']' to close tuple type");
            }
            p.complete(m, TupleType)
        }
        _ => {
            let m = p.start();
            p.error("expected a type");
            p.complete(m, Error)
        }
    }
}

fn class_decl(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // class
    if p.at(Ident) {
        p.bump();
    } else {
        p.error("expected class name");
    }
    if p.at_kw("extends") {
        p.bump(); // extends
        if p.at(Ident) {
            p.bump();
        } else {
            p.error("expected superclass name after 'extends'");
        }
    }
    if p.at(LBrace) {
        p.bump();
    } else {
        p.error("expected '{' for class body");
    }
    while !p.at(RBrace) && !p.at_end() {
        let before = p.pos;
        if p.at(AsyncKw) || p.at(FnKw) || at_static_method(p) || at_worker_modifier(p) {
            method_decl(p);
        } else if p.at(Ident) {
            field_decl(p);
        } else {
            p.error("expected a field or method");
            p.bump();
        }
        if p.pos == before {
            p.bump();
        }
    }
    if p.at(RBrace) {
        p.bump();
    } else {
        p.error("expected '}' to close class");
    }
    p.complete(m, ClassDecl);
}

fn field_decl(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // field name (Ident)
    if p.at(Question) {
        p.bump();
    } // optional marker `name?:`
    if p.at(Colon) {
        p.bump();
        type_ann(p);
    } else {
        p.error("expected ':' and a type in field declaration");
    }
    if p.at(Eq) {
        p.bump();
        expr(p); // default value
    }
    p.complete(m, FieldDecl);
}

/// True when the cursor is at a `static` member modifier: the soft keyword
/// `static` (an `Ident`) immediately followed by `fn`, `async`, or `worker`
/// (the start of a method). This is the ONLY position where `static` is
/// recognized as a keyword; `static: T` (a field) or `static = …` keep
/// `static` an ordinary identifier.
fn at_static_method(p: &Parser) -> bool {
    if !p.at_kw("static") {
        return false;
    }
    // `static fn`, `static async fn`, `static worker fn`, `static worker async fn`.
    let next_kind = p.nontrivia.get(p.pos + 1).map(|&ti| p.tokens[ti].kind);
    let next_text = p
        .nontrivia
        .get(p.pos + 1)
        .map(|&ti| p.tokens[ti].text.as_str());
    matches!(next_kind, Some(SyntaxKind::FnKw) | Some(SyntaxKind::AsyncKw))
        || matches!(next_kind, Some(SyntaxKind::Ident)) && matches!(next_text, Some("worker"))
}

fn method_decl(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    if at_static_method(p) {
        p.bump_remap(StaticKw); // contextual `static` modifier
    }
    // `worker fn` / `worker async fn` — remap the contextual identifier to WorkerKw.
    if at_worker_modifier(p) {
        p.bump_remap(WorkerKw);
    }
    if p.at(AsyncKw) {
        p.bump();
    }
    if p.at(FnKw) {
        p.bump();
    } else {
        p.error("expected 'fn' in method");
    }
    if p.at(Star) {
        p.bump();
    } // generator method
    if p.at(Ident) {
        p.bump();
    } else {
        p.error("expected method name");
    }
    if p.at(LParen) {
        param_list(p);
    } else {
        p.error("expected '(' after method name");
    }
    if p.at(Colon) {
        ret_type(p);
    }
    if p.at(LBrace) {
        block(p);
    } else {
        p.error("expected '{' for method body");
    }
    p.complete(m, MethodDecl);
}

fn enum_decl(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // enum
    if p.at(Ident) {
        p.bump();
    } else {
        p.error("expected enum name");
    }
    if p.at(LBrace) {
        p.bump();
    } else {
        p.error("expected '{' for enum body");
    }
    while !p.at(RBrace) && !p.at_end() {
        let vm = p.start();
        if p.at(Ident) {
            p.bump();
        } else {
            p.error("expected variant name");
        }
        if p.at(Eq) {
            p.bump();
            expr(p);
        }
        p.complete(vm, EnumVariant);
        if p.at(Comma) {
            p.bump();
        } else {
            break;
        }
    }
    if p.at(RBrace) {
        p.bump();
    } else {
        p.error("expected '}' to close enum");
    }
    p.complete(m, EnumDecl);
}

fn import_stmt(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // import
    if p.at(Star) {
        p.bump(); // *
        if p.at_kw("as") {
            p.bump();
        } else {
            p.error("expected 'as' in namespace import");
        }
        if p.at(Ident) {
            p.bump();
        } else {
            p.error("expected import alias");
        }
    } else if p.at(LBrace) {
        let l = p.start();
        p.bump(); // {
        while !p.at(RBrace) && !p.at_end() {
            if p.at(Ident) {
                p.bump();
            } else {
                p.error("expected an import name");
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
            p.error("expected '}' to close import list");
        }
        p.complete(l, ImportList);
    } else {
        p.error("expected '{' or '*' after import");
    }
    if p.at_kw("from") {
        p.bump();
    } else {
        p.error("expected 'from'");
    }
    if p.at(Str) {
        p.bump();
    } else {
        p.error("expected a module path string");
    }
    p.complete(m, ImportStmt);
}

fn export_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump(); // export
    stmt(p); // the exported declaration
    p.complete(m, SyntaxKind::ExportStmt);
}

fn arg_list(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // (
    while !p.at(RParen) && !p.at_end() {
        if p.at(DotDotDot) {
            spread_elem(p);
        } else {
            with_arrows_allowed(p, expr);
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

fn match_expr(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // match
    with_arrows_allowed(p, expr); // subject
    if p.at(LBrace) {
        p.bump();
    } else {
        p.error("expected '{' for match body");
    }
    while !p.at(RBrace) && !p.at_end() {
        match_arm(p);
        if p.at(Comma) {
            p.bump();
        } else {
            break;
        }
    }
    if p.at(RBrace) {
        p.bump();
    } else {
        p.error("expected '}' to close match");
    }
    p.complete(m, MatchExpr)
}

fn match_arm(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    // Parse the first pattern, then check for `|` or-alternatives.
    let first_pat = pattern(p);
    if p.at(Pipe) {
        // Wrap first_pat + all subsequent alternatives in an OrPat node.
        let orm = p.precede(&first_pat);
        while p.at(Pipe) {
            p.bump(); // |
            pattern(p);
        }
        p.complete(orm, OrPat);
    }
    // Optional guard: `if cond`
    if p.at(IfKw) {
        let g = p.start();
        p.bump(); // if
                  // Suppress a TOP-LEVEL bare `IDENT =>` (or `async IDENT/(…) =>`) inside the
                  // guard: a guard ending in a bare identifier (`n if n == lim => ...`) must
                  // not swallow the arm's `=>`. Parenthesized/nested arrows still parse
                  // because every bracketed descent clears the flag via `with_arrows_allowed`.
        let saved = p.suppress_arrow;
        p.suppress_arrow = true;
        expr(p);
        p.suppress_arrow = saved;
        p.complete(g, MatchGuard);
    }
    if p.at(FatArrow) {
        p.bump();
    } else {
        p.error("expected '=>' in match arm");
    }
    // The arm body is a complete sub-expression bounded by the match braces, so a
    // bare arrow here is fine even if this match itself sits inside an outer guard.
    with_arrows_allowed(p, expr); // arm body
    p.complete(m, MatchArm);
}

/// A version of `primary` that never interprets `Ident =>` as a bare arrow —
/// used from pattern context where `x =>` is the match arm separator, not an arrow.
fn primary_no_arrow(p: &mut Parser) -> CompletedMarker {
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
        _ => {
            let m = p.start();
            p.error("expected pattern value");
            p.complete(m, Error)
        }
    };
    // Allow member access like `Shape.Circle` in patterns.
    postfix(p, cm)
}

/// Like `lhs` but uses `primary_no_arrow` — safe to use from pattern context.
fn lhs_for_pat(p: &mut Parser) -> CompletedMarker {
    let u = match p.current() {
        SyntaxKind::Minus | SyntaxKind::Bang => {
            let m = p.start();
            p.bump();
            let _ = primary_no_arrow(p);
            p.complete(m, SyntaxKind::UnaryExpr)
        }
        _ => primary_no_arrow(p),
    };
    unwrap_tier(p, u)
}

/// Parse a single match pattern. Returns the CompletedMarker so callers can
/// wrap it (e.g. for OrPat). Bare identifiers are emitted as LiteralPat
/// (Option-C: compare-vs-bind resolved later); wildcard `_` → WildcardPat.
fn pattern(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    match p.current() {
        Ident if p.at_kw("_") => {
            let m = p.start();
            p.bump();
            p.complete(m, WildcardPat)
        }
        LBracket => array_pat(p),
        LBrace => object_pat(p),
        _ => {
            // value / ident / range: parse a primary-ish expression; a trailing
            // `..` / `..=` makes it a RangePat, else LiteralPat.
            let m = p.start();
            let _ = lhs_for_pat(p);
            if p.at(DotDot) || p.at(DotDotEq) {
                p.bump();
                let _ = lhs_for_pat(p);
                // Trailing contextual `step <expr>` (strided membership, spec §3.7).
                // `step` is recognized only when it directly follows the range end,
                // so it stays a normal identifier elsewhere. The step expression is
                // the 3rd Expr child of the `RangePat`, after `start..end`.
                parse_range_step(p);
                p.complete(m, RangePat)
            } else {
                p.complete(m, LiteralPat)
            }
        }
    }
}

fn array_pat(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // [
    while !p.at(RBracket) && !p.at_end() {
        if p.at(DotDotDot) {
            pat_rest(p);
        } else {
            pattern(p);
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
        p.error("expected ']' to close array pattern");
    }
    p.complete(m, ArrayPat)
}

fn object_pat(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // {
    while !p.at(RBrace) && !p.at_end() {
        if p.at(DotDotDot) {
            pat_rest(p);
        } else {
            let e = p.start();
            if p.at(Ident) || p.at(Str) {
                p.bump();
            } else {
                p.error("expected key in object pattern");
            }
            if p.at(Colon) {
                p.bump();
                pattern(p);
            }
            p.complete(e, ObjPatEntry);
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
        p.error("expected '}' to close object pattern");
    }
    p.complete(m, ObjectPat)
}

fn pat_rest(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // ...
    if p.at(Ident) {
        p.bump(); // optional bound name
    }
    p.complete(m, PatRest)
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
            vec![
                SyntaxKind::SourceFile,
                SyntaxKind::ExprStmt,
                SyntaxKind::Literal
            ]
        );
        assert!(parse("42").errors.is_empty());
    }

    #[test]
    fn unexpected_token_recovers_not_panics() {
        let p = parse("+");
        assert!(!p.errors.is_empty(), "should record an error");
        assert!(matches!(
            p.events.first(),
            Some(Event::Start {
                kind: SyntaxKind::SourceFile,
                ..
            })
        ));
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
                SyntaxKind::SourceFile,
                SyntaxKind::ExprStmt,
                SyntaxKind::BinaryExpr,
                SyntaxKind::Literal,
                SyntaxKind::BinaryExpr,
                SyntaxKind::Literal,
                SyntaxKind::Literal,
            ]
        );
        assert!(parse("1 + 2 * 3").errors.is_empty());
    }

    #[test]
    fn unary_and_paren() {
        assert_eq!(
            tree_shape("-(1)"),
            vec![
                SyntaxKind::SourceFile,
                SyntaxKind::ExprStmt,
                SyntaxKind::UnaryExpr,
                SyntaxKind::ParenExpr,
                SyntaxKind::Literal,
            ]
        );
    }

    #[test]
    fn name_reference() {
        assert_eq!(
            tree_shape("x"),
            vec![
                SyntaxKind::SourceFile,
                SyntaxKind::ExprStmt,
                SyntaxKind::NameRef
            ]
        );
    }

    #[test]
    fn let_statement() {
        assert_eq!(
            tree_shape("let x = 1"),
            vec![
                SyntaxKind::SourceFile,
                SyntaxKind::LetStmt,
                SyntaxKind::Literal
            ]
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
    fn assignment_in_call_arg() {
        // print(x = 5): assignment is valid in expression position (matches legacy + tree-sitter).
        let p = parse("print(x = 5)");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let shape = tree_shape("print(x = 5)");
        assert!(shape.contains(&SyntaxKind::AssignExpr), "shape: {shape:?}");
        assert!(!shape.contains(&SyntaxKind::Error), "shape: {shape:?}");
    }

    #[test]
    fn assignment_among_call_args() {
        let p = parse("f(a, b = 2, c)");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        assert!(tree_shape("f(a, b = 2, c)").contains(&SyntaxKind::AssignExpr));
    }

    #[test]
    fn assignment_in_array_element() {
        let p = parse("[x = 1]");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        assert!(tree_shape("[x = 1]").contains(&SyntaxKind::AssignExpr));
    }

    #[test]
    fn assignment_in_paren_initializer() {
        let p = parse("let r = (x = 5)");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        assert!(tree_shape("let r = (x = 5)").contains(&SyntaxKind::AssignExpr));
    }

    #[test]
    fn chained_assignment_right_assoc() {
        // a = b = c: right-associative; the outer AssignExpr's rhs is another AssignExpr.
        let p = parse("a = b = c");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let shape = tree_shape("a = b = c");
        // Two AssignExpr nodes (nested), no error.
        assert_eq!(
            shape
                .iter()
                .filter(|k| **k == SyntaxKind::AssignExpr)
                .count(),
            2,
            "shape: {shape:?}"
        );
        assert!(!shape.contains(&SyntaxKind::Error));
    }

    #[test]
    fn compound_assignment_in_expr_position() {
        for src in ["f(x += 1)", "f(x -= 1)", "f(x *= 2)", "f(x /= 2)"] {
            let p = parse(src);
            assert!(p.errors.is_empty(), "{src}: {:?}", p.errors);
            assert!(tree_shape(src).contains(&SyntaxKind::AssignExpr), "{src}");
        }
    }

    #[test]
    fn ternary_lower_than_nothing_but_assign_wraps_it() {
        // `a ? b : c = d` — `=` is lower precedence than `?:`, so the whole ternary
        // is the assignment LHS (matches the legacy parser, whose assignment() wraps
        // ternary()). Tree: AssignExpr { TernaryExpr, ... }.
        let p = parse("a ? b : c = d");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let shape = tree_shape("a ? b : c = d");
        assert!(shape.contains(&SyntaxKind::AssignExpr), "shape: {shape:?}");
        assert!(shape.contains(&SyntaxKind::TernaryExpr), "shape: {shape:?}");
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
        assert!(parse(r#"let o = { a: 1, "k": 2, ...rest }"#)
            .errors
            .is_empty());
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
        let try_idx = shape
            .iter()
            .position(|k| *k == SyntaxKind::TryExpr)
            .expect("TryExpr present");
        let await_idx = shape
            .iter()
            .position(|k| *k == SyntaxKind::AwaitExpr)
            .expect("AwaitExpr present");
        assert!(
            try_idx < await_idx,
            "expected (await x)? — TryExpr should wrap AwaitExpr"
        );
    }

    #[test]
    fn yield_expression() {
        assert!(tree_shape("yield x").contains(&SyntaxKind::YieldExpr));
        assert!(tree_shape("yield").contains(&SyntaxKind::YieldExpr));
    }

    #[test]
    fn nullish_coalescing() {
        assert!(tree_shape("a ?? b").contains(&SyntaxKind::BinaryExpr));
        assert!(parse("a ?? b").errors.is_empty());
    }

    #[test]
    fn compound_assignment() {
        for src in ["x += 1", "x -= 1", "x *= 2", "x /= 2"] {
            let p = parse(src);
            assert!(p.errors.is_empty(), "errors for {src}: {:?}", p.errors);
            assert!(
                tree_shape(src).contains(&SyntaxKind::AssignExpr),
                "no assign for {src}"
            );
        }
    }

    #[test]
    fn array_destructuring() {
        let p = parse("let [a, b, ...rest] = xs");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let s = tree_shape("let [a, b, ...rest] = xs");
        assert!(s.contains(&SyntaxKind::ArrayBindPat));
        assert!(s.contains(&SyntaxKind::RestBind));
    }

    #[test]
    fn object_destructuring_with_rename() {
        let p = parse("let {a, b as local, ...rest} = obj");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let s = tree_shape("let {a, b as local, ...rest} = obj");
        assert!(s.contains(&SyntaxKind::ObjectBindPat));
        assert!(s.contains(&SyntaxKind::BindEntry));
        assert!(s.contains(&SyntaxKind::RestBind));
    }

    #[test]
    fn for_loops() {
        for src in [
            "for (x of items) { print(x) }",
            "for (i in 1..6) { print(i) }",
            "for (i in 0..=5) { print(i) }",
            "for await (x in stream) { print(x) }",
        ] {
            let p = parse(src);
            assert!(p.errors.is_empty(), "errors for {src}: {:?}", p.errors);
            assert!(
                tree_shape(src).contains(&SyntaxKind::ForStmt),
                "no ForStmt for {src}"
            );
        }
        assert!(tree_shape("for (i in 1..6) {}").contains(&SyntaxKind::RangeExpr));
    }

    #[test]
    fn break_continue() {
        assert!(parse("while (x) { break }").errors.is_empty());
        assert!(tree_shape("while (x) { break }").contains(&SyntaxKind::BreakStmt));
        assert!(tree_shape("while (x) { continue }").contains(&SyntaxKind::ContinueStmt));
    }

    #[test]
    fn type_annotations() {
        for (src, kind) in [
            ("let x: number = 1", SyntaxKind::NamedType),
            ("let x: nil = nil", SyntaxKind::NamedType),
            ("fn f(): nil {}", SyntaxKind::NamedType),
            ("let f: fn = x", SyntaxKind::NamedType),
            ("fn apply(g: fn, x) {}", SyntaxKind::NamedType),
            ("fn h(): fn { return g }", SyntaxKind::NamedType),
            ("fn g(): number | nil { return nil }", SyntaxKind::NamedType),
            ("let x: array<number> = []", SyntaxKind::GenericType),
            ("let x: number? = nil", SyntaxKind::OptionalType),
            ("let x: number | string = 1", SyntaxKind::UnionType),
            ("let x: map<string, number> = m", SyntaxKind::GenericType),
            ("let x: [number, string] = t", SyntaxKind::TupleType),
        ] {
            let p = parse(src);
            assert!(p.errors.is_empty(), "errors for {src}: {:?}", p.errors);
            assert!(
                tree_shape(src).contains(&kind),
                "missing {kind:?} for {src}"
            );
            assert!(
                !tree_shape(src).contains(&SyntaxKind::Error),
                "Error node for {src}"
            );
        }
    }

    #[test]
    fn async_and_generator_fns() {
        for src in [
            "async fn f() { return 1 }",
            "fn* g() { yield 1 }",
            "async fn* h() { yield 1 }",
            "fn add(a: number, b: number): number { return a + b }",
            "fn variadic(first, ...rest) { return rest }",
        ] {
            let p = parse(src);
            assert!(p.errors.is_empty(), "errors for {src}: {:?}", p.errors);
            assert!(
                tree_shape(src).contains(&SyntaxKind::FnDecl),
                "no FnDecl for {src}"
            );
        }
        assert!(tree_shape("fn add(a: number): number {}").contains(&SyntaxKind::RetType));
    }

    #[test]
    fn worker_fn_and_static_worker_parse() {
        for src in [
            "worker fn f() { return 1 }",
            "worker fn g(a, b) { return a }",
            "class C { static worker fn h(x) { return x } }",
            "class C { worker fn m(x) { return x } }",
        ] {
            let r = parse(src);
            assert!(r.errors.is_empty(), "errors for {src}: {:?}", r.errors);
            assert!(
                tree_shape(src).contains(&SyntaxKind::FnDecl)
                    || tree_shape(src).contains(&SyntaxKind::MethodDecl),
                "no Fn/Method decl for {src}"
            );
            assert!(
                !tree_shape(src).contains(&SyntaxKind::Error),
                "Error node in tree for {src}"
            );
        }
    }

    #[test]
    fn worker_stays_identifier_when_not_a_modifier() {
        assert!(parse("let worker = 5").errors.is_empty());
        assert!(parse("worker(1)").errors.is_empty());
    }

    #[test]
    fn async_arrow() {
        assert!(parse("let f = async (x) => x").errors.is_empty());
        assert!(tree_shape("let f = async (x) => x").contains(&SyntaxKind::ArrowExpr));
    }

    #[test]
    fn enum_declaration() {
        let p = parse("enum Color { Red, Green = 2, Blue }");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let s = tree_shape("enum Color { Red, Green = 2, Blue }");
        assert!(s.contains(&SyntaxKind::EnumDecl));
        assert!(s.contains(&SyntaxKind::EnumVariant));
    }

    #[test]
    fn class_declaration() {
        let src = "class Dog extends Animal {\n  name: string\n  age: number = 0\n  nickname?: string\n  fn init(name) { self.name = name }\n  fn describe(): string { return self.name }\n}";
        let p = parse(src);
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let s = tree_shape(src);
        assert!(s.contains(&SyntaxKind::ClassDecl));
        assert!(s.contains(&SyntaxKind::FieldDecl));
        assert!(s.contains(&SyntaxKind::MethodDecl));
    }

    #[test]
    fn match_expression_patterns() {
        let src = r#"let r = match n { _ if n < 0 => "neg", 0 => "zero", 1..=9 => "single", "sat" | "sun" => "weekend", [] => "empty", [x] => "one", [first, ...rest] => "many", {a, b: c} => "obj", _ => "big" }"#;
        let p = parse(src);
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let s = tree_shape(src);
        for k in [
            SyntaxKind::MatchExpr,
            SyntaxKind::MatchArm,
            SyntaxKind::MatchGuard,
            SyntaxKind::WildcardPat,
            SyntaxKind::LiteralPat,
            SyntaxKind::RangePat,
            SyntaxKind::OrPat,
            SyntaxKind::ArrayPat,
            SyntaxKind::ObjectPat,
            SyntaxKind::PatRest,
        ] {
            assert!(s.contains(&k), "missing {k:?}");
        }
    }

    #[test]
    fn match_guard_ending_in_ident_is_not_an_arrow() {
        // A guard ending in a bare identifier right before `=>` must NOT be parsed
        // as an arrow; the `=>` belongs to the match arm. Regression for the V10
        // differential blind spot (guards previously only tested ending in literals).
        let src = r#"match v { n if n == lim => 1, _ => 2 }"#;
        let p = parse(src);
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let s = tree_shape(src);
        assert!(s.contains(&SyntaxKind::MatchExpr), "no MatchExpr");
        assert!(s.contains(&SyntaxKind::MatchGuard), "no MatchGuard");
        assert!(
            !s.contains(&SyntaxKind::ArrowExpr),
            "guard was mis-parsed as an arrow: {s:?}"
        );
        assert!(
            !s.contains(&SyntaxKind::Error),
            "unexpected Error node: {s:?}"
        );
        // Both arms must have a body: the arm node count should be 2.
        let arms = s.iter().filter(|&&k| k == SyntaxKind::MatchArm).count();
        assert_eq!(arms, 2, "expected 2 arms, got shape {s:?}");
    }

    #[test]
    fn match_guard_with_logical_and_ending_in_ident() {
        // A guard with `&&`/comparison that ends in an identifier.
        let src = r#"match v { n if n > 0 && n == lim => "a", other => "o" }"#;
        let p = parse(src);
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let s = tree_shape(src);
        assert!(!s.contains(&SyntaxKind::ArrowExpr), "spurious arrow: {s:?}");
        assert!(!s.contains(&SyntaxKind::Error), "unexpected Error: {s:?}");
    }

    #[test]
    fn match_guard_with_parenthesized_arrow_still_parses() {
        // Parens disambiguate: an arrow inside the guard still works.
        let src = r#"match v { x if (() => true)() => 1, _ => 2 }"#;
        let p = parse(src);
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let s = tree_shape(src);
        assert!(
            s.contains(&SyntaxKind::ArrowExpr),
            "paren arrow lost: {s:?}"
        );
        assert!(!s.contains(&SyntaxKind::Error), "unexpected Error: {s:?}");
    }

    #[test]
    fn match_guard_with_call_containing_arrow_arg() {
        // A bare arrow inside a call argument within the guard must still parse.
        let src = r#"match v { x if f(y => y) => 1, _ => 2 }"#;
        let p = parse(src);
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let s = tree_shape(src);
        assert!(s.contains(&SyntaxKind::ArrowExpr), "arg arrow lost: {s:?}");
        assert!(!s.contains(&SyntaxKind::Error), "unexpected Error: {s:?}");
    }

    #[test]
    fn imports_and_exports() {
        for src in [
            r#"import * as task from "std/task""#,
            r#"import { a, b } from "./mod""#,
            "export fn f() { return 1 }",
        ] {
            let p = parse(src);
            assert!(p.errors.is_empty(), "errors for {src}: {:?}", p.errors);
        }
        assert!(tree_shape(r#"import * as t from "std/task""#).contains(&SyntaxKind::ImportStmt));
        assert!(tree_shape(r#"import { a } from "m""#).contains(&SyntaxKind::ImportList));
        assert!(tree_shape("export fn f() {}").contains(&SyntaxKind::ExportStmt));
    }
}
