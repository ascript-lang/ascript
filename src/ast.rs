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
    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<CallArg>,
    },
    Assign {
        target: Box<Expr>,
        value: Box<Expr>,
    },
    Arrow {
        params: Vec<Param>,
        body: Box<ArrowBody>,
        is_async: bool,
        is_generator: bool,
    },
    Array(Vec<ArrayElem>),
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    Object(Vec<ObjEntry>),
    Member {
        object: Box<Expr>,
        name: String,
    },
    OptMember {
        object: Box<Expr>,
        name: String,
    },
    Try(Box<Expr>),
    /// `expr!` — force-unwrap a Tier-1 `[value, err]` pair: evaluates to `value`
    /// when `err == nil`, otherwise panics (carrying the original error's
    /// message). The dual of `Try` (`?`).
    Unwrap(Box<Expr>),
    /// The conditional operator `cond ? then : els` (spec §3). Right-associative,
    /// binds just above assignment. `then`/`els` are evaluated lazily — only the
    /// selected branch runs.
    Ternary {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    Template {
        parts: Vec<TemplatePart>,
    },
    Match {
        subject: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    Await(Box<Expr>),
    /// `yield` / `yield <expr>` inside a generator body (`fn*` / `async fn*`).
    /// Hands a value to the consumer and evaluates to the resume value the
    /// consumer passed via `gen.next(v)` (`nil` for `next()` / `for await`).
    Yield(Option<Box<Expr>>),
    /// A parenthesized expression, kept distinct (not flattened) so parentheses
    /// break an optional chain: `(a?.b).c` errors on `.c` rather than
    /// short-circuiting (spec §4, matching JS).
    Paren(Box<Expr>),
}

/// An element of an array literal: a plain item `x` or a spread `...x`.
/// Spreading a non-array is a runtime panic (strict, no coercion).
#[derive(Debug, Clone)]
pub enum ArrayElem {
    Item(Expr),
    Spread(Expr),
}

/// An entry in an object literal: a key/value `k: v` or a spread `...o`.
/// Object-spread is later-value-wins; `IndexMap` keeps first-seen key position.
/// Spreading a non-object is a runtime panic (strict, no coercion).
#[derive(Debug, Clone)]
pub enum ObjEntry {
    KV(String, Expr),
    Spread(Expr),
}

/// A call argument: positional `x` or a spread `...args`.
/// Spreading a non-array as call args is a runtime panic (strict).
#[derive(Debug, Clone)]
pub enum CallArg {
    Pos(Expr),
    Spread(Expr),
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
    Map(Box<Type>, Box<Type>),
    Future(Box<Type>),
    /// `T?` — nullable type, sugar for `T | nil`. The class-field marker
    /// `name?:` will also lower to this node once class fields land (Phase 3).
    Optional(Box<Type>),
}

/// A function parameter: a name with an optional type annotation.
#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: Option<Type>,
    /// Span of just the parameter name (for LSP go-to-definition).
    pub name_span: Span,
    /// `true` if this is a rest parameter (`...name`), which collects trailing
    /// arguments into an array. A rest parameter must be the last parameter.
    pub rest: bool,
}

/// One `{key as binding}` entry in an object-destructuring pattern. `key` is the
/// SOURCE key looked up in the value; `binding` is the local name introduced
/// (equal to `key` for the shorthand `{key}`). `key_span` covers the key token,
/// `binding_span` the local name (they coincide for shorthand).
#[derive(Debug, Clone, PartialEq)]
pub struct ObjBinding {
    pub key: String,
    pub binding: String,
    pub key_span: Span,
    pub binding_span: Span,
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
            Type::Map(k, v) => write!(f, "map<{}, {}>", k, v),
            Type::Future(t) => write!(f, "future<{}>", t),
            Type::Optional(t) => write!(f, "{}?", t),
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
    /// `span` covers the whole declaration; `name_span` covers just the bound name
    /// (used by the LSP for symbol selection ranges and go-to-definition).
    Let {
        name: String,
        ty: Option<Type>,
        value: Option<Expr>,
        mutable: bool,
        span: Span,
        name_span: Span,
    },
    /// `name_spans[i]` covers the i-th destructured name; `span` covers the whole
    /// declaration.
    LetDestructure {
        names: Vec<String>,
        /// Optional `...name` collector for trailing elements (`let [a, ...rest] = arr`).
        rest: Option<(String, Span)>,
        value: Expr,
        mutable: bool,
        span: Span,
        name_spans: Vec<Span>,
    },
    /// `let {a, b as local} = expr` — object destructuring (binds by key name).
    LetDestructureObject {
        bindings: Vec<ObjBinding>,
        /// Optional trailing `...name` rest collector — gathers the leftover keys
        /// (those not named by `bindings`) into a new object.
        rest: Option<(String, Span)>,
        value: Expr,
        mutable: bool,
        span: Span,
    },
    Block(Vec<Stmt>),
    If {
        cond: Expr,
        then_branch: Vec<Stmt>,
        else_branch: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    ForRange {
        var: String,
        start: Expr,
        end: Expr,
        body: Vec<Stmt>,
    },
    ForOf {
        var: String,
        iter: Expr,
        body: Vec<Stmt>,
        for_await: bool,
    },
    Return(Option<Expr>),
    Break,
    Continue,
    Fn {
        name: String,
        params: Vec<Param>,
        ret: Option<Type>,
        body: Vec<Stmt>,
        is_async: bool,
        is_generator: bool,
        span: Span,
        name_span: Span,
    },
    Enum {
        name: String,
        variants: Vec<EnumVariantDecl>,
        span: Span,
        name_span: Span,
    },
    Class {
        name: String,
        superclass: Option<String>,
        fields: Vec<FieldDecl>,
        methods: Vec<MethodDecl>,
        span: Span,
        name_span: Span,
    },
    Import {
        names: ImportNames,
        source: String,
    },
    Export(Box<Stmt>),
}

#[derive(Clone, Debug)]
pub enum ImportNames {
    Named(Vec<String>),
    Namespace(String),
}

#[derive(Clone, Debug)]
pub struct FieldDecl {
    pub name: String,
    pub ty: Type,
    /// Lazily-evaluated default (in the class def env) when the field is absent.
    pub default: Option<Expr>,
    pub span: Span,
    pub name_span: Span,
}

#[derive(Clone, Debug)]
pub struct MethodDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub body: Vec<Stmt>,
    pub is_async: bool,
    pub is_generator: bool,
    /// Span of the method (for LSP symbol range).
    pub span: Span,
    /// Span of just the method name (for LSP selection range).
    pub name_span: Span,
}

#[derive(Clone, Debug)]
pub struct MatchArm {
    /// One or more `|`-separated patterns (an or-pattern); the arm fires when ANY
    /// matches. (A bare `_` is `Pattern::Wildcard`.)
    pub patterns: Vec<Pattern>,
    /// Optional `if <cond>` guard, evaluated in the arm scope (with bindings) after
    /// the pattern structurally matches; a falsy guard rejects the arm.
    pub guard: Option<Expr>,
    pub body: Expr,
}

/// A `match`-arm pattern (Phase 8a). Bare identifiers are resolved at match time
/// (Option C): a name DEFINED in the enclosing scope is a value-compare, an
/// UNDEFINED name binds the subject.
#[derive(Clone, Debug)]
pub enum Pattern {
    /// `_` — matches anything, binds nothing.
    Wildcard,
    /// A bare identifier — Option-C resolved (compare if defined, bind if new).
    Ident(std::rc::Rc<str>),
    /// Any value expression (literal, enum ref, member access, call, `1+1`, …) —
    /// evaluated then compared with `==`.
    Value(Box<Expr>),
    /// `a..b` (exclusive) / `a..=b` (inclusive) — subject is a Number in range.
    Range {
        start: Box<Expr>,
        end: Box<Expr>,
        inclusive: bool,
    },
    /// `[p0, p1, ...]` — subject is an array; exact arity unless a trailing rest.
    /// The rest: `None` = no rest, `Some(None)` = `...` (ignore), `Some(Some(n))`
    /// = `...n` (bind remainder as an array).
    Array(Vec<Pattern>, Option<Option<std::rc::Rc<str>>>),
    /// `{key, key2: subpat, ...}` — subject is an Object/Instance with the keys.
    /// Rest as for `Array` but binds remaining keys into a new Object.
    Object(Vec<ObjPatEntry>, Option<Option<std::rc::Rc<str>>>),
}

/// One entry in an object pattern. `pat: None` is the shorthand `{key}` which
/// ALWAYS binds `key` to that field (documented exception to Option C);
/// `pat: Some(p)` is `{key: p}` and matches the field against `p`.
#[derive(Clone, Debug)]
pub struct ObjPatEntry {
    pub key: std::rc::Rc<str>,
    pub pat: Option<Pattern>,
}

impl std::fmt::Display for Pattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Pattern::Wildcard => write!(f, "_"),
            Pattern::Ident(n) => write!(f, "{}", n),
            Pattern::Value(e) => write!(f, "{}", e),
            Pattern::Range {
                start,
                end,
                inclusive,
            } => {
                let op = if *inclusive { "..=" } else { ".." };
                write!(f, "{}{}{}", start, op, end)
            }
            Pattern::Array(pats, rest) => {
                write!(f, "[")?;
                for (i, p) in pats.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", p)?;
                }
                match rest {
                    None => {}
                    Some(None) => {
                        if !pats.is_empty() {
                            write!(f, ", ")?;
                        }
                        write!(f, "...")?;
                    }
                    Some(Some(n)) => {
                        if !pats.is_empty() {
                            write!(f, ", ")?;
                        }
                        write!(f, "...{}", n)?;
                    }
                }
                write!(f, "]")
            }
            Pattern::Object(entries, rest) => {
                write!(f, "{{")?;
                for (i, e) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    match &e.pat {
                        None => write!(f, "{}", e.key)?,
                        Some(p) => write!(f, "{}: {}", e.key, p)?,
                    }
                }
                match rest {
                    None => {}
                    Some(None) => {
                        if !entries.is_empty() {
                            write!(f, ", ")?;
                        }
                        write!(f, "...")?;
                    }
                    Some(Some(n)) => {
                        if !entries.is_empty() {
                            write!(f, ", ")?;
                        }
                        write!(f, "...{}", n)?;
                    }
                }
                write!(f, "}}")
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct EnumVariantDecl {
    pub name: String,
    pub value: Option<Expr>,
    /// Span of the variant name (for LSP selection range).
    pub name_span: Span,
}

#[derive(Clone, Copy, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
    Coalesce,
    Range,
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
            BinOp::Range => "..",
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
                    match a {
                        CallArg::Pos(x) => write!(f, " {}", x)?,
                        CallArg::Spread(x) => write!(f, " ...{}", x)?,
                    }
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
                    match it {
                        ArrayElem::Item(x) => write!(f, "{}", x)?,
                        ArrayElem::Spread(x) => write!(f, "...{}", x)?,
                    }
                }
                write!(f, "]")
            }
            ExprKind::Index { object, index } => write!(f, "(index {} {})", object, index),
            ExprKind::Object(entries) => {
                write!(f, "{{")?;
                for (i, e) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    match e {
                        ObjEntry::KV(k, v) => write!(f, "{}: {}", k, v)?,
                        ObjEntry::Spread(x) => write!(f, "...{}", x)?,
                    }
                }
                write!(f, "}}")
            }
            ExprKind::Member { object, name } => write!(f, "(. {} {})", object, name),
            ExprKind::OptMember { object, name } => write!(f, "(?. {} {})", object, name),
            ExprKind::Try(e) => write!(f, "(? {})", e),
            ExprKind::Unwrap(e) => write!(f, "(unwrap {})", e),
            ExprKind::Ternary { cond, then, els } => write!(f, "(?: {} {} {})", cond, then, els),
            ExprKind::Template { .. } => write!(f, "(template)"),
            ExprKind::Match { .. } => write!(f, "(match)"),
            ExprKind::Await(e) => write!(f, "(await {})", e),
            ExprKind::Yield(Some(e)) => write!(f, "(yield {})", e),
            ExprKind::Yield(None) => write!(f, "(yield)"),
            ExprKind::Paren(inner) => write!(f, "{}", inner),
        }
    }
}
