//! Recursive-descent / precedence-climbing parser.

use crate::ast::{ArrowBody, BinOp, Expr, ExprKind, Stmt, UnOp};
use crate::error::AsError;
use crate::span::Span;
use crate::token::{is_ident_like, Tok, Token};

pub fn parse(tokens: &[Token]) -> Result<Vec<Stmt>, AsError> {
    let mut parser = Parser {
        tokens,
        pos: 0,
        pending_gt: 0,
        type_param_scope: Vec::new(),
    };
    parser.program()
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    /// Pending virtual `>` tokens produced by splitting a `Shr` (`>>`) in
    /// type-argument position (NUM §3.4). When `> 0`, the next `eat_type_gt` /
    /// `peek_type_gt` consumes a virtual `>` (the remainder of a `>>`) WITHOUT
    /// advancing the cursor. Type-arg closings nest strictly, so a simple counter
    /// suffices (the Rust/Java/C# nested-generics technique).
    pending_gt: usize,
    /// TYPE §6: the generic type-parameter names currently in scope (the params of
    /// the enclosing `fn`/`class`/`enum`/`interface` decl). A bare `Ident` in TYPE
    /// position that matches one of these lowers to `Type::Param` (runtime-erased)
    /// rather than `Type::Named`. Pushed by `parse_type_params`, popped after the
    /// decl body parses. A flat `Vec` (not a stack of frames) suffices because
    /// generic decls do not nest a SECOND generic param list in v1 (a method inside
    /// a generic class shares the class's params; per-method type params are a
    /// later additive feature). When that lands, this becomes a frame stack.
    type_param_scope: Vec<String>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Tok {
        &self.tokens[self.pos].tok
    }

    /// Look ahead `n` tokens past the cursor (0 = current). Returns `Tok::Eof`
    /// past the end. Used for small contextual-keyword lookahead (e.g. `static`).
    fn peek_nth(&self, n: usize) -> &Tok {
        self.tokens
            .get(self.pos + n)
            .map(|t| &t.tok)
            .unwrap_or(&Tok::Eof)
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
            // `worker fn` / `worker async fn` / `worker class` — contextual keyword like `async`.
            Tok::Ident(s) if s == "worker" && matches!(self.peek_nth(1), Tok::Fn | Tok::Async | Tok::Class) => {
                self.advance(); // consume contextual `worker`
                if *self.peek() == Tok::Class {
                    self.class_decl_inner(true)
                } else {
                    let is_async = if *self.peek() == Tok::Async { self.advance(); true } else { false };
                    self.fn_decl(is_async, true)
                }
            }
            Tok::Fn => self.fn_decl(false, false),
            // `async fn` is a declaration; `async (…) =>` / `async x =>` are
            // arrow expressions, handled by the expression path below.
            Tok::Async if self.tokens[self.pos + 1].tok == Tok::Fn => {
                self.advance(); // consume `async`
                self.fn_decl(true, false)
            }
            Tok::Enum => self.enum_decl(),
            Tok::Class => self.class_decl(),
            Tok::Interface => self.interface_decl(),
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
            Tok::Defer => {
                let start = self.span().start;
                self.advance(); // consume `defer`
                let awaited = if *self.peek() == Tok::Await {
                    self.advance();
                    true
                } else {
                    false
                };
                let call = self.expr()?;
                let span = Span::new(start, call.span.end);
                match &call.kind {
                    ExprKind::Call { args, .. } => {
                        if args.iter().any(|a| matches!(a, crate::ast::CallArg::Named { .. })) {
                            return Err(AsError::at(
                                "defer does not support named-argument calls — bind the value first or use an arrow",
                                span,
                            ));
                        }
                        Ok(Stmt::Defer { call, awaited, span })
                    }
                    _ => Err(AsError::at(
                        "defer requires a call — only a call expression can be deferred (write `defer (() => …)()` for inline cleanup)",
                        span,
                    )),
                }
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
                return Err(AsError::at(
                    "expected 'as' after '*' in import",
                    self.span(),
                ));
            }
            self.advance();
            let alias = match self.advance() {
                Tok::Ident(n) => n,
                other => {
                    return Err(AsError::at(
                        format!("expected namespace alias, found {:?}", other),
                        self.tokens[self.pos - 1].span,
                    ))
                }
            };
            crate::ast::ImportNames::Namespace(alias)
        } else {
            self.eat(&Tok::LBrace)?;
            let mut names = Vec::new();
            if *self.peek() != Tok::RBrace {
                loop {
                    match self.advance() {
                        Tok::Ident(n) => names.push(n),
                        other => {
                            return Err(AsError::at(
                                format!("expected import name, found {:?}", other),
                                self.tokens[self.pos - 1].span,
                            ))
                        }
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
            crate::ast::ImportNames::Named(names)
        };
        // `from`
        if !matches!(self.peek(), Tok::Ident(s) if s == "from") {
            return Err(AsError::at("expected 'from' in import", self.span()));
        }
        self.advance();
        let source = match self.advance() {
            Tok::Str(s) => s,
            other => {
                return Err(AsError::at(
                    format!("expected module path string, found {:?}", other),
                    self.tokens[self.pos - 1].span,
                ))
            }
        };
        Ok(Stmt::Import { names, source })
    }

    fn export_decl(&mut self) -> Result<Stmt, AsError> {
        self.eat(&Tok::Export)?;
        // Only declarations are exportable.
        let inner = match self.peek() {
            Tok::Let => self.let_stmt(true)?,
            Tok::Const => self.let_stmt(false)?,
            Tok::Fn => self.fn_decl(false, false)?,
            Tok::Async => {
                self.advance(); // consume `async`
                if *self.peek() != Tok::Fn {
                    return Err(AsError::at(
                        format!("expected 'fn' after 'async', found {:?}", self.peek()),
                        self.span(),
                    ));
                }
                self.fn_decl(true, false)?
            }
            // `export worker fn` / `export worker async fn` / `export worker class`
            Tok::Ident(s) if s == "worker" && matches!(self.peek_nth(1), Tok::Fn | Tok::Async | Tok::Class) => {
                self.advance(); // consume contextual `worker`
                if *self.peek() == Tok::Class {
                    self.class_decl_inner(true)?
                } else {
                    let is_async = if *self.peek() == Tok::Async { self.advance(); true } else { false };
                    self.fn_decl(is_async, true)?
                }
            }
            Tok::Class => self.class_decl()?,
            Tok::Enum => self.enum_decl()?,
            Tok::Interface => self.interface_decl()?,
            other => {
                return Err(AsError::at(
                    format!(
                        "only let/const/fn/class/enum/interface can be exported, found {:?}",
                        other
                    ),
                    self.span(),
                ))
            }
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

    fn fn_decl(&mut self, is_async: bool, is_worker: bool) -> Result<Stmt, AsError> {
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
        // TYPE §6: optional `<T, U: Bound>` generic param list (in scope for the
        // signature + body); runtime-erased, popped after the body parses.
        let type_params = self.parse_type_params()?;
        let params = self.param_list()?;
        let ret = if *self.peek() == Tok::Colon {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = self.block()?;
        self.pop_type_params(type_params.len());
        let span = Span::new(start, self.prev_end());
        Ok(Stmt::Fn {
            name,
            params,
            ret,
            body,
            is_async,
            is_generator,
            is_worker,
            span,
            name_span,
        })
    }

    fn enum_decl(&mut self) -> Result<Stmt, AsError> {
        let start = self.span().start;
        self.eat(&Tok::Enum)?;
        let name_span = self.span();
        let name = match self.advance() {
            Tok::Ident(n) => n,
            other => {
                return Err(AsError::at(
                    format!("expected enum name, found {:?}", other),
                    self.tokens[self.pos - 1].span,
                ))
            }
        };
        // TYPE §6: optional `<T>` generic param list, in scope for variant payload
        // field types (`enum Option<T> { Some(value: T), None }`).
        let type_params = self.parse_type_params()?;
        self.eat(&Tok::LBrace)?;
        let mut variants = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            let vname_span = self.span();
            let vname = match self.advance() {
                Tok::Ident(n) => n,
                other => {
                    return Err(AsError::at(
                        format!("expected variant name, found {:?}", other),
                        self.tokens[self.pos - 1].span,
                    ))
                }
            };
            let value = if *self.peek() == Tok::Eq {
                self.advance();
                Some(self.expr()?)
            } else {
                None
            };
            // ADT: a variant has EITHER a `= scalar` backing OR a `(…)` payload, never
            // both. The two ways this surfaces:
            //  - `Foo = 2 (int)` — a `(` trails the backing expr (separate tokens).
            //  - `Foo = 2(int)`  — the backing expr parsed AS a call (the `(` was
            //    swallowed). A scalar backing is never a call, so a `Call` backing is
            //    the both-form. (Enum backings are scalar literals/consts per spec.)
            let backing_is_call =
                matches!(&value, Some(e) if matches!(e.kind, ExprKind::Call { .. }));
            let payload = if *self.peek() == Tok::LParen || backing_is_call {
                if value.is_some() {
                    return Err(AsError::at(
                        "a variant cannot have both a '= value' backing and a '(…)' payload"
                            .to_string(),
                        self.span(),
                    ));
                }
                self.enum_payload_fields()?
            } else {
                Vec::new()
            };
            variants.push(crate::ast::EnumVariantDecl {
                name: vname,
                value,
                payload,
                name_span: vname_span,
            });
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.eat(&Tok::RBrace)?;
        self.pop_type_params(type_params.len());
        let span = Span::new(start, self.prev_end());
        Ok(Stmt::Enum {
            name,
            variants,
            span,
            name_span,
        })
    }

    /// IFACE §3: parse `interface Name [extends A, B] { (fn name(params)[: ret])* }`.
    /// Each requirement is a plain instance-method signature with NO body; `;`/newlines
    /// separate them (`skip_semicolons`). `async`/`fn*`/`static`/`worker` modifiers on a
    /// requirement are a parse error in v1. `extends` is contextual (lexes as Ident).
    fn interface_decl(&mut self) -> Result<Stmt, AsError> {
        let start = self.span().start;
        self.eat(&Tok::Interface)?;
        let name_span = self.span();
        let name = match self.advance() {
            Tok::Ident(n) => n,
            other => {
                return Err(AsError::at(
                    format!("expected interface name, found {:?}", other),
                    self.tokens[self.pos - 1].span,
                ))
            }
        };
        // TYPE §6: optional `<T>` generic param list (in scope for the method
        // requirement signatures). The `Interface` decl node already reserves a
        // `type_params: Vec<String>` field (IFACE §6.1), so populate it with the
        // declared names (bounds stay parser-only / static-checker-consumed).
        let type_param_nodes = self.parse_type_params()?;
        let type_params: Vec<String> =
            type_param_nodes.iter().map(|tp| tp.name.clone()).collect();
        // Optional `extends I1, I2, …` composition list (contextual keyword).
        let extends = self.parse_iface_name_list("extends")?;
        self.eat(&Tok::LBrace)?;
        let mut methods: Vec<crate::ast::MethodReqNode> = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            self.skip_semicolons();
            if *self.peek() == Tok::RBrace {
                break;
            }
            // Reject method MODIFIERS: an interface requirement is a plain instance
            // method. `static` lexes as Ident; `worker` is contextual; `async` is a
            // keyword; `fn*` is a star after `fn`.
            if *self.peek() == Tok::Async {
                return Err(AsError::at(
                    "an interface method requirement may not be 'async'",
                    self.span(),
                ));
            }
            if matches!(self.peek(), Tok::Ident(s) if s == "static") {
                return Err(AsError::at(
                    "an interface method requirement may not be 'static'",
                    self.span(),
                ));
            }
            if matches!(self.peek(), Tok::Ident(s) if s == "worker") {
                return Err(AsError::at(
                    "an interface method requirement may not be 'worker'",
                    self.span(),
                ));
            }
            let mstart = self.span().start;
            self.eat(&Tok::Fn)?;
            if *self.peek() == Tok::Star {
                return Err(AsError::at(
                    "an interface method requirement may not be a generator ('fn*')",
                    self.span(),
                ));
            }
            let mname_span = self.span();
            let mname = match self.advance() {
                Tok::Ident(n) => n,
                other => {
                    return Err(AsError::at(
                        format!("expected method name, found {:?}", other),
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
            let mspan = Span::new(mstart, self.prev_end());
            methods.push(crate::ast::MethodReqNode {
                name: mname,
                params,
                ret,
                span: mspan,
                name_span: mname_span,
            });
        }
        self.eat(&Tok::RBrace)?;
        self.pop_type_params(type_param_nodes.len());
        let span = Span::new(start, self.prev_end());
        Ok(Stmt::Interface {
            name,
            type_params,
            extends,
            methods,
            span,
            name_span,
        })
    }

    /// Parse an optional contextual-keyword-introduced comma list of names, e.g.
    /// `implements A, B` or `extends A, B` (interface composition). Returns `[]` when
    /// the keyword is absent. `keyword` lexes as a `Tok::Ident` (soft keyword).
    fn parse_iface_name_list(&mut self, keyword: &str) -> Result<Vec<String>, AsError> {
        let mut names = Vec::new();
        if matches!(self.peek(), Tok::Ident(s) if s == keyword) {
            self.advance(); // consume the contextual keyword
            loop {
                match self.advance() {
                    Tok::Ident(n) => names.push(n),
                    other => {
                        return Err(AsError::at(
                            format!("expected an interface name after '{}', found {:?}", keyword, other),
                            self.tokens[self.pos - 1].span,
                        ))
                    }
                }
                if *self.peek() == Tok::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        Ok(names)
    }

    /// ADT: parse a variant's `(field, field, …)` payload list. Each field is either
    /// named (`id: T`) or positional (`T`); the list must be UNIFORMLY one or the
    /// other (mixing is a parse error). A field type is REQUIRED. Assumes the current
    /// token is `(`.
    fn enum_payload_fields(&mut self) -> Result<Vec<crate::ast::VariantField>, AsError> {
        use crate::ast::VariantField;
        self.eat(&Tok::LParen)?;
        let mut fields: Vec<VariantField> = Vec::new();
        // `named` is decided by the FIRST field; subsequent fields must match.
        let mut named: Option<bool> = None;
        if *self.peek() != Tok::RParen {
            loop {
                let field_start = self.span();
                // A named field is `ident : Type`; a positional field is a bare Type.
                // Disambiguate by peeking for `ident` followed by `:`.
                let is_named = matches!(self.peek(), Tok::Ident(_))
                    && matches!(self.peek_nth(1), Tok::Colon);
                match named {
                    None => named = Some(is_named),
                    Some(prev) if prev != is_named => {
                        return Err(AsError::at(
                            "enum variant fields must be all named or all positional".to_string(),
                            field_start,
                        ));
                    }
                    _ => {}
                }
                let name = if is_named {
                    let n = match self.advance() {
                        Tok::Ident(n) => n,
                        other => {
                            return Err(AsError::at(
                                format!("expected field name, found {:?}", other),
                                self.tokens[self.pos - 1].span,
                            ))
                        }
                    };
                    self.eat(&Tok::Colon)?;
                    Some(std::rc::Rc::from(n.as_str()))
                } else {
                    None
                };
                let ty = self.parse_type()?;
                fields.push(VariantField { name, ty });
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
        if fields.is_empty() {
            return Err(AsError::at(
                "an enum variant payload must declare at least one field".to_string(),
                self.tokens[self.pos - 1].span,
            ));
        }
        Ok(fields)
    }

    fn class_decl(&mut self) -> Result<Stmt, AsError> {
        self.class_decl_inner(false)
    }

    fn class_decl_inner(&mut self, is_worker: bool) -> Result<Stmt, AsError> {
        let start = self.span().start;
        self.eat(&Tok::Class)?;
        let name_span = self.span();
        let name = match self.advance() {
            Tok::Ident(n) => n,
            other => {
                return Err(AsError::at(
                    format!("expected class name, found {:?}", other),
                    self.tokens[self.pos - 1].span,
                ))
            }
        };
        // TYPE §6: optional `<T>` generic param list, in scope for field types,
        // method signatures, and the `extends`/`implements` clauses.
        let type_params = self.parse_type_params()?;
        let superclass = if matches!(self.peek(), Tok::Ident(s) if s == "extends") {
            // `extends` is a soft keyword here (lexes as Ident)
            self.advance();
            match self.advance() {
                Tok::Ident(n) => Some(n),
                other => {
                    return Err(AsError::at(
                        format!("expected superclass name, found {:?}", other),
                        self.tokens[self.pos - 1].span,
                    ))
                }
            }
        } else {
            None
        };
        // IFACE: optional `implements I1, I2` clause (after `extends`, before body).
        let implements = self.parse_iface_name_list("implements")?;
        self.eat(&Tok::LBrace)?;
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            // `;` is an optional separator between (and after) class members.
            self.skip_semicolons();
            if *self.peek() == Tok::RBrace {
                break;
            }
            // A member starting with `async`, `fn`, `worker`, or `static` (followed
            // by `fn`/`async`/`worker`) is a method. `static` lexes as
            // `Tok::Ident("static")`; it is a method modifier ONLY when directly
            // followed by `fn`/`async`/`worker`, so `static: T` stays a field.
            // Factor out the `static` peek to avoid calling `self.peek()` twice.
            let at_static = matches!(self.peek(), Tok::Ident(s) if s == "static");
            let is_static_method = at_static
                && (matches!(self.peek_nth(1), Tok::Async | Tok::Fn)
                    || matches!(self.peek_nth(1), Tok::Ident(s) if s == "worker")
                        && matches!(self.peek_nth(2), Tok::Async | Tok::Fn));
            // A bare `worker fn` / `worker async fn` at the start of a member.
            let is_bare_worker_method = matches!(self.peek(), Tok::Ident(s) if s == "worker")
                && matches!(self.peek_nth(1), Tok::Async | Tok::Fn);
            if *self.peek() == Tok::Async
                || *self.peek() == Tok::Fn
                || is_static_method
                || is_bare_worker_method
            {
                let mstart = self.span().start;
                let is_static = if is_static_method {
                    self.advance(); // consume the contextual `static`
                    true
                } else {
                    false
                };
                // Optional contextual `worker` after optional `static`.
                let is_worker = if matches!(self.peek(), Tok::Ident(s) if s == "worker")
                    && matches!(self.peek_nth(1), Tok::Async | Tok::Fn)
                {
                    self.advance(); // consume `worker`
                    true
                } else {
                    false
                };
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
                    other => {
                        return Err(AsError::at(
                            format!("expected method name, found {:?}", other),
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
                let mspan = Span::new(mstart, self.prev_end());
                methods.push(crate::ast::MethodDecl {
                    name: mname,
                    params,
                    ret,
                    body,
                    is_async,
                    is_generator,
                    is_worker,
                    is_static,
                    span: mspan,
                    name_span: mname_span,
                });
            } else {
                // Field declaration: Ident ["?"] ":" type ["=" expr]
                let fstart = self.span().start;
                let fname_span = self.span();
                let fname = match self.advance() {
                    Tok::Ident(n) => n,
                    other => {
                        return Err(AsError::at(
                            format!("expected a field name or method, found {:?}", other),
                            self.tokens[self.pos - 1].span,
                        ))
                    }
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
                fields.push(crate::ast::FieldDecl {
                    name: fname,
                    ty,
                    default,
                    span: fspan,
                    name_span: fname_span,
                });
            }
        }
        self.eat(&Tok::RBrace)?;
        self.pop_type_params(type_params.len());
        let span = Span::new(start, self.prev_end());
        Ok(Stmt::Class {
            name,
            superclass,
            implements,
            fields,
            methods,
            is_worker,
            span,
            name_span,
        })
    }

    /// Consume a closing `>` for a type-argument list (NUM §3.4). A `>>` (`Shr`)
    /// closes TWO nested generics: the first `eat_type_gt` consumes one `>` and
    /// records a `pending_gt` for the second, which the enclosing close consumes
    /// WITHOUT advancing. A `>=` (`Ge`) would split similarly, but `>=` never
    /// follows a type argument in valid source, so only `Shr` is split here. A plain
    /// `Gt` is consumed normally. Anything else is the usual "expected '>'" error.
    fn eat_type_gt(&mut self) -> Result<(), AsError> {
        if self.pending_gt > 0 {
            // The remainder of a previously-split `>>` — consume virtually, no cursor
            // advance (the `Shr` token was already advanced past when first split).
            self.pending_gt -= 1;
            return Ok(());
        }
        match self.peek() {
            Tok::Gt => {
                self.advance();
                Ok(())
            }
            Tok::Shr => {
                // Split `>>` into `> ` + a pending `>`. Advance past the single `Shr`
                // token now (it covers both), and leave one virtual `>` for the
                // enclosing close.
                self.advance();
                self.pending_gt += 1;
                Ok(())
            }
            _ => Err(AsError::at(
                format!("expected {:?}, found {:?}", Tok::Gt, self.peek()),
                self.span(),
            )),
        }
    }

    /// TYPE §6: parse an OPTIONAL generic type-parameter list after a decl name —
    /// `< Ident (: Bound)? (, Ident (: Bound)?)* >`. Returns the declared param
    /// names (with their optional bound types) AND pushes the names onto
    /// `type_param_scope` so type references inside the decl lower to `Type::Param`.
    /// The caller MUST call `pop_type_params(n)` with the returned count after the
    /// decl body is parsed. When the next token is not `<`, this is a no-op (returns
    /// an empty list, pushes nothing). The list itself is RUNTIME-ERASED — the
    /// returned bounds are consumed only by the static checker (TYPE Tasks 8–12);
    /// the runtime decl nodes do not store them in this unit.
    fn parse_type_params(
        &mut self,
    ) -> Result<Vec<crate::ast::TypeParam>, AsError> {
        let mut params = Vec::new();
        if *self.peek() != Tok::Lt {
            return Ok(params);
        }
        self.advance(); // <
        // An empty `<>` is rejected (a generic decl must declare ≥1 param).
        loop {
            let name = match self.advance() {
                Tok::Ident(n) => n,
                other => {
                    return Err(AsError::at(
                        format!("expected a type-parameter name, found {:?}", other),
                        self.tokens[self.pos - 1].span,
                    ))
                }
            };
            // Push the name into scope IMMEDIATELY so a later param's bound can
            // reference an earlier param (`<T, C: Container<T>>`).
            self.type_param_scope.push(name.clone());
            // Optional bound: `: Type` (only interfaces are admitted by the checker,
            // but the parser accepts any type here — the checker rejects non-iface
            // bounds in TYPE Tasks 8–12).
            let bound = if *self.peek() == Tok::Colon {
                self.advance();
                Some(self.parse_type()?)
            } else {
                None
            };
            params.push(crate::ast::TypeParam { name, bound });
            if *self.peek() == Tok::Comma {
                self.advance();
                // Trailing comma before `>` is tolerated.
                if *self.peek() == Tok::Gt || *self.peek() == Tok::Shr {
                    break;
                }
            } else {
                break;
            }
        }
        self.eat_type_gt()?;
        Ok(params)
    }

    /// Pop `n` names off `type_param_scope` (the count of params an earlier
    /// `parse_type_params` pushed). Called after a generic decl's body parses so the
    /// params go out of scope.
    fn pop_type_params(&mut self, n: usize) {
        let new_len = self.type_param_scope.len().saturating_sub(n);
        self.type_param_scope.truncate(new_len);
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
            // `fn` is the bare callable type; `fn(A) -> B` is a parameterized
            // function type (`Type::FnSig`). Disambiguated by a `(` immediately
            // after `fn` (TYPE §6 — known type position, no expression ambiguity).
            Tok::Fn => {
                if *self.peek() == Tok::LParen {
                    self.advance(); // (
                    let mut params = Vec::new();
                    if *self.peek() != Tok::RParen {
                        loop {
                            params.push(self.parse_type()?);
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
                    // `-> Ret` is REQUIRED for a function-type signature. `->` lexes
                    // as two tokens (`Minus` then `Gt`) — there is no dedicated arrow
                    // token (`=>` is `FatArrow`); consume the pair.
                    self.eat(&Tok::Minus)?;
                    self.eat(&Tok::Gt)?;
                    let ret = self.parse_type()?;
                    Type::FnSig(params, Box::new(ret))
                } else {
                    Type::Fn
                }
            }
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
                "int" => Type::Int,
                "float" => Type::Float,
                "string" => Type::String,
                "bool" => Type::Bool,
                "any" => Type::Any,
                "object" => Type::Object,
                "error" => Type::Error,
                "array" => {
                    self.eat(&Tok::Lt)?;
                    let inner = self.parse_type()?;
                    self.eat_type_gt()?;
                    Type::Array(Box::new(inner))
                }
                "Result" => {
                    self.eat(&Tok::Lt)?;
                    let inner = self.parse_type()?;
                    self.eat_type_gt()?;
                    Type::Result(Box::new(inner))
                }
                "future" => {
                    self.eat(&Tok::Lt)?;
                    let inner = self.parse_type()?;
                    self.eat_type_gt()?;
                    Type::Future(Box::new(inner))
                }
                "map" => {
                    self.eat(&Tok::Lt)?;
                    let k = self.parse_type()?;
                    self.eat(&Tok::Comma)?;
                    let v = self.parse_type()?;
                    self.eat_type_gt()?;
                    Type::Map(Box::new(k), Box::new(v))
                }
                // TYPE §6: a bare identifier matching an in-scope generic type
                // parameter lowers to `Type::Param` (runtime-erased accept-anything).
                // A type param is NEVER applied to type args (`T<int>` is nonsense),
                // so this only handles the bare reference.
                _ if self.type_param_scope.iter().any(|p| p == &name) => Type::Param(name),
                // TYPE §6: a USER generic application in type position
                // (`Box<int>`, `Box<Box<int>>`). The head is a user class/enum/
                // interface name; its type-argument list is PARSED (so the syntax is
                // accepted and `>>` splits correctly) but DISCARDED for the runtime
                // contract — generics are erased, so `Box<int>` checks exactly like
                // the bare `Box` head (`Type::Named`). The static checker (TYPE Tasks
                // 8–12) is what reads the args.
                _ if *self.peek() == Tok::Lt => {
                    self.advance(); // <
                    // Parse-and-discard the comma-separated type-argument list.
                    if *self.peek() != Tok::Gt && *self.peek() != Tok::Shr {
                        loop {
                            let _ = self.parse_type()?;
                            if *self.peek() == Tok::Comma {
                                self.advance();
                                if *self.peek() == Tok::Gt || *self.peek() == Tok::Shr {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }
                    }
                    self.eat_type_gt()?;
                    Type::Named(name)
                }
                _ => Type::Named(name),
            },
            other => {
                return Err(AsError::at(
                    format!("expected a type, found {:?}", other),
                    span,
                ))
            }
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
                let is_rest = if *self.peek() == Tok::DotDotDot {
                    self.advance();
                    true
                } else {
                    false
                };
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
                // Optional default value: `name = expr` (not for a rest param).
                let default = if !is_rest && *self.peek() == Tok::Eq {
                    self.advance();
                    Some(self.expr()?)
                } else {
                    None
                };
                // A required (no-default) param may not follow a defaulted one.
                if default.is_none()
                    && !is_rest
                    && params
                        .iter()
                        .any(|p: &crate::ast::Param| p.default.is_some())
                {
                    return Err(AsError::at(
                        "a required parameter cannot follow a defaulted parameter",
                        name_span,
                    ));
                }
                params.push(crate::ast::Param {
                    name,
                    ty,
                    name_span,
                    rest: is_rest,
                    default,
                });
                if is_rest {
                    if *self.peek() == Tok::Comma {
                        return Err(AsError::at("a rest parameter must be last", name_span));
                    }
                    break;
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
        Ok(Stmt::If {
            cond,
            then_branch,
            else_branch,
        })
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
                    if let ExprKind::Range {
                        start,
                        end,
                        inclusive,
                        step,
                    } = iter.kind
                    {
                        return Ok(Stmt::ForRange {
                            var,
                            start: *start,
                            end: *end,
                            inclusive,
                            step: step.map(|s| *s),
                            body,
                        });
                    }
                }
                Ok(Stmt::ForOf {
                    var,
                    iter,
                    body,
                    for_await,
                })
            }
            Tok::Of => {
                let iter = self.expr()?;
                self.eat(&Tok::RParen)?;
                let body = self.block()?;
                Ok(Stmt::ForOf {
                    var,
                    iter,
                    body,
                    for_await,
                })
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
            let mut rest: Option<(String, Span)> = None;
            if *self.peek() != Tok::RBracket {
                loop {
                    if *self.peek() == Tok::DotDotDot {
                        self.advance();
                        let rspan = self.span();
                        let rname = match self.advance() {
                            Tok::Ident(n) => n,
                            other => {
                                return Err(AsError::at(
                                    format!("expected a name after '...', found {:?}", other),
                                    self.tokens[self.pos - 1].span,
                                ))
                            }
                        };
                        rest = Some((rname, rspan));
                        if *self.peek() == Tok::Comma {
                            return Err(AsError::at("a rest element must be last", rspan));
                        }
                        break;
                    }
                    let span = self.span();
                    match self.advance() {
                        Tok::Ident(n) => {
                            names.push(n);
                            name_spans.push(span);
                        }
                        other => {
                            return Err(AsError::at(
                                format!(
                                    "expected an identifier in destructuring pattern, found {:?}",
                                    other
                                ),
                                self.tokens[self.pos - 1].span,
                            ))
                        }
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
            self.eat(&Tok::Eq)?;
            let value = self.expr()?;
            let span = Span::new(start, self.prev_end());
            return Ok(Stmt::LetDestructure {
                names,
                rest,
                value,
                mutable,
                span,
                name_spans,
            });
        }
        // `let {a, b as local} = expr` — object destructuring binding.
        if *self.peek() == Tok::LBrace {
            self.advance(); // consume '{'
            let mut bindings = Vec::new();
            let mut rest: Option<(String, Span)> = None;
            if *self.peek() != Tok::RBrace {
                loop {
                    if *self.peek() == Tok::DotDotDot {
                        self.advance();
                        let rspan = self.span();
                        let rname = match self.advance() {
                            Tok::Ident(n) => n,
                            other => {
                                return Err(AsError::at(
                                    format!("expected a name after '...', found {:?}", other),
                                    self.tokens[self.pos - 1].span,
                                ))
                            }
                        };
                        rest = Some((rname, rspan));
                        if *self.peek() == Tok::Comma {
                            return Err(AsError::at("a rest element must be last", rspan));
                        }
                        break;
                    }
                    let key_span = self.span();
                    let key = match self.advance() {
                        Tok::Ident(n) => n,
                        Tok::Str(s) => s,
                        other => {
                            return Err(AsError::at(
                                format!("expected a key in object pattern, found {:?}", other),
                                self.tokens[self.pos - 1].span,
                            ))
                        }
                    };
                    let (binding, binding_span) = if matches!(self.peek(), Tok::Ident(s) if s == "as")
                    {
                        self.advance();
                        let bspan = self.span();
                        match self.advance() {
                            Tok::Ident(b) => (b, bspan),
                            other => {
                                return Err(AsError::at(
                                    format!("expected a local name after 'as', found {:?}", other),
                                    self.tokens[self.pos - 1].span,
                                ))
                            }
                        }
                    } else {
                        if !is_ident_like(&key) {
                            return Err(AsError::at(
                                format!("key {:?} is not a valid binding name; use `as`", key),
                                key_span,
                            ));
                        }
                        (key.clone(), key_span)
                    };
                    bindings.push(crate::ast::ObjBinding {
                        key,
                        binding,
                        key_span,
                        binding_span,
                    });
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
            self.eat(&Tok::Eq)?;
            let value = self.expr()?;
            let span = Span::new(start, self.prev_end());
            return Ok(Stmt::LetDestructureObject {
                bindings,
                rest,
                value,
                mutable,
                span,
            });
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
            return Err(AsError::at(
                "const declarations must be initialized",
                self.span(),
            ));
        };
        let span = Span::new(start, self.prev_end());
        Ok(Stmt::Let {
            name,
            ty,
            value,
            mutable,
            span,
            name_span,
        })
    }

    fn expr(&mut self) -> Result<Expr, AsError> {
        // SP9 §1: the recursive-descent parser re-enters `expr` for every bracketed
        // sub-expression (`(`/`[`/`{`/template `${`), so a deeply nested SOURCE
        // expression (`((((…))))`) is native Rust recursion HERE — before compile or
        // eval. Grow the native stack at this funnel so parsing reaches the AST (and
        // then the compile/eval `EXPR_NEST_LIMIT` cap) rather than SIGABRTing first.
        // Synchronous, so the cheap probe-and-grow `grow` suffices; inert until the
        // stack runs low.
        crate::vm::stack::grow(|| self.assignment())
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
            kind: ExprKind::Assign {
                target: Box::new(target),
                value: Box::new(value),
            },
            span,
        })
    }

    /// Build a left-associative binary node from an already-parsed left side.
    fn make_binary(left: Expr, op: BinOp, right: Expr) -> Expr {
        let span = Span::new(left.span.start, right.span.end);
        Expr {
            kind: ExprKind::Binary {
                op,
                lhs: Box::new(left),
                rhs: Box::new(right),
            },
            span,
        }
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
                Tok::Ident(_) => {
                    self.tokens.get(self.pos + 2).map(|t| &t.tok) == Some(&Tok::FatArrow)
                }
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
                        params: vec![crate::ast::Param {
                            name,
                            ty: None,
                            name_span,
                            rest: false,
                            default: None,
                        }],
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
                kind: ExprKind::Arrow {
                    params,
                    body: Box::new(body),
                    is_async,
                    is_generator: false,
                },
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
        let mut left = self.bitor()?;
        loop {
            let op = match self.peek() {
                Tok::Lt => BinOp::Lt,
                Tok::Le => BinOp::Le,
                Tok::Gt => BinOp::Gt,
                Tok::Ge => BinOp::Ge,
                Tok::Instanceof => BinOp::InstanceOf,
                _ => break,
            };
            self.advance();
            let right = self.bitor()?;
            left = Self::make_binary(left, op, right);
        }
        Ok(left)
    }

    /// Bitwise-OR tier (`| ^`), NUM §3.4 (Go's binding): TIGHTER than
    /// comparison/equality, LOOSER than `+ -`/range — so `a | b == c` is `(a|b)==c`
    /// and `a | b + c` is `a | (b + c)`. CRITICAL: this tier is reached ONLY in
    /// value position (the full `coalesce → … → comparison → bitor → range` chain).
    /// `parse_pattern` deliberately bypasses it (it calls `range()` directly) so a
    /// bare `|` between match patterns stays an or-pattern, and `parse_type`'s `|`
    /// loop stays a union — exactly as `|` was invisible to the expression chain
    /// before NUM. `&` and `<< >>` bind one tier TIGHTER (multiplicative).
    fn bitor(&mut self) -> Result<Expr, AsError> {
        let mut left = self.range()?;
        loop {
            let op = match self.peek() {
                Tok::Pipe => BinOp::BitOr,
                Tok::Caret => BinOp::BitXor,
                _ => break,
            };
            self.advance();
            let right = self.range()?;
            left = Self::make_binary(left, op, right);
        }
        Ok(left)
    }

    /// The range operator `..`/`..=` (grammar PREC.range = 7): binds tighter than
    /// comparison but looser than additive (`1+1..5` parses as `(1+1)..5`).
    /// Produces a dedicated `ExprKind::Range` node carrying `inclusive` and an
    /// optional contextual `step <expr>` suffix. Not chained: `a..b..c` is not a
    /// thing — a single `start..end` (with optional `step`) per range.
    fn range(&mut self) -> Result<Expr, AsError> {
        let left = self.additive()?;
        let inclusive = match self.peek() {
            Tok::DotDot => false,
            Tok::DotDotEq => true,
            _ => return Ok(left),
        };
        self.advance();
        let right = self.additive()?;
        // Contextual `step <expr>`: only consumed when the identifier `step`
        // directly follows the range end. The step expression runs to the
        // natural range boundary (the `)` of a for-header or end of a `let`
        // initializer), so parse it at the additive level (tighter than `..`,
        // so a bare `step 2` is `Range{..,step:2}` and not re-entered as a range).
        let step = if matches!(self.peek(), Tok::Ident(s) if s == "step") {
            self.advance();
            Some(Box::new(self.additive()?))
        } else {
            None
        };
        // The span runs through the END of the `step` clause when present, so a
        // panic on a stepped VALUE range (`1..10 step 0`) underlines the whole
        // `1..10 step 0` — byte-identical to the VM's caret (the CST front-end
        // already spans the step). Without a step it ends at the range bound.
        let end_off = step.as_ref().map_or(right.span.end, |s| s.span.end);
        let span = Span::new(left.span.start, end_off);
        Ok(Expr {
            kind: ExprKind::Range {
                start: Box::new(left),
                end: Box::new(right),
                inclusive,
                step,
            },
            span,
        })
    }

    /// Parse a single `match`-arm pattern (one alternative of a `|` group),
    /// Phase 8a. Does NOT consume `|`/`if`/`=>` (the arm loop handles those).
    fn parse_pattern(&mut self) -> Result<crate::ast::Pattern, AsError> {
        use crate::ast::{ObjPatEntry, Pattern};
        // `_` wildcard (lexes as Ident("_")).
        if matches!(self.peek(), Tok::Ident(s) if s == "_") {
            self.advance();
            return Ok(Pattern::Wildcard);
        }
        // Array pattern `[p, ..., (...name | ...)?]`.
        if *self.peek() == Tok::LBracket {
            self.advance();
            let mut pats = Vec::new();
            let mut rest: Option<Option<std::rc::Rc<str>>> = None;
            if *self.peek() != Tok::RBracket {
                loop {
                    if *self.peek() == Tok::DotDotDot {
                        rest = Some(self.parse_pattern_rest()?);
                        break;
                    }
                    pats.push(self.parse_pattern()?);
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
            return Ok(Pattern::Array(pats, rest));
        }
        // Object pattern `{key, key2: subpat, ..., (...name)?}`.
        if *self.peek() == Tok::LBrace {
            self.advance();
            let mut entries = Vec::new();
            let mut rest: Option<Option<std::rc::Rc<str>>> = None;
            if *self.peek() != Tok::RBrace {
                loop {
                    if *self.peek() == Tok::DotDotDot {
                        rest = Some(self.parse_pattern_rest()?);
                        break;
                    }
                    let key: std::rc::Rc<str> = match self.advance() {
                        Tok::Ident(n) => n.into(),
                        Tok::Str(s) => s.into(),
                        other => {
                            return Err(AsError::at(
                                format!("expected a key in object pattern, found {:?}", other),
                                self.tokens[self.pos - 1].span,
                            ))
                        }
                    };
                    let pat = if *self.peek() == Tok::Colon {
                        self.advance();
                        Some(self.parse_pattern()?)
                    } else {
                        None
                    };
                    entries.push(ObjPatEntry { key, pat });
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
            return Ok(Pattern::Object(entries, rest));
        }
        // ADT: a VARIANT pattern — `Circle(…)` / `Shape.Circle(…)`. Detected as an
        // `Ident` (optionally `Ident.Ident`) IMMEDIATELY followed by `(`. The trailing
        // `(` makes this unambiguously a variant destructuring (never an Option-C bare
        // ident, never a range bound). A bare/qualified UNIT pattern (no parens) flows
        // through the value-expr path below UNCHANGED.
        if let Some(pat) = self.try_variant_pattern()? {
            return Ok(pat);
        }
        // Otherwise parse a value-expression and classify it. NUM §3.4: a match
        // pattern enters at `range()` — the tier JUST BELOW the new `bitor()` — so a
        // bare `|` between patterns is NEVER swallowed by the value-parser and stays
        // owned by the arm loop (the or-pattern `|`). The layers above `bitor`
        // (`??`/`||`/`&&`/`==`/`<`) are not valid leading forms inside a single
        // pattern anyway, so dropping to `range()` loses nothing; the `Range`/
        // `Ident`/`Value` classification below is unchanged.
        let start = self.range()?;
        // A range pattern: `a..b`, `a..=b`, optionally `… step k`. The expression
        // parser (`range()`) produces a dedicated `ExprKind::Range` for both the
        // exclusive and inclusive forms and already consumed any trailing `step`.
        if let ExprKind::Range {
            start: lo,
            end,
            inclusive,
            step,
        } = start.kind
        {
            return Ok(Pattern::Range {
                start: lo,
                end,
                inclusive,
                step,
            });
        }
        // A lone identifier → Option-C resolved at match time.
        if let ExprKind::Ident(name) = &start.kind {
            return Ok(Pattern::Ident(name.as_str().into()));
        }
        Ok(Pattern::Value(Box::new(start)))
    }

    /// ADT: try to parse a VARIANT pattern (`Circle(…)` / `Shape.Circle(…)`). Returns
    /// `Some(pat)` if the cursor is at `Ident (` or `Ident . Ident (` (a variant ref
    /// followed by `(`); `None` otherwise (leaving the cursor untouched so the caller
    /// falls back to value-expr classification). Inside the parens, fields are either
    /// positional sub-patterns (`Pair(a, b)`) or named (`Rect(w: ww)` / `Rect(w)`);
    /// the form is decided by the FIRST field (`ident :` ⇒ named).
    fn try_variant_pattern(&mut self) -> Result<Option<crate::ast::Pattern>, AsError> {
        use crate::ast::{Pattern, VariantPatFields};
        // Probe the shape WITHOUT consuming, so a non-match leaves the cursor for the
        // value-expr path. Two accepted lead shapes: `Ident (` or `Ident . Ident (`.
        let (enum_name, variant): (Option<std::rc::Rc<str>>, std::rc::Rc<str>) = match self.peek() {
            Tok::Ident(a) => {
                if matches!(self.peek_nth(1), Tok::LParen) {
                    let v: std::rc::Rc<str> = a.as_str().into();
                    self.advance(); // variant ident
                    (None, v)
                } else if matches!(self.peek_nth(1), Tok::Dot) {
                    // `Ident . Ident (` — qualified variant ref.
                    let is_qualified = matches!(self.peek_nth(2), Tok::Ident(_))
                        && matches!(self.peek_nth(3), Tok::LParen);
                    if !is_qualified {
                        return Ok(None);
                    }
                    let en: std::rc::Rc<str> = a.as_str().into();
                    self.advance(); // enum ident
                    self.advance(); // `.`
                    let v: std::rc::Rc<str> = match self.advance() {
                        Tok::Ident(n) => n.as_str().into(),
                        // Unreachable given the peek above, but stay total.
                        other => {
                            return Err(AsError::at(
                                format!("expected variant name, found {:?}", other),
                                self.tokens[self.pos - 1].span,
                            ))
                        }
                    };
                    (Some(en), v)
                } else {
                    return Ok(None);
                }
            }
            _ => return Ok(None),
        };
        // Now at `(`. Parse the field sub-patterns.
        self.eat(&Tok::LParen)?;
        let mut positional: Vec<Pattern> = Vec::new();
        let mut named: Vec<(std::rc::Rc<str>, Option<Pattern>)> = Vec::new();
        // `is_named` decided by the first field: `ident :` ⇒ named. A bare `ident`
        // with no `:` in a named-context is a shorthand bind (`Rect(w)`); but the
        // FIRST field decides positional-vs-named, so a leading bare ident is
        // positional (binds by index) unless followed by `:`.
        let mut mode: Option<bool> = None; // Some(true) = named
        if *self.peek() != Tok::RParen {
            loop {
                let field_named = matches!(self.peek(), Tok::Ident(_))
                    && matches!(self.peek_nth(1), Tok::Colon);
                match mode {
                    None => mode = Some(field_named),
                    // A named field (`field_named`) appearing in POSITIONAL mode
                    // (`!prev`) is a mix error. (A named-mode shorthand `Rect(w)` —
                    // `prev && !field_named` — is allowed and handled below.)
                    Some(prev) if !prev && field_named => {
                        return Err(AsError::at(
                            "variant pattern fields must be all positional or all named".to_string(),
                            self.span(),
                        ));
                    }
                    _ => {}
                }
                if matches!(mode, Some(true)) {
                    // Named field: `name` or `name: subpat`.
                    let key: std::rc::Rc<str> = match self.advance() {
                        Tok::Ident(n) => n.as_str().into(),
                        other => {
                            return Err(AsError::at(
                                format!("expected field name in variant pattern, found {:?}", other),
                                self.tokens[self.pos - 1].span,
                            ))
                        }
                    };
                    let subpat = if *self.peek() == Tok::Colon {
                        self.advance();
                        Some(self.parse_pattern()?)
                    } else {
                        None
                    };
                    named.push((key, subpat));
                } else {
                    positional.push(self.parse_pattern()?);
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
        let fields = if matches!(mode, Some(true)) {
            VariantPatFields::Named(named)
        } else {
            VariantPatFields::Positional(positional)
        };
        Ok(Some(Pattern::Variant {
            enum_name,
            variant,
            fields,
        }))
    }

    /// Parse a trailing rest in an array/object pattern: assumes the current token
    /// is `...`. Returns `None` for `...` (ignore) or `Some(name)` for `...name`.
    /// A rest must be last (no trailing comma).
    fn parse_pattern_rest(&mut self) -> Result<Option<std::rc::Rc<str>>, AsError> {
        let rspan = self.span();
        self.advance(); // consume `...`
        let rest = if let Tok::Ident(n) = self.peek() {
            let n = n.clone();
            self.advance();
            Some(n.into())
        } else {
            None
        };
        if *self.peek() == Tok::Comma {
            return Err(AsError::at("a rest element must be last", rspan));
        }
        Ok(rest)
    }

    fn additive(&mut self) -> Result<Expr, AsError> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                // Wrapping add/subtract (NUM §3.2/§3.4): additive peers.
                Tok::PlusPercent => BinOp::WrapAdd,
                Tok::MinusPercent => BinOp::WrapSub,
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
                // Wrapping multiply, shifts, and bitwise-AND (NUM §3.2/§3.4) bind at
                // the multiplicative tier — Go's binding (so `a & b == c` is
                // `(a&b)==c`, `a << b + c` is `(a<<b)+c` … wait: `+ -` are LOOSER
                // than `<<`/`&`, so `a + b << c` is `a + (b<<c)` and `1 << 2 + 3` is
                // `(1<<2) + 3` — matching Go).
                Tok::StarPercent => BinOp::WrapMul,
                Tok::Shl => BinOp::Shl,
                Tok::Shr => BinOp::Shr,
                Tok::Amp => BinOp::BitAnd,
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
                    expr = Expr {
                        kind: ExprKind::Try(Box::new(expr)),
                        span,
                    };
                }
                Tok::Bang => {
                    self.advance();
                    let span = Span::new(expr.span.start, self.prev_end());
                    expr = Expr {
                        kind: ExprKind::Unwrap(Box::new(expr)),
                        span,
                    };
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
            // `~` — int bitwise NOT (NUM §3.2), prefix unary.
            Tok::Tilde => Some(UnOp::BitNot),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let operand = self.unary()?;
            let span = Span::new(start, operand.span.end);
            return Ok(Expr {
                kind: ExprKind::Unary {
                    op,
                    expr: Box::new(operand),
                },
                span,
            });
        }
        self.postfix()
    }

    /// TYPE §6 (Task 5): speculatively consume an expression-level explicit
    /// type-argument list `< Type (, Type)* >` IFF the closing `>` is immediately
    /// followed by `(` (a generic-instantiation call). Returns `true` and leaves the
    /// cursor on the `(` when it matched; returns `false` and FULLY REWINDS (both the
    /// cursor and `pending_gt`) otherwise. The parsed type args are DISCARDED
    /// (generics are runtime-erased). Precondition: the current token is `Tok::Lt`.
    ///
    /// The trailing-`(` requirement is the sole discriminator (matching the spec's
    /// rule and TypeScript): a `>` followed by anything but `(` rewinds to a
    /// comparison, so every ordinary `a < b`, `a < b > c`, `f(a < b, c > d)` chain
    /// is preserved. (The single token-identical case `IDENT < IDENT > ( … )` —
    /// shared by `Box<int>(5)` and a hypothetical `(a<b)>(c)` — resolves to the
    /// generic call; the example corpus contains no such comparison, so there is no
    /// regression.)
    fn try_explicit_type_args(&mut self) -> bool {
        debug_assert_eq!(*self.peek(), Tok::Lt);
        let saved_pos = self.pos;
        let saved_pending = self.pending_gt;
        // Parse `< Type (, Type)* >` with the NUM `>>`-split (`eat_type_gt`). Any
        // error → rewind. We do NOT propagate the error (speculation must not abort
        // the enclosing parse).
        self.advance(); // <
        let ok = self.speculate_type_arg_list();
        if ok && *self.peek() == Tok::LParen {
            // Committed: the `(` confirms a call. Leave the cursor on `(`.
            return true;
        }
        // Rewind everything.
        self.pos = saved_pos;
        self.pending_gt = saved_pending;
        false
    }

    /// Helper for `try_explicit_type_args`: parse the body of a `< … >` type-arg
    /// list (the `<` already consumed), returning `true` if it closed cleanly. Never
    /// returns an `Err` — a malformed arg is reported as `false` so the caller can
    /// rewind to a comparison. An empty `<>` is NOT a valid type-arg list.
    fn speculate_type_arg_list(&mut self) -> bool {
        // An immediate close (`<>`) is not a type-arg list.
        if *self.peek() == Tok::Gt || *self.peek() == Tok::Shr {
            return false;
        }
        loop {
            if self.parse_type().is_err() {
                return false;
            }
            if *self.peek() == Tok::Comma {
                self.advance();
                // A trailing comma before `>` is not valid in a type-arg list.
                if *self.peek() == Tok::Gt || *self.peek() == Tok::Shr {
                    return false;
                }
            } else {
                break;
            }
        }
        // Close the list (handles a split `>>`).
        self.eat_type_gt().is_ok()
    }

    fn postfix(&mut self) -> Result<Expr, AsError> {
        let mut expr = self.primary()?;
        loop {
            // TYPE §6 (Task 5): expression-level EXPLICIT type arguments
            // (`Box<int>(5)`, `map<string, number>(xs, f)`). At a `<` after a primary
            // callee, this is lexically ambiguous with the comparison chain
            // `(callee < arg) > ...`. Resolve it the TypeScript/Rust-turbofish-free
            // way: SPECULATIVELY parse `< Type (, Type)* >` and accept the type-arg
            // reading ONLY if the closing `>` is IMMEDIATELY followed by `(` (the call
            // shape). On ANY failure — a non-type token, an unbalanced `>`, or a `>`
            // not followed by `(` — REWIND completely (cursor + `pending_gt`) and
            // leave the `<` for `comparison()` to consume as `BinOp::Lt`. The type
            // arguments are RUNTIME-ERASED: a match consumes and DISCARDS them,
            // producing the exact same callee `Box(5)` would, so the AST (and both
            // engines) stay byte-identical to the erased form. Comparison/bitwise code
            // is untouched: every chain whose `>` is NOT immediately followed by `(`
            // (`a < b`, `a < b > c`, `f(a < b, c > d)`, `a << b`, `a >> b`) rewinds.
            // Checked BEFORE the match to avoid a borrow conflict with `self.peek()`.
            if *self.peek() == Tok::Lt && self.try_explicit_type_args() {
                // Type args consumed (and discarded); the cursor is now on `(`. Loop
                // again so the `LParen` arm parses the call against the SAME callee.
                continue;
            }
            match self.peek() {
                Tok::LParen => {
                    self.advance();
                    let mut args = Vec::new();
                    if *self.peek() != Tok::RParen {
                        loop {
                            let named_arg = match self.peek() {
                                // ADT §3.2: a named call argument `name: expr`
                                // (variant construction). Disambiguated by an
                                // `IDENT :` at argument start — a bare `:` cannot
                                // otherwise begin a call argument (ternary always
                                // has a `?` before the `:`).
                                Tok::Ident(n) if *self.peek_nth(1) == Tok::Colon => {
                                    Some(std::rc::Rc::<str>::from(n.as_str()))
                                }
                                _ => None,
                            };
                            if *self.peek() == Tok::DotDotDot {
                                self.advance();
                                args.push(crate::ast::CallArg::Spread(self.expr()?));
                            } else if let Some(arg_name) = named_arg {
                                self.advance(); // ident
                                self.advance(); // ':'
                                args.push(crate::ast::CallArg::Named {
                                    name: arg_name,
                                    value: self.expr()?,
                                });
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
                    expr = Expr {
                        kind: ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                        },
                        span,
                    };
                }
                Tok::LBracket => {
                    self.advance();
                    let index = self.expr()?;
                    self.eat(&Tok::RBracket)?;
                    let span = Span::new(expr.span.start, self.prev_end());
                    expr = Expr {
                        kind: ExprKind::Index {
                            object: Box::new(expr),
                            index: Box::new(index),
                        },
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
                    expr = Expr {
                        kind: ExprKind::Member {
                            object: Box::new(expr),
                            name,
                        },
                        span,
                    };
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
                    expr = Expr {
                        kind: ExprKind::OptMember {
                            object: Box::new(expr),
                            name,
                        },
                        span,
                    };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn primary(&mut self) -> Result<Expr, AsError> {
        let tok_span = self.span();
        let kind = match self.advance() {
            Tok::Int(n) => ExprKind::Int(n),
            Tok::Float(n) => ExprKind::Float(n),
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
                return Ok(Expr {
                    kind: ExprKind::Paren(Box::new(inner)),
                    span,
                });
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
                return Ok(Expr {
                    kind: ExprKind::Array(items),
                    span,
                });
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
                return Ok(Expr {
                    kind: ExprKind::Object(entries),
                    span,
                });
            }
            Tok::HashBrace => {
                let mut entries = Vec::new();
                if *self.peek() != Tok::RBrace {
                    loop {
                        // D4: spread is out of scope for map literals in SP2 — a
                        // `...` element is a clean parse error (no panic).
                        if *self.peek() == Tok::DotDotDot {
                            return Err(AsError::at(
                                "spread is not allowed in a map literal",
                                self.span(),
                            ));
                        }
                        // The KEY is an arbitrary evaluated expression (unlike
                        // object literals, where the key is a bare name/string).
                        let key = self.expr()?;
                        self.eat(&Tok::Colon)?;
                        let value = self.expr()?;
                        entries.push(crate::ast::MapEntry { key, value });
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
                return Ok(Expr {
                    kind: ExprKind::Map(entries),
                    span,
                });
            }
            Tok::TemplateStr(s) => {
                let parts = vec![crate::ast::TemplatePart::Lit(s)];
                let span = Span::new(tok_span.start, self.prev_end());
                return Ok(Expr {
                    kind: ExprKind::Template { parts },
                    span,
                });
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
                return Ok(Expr {
                    kind: ExprKind::Template { parts },
                    span,
                });
            }
            Tok::Match => {
                let subject = self.expr()?;
                self.eat(&Tok::LBrace)?;
                let mut arms = Vec::new();
                while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
                    // `|`-separated patterns, then optional `if <guard>`, then `=> body`.
                    let mut patterns = vec![self.parse_pattern()?];
                    while *self.peek() == Tok::Pipe {
                        self.advance();
                        patterns.push(self.parse_pattern()?);
                    }
                    let guard = if *self.peek() == Tok::If {
                        self.advance();
                        Some(self.expr()?)
                    } else {
                        None
                    };
                    self.eat(&Tok::FatArrow)?;
                    let body = self.expr()?;
                    arms.push(crate::ast::MatchArm {
                        patterns,
                        guard,
                        body,
                    });
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::RBrace)?;
                let span = Span::new(tok_span.start, self.prev_end());
                return Ok(Expr {
                    kind: ExprKind::Match {
                        subject: Box::new(subject),
                        arms,
                    },
                    span,
                });
            }
            other => {
                return Err(AsError::at(
                    format!("unexpected token {:?}", other),
                    tok_span,
                ))
            }
        };
        Ok(Expr {
            kind,
            span: tok_span,
        })
    }
}

/// Whether `tok` can begin an expression. Used to decide if a `yield` carries an
/// operand: since AScript has no newline tokens, a bare `yield` is only
/// distinguishable from `yield <expr>` by whether what follows can start an
/// expression (a terminator or a statement keyword cannot).
fn starts_expression(tok: &Tok) -> bool {
    matches!(
        tok,
        Tok::Int(_)
            | Tok::Float(_)
            | Tok::Str(_)
            | Tok::Ident(_)
            | Tok::True
            | Tok::False
            | Tok::Nil
            | Tok::LParen
            | Tok::LBracket
            | Tok::LBrace
            | Tok::HashBrace
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

    // ---- IFACE Task 4: lexer + legacy parser ----

    fn parse_one(src: &str) -> Stmt {
        let toks = lex(src).unwrap();
        parse(&toks).unwrap().into_iter().next().unwrap()
    }

    #[test]
    fn iface_lexes_interface_keyword_and_contextual_extends_implements() {
        // `interface` is a reserved keyword
        assert!(lex("interface").unwrap().iter().any(|t| t.tok == Tok::Interface));
        // `implements`/`extends` stay contextual (Tok::Ident)
        let toks = lex("implements extends").unwrap();
        assert!(matches!(&toks[0].tok, Tok::Ident(s) if s == "implements"));
        assert!(matches!(&toks[1].tok, Tok::Ident(s) if s == "extends"));
    }

    #[test]
    fn iface_parses_requirement_counts() {
        // 0 requirements
        match parse_one("interface Empty {}") {
            Stmt::Interface { name, methods, extends, .. } => {
                assert_eq!(name, "Empty");
                assert!(methods.is_empty());
                assert!(extends.is_empty());
            }
            o => panic!("expected Interface, got {o:?}"),
        }
        // 1 requirement with a return type
        match parse_one("interface Reader { fn read(b): int }") {
            Stmt::Interface { methods, .. } => {
                assert_eq!(methods.len(), 1);
                assert_eq!(methods[0].name, "read");
                assert_eq!(methods[0].params.len(), 1);
                assert!(methods[0].ret.is_some());
            }
            o => panic!("expected Interface, got {o:?}"),
        }
        // N requirements, semicolon-separated (class-body skip_semicolons rule)
        match parse_one("interface RW { fn read(b): int; fn write(b): int }") {
            Stmt::Interface { methods, .. } => assert_eq!(methods.len(), 2),
            o => panic!("expected Interface, got {o:?}"),
        }
        // N requirements, newline-separated
        match parse_one("interface RW {\n fn read(b)\n fn write(b)\n}") {
            Stmt::Interface { methods, .. } => assert_eq!(methods.len(), 2),
            o => panic!("expected Interface, got {o:?}"),
        }
    }

    #[test]
    fn iface_parses_extends_composition() {
        match parse_one("interface RW extends A, B {}") {
            Stmt::Interface { extends, methods, .. } => {
                assert_eq!(extends, vec!["A".to_string(), "B".to_string()]);
                assert!(methods.is_empty());
            }
            o => panic!("expected Interface, got {o:?}"),
        }
    }

    #[test]
    fn class_parses_implements_clause() {
        match parse_one("class C extends Super implements A, B { fn read(b) { return 0 } }") {
            Stmt::Class { superclass, implements, .. } => {
                assert_eq!(superclass, Some("Super".to_string()));
                assert_eq!(implements, vec!["A".to_string(), "B".to_string()]);
            }
            o => panic!("expected Class, got {o:?}"),
        }
        // implements with NO extends
        match parse_one("class C implements A { fn read(b) {} }") {
            Stmt::Class { superclass, implements, .. } => {
                assert_eq!(superclass, None);
                assert_eq!(implements, vec!["A".to_string()]);
            }
            o => panic!("expected Class, got {o:?}"),
        }
    }

    #[test]
    fn iface_rejects_modifiers_on_requirement() {
        for src in [
            "interface R { async fn read(b) }",
            "interface R { fn* read(b) }",
            "interface R { static fn read(b) }",
            "interface R { worker fn read(b) }",
        ] {
            let toks = lex(src).unwrap();
            assert!(parse(&toks).is_err(), "should reject: {src}");
        }
    }

    #[test]
    fn iface_can_be_exported() {
        match parse_one("export interface R { fn read(b): int }") {
            Stmt::Export(inner) => assert!(matches!(*inner, Stmt::Interface { .. })),
            o => panic!("expected Export, got {o:?}"),
        }
    }

    #[test]
    fn parses_inclusive_and_step_ranges() {
        for src in [
            "for (i in 1..=5) {}",
            "for (i in 1..10 step 2) {}",
            "for (i in 10..1 step -2) {}",
            "let xs = 1..=5",
            "let ys = 1..10 step 2",
        ] {
            let toks = lex(src).unwrap();
            assert!(parse(&toks).is_ok(), "failed to parse: {src}");
        }
    }

    #[test]
    fn for_range_carries_inclusive_and_step() {
        let toks = lex("for (i in 1..=10 step 2) {}").unwrap();
        match &parse(&toks).unwrap()[0] {
            Stmt::ForRange {
                inclusive, step, ..
            } => {
                assert!(*inclusive, "expected inclusive ..= range");
                assert!(step.is_some(), "expected a step expression");
            }
            o => panic!("expected ForRange, got {o:?}"),
        }
    }

    #[test]
    fn value_range_produces_range_node() {
        assert_eq!(sexpr("1..=5"), "1..=5");
        assert_eq!(sexpr("1..10 step 2"), "1..10 step 2");
        // `step` only binds immediately after a range end — `f(step)` is a call.
        assert_eq!(sexpr("step"), "step");
    }

    #[test]
    fn parses_rest_param_typed_and_untyped() {
        match &parse(&lex("fn f(a, ...rest: array<number>) {}").unwrap()).unwrap()[0] {
            Stmt::Fn { params, .. } => {
                assert_eq!(params.len(), 2);
                assert!(!params[0].rest);
                assert!(params[1].rest);
                assert_eq!(params[1].ty.as_ref().unwrap().to_string(), "array<number>");
            }
            o => panic!("got {o:?}"),
        }
        match &parse(&lex("fn g(...rest) {}").unwrap()).unwrap()[0] {
            Stmt::Fn { params, .. } => assert!(params[0].rest && params[0].ty.is_none()),
            o => panic!("got {o:?}"),
        }
    }

    #[test]
    fn rest_param_must_be_last() {
        assert!(parse(&lex("fn f(...rest, a) {}").unwrap()).is_err());
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
            Stmt::LetDestructureObject {
                bindings, mutable, ..
            } => {
                assert!(*mutable);
                assert_eq!(bindings[0].key, "a");
                assert_eq!(bindings[0].binding, "a");
                assert_eq!(bindings[1].key, "b");
                assert_eq!(bindings[1].binding, "local");
            }
            other => panic!("expected LetDestructureObject, got {other:?}"),
        }
    }

    #[test]
    fn parses_object_destructuring_quoted_key() {
        let p = parse(&lex(r#"let {"weird key" as wk} = obj"#).unwrap()).unwrap();
        match &p[0] {
            Stmt::LetDestructureObject { bindings, .. } => {
                assert_eq!(bindings[0].key, "weird key");
                assert_eq!(bindings[0].binding, "wk");
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
        assert!(
            parse(&lex("fn add(a: number, b: number): number { return a + b }").unwrap()).is_ok()
        );
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
            Stmt::Fn {
                name,
                is_async,
                is_generator,
                ..
            } => {
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
            Stmt::Fn {
                name,
                is_async,
                is_generator,
                ..
            } => {
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
            Stmt::Fn {
                is_async,
                is_generator,
                ..
            } => {
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
    fn parses_worker_fn_decl() {
        let p = parse(&lex("worker fn render(s) { return s }").unwrap()).unwrap();
        match &p[0] {
            Stmt::Fn { name, is_worker, is_async, .. } => {
                assert_eq!(name, "render");
                assert!(*is_worker);
                assert!(!*is_async);
            }
            other => panic!("expected Stmt::Fn, got {other:?}"),
        }
    }

    #[test]
    fn worker_is_contextual_not_reserved() {
        assert!(parse(&lex("let worker = 5").unwrap()).is_ok());
        assert!(parse(&lex("fn worker() { return 1 }").unwrap()).is_ok());
    }

    #[test]
    fn parses_static_worker_method() {
        let p = parse(&lex("class Img { static worker fn encode(px) { return px } }").unwrap()).unwrap();
        match &p[0] {
            Stmt::Class { methods, .. } => {
                assert!(methods[0].is_static);
                assert!(methods[0].is_worker);
            }
            other => panic!("expected Stmt::Class, got {other:?}"),
        }
    }

    #[test]
    fn worker_async_fn_sets_both_flags() {
        let p = parse(&lex("worker async fn foo() { return 1 }").unwrap()).unwrap();
        match &p[0] {
            Stmt::Fn { name, is_worker, is_async, .. } => {
                assert_eq!(name, "foo");
                assert!(*is_worker, "expected is_worker = true");
                assert!(*is_async, "expected is_async = true");
            }
            other => panic!("expected Stmt::Fn, got {other:?}"),
        }
    }

    #[test]
    fn export_worker_fn_has_is_worker() {
        let p = parse(&lex("export worker fn bar() { return 2 }").unwrap()).unwrap();
        match &p[0] {
            Stmt::Export(inner) => match inner.as_ref() {
                Stmt::Fn { name, is_worker, .. } => {
                    assert_eq!(name, "bar");
                    assert!(*is_worker, "expected exported fn to have is_worker = true");
                }
                other => panic!("expected Stmt::Fn inside export, got {other:?}"),
            },
            other => panic!("expected Stmt::Export, got {other:?}"),
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
                assert!(
                    matches!(&body[0], Stmt::Expr(e) if matches!(e.kind, ExprKind::Yield(None)))
                );
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
            Stmt::Fn {
                params,
                ret: Some(r),
                ..
            } => {
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
            Stmt::Class {
                fields, methods, ..
            } => {
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

    #[test]
    fn class_body_allows_semicolon_separators() {
        let p = parse(&lex("class P { x: number; y: number }").unwrap()).unwrap();
        match &p[0] {
            Stmt::Class { fields, .. } => assert_eq!(fields.len(), 2),
            other => panic!("expected Class, got {other:?}"),
        }
        assert!(parse(&lex("class C { fn a() {}; fn b() {}; }").unwrap()).is_ok());
        assert!(parse(&lex("class M { n: number; fn get() { return self.n } }").unwrap()).is_ok());
        assert!(parse(&lex("class N {\n  a: number\n  b: number\n}").unwrap()).is_ok());
        // no regression
    }

    #[test]
    fn class_body_semicolon_edge_cases() {
        assert!(parse(&lex("class C {}").unwrap()).is_ok()); // empty
        assert!(parse(&lex("class C { ; x: number }").unwrap()).is_ok()); // leading ;
        assert!(parse(&lex("class C { ; ; }").unwrap()).is_ok()); // only ;
        assert!(parse(&lex("class C { a: number;; b: number }").unwrap()).is_ok()); // doubled ;;
        assert!(parse(&lex("class C { fn a() {}; }").unwrap()).is_ok()); // trailing ;
    }

    #[test]
    fn class_with_semicolons_formats_to_newlines() {
        let out = crate::fmt::format_source("class P { x: number; y: number }").unwrap();
        assert!(out.contains("x: number\n"), "got: {out}");
        assert!(
            !out.contains(';'),
            "formatter should not emit semicolons: {out}"
        );
    }

    // ─────────────────────────── ADT (Task 3) ───────────────────────────

    fn parse_src(src: &str) -> Result<Vec<Stmt>, AsError> {
        parse(&lex(src).unwrap())
    }

    #[test]
    fn adt_enum_payload_variants_parse() {
        let src = "enum Shape { Circle(radius: float), Rect(w: float, h: float), Pair(int, int), Point }";
        let stmts = parse_src(src).expect("payload enum parses");
        match &stmts[0] {
            Stmt::Enum { variants, .. } => {
                assert_eq!(variants.len(), 4);
                // Circle: one named field `radius: float`.
                assert_eq!(variants[0].payload.len(), 1);
                assert_eq!(variants[0].payload[0].name.as_deref(), Some("radius"));
                // Rect: two named fields.
                assert_eq!(variants[1].payload.len(), 2);
                assert_eq!(variants[1].payload[1].name.as_deref(), Some("h"));
                // Pair: two positional fields (name = None).
                assert_eq!(variants[2].payload.len(), 2);
                assert!(variants[2].payload[0].name.is_none());
                // Point: unit (no payload, no backing).
                assert!(variants[3].payload.is_empty());
                assert!(variants[3].value.is_none());
            }
            o => panic!("expected Enum, got {o:?}"),
        }
    }

    #[test]
    fn adt_mixed_named_positional_is_a_parse_error() {
        let err = parse_src("enum E { Pair(int, h: float) }").unwrap_err();
        assert!(
            err.message.contains("all named or all positional"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn adt_backing_and_payload_is_a_parse_error() {
        let err = parse_src("enum E { Foo = 2(int) }").unwrap_err();
        assert!(
            err.message
                .contains("cannot have both a '= value' backing and a '(…)' payload"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn adt_variant_patterns_parse() {
        use crate::ast::{Pattern, VariantPatFields};
        fn first_arm_pattern(src: &str) -> Pattern {
            let stmts = parse_src(src).expect("match parses");
            // Find the match expression and its first arm.
            fn find(stmts: &[Stmt]) -> Option<Pattern> {
                for s in stmts {
                    if let Stmt::Expr(e) = s {
                        if let ExprKind::Match { arms, .. } = &e.kind {
                            return Some(arms[0].patterns[0].clone());
                        }
                    }
                }
                None
            }
            find(&stmts).expect("found a match arm")
        }
        // Positional bind.
        match first_arm_pattern("match s { Circle(r) => 1 }") {
            Pattern::Variant {
                enum_name,
                variant,
                fields,
            } => {
                assert!(enum_name.is_none());
                assert_eq!(&*variant, "Circle");
                match fields {
                    VariantPatFields::Positional(p) => assert_eq!(p.len(), 1),
                    _ => panic!("expected positional"),
                }
            }
            o => panic!("expected Variant, got {o}"),
        }
        // Qualified, multi-positional.
        match first_arm_pattern("match s { Shape.Pair(a, b) => 1 }") {
            Pattern::Variant {
                enum_name, fields, ..
            } => {
                assert_eq!(enum_name.as_deref(), Some("Shape"));
                match fields {
                    VariantPatFields::Positional(p) => assert_eq!(p.len(), 2),
                    _ => panic!("expected positional"),
                }
            }
            o => panic!("expected Variant, got {o}"),
        }
        // Named with rename.
        match first_arm_pattern("match s { Rect(w: ww, h: hh) => 1 }") {
            Pattern::Variant { fields, .. } => match fields {
                VariantPatFields::Named(entries) => {
                    assert_eq!(entries.len(), 2);
                    assert_eq!(&*entries[0].0, "w");
                    assert!(entries[0].1.is_some());
                }
                _ => panic!("expected named"),
            },
            o => panic!("expected Variant, got {o}"),
        }
        // Named shorthand.
        match first_arm_pattern("match s { Circle(radius) => 1 }") {
            Pattern::Variant { fields, .. } => {
                // `radius` with no colon → positional in the FIRST-field rule, since
                // there is no `:`. This is the documented convenience: a single bare
                // field binds positionally. (Named shorthand needs `field:` somewhere.)
                assert!(matches!(fields, VariantPatFields::Positional(_)));
            }
            o => panic!("expected Variant, got {o}"),
        }
        // Bare unit pattern (no parens) stays an Ident pattern (Option-C).
        assert!(matches!(
            first_arm_pattern("match s { Point => 1 }"),
            Pattern::Ident(_)
        ));
    }

    // ---- TYPE Task 4: generics surface (type-param lists, Param, FnSig) ----

    /// Pull the declared return / first-param type text out of a `fn` decl for
    /// assertions.
    fn fn_first_param_ty(stmt: &Stmt) -> Option<String> {
        match stmt {
            Stmt::Fn { params, .. } => params.first().and_then(|p| p.ty.as_ref().map(|t| t.to_string())),
            _ => None,
        }
    }
    fn fn_ret_ty(stmt: &Stmt) -> Option<String> {
        match stmt {
            Stmt::Fn { ret, .. } => ret.as_ref().map(|t| t.to_string()),
            _ => None,
        }
    }

    #[test]
    fn parses_generic_fn_type_param_list_and_param_refs() {
        // The fn RETURN type uses `:` (the real AScript syntax); the `->` arrow is
        // ONLY for a `fn(A) -> B` function-TYPE annotation.
        let s = parse_one("fn map<A, B>(xs: array<A>, f: fn(A) -> B): array<B> {}");
        // In-scope params lower to Type::Param, so `array<A>` renders `array<A>`.
        assert_eq!(fn_first_param_ty(&s).as_deref(), Some("array<A>"));
        assert_eq!(fn_ret_ty(&s).as_deref(), Some("array<B>"));
        match &s {
            Stmt::Fn { params, .. } => {
                // Second param `f: fn(A) -> B` lowered to a FnSig.
                assert_eq!(params[1].ty.as_ref().unwrap().to_string(), "fn(A) -> B");
            }
            o => panic!("expected fn decl, got {o:?}"),
        }
    }

    #[test]
    fn parses_generic_fn_bound() {
        // `fn first<T, C: Container<T>>(c: C): T` — the bound parses; `C` and `T`
        // are in-scope params in the signature (return type via `:`).
        let s = parse_one("fn first<T, C: Container<T>>(c: C): T { return c.at(0) }");
        assert_eq!(fn_first_param_ty(&s).as_deref(), Some("C"));
        assert_eq!(fn_ret_ty(&s).as_deref(), Some("T"));
    }

    #[test]
    fn parses_generic_class_enum_interface_decls() {
        assert!(matches!(
            parse_one("class Box<T> { value: T\n fn get(): T { return self.value } }"),
            Stmt::Class { .. }
        ));
        assert!(matches!(
            parse_one("enum Option<T> { Some(value: T), None }"),
            Stmt::Enum { .. }
        ));
        assert!(matches!(
            parse_one("interface Container<T> { fn len(): int\n fn at(i: int): T }"),
            Stmt::Interface { .. }
        ));
        // A class field typed by the class's own param renders the bare param name.
        match parse_one("class Box<T> { value: T }") {
            Stmt::Class { fields, .. } => assert_eq!(fields[0].ty.to_string(), "T"),
            o => panic!("expected class, got {o:?}"),
        }
    }

    #[test]
    fn parses_fnsig_type_zero_and_multi_arg() {
        // Zero-arg
        let s = parse_one("fn f(cb: fn() -> bool) {}");
        assert_eq!(fn_first_param_ty(&s).as_deref(), Some("fn() -> bool"));
        // Multi-arg, nested generic
        let s = parse_one("fn g(cb: fn(int, string) -> array<int>) {}");
        assert_eq!(
            fn_first_param_ty(&s).as_deref(),
            Some("fn(int, string) -> array<int>")
        );
    }

    #[test]
    fn nested_generic_application_closes_via_shr_split() {
        // `map<int, array<int>>` — the closing `>>` must split (NUM helper reuse).
        let s = parse_one("fn h(m: map<int, array<int>>) {}");
        assert_eq!(fn_first_param_ty(&s).as_deref(), Some("map<int, array<int>>"));
        // Box<Box<int>> nested user heads.
        let s = parse_one("fn k(b: Box<Box<int>>) {}");
        // User generic head discards args at runtime → renders the bare head name.
        assert_eq!(fn_first_param_ty(&s).as_deref(), Some("Box"));
    }

    // ---- TYPE Task 5: expression-level explicit type args (the disambiguation) ----

    /// Parse a single expression statement and return its `ExprKind`-ish structure.
    fn parse_expr(src: &str) -> Expr {
        let toks = lex(src).unwrap();
        match parse(&toks).unwrap().into_iter().next().unwrap() {
            Stmt::Expr(e) => e,
            o => panic!("expected an expression statement, got {o:?}"),
        }
    }

    #[test]
    fn explicit_type_args_parse_as_a_call() {
        // `Box<int>(5)` → a Call (type args erased: same shape as `Box(5)`).
        let e = parse_expr("Box<int>(5)");
        match &e.kind {
            ExprKind::Call { callee, args } => {
                assert!(matches!(callee.kind, ExprKind::Ident(ref n) if n == "Box"));
                assert_eq!(args.len(), 1);
            }
            o => panic!("expected a Call, got {o:?}"),
        }
        // `map<string, number>(xs, f)` → a Call with two args (type args erased).
        let e = parse_expr("map<string, number>(xs, f)");
        match &e.kind {
            ExprKind::Call { callee, args } => {
                assert!(matches!(callee.kind, ExprKind::Ident(ref n) if n == "map"));
                assert_eq!(args.len(), 2);
            }
            o => panic!("expected a Call, got {o:?}"),
        }
        // Nested: `Box<Box<int>>(5)` — the `>>` splits inside the speculative parse.
        let e = parse_expr("Box<Box<int>>(5)");
        assert!(matches!(e.kind, ExprKind::Call { .. }));
    }

    #[test]
    fn comparison_chains_are_not_type_arg_calls() {
        // None of these have a `>` immediately followed by `(`, so they stay
        // comparison/binary — the trailing-`(` rule never fires.
        for src in [
            "a < b",
            "a > b",
            "a << b",
            "a >> b",
            "a < b && c > d",
            "f(a < b, c > d)",
            "x < y ? a : b",
            "a < b > c",
        ] {
            let e = parse_expr(src);
            // The top node must NOT be a bare Call whose callee is `a`/`x` with the
            // operands swallowed — i.e. a comparison/ternary/logical stays intact.
            assert!(
                !matches!(&e.kind, ExprKind::Call { callee, .. }
                    if matches!(callee.kind, ExprKind::Ident(ref n) if n == "a" || n == "x")),
                "regressed comparison into a generic call: {src:?} → {:?}",
                e.kind
            );
        }
    }

    #[test]
    fn comparison_a_lt_b_stays_binary() {
        let e = parse_expr("a < b");
        assert!(matches!(
            e.kind,
            ExprKind::Binary { op: BinOp::Lt, .. }
        ));
        let e = parse_expr("a >> b");
        assert!(matches!(
            e.kind,
            ExprKind::Binary { op: BinOp::Shr, .. }
        ));
    }

    #[test]
    fn type_param_only_in_scope_inside_its_decl() {
        // Outside any generic decl, a bare `T` is an ordinary Named type, not a Param.
        let s = parse_one("fn plain(x: T) {}");
        match &s {
            Stmt::Fn { params, .. } => {
                // Renders identically (`T`), but it is a Named, not a Param.
                assert!(matches!(params[0].ty, Some(crate::ast::Type::Named(_))));
            }
            o => panic!("got {o:?}"),
        }
        // Inside a generic decl, the SAME `T` lowers to a Param.
        let s = parse_one("fn gen<T>(x: T) {}");
        match &s {
            Stmt::Fn { params, .. } => {
                assert!(matches!(params[0].ty, Some(crate::ast::Type::Param(_))));
            }
            o => panic!("got {o:?}"),
        }
    }

    // ---- DEFER Task 1.1: reserve keyword + legacy parser ----

    #[test]
    fn defer_is_reserved_and_call_only() {
        // Accepted forms — these must all parse without error.
        for src in [
            "fn f() { defer g() }",
            "fn f() { defer obj.close() }",
            "fn f() { defer a?.flush() }",
            "fn f() { defer (cond ? a : b)() }",
            "fn f() { defer (() => { print(1) })() }",
            "fn f() { defer g(...xs) }",
            "fn f() { defer await g() }",
            "defer g()",  // top level is legal
        ] {
            assert!(
                parse_src(src).is_ok(),
                "should accept: {src}"
            );
        }

        // Rejected: non-call expressions.
        for src in [
            "fn f() { defer x }",
            "fn f() { defer a + b }",
            "fn f() { defer g }",
            "fn f() { defer g()? }",
            "fn f() { defer g()! }",
        ] {
            let err = parse_src(src).unwrap_err();
            assert!(
                err.message.contains("defer requires a call"),
                "expected 'defer requires a call' for `{src}`, got: {}",
                err.message
            );
        }

        // Rejected: named-argument calls (spec §2.1 v1 Tier-1).
        let err = parse_src("fn f() { defer g(x: 1) }").unwrap_err();
        assert!(
            err.message.contains("defer does not support named-argument calls"),
            "expected named-arg error, got: {}",
            err.message
        );

        // `defer` is a RESERVED keyword — cannot be used as an identifier.
        assert!(
            parse_src("let defer = 5").is_err(),
            "defer must be reserved (let defer = 5)"
        );
        assert!(
            parse_src("fn defer() {}").is_err(),
            "defer must be reserved (fn defer() {{}})"
        );
    }

    #[test]
    fn defer_stmt_node_fields() {
        // Verify the AST node carries the right fields.
        let s = parse_one("fn f() { defer g() }");
        match s {
            Stmt::Fn { body, .. } => match &body[0] {
                Stmt::Defer { awaited, .. } => {
                    assert!(!awaited, "plain defer should have awaited=false");
                }
                o => panic!("expected Stmt::Defer, got {o:?}"),
            },
            o => panic!("expected Stmt::Fn, got {o:?}"),
        }

        let s = parse_one("fn f() { defer await teardown() }");
        match s {
            Stmt::Fn { body, .. } => match &body[0] {
                Stmt::Defer { awaited, call, .. } => {
                    assert!(*awaited, "defer await should have awaited=true");
                    assert!(
                        matches!(call.kind, ExprKind::Call { .. }),
                        "call must be ExprKind::Call"
                    );
                }
                o => panic!("expected Stmt::Defer, got {o:?}"),
            },
            o => panic!("expected Stmt::Fn, got {o:?}"),
        }
    }
}
