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
    /// An integer literal (NUM §3.1): a digit sequence with no `.` and no
    /// exponent (decimal / `0x` / `0b` / `0o`, underscores allowed).
    Int(i64),
    /// A float literal (NUM §3.1): contains a `.` or an exponent.
    Float(f64),
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
    /// `#{ keyExpr: valueExpr, … }` — a map literal producing a `Value::Map`.
    /// Unlike object literals, the KEY is an arbitrary evaluated expression
    /// (converted via `MapKey::from_value`), not a bare identifier name.
    /// Spread is not representable (out of scope for SP2; a `...` inside `#{}`
    /// is a parse error). Later-key-wins; insertion order = first-seen key.
    Map(Vec<MapEntry>),
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
    /// `a..b`, `a..=b`, optionally `… step k` — value position. Materializes to
    /// `array<number>` at eval. The dedicated successor to the old
    /// `Binary { op: BinOp::Range, .. }` path (which still exists for this task;
    /// the parser starts producing `Range` in Task 3).
    Range {
        start: Box<Expr>,
        end: Box<Expr>,
        /// `true` for `..=` (inclusive upper bound), `false` for `..`.
        inclusive: bool,
        /// Optional signed `step k` modifier; `None` means the default ±1.
        step: Option<Box<Expr>>,
    },
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

/// An entry in a `#{…}` map literal: `key: value`, where BOTH the key and the
/// value are arbitrary evaluated expressions. The key is converted to a
/// `MapKey` at eval (an unhashable key is a Tier-2 panic). Later-key-wins.
#[derive(Debug, Clone)]
pub struct MapEntry {
    pub key: Expr,
    pub value: Expr,
}

/// A call argument: positional `x`, a spread `...args`, or a named `name: x`.
/// Spreading a non-array as call args is a runtime panic (strict). Named args
/// (ADT §3.2) are accepted in any call's surface but are only MEANINGFUL for an
/// enum-variant constructor call (`Shape.Rect(w: 3.0, h: 4.0)`); a named arg on
/// any other callee is a recoverable Tier-2 error.
#[derive(Debug, Clone)]
pub enum CallArg {
    Pos(Expr),
    Spread(Expr),
    Named { name: std::rc::Rc<str>, value: Expr },
}

/// A type annotation (spec §5). Checked at runtime as a contract.
#[derive(Clone, Debug)]
pub enum Type {
    /// `number` — the union `int | float`; accepts either numeric subtype.
    Number,
    /// `int` (NUM) — accepts only `Value::Int`.
    Int,
    /// `float` (NUM) — accepts only `Value::Float`.
    Float,
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
    /// TYPE §5.4: a generic type PARAMETER reference (`T` inside a `fn f<T>(...)` /
    /// `class Box<T>` etc.). Generics are RUNTIME-ERASED — this carries NO runtime
    /// obligation: `check_type` treats it as accept-anything (exactly like `Any`).
    /// The static checker (`src/check/infer/`) is what enforces a `T`'s consistency.
    Param(String),
    /// TYPE §5.4: a parameterized FUNCTION type (`fn(A) -> B`) — a strict extension
    /// of the bare `Fn`. Also runtime-erased: a value of this type is checked as a
    /// plain callable (`Fn`) at runtime; the param/return signature is advisory and
    /// consumed only by the static checker.
    FnSig(Vec<Type>, Box<Type>),
}

/// TYPE §6: one declared generic type parameter — a name with an optional
/// interface bound (`T` or `C: Container<T>`). Produced by the parsers' type-param
/// lists. RUNTIME-ERASED: the runtime decl nodes do not store these (generics carry
/// no runtime obligation); they are consumed only by the static checker (TYPE Tasks
/// 8–12), which records the names + bounds in its symbol table.
#[derive(Clone, Debug)]
pub struct TypeParam {
    pub name: String,
    pub bound: Option<Type>,
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
    /// Default value expression (`fn f(a, b = expr)`), evaluated at CALL time in
    /// the callee frame when the corresponding trailing argument is omitted. A
    /// default may reference earlier already-bound params and the enclosing
    /// scope. A required (no-default) param may not follow a defaulted one
    /// (parse/compile error). Mirrors `FieldDecl.default`.
    pub default: Option<Expr>,
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
            Type::Int => write!(f, "int"),
            Type::Float => write!(f, "float"),
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
            Type::Param(name) => write!(f, "{}", name),
            Type::FnSig(params, ret) => {
                write!(f, "fn(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", p)?;
                }
                write!(f, ") -> {}", ret)
            }
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
        /// `true` for `..=` (inclusive upper bound), `false` for `..`.
        inclusive: bool,
        /// Optional signed `step k` modifier; `None` means the default ±1.
        step: Option<Expr>,
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
        is_worker: bool,
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
        /// IFACE: the interface names this class asserts conformance to
        /// (`class C extends Super implements A, B`). Empty when no clause. The
        /// RUNTIME ignores it (conformance is structural); the checker proves it
        /// (`implements-violation`, TYPE-era). Contextual `implements` keyword.
        implements: Vec<String>,
        fields: Vec<FieldDecl>,
        methods: Vec<MethodDecl>,
        /// `worker class C { … }` — Spec B: a stateful actor class whose instances
        /// are spawned into a dedicated isolate. The runtime side is wired in Task 5.
        is_worker: bool,
        span: Span,
        name_span: Span,
    },
    /// IFACE §3: a structural interface declaration — a named method SET (no bodies).
    /// Binds a `Value::Interface` module-global. `extends` composes other interfaces
    /// (transitive union, lazy). `type_params` reserves generics (empty in v1, §6.1).
    Interface {
        name: String,
        type_params: Vec<String>,
        extends: Vec<String>,
        methods: Vec<MethodReqNode>,
        span: Span,
        name_span: Span,
    },
    Import {
        names: ImportNames,
        source: String,
    },
    Export(Box<Stmt>),
    /// DEFER §2.1: `defer [await] <call>` — registers a call to run at enclosing
    /// function-body exit (Go semantics, LIFO). `call` is guaranteed `ExprKind::Call`
    /// by the parser. `awaited` records the `defer await` statement form.
    Defer {
        call: Expr,
        awaited: bool,
        span: Span,
    },
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

/// IFACE §3: one method REQUIREMENT in an `interface` body — a signature with NO
/// body. `async`/`fn*`/`static`/`worker` modifiers are rejected at parse time in v1.
#[derive(Clone, Debug)]
pub struct MethodReqNode {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    /// Span of the whole requirement (for LSP symbol range).
    pub span: Span,
    /// Span of just the method name (for LSP selection range / go-to-def).
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
    /// `worker fn` / `static worker fn` — Spec A: dispatched to a pooled isolate, returns future<T>.
    pub is_worker: bool,
    /// `static fn` / `static async fn` / `static fn*` — a class-level method with
    /// no `self`, called as `C.name(args)` (SP1 Phase C).
    pub is_static: bool,
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
    /// `a..b` (exclusive) / `a..=b` (inclusive), optionally `… step k` — subject
    /// is a Number in range (with strided membership when `step` is present).
    Range {
        start: Box<Expr>,
        end: Box<Expr>,
        inclusive: bool,
        /// Optional signed `step k` modifier; `None` means the default ±1.
        step: Option<Box<Expr>>,
    },
    /// `[p0, p1, ...]` — subject is an array; exact arity unless a trailing rest.
    /// The rest: `None` = no rest, `Some(None)` = `...` (ignore), `Some(Some(n))`
    /// = `...n` (bind remainder as an array).
    Array(Vec<Pattern>, Option<Option<std::rc::Rc<str>>>),
    /// `{key, key2: subpat, ...}` — subject is an Object/Instance with the keys.
    /// Rest as for `Array` but binds remaining keys into a new Object.
    Object(Vec<ObjPatEntry>, Option<Option<std::rc::Rc<str>>>),
    /// ADT: a variant-destructuring pattern — `Circle(r)`, `Shape.Circle(r)`,
    /// `Pair(a, b)`, `Rect(w: ww, h: hh)`. Matches when the subject is an
    /// `EnumVariant` of `variant` (and, if `enum_name` is `Some`, that enum), then
    /// destructures the payload by position or by field name.
    Variant {
        /// `Some` when written qualified (`Shape.Circle(r)`); `None` when bare
        /// (`Circle(r)`). A bare form matches any enum's variant of that name.
        enum_name: Option<std::rc::Rc<str>>,
        variant: std::rc::Rc<str>,
        fields: VariantPatFields,
    },
}

/// ADT: the sub-pattern shape of a `Pattern::Variant`. Positional binds by index
/// (`Pair(a, b)`); named binds by field, optionally renaming (`Rect(w: ww)` matches
/// field `w` against sub-pattern `ww`; `Rect(w)` shorthand binds field `w`).
#[derive(Clone, Debug)]
pub enum VariantPatFields {
    Positional(Vec<Pattern>),
    Named(Vec<(std::rc::Rc<str>, Option<Pattern>)>),
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
                step,
            } => {
                let op = if *inclusive { "..=" } else { ".." };
                write!(f, "{}{}{}", start, op, end)?;
                if let Some(k) = step {
                    write!(f, " step {}", k)?;
                }
                Ok(())
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
            Pattern::Variant {
                enum_name,
                variant,
                fields,
            } => {
                if let Some(en) = enum_name {
                    write!(f, "{}.", en)?;
                }
                write!(f, "{}(", variant)?;
                match fields {
                    VariantPatFields::Positional(pats) => {
                        for (i, p) in pats.iter().enumerate() {
                            if i > 0 {
                                write!(f, ", ")?;
                            }
                            write!(f, "{}", p)?;
                        }
                    }
                    VariantPatFields::Named(entries) => {
                        for (i, (k, p)) in entries.iter().enumerate() {
                            if i > 0 {
                                write!(f, ", ")?;
                            }
                            match p {
                                None => write!(f, "{}", k)?,
                                Some(p) => write!(f, "{}: {}", k, p)?,
                            }
                        }
                    }
                }
                write!(f, ")")
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct EnumVariantDecl {
    pub name: String,
    /// Scalar backing (`Red = 2`). MUTUALLY EXCLUSIVE with `payload` (a variant has
    /// EITHER a `= scalar` backing OR a `(…)` payload, never both — parse error).
    pub value: Option<Expr>,
    /// ADT: payload fields (`Circle(radius: float)` / `Pair(int, int)`). Empty for a
    /// unit / scalar-backed variant. A field's `name` is `Some` for a named-field
    /// variant, `None` for positional; uniformity (all-named XOR all-positional) is
    /// enforced at parse time.
    pub payload: Vec<VariantField>,
    /// Span of the variant name (for LSP selection range).
    pub name_span: Span,
}

/// ADT: one declared payload field of an enum variant. `name` is `Some` for a named
/// field (`radius: float`), `None` for a positional one (`int`). The type is required.
#[derive(Clone, Debug)]
pub struct VariantField {
    pub name: Option<std::rc::Rc<str>>,
    pub ty: Type,
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
    InstanceOf,
    /// `&` — int bitwise AND (NUM §3.2). A float operand is a Tier-2 panic.
    BitAnd,
    /// `|` — int bitwise OR (NUM §3.2). In value position only; in pattern/type
    /// position `|` is an or-pattern / union (the parsers route around this tier).
    BitOr,
    /// `^` — int bitwise XOR (NUM §3.2).
    BitXor,
    /// `<<` — int left shift (NUM §3.2). Shift amount `< 0` or `>= 64` panics.
    Shl,
    /// `>>` — int arithmetic (sign-extending) right shift (NUM §3.2).
    Shr,
    /// `+%` — int two's-complement wrapping add (NUM §3.2). Never panics.
    WrapAdd,
    /// `-%` — int two's-complement wrapping subtract (NUM §3.2). Never panics.
    WrapSub,
    /// `*%` — int two's-complement wrapping multiply (NUM §3.2). Never panics.
    WrapMul,
}

#[derive(Clone, Copy, Debug)]
pub enum UnOp {
    Neg,
    Not,
    /// `~` — int bitwise NOT (NUM §3.2). A float operand is a Tier-2 panic.
    BitNot,
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
            BinOp::InstanceOf => "instanceof",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::WrapAdd => "+%",
            BinOp::WrapSub => "-%",
            BinOp::WrapMul => "*%",
        };
        write!(f, "{}", s)
    }
}

impl fmt::Display for UnOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnOp::Neg => write!(f, "-"),
            UnOp::Not => write!(f, "!"),
            UnOp::BitNot => write!(f, "~"),
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
            ExprKind::Int(n) => write!(f, "{}", n),
            ExprKind::Float(n) => write!(f, "{}", n),
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
                        CallArg::Named { name, value } => write!(f, " {}: {}", name, value)?,
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
            ExprKind::Map(entries) => {
                write!(f, "#{{")?;
                for (i, e) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{}: {}", e.key, e.value)?;
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
            ExprKind::Range {
                start,
                end,
                inclusive,
                step,
            } => {
                let op = if *inclusive { "..=" } else { ".." };
                write!(f, "{}{}{}", start, op, end)?;
                if let Some(k) = step {
                    write!(f, " step {}", k)?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(n: f64) -> Box<Expr> {
        Box::new(Expr {
            kind: ExprKind::Float(n),
            span: Span::new(0, 0),
        })
    }

    fn range_expr(start: f64, end: f64, inclusive: bool, step: Option<f64>) -> Expr {
        Expr {
            kind: ExprKind::Range {
                start: num(start),
                end: num(end),
                inclusive,
                step: step.map(num),
            },
            span: Span::new(0, 0),
        }
    }

    #[test]
    fn range_display_inclusive_and_step() {
        // inclusive + step
        assert_eq!(
            range_expr(1.0, 10.0, true, Some(2.0)).to_string(),
            "1..=10 step 2"
        );
        // exclusive, no step
        assert_eq!(range_expr(1.0, 5.0, false, None).to_string(), "1..5");
        // exclusive + step
        assert_eq!(
            range_expr(0.0, 10.0, false, Some(2.0)).to_string(),
            "0..10 step 2"
        );
        // inclusive, no step
        assert_eq!(range_expr(1.0, 5.0, true, None).to_string(), "1..=5");
    }

    #[test]
    fn pattern_range_display_inclusive_and_step() {
        let pat = Pattern::Range {
            start: num(1.0),
            end: num(10.0),
            inclusive: true,
            step: Some(num(2.0)),
        };
        assert_eq!(pat.to_string(), "1..=10 step 2");

        let pat = Pattern::Range {
            start: num(1.0),
            end: num(5.0),
            inclusive: false,
            step: None,
        };
        assert_eq!(pat.to_string(), "1..5");
    }

    // ----- TYPE Task 3: generics surface (runtime-erased) ------------------

    #[test]
    fn type_param_display_renders_bare_name() {
        assert_eq!(Type::Param("T".into()).to_string(), "T");
        assert_eq!(Type::Param("Elem".into()).to_string(), "Elem");
    }

    #[test]
    fn type_fnsig_display_renders_arrow_signature() {
        // fn(int) -> string
        let sig = Type::FnSig(vec![Type::Int], Box::new(Type::String));
        assert_eq!(sig.to_string(), "fn(int) -> string");
        // zero-arg: fn() -> bool
        let sig0 = Type::FnSig(vec![], Box::new(Type::Bool));
        assert_eq!(sig0.to_string(), "fn() -> bool");
        // multi-arg: fn(int, string) -> nil
        let sig2 = Type::FnSig(vec![Type::Int, Type::String], Box::new(Type::Nil));
        assert_eq!(sig2.to_string(), "fn(int, string) -> nil");
        // nested params: fn(fn(int) -> int, array<T>) -> T
        let inner = Type::FnSig(vec![Type::Int], Box::new(Type::Int));
        let nested = Type::FnSig(
            vec![inner, Type::Array(Box::new(Type::Param("T".into())))],
            Box::new(Type::Param("T".into())),
        );
        assert_eq!(nested.to_string(), "fn(fn(int) -> int, array<T>) -> T");
    }

    #[test]
    fn type_param_and_fnsig_round_trip_through_formatter() {
        // `render_type` (fmt.rs) delegates to Display, so Display IS the canonical
        // round-trip. Confirm idempotence of the rendered text.
        let t = Type::Param("T".into());
        assert_eq!(t.to_string(), "T");
        let sig = Type::FnSig(vec![Type::Param("A".into())], Box::new(Type::Param("B".into())));
        assert_eq!(sig.to_string(), "fn(A) -> B");
    }

    #[test]
    fn check_type_param_accepts_any_value_runtime_erased() {
        use crate::interp::check_type;
        use crate::value::Value;
        let t = Type::Param("T".into());
        // Runtime-erased: a `T`-annotated slot accepts EVERY value (accept-anything),
        // exactly like `any`. No runtime obligation.
        assert!(check_type(&Value::Int(5), &t));
        assert!(check_type(&Value::Str("hi".into()), &t));
        assert!(check_type(&Value::Bool(true), &t));
        assert!(check_type(&Value::Nil, &t));
    }
}
