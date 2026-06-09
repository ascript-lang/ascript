//! Lint rules. Each is `fn(&ResolvedNode, &ResolveResult, &str) -> Vec<AsDiagnostic>`.
use crate::check::diagnostic::{AsDiagnostic, ByteSpan};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{Resolution, ResolveResult};

pub mod call_arity;
pub mod contract;
pub mod dead_recover;
pub mod duplicate_member;
pub mod ffi_nondeterminism;
pub mod field_default_type;
pub mod ignored_result;
pub mod invalid_propagate;
pub mod missing_return;
pub mod range_step;
pub mod shadowing;
pub mod super_misuse;
pub mod unawaited;
pub mod undefined;
pub mod unknown_enum_variant;
pub mod unreachable;
pub mod unresolved_import;
pub mod unused;
pub mod worker_capture;
pub mod workflow_determinism;

pub type Rule = fn(&ResolvedNode, &ResolveResult, &str) -> Vec<AsDiagnostic>;

/// All enabled rules. (Each C2 task fills in its rule body.)
pub static ALL: &[Rule] = &[
    undefined::check,
    unused::check,
    shadowing::check,
    unreachable::check,
    missing_return::check,
    unawaited::check,
    ignored_result::check,
    dead_recover::check,
    contract::check,
    call_arity::check,
    range_step::check,
    invalid_propagate::check,
    unresolved_import::check,
    unknown_enum_variant::check,
    duplicate_member::check,
    super_misuse::check,
    field_default_type::check,
    workflow_determinism::check,
    ffi_nondeterminism::check,
    worker_capture::check,
];

/// The `CallExpr` directly dropped by an `ExprStmt` (result unused). `None` if the
/// statement's expression isn't a bare call (e.g. it's `await f()`, `x = f()`,
/// `f()?`, `f()!`, or `return f()` — those wrap the call in another node).
pub fn dropped_call(expr_stmt: &ResolvedNode) -> Option<ResolvedNode> {
    use crate::syntax::kind::SyntaxKind;
    if expr_stmt.kind() != SyntaxKind::ExprStmt {
        return None;
    }
    expr_stmt
        .children()
        .find(|c| c.kind() == SyntaxKind::CallExpr)
        .cloned()
}

/// The declared name of a `FnDecl` (its first `Ident` token), if any. Shared by
/// the rules that collect a set of locally-declared function names (async fns for
/// unawaited-future, Result-returning fns for ignored-result).
pub fn fn_name(fn_decl: &ResolvedNode) -> Option<String> {
    fn_decl
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

/// If `expr_stmt` is a bare dropped call `name(...)` whose callee resolves to a
/// binding DECLARED in this file — a LOCAL/UPVALUE, or a MODULE-SCOPE user-global
/// (a top-level `fn`, the common case) — returns `(name, call_node)`. Used by
/// unawaited-future and ignored-result to find a dropped call to a file-declared
/// function (so they can look up its declared return type). The returned call node
/// lets each rule compute `code_range(&call)` for its diagnostic.
pub fn dropped_local_call(
    expr_stmt: &ResolvedNode,
    resolved: &ResolveResult,
) -> Option<(String, ResolvedNode)> {
    let call = dropped_call(expr_stmt)?;
    let callee = call.children().find(|c| c.kind() == SyntaxKind::NameRef)?;
    let name = crate::syntax::resolve::ident_text(callee).unwrap_or_default();
    let resolution = resolved.uses.get(&callee.text_range());
    let file_declared = match resolution {
        Some(Resolution::Local(_) | Resolution::Upvalue(_)) => true,
        // A module-scope user-global callee is file-declared iff its name has a
        // binding recorded for this file (a top-level `fn`/`let`/… — NOT a bare
        // builtin, which has no binding).
        Some(Resolution::Global(gname)) => resolved
            .bindings
            .iter()
            .any(|b| b.is_global && &b.name == gname),
        _ => false,
    };
    if !file_declared {
        return None;
    }
    Some((name, call))
}

/// The arity RANGE of a parameter list: `min` leading required params (no
/// default), `max` total positional params, or `None` when a `...rest` makes it
/// unbounded. Shared by `call_arity`'s fn/method/constructor/std checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Arity {
    pub min: usize,
    pub max: Option<usize>,
}

/// Compute the [`Arity`] of a `ParamList` CST node. `min` is the leading run of
/// POSITIONAL params with no default (SP2 §2: a required param may not follow a
/// defaulted one); `max` is the positional param count, or `None` when a `...rest`
/// param makes it unbounded. A param is a rest iff it carries a `DotDotDot` token;
/// it has a default iff it has an EXPRESSION child (the `= expr`, distinct from
/// its TYPE child).
pub(crate) fn arity_of(param_list: &ResolvedNode) -> Arity {
    use SyntaxKind::*;
    let mut min = 0usize;
    let mut positional = 0usize;
    let mut seen_default = false;
    for p in param_list.children().filter(|c| c.kind() == Param) {
        let is_rest = p
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == DotDotDot);
        if is_rest {
            return Arity { min, max: None }; // variadic — max unbounded
        }
        positional += 1;
        let has_default = p.children().any(|c| is_expr_kind(c.kind()));
        if has_default {
            seen_default = true;
        } else if !seen_default {
            min += 1;
        }
    }
    Arity {
        min,
        max: Some(positional),
    }
}

/// The [`Arity`] of a function/method/init declaration (a node that may contain a
/// `ParamList`). A declaration with no `ParamList` is zero-arity.
pub(crate) fn decl_arity(decl: &ResolvedNode) -> Arity {
    match decl.children().find(|c| c.kind() == SyntaxKind::ParamList) {
        Some(list) => arity_of(list),
        None => Arity {
            min: 0,
            max: Some(0),
        },
    }
}

/// Does the callee `NameRef` resolve to the GENUINE unique binding of `name`,
/// of kind `kind`, declared at `decl_range`? True iff the resolver maps this use
/// to an in-file/global binding AND there is exactly ONE binding of that name,
/// which is a `kind` binding at exactly `decl_range`. A shadowing `let`/`const`/
/// param produces a second (different-kind/range) binding, so the call is
/// correctly skipped — the zero-false-positive uniqueness gate shared by
/// `call_arity` (fn/class) and `contract` (fn). Mirrors the original
/// per-rule `resolves_to_fn`/`resolves_to_class` exactly.
pub(crate) fn resolves_to_unique(
    callee: &ResolvedNode,
    name: &str,
    decl_range: cstree::text::TextRange,
    kind: crate::syntax::resolve::types::BindingKind,
    resolved: &ResolveResult,
) -> bool {
    // The use must resolve to *some* in-file/global binding (not Unresolved/builtin).
    let bound = match resolved.uses.get(&callee.text_range()) {
        Some(Resolution::Local(_) | Resolution::Upvalue(_)) => true,
        Some(Resolution::Global(gname)) => resolved
            .bindings
            .iter()
            .any(|b| b.is_global && b.name == *gname),
        _ => false,
    };
    if !bound {
        return false;
    }
    // Exactly one binding of that name, which must be the expected decl.
    let mut same_name = resolved.bindings.iter().filter(|b| b.name == name);
    let Some(only) = same_name.next() else {
        return false;
    };
    if same_name.next().is_some() {
        return false; // ambiguous: the name is bound more than once (shadowing)
    }
    only.kind == kind && only.decl_range == decl_range
}

/// The CST expression kinds that can appear in an expression position. Mirrors
/// `is_expr_kind` in `src/compile/mod.rs` for the cases the checker recurses into.
/// Shared by the rules that need to pick out the expression children of a node
/// (e.g. `range_step` operands, `call_arity` positional args).
pub(crate) fn is_expr_kind(k: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        k,
        Literal
            | NameRef
            | UnaryExpr
            | BinaryExpr
            | ParenExpr
            | CallExpr
            | MemberExpr
            | IndexExpr
            | ArrowExpr
            | AssignExpr
            | ArrayExpr
            | ObjectExpr
            | TemplateExpr
            | OptMemberExpr
            | TryExpr
            | UnwrapExpr
            | TernaryExpr
            | AwaitExpr
            | YieldExpr
            | MatchExpr
            | RangeExpr
    )
}

/// The primitive kind of a literal expression node, for literal-vs-type
/// compatibility checks. Shared by `contract` (argument literals) and
/// `field_default_type` (field default literals). Returns `None` for any
/// non-literal expression (a call, name, arithmetic, array/object, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LitKind {
    Number,
    String,
    Bool,
    Nil,
}

/// Result of asking "could this literal satisfy this type?":
/// `Yes` = definitely accepts, `No` = definitely rejects (the only thing a rule
/// flags), `Unknown` = can't tell (any / named class / generic / partial union).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Compat {
    Yes,
    No,
    Unknown,
}

/// Display name of a literal kind, for diagnostic messages.
pub(crate) fn lit_name(lit: LitKind) -> &'static str {
    match lit {
        LitKind::Number => "number",
        LitKind::String => "string",
        LitKind::Bool => "bool",
        LitKind::Nil => "nil",
    }
}

/// Is `kind` a type-annotation CST node?
///
/// Includes the TYPE §6 generics nodes (`ParamType` — a generic type-parameter
/// reference; `FnType` — a parameterized `fn(A) -> B`) so child-type filters pick
/// them up. The legacy literal-compat rules treat both as `Unknown` (silent).
pub(crate) fn is_type_kind(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        NamedType | GenericType | OptionalType | UnionType | TupleType | ParamType | FnType
    )
}

/// The primitive kind of an expression IF it is a literal (number/string/bool/nil
/// literal, including a `TemplateExpr` which is always a string). `None` otherwise.
pub(crate) fn literal_kind(expr: &ResolvedNode) -> Option<LitKind> {
    use SyntaxKind::*;
    match expr.kind() {
        TemplateExpr => Some(LitKind::String),
        Literal => {
            let t = expr
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .find(|t| !t.kind().is_trivia())?;
            Some(match t.kind() {
                Number => LitKind::Number,
                Str => LitKind::String,
                TrueKw | FalseKw => LitKind::Bool,
                NilKw => LitKind::Nil,
                _ => return None,
            })
        }
        _ => None,
    }
}

/// Is the literal PROVABLY incompatible with the (possibly composite) type?
/// `Yes` = definitely accepts; `No` = definitely rejects (the only thing a rule
/// flags); `Unknown` = can't tell (any / named class / generic / partial union).
pub(crate) fn type_compat(ty: &ResolvedNode, lit: LitKind) -> Compat {
    use SyntaxKind::*;
    match ty.kind() {
        NamedType => match ty.text().to_string().trim() {
            "any" => Compat::Yes,
            "number" => prim_compat(lit, LitKind::Number),
            "string" => prim_compat(lit, LitKind::String),
            "bool" => prim_compat(lit, LitKind::Bool),
            "nil" => prim_compat(lit, LitKind::Nil),
            _ => Compat::Unknown, // a class / named type — unknowable from a literal
        },
        OptionalType => {
            if lit == LitKind::Nil {
                Compat::Yes // T? accepts nil
            } else if let Some(inner) = ty.children().find(|c| is_type_kind(c.kind())) {
                type_compat(inner, lit)
            } else {
                Compat::Unknown
            }
        }
        UnionType => {
            let members: Vec<_> = ty.children().filter(|c| is_type_kind(c.kind())).collect();
            let mut all_no = !members.is_empty();
            for m in &members {
                match type_compat(m, lit) {
                    Compat::Yes => return Compat::Yes, // any member accepts → accepts
                    Compat::Unknown => all_no = false, // a member might accept → uncertain
                    Compat::No => {}
                }
            }
            if all_no {
                Compat::No
            } else {
                Compat::Unknown
            }
        }
        // array<T> / map / tuple / future: a scalar literal *could* be wrong, but
        // proving it requires more than a literal kind → stay silent.
        GenericType | TupleType => Compat::Unknown,
        _ => Compat::Unknown,
    }
}

/// A known-primitive annotation: matches the expected kind → Yes, else No
/// (every `LitKind` is a concrete primitive, so a mismatch is provable).
fn prim_compat(lit: LitKind, expected: LitKind) -> Compat {
    if lit == expected {
        Compat::Yes
    } else {
        Compat::No
    }
}

/// Byte span of `node` starting at its first *non-trivia* token (a CST node's
/// `text_range()` begins at any leading whitespace/comment/newline trivia, which
/// would misattribute a diagnostic — and its inline `ascript-ignore` suppression —
/// to the *previous* source line). Falls back to the full range if (impossibly)
/// there is no inner token.
pub fn code_range(node: &ResolvedNode) -> ByteSpan {
    let full = ByteSpan::from(node.text_range());
    let start = node
        .descendants_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .find(|t| !t.kind().is_trivia())
        .map(|t| usize::from(t.text_range().start()))
        .unwrap_or(full.start);
    ByteSpan {
        start,
        end: full.end,
    }
}
