//! Recursive-descent / precedence-climbing parser.

use crate::ast::{ArrowBody, BinOp, Expr, ExprKind, Stmt, UnOp};
use crate::error::AsError;
use crate::span::Span;
use crate::token::{is_ident_like, Tok, Token};

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
        let start = self.span().start; // at the `fn` keyword
        self.eat(&Tok::Fn)?;
        // `fn*` / `async fn*` — a generator declaration. The `*` immediately
        // follows the `fn` keyword.
        let is_generator = if *self.peek() == Tok::Star {
            self.advance();
            true
        } else {
            false
        };
        let name_span = self.span();
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
        let span = Span::new(start, self.prev_end());
        Ok(Stmt::Fn { name, params, ret, body, is_async, is_generator, span, name_span })
    }

    fn enum_decl(&mut self) -> Result<Stmt, AsError> {
        let start = self.span().start;
        self.eat(&Tok::Enum)?;
        let name_span = self.span();
        let name = match self.advance() {
            Tok::Ident(n) => n,
            other => return Err(AsError::at(format!("expected enum name, found {:?}", other), self.tokens[self.pos - 1].span)),
        };
        self.eat(&Tok::LBrace)?;
        let mut variants = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            let vname_span = self.span();
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
            variants.push(crate::ast::EnumVariantDecl { name: vname, value, name_span: vname_span });
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.eat(&Tok::RBrace)?;
        let span = Span::new(start, self.prev_end());
        Ok(Stmt::Enum { name, variants, span, name_span })
    }

    fn class_decl(&mut self) -> Result<Stmt, AsError> {
        let start = self.span().start;
        self.eat(&Tok::Class)?;
        let name_span = self.span();
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
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            // A member starting with `async` or `fn` is a method; otherwise a field.
            if *self.peek() == Tok::Async || *self.peek() == Tok::Fn {
                let mstart = self.span().start;
                let is_async = if *self.peek() == Tok::Async {
                    self.advance();
                    true
                } else {
                    false
                };
                self.eat(&Tok::Fn)?;
                let is_generator = if *self.peek() == Tok::Star {
                    self.advance();
                    true
                } else {
                    false
                };
                let mname_span = self.span();
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
                let mspan = Span::new(mstart, self.prev_end());
                methods.push(crate::ast::MethodDecl {
                    name: mname,
                    params,
                    ret,
                    body,
                    is_async,
                    is_generator,
                    span: mspan,
                    name_span: mname_span,
                });
            } else {
                // Field declaration: Ident ["?"] ":" type ["=" expr]
                let fstart = self.span().start;
                let fname_span = self.span();
                let fname = match self.advance() {
                    Tok::Ident(n) => n,
                    other => return Err(AsError::at(format!("expected a field name or method, found {:?}", other), self.tokens[self.pos - 1].span)),
                };
                // `name?:` marker — lower to Optional below.
                let marker_optional = if *self.peek() == Tok::Question {
                    self.advance();
                    true
                } else {
                    false
                };
                self.eat(&Tok::Colon)?;
                let mut ty = self.parse_type()?;
                if marker_optional && !matches!(ty, crate::ast::Type::Optional(_)) {
                    ty = crate::ast::Type::Optional(Box::new(ty));
                }
                let default = if *self.peek() == Tok::Eq {
                    self.advance();
                    Some(self.expr()?)
                } else {
                    None
                };
                let fspan = Span::new(fstart, self.prev_end());
                fields.push(crate::ast::FieldDecl { name: fname, ty, default, span: fspan, name_span: fname_span });
            }
        }
        self.eat(&Tok::RBrace)?;
        let span = Span::new(start, self.prev_end());
        Ok(Stmt::Class { name, superclass, fields, methods, span, name_span })
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
        let atom = match self.advance() {
            Tok::Nil => Type::Nil,
            Tok::Fn => Type::Fn,
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
                Type::Tuple(parts)
            }
            Tok::Ident(name) => match name.as_str() {
                "number" => Type::Number,
                "string" => Type::String,
                "bool" => Type::Bool,
                "any" => Type::Any,
                "object" => Type::Object,
                "error" => Type::Error,
                "array" => {
                    self.eat(&Tok::Lt)?;
                    let inner = self.parse_type()?;
                    self.eat(&Tok::Gt)?;
                    Type::Array(Box::new(inner))
                }
                "Result" => {
                    self.eat(&Tok::Lt)?;
                    let inner = self.parse_type()?;
                    self.eat(&Tok::Gt)?;
                    Type::Result(Box::new(inner))
                }
                "future" => {
                    self.eat(&Tok::Lt)?;
                    let inner = self.parse_type()?;
                    self.eat(&Tok::Gt)?;
                    Type::Future(Box::new(inner))
                }
                "map" => {
                    self.eat(&Tok::Lt)?;
                    let k = self.parse_type()?;
                    self.eat(&Tok::Comma)?;
                    let v = self.parse_type()?;
                    self.eat(&Tok::Gt)?;
                    Type::Map(Box::new(k), Box::new(v))
                }
                _ => Type::Named(name),
            },
            other => return Err(AsError::at(format!("expected a type, found {:?}", other), span)),
        };
        // `T?` nullable suffix (sugar for `T | nil`). Only reachable in type
        // position (after `:` / inside `<...>`), so it never collides with the
        // expression-level `?` (ternary / propagate).
        if *self.peek() == Tok::Question {
            self.advance();
            Ok(Type::Optional(Box::new(atom)))
        } else {
            Ok(atom)
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
                let name_span = self.tokens[self.pos - 1].span;
                let ty = if *self.peek() == Tok::Colon {
                    self.advance();
                    Some(self.parse_type()?)
                } else {
                    None
                };
                params.push(crate::ast::Param { name, ty, name_span });
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
        // `for await (x in e)` — async iteration over a generator or a native
        // stream handle. The `await` sits between `for` and `(`.
        let for_await = if *self.peek() == Tok::Await {
            self.advance();
            true
        } else {
            false
        };
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
                // The iterable is a general expression. If it's exactly a `..`
                // range, keep the allocation-free lazy ForRange path; otherwise
                // fall back to ForOf (which iterates the resulting array).
                let iter = self.expr()?;
                self.eat(&Tok::RParen)?;
                let body = self.block()?;
                // `for await` always uses the async-iteration ForOf path (even
                // over a `..` range it would be a non-iterable Tier-2 error, but
                // a range is never async-iterable so keep it ForOf for the error).
                if !for_await {
                    if let ExprKind::Binary { op: BinOp::Range, lhs, rhs } = iter.kind {
                        return Ok(Stmt::ForRange { var, start: *lhs, end: *rhs, body });
                    }
                }
                Ok(Stmt::ForOf { var, iter, body, for_await })
            }
            Tok::Of => {
                let iter = self.expr()?;
                self.eat(&Tok::RParen)?;
                let body = self.block()?;
                Ok(Stmt::ForOf { var, iter, body, for_await })
            }
            other => Err(AsError::at(
                format!("expected 'in' or 'of' in for-loop, found {:?}", other),
                self.tokens[self.pos - 1].span,
            )),
        }
    }

    fn let_stmt(&mut self, mutable: bool) -> Result<Stmt, AsError> {
        let start = self.span().start;
        self.advance(); // consume `let` / `const`
        // `let [a, b] = expr` — array destructuring binding (spec §6).
        if *self.peek() == Tok::LBracket {
            self.advance(); // consume '['
            let mut names = Vec::new();
            let mut name_spans = Vec::new();
            if *self.peek() != Tok::RBracket {
                loop {
                    let span = self.span();
                    match self.advance() {
                        Tok::Ident(n) => {
                            names.push(n);
                            name_spans.push(span);
                        }
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
            let span = Span::new(start, self.prev_end());
            return Ok(Stmt::LetDestructure { names, value, mutable, span, name_spans });
        }
        // `let {a, b as local} = expr` — object destructuring binding.
        if *self.peek() == Tok::LBrace {
            self.advance(); // consume '{'
            let mut bindings = Vec::new();
            let rest: Option<(String, Span)> = None; // populated in a later phase
            if *self.peek() != Tok::RBrace {
                loop {
                    let key_span = self.span();
                    let key = match self.advance() {
                        Tok::Ident(n) => n,
                        Tok::Str(s) => s,
                        other => return Err(AsError::at(
                            format!("expected a key in object pattern, found {:?}", other),
                            self.tokens[self.pos - 1].span)),
                    };
                    let (binding, binding_span) =
                        if matches!(self.peek(), Tok::Ident(s) if s == "as") {
                            self.advance();
                            let bspan = self.span();
                            match self.advance() {
                                Tok::Ident(b) => (b, bspan),
                                other => return Err(AsError::at(
                                    format!("expected a local name after 'as', found {:?}", other),
                                    self.tokens[self.pos - 1].span)),
                            }
                        } else {
                            if !is_ident_like(&key) {
                                return Err(AsError::at(
                                    format!("key {:?} is not a valid binding name; use `as`", key),
                                    key_span));
                            }
                            (key.clone(), key_span)
                        };
                    bindings.push(crate::ast::ObjBinding { key, binding, key_span, binding_span });
                    if *self.peek() == Tok::Comma {
                        self.advance();
                        if *self.peek() == Tok::RBrace { break; }
                    } else { break; }
                }
            }
            self.eat(&Tok::RBrace)?;
            self.eat(&Tok::Eq)?;
            let value = self.expr()?;
            let span = Span::new(start, self.prev_end());
            return Ok(Stmt::LetDestructureObject { bindings, rest, value, mutable, span });
        }
        let name_span = self.span();
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
        // For `let` (mutable) the initializer is optional (`let x`, `let x: T`);
        // a later assignment supplies the value. `const` (immutable) must be
        // initialized at the declaration.
        let value = if *self.peek() == Tok::Eq {
            self.advance();
            Some(self.expr()?)
        } else if mutable {
            None
        } else {
            return Err(AsError::at("const declarations must be initialized", self.span()));
        };
        let span = Span::new(start, self.prev_end());
        Ok(Stmt::Let { name, ty, value, mutable, span, name_span })
    }

    fn expr(&mut self) -> Result<Expr, AsError> {
        self.assignment()
    }

    fn assignment(&mut self) -> Result<Expr, AsError> {
        // `yield` / `yield <expr>` — a prefix expression at the lowest (assignment)
        // precedence (spec §7, like JS `yield`). An operand is present unless the
        // next token is a terminator (`)`, `}`, `]`, `,`, `;`, EOF) — those forms
        // are a bare `yield`.
        if *self.peek() == Tok::Yield {
            let start = self.span().start;
            self.advance(); // consume `yield`
            // An operand is present iff the next token can begin an expression.
            // AScript has no newline tokens (ASI is parser-driven), so a bare
            // `yield` is recognized by what follows: a terminator (`)`, `}`, `,`,
            // `;`, `:`, EOF) or a statement keyword (`let`, `return`, `if`, …)
            // that cannot start an expression ends the `yield`.
            let operand = if starts_expression(self.peek()) {
                Some(Box::new(self.assignment()?))
            } else {
                None
            };
            let end = self.prev_end();
            return Ok(Expr {
                kind: ExprKind::Yield(operand),
                span: Span::new(start, end),
            });
        }
        // Arrow functions: `x => …` or `(a, b) => …`. Detect without breaking
        // ordinary parenthesized expressions by checking ahead for `=>`.
        if let Some(arrow) = self.try_arrow()? {
            return Ok(arrow);
        }

        let target = self.ternary()?;

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
                let name_span = self.tokens[self.pos].span;
                self.advance(); // ident
                self.advance(); // =>
                let body = self.arrow_body()?;
                let end = self.prev_end();
                return Ok(Some(Expr {
                    kind: ExprKind::Arrow {
                        params: vec![crate::ast::Param { name, ty: None, name_span }],
                        body: Box::new(body),
                        is_async,
                        is_generator: false,
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
                kind: ExprKind::Arrow { params, body: Box::new(body), is_async, is_generator: false },
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

    /// The conditional operator `cond ? then : els` (grammar PREC.ternary, just
    /// above assignment). Right-associative: `a ? b : c ? d : e` parses as
    /// `a ? b : (c ? d : e)`.
    ///
    /// `?` is overloaded — it is also the postfix Result-propagation operator
    /// (`expr?`, parsed in `postfix`). They are disambiguated by
    /// `question_begins_ternary`, which `postfix` consults so it never swallows a
    /// ternary `?` as a `Try`. By the time control reaches here, any `?` left
    /// unconsumed by `postfix` is therefore a ternary, so a bare token check suffices.
    fn ternary(&mut self) -> Result<Expr, AsError> {
        let cond = self.coalesce()?;
        if *self.peek() == Tok::Question {
            self.advance(); // `?`
            let then = self.assignment()?;
            self.eat(&Tok::Colon)?;
            // Right-associative: the `else` branch may itself be a ternary.
            let els = self.assignment()?;
            let span = Span::new(cond.span.start, els.span.end);
            return Ok(Expr {
                kind: ExprKind::Ternary {
                    cond: Box::new(cond),
                    then: Box::new(then),
                    els: Box::new(els),
                },
                span,
            });
        }
        Ok(cond)
    }

    /// Decide whether the `?` at the current position begins a ternary
    /// (vs. a postfix `Try`) by **speculatively parsing**: it is a ternary iff a
    /// consequent expression parses and is immediately followed by `:`. The trial
    /// parse is always rolled back via `self.pos`.
    ///
    /// Using the real expression grammar (rather than a raw token scan) makes this
    /// respect statement boundaries automatically — `a?` followed by an adjacent
    /// `b ? c : d` statement does not fuse, and a `Try` result used as a ternary
    /// condition (`g()? ? a : b`) resolves correctly — matching the tree-sitter
    /// grammar's GLR resolution.
    fn question_begins_ternary(&mut self) -> bool {
        debug_assert_eq!(*self.peek(), Tok::Question);
        let saved = self.pos;
        self.advance(); // consume `?`
        let begins = self.assignment().is_ok() && *self.peek() == Tok::Colon;
        self.pos = saved; // roll back the trial parse unconditionally
        begins
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
        let mut left = self.range()?;
        loop {
            let op = match self.peek() {
                Tok::Lt => BinOp::Lt,
                Tok::Le => BinOp::Le,
                Tok::Gt => BinOp::Gt,
                Tok::Ge => BinOp::Ge,
                _ => break,
            };
            self.advance();
            let right = self.range()?;
            left = Self::make_binary(left, op, right);
        }
        Ok(left)
    }

    /// The range operator `..` (grammar PREC.range = 7): binds tighter than
    /// comparison but looser than additive (`1+1..5` parses as `(1+1)..5`).
    /// Left-associative, like the other binary levels.
    fn range(&mut self) -> Result<Expr, AsError> {
        let mut left = self.additive()?;
        while *self.peek() == Tok::DotDot {
            self.advance();
            let right = self.additive()?;
            left = Self::make_binary(left, BinOp::Range, right);
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

    /// Postfix Result operators `?` (propagate) and `!` (force-unwrap). They sit
    /// LOOSER than `await`/unary but TIGHTER than every binary operator, so
    /// `await x!` parses as `(await x)!` and `a! + b` as `(a!) + b`. Left-assoc.
    fn unwrap_tier(&mut self) -> Result<Expr, AsError> {
        let mut expr = self.unary()?;
        loop {
            match self.peek() {
                Tok::Question => {
                    // Leave a ternary `?` for `ternary()` higher up.
                    if self.question_begins_ternary() {
                        break;
                    }
                    self.advance();
                    let span = Span::new(expr.span.start, self.prev_end());
                    expr = Expr { kind: ExprKind::Try(Box::new(expr)), span };
                }
                Tok::Bang => {
                    self.advance();
                    let span = Span::new(expr.span.start, self.prev_end());
                    expr = Expr { kind: ExprKind::Unwrap(Box::new(expr)), span };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn exponent(&mut self) -> Result<Expr, AsError> {
        let base = self.unwrap_tier()?;
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
                            if *self.peek() == Tok::DotDotDot {
                                self.advance();
                                args.push(crate::ast::CallArg::Spread(self.expr()?));
                            } else {
                                args.push(crate::ast::CallArg::Pos(self.expr()?));
                            }
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
                        if *self.peek() == Tok::DotDotDot {
                            self.advance();
                            items.push(crate::ast::ArrayElem::Spread(self.expr()?));
                        } else {
                            items.push(crate::ast::ArrayElem::Item(self.expr()?));
                        }
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
                        if *self.peek() == Tok::DotDotDot {
                            self.advance();
                            entries.push(crate::ast::ObjEntry::Spread(self.expr()?));
                        } else {
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
                            entries.push(crate::ast::ObjEntry::KV(key, value));
                        }
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

/// Whether `tok` can begin an expression. Used to decide if a `yield` carries an
/// operand: since AScript has no newline tokens, a bare `yield` is only
/// distinguishable from `yield <expr>` by whether what follows can start an
/// expression (a terminator or a statement keyword cannot).
fn starts_expression(tok: &Tok) -> bool {
    matches!(
        tok,
        Tok::Number(_)
            | Tok::Str(_)
            | Tok::Ident(_)
            | Tok::True
            | Tok::False
            | Tok::Nil
            | Tok::LParen
            | Tok::LBracket
            | Tok::LBrace
            | Tok::Minus
            | Tok::Bang
            | Tok::Await
            | Tok::Yield
            | Tok::Async
            | Tok::Match
            | Tok::TemplateStr(_)
            | Tok::TemplateStart(_)
    )
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
    fn parses_object_destructuring_shorthand_and_rename() {
        let p = parse(&lex("let {a, b as local} = obj").unwrap()).unwrap();
        match &p[0] {
            Stmt::LetDestructureObject { bindings, mutable, .. } => {
                assert!(*mutable);
                assert_eq!(bindings[0].key, "a"); assert_eq!(bindings[0].binding, "a");
                assert_eq!(bindings[1].key, "b"); assert_eq!(bindings[1].binding, "local");
            }
            other => panic!("expected LetDestructureObject, got {other:?}"),
        }
    }

    #[test]
    fn parses_object_destructuring_quoted_key() {
        let p = parse(&lex(r#"let {"weird key" as wk} = obj"#).unwrap()).unwrap();
        match &p[0] {
            Stmt::LetDestructureObject { bindings, .. } => {
                assert_eq!(bindings[0].key, "weird key"); assert_eq!(bindings[0].binding, "wk");
            }
            other => panic!("expected LetDestructureObject, got {other:?}"),
        }
    }

    #[test]
    fn object_destructuring_quoted_shorthand_is_error() {
        assert!(parse(&lex(r#"let {"weird key"} = obj"#).unwrap()).is_err());
    }

    #[test]
    fn parses_map_type_annotation() {
        let toks = lex("let m: map<string, number> = empty").unwrap();
        let prog = parse(&toks).unwrap();
        match &prog[0] {
            Stmt::Let { ty: Some(t), .. } => assert_eq!(t.to_string(), "map<string, number>"),
            other => panic!("expected typed let, got {other:?}"),
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
    fn parses_spread_in_array_object_call() {
        assert!(parse(&lex("let a = [...x, 1]").unwrap()).is_ok());
        assert!(parse(&lex("let o = {...x, k: 1}").unwrap()).is_ok());
        assert!(parse(&lex("f(...args, 2)").unwrap()).is_ok());
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
            Stmt::Fn { name, is_async, is_generator, .. } => {
                assert_eq!(name, "fetch");
                assert!(is_async);
                assert!(!is_generator);
            }
            other => panic!("expected an async fn decl, got {other:?}"),
        }
    }

    #[test]
    fn parses_generator_fn_decl() {
        let stmts = parse(&lex("fn* count() { yield 1 }").unwrap()).unwrap();
        match &stmts[0] {
            Stmt::Fn { name, is_async, is_generator, .. } => {
                assert_eq!(name, "count");
                assert!(!is_async);
                assert!(is_generator);
            }
            other => panic!("expected a generator fn decl, got {other:?}"),
        }
    }

    #[test]
    fn parses_async_generator_fn_decl() {
        let stmts = parse(&lex("async fn* g() { yield 1 }").unwrap()).unwrap();
        match &stmts[0] {
            Stmt::Fn { is_async, is_generator, .. } => {
                assert!(is_async);
                assert!(is_generator);
            }
            other => panic!("expected an async generator fn decl, got {other:?}"),
        }
    }

    #[test]
    fn parses_generator_method() {
        let stmts = parse(&lex("class C { fn* gen() { yield 1 } }").unwrap()).unwrap();
        match &stmts[0] {
            Stmt::Class { methods, .. } => {
                assert_eq!(methods[0].name, "gen");
                assert!(methods[0].is_generator);
            }
            other => panic!("expected a class with a generator method, got {other:?}"),
        }
    }

    #[test]
    fn parses_yield_with_and_without_operand() {
        assert_eq!(sexpr("yield 1"), "(yield 1)");
        assert_eq!(sexpr("yield x + 1"), "(yield (+ x 1))");
        // A bare `yield` terminated by `)`.
        assert_eq!(sexpr("(yield)"), "(yield)");
        // A bare `yield` followed by a statement keyword (`let` cannot start an
        // expression, so the `yield` does not consume it).
        let stmts = parse(&lex("fn* g() { yield\nlet x = 1 }").unwrap()).unwrap();
        match &stmts[0] {
            Stmt::Fn { body, .. } => {
                assert!(matches!(&body[0], Stmt::Expr(e) if matches!(e.kind, ExprKind::Yield(None))));
                assert!(matches!(&body[1], Stmt::Let { .. }));
            }
            other => panic!("expected fn body, got {other:?}"),
        }
        // `yield` IS an expression start, so `yield yield 2` nests (right-assoc).
        assert_eq!(sexpr("yield yield 2"), "(yield (yield 2))");
    }

    #[test]
    fn yield_resume_value_is_usable() {
        // `let a = yield "q"` — yield's value (the resume value) binds to `a`.
        let stmts = parse(&lex("fn* echo() { let a = yield \"q\" }").unwrap()).unwrap();
        assert!(matches!(&stmts[0], Stmt::Fn { .. }));
    }

    #[test]
    fn parses_for_await() {
        let stmts = parse(&lex("for await (x in gen()) { print(x) }").unwrap()).unwrap();
        match &stmts[0] {
            Stmt::ForOf { var, for_await, .. } => {
                assert_eq!(var, "x");
                assert!(for_await);
            }
            other => panic!("expected a for-await loop, got {other:?}"),
        }
        // A plain `for (x in xs)` is NOT for_await.
        let plain = parse(&lex("for (x in xs) { print(x) }").unwrap()).unwrap();
        match &plain[0] {
            Stmt::ForOf { for_await, .. } => assert!(!for_await),
            other => panic!("expected a plain for-of, got {other:?}"),
        }
    }

    #[test]
    fn unwrap_and_propagate_bind_looser_than_await() {
        // `!` and `?` apply to the resolved value, not the future.
        assert_eq!(sexpr("await f()!"), "(unwrap (await (call f)))");
        assert_eq!(sexpr("await f()?"), "(? (await (call f)))");
    }

    #[test]
    fn unwrap_binds_tighter_than_binary() {
        assert_eq!(sexpr("a! + b"), "(+ (unwrap a) b)");
        assert_eq!(sexpr("f()?"), "(? (call f))");
    }

    #[test]
    fn ternary_still_disambiguates_from_propagate() {
        // A `?` followed by `:` is still a ternary, not a Try.
        assert_eq!(sexpr("a ? b : c"), "(?: a b c)");
        assert_eq!(sexpr("g()? ? a : b"), "(?: (? (call g)) a b)");
    }

    #[test]
    fn await_parses_at_unary_precedence() {
        // `await f()` => Await(Call(f)); `await a + b` => (await a) + b.
        assert_eq!(sexpr("await f()"), "(await (call f))");
        assert_eq!(sexpr("await a + b"), "(+ (await a) b)");
    }

    #[test]
    fn optional_type_suffix_parses() {
        // `number?` in a let binding parses to Type::Optional(Number).
        let stmts = parse(&lex("let x: number? = nil").unwrap()).unwrap();
        match &stmts[0] {
            Stmt::Let { ty: Some(t), .. } => {
                assert_eq!(t.to_string(), "number?");
            }
            other => panic!("expected a typed let, got {other:?}"),
        }
    }

    #[test]
    fn optional_type_in_param_and_return() {
        let stmts = parse(&lex("fn f(a: string?): number? { return nil }").unwrap()).unwrap();
        match &stmts[0] {
            Stmt::Fn { params, ret: Some(r), .. } => {
                assert_eq!(params[0].ty.as_ref().unwrap().to_string(), "string?");
                assert_eq!(r.to_string(), "number?");
            }
            other => panic!("expected a typed fn, got {other:?}"),
        }
    }

    #[test]
    fn class_fields_both_spellings_parse() {
        let src = "class U {\n  id: number\n  nick: string?\n  avatar?: string\n  role: string = \"guest\"\n  fn init() {}\n}";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        match &stmts[0] {
            Stmt::Class { fields, methods, .. } => {
                assert_eq!(fields.len(), 4);
                assert_eq!(fields[0].name, "id");
                assert_eq!(fields[0].ty.to_string(), "number");
                // `string?` and `avatar?` both lower to Optional.
                assert_eq!(fields[1].ty.to_string(), "string?");
                assert_eq!(fields[2].name, "avatar");
                assert_eq!(fields[2].ty.to_string(), "string?");
                // default present
                assert!(fields[3].default.is_some());
                assert_eq!(methods.len(), 1);
            }
            other => panic!("expected a class, got {other:?}"),
        }
    }

    #[test]
    fn parses_async_arrow() {
        // Both single-param and parenthesized async arrows parse.
        assert_eq!(sexpr("async x => x + 1"), "(arrow [x])");
        assert_eq!(sexpr("async (a, b) => a + b"), "(arrow [a b])");
        let stmts = parse(&lex("let g = async (n) => n + 1").unwrap()).unwrap();
        assert!(matches!(&stmts[0], Stmt::Let { .. }));
    }

    #[test]
    fn parses_ternary() {
        assert_eq!(sexpr("a ? b : c"), "(?: a b c)");
        // Right-associative: a ? b : (c ? d : e)
        assert_eq!(sexpr("a ? b : c ? d : e"), "(?: a b (?: c d e))");
        // Binds looser than every binary op: (a + 1) ? (b * 2) : c
        assert_eq!(sexpr("a + 1 ? b * 2 : c"), "(?: (+ a 1) (* b 2) c)");
        // Looser than `??`.
        assert_eq!(sexpr("a ?? b ? c : d"), "(?: (?? a b) c d)");
        // A parenthesized ternary as the condition stays grouped.
        assert_eq!(sexpr("(a ? b : c) ? d : e"), "(?: (?: a b c) d e)");
    }

    #[test]
    fn ternary_vs_postfix_try_disambiguation() {
        // No following `:` → the `?` is a postfix Try, not a ternary.
        assert_eq!(sexpr("f()?"), "(? (call f))");
        // Try as a call argument (closing `)` before any `:`).
        assert_eq!(sexpr("g(f()?)"), "(call g (? (call f)))");
        // Try followed by subtraction — the `-` does NOT make it a ternary.
        assert_eq!(sexpr("a? - b"), "(- (? a) b)");
        // But a real ternary with a negative consequent is a ternary.
        assert_eq!(sexpr("a ? -b : c"), "(?: a (- b) c)");
        // A `:` belonging to a *later* statement must not be captured: here the
        // `?` is a Try and the `let y: T` colon is in the next statement.
        let stmts = parse(&lex("let x = f()?\nlet y: number = 9").unwrap()).unwrap();
        assert!(matches!(&stmts[0], Stmt::Let { .. }));
        assert!(matches!(&stmts[1], Stmt::Let { .. }));

        // A `Try` result used directly as a ternary condition: `g()? ? a : b`.
        assert_eq!(sexpr("g()? ? a : b"), "(?: (? (call g)) a b)");

        // A bare `expr?` statement followed by an adjacent ternary statement (no
        // separator) must NOT fuse — the scan respects statement boundaries.
        let stmts = parse(&lex("a?\nb ? c : d").unwrap()).unwrap();
        assert_eq!(stmts.len(), 2);
        assert!(matches!(&stmts[0], Stmt::Expr(e) if matches!(e.kind, ExprKind::Try(_))));
        assert!(matches!(&stmts[1], Stmt::Expr(e) if matches!(e.kind, ExprKind::Ternary { .. })));
    }
}
