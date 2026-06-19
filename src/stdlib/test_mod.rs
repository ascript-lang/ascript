//! `std/test` — property-testing **generator combinators** (BATT C2, spec §10.4).
//!
//! This module is the value-generator half of AScript's property-testing story
//! (the `prop()` runner lands in C3). It is **CORE** — no Cargo feature gates it
//! (like `std/assert` / `std/bench`), so property generators are available in a
//! bare `--no-default-features` build.
//!
//! ## Posture — inert tagged Objects (the `std/schema` precedent)
//!
//! A generator is an ordinary AScript Object carrying a `__gen: "<kind>"` tag plus
//! its config — the SAME inert-value posture `std/schema` uses for its
//! `{__kind:"<t>"}` validators (spec §0/§10.4). There is NO new `Value` variant: a
//! generator prints, JSON-serializes, and pattern-matches like any Object. The
//! `map`/`filter` combinators store the user fn on the Object exactly as
//! `schema.refine` stores its predicate.
//!
//! The `gen` export is a namespace Object whose fields are builtins
//! (`{int: <test.int>, float: <test.float>, …}`), so `gen.int(-10, 10)` reads the
//! `int` field (a builtin named `test.int`) and calls it — routing through
//! [`Interp::call_test`] like any other qualified stdlib call.
//!
//! ## The drawer — seeded, edge-biased, recursion-budgeted
//!
//! [`Interp::draw_gen`] turns a generator + a [`crate::det::SeededRng`] into a
//! value. It is the native engine the C3 runner draws from. Two properties matter:
//!
//! - **Edge bias (the fuzzgen philosophy, §10.4).** The internal source-program
//!   fuzzer (`src/fuzzgen/`) is deliberately edge-biased — it pulls boundary values
//!   far more often than a uniform sampler would, because bugs cluster at
//!   boundaries. This module surfaces that SAME philosophy as a *user-facing* API:
//!   for a bounded numeric or collection draw, roughly **1 draw in 4** comes from a
//!   BOUNDARY POOL (`{min, max, 0, ±1}` clamped into range) instead of the uniform
//!   draw. Over many draws the boundary set therefore appears materially more often
//!   than uniform sampling would produce — the mechanism a property test needs to
//!   actually hit corner cases.
//! - **Recursion budget.** Nested generators (`arrayOf(objectWith(...))`) recurse;
//!   the drawer carries a `depth` budget ([`MAX_GEN_DEPTH`]) and turns exceeding it
//!   into a clean Tier-2 panic — data recursion is bounded, so `stacker` is NOT
//!   needed and a malicious/accidental deeply-nested generator can never overflow
//!   the Rust stack.
//!
//! ## Error tiering (the house rule, §2.2)
//!
//! - **Tier-2 panic** (recoverable via `recover`): invalid *programmer* config — a
//!   bad combinator argument type, `min > max`, a negative length, an unknown
//!   charset, an empty `oneOf`, a malformed generator Object, or exceeding the
//!   recursion budget.
//! - **Tier-1 `[value, err]` pair**: `filter` exhausting its `maxDiscard` budget
//!   (the world — the predicate — didn't cooperate); the err names the generator so
//!   a property author can see which `filter` starved.

use super::arg;
use crate::det::SeededRng;
use crate::error::AsError;
use crate::interp::{make_error, make_pair, type_name, Control, Interp};
use crate::span::Span;
use crate::value::{OwnedKind, Value, ValueKind};
use indexmap::IndexMap;

/// The recursion budget for nested generators. A `arrayOf(objectWith(arrayOf(…)))`
/// chain deeper than this raises a clean Tier-2 panic rather than risking a stack
/// overflow. 32 is generous for any realistic generator shape (the fuzzgen budget
/// precedent) while keeping the bound far below the Rust stack ceiling.
//
// `dead_code` allow: the drawer + its primitives are the C2 deliverable consumed by
// C3's `prop()` runner (not yet landed). Exercised by this module's `#[cfg(test)]`
// suite; the allow drops away once C3 wires the runner.
#[allow(dead_code)]
pub(crate) const MAX_GEN_DEPTH: u32 = 32;

/// Default array/string length bounds (spec §10.4 — `arrayOf` opts default
/// `{minLen: 0, maxLen: 32}`; `string` similarly).
const DEFAULT_MIN_LEN: usize = 0;
const DEFAULT_MAX_LEN: usize = 32;

/// A hard cap on any drawn length so a hostile `{maxLen: 1e9}` config cannot drive
/// a giant allocation. Mirrors the `want_count` discipline (§2.2); generators are a
/// testing tool, not a bulk allocator.
const MAX_GEN_LEN: usize = 4096;

/// Default `filter` discard budget (spec §10.4 — `{maxDiscard: 100}`).
const DEFAULT_MAX_DISCARD: u32 = 100;

/// Default integer bounds when `int()` is called with no args. A wide-but-bounded
/// window so the boundary pool (`{min, max, 0, ±1}`) is always well-defined.
const DEFAULT_INT_MIN: i64 = -1_000_000;
const DEFAULT_INT_MAX: i64 = 1_000_000;

/// Default float bounds when `float()` is called with no args.
const DEFAULT_FLOAT_MIN: f64 = -1_000_000.0;
const DEFAULT_FLOAT_MAX: f64 = 1_000_000.0;

// ─────────────────────────────────────────────────────────────────────────────
// exports
// ─────────────────────────────────────────────────────────────────────────────

/// The export list for `import { ... } from "std/test"`.
///
/// `gen` is a namespace Object whose fields are the combinator builtins; the
/// individual constructors are ALSO exported flat (`gen.int` == `int`) so both
/// `import { gen }` and `import { int, arrayOf }` work.
pub fn exports() -> Vec<(&'static str, Value)> {
    // The combinator builtins, by their short name → qualified `test.<name>`.
    let combinators: &[&str] = &[
        "int",
        "float",
        "bool",
        "constant",
        "string",
        "oneOf",
        "frequency",
        "arrayOf",
        "objectWith",
        "map",
        "filter",
        "nilOr",
    ];

    // The `gen` namespace Object: { int: <test.int>, float: <test.float>, … }.
    let mut ns: IndexMap<String, Value> = IndexMap::new();
    for &name in combinators {
        ns.insert(name.to_string(), super::bi(&format!("test.{name}")));
    }
    let gen_ns = Value::object(ns);

    let mut out: Vec<(&'static str, Value)> = vec![("gen", gen_ns)];
    // Flat re-exports for ergonomics (and so the std_sigs member table — which is
    // flat — covers every constructor by its short name).
    out.push(("int", super::bi("test.int")));
    out.push(("float", super::bi("test.float")));
    out.push(("bool", super::bi("test.bool")));
    out.push(("constant", super::bi("test.constant")));
    out.push(("string", super::bi("test.string")));
    out.push(("oneOf", super::bi("test.oneOf")));
    out.push(("frequency", super::bi("test.frequency")));
    out.push(("arrayOf", super::bi("test.arrayOf")));
    out.push(("objectWith", super::bi("test.objectWith")));
    out.push(("map", super::bi("test.map")));
    out.push(("filter", super::bi("test.filter")));
    out.push(("nilOr", super::bi("test.nilOr")));
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// tagged-object helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Build a generator tag Object `{__gen: kind, ...fields}`.
fn make_gen(kind: &str, fields: Vec<(&str, Value)>) -> Value {
    let mut m: IndexMap<String, Value> = IndexMap::new();
    m.insert("__gen".to_string(), Value::str(kind));
    for (k, v) in fields {
        m.insert(k.to_string(), v);
    }
    Value::object(m)
}

/// Extract the `__gen` kind tag from a generator Object, or `None` if `v` is not a
/// generator.
pub(crate) fn gen_kind(v: &Value) -> Option<String> {
    match v.kind() {
        ValueKind::Object(o) => match o.get("__gen").as_ref().map(|x| x.kind()) {
            Some(ValueKind::Str(s)) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// Read a field off a generator Object.
#[allow(dead_code)] // consumed by draw_gen / the C3 runner (see MAX_GEN_DEPTH note).
fn gen_field(g: &Value, key: &str) -> Option<Value> {
    match g.kind() {
        ValueKind::Object(o) => o.get(key),
        _ => None,
    }
}

/// A Tier-2 config panic (`test.<kind>: <msg>`).
fn cfg_err(kind: &str, msg: impl AsRef<str>, span: Span) -> Control {
    AsError::at(format!("test.{kind}: {}", msg.as_ref()), span).into()
}

// ─────────────────────────────────────────────────────────────────────────────
// argument coercion for the constructors (Tier-2 on misuse)
// ─────────────────────────────────────────────────────────────────────────────

/// Read an optional integer bound from a constructor arg. `nil` → `None` (use the
/// default); a non-integer number is truncated; a non-number → Tier-2.
fn opt_int(args: &[Value], i: usize, kind: &str, what: &str, span: Span) -> Result<Option<i64>, Control> {
    match arg(args, i).kind() {
        ValueKind::Nil => Ok(None),
        ValueKind::Int(n) => Ok(Some(n)),
        ValueKind::Float(f) if f.is_finite() => Ok(Some(f as i64)),
        other => Err(cfg_err(
            kind,
            format!("{what} must be a number, got {}", kind_name(&other)),
            span,
        )),
    }
}

/// Read an optional float bound. `nil` → `None`; any finite number → `Some`.
fn opt_float(args: &[Value], i: usize, kind: &str, what: &str, span: Span) -> Result<Option<f64>, Control> {
    let v = arg(args, i);
    match v.kind() {
        ValueKind::Nil => Ok(None),
        _ => match v.as_f64() {
            Some(f) if f.is_finite() => Ok(Some(f)),
            _ => Err(cfg_err(
                kind,
                format!("{what} must be a finite number, got {}", type_name(&v)),
                span,
            )),
        },
    }
}

/// A short type-name for a borrowed `ValueKind` (for error messages).
fn kind_name(k: &ValueKind<'_>) -> &'static str {
    match k {
        ValueKind::Nil => "nil",
        ValueKind::Bool(_) => "bool",
        ValueKind::Int(_) => "int",
        ValueKind::Float(_) => "float",
        ValueKind::Str(_) => "string",
        ValueKind::Array(_) => "array",
        ValueKind::Object(_) => "object",
        _ => "value",
    }
}

/// Read an optional length-bound field from an opts Object (`{minLen, maxLen}` /
/// `{maxDiscard}`). Missing → `None`; a non-negative finite number → clamped to
/// [`MAX_GEN_LEN`]; a negative or non-number → Tier-2.
fn opt_len_field(opts: &Value, field: &str, kind: &str, span: Span) -> Result<Option<usize>, Control> {
    let v = match opts.kind() {
        ValueKind::Object(o) => match o.get(field) {
            Some(v) => v,
            None => return Ok(None),
        },
        ValueKind::Nil => return Ok(None),
        _ => {
            return Err(cfg_err(
                kind,
                format!("opts must be an object, got {}", type_name(opts)),
                span,
            ))
        }
    };
    match v.as_f64() {
        Some(f) if f.is_finite() && f >= 0.0 => Ok(Some((f as usize).min(MAX_GEN_LEN))),
        _ => Err(cfg_err(
            kind,
            format!("{field} must be a non-negative number, got {}", type_name(&v)),
            span,
        )),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// the constructors (called via call_test)
// ─────────────────────────────────────────────────────────────────────────────

impl Interp {
    /// Dispatch for `test.*` (the generator combinators). All constructors are
    /// pure (build a tagged Object); only `map`/`filter` store a user fn for the
    /// drawer to call. The actual drawing happens in [`Interp::draw_gen`] (used by
    /// the C3 runner) — constructing a generator never draws.
    pub(crate) async fn call_test(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            // ── int(min?, max?) ────────────────────────────────────────────────
            "int" => {
                let min = opt_int(args, 0, "int", "min", span)?.unwrap_or(DEFAULT_INT_MIN);
                let max = opt_int(args, 1, "int", "max", span)?.unwrap_or(DEFAULT_INT_MAX);
                if min > max {
                    return Err(cfg_err("int", format!("min ({min}) must be <= max ({max})"), span));
                }
                Ok(make_gen(
                    "int",
                    vec![("min", Value::int(min)), ("max", Value::int(max))],
                ))
            }
            // ── float(min?, max?) ──────────────────────────────────────────────
            "float" => {
                let min = opt_float(args, 0, "float", "min", span)?.unwrap_or(DEFAULT_FLOAT_MIN);
                let max = opt_float(args, 1, "float", "max", span)?.unwrap_or(DEFAULT_FLOAT_MAX);
                if min > max {
                    return Err(cfg_err("float", format!("min ({min}) must be <= max ({max})"), span));
                }
                Ok(make_gen(
                    "float",
                    vec![("min", Value::float(min)), ("max", Value::float(max))],
                ))
            }
            // ── bool() ─────────────────────────────────────────────────────────
            "bool" => Ok(make_gen("bool", vec![])),
            // ── constant(v) ────────────────────────────────────────────────────
            "constant" => Ok(make_gen("constant", vec![("value", arg(args, 0))])),
            // ── string(opts?) ──────────────────────────────────────────────────
            "string" => {
                let opts = arg(args, 0);
                let min = opt_len_field(&opts, "minLen", "string", span)?.unwrap_or(DEFAULT_MIN_LEN);
                let max = opt_len_field(&opts, "maxLen", "string", span)?.unwrap_or(DEFAULT_MAX_LEN);
                if min > max {
                    return Err(cfg_err(
                        "string",
                        format!("minLen ({min}) must be <= maxLen ({max})"),
                        span,
                    ));
                }
                let charset = match opts.kind() {
                    ValueKind::Object(o) => match o.get("charset") {
                        Some(v) => match v.kind() {
                            ValueKind::Str(s) => s.to_string(),
                            ValueKind::Nil => "ascii".to_string(),
                            _ => {
                                return Err(cfg_err(
                                    "string",
                                    format!("charset must be a string, got {}", type_name(&v)),
                                    span,
                                ))
                            }
                        },
                        None => "ascii".to_string(),
                    },
                    _ => "ascii".to_string(),
                };
                // Validate the charset eagerly (Tier-2) so misuse surfaces at
                // construction, not at draw time. The special `"unicode"` charset is
                // procedural (`charset_chars` returns None); every other name resolves
                // to a concrete character vector — an EMPTY one (e.g. `charset: ""`) is
                // an invalid generator config.
                if charset != "unicode" {
                    match charset_chars(&charset) {
                        Some(chars) if chars.is_empty() => {
                            return Err(cfg_err(
                                "string",
                                format!("charset '{charset}' has no characters to draw from"),
                                span,
                            ));
                        }
                        _ => {}
                    }
                }
                Ok(make_gen(
                    "string",
                    vec![
                        ("minLen", Value::int(min as i64)),
                        ("maxLen", Value::int(max as i64)),
                        ("charset", Value::str(charset)),
                    ],
                ))
            }
            // ── oneOf(...values) ───────────────────────────────────────────────
            //
            // Accepts EITHER a single array argument (`oneOf([1,2,3])`) or variadic
            // values (`oneOf(1, 2, 3)`). The choices are stored as an array; an
            // empty choice set is a Tier-2 config error.
            "oneOf" => {
                let choices: Vec<Value> = if args.len() == 1 {
                    let only = arg(args, 0);
                    match only.kind() {
                        ValueKind::Array(a) => a.borrow().to_vec(),
                        _ => vec![only],
                    }
                } else {
                    args.to_vec()
                };
                if choices.is_empty() {
                    return Err(cfg_err("oneOf", "needs at least one choice", span));
                }
                Ok(make_gen("oneOf", vec![("choices", Value::array(choices))]))
            }
            // ── frequency([[weight, gen], ...]) ────────────────────────────────
            //
            // Weighted choice over (weight, generator) pairs. Each pair is an
            // array `[weight, gen]`; the weight must be a positive number. An empty
            // pair list or a non-positive total weight is a Tier-2 config error.
            "frequency" => {
                let pairs_arg = arg(args, 0);
                let pairs = match pairs_arg.kind() {
                    ValueKind::Array(a) => a.borrow().to_vec(),
                    _ => {
                        return Err(cfg_err(
                            "frequency",
                            format!(
                                "expects an array of [weight, gen] pairs, got {}",
                                type_name(&pairs_arg)
                            ),
                            span,
                        ))
                    }
                };
                if pairs.is_empty() {
                    return Err(cfg_err("frequency", "needs at least one [weight, gen] pair", span));
                }
                let mut total = 0.0f64;
                for p in &pairs {
                    let (w, _g) = pair_weight_gen(p, span)?;
                    total += w;
                }
                if total <= 0.0 {
                    return Err(cfg_err("frequency", "total weight must be positive", span));
                }
                Ok(make_gen("frequency", vec![("pairs", Value::array(pairs))]))
            }
            // ── arrayOf(gen, opts?) ────────────────────────────────────────────
            "arrayOf" => {
                let elem = arg(args, 0);
                if gen_kind(&elem).is_none() {
                    return Err(cfg_err(
                        "arrayOf",
                        format!("first argument must be a generator, got {}", type_name(&elem)),
                        span,
                    ));
                }
                let opts = arg(args, 1);
                let min = opt_len_field(&opts, "minLen", "arrayOf", span)?.unwrap_or(DEFAULT_MIN_LEN);
                let max = opt_len_field(&opts, "maxLen", "arrayOf", span)?.unwrap_or(DEFAULT_MAX_LEN);
                if min > max {
                    return Err(cfg_err(
                        "arrayOf",
                        format!("minLen ({min}) must be <= maxLen ({max})"),
                        span,
                    ));
                }
                Ok(make_gen(
                    "arrayOf",
                    vec![
                        ("elem", elem),
                        ("minLen", Value::int(min as i64)),
                        ("maxLen", Value::int(max as i64)),
                    ],
                ))
            }
            // ── objectWith({k: gen, ...}) ──────────────────────────────────────
            "objectWith" => {
                let shape = arg(args, 0);
                match shape.kind() {
                    ValueKind::Object(o) => {
                        // Validate every field value is a generator.
                        for (k, v) in o.entries() {
                            if gen_kind(&v).is_none() {
                                return Err(cfg_err(
                                    "objectWith",
                                    format!("field '{k}' must be a generator, got {}", type_name(&v)),
                                    span,
                                ));
                            }
                        }
                    }
                    _ => {
                        return Err(cfg_err(
                            "objectWith",
                            format!("expects an object of generators, got {}", type_name(&shape)),
                            span,
                        ))
                    }
                }
                Ok(make_gen("objectWith", vec![("shape", shape)]))
            }
            // ── map(gen, fn) ───────────────────────────────────────────────────
            "map" => {
                let inner = arg(args, 0);
                if gen_kind(&inner).is_none() {
                    return Err(cfg_err(
                        "map",
                        format!("first argument must be a generator, got {}", type_name(&inner)),
                        span,
                    ));
                }
                let f = arg(args, 1);
                if !is_callable(&f) {
                    return Err(cfg_err(
                        "map",
                        format!("second argument must be a function, got {}", type_name(&f)),
                        span,
                    ));
                }
                Ok(make_gen("map", vec![("inner", inner), ("fn", f)]))
            }
            // ── filter(gen, pred, opts?) ───────────────────────────────────────
            "filter" => {
                let inner = arg(args, 0);
                if gen_kind(&inner).is_none() {
                    return Err(cfg_err(
                        "filter",
                        format!("first argument must be a generator, got {}", type_name(&inner)),
                        span,
                    ));
                }
                let pred = arg(args, 1);
                if !is_callable(&pred) {
                    return Err(cfg_err(
                        "filter",
                        format!("second argument must be a function, got {}", type_name(&pred)),
                        span,
                    ));
                }
                let opts = arg(args, 2);
                let max_discard = opt_len_field(&opts, "maxDiscard", "filter", span)?
                    .map(|n| n as u32)
                    .unwrap_or(DEFAULT_MAX_DISCARD);
                Ok(make_gen(
                    "filter",
                    vec![
                        ("inner", inner),
                        ("pred", pred),
                        ("maxDiscard", Value::int(max_discard as i64)),
                    ],
                ))
            }
            // ── nilOr(gen) ─────────────────────────────────────────────────────
            "nilOr" => {
                let inner = arg(args, 0);
                if gen_kind(&inner).is_none() {
                    return Err(cfg_err(
                        "nilOr",
                        format!("argument must be a generator, got {}", type_name(&inner)),
                        span,
                    ));
                }
                Ok(make_gen("nilOr", vec![("inner", inner)]))
            }
            other => Err(AsError::at(
                format!("std/test has no function '{other}'"),
                span,
            )
            .into()),
        }
    }

    /// Draw a single value from `gen` using the deterministic `rng` stream, with a
    /// recursion budget (`depth` counts UP toward [`MAX_GEN_DEPTH`]).
    ///
    /// This is the native engine the C3 `prop()` runner draws each argument from. A
    /// non-generator value is a Tier-2 panic (the runner only ever passes a real
    /// generator, but a hand-rolled `{__gen:"bogus"}` is caught here).
    #[async_recursion::async_recursion(?Send)]
    #[allow(dead_code)] // C2 deliverable; the C3 prop() runner is its production caller.
    pub(crate) async fn draw_gen(
        &self,
        gen: &Value,
        rng: &mut SeededRng,
        depth: u32,
        span: Span,
    ) -> Result<Value, Control> {
        if depth > MAX_GEN_DEPTH {
            return Err(AsError::at(
                format!(
                    "test: generator nesting exceeded the depth budget ({MAX_GEN_DEPTH}) — \
                     simplify the nested arrayOf/objectWith structure"
                ),
                span,
            )
            .into());
        }
        let kind = match gen_kind(gen) {
            Some(k) => k,
            None => {
                return Err(AsError::at(
                    format!(
                        "test: draw target is not a generator (got {})",
                        type_name(gen)
                    ),
                    span,
                )
                .into())
            }
        };
        match kind.as_str() {
            "int" => {
                let min = gen_field(gen, "min").and_then(|v| v.as_f64()).unwrap_or(0.0) as i64;
                let max = gen_field(gen, "max").and_then(|v| v.as_f64()).unwrap_or(0.0) as i64;
                Ok(Value::int(draw_int(rng, min, max)))
            }
            "float" => {
                let min = gen_field(gen, "min").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let max = gen_field(gen, "max").and_then(|v| v.as_f64()).unwrap_or(0.0);
                Ok(Value::float(draw_float(rng, min, max)))
            }
            "bool" => Ok(Value::bool_(rng.next_u64() & 1 == 1)),
            "constant" => Ok(gen_field(gen, "value").unwrap_or(Value::nil())),
            "string" => {
                let min = gen_field(gen, "minLen").and_then(|v| v.as_f64()).unwrap_or(0.0) as usize;
                let max = gen_field(gen, "maxLen").and_then(|v| v.as_f64()).unwrap_or(0.0) as usize;
                let charset = match gen_field(gen, "charset").map(|v| v.into_kind()) {
                    Some(OwnedKind::Str(s)) => s.to_string(),
                    _ => "ascii".to_string(),
                };
                let n = draw_count(rng, min, max);
                Ok(Value::str(draw_string(rng, n, &charset)))
            }
            "oneOf" => {
                let choices = match gen_field(gen, "choices").map(|v| v.into_kind()) {
                    Some(OwnedKind::Array(a)) => a.borrow().to_vec(),
                    _ => vec![],
                };
                if choices.is_empty() {
                    return Err(cfg_err("oneOf", "needs at least one choice", span));
                }
                let i = (rng.next_u64() % choices.len() as u64) as usize;
                Ok(choices[i].clone())
            }
            "frequency" => {
                let pairs = match gen_field(gen, "pairs").map(|v| v.into_kind()) {
                    Some(OwnedKind::Array(a)) => a.borrow().to_vec(),
                    _ => vec![],
                };
                if pairs.is_empty() {
                    return Err(cfg_err("frequency", "needs at least one pair", span));
                }
                let mut weights: Vec<f64> = Vec::with_capacity(pairs.len());
                let mut total = 0.0;
                for p in &pairs {
                    let (w, _g) = pair_weight_gen(p, span)?;
                    weights.push(w);
                    total += w;
                }
                // Weighted pick.
                let r = rng.next_f64() * total;
                let mut acc = 0.0;
                let mut chosen = pairs.len() - 1;
                for (i, w) in weights.iter().enumerate() {
                    acc += *w;
                    if r < acc {
                        chosen = i;
                        break;
                    }
                }
                let (_w, g) = pair_weight_gen(&pairs[chosen], span)?;
                self.draw_gen(&g, rng, depth + 1, span).await
            }
            "arrayOf" => {
                let elem = gen_field(gen, "elem").unwrap_or(Value::nil());
                let min = gen_field(gen, "minLen").and_then(|v| v.as_f64()).unwrap_or(0.0) as usize;
                let max = gen_field(gen, "maxLen").and_then(|v| v.as_f64()).unwrap_or(0.0) as usize;
                let n = draw_count(rng, min, max);
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    out.push(self.draw_gen(&elem, rng, depth + 1, span).await?);
                }
                Ok(Value::array(out))
            }
            "objectWith" => {
                let shape = gen_field(gen, "shape").unwrap_or(Value::nil());
                let entries = match shape.kind() {
                    ValueKind::Object(o) => o.entries(),
                    _ => vec![],
                };
                let mut m: IndexMap<String, Value> = IndexMap::new();
                for (k, g) in entries {
                    if k.as_ref() == "__gen" {
                        continue; // defensive — a shape Object never carries __gen
                    }
                    let v = self.draw_gen(&g, rng, depth + 1, span).await?;
                    m.insert(k.to_string(), v);
                }
                Ok(Value::object(m))
            }
            "map" => {
                let inner = gen_field(gen, "inner").unwrap_or(Value::nil());
                let f = gen_field(gen, "fn").unwrap_or(Value::nil());
                let drawn = self.draw_gen(&inner, rng, depth + 1, span).await?;
                self.call_value(f, vec![drawn], span).await
            }
            "filter" => {
                let inner = gen_field(gen, "inner").unwrap_or(Value::nil());
                let pred = gen_field(gen, "pred").unwrap_or(Value::nil());
                let max_discard = gen_field(gen, "maxDiscard")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(DEFAULT_MAX_DISCARD as f64) as u32;
                let mut discarded = 0u32;
                loop {
                    let candidate = self.draw_gen(&inner, rng, depth + 1, span).await?;
                    let keep = self
                        .call_value(pred.clone(), vec![candidate.clone()], span)
                        .await?;
                    if keep.is_truthy() {
                        return Ok(candidate);
                    }
                    discarded += 1;
                    if discarded >= max_discard {
                        // Tier-1: the predicate starved the generator — name it so the
                        // property author can widen the filter or raise maxDiscard.
                        return Ok(make_pair(
                            Value::nil(),
                            make_error(Value::str(format!(
                                "test.filter: exhausted maxDiscard ({max_discard}) without a value satisfying the predicate"
                            ))),
                        ));
                    }
                }
            }
            "nilOr" => {
                // ~1-in-4 nil bias (the boundary value for an optional).
                if rng.next_u64().is_multiple_of(4) {
                    Ok(Value::nil())
                } else {
                    let inner = gen_field(gen, "inner").unwrap_or(Value::nil());
                    self.draw_gen(&inner, rng, depth + 1, span).await
                }
            }
            other => Err(AsError::at(
                format!("test: unknown generator kind '{other}'"),
                span,
            )
            .into()),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// the seeded drawing primitives (edge-biased)
// ─────────────────────────────────────────────────────────────────────────────

/// Draw an integer in `[min, max]` with edge bias: roughly 1 draw in 4 comes from
/// the BOUNDARY POOL (`{min, max, 0, ±1}` clamped into range) instead of a uniform
/// draw. This is the fuzzgen philosophy surfaced (`src/fuzzgen/`): boundary values
/// far more often than uniform sampling, because bugs cluster at boundaries.
#[allow(dead_code)] // drawing primitives consumed by draw_gen / the C3 runner.
fn draw_int(rng: &mut SeededRng, min: i64, max: i64) -> i64 {
    if min == max {
        return min;
    }
    // 1-in-4 → boundary pool.
    if rng.next_u64().is_multiple_of(4) {
        let pool = int_boundary_pool(min, max);
        let i = (rng.next_u64() % pool.len() as u64) as usize;
        return pool[i];
    }
    // Uniform draw over the inclusive range. Use the full unsigned width of the
    // span to avoid modulo bias being concentrated; span+1 may overflow i64 at the
    // extremes, so compute in u128.
    let span = (max as i128 - min as i128) as u128 + 1;
    let r = (rng.next_u64() as u128) % span;
    (min as i128 + r as i128) as i64
}

/// The boundary pool for an int range: `{min, max, 0, 1, -1}` clamped to lie within
/// `[min, max]`, de-duplicated. Always non-empty (min and max are always in range).
#[allow(dead_code)]
fn int_boundary_pool(min: i64, max: i64) -> Vec<i64> {
    let mut pool = vec![min, max];
    for &cand in &[0i64, 1, -1] {
        if cand >= min && cand <= max {
            pool.push(cand);
        }
    }
    pool.sort_unstable();
    pool.dedup();
    pool
}

/// True iff `v` is one of the int boundary-pool values for `[min, max]`. Used by
/// the edge-bias test to count boundary hits.
#[cfg(test)]
fn is_int_boundary(v: i64, min: i64, max: i64) -> bool {
    int_boundary_pool(min, max).contains(&v)
}

/// Draw a float in `[min, max]` with the same 1-in-4 boundary bias (`{min, max, 0,
/// ±1}` clamped into range).
#[allow(dead_code)]
fn draw_float(rng: &mut SeededRng, min: f64, max: f64) -> f64 {
    if min == max {
        return min;
    }
    if rng.next_u64().is_multiple_of(4) {
        let mut pool = vec![min, max];
        for &cand in &[0.0f64, 1.0, -1.0] {
            if cand >= min && cand <= max {
                pool.push(cand);
            }
        }
        let i = (rng.next_u64() % pool.len() as u64) as usize;
        return pool[i];
    }
    min + rng.next_f64() * (max - min)
}

/// Draw a collection/string length in `[min, max]` with edge bias toward the
/// endpoints (empty / single / full) — arrays and strings bias toward
/// empty/single, matching §10.4.
#[allow(dead_code)]
fn draw_count(rng: &mut SeededRng, min: usize, max: usize) -> usize {
    if min >= max {
        return min;
    }
    if rng.next_u64().is_multiple_of(4) {
        // Boundary lengths: min, min+1 (clamped), max.
        let pool = [min, (min + 1).min(max), max];
        let i = (rng.next_u64() % pool.len() as u64) as usize;
        return pool[i];
    }
    let span = (max - min) as u64 + 1;
    min + (rng.next_u64() % span) as usize
}

/// Draw a string of `n` characters from a named charset (or a literal character
/// set). Every produced char is a valid Unicode scalar value.
#[allow(dead_code)]
fn draw_string(rng: &mut SeededRng, n: usize, charset: &str) -> String {
    let mut s = String::with_capacity(n);
    match charset_chars(charset) {
        Some(chars) => {
            if chars.is_empty() {
                return s;
            }
            for _ in 0..n {
                let i = (rng.next_u64() % chars.len() as u64) as usize;
                s.push(chars[i]);
            }
        }
        None => {
            // "unicode": draw a valid scalar value, biased toward the BMP and a few
            // boundary code points (the smallest, the ASCII boundary, the BMP edge).
            for _ in 0..n {
                s.push(draw_unicode_scalar(rng));
            }
        }
    }
    s
}

/// Resolve a named charset to its character vector. Returns `None` for the special
/// `"unicode"` charset (handled procedurally) so the caller knows to draw scalars.
/// A literal string (not a known name) is treated as an explicit character set.
fn charset_chars(charset: &str) -> Option<Vec<char>> {
    match charset {
        "ascii" => Some((0x20u8..=0x7e).map(|b| b as char).collect()),
        "alpha" => Some(
            ('a'..='z').chain('A'..='Z').collect(),
        ),
        "alphanumeric" => Some(
            ('a'..='z').chain('A'..='Z').chain('0'..='9').collect(),
        ),
        "digit" => Some(('0'..='9').collect()),
        "unicode" => None,
        // A literal charset: the user passes the exact characters to draw from.
        other => Some(other.chars().collect()),
    }
}

/// Draw a single valid Unicode scalar value with a light boundary bias. Never
/// produces a surrogate (`U+D800..=U+DFFF`) — every result is a real scalar.
#[allow(dead_code)]
fn draw_unicode_scalar(rng: &mut SeededRng) -> char {
    // 1-in-4: a boundary scalar.
    if rng.next_u64().is_multiple_of(4) {
        let pool = ['\u{0}', '\u{7f}', '\u{80}', '\u{ff}', '\u{100}', '\u{7ff}', '\u{800}', '\u{ffff}', '\u{10000}', '\u{10ffff}'];
        let i = (rng.next_u64() % pool.len() as u64) as usize;
        return pool[i];
    }
    // Uniform over the scalar space, retrying past the surrogate gap.
    loop {
        let cp = (rng.next_u64() % 0x11_0000) as u32;
        if (0xD800..=0xDFFF).contains(&cp) {
            continue;
        }
        if let Some(c) = char::from_u32(cp) {
            return c;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// misc helpers
// ─────────────────────────────────────────────────────────────────────────────

/// True iff `v` is a callable value (a user fn, a builtin, a closure, a bound
/// method, or a class-method). Used to validate `map`/`filter` second args.
fn is_callable(v: &Value) -> bool {
    matches!(
        v.kind(),
        ValueKind::Function(_)
            | ValueKind::Builtin(_)
            | ValueKind::Closure(_)
            | ValueKind::BoundMethod(_)
            | ValueKind::ClassMethod(_)
            | ValueKind::NativeMethod(_)
    )
}

/// Decompose a `frequency` pair `[weight, gen]` into `(weight, gen)`. A malformed
/// pair (not a 2-element array, non-positive weight, non-generator) is Tier-2.
fn pair_weight_gen(p: &Value, span: Span) -> Result<(f64, Value), Control> {
    let arr = match p.kind() {
        ValueKind::Array(a) => a.borrow().to_vec(),
        _ => {
            return Err(cfg_err(
                "frequency",
                format!("each entry must be a [weight, gen] pair, got {}", type_name(p)),
                span,
            ))
        }
    };
    if arr.len() != 2 {
        return Err(cfg_err("frequency", "each pair must be [weight, gen]", span));
    }
    let w = match arr[0].as_f64() {
        Some(w) if w.is_finite() && w > 0.0 => w,
        _ => {
            return Err(cfg_err(
                "frequency",
                format!("weight must be a positive number, got {}", type_name(&arr[0])),
                span,
            ))
        }
    };
    let g = arr[1].clone();
    if gen_kind(&g).is_none() {
        return Err(cfg_err(
            "frequency",
            format!("second element of a pair must be a generator, got {}", type_name(&g)),
            span,
        ));
    }
    Ok((w, g))
}

// ─────────────────────────────────────────────────────────────────────────────
// tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::Interp;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    fn rng(seed: u64) -> SeededRng {
        SeededRng::new(seed)
    }

    async fn draw(interp: &Interp, gen: &Value, rng: &mut SeededRng) -> Value {
        interp.draw_gen(gen, rng, 0, sp()).await.unwrap()
    }

    // ── constructors build inert tagged Objects ────────────────────────────────

    #[tokio::test]
    async fn int_constructor_is_a_tagged_object() {
        let interp = Interp::new();
        let g = interp
            .call_test("int", &[Value::int(-5), Value::int(5)], sp())
            .await
            .unwrap();
        assert_eq!(gen_kind(&g).as_deref(), Some("int"));
        // Inert + printable: it's a plain Object, so it renders/serializes.
        assert!(matches!(g.kind(), ValueKind::Object(_)));
        let printed = format!("{g}");
        assert!(printed.contains("__gen"), "printed form: {printed}");
        // JSON-able (a plain Object round-trips through the value display).
        assert_eq!(gen_field(&g, "min"), Some(Value::int(-5)));
        assert_eq!(gen_field(&g, "max"), Some(Value::int(5)));
    }

    #[tokio::test]
    async fn gen_namespace_object_exposes_combinators() {
        let exp = exports();
        let (_, gen_ns) = exp.iter().find(|(n, _)| *n == "gen").expect("gen export");
        match gen_ns.kind() {
            ValueKind::Object(o) => {
                assert!(o.get("int").is_some());
                assert!(o.get("arrayOf").is_some());
                assert!(o.get("filter").is_some());
            }
            _ => panic!("gen export must be a namespace Object"),
        }
    }

    // ── seeded determinism + bounds ────────────────────────────────────────────

    #[tokio::test]
    async fn int_respects_bounds_and_is_deterministic() {
        let interp = Interp::new();
        let g = interp
            .call_test("int", &[Value::int(-1000), Value::int(1000)], sp())
            .await
            .unwrap();
        let mut a = rng(42);
        let mut b = rng(42);
        for _ in 0..200 {
            let x = draw(&interp, &g, &mut a).await;
            let y = draw(&interp, &g, &mut b).await;
            assert_eq!(x, y, "same seed → same draw");
            let n = x.as_f64().unwrap() as i64;
            assert!((-1000..=1000).contains(&n), "out of bounds: {n}");
        }
    }

    #[tokio::test]
    async fn int_pinned_sequence() {
        // Pin a concrete seeded sequence so a future change to the draw math is a
        // visible, intentional break (not a silent distribution shift).
        let interp = Interp::new();
        let g = interp
            .call_test("int", &[Value::int(0), Value::int(100)], sp())
            .await
            .unwrap();
        let mut r = rng(7);
        let seq: Vec<i64> = {
            let mut v = vec![];
            for _ in 0..5 {
                v.push(draw(&interp, &g, &mut r).await.as_f64().unwrap() as i64);
            }
            v
        };
        // Re-derive with the same seed → identical.
        let mut r2 = rng(7);
        let seq2: Vec<i64> = {
            let mut v = vec![];
            for _ in 0..5 {
                v.push(draw(&interp, &g, &mut r2).await.as_f64().unwrap() as i64);
            }
            v
        };
        assert_eq!(seq, seq2);
        // All in range.
        assert!(seq.iter().all(|&n| (0..=100).contains(&n)));
    }

    // ── EDGE BIAS: the mechanism, pinned over 200 draws ─────────────────────────

    #[tokio::test]
    async fn int_edge_bias_boosts_boundary_frequency() {
        let interp = Interp::new();
        let (min, max) = (-1000i64, 1000i64);
        let g = interp
            .call_test("int", &[Value::int(min), Value::int(max)], sp())
            .await
            .unwrap();
        let mut r = rng(12345);
        let draws = 2000;
        let mut boundary_hits = 0usize;
        for _ in 0..draws {
            let n = draw(&interp, &g, &mut r).await.as_f64().unwrap() as i64;
            if is_int_boundary(n, min, max) {
                boundary_hits += 1;
            }
        }
        // The boundary pool is {-1000, -1, 0, 1, 1000} = 5 values out of 2001. A
        // UNIFORM sampler would hit them ~5/2001 ≈ 0.25% of the time (≈5 of 2000).
        // With the 1-in-4 boundary bias the expected rate is ≈25% (≈500 of 2000).
        // Assert the rate is MATERIALLY above uniform — well past any noise floor —
        // without pinning an exact count (the mechanism, not the number).
        let uniform_expected = draws as f64 * 5.0 / 2001.0; // ≈5
        assert!(
            boundary_hits as f64 > uniform_expected * 20.0,
            "edge bias not boosting boundaries: {boundary_hits} hits over {draws} draws \
             (uniform would give ≈{uniform_expected:.1})"
        );
        // And it's not degenerate (not ALL boundary).
        assert!(
            boundary_hits < draws,
            "every draw was a boundary — bias should be ~1/4, not always"
        );
    }

    // ── string ──────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn string_respects_length_and_charset() {
        let interp = Interp::new();
        let mut opts: IndexMap<String, Value> = IndexMap::new();
        opts.insert("minLen".to_string(), Value::int(3));
        opts.insert("maxLen".to_string(), Value::int(8));
        opts.insert("charset".to_string(), Value::str("digit"));
        let g = interp
            .call_test("string", &[Value::object(opts)], sp())
            .await
            .unwrap();
        let mut r = rng(99);
        for _ in 0..100 {
            let s = draw(&interp, &g, &mut r).await;
            let s = s.as_str().unwrap().to_string();
            assert!((3..=8).contains(&s.chars().count()), "len: {}", s.chars().count());
            assert!(s.chars().all(|c| c.is_ascii_digit()), "non-digit in {s:?}");
        }
    }

    #[tokio::test]
    async fn string_unicode_draws_are_valid_scalars() {
        let interp = Interp::new();
        let mut opts: IndexMap<String, Value> = IndexMap::new();
        opts.insert("minLen".to_string(), Value::int(1));
        opts.insert("maxLen".to_string(), Value::int(16));
        opts.insert("charset".to_string(), Value::str("unicode"));
        let g = interp
            .call_test("string", &[Value::object(opts)], sp())
            .await
            .unwrap();
        let mut r = rng(2024);
        for _ in 0..200 {
            let s = draw(&interp, &g, &mut r).await;
            let s = s.as_str().unwrap().to_string();
            // Every char is a valid scalar (no surrogate) — guaranteed by the
            // Rust `char` type; assert non-empty within bounds.
            assert!((1..=16).contains(&s.chars().count()));
            for c in s.chars() {
                let cp = c as u32;
                assert!(!(0xD800..=0xDFFF).contains(&cp), "surrogate leaked: {cp:#x}");
            }
        }
    }

    #[tokio::test]
    async fn unknown_charset_is_caught_only_when_empty_literal() {
        let interp = Interp::new();
        // An empty literal charset is rejected (Tier-2).
        let mut opts: IndexMap<String, Value> = IndexMap::new();
        opts.insert("charset".to_string(), Value::str(""));
        let r = interp.call_test("string", &[Value::object(opts)], sp()).await;
        assert!(r.is_err(), "empty charset should be Tier-2");
    }

    // ── arrayOf + objectWith + nesting ─────────────────────────────────────────

    #[tokio::test]
    async fn array_of_respects_bounds() {
        let interp = Interp::new();
        let elem = interp
            .call_test("int", &[Value::int(0), Value::int(9)], sp())
            .await
            .unwrap();
        let mut opts: IndexMap<String, Value> = IndexMap::new();
        opts.insert("minLen".to_string(), Value::int(2));
        opts.insert("maxLen".to_string(), Value::int(5));
        let g = interp
            .call_test("arrayOf", &[elem, Value::object(opts)], sp())
            .await
            .unwrap();
        let mut r = rng(555);
        for _ in 0..100 {
            let a = draw(&interp, &g, &mut r).await;
            match a.kind() {
                ValueKind::Array(arr) => {
                    let v = arr.borrow();
                    assert!((2..=5).contains(&v.len()), "len {}", v.len());
                    for x in v.iter() {
                        let n = x.as_f64().unwrap() as i64;
                        assert!((0..=9).contains(&n));
                    }
                }
                _ => panic!("arrayOf must draw an array"),
            }
        }
    }

    #[tokio::test]
    async fn object_with_draws_each_field() {
        let interp = Interp::new();
        let id = interp
            .call_test("int", &[Value::int(1), Value::int(100)], sp())
            .await
            .unwrap();
        let flag = interp.call_test("bool", &[], sp()).await.unwrap();
        let mut shape: IndexMap<String, Value> = IndexMap::new();
        shape.insert("id".to_string(), id);
        shape.insert("flag".to_string(), flag);
        let g = interp
            .call_test("objectWith", &[Value::object(shape)], sp())
            .await
            .unwrap();
        let mut r = rng(31);
        let o = draw(&interp, &g, &mut r).await;
        match o.kind() {
            ValueKind::Object(obj) => {
                let id = obj.get("id").unwrap();
                let n = id.as_f64().unwrap() as i64;
                assert!((1..=100).contains(&n));
                assert!(matches!(obj.get("flag").unwrap().kind(), ValueKind::Bool(_)));
                // No __gen leaked into the drawn object.
                assert!(obj.get("__gen").is_none());
            }
            _ => panic!("objectWith must draw an object"),
        }
    }

    #[tokio::test]
    async fn deeply_nested_generators_hit_the_budget_cleanly() {
        let interp = Interp::new();
        // Build arrayOf(arrayOf(arrayOf(... int ...))) deeper than MAX_GEN_DEPTH.
        let mut g = interp
            .call_test("int", &[Value::int(0), Value::int(1)], sp())
            .await
            .unwrap();
        for _ in 0..(MAX_GEN_DEPTH + 5) {
            // Force length 1 so each level actually recurses.
            let mut opts: IndexMap<String, Value> = IndexMap::new();
            opts.insert("minLen".to_string(), Value::int(1));
            opts.insert("maxLen".to_string(), Value::int(1));
            g = interp
                .call_test("arrayOf", &[g, Value::object(opts)], sp())
                .await
                .unwrap();
        }
        let mut r = rng(1);
        let result = interp.draw_gen(&g, &mut r, 0, sp()).await;
        // A clean Tier-2 panic (Control::Panic), NOT a stack overflow.
        match result {
            Err(Control::Panic(e)) => {
                assert!(
                    e.message.contains("depth budget"),
                    "expected a depth-budget panic, got: {}",
                    e.message
                );
            }
            other => panic!("expected a clean depth-budget Tier-2 panic, got {other:?}"),
        }
    }

    // ── oneOf + constant + frequency ───────────────────────────────────────────

    #[tokio::test]
    async fn one_of_picks_a_choice() {
        let interp = Interp::new();
        let choices = Value::array(vec![Value::str("a"), Value::str("b"), Value::str("c")]);
        let g = interp.call_test("oneOf", &[choices], sp()).await.unwrap();
        let mut r = rng(8);
        for _ in 0..50 {
            let v = draw(&interp, &g, &mut r).await;
            let s = v.as_str().unwrap();
            assert!(["a", "b", "c"].contains(&s), "got {s}");
        }
    }

    #[tokio::test]
    async fn one_of_empty_is_tier2() {
        let interp = Interp::new();
        let r = interp
            .call_test("oneOf", &[Value::array(vec![])], sp())
            .await;
        assert!(matches!(r, Err(Control::Panic(_))), "oneOf([]) must be Tier-2");
    }

    #[tokio::test]
    async fn constant_always_returns_value() {
        let interp = Interp::new();
        let g = interp
            .call_test("constant", &[Value::int(42)], sp())
            .await
            .unwrap();
        let mut r = rng(0);
        for _ in 0..10 {
            assert_eq!(draw(&interp, &g, &mut r).await, Value::int(42));
        }
    }

    #[tokio::test]
    async fn frequency_draws_from_pairs() {
        let interp = Interp::new();
        let ga = interp.call_test("constant", &[Value::str("a")], sp()).await.unwrap();
        let gb = interp.call_test("constant", &[Value::str("b")], sp()).await.unwrap();
        let pairs = Value::array(vec![
            Value::array(vec![Value::int(3), ga]),
            Value::array(vec![Value::int(1), gb]),
        ]);
        let g = interp.call_test("frequency", &[pairs], sp()).await.unwrap();
        let mut r = rng(77);
        let mut a = 0;
        let mut b = 0;
        for _ in 0..400 {
            match draw(&interp, &g, &mut r).await.as_str().unwrap() {
                "a" => a += 1,
                "b" => b += 1,
                other => panic!("unexpected {other}"),
            }
        }
        // 3:1 weight → "a" should dominate (not exact; mechanism only).
        assert!(a > b, "weighted frequency: a={a}, b={b}");
        assert!(b > 0, "the lower-weight branch should still appear");
    }

    // ── map + filter (the async call_value path) ───────────────────────────────

    #[tokio::test]
    async fn filter_exhausting_max_discard_is_tier1_naming_the_gen() {
        // A predicate that ALWAYS rejects → maxDiscard exhausted → Tier-1 pair.
        let interp = Interp::new();
        let inner = interp
            .call_test("int", &[Value::int(0), Value::int(10)], sp())
            .await
            .unwrap();
        let pred = always_false_fn();
        let mut opts: IndexMap<String, Value> = IndexMap::new();
        opts.insert("maxDiscard".to_string(), Value::int(5));
        let g = interp
            .call_test("filter", &[inner, pred, Value::object(opts)], sp())
            .await
            .unwrap();
        let mut r = rng(3);
        let result = interp.draw_gen(&g, &mut r, 0, sp()).await.unwrap();
        // Tier-1 [nil, err] pair naming the filter.
        match result.kind() {
            ValueKind::Array(a) => {
                let v = a.borrow();
                assert_eq!(v.len(), 2);
                assert_eq!(v[0], Value::nil());
                let msg = err_message(&v[1]);
                assert!(msg.contains("filter"), "err should name the filter: {msg}");
                assert!(msg.contains("maxDiscard"), "err should mention maxDiscard: {msg}");
            }
            _ => panic!("filter exhaustion must be a Tier-1 pair, got {result}"),
        }
    }

    #[tokio::test]
    async fn filter_keeps_a_passing_value() {
        let interp = Interp::new();
        let inner = interp
            .call_test("int", &[Value::int(0), Value::int(100)], sp())
            .await
            .unwrap();
        let pred = always_true_fn();
        let g = interp
            .call_test("filter", &[inner, pred], sp())
            .await
            .unwrap();
        let mut r = rng(9);
        let v = interp.draw_gen(&g, &mut r, 0, sp()).await.unwrap();
        let n = v.as_f64().unwrap() as i64;
        assert!((0..=100).contains(&n));
    }

    #[tokio::test]
    async fn map_applies_the_user_fn() {
        let interp = Interp::new();
        let inner = interp
            .call_test("int", &[Value::int(1), Value::int(1)], sp()) // constant 1
            .await
            .unwrap();
        let f = double_fn();
        let g = interp.call_test("map", &[inner, f], sp()).await.unwrap();
        let mut r = rng(5);
        let v = interp.draw_gen(&g, &mut r, 0, sp()).await.unwrap();
        // 1 doubled → 2.
        assert_eq!(v.as_f64().unwrap() as i64, 2);
    }

    // ── nilOr ───────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn nil_or_produces_both_nil_and_inner() {
        let interp = Interp::new();
        let inner = interp
            .call_test("constant", &[Value::int(7)], sp())
            .await
            .unwrap();
        let g = interp.call_test("nilOr", &[inner], sp()).await.unwrap();
        let mut r = rng(2026);
        let mut saw_nil = false;
        let mut saw_val = false;
        for _ in 0..100 {
            match draw(&interp, &g, &mut r).await.kind() {
                ValueKind::Nil => saw_nil = true,
                ValueKind::Int(7) => saw_val = true,
                other => panic!("nilOr produced {other:?}"),
            }
        }
        assert!(saw_nil && saw_val, "nilOr must produce both nil and the inner value");
    }

    // ── invalid configs are Tier-2 ──────────────────────────────────────────────

    #[tokio::test]
    async fn int_min_gt_max_is_tier2() {
        let interp = Interp::new();
        let r = interp
            .call_test("int", &[Value::int(10), Value::int(1)], sp())
            .await;
        assert!(matches!(r, Err(Control::Panic(_))));
    }

    #[tokio::test]
    async fn array_of_negative_len_is_tier2() {
        let interp = Interp::new();
        let elem = interp.call_test("bool", &[], sp()).await.unwrap();
        let mut opts: IndexMap<String, Value> = IndexMap::new();
        opts.insert("minLen".to_string(), Value::int(-2));
        let r = interp
            .call_test("arrayOf", &[elem, Value::object(opts)], sp())
            .await;
        assert!(matches!(r, Err(Control::Panic(_))));
    }

    #[tokio::test]
    async fn array_of_non_generator_elem_is_tier2() {
        let interp = Interp::new();
        let r = interp
            .call_test("arrayOf", &[Value::int(5)], sp())
            .await;
        assert!(matches!(r, Err(Control::Panic(_))));
    }

    #[tokio::test]
    async fn map_non_callable_is_tier2() {
        let interp = Interp::new();
        let inner = interp.call_test("bool", &[], sp()).await.unwrap();
        let r = interp.call_test("map", &[inner, Value::int(3)], sp()).await;
        assert!(matches!(r, Err(Control::Panic(_))));
    }

    // ── test helpers: build callable Values directly from AST ───────────────────
    //
    // The schema-refine test precedent: construct a `Value::function` from a
    // hand-built AST `Function` node so the async `call_value` path can drive it
    // without spinning up a full source compile.

    use crate::ast::{Expr, ExprKind, Param, Stmt};
    use crate::value::Function;
    use std::rc::Rc;

    fn fn1(name: &str, body_expr: ExprKind) -> Value {
        let body = vec![Stmt::Return(Some(Expr {
            kind: body_expr,
            span: sp(),
        }))];
        let func = Function {
            name: Some(name.into()),
            params: vec![Param {
                name: "x".into(),
                ty: None,
                name_span: sp(),
                rest: false,
                default: None,
            }],
            ret: None,
            body,
            closure: crate::interp::global_env(),
            is_async: false,
            is_generator: false,
            is_worker: false,
            name_span: None,
        };
        Value::function(Rc::new(func))
    }

    fn always_false_fn() -> Value {
        fn1("falsePred", ExprKind::Bool(false))
    }
    fn always_true_fn() -> Value {
        fn1("truePred", ExprKind::Bool(true))
    }
    fn double_fn() -> Value {
        // (x) => x * 2
        fn1(
            "double",
            ExprKind::Binary {
                op: crate::ast::BinOp::Mul,
                lhs: Box::new(Expr {
                    kind: ExprKind::Ident("x".to_string()),
                    span: sp(),
                }),
                rhs: Box::new(Expr {
                    kind: ExprKind::Int(2),
                    span: sp(),
                }),
            },
        )
    }

    fn err_message(v: &Value) -> String {
        match v.kind() {
            ValueKind::Object(o) => match o.get("message").map(|m| m.into_kind()) {
                Some(OwnedKind::Str(s)) => s.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }
}
