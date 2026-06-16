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
/// An interface declaration index into the [`Table`] (IFACE §6 — reserved name).
pub type InterfaceId = usize;
/// A unique generic type-variable id, allocated within a single instantiation
/// context by the unifier (TYPE §4.2). Two `Var`s with the same id are the same
/// variable; distinct ids are distinct variables.
pub type VarId = u32;

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
    /// A generic type VARIABLE (TYPE §4.2). `id` identifies it within an
    /// instantiation context; the optional boxed `CheckTy` is its interface
    /// `bound` (`None` ⇒ unbounded ⇒ gradual top). **The cardinal Gate-5 rule:**
    /// an unsolved/unbounded `Var` is `Unknown`-yielding (never `No`) — it is the
    /// gradual escape (`Any`) generalized. After unification a solved `Var` is
    /// substituted by its solution everywhere before any diagnosing `assignable`.
    Var(VarId, Option<Box<CheckTy>>),
    /// A PARAMETERIZED function type (`fn(A) -> B`): the param types + return type
    /// (TYPE §5.1). A strict extension of the bare [`CheckTy::Fn`] — a bare `fn`
    /// stays `Fn` and is `Unknown`-compatible with any `FnSig` (gradual).
    FnSig(Vec<CheckTy>, Box<CheckTy>),
    /// A NOMINAL-by-id but PARAMETERIZED class instantiation (`Box<int>`): the head
    /// [`ClassId`] plus its solved type arguments (TYPE §5.1). [`CheckTy::Class`]
    /// remains the zero-arg form. INVARIANT in `assignable` (§4.6).
    ClassApp(ClassId, Vec<CheckTy>),
    /// A NOMINAL-by-id but PARAMETERIZED enum instantiation (`Option<int>`): the head
    /// [`EnumId`] plus its solved type arguments (TYPE §5.1). INVARIANT (§4.6).
    EnumApp(EnumId, Vec<CheckTy>),
    /// A structural INTERFACE (IFACE §6), identified by table id. Conformance is
    /// structural (a value conforms if it has the required methods with assignable
    /// signatures) — see `Table::conforms`. Carries no args here in v1.
    Interface(InterfaceId),
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
                        } else if let Some(id) = table.interface_id(other) {
                            // IFACE: an interface-typed slot (`p: Reader`) → `Interface`.
                            CheckTy::Interface(id)
                        } else {
                            CheckTy::Any // unknown name → gradual default
                        }
                    }
                }
            }
            // TYPE §6: a reference to an in-scope generic type parameter (`T`), tagged
            // distinctly by the CST parser. Lower to a `Var` whose id is a stable hash
            // of the param NAME — so the same param name in a signature lowers to the
            // same template `Var` (the unifier freshens these to per-call fresh vars,
            // §4.3). Unbounded ⇒ gradual top (`None`).
            ParamType => {
                let name = node.text().to_string();
                CheckTy::Var(param_template_id(name.trim()), None)
            }
            // TYPE §6: a parameterized `fn(A) -> B`. Children (in order): the param
            // type nodes, then the return type node (the LAST type-kind child).
            FnType => {
                let types: Vec<CheckTy> = node
                    .children()
                    .filter(|c| crate::check::rules::is_type_kind(c.kind()))
                    .map(|c| Self::from_type_node_depth(c, table, depth + 1))
                    .collect();
                if types.is_empty() {
                    return CheckTy::Fn; // malformed — degrade to the bare fn type
                }
                let mut it = types;
                let ret = it.pop().unwrap();
                CheckTy::FnSig(it, Box::new(ret))
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
                    // TYPE §6: a user CLASS/ENUM/INTERFACE head with type args → a
                    // parameterized application (`Box<int>`/`Option<T>`/`Container<T>`).
                    // Today such heads fell to `Any` — a strict, gradual-preserving
                    // upgrade (the args are still gradual leaves until inference solves
                    // them). An unknown head stays `Any`.
                    Some(other) => {
                        if let Some(id) = table.class_id(other) {
                            CheckTy::ClassApp(id, args)
                        } else if let Some(id) = table.enum_id(other) {
                            CheckTy::EnumApp(id, args)
                        } else if let Some(id) = table.interface_id(other) {
                            // A parameterized interface keeps its id (v1 conformance is
                            // by id); the args are dropped (no use-site interface args
                            // in v1, §5.1).
                            CheckTy::Interface(id)
                        } else {
                            CheckTy::Any
                        }
                    }
                    None => CheckTy::Any,
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
            // A leftover (unsubstituted) `Var` widens to `Any` — the gradual leaf
            // (TYPE §4.2: an unsolved type variable behaves like `Any`).
            CheckTy::Var(_, _) => CheckTy::Any,
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

        // Rule 1.5 — type VARIABLE (TYPE §4.2). The cardinal Gate-5 invariant: an
        // unsolved/unbounded `Var` is the gradual top — `Unknown`, NEVER `No`. A
        // BOUNDED `Var` as the DESTINATION checks the source against its interface
        // bound via `conforms` (so a bounded `T` only accepts a conforming value, but
        // `No` only on a provable concrete failure). Every other `Var` interaction is
        // `Unknown`. (A solved `Var` is substituted away before any diagnosing
        // `assignable` — §4.2 — so a `Var` reaching here is genuinely unsolved.)
        if let Var(_, bound) = dst {
            return match bound {
                Some(b) => match b.as_ref() {
                    Interface(iid) => table.conforms(self, *iid),
                    // A non-interface bound (shouldn't occur — bounds are
                    // interface-only) → gradual.
                    _ => Compat3::Unknown,
                },
                None => Compat3::Unknown,
            };
        }
        if matches!(self, Var(_, _)) {
            // A `Var` SOURCE flowing into any slot is never provably wrong (gradual).
            return Compat3::Unknown;
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

        // Rule 5b — INTERFACE destination (IFACE §6 / TYPE §4.5): structural
        // conformance. Placed BEFORE the nominal-class rule 6 so a `Class`/`ClassApp`
        // source into an interface slot routes through `conforms` (not rule 6's
        // class-vs-non-class fall-through). `No` only on a fully-concrete provable
        // failure; a non-class / partial source stays `Unknown` (the gradual gate).
        if let Interface(iid) = dst {
            return table.conforms(self, *iid);
        }
        // A source interface flowing into a non-interface slot is not provably wrong
        // (a value typed by an interface could be anything that conforms) → gradual.
        if matches!(self, Interface(_)) {
            return Compat3::Unknown;
        }

        // Rule 5c — parameterized user generics: INVARIANT (TYPE §4.6 — a NEW arm,
        // NOT the covariant built-in rule 8). Same head + arity → each type-arg pair
        // checked in BOTH directions; `No` only when EVERY pair is concrete-distinct
        // both ways. Different head / arity (both concrete) → `No`. Any pair involving
        // a `Var`/`Any` → `Unknown` (the Var-bias). Placed before rule 6 so a
        // parameterized head never falls into the nominal-class arm.
        match (self, dst) {
            (ClassApp(c, sargs), ClassApp(d, dargs)) => {
                return if c != d || sargs.len() != dargs.len() {
                    Compat3::No
                } else {
                    invariant_args(sargs, dargs, table, depth)
                };
            }
            (EnumApp(c, sargs), EnumApp(d, dargs)) => {
                return if c != d || sargs.len() != dargs.len() {
                    Compat3::No
                } else {
                    invariant_args(sargs, dargs, table, depth)
                };
            }
            // ELIDE §6.6: `ClassApp → Object` is `Unknown`, not `Yes`.
            // The runtime `check_type` for `object` only accepts `ValueKind::Object(_)`
            // and REJECTS `ValueKind::Instance(_)` (see `src/interp.rs` `check_type`
            // `Type::Object` arm). Returning `Yes` here would tell ELIDE it is safe to
            // skip the runtime contract check, which is unsound. `Unknown` keeps the
            // checker silent (zero new corpus diagnostics) without being elidable.
            (ClassApp(_, _), Object) => return Compat3::Unknown,
            // A raw class ref vs a parameterized one of the SAME (or related) head is
            // not provably wrong → gradual; of an unrelated head → provably wrong.
            (ClassApp(c, _), Class(d)) | (Class(c), ClassApp(d, _)) => {
                return if c == d || table.is_subclass(*c, *d) || table.is_subclass(*d, *c) {
                    Compat3::Unknown
                } else {
                    Compat3::No
                };
            }
            (EnumApp(c, _), Enum(d)) | (Enum(c), EnumApp(d, _)) => {
                return if c == d {
                    Compat3::Unknown
                } else {
                    Compat3::No
                };
            }
            // A parameterized head vs a concrete primitive/container → provably wrong.
            (ClassApp(_, _) | EnumApp(_, _), d) if prim(d) || is_concrete_ctor(d) => {
                return Compat3::No;
            }
            (s, ClassApp(_, _) | EnumApp(_, _)) if prim(s) || is_concrete_ctor(s) => {
                return Compat3::No;
            }
            _ => {}
        }

        // Rule 5d — parameterized FUNCTION types (TYPE §5.2). `FnSig` vs `FnSig`:
        // params CONTRAVARIANT, return COVARIANT — a `No` needs ALL components
        // provable, and the corpus uses the bare `fn` escape, so this almost always
        // lands on `Unknown`. `FnSig` vs the bare `Fn` (either direction) → `Unknown`.
        match (self, dst) {
            (FnSig(sps, sret), FnSig(dps, dret)) => {
                if sps.len() != dps.len() {
                    return Compat3::No; // a provable arity clash
                }
                let mut acc = Compat3::Yes;
                for (sp, dp) in sps.iter().zip(dps.iter()) {
                    // params: contravariant → dst-param assignable to src-param.
                    acc = meet_compat(acc, dp.assignable_depth(sp, table, depth + 1));
                }
                acc = meet_compat(acc, sret.assignable_depth(dret, table, depth + 1));
                return acc;
            }
            (FnSig(_, _), Fn) | (Fn, FnSig(_, _)) => return Compat3::Unknown,
            (FnSig(_, _), d) if (prim(d) && !matches!(d, Fn)) || is_concrete_ctor(d) => {
                return Compat3::No;
            }
            (s, FnSig(_, _)) if (prim(s) && !matches!(s, Fn)) || is_concrete_ctor(s) => {
                return Compat3::No;
            }
            _ => {}
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
                // ELIDE §6.6: `Class → Object` is `Unknown`, not `Yes`.
                // The runtime `check_type` for `object` only accepts
                // `ValueKind::Object(_)` and REJECTS `ValueKind::Instance(_)`
                // (see `src/interp.rs` `check_type` `Type::Object` arm, ~line 8545).
                // A `Yes` here would let ELIDE skip the runtime contract check,
                // which is unsound. `Unknown` is gradual-silent (zero new diagnostics)
                // and is never elidable.
                Object => Compat3::Unknown,
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
            // TYPE §6: parameterized heads render `Head<arg, …>`.
            ClassApp(id, args) => {
                let head = table
                    .class(id)
                    .map(|c| c.name.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| "object".into());
                render_app(&head, &args, table)
            }
            EnumApp(id, args) => {
                let head = table
                    .enum_info(id)
                    .map(|e| e.name.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| "enum".into());
                render_app(&head, &args, table)
            }
            Interface(id) => table
                .interface_info(id)
                .map(|i| i.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "interface".into()),
            // A parameterized fn type: `fn(p, …) -> ret`.
            FnSig(params, ret) => {
                let ps: Vec<_> = params.iter().map(|p| p.display(table)).collect();
                format!("fn({}) -> {}", ps.join(", "), ret.display(table))
            }
            // widened away above (`Var` → `Any`, narrowing artifacts → base)
            Var(_, _) | EnumVariant(_, _) | Literal(_) => unreachable!("widened above"),
        }
    }
}

/// Render a parameterized application `Head<arg, …>` for `display`. A zero-arg
/// application (a raw class/enum ref) renders as the bare head.
fn render_app(head: &str, args: &[CheckTy], table: &Table) -> String {
    if args.is_empty() {
        return head.to_string();
    }
    let inner: Vec<_> = args.iter().map(|a| a.display(table)).collect();
    format!("{head}<{}>", inner.join(", "))
}

/// The high half of the `VarId` space, reserved for "template" vars — the
/// declaration-context `Var`s `from_type_node` produces for a type-param reference
/// (hashed from the param name). The unifier allocates "fresh" vars from the LOW
/// half (a monotonic counter) and freshening rewrites every template var to a fresh
/// one, so the two spaces never collide. A `Var` id `>= TEMPLATE_VAR_BASE` is a
/// template; below it is a fresh (solvable) var.
pub const TEMPLATE_VAR_BASE: VarId = 0x8000_0000;

/// A STABLE template-`Var` id for a type-parameter NAME (TYPE §4.2). FNV-1a over the
/// name, folded into the high (template) half of the id space. The same name always
/// maps to the same id (so a signature's repeated `T` is one variable).
///
/// A hash COLLISION across two DISTINCT param names in one signature (`<T, U>`) would
/// fuse them into one variable. That is NOT necessarily gradual: like the same-`T`
/// over-constraint (see the unifier's numeric-join rescue), two non-numeric concrete
/// args could then surface a spurious mismatch. It is, however, practically
/// unreachable — a 31-bit FNV-1a collision between two short type-parameter names does
/// not occur in real code — so it is left as a documented, accepted residual rather
/// than paid for with a per-decl interning table. (The numeric case is already
/// rescued by the unifier; only a non-numeric collision could mislead.)
pub fn param_template_id(name: &str) -> VarId {
    let mut h: u32 = 0x811c_9dc5;
    for b in name.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    // Keep it in the template half (set the top bit), but never 0 within that half.
    TEMPLATE_VAR_BASE | (h & 0x7fff_ffff)
}

/// True if `id` is a TEMPLATE var (a declaration-context type-param reference), as
/// opposed to a fresh, solvable unification var.
pub fn is_template_var(id: VarId) -> bool {
    id >= TEMPLATE_VAR_BASE
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

/// INVARIANT type-argument compatibility (TYPE §4.6) for the NEW user generic heads
/// (`ClassApp`/`EnumApp`). Each pair `(s, d)` is checked in BOTH directions:
/// - either side an unsolved/unbounded `Var` (or `Any`) → that pair is `Unknown`
///   (the Var-bias: a pair with a variable NEVER yields `No`);
/// - both directions `Yes` (mutually assignable, i.e. equal-up-to-gradual) → `Yes`;
/// - both directions `No` (concrete-distinct both ways) → the WHOLE thing is `No`;
/// - anything else (e.g. `Dog → Animal` Yes but `Animal → Dog` No) → `Unknown`
///   (the documented v1 invariance limitation: silent, not blocking).
fn invariant_args(sargs: &[CheckTy], dargs: &[CheckTy], table: &Table, depth: usize) -> Compat3 {
    let mut acc = Compat3::Yes;
    for (s, d) in sargs.iter().zip(dargs.iter()) {
        // Var-bias / gradual: any side a var or `Any` → this pair is `Unknown`.
        let involves_gradual = matches!(s, CheckTy::Var(_, _) | CheckTy::Any)
            || matches!(d, CheckTy::Var(_, _) | CheckTy::Any);
        let pair = if involves_gradual {
            Compat3::Unknown
        } else {
            let fwd = s.assignable_depth(d, table, depth + 1);
            let bwd = d.assignable_depth(s, table, depth + 1);
            match (fwd, bwd) {
                (Compat3::No, Compat3::No) => return Compat3::No, // concrete-distinct both ways
                (Compat3::Yes, Compat3::Yes) => Compat3::Yes,
                _ => Compat3::Unknown,
            }
        };
        acc = meet_compat(acc, pair);
    }
    acc
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
        Var(_, _) => 23,
        FnSig(_, _) => 24,
        ClassApp(_, _) => 25,
        EnumApp(_, _) => 26,
        Interface(_) => 27,
    }
}

/// A secondary key (within the same discriminant) for a fully stable sort.
fn secondary_key(t: &CheckTy) -> usize {
    use CheckTy::*;
    match t {
        Class(id) | Enum(id) => *id,
        EnumVariant(id, _) => *id,
        ClassApp(id, _) | EnumApp(id, _) | Interface(id) => *id,
        Var(id, _) => *id as usize,
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
            // `Any` swallows a union (gradual top); a leftover `Var` widens to `Any`
            // (TYPE §4.2: an unsolved type variable is the gradual top) and so does
            // too.
            CheckTy::Any | CheckTy::Var(_, _) => return CheckTy::Any,
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

    // ---- TYPE Task 8: new variant display / widen / from_type_node lowering ----

    fn build_table(src: &str) -> Table {
        let tree = crate::syntax::tree_builder::build_tree(crate::syntax::parser::parse(src));
        let resolved = crate::syntax::resolve::resolve(&tree);
        Table::build(&tree, &resolved)
    }

    /// Find the first type-annotation node of `kind` in a built tree and lower it.
    fn lower_first(src: &str, kind: SyntaxKind) -> CheckTy {
        let tree = crate::syntax::tree_builder::build_tree(crate::syntax::parser::parse(src));
        let resolved = crate::syntax::resolve::resolve(&tree);
        let table = Table::build(&tree, &resolved);
        let node = tree
            .descendants()
            .find(|n| n.kind() == kind)
            .expect("a node of the requested kind");
        CheckTy::from_type_node(node, &table)
    }

    #[test]
    fn var_widens_to_any() {
        let v = CheckTy::Var(param_template_id("T"), None);
        assert_eq!(v.widen(), CheckTy::Any);
        // displayed (which widens first) → "any"
        assert_eq!(v.display(&t()), "any");
    }

    #[test]
    fn param_template_id_is_stable_and_distinct() {
        assert_eq!(param_template_id("T"), param_template_id("T"));
        assert_ne!(param_template_id("T"), param_template_id("U"));
        assert!(is_template_var(param_template_id("T")));
        assert!(!is_template_var(0));
        assert!(!is_template_var(5));
    }

    #[test]
    fn fnsig_displays() {
        let sig = CheckTy::FnSig(vec![CheckTy::Int], Box::new(CheckTy::String));
        assert_eq!(sig.display(&t()), "fn(int) -> string");
    }

    #[test]
    fn classapp_displays() {
        let table = build_table("class Box<T> { value: number }");
        let cid = table.class_id("Box").unwrap();
        let app = CheckTy::ClassApp(cid, vec![CheckTy::Int]);
        assert_eq!(app.display(&table), "Box<int>");
    }

    #[test]
    fn from_type_node_param_lowers_to_var() {
        // `T` in a generic-class field type is a ParamType → Var.
        let ty = lower_first("class Box<T> { value: T }", SyntaxKind::ParamType);
        assert!(matches!(ty, CheckTy::Var(_, None)));
    }

    #[test]
    fn from_type_node_fn_type_lowers_to_fnsig() {
        let ty = lower_first(
            "fn higher(f: fn(int) -> string) {}",
            SyntaxKind::FnType,
        );
        match ty {
            CheckTy::FnSig(params, ret) => {
                assert_eq!(params, vec![CheckTy::Int]);
                assert_eq!(*ret, CheckTy::String);
            }
            other => panic!("expected FnSig, got {other:?}"),
        }
    }

    #[test]
    fn from_type_node_user_generic_head_lowers_to_classapp() {
        // `Box<int>` (a user class head) → ClassApp (was Any before TYPE).
        let table = build_table("class Box<T> { value: T }\nlet b: Box<int> = x");
        let tree = crate::syntax::tree_builder::build_tree(crate::syntax::parser::parse(
            "class Box<T> { value: T }\nlet b: Box<int> = x",
        ));
        let node = tree
            .descendants()
            .find(|n| n.kind() == SyntaxKind::GenericType)
            .unwrap();
        let ty = CheckTy::from_type_node(node, &table);
        match ty {
            CheckTy::ClassApp(id, args) => {
                assert_eq!(id, table.class_id("Box").unwrap());
                assert_eq!(args, vec![CheckTy::Int]);
            }
            other => panic!("expected ClassApp, got {other:?}"),
        }
    }

    #[test]
    fn from_type_node_unknown_generic_head_stays_any() {
        // `Widget<int>` — unknown head → Any (gradual-preserving).
        let table = build_table("let b: Widget<int> = x");
        let tree = crate::syntax::tree_builder::build_tree(crate::syntax::parser::parse(
            "let b: Widget<int> = x",
        ));
        let node = tree
            .descendants()
            .find(|n| n.kind() == SyntaxKind::GenericType)
            .unwrap();
        assert_eq!(CheckTy::from_type_node(node, &table), CheckTy::Any);
    }

    // ------------------ TYPE Task 11: assignable for the new variants ------------

    fn cid(table: &Table, name: &str) -> ClassId {
        table.class_id(name).unwrap()
    }

    #[test]
    fn var_is_gradual_both_directions_never_no() {
        let tbl = t();
        let v = CheckTy::Var(param_template_id("T"), None);
        // Var as source: into anything → Unknown, never No.
        assert_eq!(v.assignable(&CheckTy::Int, &tbl), Compat3::Unknown);
        assert_eq!(v.assignable(&CheckTy::String, &tbl), Compat3::Unknown);
        // Var as destination (unbounded): from anything → Unknown, never No.
        assert_eq!(CheckTy::Int.assignable(&v, &tbl), Compat3::Unknown);
        assert_eq!(CheckTy::String.assignable(&v, &tbl), Compat3::Unknown);
    }

    #[test]
    fn classapp_invariance() {
        let table = build_table("class Box<T> { value: T }");
        let b = cid(&table, "Box");
        let box_int = CheckTy::ClassApp(b, vec![CheckTy::Int]);
        let box_str = CheckTy::ClassApp(b, vec![CheckTy::String]);
        let box_any = CheckTy::ClassApp(b, vec![CheckTy::Any]);
        // Box<int> ↮ Box<string> → No (concrete-distinct both ways).
        assert_eq!(box_int.assignable(&box_str, &table), Compat3::No);
        // Box<int> ↔ Box<any> → gradual (not No).
        assert_ne!(box_int.assignable(&box_any, &table), Compat3::No);
        assert_ne!(box_any.assignable(&box_int, &table), Compat3::No);
        // Box<int> → Box<int> → Yes.
        assert_eq!(
            box_int.assignable(&CheckTy::ClassApp(b, vec![CheckTy::Int]), &table),
            Compat3::Yes
        );
    }

    #[test]
    fn classapp_dog_animal_is_silent_not_blocking() {
        // Box<Dog> NOT assignable to Box<Animal>, but the checker stays SILENT
        // (Unknown) — the documented v1 invariance limitation, NOT a blocking No.
        let table = build_table("class Animal {}\nclass Dog extends Animal {}\nclass Box<T> { value: T }");
        let b = cid(&table, "Box");
        let dog = CheckTy::Class(cid(&table, "Dog"));
        let animal = CheckTy::Class(cid(&table, "Animal"));
        let box_dog = CheckTy::ClassApp(b, vec![dog]);
        let box_animal = CheckTy::ClassApp(b, vec![animal]);
        assert_eq!(box_dog.assignable(&box_animal, &table), Compat3::Unknown);
    }

    #[test]
    fn classapp_var_arg_never_no() {
        // Box<T> (unsolved var arg) vs Box<int> → Unknown (Var-bias), never No.
        let table = build_table("class Box<T> { value: T }");
        let b = cid(&table, "Box");
        let box_t = CheckTy::ClassApp(b, vec![CheckTy::Var(param_template_id("T"), None)]);
        let box_int = CheckTy::ClassApp(b, vec![CheckTy::Int]);
        assert_eq!(box_t.assignable(&box_int, &table), Compat3::Unknown);
        assert_eq!(box_int.assignable(&box_t, &table), Compat3::Unknown);
    }

    #[test]
    fn classapp_distinct_heads_no() {
        let table = build_table("class Box<T> { value: T }\nclass Cell<T> { value: T }");
        let b = cid(&table, "Box");
        let c = cid(&table, "Cell");
        let box_int = CheckTy::ClassApp(b, vec![CheckTy::Int]);
        let cell_int = CheckTy::ClassApp(c, vec![CheckTy::Int]);
        assert_eq!(box_int.assignable(&cell_int, &table), Compat3::No);
    }

    #[test]
    fn fnsig_vs_bare_fn_is_unknown() {
        let tbl = t();
        let sig = CheckTy::FnSig(vec![CheckTy::Int], Box::new(CheckTy::String));
        assert_eq!(sig.assignable(&CheckTy::Fn, &tbl), Compat3::Unknown);
        assert_eq!(CheckTy::Fn.assignable(&sig, &tbl), Compat3::Unknown);
    }

    #[test]
    fn fnsig_vs_nonfn_concrete_is_no() {
        let tbl = t();
        let sig = CheckTy::FnSig(vec![CheckTy::Int], Box::new(CheckTy::String));
        assert_eq!(sig.assignable(&CheckTy::Int, &tbl), Compat3::No);
    }

    #[test]
    fn interface_destination_uses_conforms() {
        let table = build_table(
            "interface Shape { fn area(): number }\nclass Circle { fn area(): number { return 1 } }\nclass Empty { fn name(): string { return \"x\" } }",
        );
        let iid = table.interface_id("Shape").unwrap();
        let iface = CheckTy::Interface(iid);
        let circle = CheckTy::Class(table.class_id("Circle").unwrap());
        let empty = CheckTy::Class(table.class_id("Empty").unwrap());
        assert_eq!(circle.assignable(&iface, &table), Compat3::Yes);
        assert_eq!(empty.assignable(&iface, &table), Compat3::No);
        // a primitive into an interface slot → Unknown (gradual, not a class).
        assert_eq!(CheckTy::Int.assignable(&iface, &table), Compat3::Unknown);
    }

    #[test]
    fn bounded_var_destination_checks_conforms() {
        let table = build_table(
            "interface Shape { fn area(): number }\nclass Circle { fn area(): number { return 1 } }\nclass Empty { fn name(): string { return \"x\" } }",
        );
        let iid = table.interface_id("Shape").unwrap();
        let bounded = CheckTy::Var(
            param_template_id("T"),
            Some(Box::new(CheckTy::Interface(iid))),
        );
        let circle = CheckTy::Class(table.class_id("Circle").unwrap());
        let empty = CheckTy::Class(table.class_id("Empty").unwrap());
        // A conforming class into a bounded `T` slot → Yes; non-conforming → No.
        assert_eq!(circle.assignable(&bounded, &table), Compat3::Yes);
        assert_eq!(empty.assignable(&bounded, &table), Compat3::No);
    }

    #[test]
    fn num_interplay_int_into_number_and_not_string() {
        let tbl = t();
        // T solved to int flows into `: number` (Yes — union membership via rule 9 /
        // numeric tower) and NOT into `: string` (No).
        assert_eq!(CheckTy::Int.assignable(&CheckTy::Number, &tbl), Compat3::Yes);
        assert_eq!(CheckTy::Int.assignable(&CheckTy::String, &tbl), Compat3::No);
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

    // ---- ELIDE Task 1.1: rule-6 instance→object assignability bug fix --------
    //
    // The checker's rule 6 previously returned `Yes` for `Class(_) → Object` and
    // `ClassApp(_, _) → Object`, but the runtime `check_type` for `object` only
    // accepts `ValueKind::Object(_)` — it REJECTS `ValueKind::Instance(_)`.
    // See `src/interp.rs` `check_type` `Type::Object` arm (line ~8545).
    //
    // ELIDE §6.6: the correct verdict is `Unknown` (gradual-silent, never No),
    // so the checker stays silent (zero new corpus diagnostics) while ELIDE cannot
    // treat these as elision-safe.
    #[test]
    fn class_to_object_is_unknown_not_yes() {
        let table = build_table("class C {}");
        let c_id = table.class_id("C").unwrap();

        // Class(_) → Object must be Unknown (not Yes) — runtime rejects instances.
        assert_eq!(
            CheckTy::Class(c_id).assignable(&CheckTy::Object, &table),
            Compat3::Unknown,
            "Class→Object must be Unknown (runtime rejects instances against `object` contract)"
        );

        // ClassApp(_, _) → Object must also be Unknown — same runtime divergence.
        let box_table = build_table("class Box<T> { value: T }");
        let box_id = box_table.class_id("Box").unwrap();
        let box_int = CheckTy::ClassApp(box_id, vec![CheckTy::Int]);
        assert_eq!(
            box_int.assignable(&CheckTy::Object, &box_table),
            Compat3::Unknown,
            "ClassApp→Object must be Unknown (runtime rejects instances against `object` contract)"
        );
    }
}
