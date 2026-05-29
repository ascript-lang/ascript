//! Abstract syntax tree.

use crate::span::Span;
use std::fmt;

/// An expression node plus the source span it was parsed from.
#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Number(f64),
    Str(String),
    Bool(bool),
    Nil,
    Ident(String),
    Unary { op: UnOp, expr: Box<Expr> },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr> },
    Call { callee: Box<Expr>, args: Vec<Expr> },
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Expr(Expr),
}

#[derive(Clone, Copy, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Clone, Copy, Debug)]
pub enum UnOp {
    Neg,
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
        };
        write!(f, "{}", s)
    }
}

impl fmt::Display for UnOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnOp::Neg => write!(f, "-"),
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}

impl fmt::Display for ExprKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExprKind::Number(n) => write!(f, "{}", n),
            ExprKind::Str(s) => write!(f, "{:?}", s),
            ExprKind::Bool(b) => write!(f, "{}", b),
            ExprKind::Nil => write!(f, "nil"),
            ExprKind::Ident(name) => write!(f, "{}", name),
            ExprKind::Unary { op, expr } => write!(f, "({} {})", op, expr),
            ExprKind::Binary { op, lhs, rhs } => write!(f, "({} {} {})", op, lhs, rhs),
            ExprKind::Call { callee, args } => {
                write!(f, "(call {}", callee)?;
                for a in args {
                    write!(f, " {}", a)?;
                }
                write!(f, ")")
            }
        }
    }
}
