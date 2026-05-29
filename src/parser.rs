//! Recursive-descent / precedence-climbing parser.

use crate::ast::{BinOp, Expr, ExprKind, Stmt, UnOp};
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
        while *self.peek() != Tok::Eof {
            stmts.push(Stmt::Expr(self.expr()?));
        }
        Ok(stmts)
    }

    fn expr(&mut self) -> Result<Expr, AsError> {
        self.additive()
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
            let span = Span::new(left.span.start, right.span.end);
            left = Expr {
                kind: ExprKind::Binary { op, lhs: Box::new(left), rhs: Box::new(right) },
                span,
            };
        }
        Ok(left)
    }

    fn multiplicative(&mut self) -> Result<Expr, AsError> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Mod,
                _ => break,
            };
            self.advance();
            let right = self.unary()?;
            let span = Span::new(left.span.start, right.span.end);
            left = Expr {
                kind: ExprKind::Binary { op, lhs: Box::new(left), rhs: Box::new(right) },
                span,
            };
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, AsError> {
        let start = self.span().start;
        if *self.peek() == Tok::Minus {
            self.advance();
            let operand = self.unary()?;
            let span = Span::new(start, operand.span.end);
            return Ok(Expr {
                kind: ExprKind::Unary { op: UnOp::Neg, expr: Box::new(operand) },
                span,
            });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, AsError> {
        let mut expr = self.primary()?;
        while *self.peek() == Tok::LParen {
            self.advance();
            let mut args = Vec::new();
            if *self.peek() != Tok::RParen {
                loop {
                    args.push(self.expr()?);
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
            }
            self.eat(&Tok::RParen)?;
            let span = Span::new(expr.span.start, self.prev_end());
            expr = Expr {
                kind: ExprKind::Call { callee: Box::new(expr), args },
                span,
            };
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
                return Ok(inner);
            }
            other => return Err(AsError::at(format!("unexpected token {:?}", other), tok_span)),
        };
        Ok(Expr { kind, span: tok_span })
    }
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
        }
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
    fn binary_span_covers_both_operands() {
        let tokens = lex("1 + 2").unwrap();
        let stmts = parse(&tokens).unwrap();
        match &stmts[0] {
            Stmt::Expr(e) => assert_eq!(e.span, Span::new(0, 5)),
        }
    }
}
