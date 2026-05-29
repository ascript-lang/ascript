//! Recursive-descent / precedence-climbing parser.

use crate::ast::{ArrowBody, BinOp, Expr, ExprKind, Stmt, UnOp};
use crate::error::AsError;
use crate::span::Span;
use crate::token::{Tok, Token};

pub fn parse(tokens: &[Token]) -> Result<Vec<Stmt>, AsError> {
    let mut parser = Parser { tokens, pos: 0 };
    parser.program()
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Tok {
        &self.tokens[self.pos].tok
    }

    fn span(&self) -> Span {
        self.tokens[self.pos].span
    }

    /// End offset of the most recently consumed token.
    fn prev_end(&self) -> usize {
        self.tokens[self.pos - 1].span.end
    }

    fn advance(&mut self) -> Tok {
        let t = self.tokens[self.pos].tok.clone();
        self.pos += 1;
        t
    }

    fn eat(&mut self, expected: &Tok) -> Result<(), AsError> {
        if self.peek() == expected {
            self.pos += 1;
            Ok(())
        } else {
            Err(AsError::at(
                format!("expected {:?}, found {:?}", expected, self.peek()),
                self.span(),
            ))
        }
    }

    fn program(&mut self) -> Result<Vec<Stmt>, AsError> {
        let mut stmts = Vec::new();
        self.skip_semicolons();
        while *self.peek() != Tok::Eof {
            stmts.push(self.statement()?);
            self.skip_semicolons();
        }
        Ok(stmts)
    }

    /// `;` is an optional statement separator; consume any run of them.
    fn skip_semicolons(&mut self) {
        while *self.peek() == Tok::Semicolon {
            self.advance();
        }
    }

    fn statement(&mut self) -> Result<Stmt, AsError> {
        match self.peek() {
            Tok::Let => self.let_stmt(true),
            Tok::Const => self.let_stmt(false),
            Tok::LBrace => Ok(Stmt::Block(self.block()?)),
            Tok::If => self.if_stmt(),
            Tok::While => self.while_stmt(),
            Tok::For => self.for_stmt(),
            Tok::Return => self.return_stmt(),
            Tok::Fn => self.fn_decl(false),
            // `async fn` is a declaration; `async (…) =>` / `async x =>` are
            // arrow expressions, handled by the expression path below.
            Tok::Async if self.tokens[self.pos + 1].tok == Tok::Fn => {
                self.advance(); // consume `async`
                self.fn_decl(true)
            }
            Tok::Enum => self.enum_decl(),
            Tok::Class => self.class_decl(),
            Tok::Import => self.import_decl(),
            Tok::Export => self.export_decl(),
            Tok::Break => {
                self.advance();
                Ok(Stmt::Break)
            }
            Tok::Continue => {
                self.advance();
                Ok(Stmt::Continue)
            }
            _ => Ok(Stmt::Expr(self.expr()?)),
        }
    }

    fn import_decl(&mut self) -> Result<Stmt, AsError> {
        self.eat(&Tok::Import)?;
        let names = if *self.peek() == Tok::Star {
            self.advance();
            // `as`
            if !matches!(self.peek(), Tok::Ident(s) if s == "as") {
                return Err(AsError::at("expected 'as' after '*' in import", self.span()));
            }
            self.advance();
            let alias = match self.advance() {
                Tok::Ident(n) => n,
                other => return Err(AsError::at(format!("expected namespace alias, found {:?}", other), self.tokens[self.pos - 1].span)),
            };
            crate::ast::ImportNames::Namespace(alias)
        } else {
            self.eat(&Tok::LBrace)?;
            let mut names = Vec::new();
            if *self.peek() != Tok::RBrace {
                loop {
                    match self.advance() {
                        Tok::Ident(n) => names.push(n),
                        other => return Err(AsError::at(format!("expected import name, found {:?}", other), self.tokens[self.pos - 1].span)),
                    }
                    if *self.peek() == Tok::Comma {
                        self.advance();
                        if *self.peek() == Tok::RBrace { break; }
                    } else {
                        break;
                    }
                }
            }
            self.eat(&Tok::RBrace)?;
            crate::ast::ImportNames::Named(names)
        };
        // `from`
        if !matches!(self.peek(), Tok::Ident(s) if s == "from") {
            return Err(AsError::at("expected 'from' in import", self.span()));
        }
        self.advance();
        let source = match self.advance() {
            Tok::Str(s) => s,
            other => return Err(AsError::at(format!("expected module path string, found {:?}", other), self.tokens[self.pos - 1].span)),
        };
        Ok(Stmt::Import { names, source })
    }

    fn export_decl(&mut self) -> Result<Stmt, AsError> {
        self.eat(&Tok::Export)?;
        // Only declarations are exportable.
        let inner = match self.peek() {
            Tok::Let => self.let_stmt(true)?,
            Tok::Const => self.let_stmt(false)?,
            Tok::Fn => self.fn_decl(false)?,
            Tok::Async => {
                self.advance(); // consume `async`
                if *self.peek() != Tok::Fn {
                    return Err(AsError::at(
                        format!("expected 'fn' after 'async', found {:?}", self.peek()),
                        self.span(),
                    ));
                }
                self.fn_decl(true)?
            }
            Tok::Class => self.class_decl()?,
            Tok::Enum => self.enum_decl()?,
            other => return Err(AsError::at(format!("only let/const/fn/class/enum can be exported, found {:?}", other), self.span())),
        };
        Ok(Stmt::Export(Box::new(inner)))
    }

    fn return_stmt(&mut self) -> Result<Stmt, AsError> {
        self.advance(); // consume `return`
        // No value if the next token cannot begin an expression in this position.
        match self.peek() {
            Tok::RBrace | Tok::Eof | Tok::Semicolon => Ok(Stmt::Return(None)),
            _ => {
                let value = self.expr()?;
                Ok(Stmt::Return(Some(value)))
            }
        }
    }

    fn fn_decl(&mut self, is_async: bool) -> Result<Stmt, AsError> {
        self.eat(&Tok::Fn)?;
        let name = match self.advance() {
            Tok::Ident(name) => name,
            other => {
                return Err(AsError::at(
                    format!("expected a function name, found {:?}", other),
                    self.tokens[self.pos - 1].span,
                ))
            }
        };
        let params = self.param_list()?;
        let ret = if *self.peek() == Tok::Colon {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = self.block()?;
        Ok(Stmt::Fn { name, params, ret, body, is_async })
    }

    fn enum_decl(&mut self) -> Result<Stmt, AsError> {
        self.eat(&Tok::Enum)?;
        let name = match self.advance() {
            Tok::Ident(n) => n,
            other => return Err(AsError::at(format!("expected enum name, found {:?}", other), self.tokens[self.pos - 1].span)),
        };
        self.eat(&Tok::LBrace)?;
        let mut variants = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            let vname = match self.advance() {
                Tok::Ident(n) => n,
                other => return Err(AsError::at(format!("expected variant name, found {:?}", other), self.tokens[self.pos - 1].span)),
            };
            let value = if *self.peek() == Tok::Eq {
                self.advance();
                Some(self.expr()?)
            } else {
                None
            };
            variants.push(crate::ast::EnumVariantDecl { name: vname, value });
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.eat(&Tok::RBrace)?;
        Ok(Stmt::Enum { name, variants })
    }

    fn class_decl(&mut self) -> Result<Stmt, AsError> {
        self.eat(&Tok::Class)?;
        let name = match self.advance() {
            Tok::Ident(n) => n,
            other => return Err(AsError::at(format!("expected class name, found {:?}", other), self.tokens[self.pos - 1].span)),
        };
        let superclass = if matches!(self.peek(), Tok::Ident(s) if s == "extends") {
            // `extends` is a soft keyword here (lexes as Ident)
            self.advance();
            match self.advance() {
                Tok::Ident(n) => Some(n),
                other => return Err(AsError::at(format!("expected superclass name, found {:?}", other), self.tokens[self.pos - 1].span)),
            }
        } else {
            None
        };
        self.eat(&Tok::LBrace)?;
        let mut methods = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            let is_async = if *self.peek() == Tok::Async {
                self.advance();
                true
            } else {
                false
            };
            self.eat(&Tok::Fn)?;
            let mname = match self.advance() {
                Tok::Ident(n) => n,
                other => return Err(AsError::at(format!("expected method name, found {:?}", other), self.tokens[self.pos - 1].span)),
            };
            let params = self.param_list()?;
            let ret = if *self.peek() == Tok::Colon {
                self.advance();
                Some(self.parse_type()?)
            } else {
                None
            };
            let body = self.block()?;
            methods.push(crate::ast::MethodDecl { name: mname, params, ret, body, is_async });
        }
        self.eat(&Tok::RBrace)?;
        Ok(Stmt::Class { name, superclass, methods })
    }

    fn parse_type(&mut self) -> Result<crate::ast::Type, AsError> {
        let mut t = self.parse_type_atom()?;
        while *self.peek() == Tok::Pipe {
            self.advance();
            let rhs = self.parse_type_atom()?;
            t = crate::ast::Type::Union(Box::new(t), Box::new(rhs));
        }
        Ok(t)
    }

    fn parse_type_atom(&mut self) -> Result<crate::ast::Type, AsError> {
        use crate::ast::Type;
        let span = self.span();
        match self.advance() {
            Tok::Nil => Ok(Type::Nil),
            Tok::Fn => Ok(Type::Fn),
            Tok::LBracket => {
                // tuple type [T1, T2, ...]
                let mut parts = Vec::new();
                if *self.peek() != Tok::RBracket {
                    loop {
                        parts.push(self.parse_type()?);
                        if *self.peek() == Tok::Comma {
                            self.advance();
                            if *self.peek() == Tok::RBracket {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                self.eat(&Tok::RBracket)?;
                Ok(Type::Tuple(parts))
            }
            Tok::Ident(name) => match name.as_str() {
                "number" => Ok(Type::Number),
                "string" => Ok(Type::String),
                "bool" => Ok(Type::Bool),
                "any" => Ok(Type::Any),
                "object" => Ok(Type::Object),
                "error" => Ok(Type::Error),
                "array" => {
                    self.eat(&Tok::Lt)?;
                    let inner = self.parse_type()?;
                    self.eat(&Tok::Gt)?;
                    Ok(Type::Array(Box::new(inner)))
                }
                "Result" => {
                    self.eat(&Tok::Lt)?;
                    let inner = self.parse_type()?;
                    self.eat(&Tok::Gt)?;
                    Ok(Type::Result(Box::new(inner)))
                }
                "map" => Err(AsError::at(
                    "map<K,V> type annotations arrive in Milestone 8",
                    span,
                )),
                _ => Ok(Type::Named(name)),
            },
            other => Err(AsError::at(format!("expected a type, found {:?}", other), span)),
        }
    }

    /// Parse `( ident, ident, … )` — a comma-separated list of parameters, each
    /// an optionally type-annotated name.
    fn param_list(&mut self) -> Result<Vec<crate::ast::Param>, AsError> {
        self.eat(&Tok::LParen)?;
        let mut params = Vec::new();
        if *self.peek() != Tok::RParen {
            loop {
                let name = match self.advance() {
                    Tok::Ident(name) => name,
                    other => {
                        return Err(AsError::at(
                            format!("expected a parameter name, found {:?}", other),
                            self.tokens[self.pos - 1].span,
                        ))
                    }
                };
                let ty = if *self.peek() == Tok::Colon {
                    self.advance();
                    Some(self.parse_type()?)
                } else {
                    None
                };
                params.push(crate::ast::Param { name, ty });
                if *self.peek() == Tok::Comma {
                    self.advance();
                    if *self.peek() == Tok::RParen {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        self.eat(&Tok::RParen)?;
        Ok(params)
    }

    /// Parse `{ stmt* }` (with optional `;` separators) and return the inner statements.
    fn block(&mut self) -> Result<Vec<Stmt>, AsError> {
        self.eat(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        self.skip_semicolons();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            stmts.push(self.statement()?);
            self.skip_semicolons();
        }
        self.eat(&Tok::RBrace)?;
        Ok(stmts)
    }

    fn if_stmt(&mut self) -> Result<Stmt, AsError> {
        self.eat(&Tok::If)?;
        self.eat(&Tok::LParen)?;
        let cond = self.expr()?;
        self.eat(&Tok::RParen)?;
        let then_branch = self.block()?;
        let else_branch = if *self.peek() == Tok::Else {
            self.advance();
            if *self.peek() == Tok::If {
                Some(vec![self.if_stmt()?]) // `else if`
            } else {
                Some(self.block()?)
            }
        } else {
            None
        };
        Ok(Stmt::If { cond, then_branch, else_branch })
    }

    fn while_stmt(&mut self) -> Result<Stmt, AsError> {
        self.eat(&Tok::While)?;
        self.eat(&Tok::LParen)?;
        let cond = self.expr()?;
        self.eat(&Tok::RParen)?;
        let body = self.block()?;
        Ok(Stmt::While { cond, body })
    }

    fn for_stmt(&mut self) -> Result<Stmt, AsError> {
        self.eat(&Tok::For)?;
        self.eat(&Tok::LParen)?;
        let var = match self.advance() {
            Tok::Ident(name) => name,
            other => {
                return Err(AsError::at(
                    format!("expected a loop variable name, found {:?}", other),
                    self.tokens[self.pos - 1].span,
                ))
            }
        };
        match self.advance() {
            Tok::In => {
                let start = self.expr()?;
                self.eat(&Tok::DotDot)?;
                let end = self.expr()?;
                self.eat(&Tok::RParen)?;
                let body = self.block()?;
                Ok(Stmt::ForRange { var, start, end, body })
            }
            Tok::Of => {
                let iter = self.expr()?;
                self.eat(&Tok::RParen)?;
                let body = self.block()?;
                Ok(Stmt::ForOf { var, iter, body })
            }
            other => Err(AsError::at(
                format!("expected 'in' or 'of' in for-loop, found {:?}", other),
                self.tokens[self.pos - 1].span,
            )),
        }
    }

    fn let_stmt(&mut self, mutable: bool) -> Result<Stmt, AsError> {
        self.advance(); // consume `let` / `const`
        // `let [a, b] = expr` — array destructuring binding (spec §6).
        if *self.peek() == Tok::LBracket {
            self.advance(); // consume '['
            let mut names = Vec::new();
            if *self.peek() != Tok::RBracket {
                loop {
                    match self.advance() {
                        Tok::Ident(n) => names.push(n),
                        other => return Err(AsError::at(
                            format!("expected an identifier in destructuring pattern, found {:?}", other),
                            self.tokens[self.pos - 1].span,
                        )),
                    }
                    if *self.peek() == Tok::Comma {
                        self.advance();
                        if *self.peek() == Tok::RBracket { break; }
                    } else {
                        break;
                    }
                }
            }
            self.eat(&Tok::RBracket)?;
            self.eat(&Tok::Eq)?;
            let value = self.expr()?;
            return Ok(Stmt::LetDestructure { names, value, mutable });
        }
        let name = match self.advance() {
            Tok::Ident(name) => name,
            other => {
                return Err(AsError::at(
                    format!("expected a variable name, found {:?}", other),
                    self.tokens[self.pos - 1].span,
                ))
            }
        };
        let ty = if *self.peek() == Tok::Colon {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        self.eat(&Tok::Eq)?;
        let value = self.expr()?;
        Ok(Stmt::Let { name, ty, value, mutable })
    }

    fn expr(&mut self) -> Result<Expr, AsError> {
        self.assignment()
    }

    fn assignment(&mut self) -> Result<Expr, AsError> {
        // Arrow functions: `x => …` or `(a, b) => …`. Detect without breaking
        // ordinary parenthesized expressions by checking ahead for `=>`.
        if let Some(arrow) = self.try_arrow()? {
            return Ok(arrow);
        }

        let target = self.coalesce()?;

        // Map a compound-assignment token to the binary op it desugars to.
        let compound = match self.peek() {
            Tok::Eq => None,
            Tok::PlusEq => Some(BinOp::Add),
            Tok::MinusEq => Some(BinOp::Sub),
            Tok::StarEq => Some(BinOp::Mul),
            Tok::SlashEq => Some(BinOp::Div),
            _ => return Ok(target),
        };
        self.advance(); // assignment operator
        let value = self.assignment()?; // right-associative

        // Only assignable expressions are valid targets. (Index/Member added later
        // in this milestone are also assignable; identifiers always are.)
        if !is_assignable(&target) {
            return Err(AsError::at("invalid assignment target", target.span));
        }

        let span = Span::new(target.span.start, value.span.end);
        let value = match compound {
            None => value,
            Some(op) => {
                // x += e  =>  x = (x + e). Re-uses the target expression as the lhs.
                let lhs = target.clone();
                Self::make_binary(lhs, op, value)
            }
        };

        Ok(Expr {
            kind: ExprKind::Assign { target: Box::new(target), value: Box::new(value) },
            span,
        })
    }

    /// Build a left-associative binary node from an already-parsed left side.
    fn make_binary(left: Expr, op: BinOp, right: Expr) -> Expr {
        let span = Span::new(left.span.start, right.span.end);
        Expr { kind: ExprKind::Binary { op, lhs: Box::new(left), rhs: Box::new(right) }, span }
    }

    /// Attempt to parse an arrow function at the current position. Returns
    /// `Ok(None)` (without consuming) if what follows is not an arrow.
    fn try_arrow(&mut self) -> Result<Option<Expr>, AsError> {
        let start = self.span().start;
        // Optional leading `async`: `async x => …` / `async (params) => …`. Only
        // commit to consuming it if an arrow actually follows.
        let is_async = if *self.peek() == Tok::Async {
            let next = &self.tokens[self.pos + 1].tok;
            let looks_like_arrow = match next {
                Tok::Ident(_) => self.tokens.get(self.pos + 2).map(|t| &t.tok) == Some(&Tok::FatArrow),
                Tok::LParen => {
                    let saved = self.pos;
                    self.pos += 1;
                    let ok = self.parens_then_arrow();
                    self.pos = saved;
                    ok
                }
                _ => false,
            };
            if !looks_like_arrow {
                return Ok(None);
            }
            self.advance(); // consume `async`
            true
        } else {
            false
        };
        // Single-parameter form: `ident => …`
        if let Tok::Ident(name) = self.peek().clone() {
            if self.tokens[self.pos + 1].tok == Tok::FatArrow {
                self.advance(); // ident
                self.advance(); // =>
                let body = self.arrow_body()?;
                let end = self.prev_end();
                return Ok(Some(Expr {
                    kind: ExprKind::Arrow {
                        params: vec![crate::ast::Param { name, ty: None }],
                        body: Box::new(body),
                        is_async,
                    },
                    span: Span::new(start, end),
                }));
            }
            return Ok(None);
        }
        // Parenthesized form: `( params ) => …`. Scan ahead to find the matching
        // `)` and check whether `=>` follows; only then commit to arrow parsing.
        if *self.peek() == Tok::LParen && self.parens_then_arrow() {
            let params = self.param_list()?;
            self.eat(&Tok::FatArrow)?;
            let body = self.arrow_body()?;
            let end = self.prev_end();
            return Ok(Some(Expr {
                kind: ExprKind::Arrow { params, body: Box::new(body), is_async },
                span: Span::new(start, end),
            }));
        }
        Ok(None)
    }

    /// Look ahead from a `(` to its matching `)` and report whether the next
    /// token after the `)` is `=>`. Does not consume tokens.
    fn parens_then_arrow(&self) -> bool {
        let mut depth = 0usize;
        let mut i = self.pos;
        while i < self.tokens.len() {
            match self.tokens[i].tok {
                Tok::LParen => depth += 1,
                Tok::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(
                            self.tokens.get(i + 1).map(|t| &t.tok),
                            Some(Tok::FatArrow)
                        );
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    fn arrow_body(&mut self) -> Result<ArrowBody, AsError> {
        if *self.peek() == Tok::LBrace {
            Ok(ArrowBody::Block(self.block()?))
        } else {
            Ok(ArrowBody::Expr(Box::new(self.assignment()?)))
        }
    }

    fn coalesce(&mut self) -> Result<Expr, AsError> {
        let mut left = self.logic_or()?;
        while *self.peek() == Tok::QuestionQuestion {
            self.advance();
            let right = self.logic_or()?;
            left = Self::make_binary(left, BinOp::Coalesce, right);
        }
        Ok(left)
    }

    fn logic_or(&mut self) -> Result<Expr, AsError> {
        let mut left = self.logic_and()?;
        while *self.peek() == Tok::PipePipe {
            self.advance();
            let right = self.logic_and()?;
            left = Self::make_binary(left, BinOp::Or, right);
        }
        Ok(left)
    }

    fn logic_and(&mut self) -> Result<Expr, AsError> {
        let mut left = self.equality()?;
        while *self.peek() == Tok::AmpAmp {
            self.advance();
            let right = self.equality()?;
            left = Self::make_binary(left, BinOp::And, right);
        }
        Ok(left)
    }

    fn equality(&mut self) -> Result<Expr, AsError> {
        let mut left = self.comparison()?;
        loop {
            let op = match self.peek() {
                Tok::EqEq => BinOp::Eq,
                Tok::BangEq => BinOp::Ne,
                _ => break,
            };
            self.advance();
            let right = self.comparison()?;
            left = Self::make_binary(left, op, right);
        }
        Ok(left)
    }

    fn comparison(&mut self) -> Result<Expr, AsError> {
        let mut left = self.additive()?;
        loop {
            let op = match self.peek() {
                Tok::Lt => BinOp::Lt,
                Tok::Le => BinOp::Le,
                Tok::Gt => BinOp::Gt,
                Tok::Ge => BinOp::Ge,
                _ => break,
            };
            self.advance();
            let right = self.additive()?;
            left = Self::make_binary(left, op, right);
        }
        Ok(left)
    }

    fn additive(&mut self) -> Result<Expr, AsError> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.multiplicative()?;
            left = Self::make_binary(left, op, right);
        }
        Ok(left)
    }

    fn multiplicative(&mut self) -> Result<Expr, AsError> {
        let mut left = self.exponent()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Mod,
                _ => break,
            };
            self.advance();
            let right = self.exponent()?;
            left = Self::make_binary(left, op, right);
        }
        Ok(left)
    }

    fn exponent(&mut self) -> Result<Expr, AsError> {
        let base = self.unary()?;
        if *self.peek() == Tok::StarStar {
            self.advance();
            // right-associative: 2 ** 3 ** 2 == 2 ** (3 ** 2)
            let exp = self.exponent()?;
            Ok(Self::make_binary(base, BinOp::Pow, exp))
        } else {
            Ok(base)
        }
    }

    fn unary(&mut self) -> Result<Expr, AsError> {
        let start = self.span().start;
        // `await` is a prefix operator at unary precedence (spec §7).
        if *self.peek() == Tok::Await {
            self.advance();
            let operand = self.unary()?;
            let span = Span::new(start, operand.span.end);
            return Ok(Expr {
                kind: ExprKind::Await(Box::new(operand)),
                span,
            });
        }
        let op = match self.peek() {
            Tok::Minus => Some(UnOp::Neg),
            Tok::Bang => Some(UnOp::Not),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let operand = self.unary()?;
            let span = Span::new(start, operand.span.end);
            return Ok(Expr {
                kind: ExprKind::Unary { op, expr: Box::new(operand) },
                span,
            });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, AsError> {
        let mut expr = self.primary()?;
        loop {
            match self.peek() {
                Tok::LParen => {
                    self.advance();
                    let mut args = Vec::new();
                    if *self.peek() != Tok::RParen {
                        loop {
                            args.push(self.expr()?);
                            if *self.peek() == Tok::Comma {
                                self.advance();
                                if *self.peek() == Tok::RParen {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }
                    }
                    self.eat(&Tok::RParen)?;
                    let span = Span::new(expr.span.start, self.prev_end());
                    expr = Expr { kind: ExprKind::Call { callee: Box::new(expr), args }, span };
                }
                Tok::LBracket => {
                    self.advance();
                    let index = self.expr()?;
                    self.eat(&Tok::RBracket)?;
                    let span = Span::new(expr.span.start, self.prev_end());
                    expr = Expr {
                        kind: ExprKind::Index { object: Box::new(expr), index: Box::new(index) },
                        span,
                    };
                }
                Tok::Dot => {
                    self.advance();
                    let name = match self.advance() {
                        Tok::Ident(name) => name,
                        other => {
                            return Err(AsError::at(
                                format!("expected a property name after '.', found {:?}", other),
                                self.tokens[self.pos - 1].span,
                            ))
                        }
                    };
                    let span = Span::new(expr.span.start, self.prev_end());
                    expr = Expr { kind: ExprKind::Member { object: Box::new(expr), name }, span };
                }
                Tok::QuestionDot => {
                    self.advance();
                    let name = match self.advance() {
                        Tok::Ident(name) => name,
                        other => {
                            return Err(AsError::at(
                                format!("expected a property name after '?.', found {:?}", other),
                                self.tokens[self.pos - 1].span,
                            ))
                        }
                    };
                    let span = Span::new(expr.span.start, self.prev_end());
                    expr = Expr { kind: ExprKind::OptMember { object: Box::new(expr), name }, span };
                }
                Tok::Question => {
                    self.advance();
                    let span = Span::new(expr.span.start, self.prev_end());
                    expr = Expr { kind: ExprKind::Try(Box::new(expr)), span };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn primary(&mut self) -> Result<Expr, AsError> {
        let tok_span = self.span();
        let kind = match self.advance() {
            Tok::Number(n) => ExprKind::Number(n),
            Tok::Str(s) => ExprKind::Str(s),
            Tok::True => ExprKind::Bool(true),
            Tok::False => ExprKind::Bool(false),
            Tok::Nil => ExprKind::Nil,
            Tok::Ident(name) => ExprKind::Ident(name),
            Tok::LParen => {
                let inner = self.expr()?;
                self.eat(&Tok::RParen)?;
                // Preserve the parentheses so they break an optional chain
                // (`(a?.b).c` must not short-circuit). See ExprKind::Paren.
                let span = Span::new(tok_span.start, self.prev_end());
                return Ok(Expr { kind: ExprKind::Paren(Box::new(inner)), span });
            }
            Tok::LBracket => {
                let mut items = Vec::new();
                if *self.peek() != Tok::RBracket {
                    loop {
                        items.push(self.expr()?);
                        if *self.peek() == Tok::Comma {
                            self.advance();
                            if *self.peek() == Tok::RBracket {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                self.eat(&Tok::RBracket)?;
                let span = Span::new(tok_span.start, self.prev_end());
                return Ok(Expr { kind: ExprKind::Array(items), span });
            }
            Tok::LBrace => {
                let mut entries = Vec::new();
                if *self.peek() != Tok::RBrace {
                    loop {
                        let key = match self.advance() {
                            Tok::Ident(name) => name,
                            Tok::Str(s) => s,
                            other => {
                                return Err(AsError::at(
                                    format!("expected object key, found {:?}", other),
                                    self.tokens[self.pos - 1].span,
                                ))
                            }
                        };
                        self.eat(&Tok::Colon)?;
                        let value = self.expr()?;
                        entries.push((key, value));
                        if *self.peek() == Tok::Comma {
                            self.advance();
                            if *self.peek() == Tok::RBrace {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                self.eat(&Tok::RBrace)?;
                let span = Span::new(tok_span.start, self.prev_end());
                return Ok(Expr { kind: ExprKind::Object(entries), span });
            }
            Tok::TemplateStr(s) => {
                let parts = vec![crate::ast::TemplatePart::Lit(s)];
                let span = Span::new(tok_span.start, self.prev_end());
                return Ok(Expr { kind: ExprKind::Template { parts }, span });
            }
            Tok::TemplateStart(s) => {
                let mut parts = vec![crate::ast::TemplatePart::Lit(s)];
                loop {
                    let e = self.expr()?;
                    parts.push(crate::ast::TemplatePart::Expr(Box::new(e)));
                    match self.advance() {
                        Tok::TemplateMiddle(s) => parts.push(crate::ast::TemplatePart::Lit(s)),
                        Tok::TemplateEnd(s) => {
                            parts.push(crate::ast::TemplatePart::Lit(s));
                            break;
                        }
                        other => {
                            return Err(AsError::at(
                                format!("malformed template, found {:?}", other),
                                self.tokens[self.pos - 1].span,
                            ))
                        }
                    }
                }
                let span = Span::new(tok_span.start, self.prev_end());
                return Ok(Expr { kind: ExprKind::Template { parts }, span });
            }
            Tok::Match => {
                let subject = self.expr()?;
                self.eat(&Tok::LBrace)?;
                let mut arms = Vec::new();
                while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
                    // pattern: `_` (wildcard) or expr ( `|` expr )*
                    // The identifier `_` lexes as `Tok::Ident("_")`.
                    let is_wildcard = matches!(self.peek(), Tok::Ident(s) if s == "_");
                    let patterns = if is_wildcard {
                        self.advance();
                        None
                    } else {
                        let mut pats = vec![self.coalesce()?];
                        while *self.peek() == Tok::Pipe {
                            self.advance();
                            pats.push(self.coalesce()?);
                        }
                        Some(pats)
                    };
                    self.eat(&Tok::FatArrow)?;
                    let body = self.expr()?;
                    arms.push(crate::ast::MatchArm { patterns, body });
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::RBrace)?;
                let span = Span::new(tok_span.start, self.prev_end());
                return Ok(Expr { kind: ExprKind::Match { subject: Box::new(subject), arms }, span });
            }
            other => return Err(AsError::at(format!("unexpected token {:?}", other), tok_span)),
        };
        Ok(Expr { kind, span: tok_span })
    }
}

/// Whether an expression can be the target of an assignment.
fn is_assignable(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::Ident(_) | ExprKind::Index { .. } | ExprKind::Member { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn sexpr(src: &str) -> String {
        let tokens = lex(src).unwrap();
        let stmts = parse(&tokens).unwrap();
        match &stmts[0] {
            Stmt::Expr(e) => e.to_string(),
            _ => panic!("expected an expression statement"),
        }
    }

    #[test]
    fn parses_array_destructuring_let() {
        let toks = lex("let [a, b] = pair").unwrap();
        let prog = parse(&toks).unwrap();
        match &prog[0] {
            Stmt::LetDestructure { names, mutable, .. } => {
                assert_eq!(names, &["a".to_string(), "b".to_string()]);
                assert!(*mutable);
            }
            other => panic!("expected LetDestructure, got {other:?}"),
        }
    }

    #[test]
    fn parses_try_operator() {
        assert_eq!(sexpr("readFile(p)?"), "(? (call readFile p))");
    }

    #[test]
    fn parses_type_annotations() {
        assert!(parse(&lex("let x: number = 5").unwrap()).is_ok());
        assert!(parse(&lex("fn add(a: number, b: number): number { return a + b }").unwrap()).is_ok());
        assert!(parse(&lex("let xs: array<number> = [1, 2]").unwrap()).is_ok());
        assert!(parse(&lex("let r: Result<string> = Ok(\"x\")").unwrap()).is_ok());
        assert!(parse(&lex("let u: number | nil = nil").unwrap()).is_ok());
        assert!(parse(&lex("let t: [number, string] = [1, \"a\"]").unwrap()).is_ok());
    }

    #[test]
    fn trailing_commas_are_allowed() {
        // arrays, calls, objects, and (separately) param lists
        assert_eq!(sexpr("[1, 2, 3,]"), "[1 2 3]");
        assert_eq!(sexpr("f(1, 2,)"), "(call f 1 2)");
        assert_eq!(sexpr("({a: 1, b: 2,}).a"), "(. {a: 1 b: 2} a)");
        // param lists with a trailing comma must parse
        assert!(parse(&lex("fn g(a, b,) { return a }").unwrap()).is_ok());
        // empty literals and non-trailing forms still work
        assert_eq!(sexpr("[]"), "[]");
        assert_eq!(sexpr("f()"), "(call f)");
        assert_eq!(sexpr("[1, 2, 3]"), "[1 2 3]");
    }

    #[test]
    fn multiplication_binds_tighter_than_addition() {
        assert_eq!(sexpr("1 + 2 * 3"), "(+ 1 (* 2 3))");
    }

    #[test]
    fn parentheses_override_precedence() {
        assert_eq!(sexpr("(1 + 2) * 3"), "(* (+ 1 2) 3)");
    }

    #[test]
    fn parses_a_call() {
        assert_eq!(sexpr("print(\"hi\")"), "(call print \"hi\")");
    }

    #[test]
    fn comparison_binds_looser_than_arithmetic() {
        assert_eq!(sexpr("1 + 2 < 3"), "(< (+ 1 2) 3)");
    }

    #[test]
    fn logical_and_binds_tighter_than_or() {
        assert_eq!(sexpr("a || b && c"), "(|| a (&& b c))");
    }

    #[test]
    fn coalesce_is_loosest() {
        assert_eq!(sexpr("a || b ?? c"), "(?? (|| a b) c)");
    }

    #[test]
    fn match_bare_ident_pattern_parses() {
        assert!(parse(&lex("match x { y => 1, _ => 2 }").unwrap()).is_ok());
    }

    #[test]
    fn exponent_is_right_associative_and_tightest() {
        assert_eq!(sexpr("2 ** 3 ** 2"), "(** 2 (** 3 2))");
        assert_eq!(sexpr("2 * 3 ** 2"), "(* 2 (** 3 2))");
    }

    #[test]
    fn not_is_unary() {
        assert_eq!(sexpr("!a"), "(! a)");
    }

    #[test]
    fn parses_assignment() {
        assert_eq!(sexpr("x = 5"), "(= x 5)");
    }

    #[test]
    fn assignment_is_right_associative() {
        assert_eq!(sexpr("x = y = 1"), "(= x (= y 1))");
    }

    #[test]
    fn compound_assignment_desugars() {
        assert_eq!(sexpr("x += 2"), "(= x (+ x 2))");
    }

    #[test]
    fn parses_single_param_arrow() {
        assert_eq!(sexpr("x => x + 1"), "(arrow [x])");
    }

    #[test]
    fn parses_multi_param_arrow() {
        assert_eq!(sexpr("(a, b) => a + b"), "(arrow [a b])");
    }

    #[test]
    fn parenthesized_non_arrow_still_works() {
        assert_eq!(sexpr("(1 + 2) * 3"), "(* (+ 1 2) 3)");
    }

    #[test]
    fn parses_object_and_member() {
        assert_eq!(sexpr("({a: 1}).a"), "(. {a: 1} a)");
    }

    #[test]
    fn binary_span_covers_both_operands() {
        let tokens = lex("1 + 2").unwrap();
        let stmts = parse(&tokens).unwrap();
        match &stmts[0] {
            Stmt::Expr(e) => assert_eq!(e.span, Span::new(0, 5)),
            _ => panic!("expected an expression statement"),
        }
    }

    #[test]
    fn parses_async_fn_decl() {
        let stmts = parse(&lex("async fn fetch(x) { return x }").unwrap()).unwrap();
        match &stmts[0] {
            Stmt::Fn { name, is_async, .. } => {
                assert_eq!(name, "fetch");
                assert!(is_async);
            }
            other => panic!("expected an async fn decl, got {other:?}"),
        }
    }

    #[test]
    fn await_parses_at_unary_precedence() {
        // `await f()` => Await(Call(f)); `await a + b` => (await a) + b.
        assert_eq!(sexpr("await f()"), "(await (call f))");
        assert_eq!(sexpr("await a + b"), "(+ (await a) b)");
    }

    #[test]
    fn parses_async_arrow() {
        // Both single-param and parenthesized async arrows parse.
        assert_eq!(sexpr("async x => x + 1"), "(arrow [x])");
        assert_eq!(sexpr("async (a, b) => a + b"), "(arrow [a b])");
        let stmts = parse(&lex("let g = async (n) => n + 1").unwrap()).unwrap();
        assert!(matches!(&stmts[0], Stmt::Let { .. }));
    }
}
