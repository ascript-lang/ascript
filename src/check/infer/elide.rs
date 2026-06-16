//! ELIDE — the pure predicate foundation (spec §2.2, §2.3, §4.1).
//!
//! This module is the **pure, total, checker-state-free** core of contract
//! elision: the two predicates the (E)(Y)(A) proof rests on, plus the
//! [`ElisionSet`] data structure and its byte→char span helper. Anchoring and
//! proof COLLECTION (which consult these predicates while walking the pass) land
//! in Task 1.3 — this file is just the tables + the set + the key convention.
//!
//! Soundness discipline (the whole-file invariant): **when unsure, fail safe.**
//! [`elide_safe`] returns `false` for any [`CheckTy`] whose runtime check is not a
//! pure, env-free function of the value's stable kind — a miss merely keeps the
//! runtime check, never removes one. [`arith_result_kind`] returns `None` for any
//! operator/operand combination whose runtime result kind is not deterministically
//! a single kind — an unknown combination is never anchored.
//!
//! This module is **feature-independent** (it must build under
//! `--no-default-features`): it depends only on `CheckTy` and `std`.

use crate::check::infer::ty::{CheckTy, LitVal};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// (E) — ElideSafe destination types (spec §2.2)
// ---------------------------------------------------------------------------

/// **(E)** Is the runtime check for a slot declared with type `ty` a *pure
/// function of the value's stable kind*, resolvable WITHOUT the environment?
///
/// This is the §2.2 table, transcribed onto the checker's lowered [`CheckTy`]
/// lattice (the table in the spec is written over `ast::Type`; the correspondence
/// is noted per arm). Only an ElideSafe destination is a candidate for elision —
/// for every other form the runtime check stays exactly as today.
///
/// The soundness theorem (spec §2.1): for an ElideSafe `T`,
/// `check_type_env(v, T, env)` depends only on the *kind* of `v`, never the env —
/// so a concrete-`Yes` assignability verdict against an anchored argument proves
/// the check passes for *every* execution. A type that fails this predicate could
/// have an env-dependent or interior-mutable or deep check, so it is excluded.
///
/// Pure and total; no I/O, no checker state. **Default-closed: unknown ⇒ `false`.**
pub fn elide_safe(ty: &CheckTy) -> bool {
    match ty {
        // Scalar kinds — kind-only, immutable per value. (§2.2 rows 1–2.)
        CheckTy::Int | CheckTy::Float | CheckTy::Number => true,
        CheckTy::String | CheckTy::Bool | CheckTy::Nil => true,

        // `any` — the check is a no-op (always passes); elidable as a FREE-PASS
        // with no argument proof needed. (§2.2 `Any` row.)
        CheckTy::Any => true,

        // A runtime-ERASED generic type parameter `Param(T)` lowers to `Var`
        // (`from_type_node` ParamType arm), and `check_type` treats it as `Any`
        // (always passes). Free-pass, like `Any`. (§2.2 `Param(T)` row.) An
        // unsolved `Var` widening to `Any` is the same conclusion.
        CheckTy::Var(_, _) => true,

        // Callable kinds — kind-only. `FnSig(..)` is erased to the bare callable
        // check at runtime, so its obligation is exactly the `Fn` kind check.
        // (§2.2 `Fn` / `FnSig` rows.)
        CheckTy::Fn | CheckTy::FnSig(_, _) => true,

        // The O(n) element walk of a bare untyped `array<any>` / `map<any, any>`
        // always succeeds (every element vacuously passes), so the whole check is
        // extensionally kind-only — and eliding it also removes the walk. A typed
        // array/map is EXCLUDED (interior mutation invalidates depth between check
        // sites). (§2.2 `Array(Any)` / `Map(Any, Any)` rows vs the deep rows.)
        CheckTy::Array(inner) => matches!(**inner, CheckTy::Any),
        CheckTy::Map(k, v) => matches!(**k, CheckTy::Any) && matches!(**v, CheckTy::Any),

        // A union (incl. `T?` sugar, which normalizes to `Union[T, Nil]`) is
        // ElideSafe iff EVERY member is — the union of kind-only checks is
        // kind-only. (§2.2 `Optional(T)` / `Union(a, b)` row.) An empty union
        // cannot arise (normalize collapses it), but `all` over empty is `true`;
        // guard it to fail-safe anyway.
        CheckTy::Union(members) => !members.is_empty() && members.iter().all(elide_safe),

        // An internal narrowing literal widens to its base primitive, which is
        // ElideSafe (Number/String/Bool/Nil). (Artifacts widen before any verdict;
        // §2.3 narrowing.)
        CheckTy::Literal(LitVal::Number | LitVal::String | LitVal::Bool | LitVal::Nil) => true,

        // ---- Everything below is NOT ElideSafe (default-closed). ----

        // `object` — the runtime check REJECTS instances, but the checker's rule 6
        // (pre-ELIDE) said instance→object is assignable; the two disagree
        // (spec §0 #3). Excluded as defense in depth (the rule-6 verdict is also
        // fixed to `Unknown` in `ty.rs`, ELIDE §6.6). (§2.2 `Object` row → NO.)
        CheckTy::Object => false,

        // Deep / interior-mutable containers: a `tuple` / `Result` check is a
        // per-element / length walk that interior mutation can invalidate between
        // check sites; tuple length is mutable. (Typed `Array`/`Map` are handled by
        // the `Array`/`Map` arms ABOVE — only the bare-`any` forms are ElideSafe.)
        // (§2.2 `Tuple` / `Result` rows → NO.)
        CheckTy::Tuple(_) | CheckTy::Result(_) => false,

        // `future<T>` — async sites are out of v1 eligibility anyway. (§2.2 row → NO.)
        CheckTy::Future(_) => false,

        // `error` — `object | nil` hybrid with asymmetric semantics; not worth the
        // v1 audit. (§2.2 `Error` row → NO.)
        CheckTy::Error => false,

        // `bytes` / `regex` — not in the §2.2 table; default-closed → NO. (A
        // `bytes`/`regex` annotation is a kind check at runtime, but it is not on
        // the spec's allowlist, and the soundness rule is "unsure ⇒ false".)
        CheckTy::Bytes | CheckTy::Regex => false,

        // Named-derived nominal types — class / enum / interface (incl. their
        // parameterized `ClassApp`/`EnumApp` and the narrowing `EnumVariant`).
        // Their runtime check is ENV-RESOLVED (`check_type_env`): interface →
        // structural `conforms`, class → nominal lookup through the callee frame's
        // env chain (shadowing, late binding) — the checker resolves lexically and
        // the two can disagree. (§2.2 `Named(name)` row → NO.)
        CheckTy::Class(_)
        | CheckTy::ClassApp(_, _)
        | CheckTy::Enum(_)
        | CheckTy::EnumApp(_, _)
        | CheckTy::EnumVariant(_, _)
        | CheckTy::Interface(_) => false,

        // `Never` is an internal bottom (exhaustive narrowing); it is never a
        // user-written annotation slot, so treat it fail-safe → NO.
        CheckTy::Never => false,
    }
}

// ---------------------------------------------------------------------------
// (A) — arithmetic/comparison result-kind table (the NUM mirror, spec §2.3)
// ---------------------------------------------------------------------------

/// The runtime KIND of a value, for the anchoring result-kind table. A minimal,
/// pure enum (the four kinds AScript's arithmetic/comparison surface can produce)
/// — deliberately NOT a `CheckTy` (we want a closed, total domain with no gradual
/// arms). Maps 1:1 onto the `type()` builtin strings `"int"`/`"float"`/`"string"`/
/// `"bool"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResultKind {
    Int,
    Float,
    String,
    Bool,
}

/// The operand kind feeding [`arith_result_kind`]. A binary op is anchorable only
/// when BOTH operands resolve to one of these concrete kinds (§2.3 — an anchored
/// operand has a runtime-guaranteed kind). `Number` is included because a
/// `: number`-typed anchored operand has a definite-but-unknown-subtype kind; the
/// table below treats it conservatively (it never produces a definite `Int`/`Float`
/// result unless BOTH operands are the same concrete subtype).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OperandKind {
    Int,
    Float,
    Number,
    String,
    Bool,
    Nil,
}

/// The arithmetic / comparison / logical operator at an anchored binary site. The
/// names mirror the surface operators (NUM §10); a `%`/`**` are arithmetic,
/// `+%`/`-%`/`*%` are wrapping arithmetic, `&`/`|`/`^`/`<<`/`>>` are bitwise/shift,
/// the comparisons are `==`/`!=`/`<`/`<=`/`>`/`>=`/`instanceof`, and the logical
/// trio `&&`/`||`/`??` is the one family that is **never** anchored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArithOp {
    /// `+` — numeric add OR string concatenation (overloaded on operand kind).
    Add,
    /// `- * / % **` — strictly numeric arithmetic: `int ∘ int → int` (incl. the
    /// truncating `/`), any `float` operand → `float`. (These ACCEPT floats — they
    /// are NOT the wrapping family.)
    NumericArith,
    /// `+% -% *%` — explicit overflow-WRAPPING arithmetic. Per NUM these are
    /// **int-only** (a `float` operand panics: `wrapping op requires int operands`),
    /// so the only value-producing case is `int ∘ int → int`. (Verified against the
    /// runtime 2026-06-15 — `5.0 +% 2.0` panics. This corrects the plan-task
    /// summary, which grouped them with the float-accepting arithmetic.)
    WrappingArith,
    /// `& | ^ << >> ~` — bitwise / shift; int-only operands → `int`.
    Bitwise,
    /// `== != < <= > >= instanceof` — comparison/membership → `bool`.
    Comparison,
    /// `&& || ??` — logical / coalesce; returns an OPERAND (truthiness), not a
    /// fixed kind → NEVER anchored (the table returns `None`).
    Logical,
}

/// **(A) helper.** The runtime result KIND of a binary op given its two operand
/// kinds — the NUM-promotion MIRROR. Returns `None` when the result kind is not a
/// single deterministic kind (so the site is **not** anchored): the logical trio,
/// and any operand combination NUM does not deterministically promote.
///
/// NUM model (CLAUDE.md "Numeric model"):
/// - numeric arithmetic (`- * / % **`, wrapping): `int ∘ int → int` (incl. the
///   truncating `/`); any `float` operand → `float`. Mixed with `number` is not a
///   provable single subtype → `None`.
/// - `+`: numeric like the above, OR `string + string → string`. A mixed
///   string/number `+` is a runtime panic (not a kind) → `None`.
/// - bitwise/shift: `int ∘ int → int` only.
/// - comparison: always `bool` (any operands — the runtime yields a bool or panics
///   before the site).
/// - logical/coalesce: `None` (never anchored).
///
/// Pure and total. **Default-closed: an unhandled combination ⇒ `None`.**
pub fn arith_result_kind(op: ArithOp, lhs: OperandKind, rhs: OperandKind) -> Option<ResultKind> {
    use OperandKind as K;
    match op {
        // `&& || ??` return an operand (truthiness), not a fixed kind. Never
        // anchored. (§2.3 logical row → NOT anchored.)
        ArithOp::Logical => None,

        // Comparisons always produce a bool (or panic before the site). (§2.3
        // comparison row → always `Bool`.) The operand kinds are irrelevant to the
        // result kind, so any pair is anchored as `Bool`.
        ArithOp::Comparison => Some(ResultKind::Bool),

        // Bitwise/shift AND wrapping arithmetic are int-only: both operands must be
        // `int` → `int`. Anything else (a `float`/`number`/`string`/`bool`/`nil`
        // operand) is a runtime panic, not a kind → `None` (the panic happens
        // before the site).
        ArithOp::Bitwise | ArithOp::WrappingArith => match (lhs, rhs) {
            (K::Int, K::Int) => Some(ResultKind::Int),
            _ => None,
        },

        // Strictly-numeric arithmetic (`- * / % **`, wrapping `+% -% *%`):
        //   int  ∘ int   → int   (incl. truncating `/`)
        //   any float operand (with a numeric other operand) → float
        // A `number`-typed operand is not a provable single subtype, so any pair
        // involving `Number` is `None` (we cannot prove int-vs-float). Non-numeric
        // operands → `None` (runtime panic before the site).
        ArithOp::NumericArith => numeric_result(lhs, rhs),

        // `+` is numeric like the above, PLUS string concatenation.
        ArithOp::Add => match (lhs, rhs) {
            (K::String, K::String) => Some(ResultKind::String),
            // a mixed string/non-string `+` is a runtime panic, not a kind.
            (K::String, _) | (_, K::String) => None,
            // otherwise it is numeric `+`.
            _ => numeric_result(lhs, rhs),
        },
    }
}

/// The numeric-promotion sub-table shared by `+` (numeric branch) and the strictly
/// numeric arithmetic ops. `int ∘ int → int`; any `float` (with the other operand
/// numeric-concrete) → `float`; anything involving `number`/non-numeric → `None`.
fn numeric_result(lhs: OperandKind, rhs: OperandKind) -> Option<ResultKind> {
    use OperandKind as K;
    match (lhs, rhs) {
        (K::Int, K::Int) => Some(ResultKind::Int),
        (K::Int, K::Float) | (K::Float, K::Int) | (K::Float, K::Float) => Some(ResultKind::Float),
        // `number` (unknown int-vs-float subtype) on either side → not a provable
        // single result kind. Non-numeric operands (`string`/`bool`/`nil`) →
        // runtime panic, not a kind. Both default-closed to `None`.
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// ElisionSet + the byte→char span helper (spec §4.1)
// ---------------------------------------------------------------------------

/// The per-module set of PROVEN sites (spec §4.1). All spans are **CHAR offsets**
/// into the module source `(start, end)` (the legacy front-end's `Span`
/// convention), converted once from the CST's BYTE ranges by [`ByteToCharMap`].
///
/// The collector (Task 1.3) records keys here; the VM compiler and the tree-walker
/// marking pass each look them up by EXACT match (fail-safe — a miss keeps the
/// check). The keys are deliberately the most collision-proof extent for each site
/// kind (§4.1):
/// - `calls`: the call expression's trivia-trimmed `(start, end)` extent.
/// - `lets`: the INITIALIZER expression's extent (the same span `Op::CheckLocal`
///   is emitted at and the tree-walker panics at).
/// - `fn_rets`: the fn's NAME-token span (a single token — the most collision-proof
///   key; a whole-fn return contract is dropped at definition).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ElisionSet {
    /// Proven call sites, keyed by the call expression's trivia-trimmed
    /// `(start_char, end_char)` extent.
    pub calls: HashSet<(u32, u32)>,
    /// Proven annotated-`let`/`const` sites, keyed by the INITIALIZER expression's
    /// `(start_char, end_char)` extent.
    pub lets: HashSet<(u32, u32)>,
    /// Proven whole-fn return contracts, keyed by the fn's NAME-token
    /// `(start_char, end_char)` extent.
    pub fn_rets: HashSet<(u32, u32)>,
}

impl ElisionSet {
    /// A fresh, empty set (no proven sites). Equivalent to `Default::default()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// The total number of proven sites across all three kinds — the value the
    /// count-parity gate (§6.4) asserts equals both the VM compiler's consumed
    /// count and the tree-walker marker's mark count.
    pub fn len(&self) -> usize {
        self.calls.len() + self.lets.len() + self.fn_rets.len()
    }

    /// Whether this set proves nothing (the untyped-corpus expectation, §6.4).
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty() && self.lets.is_empty() && self.fn_rets.is_empty()
    }
}

/// A precomputed BYTE-offset → CHAR-offset map over one module source, built ONCE
/// and indexed in O(1) per conversion (NOT an O(n) re-scan per call). `prefix[b]`
/// is the number of `char`s strictly before byte offset `b`; a non-char-boundary
/// (continuation) byte clamps DOWN to the char it sits inside.
///
/// This mirrors the semantics of `compile/mod.rs`'s `ByteToChar` exactly (so the
/// CHAR spans this produces match the VM compiler's `node_span` spans for the same
/// nodes — a hard requirement for the count-parity gate). It is a STANDALONE copy
/// rather than a reuse of that module's thread-local: the compiler's helper is a
/// private thread-local installed per `compile_source`, not a clean API the checker
/// can borrow, so per the task's production-grade mandate the checker carries its
/// own documented prefix map.
#[derive(Debug)]
pub struct ByteToCharMap {
    /// `prefix[b]` = char count of `src[..b']` where `b'` is the largest char
    /// boundary `<= b`. Length is `src.len() + 1`.
    prefix: Vec<u32>,
}

impl ByteToCharMap {
    /// Build the map over `src` in one linear pass (O(n) once; O(1) lookups).
    pub fn new(src: &str) -> Self {
        // `prefix[b]` = the number of chars strictly before byte `b`. A char's
        // first byte and any continuation bytes `[b, b + len)` all clamp to the
        // char index `i` (chars strictly before this char). The tail (from the
        // last char's end through `src.len()`) maps to the total char count.
        let mut prefix = vec![0u32; src.len() + 1];
        let mut next_byte = 0usize;
        let mut i = 0u32;
        for (b, ch) in src.char_indices() {
            for slot in prefix.iter_mut().take(b + ch.len_utf8()).skip(b) {
                *slot = i;
            }
            next_byte = b + ch.len_utf8();
            i += 1;
        }
        for slot in prefix.iter_mut().skip(next_byte) {
            *slot = i;
        }
        ByteToCharMap { prefix }
    }

    /// The CHAR offset for `byte` (clamping a continuation byte DOWN, and an
    /// out-of-range byte to the total char count). O(1).
    pub fn char_of(&self, byte: usize) -> u32 {
        self.prefix
            .get(byte)
            .copied()
            .unwrap_or_else(|| self.prefix.last().copied().unwrap_or(0))
    }

    /// Convert a `[start, end)` BYTE range into a `(start_char, end_char)` CHAR
    /// span — the [`ElisionSet`] key form. O(1) (two indexed lookups).
    pub fn char_span(&self, start: usize, end: usize) -> (u32, u32) {
        (self.char_of(start), self.char_of(end))
    }
}

/// Convert a `[start, end)` BYTE range into a `(start_char, end_char)` CHAR span
/// using a freshly-built [`ByteToCharMap`]. **For one-off conversions only** — if
/// you convert more than one range over the same `src`, build a [`ByteToCharMap`]
/// once and reuse it (this function rebuilds the whole prefix map per call, so a
/// per-call use of it would be O(n) per conversion, which the §4.1 collector must
/// NOT do).
pub fn byte_range_to_char_span(src: &str, range: std::ops::Range<usize>) -> (u32, u32) {
    ByteToCharMap::new(src).char_span(range.start, range.end)
}

// ---------------------------------------------------------------------------
// (A) — mapping a synthesized `CheckTy` to a concrete operand/result kind
// ---------------------------------------------------------------------------

/// **(A) helper — binding anchoring.** Whether an ElideSafe binding declared with
/// type `ty` ANCHORS its value's runtime kind. An ElideSafe type is a valid elision
/// DESTINATION, but only a KIND-PINNED one anchors a SOURCE: `any` and an erased
/// `Var` (`Param(T)`) are ElideSafe *free-passes* that guarantee NOTHING about the
/// value's kind, so a binding annotated `any`/`T` is **not** anchored even though its
/// entry check is unmutated. Everything else ElideSafe (scalars, callables, the
/// `array<any>`/`map<any,any>` walks, ElideSafe unions) pins a kind set the entry
/// check enforced. Default-closed: a non-ElideSafe type is never anchored.
pub fn anchors_binding_kind(ty: &CheckTy) -> bool {
    elide_safe(ty) && !matches!(ty, CheckTy::Any | CheckTy::Var(_, _))
}

/// The CONCRETE runtime [`OperandKind`] of a synthesized [`CheckTy`], for the
/// arithmetic anchoring table — or `None` when the type is not a single, provable,
/// non-gradual kind. **Default-closed:** `Any`/`Number`-without-subtype/containers/
/// nominal types all yield a value that the anchoring table must treat
/// conservatively. We map:
/// - `Int`/`Float`/`String`/`Bool`/`Nil` → their kind;
/// - `Number` → [`OperandKind::Number`] (a definite numeric value of unknown
///   int-vs-float subtype — the table never produces a definite `Int`/`Float` from
///   it, but a comparison over it is still `Bool`);
/// - internal narrowing literals widen to their base kind;
/// - everything else (gradual `Any`, a `Var`, containers, classes, …) → `None`
///   (NOT anchorable as an arithmetic operand).
///
/// Pure and total. Note this is the OPERAND domain (it admits `Number`/`Nil`),
/// distinct from [`result_kind_of`] (the four kinds an op can PRODUCE).
pub fn operand_kind(ty: &CheckTy) -> Option<OperandKind> {
    match ty {
        CheckTy::Int => Some(OperandKind::Int),
        CheckTy::Float => Some(OperandKind::Float),
        CheckTy::Number => Some(OperandKind::Number),
        CheckTy::String => Some(OperandKind::String),
        CheckTy::Bool => Some(OperandKind::Bool),
        CheckTy::Nil => Some(OperandKind::Nil),
        // Internal narrowing literals widen to their base primitive kind.
        CheckTy::Literal(LitVal::Number) => Some(OperandKind::Number),
        CheckTy::Literal(LitVal::String) => Some(OperandKind::String),
        CheckTy::Literal(LitVal::Bool) => Some(OperandKind::Bool),
        CheckTy::Literal(LitVal::Nil) => Some(OperandKind::Nil),
        // Gradual / variable / aggregate / nominal — not a single provable operand
        // kind. Default-closed → None (the binary site is not anchored).
        _ => None,
    }
}

/// The CONCRETE [`ResultKind`] of a synthesized [`CheckTy`] — the four kinds an
/// anchored expression can *produce* (the kinds an anchored ARGUMENT must have to
/// feed a downstream check). `None` for any type that is not exactly one of
/// `Int`/`Float`/`String`/`Bool`. **Default-closed.**
///
/// This is intentionally narrower than [`operand_kind`]: an anchored value's
/// produced kind must be a concrete scalar the destination check accepts. A bare
/// `Number` (unknown subtype) is NOT a single result kind here → `None` (so a
/// `number`-valued expression is never anchored as a *source*; it can still be an
/// arithmetic OPERAND via [`operand_kind`]).
pub fn result_kind_of(ty: &CheckTy) -> Option<ResultKind> {
    match ty {
        CheckTy::Int => Some(ResultKind::Int),
        CheckTy::Float => Some(ResultKind::Float),
        CheckTy::String => Some(ResultKind::String),
        CheckTy::Bool => Some(ResultKind::Bool),
        CheckTy::Literal(LitVal::String) => Some(ResultKind::String),
        CheckTy::Literal(LitVal::Bool) => Some(ResultKind::Bool),
        // `Number` (no subtype), `Nil`, `Any`, containers, nominal → not a single
        // result kind. Default-closed → None.
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The collector state (spec §4.1) — driven by the inference pass in elide mode
// ---------------------------------------------------------------------------

/// Per-function accumulator for the row-3 (declared-return) proof (§3 row 3). One
/// is pushed per fn/method body the collector enters and popped (consulted) at
/// body exit; nested fns nest naturally on the stack.
#[derive(Debug)]
pub struct FnReturnProof {
    /// The fn's NAME-token CHAR span — the [`ElisionSet::fn_rets`] key, recorded
    /// only when ALL the conditions hold.
    pub name_key: (u32, u32),
    /// The fn's declared return type, lowered — `Some` ONLY when it is ElideSafe
    /// (an absent or non-ElideSafe declared return makes the fn ineligible, so the
    /// accumulator is never pushed in that case; see [`ElideCollect::push_fn`]).
    pub elide_safe_ret: bool,
    /// Whether EVERY `return <expr>` seen so far is concrete-`Yes` + Anchored
    /// against the declared return (§3 row 3). Starts `true`; any unproven return
    /// flips it `false` permanently.
    pub all_returns_proven: bool,
}

/// The collector's mutable state, threaded as `Pass.elide: Option<ElideCollect>`
/// (mirroring `Pass.hover`). It owns the growing [`ElisionSet`], the byte→char map
/// for key conversion, and the per-fn return-proof stack.
///
/// Diagnostic-neutrality (§6.5): this struct holds NO diagnostic state and the pass
/// never branches its emit logic on it — it is a pure side-accumulator. The normal
/// diagnosing pass leaves `elide == None`, so every method here is dead code there.
#[derive(Debug)]
pub struct ElideCollect {
    /// The proven-site set being built.
    pub set: ElisionSet,
    /// The byte→char map over the module source (built once; O(1) per key).
    pub bmap: ByteToCharMap,
    /// The per-fn return-proof accumulator stack (innermost last).
    pub fn_stack: Vec<FnReturnProof>,
}

impl ElideCollect {
    /// Build a fresh collector over `src`.
    pub fn new(src: &str) -> ElideCollect {
        ElideCollect {
            set: ElisionSet::new(),
            bmap: ByteToCharMap::new(src),
            fn_stack: Vec::new(),
        }
    }

    /// Convert a `[start, end)` BYTE range into the CHAR-span key form.
    pub fn key(&self, start: usize, end: usize) -> (u32, u32) {
        self.bmap.char_span(start, end)
    }

    /// Record a proven CALL site (row 1), keyed by the call expression's extent.
    pub fn record_call(&mut self, byte_start: usize, byte_end: usize) {
        let k = self.key(byte_start, byte_end);
        self.set.calls.insert(k);
    }

    /// Record a proven annotated-`let` site (row 2), keyed by the INITIALIZER's
    /// extent.
    pub fn record_let(&mut self, byte_start: usize, byte_end: usize) {
        let k = self.key(byte_start, byte_end);
        self.set.lets.insert(k);
    }

    /// Push a fn return-proof frame at body entry. `name_key` is the fn name-token's
    /// CHAR span; `elide_safe_ret` is whether the declared return is ElideSafe (a fn
    /// with no declared return, or a non-ElideSafe one, passes `false` → it can never
    /// be recorded, but the frame is still pushed so `return` handling has a frame).
    pub fn push_fn(&mut self, name_key: (u32, u32), elide_safe_ret: bool) {
        self.fn_stack.push(FnReturnProof {
            name_key,
            elide_safe_ret,
            all_returns_proven: true,
        });
    }

    /// Mark the innermost fn's return-proof as FAILED (an unproven `return`).
    pub fn fail_current_return(&mut self) {
        if let Some(f) = self.fn_stack.last_mut() {
            f.all_returns_proven = false;
        }
    }

    /// Pop the innermost fn frame and, IFF its declared return is ElideSafe AND every
    /// return was proven AND `body_total` (the caller's "always-returns OR nil-Yes"
    /// verdict) holds, record the fn-return key.
    pub fn pop_fn(&mut self, body_total: bool) {
        if let Some(f) = self.fn_stack.pop() {
            if f.elide_safe_ret && f.all_returns_proven && body_total {
                self.set.fn_rets.insert(f.name_key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::infer::ty::CheckTy;

    // -----------------------------------------------------------------------
    // (E) elide_safe — the §2.2 table, verbatim.
    // -----------------------------------------------------------------------

    fn arr(inner: CheckTy) -> CheckTy {
        CheckTy::Array(Box::new(inner))
    }
    fn map(k: CheckTy, v: CheckTy) -> CheckTy {
        CheckTy::Map(Box::new(k), Box::new(v))
    }
    fn uni(ms: Vec<CheckTy>) -> CheckTy {
        // build a union directly (no normalize — these are already distinct
        // ElideSafe leaves and we want to test the member-wise rule precisely).
        CheckTy::Union(ms)
    }

    #[test]
    fn elide_safe_scalars_and_callables_are_true() {
        // Scalar kinds (kind-only, immutable per value).
        for ty in [
            CheckTy::Int,
            CheckTy::Float,
            CheckTy::Number,
            CheckTy::String,
            CheckTy::Bool,
            CheckTy::Nil,
        ] {
            assert!(elide_safe(&ty), "{ty:?} must be ElideSafe");
        }
        // Callables: bare `fn` and an erased `FnSig`.
        assert!(elide_safe(&CheckTy::Fn));
        assert!(elide_safe(&CheckTy::FnSig(
            vec![CheckTy::Int],
            Box::new(CheckTy::String)
        )));
    }

    #[test]
    fn elide_safe_any_and_var_are_free_pass() {
        // `any` is a no-op check (free-pass); a runtime-erased `Param(T)` lowers to
        // `Var` and is treated as `Any` at runtime (free-pass).
        assert!(elide_safe(&CheckTy::Any));
        assert!(elide_safe(&CheckTy::Var(0, None)));
        assert!(elide_safe(&CheckTy::Var(
            crate::check::infer::ty::param_template_id("T"),
            None
        )));
    }

    #[test]
    fn elide_safe_bare_untyped_array_and_map_are_true() {
        // §2.2: `Array(Any)` / `Map(Any, Any)` are ElideSafe (the element walk
        // vacuously succeeds; eliding removes the walk).
        assert!(elide_safe(&arr(CheckTy::Any)));
        assert!(elide_safe(&map(CheckTy::Any, CheckTy::Any)));
    }

    #[test]
    fn elide_safe_typed_containers_are_false() {
        // §2.2: a TYPED array/map / tuple / Result is a deep, interior-mutable
        // check → NOT ElideSafe.
        assert!(!elide_safe(&arr(CheckTy::Int)));
        assert!(!elide_safe(&arr(CheckTy::String)));
        assert!(!elide_safe(&map(CheckTy::String, CheckTy::Int)));
        assert!(!elide_safe(&map(CheckTy::Any, CheckTy::Int))); // value not any
        assert!(!elide_safe(&map(CheckTy::Int, CheckTy::Any))); // key not any
        assert!(!elide_safe(&CheckTy::Tuple(vec![CheckTy::Int, CheckTy::Int])));
        assert!(!elide_safe(&CheckTy::Result(Box::new(CheckTy::Int))));
        assert!(!elide_safe(&CheckTy::Future(Box::new(CheckTy::Int))));
    }

    #[test]
    fn elide_safe_unions_are_memberwise() {
        // A union of ElideSafe members is ElideSafe (incl. `T?` == Union[T, Nil]).
        assert!(elide_safe(&uni(vec![CheckTy::Int, CheckTy::Nil])));
        assert!(elide_safe(&uni(vec![
            CheckTy::Int,
            CheckTy::Float,
            CheckTy::String
        ])));
        assert!(elide_safe(&uni(vec![CheckTy::String, CheckTy::Nil])));
        // A union with ONE non-ElideSafe member is NOT ElideSafe.
        assert!(!elide_safe(&uni(vec![CheckTy::Int, CheckTy::Object])));
        assert!(!elide_safe(&uni(vec![CheckTy::Int, arr(CheckTy::Int)])));
        assert!(!elide_safe(&uni(vec![CheckTy::String, CheckTy::Class(0)])));
        // An empty union is fail-safe → false (cannot normally arise).
        assert!(!elide_safe(&CheckTy::Union(vec![])));
    }

    #[test]
    fn elide_safe_object_and_named_are_false() {
        // §2.2: `Object` is excluded (runtime rejects instances; §0 #3).
        assert!(!elide_safe(&CheckTy::Object));
        // Named-derived nominal types: class / enum / interface (+ parameterized).
        assert!(!elide_safe(&CheckTy::Class(0)));
        assert!(!elide_safe(&CheckTy::ClassApp(0, vec![CheckTy::Int])));
        assert!(!elide_safe(&CheckTy::Enum(0)));
        assert!(!elide_safe(&CheckTy::EnumApp(0, vec![CheckTy::Int])));
        assert!(!elide_safe(&CheckTy::EnumVariant(
            0,
            std::rc::Rc::from("V")
        )));
        assert!(!elide_safe(&CheckTy::Interface(0)));
    }

    #[test]
    fn elide_safe_misc_excluded_forms_are_false() {
        // `error`, `bytes`, `regex`, and the internal `Never` are all NOT on the
        // §2.2 allowlist → fail-safe false.
        assert!(!elide_safe(&CheckTy::Error));
        assert!(!elide_safe(&CheckTy::Bytes));
        assert!(!elide_safe(&CheckTy::Regex));
        assert!(!elide_safe(&CheckTy::Never));
    }

    #[test]
    fn elide_safe_narrowing_literal_widens_to_safe_primitive() {
        // A narrowing literal widens to its (ElideSafe) base primitive.
        assert!(elide_safe(&CheckTy::Literal(LitVal::Number)));
        assert!(elide_safe(&CheckTy::Literal(LitVal::String)));
        assert!(elide_safe(&CheckTy::Literal(LitVal::Bool)));
        assert!(elide_safe(&CheckTy::Literal(LitVal::Nil)));
    }

    // -----------------------------------------------------------------------
    // (A) arith_result_kind — the NUM mirror (§2.3), exhaustive over the table.
    // -----------------------------------------------------------------------

    use ArithOp::*;
    use OperandKind as K;
    use ResultKind as R;

    #[test]
    fn arith_int_int_is_int_for_all_numeric_ops() {
        // int ∘ int → int, incl. the truncating `/` (NumericArith covers `/`).
        assert_eq!(arith_result_kind(Add, K::Int, K::Int), Some(R::Int));
        assert_eq!(arith_result_kind(NumericArith, K::Int, K::Int), Some(R::Int));
        assert_eq!(arith_result_kind(Bitwise, K::Int, K::Int), Some(R::Int));
        assert_eq!(arith_result_kind(WrappingArith, K::Int, K::Int), Some(R::Int));
    }

    #[test]
    fn arith_wrapping_is_int_only() {
        // `+% -% *%` are int-only (a float operand panics at runtime — verified
        // 2026-06-15). int ∘ int → int; anything else → None.
        assert_eq!(arith_result_kind(WrappingArith, K::Int, K::Int), Some(R::Int));
        assert_eq!(arith_result_kind(WrappingArith, K::Int, K::Float), None);
        assert_eq!(arith_result_kind(WrappingArith, K::Float, K::Float), None);
        assert_eq!(arith_result_kind(WrappingArith, K::Number, K::Int), None);
        assert_eq!(arith_result_kind(WrappingArith, K::String, K::String), None);
    }

    #[test]
    fn arith_mixed_int_float_is_float() {
        assert_eq!(arith_result_kind(Add, K::Int, K::Float), Some(R::Float));
        assert_eq!(arith_result_kind(Add, K::Float, K::Int), Some(R::Float));
        assert_eq!(arith_result_kind(Add, K::Float, K::Float), Some(R::Float));
        assert_eq!(
            arith_result_kind(NumericArith, K::Int, K::Float),
            Some(R::Float)
        );
        assert_eq!(
            arith_result_kind(NumericArith, K::Float, K::Float),
            Some(R::Float)
        );
    }

    #[test]
    fn arith_plus_string_string_is_string() {
        assert_eq!(arith_result_kind(Add, K::String, K::String), Some(R::String));
        // a mixed string/non-string `+` is a runtime panic, not a kind → None.
        assert_eq!(arith_result_kind(Add, K::String, K::Int), None);
        assert_eq!(arith_result_kind(Add, K::Int, K::String), None);
        // string `+` only applies to `+`, never to other numeric ops.
        assert_eq!(arith_result_kind(NumericArith, K::String, K::String), None);
    }

    #[test]
    fn arith_comparison_is_always_bool() {
        // Comparisons → bool for ANY operand kinds.
        for lhs in [K::Int, K::Float, K::Number, K::String, K::Bool, K::Nil] {
            for rhs in [K::Int, K::Float, K::Number, K::String, K::Bool, K::Nil] {
                assert_eq!(
                    arith_result_kind(Comparison, lhs, rhs),
                    Some(R::Bool),
                    "comparison {lhs:?} ∘ {rhs:?} must be Bool"
                );
            }
        }
    }

    #[test]
    fn arith_bitwise_is_int_only() {
        assert_eq!(arith_result_kind(Bitwise, K::Int, K::Int), Some(R::Int));
        // any non-int operand → None (runtime panic, not a kind).
        assert_eq!(arith_result_kind(Bitwise, K::Int, K::Float), None);
        assert_eq!(arith_result_kind(Bitwise, K::Float, K::Int), None);
        assert_eq!(arith_result_kind(Bitwise, K::Number, K::Int), None);
        assert_eq!(arith_result_kind(Bitwise, K::Int, K::Number), None);
        assert_eq!(arith_result_kind(Bitwise, K::String, K::String), None);
    }

    #[test]
    fn arith_logical_is_never_anchored() {
        // `&& || ??` return an operand, not a fixed kind → None always.
        for lhs in [K::Int, K::Float, K::Number, K::String, K::Bool, K::Nil] {
            for rhs in [K::Int, K::Float, K::Number, K::String, K::Bool, K::Nil] {
                assert_eq!(
                    arith_result_kind(Logical, lhs, rhs),
                    None,
                    "logical {lhs:?} ∘ {rhs:?} must be None (never anchored)"
                );
            }
        }
    }

    #[test]
    fn arith_number_operand_is_not_a_provable_subtype() {
        // A `number`-typed operand cannot prove int-vs-float, so numeric arithmetic
        // involving it is not a single result kind → None.
        assert_eq!(arith_result_kind(NumericArith, K::Number, K::Int), None);
        assert_eq!(arith_result_kind(NumericArith, K::Int, K::Number), None);
        assert_eq!(arith_result_kind(NumericArith, K::Number, K::Number), None);
        assert_eq!(arith_result_kind(Add, K::Number, K::Float), None);
    }

    #[test]
    fn arith_nonnumeric_operands_are_none() {
        // bool/nil operands feeding arithmetic → runtime panic, not a kind → None.
        assert_eq!(arith_result_kind(NumericArith, K::Bool, K::Int), None);
        assert_eq!(arith_result_kind(NumericArith, K::Nil, K::Nil), None);
        assert_eq!(arith_result_kind(Add, K::Bool, K::Bool), None);
    }

    // -----------------------------------------------------------------------
    // ElisionSet + ByteToCharMap.
    // -----------------------------------------------------------------------

    #[test]
    fn elision_set_len_and_empty() {
        let mut s = ElisionSet::new();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        s.calls.insert((0, 5));
        s.lets.insert((10, 12));
        s.fn_rets.insert((20, 21));
        assert!(!s.is_empty());
        assert_eq!(s.len(), 3);
        // duplicate insert does not grow the count.
        s.calls.insert((0, 5));
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn byte_to_char_map_ascii_is_identity() {
        let src = "fn f(p) { p }";
        let m = ByteToCharMap::new(src);
        for b in 0..=src.len() {
            assert_eq!(m.char_of(b), b as u32, "ASCII byte {b} must map to itself");
        }
        assert_eq!(m.char_span(0, 4), (0, 4));
    }

    #[test]
    fn byte_to_char_map_multibyte() {
        // `π` is 2 UTF-8 bytes / 1 char. "πx" → bytes: [0,1]=π, [2]=x, [3]=end.
        let src = "πx";
        assert_eq!(src.len(), 3);
        let m = ByteToCharMap::new(src);
        assert_eq!(m.char_of(0), 0); // start of π → char 0
        assert_eq!(m.char_of(1), 0); // π continuation byte clamps DOWN to char 0
        assert_eq!(m.char_of(2), 1); // 'x' → char 1
        assert_eq!(m.char_of(3), 2); // one-past-the-end → total char count (2)
        // out-of-range byte clamps to total.
        assert_eq!(m.char_of(99), 2);
    }

    #[test]
    fn byte_to_char_map_matches_compile_semantics_for_division_column() {
        // Mirror the compile/mod.rs `byte_to_char_map_is_correct` pin: a leading
        // multibyte char shifts a later byte's CHAR column by the byte-vs-char gap.
        // "π = 16 / 5" — the `/` token: find its byte offset, assert its char col.
        let src = "let x = π / 5";
        let m = ByteToCharMap::new(src);
        let slash_byte = src.find('/').unwrap();
        // bytes before `/`: "let x = π " = 9 chars but π is 2 bytes, so 10 bytes.
        // char col = byte col − (extra bytes of π) = slash_byte − 1.
        assert_eq!(m.char_of(slash_byte), (slash_byte - 1) as u32);
    }

    #[test]
    fn byte_range_to_char_span_oneoff() {
        let src = "πxyz";
        // "xyz" is at bytes [2, 5); chars [1, 4).
        assert_eq!(byte_range_to_char_span(src, 2..5), (1, 4));
    }
}
