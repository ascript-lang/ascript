//! The `CheckTy` lattice + three-valued assignability (SP10 §1).
//!
//! `CheckTy` mirrors `ast::Type` plus the lattice endpoints the surface language
//! does not name (`Any` gradual-dynamic, `Never` internal-bottom) and the internal
//! narrowing artifacts (`Literal`, `EnumVariant`). The whole-file invariant is the
//! three-valued [`Compat3`]: `assignable` returns `Yes`/`No`/`Unknown` and **only a
//! provable `No` ever produces a diagnostic** — everything uncertain is `Unknown`
//! (silent), which is what keeps the untyped corpus at zero false positives.

use crate::check::infer::table::Table;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;

/// A class declaration index into the [`Table`].
pub type ClassId = usize;
/// An enum declaration index into the [`Table`].
pub type EnumId = usize;

/// Width cap on a normalized union: a union with more members collapses to `Any`.
const UNION_WIDTH_CAP: usize = 8;
/// Depth cap on constructor recursion (`Array`/`Map`/`Tuple`/…): past this,
/// `from_type_node` yields `Any` and `assignable` yields `Unknown`.
const TYPE_DEPTH_CAP: usize = 8;

/// A flow-refinement literal value (an internal narrowing artifact). It widens
/// back to its base primitive (`Number`/`String`/`Bool`/`Nil`) for assignability
/// and display.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LitVal {
    Number,
    String,
    Bool,
    Nil,
}

/// The checker's internal type lattice over AScript's surface `ast::Type`.
///
/// `Never`, `Literal`, and `EnumVariant` are narrowing artifacts — never written
/// by a user — and widen back to a base type before any diagnostic text.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CheckTy {
    /// Gradual dynamic: the consistency wildcard (assignable to/from everything).
    Any,
    /// Internal bottom — the empty type; result of exhaustive narrowing.
    Never,
    /// The numeric supertype `int | float` (NUM §5). A `: number` annotation, and
    /// the result of an arithmetic op whose subtype is not provable, are `Number`.
    /// `Int`/`Float` are its concrete subtypes; `Number` itself is gradual between
    /// them (assignable to either is `Unknown`, never `No`).
    Number,
    /// The concrete `int` subtype (NUM §5). An integer literal synths `Int`.
    Int,
    /// The concrete `float` subtype (NUM §5). A float literal synths `Float`.
    Float,
    String,
    Bool,
    Nil,
    Bytes,
    Object,
    Regex,
    Error,
    /// Unparameterized callable (AScript has no fn-arity types today).
    Fn,
    Array(Box<CheckTy>),
    Map(Box<CheckTy>, Box<CheckTy>),
    Tuple(Vec<CheckTy>),
    Result(Box<CheckTy>),
    Future(Box<CheckTy>),
    /// Normalized: flattened, dedup'd, `nil`-canonicalized, sorted. `T?` == `Union[T, Nil]`.
    Union(Vec<CheckTy>),
    /// NOMINAL class, identified by declaration-site index; inheritance via the table.
    Class(ClassId),
    /// NOMINAL enum, accepts any of its variants.
    Enum(EnumId),
    /// INTERNAL refinement from `match` narrowing: a single variant of an enum.
    EnumVariant(EnumId, std::rc::Rc<str>),
    /// INTERNAL refinement from flow: a literal value.
    Literal(LitVal),
}

/// The three-valued result of [`CheckTy::assignable`]. **Only `No` diagnoses.**
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compat3 {
    Yes,
    No,
    Unknown,
}

impl CheckTy {
    /// Lower a type-annotation CST node (`NamedType`/`GenericType`/`OptionalType`/
    /// `UnionType`/`TupleType`) into a `CheckTy`. An unknown name → `Any` (the
    /// zero-false-positive default; we never invent a class). Depth-capped (§1.4).
    pub fn from_type_node(node: &ResolvedNode, table: &Table) -> CheckTy {
        Self::from_type_node_depth(node, table, 0)
    }

    fn from_type_node_depth(node: &ResolvedNode, table: &Table, depth: usize) -> CheckTy {
        use SyntaxKind::*;
        if depth > TYPE_DEPTH_CAP {
            return CheckTy::Any;
        }
        match node.kind() {
            NamedType => {
                let name = node.text().to_string();
                let name = name.trim();
                match name {
                    "number" => CheckTy::Number,
                    "int" => CheckTy::Int,
                    "float" => CheckTy::Float,
                    "string" => CheckTy::String,
                    "bool" => CheckTy::Bool,
                    "nil" => CheckTy::Nil,
                    "any" => CheckTy::Any,
                    "object" => CheckTy::Object,
                    "bytes" => CheckTy::Bytes,
                    "regex" => CheckTy::Regex,
                    "error" => CheckTy::Error,
                    "fn" => CheckTy::Fn,
                    other => {
                        if let Some(id) = table.class_id(other) {
                            CheckTy::Class(id)
                        } else if let Some(id) = table.enum_id(other) {
                            CheckTy::Enum(id)
                        } else {
                            CheckTy::Any // unknown name → gradual default
                        }
                    }
                }
            }
            GenericType => {
                let head = node
                    .children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .find(|t| !t.kind().is_trivia())
                    .map(|t| t.text().to_string());
                let args: Vec<CheckTy> = node
                    .children()
                    .find(|c| c.kind() == TypeArgs)
                    .map(|ta| {
                        ta.children()
                            .filter(|c| crate::check::rules::is_type_kind(c.kind()))
                            .map(|c| Self::from_type_node_depth(c, table, depth + 1))
                            .collect()
                    })
                    .unwrap_or_default();
                match head.as_deref() {
                    Some("array") if args.len() == 1 => {
                        CheckTy::Array(Box::new(args.into_iter().next().unwrap()))
                    }
                    Some("map") if args.len() == 2 => {
                        let mut it = args.into_iter();
                        CheckTy::Map(Box::new(it.next().unwrap()), Box::new(it.next().unwrap()))
                    }
                    Some("Result") if args.len() == 1 => {
                        CheckTy::Result(Box::new(args.into_iter().next().unwrap()))
                    }
                    Some("future") if args.len() == 1 => {
                        CheckTy::Future(Box::new(args.into_iter().next().unwrap()))
                    }
                    _ => CheckTy::Any, // any other generic head → gradual default
                }
            }
            OptionalType => {
                let inner = node
                    .children()
                    .find(|c| crate::check::rules::is_type_kind(c.kind()))
                    .map(|c| Self::from_type_node_depth(c, table, depth + 1))
                    .unwrap_or(CheckTy::Any);
                normalize(CheckTy::Union(vec![inner, CheckTy::Nil]))
            }
            UnionType => {
                let members: Vec<CheckTy> = node
                    .children()
                    .filter(|c| crate::check::rules::is_type_kind(c.kind()))
                    .map(|c| Self::from_type_node_depth(c, table, depth + 1))
                    .collect();
                normalize(CheckTy::Union(members))
            }
            TupleType => {
                let members: Vec<CheckTy> = node
                    .children()
                    .filter(|c| crate::check::rules::is_type_kind(c.kind()))
                    .map(|c| Self::from_type_node_depth(c, table, depth + 1))
                    .collect();
                CheckTy::Tuple(members)
            }
            _ => CheckTy::Any,
        }
    }

    /// Widen an internal narrowing artifact back to its base type (used before any
    /// diagnostic text and at most use sites). `Literal → primitive`,
    /// `EnumVariant(E,_) → Enum(E)`, `Never → Any`. Non-artifacts are unchanged.
    pub fn widen(&self) -> CheckTy {
        match self {
            CheckTy::Literal(LitVal::Number) => CheckTy::Number,
            CheckTy::Literal(LitVal::String) => CheckTy::String,
            CheckTy::Literal(LitVal::Bool) => CheckTy::Bool,
            CheckTy::Literal(LitVal::Nil) => CheckTy::Nil,
            CheckTy::EnumVariant(e, _) => CheckTy::Enum(*e),
            CheckTy::Never => CheckTy::Any,
            other => other.clone(),
        }
    }

    /// Does this type contain `Nil` as a member (i.e. is it a `T?`)?
    pub fn includes_nil(&self) -> bool {
        match self {
            CheckTy::Nil | CheckTy::Any => true,
            CheckTy::Literal(LitVal::Nil) => true,
            CheckTy::Union(ms) => ms.iter().any(|m| m.includes_nil()),
            _ => false,
        }
    }

    /// Is this type PROVABLY a union that includes `Nil` AND is not `Any`? — i.e. a
    /// genuine `T?` whose non-nil deref the `possibly-nil` lint can flag. `Any` and a
    /// bare `Nil` are excluded (the former is gradual, the latter is not a "T?").
    pub fn is_provable_optional(&self) -> bool {
        matches!(self, CheckTy::Union(ms) if ms.iter().any(|m| matches!(m, CheckTy::Nil)))
    }

    /// Remove `Nil` from this type (narrowing `T?` to `T`). A `Union[T, Nil]`
    /// becomes `T`; a bare `Nil` becomes `Never`; everything else is unchanged.
    pub fn without_nil(&self) -> CheckTy {
        match self {
            CheckTy::Nil | CheckTy::Literal(LitVal::Nil) => CheckTy::Never,
            CheckTy::Union(ms) => {
                let kept: Vec<CheckTy> = ms
                    .iter()
                    .filter(|m| !matches!(m, CheckTy::Nil))
                    .cloned()
                    .collect();
                normalize(CheckTy::Union(kept))
            }
            other => other.clone(),
        }
    }

    /// Keep only `Nil` from this type (the else-branch of a `!= nil` guard). A
    /// `Union` containing `Nil` becomes `Nil`; a non-nil type becomes `Never`.
    pub fn only_nil(&self) -> CheckTy {
        if self.includes_nil() {
            CheckTy::Nil
        } else {
            CheckTy::Never
        }
    }

    /// Three-valued assignability: "may a value of type `self` flow into a slot
    /// expecting `dst` without a provable contract violation?" Checked in the §1.2
    /// order; first matching arm wins. `Unknown ⇒ silent` is the discipline.
    pub fn assignable(&self, dst: &CheckTy, table: &Table) -> Compat3 {
        self.assignable_depth(dst, table, 0)
    }

    fn assignable_depth(&self, dst: &CheckTy, table: &Table, depth: usize) -> Compat3 {
        use CheckTy::*;
        // Depth cap (§1.4): past the cap, never diagnose.
        if depth > TYPE_DEPTH_CAP {
            return Compat3::Unknown;
        }

        // Rule 1 — gradual escape.
        if matches!(self, Any) || matches!(dst, Any) {
            return Compat3::Yes;
        }

        // Rule 2 — Never.
        if matches!(self, Never) {
            return Compat3::Yes; // bottom assignable to anything
        }
        if matches!(dst, Never) {
            return Compat3::Unknown; // never used to diagnose
        }

        // Rule 4 — literal refinement: widen the source literal and recurse.
        if let Literal(_) = self {
            return self.widen().assignable_depth(dst, table, depth);
        }
        // A literal destination widens too (a narrowed slot is rare but harmless).
        if let Literal(_) = dst {
            return self.assignable_depth(&dst.widen(), table, depth);
        }

        // Rule 5 — nil and optionals.
        if matches!(self, Nil) {
            return match dst {
                Nil => Compat3::Yes,
                Union(ms) if ms.iter().any(|m| matches!(m, Nil)) => Compat3::Yes,
                _ => Compat3::No,
            };
        }

        // Rule 9 (union) — handle BEFORE the nominal/primitive arms so a union on
        // either side decomposes member-wise.
        if let Union(dmembers) = dst {
            // src assignable to a union: Yes if some member is Yes; No only if every
            // member is No; else Unknown.
            let mut any_yes = false;
            let mut all_no = true;
            for m in dmembers {
                match self.assignable_depth(m, table, depth + 1) {
                    Compat3::Yes => any_yes = true,
                    Compat3::Unknown => all_no = false,
                    Compat3::No => {}
                }
            }
            return if any_yes {
                Compat3::Yes
            } else if all_no && !dmembers.is_empty() {
                Compat3::No
            } else {
                Compat3::Unknown
            };
        }
        if let Union(smembers) = self {
            // a union src assignable to dst: Yes if EVERY member is Yes; No if ANY
            // member is No; else Unknown.
            let mut all_yes = true;
            let mut any_no = false;
            for m in smembers {
                match m.assignable_depth(dst, table, depth + 1) {
                    Compat3::Yes => {}
                    Compat3::No => any_no = true,
                    Compat3::Unknown => all_yes = false,
                }
            }
            return if any_no {
                Compat3::No
            } else if all_yes && !smembers.is_empty() {
                Compat3::Yes
            } else {
                Compat3::Unknown
            };
        }

        // Rule 3a — numeric tower (NUM §5). `Number` is the supertype `int | float`;
        // `Int`/`Float` are its concrete subtypes. Subtype → supertype is `Yes`;
        // supertype → a specific subtype is `Unknown` (gradual — a `number` may be
        // either, so demanding a specific subtype is never *provably* wrong); the two
        // concrete subtypes are mutually `No` (both provably-concrete-distinct).
        {
            let is_numeric = |t: &CheckTy| matches!(t, Number | Int | Float);
            if is_numeric(self) && is_numeric(dst) {
                return match (self, dst) {
                    // reflexive
                    (Int, Int) | (Float, Float) | (Number, Number) => Compat3::Yes,
                    // concrete subtype → supertype
                    (Int, Number) | (Float, Number) => Compat3::Yes,
                    // supertype → concrete subtype: not provable either way
                    (Number, Int) | (Number, Float) => Compat3::Unknown,
                    // distinct concrete subtypes
                    (Int, Float) | (Float, Int) => Compat3::No,
                    _ => Compat3::Unknown,
                };
            }
            // A numeric vs a concrete NON-numeric primitive is provably `No`.
            let concrete_nonnum =
                |t: &CheckTy| matches!(t, String | Bool | Bytes | Object | Regex | Error | Fn);
            if (is_numeric(self) && concrete_nonnum(dst))
                || (concrete_nonnum(self) && is_numeric(dst))
            {
                return Compat3::No;
            }
        }

        // Rule 3 — reflexive / primitive. The concrete primitives.
        let prim = |t: &CheckTy| {
            matches!(
                t,
                Number | Int | Float | String | Bool | Bytes | Object | Regex | Error | Fn
            )
        };
        if prim(self) && prim(dst) {
            // Special-case: a Class is an Object at runtime (rule 6 handles Class→Object);
            // here both are bare primitives.
            return if self == dst {
                Compat3::Yes
            } else {
                Compat3::No
            };
        }

        // Rule 6 — nominal classes.
        if let Class(s) = self {
            return match dst {
                Class(d) => {
                    if table.is_subclass(*s, *d) {
                        Compat3::Yes
                    } else {
                        Compat3::No
                    }
                }
                Object => Compat3::Yes, // an instance IS an object at runtime
                // a class vs a concrete non-object primitive → provably no
                _ if prim(dst) => Compat3::No,
                _ => Compat3::Unknown,
            };
        }
        if matches!(dst, Class(_)) {
            // Object → Class is not provable (rule 6) → silent; a concrete NON-object
            // primitive → Class is provably wrong.
            return if matches!(self, Object) {
                Compat3::Unknown
            } else if prim(self) {
                Compat3::No
            } else {
                Compat3::Unknown
            };
        }

        // Rule 7 — enums.
        match (self, dst) {
            (EnumVariant(e1, v1), EnumVariant(e2, v2)) => {
                return if e1 == e2 && v1 == v2 {
                    Compat3::Yes
                } else {
                    Compat3::No
                };
            }
            (EnumVariant(e1, _), Enum(e2)) | (Enum(e1), Enum(e2)) => {
                return if e1 == e2 { Compat3::Yes } else { Compat3::No };
            }
            (Enum(e1), EnumVariant(e2, _)) => {
                // a whole enum is NOT provably a single variant → unknown.
                return if e1 == e2 {
                    Compat3::Unknown
                } else {
                    Compat3::No
                };
            }
            (Enum(_) | EnumVariant(_, _), d) if prim(d) => return Compat3::No,
            (s, Enum(_) | EnumVariant(_, _)) if prim(s) => return Compat3::No,
            _ => {}
        }

        // Rule 8 — constructors (covariant, depth-limited).
        match (self, dst) {
            (Array(s), Array(d)) => return s.assignable_depth(d, table, depth + 1),
            (Future(s), Future(d)) => return s.assignable_depth(d, table, depth + 1),
            (Result(s), Result(d)) => return s.assignable_depth(d, table, depth + 1),
            (Map(sk, sv), Map(dk, dv)) => {
                let k = sk.assignable_depth(dk, table, depth + 1);
                let v = sv.assignable_depth(dv, table, depth + 1);
                return meet_compat(k, v);
            }
            (Tuple(s), Tuple(d)) => {
                if s.len() != d.len() {
                    return Compat3::No;
                }
                let mut acc = Compat3::Yes;
                for (a, b) in s.iter().zip(d.iter()) {
                    acc = meet_compat(acc, a.assignable_depth(b, table, depth + 1));
                }
                return acc;
            }
            // a constructor vs a different constructor / primitive: provably no when
            // both sides are concrete container/primitive kinds.
            (Array(_) | Map(_, _) | Tuple(_) | Future(_) | Result(_), d)
                if prim(d) || is_concrete_ctor(d) =>
            {
                return Compat3::No;
            }
            (s, Array(_) | Map(_, _) | Tuple(_) | Future(_) | Result(_))
                if prim(s) || is_concrete_ctor(s) =>
            {
                return Compat3::No;
            }
            _ => {}
        }

        // Rule 11 — default: uncertain → silent.
        Compat3::Unknown
    }

    /// Join (least upper bound) for inference (§1.3).
    pub fn join(&self, other: &CheckTy, table: &Table) -> CheckTy {
        use CheckTy::*;
        if matches!(self, Any) || matches!(other, Any) {
            return Any;
        }
        if matches!(self, Never) {
            return other.widen();
        }
        if matches!(other, Never) {
            return self.widen();
        }
        if self == other {
            return self.widen();
        }
        // Literals widen to base primitives before joining.
        if matches!(self, Literal(_)) || matches!(other, Literal(_)) {
            let a = self.widen();
            let b = other.widen();
            if a == b {
                return a;
            }
            return normalize(Union(vec![a, b]));
        }
        // EnumVariant widens to its enum.
        if matches!(self, EnumVariant(_, _)) || matches!(other, EnumVariant(_, _)) {
            let a = self.widen();
            let b = other.widen();
            return a.join(&b, table);
        }
        // Nearest common ancestor for two classes.
        if let (Class(a), Class(b)) = (self, other) {
            if let Some(anc) = table.nearest_common_ancestor(*a, *b) {
                return Class(anc);
            }
            return Object; // both are objects at runtime
        }
        normalize(Union(vec![self.clone(), other.clone()]))
    }
}

impl CheckTy {
    /// Render this type for a diagnostic message / LSP hover. Internal narrowing
    /// artifacts (`Literal`/`EnumVariant`/`Never`) are widened first. A class/enum is
    /// rendered by its declared name via the table.
    pub fn display(&self, table: &Table) -> String {
        use CheckTy::*;
        match self.widen() {
            Any => "any".into(),
            Never => "never".into(),
            Number => "number".into(),
            Int => "int".into(),
            Float => "float".into(),
            String => "string".into(),
            Bool => "bool".into(),
            Nil => "nil".into(),
            Bytes => "bytes".into(),
            Object => "object".into(),
            Regex => "regex".into(),
            Error => "error".into(),
            Fn => "fn".into(),
            Array(inner) => format!("array<{}>", inner.display(table)),
            Map(k, v) => format!("map<{}, {}>", k.display(table), v.display(table)),
            Tuple(ms) => {
                let inner: Vec<_> = ms.iter().map(|m| m.display(table)).collect();
                format!("[{}]", inner.join(", "))
            }
            Result(inner) => format!("Result<{}>", inner.display(table)),
            Future(inner) => format!("future<{}>", inner.display(table)),
            Union(ms) => {
                // canonical T? rendering when exactly [T, Nil].
                if ms.len() == 2 && ms.iter().any(|m| matches!(m, Nil)) {
                    let base = ms.iter().find(|m| !matches!(m, Nil)).unwrap();
                    return format!("{}?", base.display(table));
                }
                let inner: Vec<_> = ms.iter().map(|m| m.display(table)).collect();
                inner.join(" | ")
            }
            Class(id) => table
                .class(id)
                .map(|c| c.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "object".into()),
            Enum(id) => table
                .enum_info(id)
                .map(|e| e.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "enum".into()),
            // widened away above
            EnumVariant(_, _) | Literal(_) => unreachable!("widened above"),
        }
    }
}

/// True for a concrete container constructor (used in the "concrete vs concrete"
/// provable-`No` guard so two distinct containers reject).
fn is_concrete_ctor(t: &CheckTy) -> bool {
    matches!(
        t,
        CheckTy::Array(_)
            | CheckTy::Map(_, _)
            | CheckTy::Tuple(_)
            | CheckTy::Future(_)
            | CheckTy::Result(_)
    )
}

/// Combine two component compatibilities (covariant constructor): `No` dominates,
/// then `Unknown`, else `Yes`.
fn meet_compat(a: Compat3, b: Compat3) -> Compat3 {
    match (a, b) {
        (Compat3::No, _) | (_, Compat3::No) => Compat3::No,
        (Compat3::Unknown, _) | (_, Compat3::Unknown) => Compat3::Unknown,
        _ => Compat3::Yes,
    }
}

/// Stable discriminant order for sorting union members (so `CheckTy` has a
/// canonical form for dedup / `Eq` / deterministic diagnostics).
fn discriminant_order(t: &CheckTy) -> u32 {
    use CheckTy::*;
    match t {
        Any => 0,
        Never => 1,
        Number => 2,
        Int => 3,
        Float => 4,
        String => 5,
        Bool => 6,
        Nil => 7,
        Bytes => 8,
        Object => 9,
        Regex => 10,
        Error => 11,
        Fn => 12,
        Array(_) => 13,
        Map(_, _) => 14,
        Tuple(_) => 15,
        Result(_) => 16,
        Future(_) => 17,
        Union(_) => 18,
        Class(_) => 19,
        Enum(_) => 20,
        EnumVariant(_, _) => 21,
        Literal(_) => 22,
    }
}

/// A secondary key (within the same discriminant) for a fully stable sort.
fn secondary_key(t: &CheckTy) -> usize {
    use CheckTy::*;
    match t {
        Class(id) | Enum(id) => *id,
        EnumVariant(id, _) => *id,
        _ => 0,
    }
}

/// Normalize a type: flatten nested unions, dedup, `nil`-canonicalize, sort, and
/// apply the width cap (§1.4). A non-union type is returned as-is (its components
/// are not recursively re-normalized — callers build them already-normal).
pub fn normalize(t: CheckTy) -> CheckTy {
    let CheckTy::Union(members) = t else {
        return t;
    };
    let mut flat: Vec<CheckTy> = Vec::new();
    let mut stack: Vec<CheckTy> = members;
    while let Some(m) = stack.pop() {
        match m {
            CheckTy::Union(inner) => stack.extend(inner),
            // `Any` swallows a union (gradual top).
            CheckTy::Any => return CheckTy::Any,
            CheckTy::Never => {} // bottom drops out of a union
            CheckTy::Literal(_) | CheckTy::EnumVariant(_, _) => flat.push(m.widen()),
            other => flat.push(other),
        }
    }
    // Dedup (preserve a single instance of each distinct member).
    flat.sort_by(|a, b| {
        discriminant_order(a)
            .cmp(&discriminant_order(b))
            .then(secondary_key(a).cmp(&secondary_key(b)))
    });
    flat.dedup();
    if flat.is_empty() {
        return CheckTy::Never;
    }
    if flat.len() == 1 {
        return flat.into_iter().next().unwrap();
    }
    if flat.len() > UNION_WIDTH_CAP {
        return CheckTy::Any;
    }
    CheckTy::Union(flat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::infer::table::Table;

    fn t() -> Table {
        Table::default()
    }

    fn assign(a: CheckTy, b: CheckTy) -> Compat3 {
        a.assignable(&b, &t())
    }

    fn num() -> CheckTy {
        CheckTy::Number
    }
    fn str_() -> CheckTy {
        CheckTy::String
    }
    fn opt(inner: CheckTy) -> CheckTy {
        normalize(CheckTy::Union(vec![inner, CheckTy::Nil]))
    }
    fn uni(ms: Vec<CheckTy>) -> CheckTy {
        normalize(CheckTy::Union(ms))
    }

    #[test]
    fn gradual_escape() {
        assert_eq!(assign(CheckTy::Any, num()), Compat3::Yes);
        assert_eq!(assign(num(), CheckTy::Any), Compat3::Yes);
    }

    #[test]
    fn primitives() {
        assert_eq!(assign(num(), num()), Compat3::Yes);
        assert_eq!(assign(num(), str_()), Compat3::No);
        assert_eq!(assign(CheckTy::Bool, num()), Compat3::No);
    }

    #[test]
    fn nil_and_optional() {
        assert_eq!(assign(CheckTy::Nil, opt(num())), Compat3::Yes);
        assert_eq!(assign(CheckTy::Nil, num()), Compat3::No);
        assert_eq!(assign(CheckTy::Nil, CheckTy::Nil), Compat3::Yes);
    }

    #[test]
    fn constructors() {
        assert_eq!(
            assign(
                CheckTy::Array(Box::new(num())),
                CheckTy::Array(Box::new(str_()))
            ),
            Compat3::No
        );
        assert_eq!(
            assign(
                CheckTy::Array(Box::new(num())),
                CheckTy::Array(Box::new(CheckTy::Any))
            ),
            Compat3::Yes
        );
        // distinct container kinds → No
        assert_eq!(
            assign(
                CheckTy::Array(Box::new(num())),
                CheckTy::Future(Box::new(num()))
            ),
            Compat3::No
        );
        // tuple length mismatch → No
        assert_eq!(
            assign(
                CheckTy::Tuple(vec![num(), num()]),
                CheckTy::Tuple(vec![num()])
            ),
            Compat3::No
        );
    }

    #[test]
    fn unions() {
        assert_eq!(assign(num(), uni(vec![num(), str_()])), Compat3::Yes);
        assert_eq!(assign(CheckTy::Bool, uni(vec![num(), str_()])), Compat3::No);
        // a union source: every member must be assignable
        assert_eq!(assign(uni(vec![num(), str_()]), CheckTy::Any), Compat3::Yes);
        assert_eq!(assign(uni(vec![num(), str_()]), num()), Compat3::No);
    }

    #[test]
    fn default_uncertain() {
        // an Object flowing to a Class is not provable → Unknown (we don't have a
        // real class id, but Object→Class with a fabricated id exercises the arm)
        assert_eq!(assign(CheckTy::Object, CheckTy::Class(0)), Compat3::Unknown);
    }

    #[test]
    fn join_basics() {
        let tbl = t();
        assert_eq!(num().join(&CheckTy::Nil, &tbl), opt(num()));
        assert_eq!(num().join(&CheckTy::Any, &tbl), CheckTy::Any);
        assert_eq!(num().join(&num(), &tbl), num());
        // literals widen before joining
        assert_eq!(
            CheckTy::Literal(LitVal::Number).join(&CheckTy::Literal(LitVal::Number), &tbl),
            CheckTy::Number
        );
    }

    #[test]
    fn normalize_canonicalizes() {
        // nested + dup + unsorted union canonicalizes
        let a = CheckTy::Union(vec![
            CheckTy::Union(vec![str_(), num()]),
            num(),
            CheckTy::Nil,
        ]);
        let b = CheckTy::Union(vec![CheckTy::Nil, num(), str_()]);
        assert_eq!(normalize(a), normalize(b));
        // a single-member union collapses to the member
        assert_eq!(normalize(CheckTy::Union(vec![num()])), num());
        // Any swallows
        assert_eq!(
            normalize(CheckTy::Union(vec![num(), CheckTy::Any])),
            CheckTy::Any
        );
        // Never drops out
        assert_eq!(
            normalize(CheckTy::Union(vec![num(), CheckTy::Never])),
            num()
        );
    }

    #[test]
    fn over_wide_union_collapses_to_any() {
        let members = vec![
            CheckTy::Number,
            CheckTy::String,
            CheckTy::Bool,
            CheckTy::Nil,
            CheckTy::Bytes,
            CheckTy::Object,
            CheckTy::Regex,
            CheckTy::Error,
            CheckTy::Fn,
        ];
        assert_eq!(members.len(), 9);
        assert_eq!(normalize(CheckTy::Union(members)), CheckTy::Any);
    }

    #[test]
    fn deep_constructor_collapses_to_unknown() {
        // build a 10-deep nested array — assignable past the cap returns Unknown.
        let mut a = CheckTy::Number;
        let mut b = CheckTy::String;
        for _ in 0..10 {
            a = CheckTy::Array(Box::new(a));
            b = CheckTy::Array(Box::new(b));
        }
        assert_eq!(assign(a, b), Compat3::Unknown);
    }

    #[test]
    fn nil_helpers() {
        assert!(opt(num()).is_provable_optional());
        assert!(!num().is_provable_optional());
        assert!(!CheckTy::Any.is_provable_optional());
        assert_eq!(opt(num()).without_nil(), num());
        assert_eq!(opt(num()).only_nil(), CheckTy::Nil);
        assert!(opt(num()).includes_nil());
        assert!(!num().includes_nil());
    }
}
