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
    Assign { target: Box<Expr>, value: Box<Expr> },
    Arrow { params: Vec<String>, body: Box<ArrowBody> },
    Array(Vec<Expr>),
    Index { object: Box<Expr>, index: Box<Expr> },
}

#[derive(Clone, Debug)]
pub enum ArrowBody {
    Expr(Box<Expr>),
    Block(Vec<Stmt>),
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Expr(Expr),
    Let { name: String, value: Expr, mutable: bool },
    Block(Vec<Stmt>),
    If { cond: Expr, then_branch: Vec<Stmt>, else_branch: Option<Vec<Stmt>> },
    While { cond: Expr, body: Vec<Stmt> },
    ForRange { var: String, start: Expr, end: Expr, body: Vec<Stmt> },
    Return(Option<Expr>),
    Break,
    Continue,
    Fn { name: String, params: Vec<String>, body: Vec<Stmt> },
}

#[derive(Clone, Copy, Debug)]
pub enum BinOp {
    Add, Sub, Mul, Div, Mod, Pow,
    Lt, Le, Gt, Ge, Eq, Ne,
    And, Or, Coalesce,
}

#[derive(Clone, Copy, Debug)]
pub enum UnOp {
    Neg,
    Not,
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
            BinOp::Pow => "**",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::And => "&&",
            BinOp::Or => "||",
            BinOp::Coalesce => "??",
        };
        write!(f, "{}", s)
    }
}

impl fmt::Display for UnOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnOp::Neg => write!(f, "-"),
            UnOp::Not => write!(f, "!"),
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
            ExprKind::Assign { target, value } => write!(f, "(= {} {})", target, value),
            ExprKind::Arrow { params, .. } => write!(f, "(arrow [{}])", params.join(" ")),
            ExprKind::Array(items) => {
                write!(f, "[")?;
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{}", it)?;
                }
                write!(f, "]")
            }
            ExprKind::Index { object, index } => write!(f, "(index {} {})", object, index),
        }
    }
}
