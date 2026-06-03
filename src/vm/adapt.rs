//! PEP-659-style adaptive specialization (V11-T4) for arithmetic and globals.
//!
//! Following CPython's "specializing adaptive interpreter", a hot bytecode site
//! is observed for a short WARMUP window and then *specialized* to a fast path
//! that assumes the observed operand kinds, guarded by a cheap type check. On a
//! guard MISS the site DEOPTIMIZES — it reverts to the generic path (and either
//! re-warms or gives up), always producing the byte-identical result the generic
//! path would.
//!
//! ## Why a side map, not in-place quickening
//!
//! CPython rewrites the opcode byte in place (`BINARY_OP` → `BINARY_OP_ADD_INT`).
//! AScript keeps `Chunk.code` immutable and shared (the disassembler, goldens and
//! the differential oracle all depend on byte-identical bytecode), so the
//! adaptive state lives in an OFFSET-KEYED side map on the chunk —
//! `RefCell<HashMap<usize, ArithCache>>` / `RefCell<HashMap<usize, GlobalCache>>`
//! — exactly like the V11-T3 inline caches (`field_ics`/`method_ics`). A site's
//! cache is consulted at dispatch by the op's bytecode offset; the bytecode and
//! disassembly stay BYTE-IDENTICAL (zero new inline operand). This is the
//! equivalent of in-place quickening with none of the code-mutation downsides.
//!
//! ## Correctness invariant
//!
//! The fast path is ONLY ever taken after its guard confirms the operand kinds it
//! specialized for, and it then performs the EXACT same computation the generic
//! `apply_binop` would for those kinds:
//!
//! - `ArithKind::Number` ⇒ both operands `Value::Number` ⇒ the same `f64`
//!   arithmetic `apply_binop` runs in its final numeric arm.
//! - `ArithKind::Decimal` ⇒ both operands `Value::Decimal` ⇒ the same
//!   `rust_decimal` op `apply_binop` runs (Add/Sub/Mul only — see
//!   [`ArithCache::specializable`]; never a path that can div-by-zero or coerce a
//!   non-finite Number, which require the generic fallback).
//! - `ArithKind::ConcatStr` ⇒ both operands `Value::Str` ⇒ the same
//!   `format!("{a}{b}")` concat `apply_binop` runs for `Add` on two strings.
//!
//! Any operand that fails the guard takes the generic `apply_binop` (correct
//! result) and triggers a DEOPT of the cache. So specialization can never change
//! a result or a panic message — it only skips dispatch when the kinds match.

/// Number of times a site must be observed with consistent specializable operand
/// kinds before it specializes. Matches the V11 plan's suggested warmup (8): long
/// enough to skip one-shot sites, short enough that hot loops specialize quickly.
pub const WARMUP_THRESHOLD: u16 = 8;

/// The operand-kind family an arithmetic site has specialized to. Each maps to a
/// single guarded fast path in the run loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithKind {
    /// Both operands `Value::Number` ⇒ inline `f64` arithmetic.
    Number,
    /// Both operands `Value::Decimal` ⇒ inline `rust_decimal` arithmetic.
    Decimal,
    /// Both operands `Value::Str`, op is `Add` ⇒ inline string concat.
    ConcatStr,
}

/// Adaptive state for ONE arithmetic op site, keyed by its bytecode offset.
///
/// State machine (per PEP 659):
/// - [`ArithCache::Generic`] — never specialized; carries a warmup `count` and the
///   candidate kind seen so far. The first observation records the candidate; a
///   later observation of the SAME kind increments; a DIFFERENT kind resets the
///   warmup (a polymorphic site never accumulates enough to specialize → it stays
///   generic, the correct outcome).
/// - [`ArithCache::Specialized`] — warmed up and committed to one [`ArithKind`].
///   The run loop guards the operands; a miss DEOPTs back to `Generic` (re-warm).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithCache {
    /// Not (yet) specialized. `candidate` is the kind under observation (`None`
    /// before the first observation); `count` is how many consecutive times that
    /// candidate has been seen.
    Generic {
        candidate: Option<ArithKind>,
        count: u16,
    },
    /// Committed to `kind`; the run loop takes the guarded fast path.
    Specialized { kind: ArithKind },
}

impl Default for ArithCache {
    fn default() -> Self {
        ArithCache::Generic {
            candidate: None,
            count: 0,
        }
    }
}

impl ArithCache {
    /// Observe an execution whose operands map to specializable `kind`
    /// (`Some(k)`), or to no specializable kind (`None`, e.g. a number+string
    /// mix). Advances the warmup and returns the new state.
    ///
    /// Only ever called on a `Generic` site (a `Specialized` site short-circuits
    /// through its guard and never re-observes unless it first deopts).
    pub fn observe(self, kind: Option<ArithKind>) -> ArithCache {
        match self {
            ArithCache::Specialized { .. } => self,
            ArithCache::Generic { candidate, count } => match kind {
                // A non-specializable execution (mixed/other operands) resets the
                // warmup: a genuinely polymorphic site never specializes.
                None => ArithCache::Generic {
                    candidate: None,
                    count: 0,
                },
                Some(k) if candidate == Some(k) => {
                    let next = count + 1;
                    if next >= WARMUP_THRESHOLD {
                        ArithCache::Specialized { kind: k }
                    } else {
                        ArithCache::Generic {
                            candidate: Some(k),
                            count: next,
                        }
                    }
                }
                // First observation, or a change of candidate kind: (re)start the
                // warmup at 1 for the newly-seen kind.
                Some(k) => ArithCache::Generic {
                    candidate: Some(k),
                    count: 1,
                },
            },
        }
    }

    /// Deoptimize a specialized site after a guard miss: revert to a fresh warmup
    /// so a site that drifts back to a stable kind can re-specialize, while a
    /// churning polymorphic site keeps resetting and stays generic.
    pub fn deopt(self) -> ArithCache {
        ArithCache::default()
    }

    /// The committed kind if this site is specialized, else `None`.
    #[inline]
    pub fn specialized(&self) -> Option<ArithKind> {
        match self {
            ArithCache::Specialized { kind } => Some(*kind),
            ArithCache::Generic { .. } => None,
        }
    }

    /// Whether `op` over operand kinds is eligible for the `Decimal` fast path.
    /// Decimal `Add`/`Sub`/`Mul` are total over two finite decimals (and the
    /// operands ARE finite — `Value::Decimal` is always finite); `Div`/`Mod` can
    /// panic on a zero divisor and comparisons/`Pow`/`Range` aren't simple binary
    /// arithmetic, so those stay generic.
    pub fn decimal_specializable(op: crate::ast::BinOp) -> bool {
        use crate::ast::BinOp;
        matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul)
    }
}

/// Adaptive state for ONE `GET_GLOBAL` site, keyed by its bytecode offset.
///
/// AScript's globals are the immutable bare builtins (top-level `let`s are
/// frame-locals, not globals; builtins are never reassigned), so the global set is
/// effectively CONSTANT. We still implement a VERSION guard for correctness and
/// forward-compatibility: the cache stores the resolved [`Value`] plus the global
/// version it was resolved at; a hit requires the version to match (it always
/// does today), otherwise the site re-resolves.
#[derive(Clone, Debug, Default)]
pub enum GlobalCache {
    /// Never resolved at this site.
    #[default]
    Cold,
    /// Resolved `value` for the global name at global-table `version`. A hit
    /// requires `version == current_global_version`.
    Cached {
        value: crate::value::Value,
        version: u64,
    },
}

impl GlobalCache {
    /// The cached value if present AND still valid for `version`, else `None`
    /// (cold, or a stale version ⇒ re-resolve).
    #[inline]
    pub fn get(&self, version: u64) -> Option<crate::value::Value> {
        match self {
            GlobalCache::Cold => None,
            GlobalCache::Cached { value, version: v } => {
                (*v == version).then(|| value.clone())
            }
        }
    }

    /// Record a freshly-resolved `value` at `version`.
    pub fn set(value: crate::value::Value, version: u64) -> GlobalCache {
        GlobalCache::Cached { value, version }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warms_up_then_specializes_number() {
        let mut c = ArithCache::default();
        assert_eq!(c.specialized(), None);
        // WARMUP_THRESHOLD consistent Number observations specialize.
        for _ in 0..WARMUP_THRESHOLD {
            c = c.observe(Some(ArithKind::Number));
        }
        assert_eq!(c.specialized(), Some(ArithKind::Number));
    }

    #[test]
    fn does_not_specialize_before_threshold() {
        let mut c = ArithCache::default();
        for _ in 0..(WARMUP_THRESHOLD - 1) {
            c = c.observe(Some(ArithKind::Number));
        }
        assert_eq!(c.specialized(), None, "one short of threshold stays generic");
        c = c.observe(Some(ArithKind::Number));
        assert_eq!(c.specialized(), Some(ArithKind::Number));
    }

    #[test]
    fn decimal_and_concat_specialize() {
        for kind in [ArithKind::Decimal, ArithKind::ConcatStr] {
            let mut c = ArithCache::default();
            for _ in 0..WARMUP_THRESHOLD {
                c = c.observe(Some(kind));
            }
            assert_eq!(c.specialized(), Some(kind));
        }
    }

    #[test]
    fn polymorphic_never_specializes() {
        // Alternating kinds keep resetting the warmup → never specialized.
        let mut c = ArithCache::default();
        for i in 0..(WARMUP_THRESHOLD as usize * 4) {
            let k = if i % 2 == 0 {
                ArithKind::Number
            } else {
                ArithKind::ConcatStr
            };
            c = c.observe(Some(k));
            assert_eq!(c.specialized(), None);
        }
    }

    #[test]
    fn non_specializable_observation_resets_warmup() {
        let mut c = ArithCache::default();
        for _ in 0..(WARMUP_THRESHOLD - 1) {
            c = c.observe(Some(ArithKind::Number));
        }
        // A mixed-operand execution (None) resets the warmup.
        c = c.observe(None);
        assert_eq!(c, ArithCache::default());
        assert_eq!(c.specialized(), None);
    }

    #[test]
    fn deopt_reverts_to_fresh_warmup() {
        let c = ArithCache::Specialized {
            kind: ArithKind::Number,
        };
        let d = c.deopt();
        assert_eq!(d, ArithCache::default());
        assert_eq!(d.specialized(), None);
    }

    #[test]
    fn changing_candidate_restarts_warmup_at_one() {
        let mut c = ArithCache::default();
        c = c.observe(Some(ArithKind::Number));
        c = c.observe(Some(ArithKind::Decimal));
        assert_eq!(
            c,
            ArithCache::Generic {
                candidate: Some(ArithKind::Decimal),
                count: 1
            }
        );
    }

    #[test]
    fn global_cache_version_guard() {
        use crate::value::Value;
        let cold = GlobalCache::Cold;
        assert!(cold.get(0).is_none());

        let c = GlobalCache::set(Value::Builtin("print".into()), 7);
        // Same version hits.
        match c.get(7) {
            Some(Value::Builtin(n)) => assert_eq!(&*n, "print"),
            other => panic!("expected cached print builtin, got {other:?}"),
        }
        // A bumped version invalidates the cache.
        assert!(c.get(8).is_none(), "stale version misses → re-resolve");
    }

    #[test]
    fn decimal_specializable_only_add_sub_mul() {
        use crate::ast::BinOp;
        assert!(ArithCache::decimal_specializable(BinOp::Add));
        assert!(ArithCache::decimal_specializable(BinOp::Sub));
        assert!(ArithCache::decimal_specializable(BinOp::Mul));
        assert!(!ArithCache::decimal_specializable(BinOp::Div));
        assert!(!ArithCache::decimal_specializable(BinOp::Mod));
        assert!(!ArithCache::decimal_specializable(BinOp::Lt));
    }
}
