//! Occurs-checked union-find unification for local generic inference (TYPE §4.3).
//!
//! A small, local (NOT whole-program Hindley–Milner) constraint solver: at a generic
//! call/construction we **freshen** the declaration's type params to fresh `Var`s,
//! **unify** each `synth(arg)` against the freshened param type, then **substitute**
//! the solution into the return type before the normal `assignable` check.
//!
//! The cardinal Gate-5 invariant lives here: **the unifier NEVER manufactures a
//! `No`.** A non-unification (a concrete clash, an arity mismatch, the occurs-check
//! firing, or the depth-cap tripping) is a *gradual give-up* — it leaves the relevant
//! vars unsolved, and an unsolved `Var` substitutes to `Any` (the gradual leaf). The
//! provable `No` only ever comes LATER, from the normal `assignable` of a SOLVED param
//! type (§4.3 step 4). The occurs-check is the infinite-type / non-termination guard:
//! a reachable infinite-type attempt is a clean give-up, never a hang or stack blow.

use crate::check::infer::ty::{is_template_var, CheckTy, VarId, TEMPLATE_VAR_BASE};
use std::collections::HashMap;

/// Depth cap on unification recursion — reuses the `ty.rs` constructor depth cap
/// (8). Past the cap, `unify` gives up (gradual), never recurses unboundedly.
const UNIFY_DEPTH_CAP: usize = 8;

/// A concrete type in the NUM numeric tower (`int`/`float` ⊆ `number`). Used by the
/// numeric-join rescue so an over-constrained type var widens to `number`.
fn is_numeric(t: &CheckTy) -> bool {
    matches!(t, CheckTy::Int | CheckTy::Float | CheckTy::Number)
}

/// The union-find unifier + fresh-var allocator. One `Solver` per instantiation
/// context (a single generic call/construction).
#[derive(Debug, Default)]
pub struct Solver {
    /// Var bindings (the union-find store). A `Var` id maps to the type it was bound
    /// to (which may itself be / contain another `Var`). Absent ⇒ unsolved.
    bindings: HashMap<VarId, CheckTy>,
    /// Monotonic fresh-var counter (the LOW half of the id space — distinct from the
    /// reserved high "template" half, so a fresh var never collides with a template).
    next_fresh: VarId,
}

impl Solver {
    pub fn new() -> Solver {
        Solver {
            bindings: HashMap::new(),
            next_fresh: 1, // 0 reserved as a "no var" sentinel; fresh ids start at 1
        }
    }

    /// Allocate a fresh (solvable) type variable id from the low half of the space.
    pub fn fresh(&mut self) -> VarId {
        let id = self.next_fresh;
        // Stay strictly below the template base so fresh and template spaces are
        // disjoint (if we ever exhausted the low half we'd wrap — astronomically
        // unreachable for a single file's inference).
        self.next_fresh = self.next_fresh.wrapping_add(1) & (TEMPLATE_VAR_BASE - 1);
        if self.next_fresh == 0 {
            self.next_fresh = 1;
        }
        id
    }

    /// **Freshen** a signature type: replace every TEMPLATE `Var` (a declaration-
    /// context type-param reference, id in the high half) with a per-context fresh
    /// `Var`, reusing one fresh var per distinct template id so the signature's
    /// repeated `T` stays one variable (TYPE §4.3 step 1). `mapping` carries the
    /// template→fresh assignment so several types of the SAME signature (params +
    /// return) freshen consistently — pass the SAME `mapping` for all of them.
    pub fn freshen(&mut self, ty: &CheckTy, mapping: &mut HashMap<VarId, VarId>) -> CheckTy {
        match ty {
            CheckTy::Var(id, bound) => {
                if is_template_var(*id) {
                    let fresh = *mapping.entry(*id).or_insert_with(|| self.fresh());
                    let bound = bound
                        .as_ref()
                        .map(|b| Box::new(self.freshen(b, mapping)));
                    CheckTy::Var(fresh, bound)
                } else {
                    // Already a fresh var (or a foreign one) — keep as-is.
                    ty.clone()
                }
            }
            CheckTy::Array(inner) => CheckTy::Array(Box::new(self.freshen(inner, mapping))),
            CheckTy::Future(inner) => CheckTy::Future(Box::new(self.freshen(inner, mapping))),
            CheckTy::Result(inner) => CheckTy::Result(Box::new(self.freshen(inner, mapping))),
            CheckTy::Map(k, v) => CheckTy::Map(
                Box::new(self.freshen(k, mapping)),
                Box::new(self.freshen(v, mapping)),
            ),
            CheckTy::Tuple(ms) => {
                CheckTy::Tuple(ms.iter().map(|m| self.freshen(m, mapping)).collect())
            }
            CheckTy::Union(ms) => {
                CheckTy::Union(ms.iter().map(|m| self.freshen(m, mapping)).collect())
            }
            CheckTy::FnSig(ps, ret) => CheckTy::FnSig(
                ps.iter().map(|p| self.freshen(p, mapping)).collect(),
                Box::new(self.freshen(ret, mapping)),
            ),
            CheckTy::ClassApp(id, args) => {
                CheckTy::ClassApp(*id, args.iter().map(|a| self.freshen(a, mapping)).collect())
            }
            CheckTy::EnumApp(id, args) => {
                CheckTy::EnumApp(*id, args.iter().map(|a| self.freshen(a, mapping)).collect())
            }
            // Scalars / nominal-by-id / interface — no nested vars to freshen.
            other => other.clone(),
        }
    }

    /// **Resolve** a type by following any var bindings to a representative: a bound
    /// `Var` is replaced by (the resolution of) its binding; an unbound `Var` stays a
    /// `Var`. Shallow — does NOT recurse into container components (use `substitute`
    /// for a full apply). Depth-bounded against a binding cycle (cannot happen given
    /// the occurs-check, but defensive).
    fn resolve_shallow(&self, ty: &CheckTy) -> CheckTy {
        let mut cur = ty.clone();
        let mut steps = 0;
        while let CheckTy::Var(id, _) = &cur {
            match self.bindings.get(id) {
                Some(bound) => {
                    cur = bound.clone();
                    steps += 1;
                    if steps > 64 {
                        break; // defensive — never spin
                    }
                }
                None => break,
            }
        }
        cur
    }

    /// **Unify** two types, recording var bindings. Returns `true` on success, `false`
    /// on a non-unification (a concrete clash / arity / occurs-check / depth-cap). A
    /// `false` is a GRADUAL GIVE-UP — it NEVER produces a diagnostic here; the caller
    /// simply leaves the relevant vars unsolved (→ `Any` on substitution).
    pub fn unify(&mut self, a: &CheckTy, b: &CheckTy) -> bool {
        self.unify_depth(a, b, 0)
    }

    fn unify_depth(&mut self, a: &CheckTy, b: &CheckTy, depth: usize) -> bool {
        use CheckTy::*;
        if depth > UNIFY_DEPTH_CAP {
            return false; // depth-cap give-up (gradual)
        }
        // Capture any var IDENTITY on each side BEFORE resolving (resolve_shallow
        // collapses a bound var to its concrete), so an OVER-CONSTRAINED numeric var can
        // be widened to its join rather than left stale-bound — see the numeric-join
        // rescue at the concrete give-up below.
        let a_var = if let Var(v, _) = a {
            Some(self.binding_chain_end(*v))
        } else {
            None
        };
        let b_var = if let Var(v, _) = b {
            Some(self.binding_chain_end(*v))
        } else {
            None
        };
        let a = self.resolve_shallow(a);
        let b = self.resolve_shallow(b);

        // Var on either side: bind it (after the occurs-check).
        match (&a, &b) {
            (Var(va, _), Var(vb, _)) if va == vb => return true, // same var
            (Var(v, _), _) => return self.bind(*v, &b, depth),
            (_, Var(v, _)) => return self.bind(*v, &a, depth),
            _ => {}
        }

        // Any side `Any` succeeds vacuously (gradual — §4.3).
        if matches!(a, Any) || matches!(b, Any) {
            return true;
        }

        // Structural unification of like constructors (same head + arity).
        match (&a, &b) {
            (Array(x), Array(y))
            | (Future(x), Future(y))
            | (Result(x), Result(y)) => self.unify_depth(x, y, depth + 1),
            (Map(k1, v1), Map(k2, v2)) => {
                self.unify_depth(k1, k2, depth + 1) && self.unify_depth(v1, v2, depth + 1)
            }
            (Tuple(xs), Tuple(ys)) | (Union(xs), Union(ys)) => {
                xs.len() == ys.len()
                    && xs
                        .iter()
                        .zip(ys.iter())
                        .all(|(x, y)| self.unify_depth(x, y, depth + 1))
            }
            (FnSig(p1, r1), FnSig(p2, r2)) => {
                p1.len() == p2.len()
                    && p1
                        .iter()
                        .zip(p2.iter())
                        .all(|(x, y)| self.unify_depth(x, y, depth + 1))
                    && self.unify_depth(r1, r2, depth + 1)
            }
            (ClassApp(c1, a1), ClassApp(c2, a2)) | (EnumApp(c1, a1), EnumApp(c2, a2)) => {
                c1 == c2
                    && a1.len() == a2.len()
                    && a1
                        .iter()
                        .zip(a2.iter())
                        .all(|(x, y)| self.unify_depth(x, y, depth + 1))
            }
            // Two identical concretes unify; distinct concretes do NOT (gradual
            // give-up — a *constraint failure*, not a diagnostic).
            (x, y) => {
                if x == y {
                    return true;
                }
                // Numeric-join rescue (§4.2): a type var constrained to two DISTINCT
                // concrete numerics (e.g. `int` then `float`, from `max(1, 2.0)`) widens
                // to `number` — their join — instead of staying stale-bound, which would
                // otherwise manufacture a FALSE blocking `type-mismatch` on sound,
                // type-erased code. A NON-numeric conflict (`int` vs `string`) still
                // gives up, leaving the stale binding so the genuine "T can't be both"
                // mismatch is still reported. Only fires when a var is actually involved
                // (two raw concretes with no var stay a plain give-up).
                if is_numeric(x) && is_numeric(y) {
                    let mut rescued = false;
                    if let Some(v) = a_var {
                        self.bindings.insert(v, CheckTy::Number);
                        rescued = true;
                    }
                    if let Some(v) = b_var {
                        self.bindings.insert(v, CheckTy::Number);
                        rescued = true;
                    }
                    if rescued {
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Follow a var→var binding chain to its END var (the one bound to a concrete or
    /// still unbound) — the var whose binding the numeric-join rescue must widen.
    fn binding_chain_end(&self, mut v: VarId) -> VarId {
        let mut steps = 0;
        while let Some(CheckTy::Var(w, _)) = self.bindings.get(&v) {
            v = *w;
            steps += 1;
            if steps > 64 {
                break; // defensive — never spin
            }
        }
        v
    }

    /// Bind `v := t` after the OCCURS-CHECK. If `v` occurs anywhere inside `t` (a
    /// reachable infinite type, e.g. `T = array<T>`), the binding is REJECTED →
    /// returns `false` (a gradual give-up — the var stays unsolved, NO hang, NO
    /// stack blow-up). This is the single most important termination guard.
    fn bind(&mut self, v: VarId, t: &CheckTy, _depth: usize) -> bool {
        // Binding a var to itself is a no-op success.
        if let CheckTy::Var(w, _) = t {
            if *w == v {
                return true;
            }
        }
        if self.occurs(v, t) {
            return false; // occurs-check failed → reject (infinite type)
        }
        self.bindings.insert(v, t.clone());
        true
    }

    /// Does var `v` occur (transitively, through any current bindings) inside `t`?
    /// The occurs-check — bounded by structure + a small step budget so even a
    /// pathological binding graph terminates.
    fn occurs(&self, v: VarId, t: &CheckTy) -> bool {
        self.occurs_budget(v, t, &mut 4096)
    }

    fn occurs_budget(&self, v: VarId, t: &CheckTy, budget: &mut u32) -> bool {
        use CheckTy::*;
        if *budget == 0 {
            return true; // out of budget → assume occurs (conservative reject = give-up)
        }
        *budget -= 1;
        match t {
            Var(w, bound) => {
                if *w == v {
                    return true;
                }
                // Follow a binding (so `v` occurring behind another var is caught).
                if let Some(bound_to) = self.bindings.get(w) {
                    if self.occurs_budget(v, bound_to, budget) {
                        return true;
                    }
                }
                bound
                    .as_ref()
                    .is_some_and(|b| self.occurs_budget(v, b, budget))
            }
            Array(x) | Future(x) | Result(x) => self.occurs_budget(v, x, budget),
            Map(k, val) => {
                self.occurs_budget(v, k, budget) || self.occurs_budget(v, val, budget)
            }
            Tuple(ms) | Union(ms) => ms.iter().any(|m| self.occurs_budget(v, m, budget)),
            FnSig(ps, ret) => {
                ps.iter().any(|p| self.occurs_budget(v, p, budget))
                    || self.occurs_budget(v, ret, budget)
            }
            ClassApp(_, args) | EnumApp(_, args) => {
                args.iter().any(|a| self.occurs_budget(v, a, budget))
            }
            _ => false,
        }
    }

    /// **Substitute** the current solution into `ty`: every bound `Var` is replaced by
    /// its (recursively substituted) solution; every UNSOLVED `Var` substitutes to
    /// `Any` (the gradual leaf — TYPE §4.2). The result contains no `Var`. Depth-
    /// bounded; a leftover deep nest past the cap collapses to `Any` (gradual).
    pub fn substitute(&self, ty: &CheckTy) -> CheckTy {
        self.subst_depth(ty, 0)
    }

    fn subst_depth(&self, ty: &CheckTy, depth: usize) -> CheckTy {
        use CheckTy::*;
        if depth > UNIFY_DEPTH_CAP {
            return Any; // past the cap → gradual
        }
        match ty {
            Var(id, _) => match self.bindings.get(id) {
                Some(bound) => self.subst_depth(bound, depth + 1),
                None => Any, // unsolved var → Any (gradual leaf)
            },
            Array(x) => Array(Box::new(self.subst_depth(x, depth + 1))),
            Future(x) => Future(Box::new(self.subst_depth(x, depth + 1))),
            Result(x) => Result(Box::new(self.subst_depth(x, depth + 1))),
            Map(k, val) => Map(
                Box::new(self.subst_depth(k, depth + 1)),
                Box::new(self.subst_depth(val, depth + 1)),
            ),
            Tuple(ms) => Tuple(ms.iter().map(|m| self.subst_depth(m, depth + 1)).collect()),
            Union(ms) => Union(ms.iter().map(|m| self.subst_depth(m, depth + 1)).collect()),
            FnSig(ps, ret) => FnSig(
                ps.iter().map(|p| self.subst_depth(p, depth + 1)).collect(),
                Box::new(self.subst_depth(ret, depth + 1)),
            ),
            ClassApp(id, args) => ClassApp(
                *id,
                args.iter().map(|a| self.subst_depth(a, depth + 1)).collect(),
            ),
            EnumApp(id, args) => EnumApp(
                *id,
                args.iter().map(|a| self.subst_depth(a, depth + 1)).collect(),
            ),
            other => other.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::infer::ty::{param_template_id, CheckTy};
    use std::collections::HashMap;

    fn tv(id: VarId) -> CheckTy {
        CheckTy::Var(id, None)
    }

    #[test]
    fn unify_var_binds_to_concrete() {
        let mut s = Solver::new();
        let v = s.fresh();
        assert!(s.unify(&tv(v), &CheckTy::Int));
        assert_eq!(s.substitute(&tv(v)), CheckTy::Int);
    }

    #[test]
    fn unify_same_constructor_componentwise() {
        let mut s = Solver::new();
        let v = s.fresh();
        // array<v> unifies against array<string> → v := string.
        assert!(s.unify(
            &CheckTy::Array(Box::new(tv(v))),
            &CheckTy::Array(Box::new(CheckTy::String))
        ));
        assert_eq!(s.substitute(&tv(v)), CheckTy::String);
    }

    #[test]
    fn unify_classapp_componentwise() {
        let mut s = Solver::new();
        let v = s.fresh();
        // Box<v> vs Box<int> (head id 7, arbitrary) → v := int.
        assert!(s.unify(
            &CheckTy::ClassApp(7, vec![tv(v)]),
            &CheckTy::ClassApp(7, vec![CheckTy::Int])
        ));
        assert_eq!(s.substitute(&tv(v)), CheckTy::Int);
        // Different head → no unify (gradual give-up).
        let mut s2 = Solver::new();
        let w = s2.fresh();
        assert!(!s2.unify(
            &CheckTy::ClassApp(7, vec![tv(w)]),
            &CheckTy::ClassApp(8, vec![CheckTy::Int])
        ));
    }

    #[test]
    fn occurs_check_rejects_infinite_type_no_hang() {
        let mut s = Solver::new();
        let v = s.fresh();
        // unify(v, array<v>) — the occurs-check must REJECT (return false), not hang.
        let recursive = CheckTy::Array(Box::new(tv(v)));
        assert!(!s.unify(&tv(v), &recursive));
        // v stays unsolved → substitutes to Any (gradual).
        assert_eq!(s.substitute(&tv(v)), CheckTy::Any);
    }

    #[test]
    fn occurs_check_deep_nesting_terminates() {
        // A deeply-nested self-reference still terminates cleanly.
        let mut s = Solver::new();
        let v = s.fresh();
        let mut t = tv(v);
        for _ in 0..50 {
            t = CheckTy::Array(Box::new(t));
        }
        assert!(!s.unify(&tv(v), &t)); // rejected, no hang/overflow
        assert_eq!(s.substitute(&tv(v)), CheckTy::Any);
    }

    #[test]
    fn any_side_unifies_vacuously() {
        let mut s = Solver::new();
        assert!(s.unify(&CheckTy::Any, &CheckTy::Int));
        assert!(s.unify(&CheckTy::String, &CheckTy::Any));
    }

    #[test]
    fn numeric_join_widens_over_constrained_var() {
        // Regression (TYPE Unit-C review B1): a single type var constrained by two
        // distinct NUMERICS — `int` then `float`, as in `max(1, 2.0)` — widens to
        // `number` (their join) instead of staying stale-bound to `int`, which would
        // otherwise manufacture a FALSE blocking `type-mismatch` on sound erased code.
        let mut s = Solver::new();
        let v = s.fresh();
        assert!(s.unify(&tv(v), &CheckTy::Int)); // arg0 → v := int
        assert!(s.unify(&tv(v), &CheckTy::Float)); // arg1 → widen v to number (NOT give-up)
        assert_eq!(s.substitute(&tv(v)), CheckTy::Number);
        // Order-independent: float then int also widens to number.
        let mut s2 = Solver::new();
        let w = s2.fresh();
        assert!(s2.unify(&tv(w), &CheckTy::Float));
        assert!(s2.unify(&tv(w), &CheckTy::Int));
        assert_eq!(s2.substitute(&tv(w)), CheckTy::Number);
    }

    #[test]
    fn non_numeric_conflict_does_not_widen() {
        // The numeric-join rescue must NOT swallow a genuine "T can't be both" conflict:
        // a var constrained to `int` then `string` gives up (leaving the binding) so the
        // downstream `assignable` still reports the real mismatch.
        let mut s = Solver::new();
        let v = s.fresh();
        assert!(s.unify(&tv(v), &CheckTy::Int));
        assert!(!s.unify(&tv(v), &CheckTy::String)); // give-up, NOT widened
        assert_eq!(s.substitute(&tv(v)), CheckTy::Int);
    }

    #[test]
    fn two_raw_numerics_without_a_var_do_not_unify() {
        // The rescue only fires when a VAR is involved; two raw concrete numerics with
        // no var stay a plain give-up (no spurious success, nothing to widen).
        let mut s = Solver::new();
        assert!(!s.unify(&CheckTy::Int, &CheckTy::Float));
    }

    #[test]
    fn distinct_concretes_do_not_unify() {
        let mut s = Solver::new();
        // A constraint FAILURE (not a panic): int vs string → false.
        assert!(!s.unify(&CheckTy::Int, &CheckTy::String));
    }

    #[test]
    fn unsolved_var_substitutes_to_any() {
        let s = Solver::new();
        assert_eq!(s.substitute(&tv(99)), CheckTy::Any);
        // Inside a container, too.
        assert_eq!(
            s.substitute(&CheckTy::Array(Box::new(tv(99)))),
            CheckTy::Array(Box::new(CheckTy::Any))
        );
    }

    #[test]
    fn freshen_maps_templates_consistently() {
        let mut s = Solver::new();
        let t_id = param_template_id("T");
        // signature: fn(T) -> array<T> — the SAME template T must freshen to ONE var.
        let sig = CheckTy::FnSig(
            vec![CheckTy::Var(t_id, None)],
            Box::new(CheckTy::Array(Box::new(CheckTy::Var(t_id, None)))),
        );
        let mut map = HashMap::new();
        let fresh = s.freshen(&sig, &mut map);
        // Solve via the param, observe the return reflects it.
        let CheckTy::FnSig(ps, ret) = &fresh else {
            panic!("expected FnSig")
        };
        assert!(s.unify(&ps[0], &CheckTy::Int));
        assert_eq!(
            s.substitute(ret),
            CheckTy::Array(Box::new(CheckTy::Int))
        );
    }

    #[test]
    fn fresh_and_template_spaces_are_disjoint() {
        let mut s = Solver::new();
        for _ in 0..1000 {
            let f = s.fresh();
            assert!(!crate::check::infer::ty::is_template_var(f));
        }
    }
}
