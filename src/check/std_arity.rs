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
//! Only functions whose REQUIRED-arg count is statically certain get a row.
//! Variadic / optional-trailing-arg / overloaded fns are simply absent (the
//! lookup returns `None`, and the call is skipped).

use crate::check::rules::Arity;

/// The required-arg arity of a `std/*` native function, or `None` when the
/// function is not in the curated table (variadic / optional / unknown → skip).
///
/// The returned `Arity` always has `max = None`: see the module docs — only a
/// below-`min` (too-few) call is a guaranteed runtime panic; surplus args are
/// silently ignored by native fns, so a too-many call must never be flagged.
pub(crate) fn std_fn_arity(module: &str, name: &str) -> Option<Arity> {
    let min = required_args(module, name)?;
    Some(Arity { min, max: None })
}

/// The number of REQUIRED positional args of a curated `std/*` fixed-arity fn.
/// Each entry is cross-checked against the real module export by the drift-guard
/// `#[test]` below. Conservative: only fns whose required count is unambiguous
/// and stable are listed; everything else is omitted (→ skipped, never flagged).
fn required_args(module: &str, name: &str) -> Option<usize> {
    let n = match (module, name) {
        // std/math — single-number transforms (one required arg each).
        ("std/math", "abs") => 1,
        ("std/math", "floor") => 1,
        ("std/math", "ceil") => 1,
        ("std/math", "round") => 1,
        ("std/math", "trunc") => 1,
        ("std/math", "sign") => 1,
        ("std/math", "sqrt") => 1,
        // std/math — two required numeric args.
        ("std/math", "pow") => 2,
        // std/math — NUM §4 int → int helpers (fixed required arity).
        ("std/math", "floordiv") => 2,
        ("std/math", "divmod") => 2,
        ("std/math", "ceildiv") => 2,
        ("std/math", "popcount") => 1,
        ("std/math", "leading_zeros") => 1,
        ("std/math", "trailing_zeros") => 1,
        ("std/math", "rotl") => 2,
        ("std/math", "rotr") => 2,
        // std/caps — the capability surface (FFI §5.2). `list`/`dropAll` take no
        // args; `has`/`drop` take exactly one (the capability name).
        ("std/caps", "has") => 1,
        ("std/caps", "list") => 0,
        ("std/caps", "drop") => 1,
        ("std/caps", "dropAll") => 0,
        // SRV §4: std/shared — `freeze`/`isShared` each take exactly one value.
        ("std/shared", "freeze") => 1,
        ("std/shared", "isShared") => 1,
        // std/ffi — the FFI surface (FFI §5.1). `open`/`cstr`/`read_cstr`/`struct`/
        // `alloc` take one required arg; `get` takes (layout, buf, name); `set` takes
        // (layout, buf, name, value). The handle METHODS `symbol` (name + argtypes +
        // rettype) and `call` (args array) bind on a `ForeignLib`/`ForeignSymbol`
        // handle — NOT module exports, so the drift guard skips them (below).
        ("std/ffi", "open") => 1,
        ("std/ffi", "struct") => 1,
        ("std/ffi", "cstr") => 1,
        ("std/ffi", "read_cstr") => 1,
        ("std/ffi", "alloc") => 1,
        ("std/ffi", "get") => 3,
        ("std/ffi", "set") => 4,
        ("std/ffi", "symbol") => 3,
        ("std/ffi", "call") => 1,
        // std/task — pipe requires exactly 2 args (generator + event bus).
        ("std/task", "pipe") => 2,
        // std/string — NUM code-point helpers (fixed required arity).
        ("std/string", "codepoints") => 1,
        ("std/string", "from_codepoints") => 1,
        ("std/string", "code_at") => 2,
        _ => return None,
    };
    Some(n)
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
        assert!(std_fn_arity("std/math", "not_a_fn").is_none());
        assert!(std_fn_arity("std/totally-unknown", "x").is_none());
        // A genuinely variadic/optional std fn is intentionally absent.
        assert!(std_fn_arity("std/math", "max").is_none());
    }

    /// Drift guard: every keyed `(module, name)` must be a REAL export of that
    /// module per `crate::stdlib::std_module_exports`. (Answers design Q2 with
    /// the cross-check-test option.) Skips a module the current feature config
    /// does not build — `std/math` is core, so it is always present.
    #[test]
    fn every_entry_is_a_real_export() {
        // The exhaustive list of curated keys (kept in sync with required_args).
        let keys: &[(&str, &str)] = &[
            ("std/math", "abs"),
            ("std/math", "floor"),
            ("std/math", "ceil"),
            ("std/math", "round"),
            ("std/math", "trunc"),
            ("std/math", "sign"),
            ("std/math", "sqrt"),
            ("std/math", "pow"),
            ("std/math", "floordiv"),
            ("std/math", "divmod"),
            ("std/math", "ceildiv"),
            ("std/math", "popcount"),
            ("std/math", "leading_zeros"),
            ("std/math", "trailing_zeros"),
            ("std/math", "rotl"),
            ("std/math", "rotr"),
            ("std/caps", "has"),
            ("std/caps", "list"),
            ("std/caps", "drop"),
            ("std/caps", "dropAll"),
            ("std/ffi", "open"),
            ("std/ffi", "struct"),
            ("std/ffi", "cstr"),
            ("std/ffi", "read_cstr"),
            ("std/ffi", "alloc"),
            ("std/ffi", "get"),
            ("std/ffi", "set"),
            ("std/ffi", "symbol"),
            ("std/ffi", "call"),
            ("std/task", "pipe"),
            ("std/string", "codepoints"),
            ("std/string", "from_codepoints"),
            ("std/string", "code_at"),
        ];
        // FFI handle METHODS (resolved on a `ForeignLib`/`ForeignSymbol` handle, not
        // module-level exports). Keyed in `required_args` so `call-arity` can reach
        // `lib.symbol(...)` / `sym.call(...)` (Gate-5), but NOT in `std_module_exports`,
        // so the export cross-check skips them.
        let handle_methods: &[(&str, &str)] = &[("std/ffi", "symbol"), ("std/ffi", "call")];
        for (module, name) in keys {
            // The entry must actually be in the table.
            assert!(
                required_args(module, name).is_some(),
                "{module}::{name} is in the drift-guard list but not in required_args"
            );
            if handle_methods.contains(&(module, name)) {
                continue; // a handle method — no module export to cross-check.
            }
            // And it must be a real export (only checkable for built modules).
            if let Some(exports) = crate::stdlib::std_module_exports(module) {
                assert!(
                    exports.iter().any(|(n, _)| n == name),
                    "std_arity lists {module}::{name} but it is not an export of {module}"
                );
            }
        }
    }
}
