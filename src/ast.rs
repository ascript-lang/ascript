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
    Arrow { params: Vec<Param>, body: Box<ArrowBody> },
    Array(Vec<Expr>),
    Index { object: Box<Expr>, index: Box<Expr> },
    Object(Vec<(String, Expr)>),
    Member { object: Box<Expr>, name: String },
    OptMember { object: Box<Expr>, name: String },
    Try(Box<Expr>),
    Template { parts: Vec<TemplatePart> },
    Match { subject: Box<Expr>, arms: Vec<MatchArm> },
    /// A parenthesized expression, kept distinct (not flattened) so parentheses
    /// break an optional chain: `(a?.b).c` errors on `.c` rather than
    /// short-circuiting (spec §4, matching JS).
    Paren(Box<Expr>),
}

/// A type annotation (spec §5). Checked at runtime as a contract.
#[derive(Clone, Debug)]
pub enum Type {
    Number,
    String,
    Bool,
    Nil,
    Any,
    Fn,
    Object,
    Error, // object | nil
    Array(Box<Type>),
    Result(Box<Type>),
    Tuple(Vec<Type>),
    Union(Box<Type>, Box<Type>),
    Named(String),
}

/// A function parameter: a name with an optional type annotation.
#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: Option<Type>,
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Number => write!(f, "number"),
            Type::String => write!(f, "string"),
            Type::Bool => write!(f, "bool"),
            Type::Nil => write!(f, "nil"),
            Type::Any => write!(f, "any"),
            Type::Fn => write!(f, "fn"),
            Type::Object => write!(f, "object"),
            Type::Error => write!(f, "error"),
            Type::Array(t) => write!(f, "array<{}>", t),
            Type::Result(t) => write!(f, "Result<{}>", t),
            Type::Tuple(ts) => {
                write!(f, "[")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", t)?;
                }
                write!(f, "]")
            }
            Type::Union(a, b) => write!(f, "{} | {}", a, b),
            Type::Named(n) => write!(f, "{}", n),
        }
    }
}

#[derive(Clone, Debug)]
pub enum TemplatePart {
    Lit(String),
    Expr(Box<Expr>),
}

#[derive(Clone, Debug)]
pub enum ArrowBody {
    Expr(Box<Expr>),
    Block(Vec<Stmt>),
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Expr(Expr),
    Let { name: String, ty: Option<Type>, value: Expr, mutable: bool },
    Block(Vec<Stmt>),
    If { cond: Expr, then_branch: Vec<Stmt>, else_branch: Option<Vec<Stmt>> },
    While { cond: Expr, body: Vec<Stmt> },
    ForRange { var: String, start: Expr, end: Expr, body: Vec<Stmt> },
    ForOf { var: String, iter: Expr, body: Vec<Stmt> },
    Return(Option<Expr>),
    Break,
    Continue,
    Fn { name: String, params: Vec<Param>, ret: Option<Type>, body: Vec<Stmt> },
    Enum { name: String, variants: Vec<EnumVariantDecl> },
    Class { name: String, superclass: Option<String>, methods: Vec<MethodDecl> },
}

#[derive(Clone, Debug)]
pub struct MethodDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct MatchArm {
    /// Patterns are value-expressions compared with `==`; `None` patterns list
    /// means a wildcard `_`. Multiple patterns = an or-pattern.
    pub patterns: Option<Vec<Expr>>,
    pub body: Expr,
}

#[derive(Clone, Debug)]
pub struct EnumVariantDecl {
    pub name: String,
    pub value: Option<Expr>,
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
            ExprKind::Arrow { params, .. } => {
                let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
                write!(f, "(arrow [{}])", names.join(" "))
            }
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
            ExprKind::Object(entries) => {
                write!(f, "{{")?;
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{}: {}", k, v)?;
                }
                write!(f, "}}")
            }
            ExprKind::Member { object, name } => write!(f, "(. {} {})", object, name),
            ExprKind::OptMember { object, name } => write!(f, "(?. {} {})", object, name),
            ExprKind::Try(e) => write!(f, "(? {})", e),
            ExprKind::Template { .. } => write!(f, "(template)"),
            ExprKind::Match { .. } => write!(f, "(match)"),
            ExprKind::Paren(inner) => write!(f, "{}", inner),
        }
    }
}
