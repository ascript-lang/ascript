//! Curated arity table for fixed-arity `std/*` native functions.
//!
//! Feature-independent (the checker core builds under `--no-default-features`):
//! this is pure DATA, not a feature-gated call into the stdlib.
//!
//! ## Zero-false-positive contract (important)
//!
//! AScript's native std functions read positional args by index and **ignore
//! extra arguments** (`arg(args, i)` returns `nil` for a missing slot but never
//! errors on a surplus). So calling a fixed-arity std fn with TOO MANY args does
//! NOT panic at runtime — only TOO FEW does (a missing required arg becomes `nil`
//! and the fn's contract check then panics, e.g. `math.abs expects a number, got
//! nil`).
//!
//! Therefore every entry here is reported with `max = None` (unbounded): the
//! `call-arity` std branch flags ONLY a below-`min` call, which is a *guaranteed*
//! runtime panic. A surplus-arg call is never flagged (it is not an error).
//!
//! ## Single source of truth (SIG §2.5)
//!
//! `std_fn_arity` is now a THIN DERIVATION over `crate::check::std_sigs::std_sig`:
//! `min` = the count of LEADING non-optional, non-variadic params; `max = None`
//! ALWAYS (the zero-false-positive contract above). The previous hardcoded
//! `required_args` match was deleted; the curated signature table in `std_sigs.rs`
//! is the one source of truth.  Handle-method entries (`ffi::symbol`/`ffi::call`,
//! docker client methods) are now rows in their module's SIGS table with
//! `MemberKind::HandleMethod` — the derivation finds them by name.
//!
//! The `every_entry_is_a_real_export` test was deleted because its export
//! cross-check coverage is now superseded by the strictly-stronger std_sigs
//! completeness pair in `tests/std_sigs_docs.rs` (Gate 7, noted in the commit).

use crate::check::rules::Arity;

/// The required-arg arity of a `std/*` native function, or `None` when the
/// function has no curated signature (unknown module / not in the table).
///
/// SIG §2.5: `min` is DERIVED as the count of LEADING non-optional,
/// non-variadic params of `crate::check::std_sigs::std_sig(module, name)`.
/// A fully-variadic fn (e.g. `math.max`) yields `Some(Arity{min:0,…})` —
/// this is correct and harmless: a call with 0 args passes the `min=0` gate
/// (no false-positive flag), while the runtime variadic validation handles the
/// actual "at least one" contract.
///
/// The returned `Arity` always has `max = None`: see the module docs — only a
/// below-`min` (too-few) call is a guaranteed runtime panic; surplus args are
/// silently ignored by native fns, so a too-many call must never be flagged.
pub(crate) fn std_fn_arity(module: &str, name: &str) -> Option<Arity> {
    let sig = crate::check::std_sigs::std_sig(module, name)?;
    let min = sig.params.iter().take_while(|p| !p.optional && !p.variadic).count();
    Some(Arity { min, max: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_have_unbounded_max() {
        // Every curated entry reports max=None (surplus args are not an error).
        let a = std_fn_arity("std/math", "abs").unwrap();
        assert_eq!(a.min, 1);
        assert_eq!(a.max, None);
        let p = std_fn_arity("std/math", "pow").unwrap();
        assert_eq!(p.min, 2);
        assert_eq!(p.max, None);
    }

    #[test]
    fn unlisted_returns_none() {
        // A truly unlisted name returns None.
        assert!(std_fn_arity("std/math", "not_a_fn").is_none());
        assert!(std_fn_arity("std/totally-unknown", "x").is_none());
        // NOTE: `std/math::max` is now covered by the derivation (it is in the
        // std_sigs table as a variadic fn) and returns Some(Arity{min:0,max:None}).
        // A min=0 entry never triggers a call-arity flag (0 args ≥ 0 required),
        // which preserves the zero-false-positive contract for variadic fns.
        let max_arity = std_fn_arity("std/math", "max");
        assert!(max_arity.is_some(), "std/math::max should be covered by std_sigs");
        assert_eq!(max_arity.unwrap().min, 0, "variadic fn has min=0");
        assert_eq!(max_arity.unwrap().max, None);
    }

    /// SIG §2.5: the std_sigs-derived arity must reproduce EVERY legacy curated
    /// arity EXACTLY (no call-arity behavior change for previously-covered fns).
    #[test]
    fn derivation_matches_every_legacy_entry() {
        let legacy: &[(&str, &str, usize)] = &[
            ("std/math","abs",1),("std/math","floor",1),("std/math","ceil",1),
            ("std/math","round",1),("std/math","trunc",1),("std/math","sign",1),
            ("std/math","sqrt",1),("std/math","pow",2),("std/math","floordiv",2),
            ("std/math","divmod",2),("std/math","ceildiv",2),("std/math","popcount",1),
            ("std/math","leading_zeros",1),("std/math","trailing_zeros",1),
            ("std/math","rotl",2),("std/math","rotr",2),
            ("std/caps","has",1),("std/caps","list",0),("std/caps","drop",1),
            ("std/caps","dropAll",0),
            ("std/shared","freeze",1),("std/shared","isShared",1),
            ("std/ffi","open",1),("std/ffi","struct",1),("std/ffi","cstr",1),
            ("std/ffi","read_cstr",1),("std/ffi","alloc",1),("std/ffi","get",3),
            ("std/ffi","set",4),("std/ffi","symbol",3),("std/ffi","call",1),
            ("std/task","pipe",2),("std/task","pmap",2),("std/task","preduce",3),
            ("std/resilience","limiter",1),("std/resilience","keyedLimiter",1),
            ("std/resilience","bulkhead",1),("std/resilience","fallback",2),
            ("std/resilience","singleflight",2),("std/resilience","deadline",2),
            ("std/resilience","withTrace",2),("std/resilience","handler",2),
            ("std/string","codepoints",1),("std/string","from_codepoints",1),
            ("std/string","code_at",2),
            ("std/assert","deepEq",2),("std/assert","matches",2),
            ("std/assert","throwsWith",2),
            ("std/net/unix","connect",1),("std/net/unix","listen",1),
            ("std/docker","inspect",1),("std/docker","start",1),("std/docker","stop",1),
            ("std/docker","restart",1),("std/docker","wait",1),("std/docker","remove",1),
            ("std/docker","removeImage",1),("std/docker","execCreate",1),
            ("std/docker","execStart",1),("std/docker","execInspect",1),
            ("std/docker","exec",1),
        ];
        for (m, n, min) in legacy {
            let a = std_fn_arity(m, n).unwrap_or_else(|| panic!("{m}::{n} lost its arity"));
            assert_eq!(a.min, *min, "{m}::{n} min drifted");
            assert_eq!(a.max, None, "{m}::{n} must keep max=None (zero-FP contract)");
        }
    }
}
