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
    // `prop` — the property-test runner entry point (BATT C3, §10.4). A top-level
    // export (NOT under `gen`); `import { prop, gen } from "std/test"`.
    out.push(("prop", super::bi("test.prop")));
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
            // ── prop(name, gens, fn, opts?) ────────────────────────────────────
            //
            // BATT C3 (§10.3/§10.4): register a property test. Like `test()`, this
            // pushes into the SAME `self.tests` table — but as a `{__prop: true,
            // gens, fn, opts}` tagged Object (the schema/`__gen` posture; no new
            // `Value` variant). `run_registered_tests_det` branches on the `__prop`
            // tag and drives `run_property` instead of `call_value`. The drawing,
            // re-running, and shrinking all happen there; this is registration only.
            "prop" => {
                let name = match arg(args, 0).kind() {
                    ValueKind::Str(s) => s.to_string(),
                    _ => arg(args, 0).to_string(),
                };
                let gens = arg(args, 1);
                // Validate the generators argument shape eagerly (Tier-2): either an
                // array of generators or an object of name→generator. (The per-gen
                // generator-ness is re-validated at draw time by draw_gen.)
                match gens.kind() {
                    ValueKind::Array(_) | ValueKind::Object(_) => {}
                    _ => {
                        return Err(cfg_err(
                            "prop",
                            format!(
                                "second argument must be an array or object of generators, got {}",
                                type_name(&gens)
                            ),
                            span,
                        ))
                    }
                }
                let f = arg(args, 2);
                if !is_callable(&f) {
                    return Err(cfg_err(
                        "prop",
                        format!("third argument must be a function, got {}", type_name(&f)),
                        span,
                    ));
                }
                let opts = arg(args, 3);
                if !matches!(opts.kind(), ValueKind::Nil | ValueKind::Object(_)) {
                    return Err(cfg_err(
                        "prop",
                        format!("opts must be an object, got {}", type_name(&opts)),
                        span,
                    ));
                }
                // Build the `{__prop, name, gens, fn, opts}` registration Object and push
                // it into the SAME table plain `test()` uses. The table type stays
                // `Vec<(String, Value)>`.
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("__prop".to_string(), Value::bool_(true));
                m.insert("name".to_string(), Value::str(name.as_str()));
                m.insert("gens".to_string(), gens);
                m.insert("fn".to_string(), f);
                m.insert("opts".to_string(), opts);
                let prop_obj = Value::object(m);
                self.register_test(name, prop_obj);
                Ok(Value::nil())
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
// BATT C3 — the prop() runner + shrinking (§10.3/§10.5)
// ─────────────────────────────────────────────────────────────────────────────

/// Default iteration count for a property (spec §10.4 — `runs?: 100`).
const DEFAULT_RUNS: u64 = 100;
/// Default shrink budget (spec §10.4 — `maxShrinks?: 500`).
const DEFAULT_MAX_SHRINKS: u64 = 500;

/// Is `v` a `{__prop: true, …}` registration Object (built by `test.prop`)?
///
/// The `run_registered_tests_det` loop calls this to decide whether to drive
/// [`Interp::run_property`] (a property test) or the unchanged `call_value` path (a
/// plain `test()` closure). A plain test func is a callable, never an Object, so the
/// two never collide.
pub(crate) fn is_prop(v: &Value) -> bool {
    match v.kind() {
        ValueKind::Object(o) => matches!(o.get("__prop").as_ref().map(|x| x.kind()), Some(ValueKind::Bool(true))),
        _ => false,
    }
}

/// How the generators were supplied (`[g, …]` positional vs `{name: g, …}` named) —
/// determines how the drawn args are passed to the property fn AND how the
/// counterexample is rendered in the report.
enum GenForm {
    /// Positional: each generator draws one positional argument; `fn(a, b, …)`.
    Array(Vec<Value>),
    /// Named: a fixed shape of `name → generator`; `fn({a, b, …})` (one object arg).
    Object(Vec<(String, Value)>),
}

impl Interp {
    /// BATT C3 (§10.3/§10.5) — run a `{__prop}` property registration: draw `runs`
    /// seeded iterations, and on the FIRST failure SHRINK to a minimal counterexample,
    /// then raise a Tier-2 panic whose message IS the formatted report. The caller
    /// (`run_registered_tests_det`) records that message verbatim, so the existing
    /// `FAIL <name>: <message>` summary machinery prints the full report.
    ///
    /// **Seed precedence (§10.4):** `opts.seed` > the CLI seed (carried by the active
    /// determinism context, i.e. `--seed`) > a fresh random seed. A fresh seed is
    /// PRINTED in the report so the failure is replayable.
    ///
    /// **Per-iteration RNG:** iteration `i` draws from `SeededRng::new(seed_for(base, i))`
    /// where `seed_for(base, i) = base ^ splitmix64(i)` — a deterministic mix so each
    /// iteration is an independent, reproducible stream and re-running with the same
    /// base seed reproduces the identical counterexample.
    pub(crate) async fn run_property(&self, prop_obj: &Value, span: Span) -> Result<Value, Control> {
        let name = prop_field_str(prop_obj, "name").unwrap_or_default();
        let fnv = prop_field(prop_obj, "fn").unwrap_or(Value::nil());
        let opts = prop_field(prop_obj, "opts").unwrap_or(Value::nil());

        // Resolve the generator form.
        let gens = prop_field(prop_obj, "gens").unwrap_or(Value::nil());
        let form = match gens.kind() {
            ValueKind::Array(a) => GenForm::Array(a.borrow().to_vec()),
            ValueKind::Object(o) => {
                let mut v = Vec::new();
                for (k, g) in o.entries() {
                    v.push((k.to_string(), g));
                }
                GenForm::Object(v)
            }
            _ => {
                return Err(AsError::at(
                    "test.prop: generators must be an array or object".to_string(),
                    span,
                )
                .into())
            }
        };

        // runs / maxShrinks budgets.
        let runs = opt_u64_field(&opts, "runs").unwrap_or(DEFAULT_RUNS).max(1);
        let max_shrinks = opt_u64_field(&opts, "maxShrinks").unwrap_or(DEFAULT_MAX_SHRINKS);

        // Seed precedence: opts.seed > CLI seed (from the active det context) > random.
        let (base_seed, seed_source) = match opt_u64_field(&opts, "seed") {
            Some(s) => (s, SeedSource::Opts),
            None => match self.determinism_seed() {
                Some(s) => (s, SeedSource::Cli),
                None => (fresh_random_seed(), SeedSource::Random),
            },
        };

        // ── the N seeded iterations ────────────────────────────────────────────
        for i in 0..runs {
            let mut rng = SeededRng::new(seed_for(base_seed, i));
            // Draw the argument list for this iteration.
            let args = self.draw_args(&form, &mut rng, span).await?;
            // Run the property once; a failure (falsy / panic / propagate) triggers
            // shrinking from THIS draw.
            if let Some(orig_msg) = self.run_property_once(&fnv, &form, &args, span).await? {
                // Shrink, then build + raise the report.
                let (shrunk, shrinks) = self
                    .shrink(&fnv, &form, &args, max_shrinks, span)
                    .await?;
                let report = format_report(
                    &name,
                    &form,
                    &shrunk,
                    i,
                    shrinks,
                    base_seed,
                    seed_source,
                    self.determinism_frozen_ms(),
                    &orig_msg,
                );
                return Err(AsError::at(report, span).into());
            }
        }
        // All iterations held → the property passes.
        Ok(Value::nil())
    }

    /// Draw the argument list for ONE iteration from the resolved generator form.
    async fn draw_args(
        &self,
        form: &GenForm,
        rng: &mut SeededRng,
        span: Span,
    ) -> Result<Vec<Value>, Control> {
        match form {
            GenForm::Array(gens) => {
                let mut args = Vec::with_capacity(gens.len());
                for g in gens {
                    args.push(self.draw_gen(g, rng, 0, span).await?);
                }
                Ok(args)
            }
            GenForm::Object(fields) => {
                let mut m: IndexMap<String, Value> = IndexMap::new();
                for (k, g) in fields {
                    m.insert(k.clone(), self.draw_gen(g, rng, 0, span).await?);
                }
                // Object-form passes a SINGLE object argument to the property fn.
                Ok(vec![Value::object(m)])
            }
        }
    }

    /// Run the property fn once on `args`. Returns `Ok(None)` if the property HELD
    /// (truthy return, no panic/propagate); `Ok(Some(msg))` if it FAILED, where `msg`
    /// describes the failure class (a panic message, a propagated err, or the falsy
    /// note). Misuse of the runner itself surfaces as `Err` (rare).
    ///
    /// For the object form, the args vector already holds the single object argument.
    async fn run_property_once(
        &self,
        fnv: &Value,
        _form: &GenForm,
        args: &[Value],
        span: Span,
    ) -> Result<Option<String>, Control> {
        match self.call_value(fnv.clone(), args.to_vec(), span).await {
            Ok(v) => {
                // A `?` inside the property body returns the `[nil, err]` Tier-1 pair AS
                // the closure's value (function-scoped early return — it does NOT escape
                // the closure as `Control::Propagate`). Per spec §10.4 a propagated error
                // is a FAILURE, so detect that pair shape here.
                if let Some(msg) = tier1_err_message(&v) {
                    Ok(Some(format!("property propagated an error: {msg}")))
                } else if v.is_truthy() {
                    Ok(None)
                } else {
                    Ok(Some("property returned a falsy value".to_string()))
                }
            }
            // A Tier-2 panic inside the body = failure (matches `assert` semantics).
            Err(Control::Panic(e)) => Ok(Some(e.message)),
            // A `?`-propagate that DOES escape (e.g. a body that is not itself a fn
            // boundary) = failure carrying the err.
            Err(Control::Propagate(pair)) => {
                Ok(Some(format!("property propagated an error: {}", propagated_err_msg(&pair))))
            }
            // exit() unwinds the runner (not a property failure).
            Err(Control::Exit(code)) => Err(Control::Exit(code)),
        }
    }

    /// Greedily shrink a failing argument list to a minimal counterexample (§10.5).
    /// Returns the shrunken args plus the number of accepted shrink steps. Each
    /// candidate is re-run with [`run_property_once`]; a candidate is kept ONLY if it
    /// STILL fails. The loop stops at a fixpoint (no smaller failing candidate) or when
    /// `max_shrinks` accepted steps is reached. `max_shrinks == 0` → the original is
    /// returned unshrunk.
    async fn shrink(
        &self,
        fnv: &Value,
        form: &GenForm,
        original: &[Value],
        max_shrinks: u64,
        span: Span,
    ) -> Result<(Vec<Value>, u64), Control> {
        let mut current = original.to_vec();
        let mut shrinks = 0u64;
        if max_shrinks == 0 {
            return Ok((current, 0));
        }
        // Outer fixpoint loop: keep sweeping the argument positions until a full sweep
        // accepts no shrink.
        'outer: loop {
            // Shrink each argument position pointwise (for object form there is one
            // object argument — its fields are shrunk inside shrink_value).
            for pos in 0..current.len() {
                // Generate ordered "smaller" candidates for this position's value.
                let candidates = shrink_value(&current[pos]);
                for cand in candidates {
                    if shrinks >= max_shrinks {
                        break 'outer;
                    }
                    let mut trial = current.clone();
                    trial[pos] = cand;
                    if self.run_property_once(fnv, form, &trial, span).await?.is_some() {
                        // Still fails → accept this smaller value and restart the sweep
                        // (greedy: always re-shrink from the new, smaller counterexample).
                        current = trial;
                        shrinks += 1;
                        continue 'outer;
                    }
                }
            }
            // A full sweep accepted nothing → fixpoint.
            break;
        }
        Ok((current, shrinks))
    }

    /// The base RNG seed carried by the active determinism context (the CLI `--seed`),
    /// or `None` when no det context is installed.
    fn determinism_seed(&self) -> Option<u64> {
        self.determinism_borrow_seed()
    }

    /// The frozen virtual-clock start (ms-epoch) of the active det context, if any —
    /// used for the `frozen-time: T` suffix in the report.
    fn determinism_frozen_ms(&self) -> Option<f64> {
        self.determinism_borrow_frozen_ms()
    }
}

/// Where the base seed came from (drives the report wording + replay recipe).
#[derive(Clone, Copy, PartialEq)]
enum SeedSource {
    Opts,
    Cli,
    Random,
}

/// Deterministic per-iteration seed: mix the base seed with a splitmix64 hash of the
/// iteration index so each iteration is an independent, reproducible stream.
fn seed_for(base: u64, i: u64) -> u64 {
    base ^ splitmix64(i.wrapping_add(0x9E37_79B9_7F4A_7C15))
}

/// A splitmix64 finalizer — a fast, well-distributed integer hash.
fn splitmix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A fresh, non-reproducible seed (used when neither `opts.seed` nor `--seed` is set).
/// PRINTED in the report so the failure is replayable.
fn fresh_random_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    splitmix64(nanos ^ (std::process::id() as u64).wrapping_shl(32))
}

/// Read a `{__prop}` field value.
fn prop_field(v: &Value, key: &str) -> Option<Value> {
    match v.kind() {
        ValueKind::Object(o) => o.get(key),
        _ => None,
    }
}

/// Read a `{__prop}` string field.
fn prop_field_str(v: &Value, key: &str) -> Option<String> {
    match prop_field(v, key).map(|x| x.into_kind()) {
        Some(OwnedKind::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// Read an optional non-negative integer field from an opts Object (`runs`/`seed`/
/// `maxShrinks`). A finite number is truncated; a negative/non-finite/missing → `None`.
/// `seed` may be any u64 (we read it through `as_f64` and reinterpret a negative as its
/// two's-complement via `as i64 as u64`, so a literal i64 seed round-trips).
fn opt_u64_field(opts: &Value, field: &str) -> Option<u64> {
    let o = match opts.kind() {
        ValueKind::Object(o) => o,
        _ => return None,
    };
    let v = o.get(field)?;
    match v.kind() {
        ValueKind::Int(n) => Some(n as u64),
        ValueKind::Float(f) if f.is_finite() && f >= 0.0 => Some(f as u64),
        _ => None,
    }
}

/// If `v` is a Tier-1 error pair `[value, err]` whose `err` slot is a NON-nil error
/// (the shape a `?`-propagation produces as a closure's return value), return the err
/// message. A success pair `[value, nil]` returns `None` (not a failure), as does any
/// non-pair value (a plain bool/number property result). This is how the prop runner
/// observes a `?`-propagation, since `?` returns the pair as the closure value rather
/// than escaping as `Control::Propagate`.
fn tier1_err_message(v: &Value) -> Option<String> {
    let a = match v.kind() {
        ValueKind::Array(a) => a,
        _ => return None,
    };
    let v = a.borrow();
    if v.len() != 2 {
        return None;
    }
    // The err slot must be non-nil to be a failure.
    match v[1].kind() {
        ValueKind::Nil => None,
        ValueKind::Object(o) => match o.get("message").map(|m| m.into_kind()) {
            Some(OwnedKind::Str(s)) => Some(s.to_string()),
            _ => Some(v[1].to_string()),
        },
        _ => Some(v[1].to_string()),
    }
}

/// The err message carried by a `?`-propagated `[nil, err]` pair (best-effort).
fn propagated_err_msg(pair: &Value) -> String {
    if let ValueKind::Array(a) = pair.kind() {
        let v = a.borrow();
        if v.len() == 2 {
            if let ValueKind::Object(o) = v[1].kind() {
                if let Some(OwnedKind::Str(s)) = o.get("message").map(|m| m.into_kind()) {
                    return s.to_string();
                }
            }
            return v[1].to_string();
        }
    }
    pair.to_string()
}

/// Produce an ordered list of "smaller" candidate values for `v` (§10.5). The greedy
/// shrink driver tries each in order, keeping the first that STILL fails. The ordering
/// is most-aggressive-first (closest to the simplest value) so the driver takes big
/// jumps when they keep failing and converges to the boundary with `-1`/tail steps.
fn shrink_value(v: &Value) -> Vec<Value> {
    match v.kind() {
        ValueKind::Int(n) => shrink_int(n).into_iter().map(Value::int).collect(),
        ValueKind::Float(f) => shrink_float(f).into_iter().map(Value::float).collect(),
        ValueKind::Str(s) => shrink_string(s),
        ValueKind::Bool(true) => vec![Value::bool_(false)],
        ValueKind::Array(a) => shrink_array(&a.borrow()),
        ValueKind::Object(o) => shrink_object(&o.entries()),
        _ => vec![],
    }
}

/// Integer shrink: toward 0 by halving the DISTANCE, then `-1` steps to reach the exact
/// boundary (so `x <= 99` over a wide range converges to exactly 100). Most-aggressive
/// first: `0`, then `n/2`, then `n - 1`. The greedy driver re-shrinks from each accepted
/// value, so the halving chain (837 → 418 → … → 101) is followed by a single `-1` to 100.
fn shrink_int(n: i64) -> Vec<i64> {
    if n == 0 {
        return vec![];
    }
    let mut out = Vec::new();
    // Toward 0 (drop the value entirely if it still fails at 0).
    out.push(0);
    // Halve the distance to 0 (rounds toward 0).
    let half = n / 2;
    if half != 0 && half != n {
        out.push(half);
    }
    // The single-step neighbor toward 0 (the boundary-finder).
    let neighbor = if n > 0 { n - 1 } else { n + 1 };
    if neighbor != 0 && neighbor != half {
        out.push(neighbor);
    }
    out.dedup();
    out
}

/// Float shrink: toward 0.0 (try 0.0, then halve, then snap to the nearest integer if
/// that is a strictly-simpler distinct value).
fn shrink_float(f: f64) -> Vec<f64> {
    if f == 0.0 || !f.is_finite() {
        return vec![];
    }
    let mut out = vec![0.0];
    let half = f / 2.0;
    if half != f {
        out.push(half);
    }
    let truncated = f.trunc();
    if truncated != f && truncated.abs() < f.abs() {
        out.push(truncated);
    }
    out
}

/// String shrink: drop the tail by halves (toward ""), then shrink the remaining run by
/// removing one trailing char. Most-aggressive first ("" then halves) so `!contains("ab")`
/// converges to exactly `"ab"` (the shortest still-failing substring).
fn shrink_string(s: &str) -> Vec<Value> {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    if len == 0 {
        return vec![];
    }
    let mut out: Vec<Value> = Vec::new();
    // "" — drop everything.
    out.push(Value::str(""));
    // Keep the first half (drop the tail half).
    let half = len / 2;
    if half > 0 && half < len {
        out.push(Value::str(chars[..half].iter().collect::<String>()));
    }
    // Keep the SECOND half (drop the head half) — lets the failing substring survive when
    // it lives in the tail.
    if half > 0 && half < len {
        out.push(Value::str(chars[half..].iter().collect::<String>()));
    }
    // Drop a single trailing char (boundary-finder toward the minimal substring).
    out.push(Value::str(chars[..len - 1].iter().collect::<String>()));
    // Drop a single LEADING char.
    out.push(Value::str(chars[1..].iter().collect::<String>()));
    out
}

/// Array shrink: drop the tail by halves (toward []), then drop a single trailing
/// element, then shrink the first element pointwise.
fn shrink_array(items: &[Value]) -> Vec<Value> {
    let len = items.len();
    if len == 0 {
        return vec![];
    }
    let mut out: Vec<Value> = Vec::new();
    out.push(Value::array(vec![])); // []
    let half = len / 2;
    if half > 0 && half < len {
        out.push(Value::array(items[..half].to_vec()));
        out.push(Value::array(items[half..].to_vec()));
    }
    if len >= 1 {
        out.push(Value::array(items[..len - 1].to_vec())); // drop last
    }
    // Pointwise: shrink the first element (one candidate each), keeping the rest.
    for cand in shrink_value(&items[0]) {
        let mut a = items.to_vec();
        a[0] = cand;
        out.push(Value::array(a));
    }
    out
}

/// Object shrink: the shape is fixed (objectWith), so shrink field VALUES pointwise.
/// `__gen`/`__prop` keys never appear in a drawn object.
fn shrink_object(entries: &[(std::rc::Rc<str>, Value)]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for (i, (_k, v)) in entries.iter().enumerate() {
        for cand in shrink_value(v) {
            let mut m: IndexMap<String, Value> = IndexMap::new();
            for (j, (kk, vv)) in entries.iter().enumerate() {
                if j == i {
                    m.insert(kk.to_string(), cand.clone());
                } else {
                    m.insert(kk.to_string(), vv.clone());
                }
            }
            out.push(Value::object(m));
        }
    }
    out
}

/// Render a counterexample argument for the report. Strings are QUOTED (so `""` and a
/// trailing space are visible, and the minimal `"ab"` reads unambiguously); arrays and
/// objects recurse with quoted string elements; everything else uses the plain
/// value display.
fn render_value(v: &Value) -> String {
    match v.kind() {
        ValueKind::Str(s) => format!("{s:?}"),
        ValueKind::Array(a) => {
            let parts: Vec<String> = a.borrow().iter().map(render_value).collect();
            format!("[{}]", parts.join(", "))
        }
        ValueKind::Object(o) => {
            let parts: Vec<String> = o
                .entries()
                .iter()
                .map(|(k, vv)| format!("{k}: {}", render_value(vv)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        _ => v.to_string(),
    }
}

/// Build the §10.3 failure report (the panic message that the summary prints verbatim).
/// Multi-line body, lowercase headline, no trailing period (the DX guide).
#[allow(clippy::too_many_arguments)]
fn format_report(
    name: &str,
    form: &GenForm,
    shrunk: &[Value],
    iteration: u64,
    shrinks: u64,
    seed: u64,
    seed_source: SeedSource,
    frozen_ms: Option<f64>,
    orig_msg: &str,
) -> String {
    let mut s = String::new();
    s.push_str("property failed");
    // Counterexample — positional (a, b, …) or named (a: …, b: …).
    s.push_str("\n  counterexample: ");
    match form {
        GenForm::Array(_) => {
            let parts: Vec<String> = shrunk.iter().map(render_value).collect();
            s.push_str(&parts.join(", "));
        }
        GenForm::Object(fields) => {
            // The drawn object is the single arg; render name: value pairs.
            let obj = shrunk.first().cloned().unwrap_or(Value::nil());
            let parts: Vec<String> = match obj.kind() {
                ValueKind::Object(o) => fields
                    .iter()
                    .map(|(k, _)| {
                        let val = o.get(k).map(|v| render_value(&v)).unwrap_or_else(|| "nil".into());
                        format!("{k}: {val}")
                    })
                    .collect(),
                _ => vec![render_value(&obj)],
            };
            s.push_str(&parts.join(", "));
        }
    }
    s.push_str(&format!("\n  failing iteration: {iteration}"));
    s.push_str(&format!("\n  shrinks: {shrinks}"));
    s.push_str(&format!("\n  reason: {orig_msg}"));
    // The seed line (§10.3) — note the source so a random seed reads as such.
    let seed_note = match seed_source {
        SeedSource::Opts => " (from opts.seed)",
        SeedSource::Cli => " (from --seed)",
        SeedSource::Random => " (random — pass it to reproduce)",
    };
    s.push_str(&format!("\n  seed: {seed}{seed_note}"));
    if let Some(t) = frozen_ms {
        s.push_str(&format!("\n  frozen-time: {t}"));
    }
    // The replay invocation, verbatim (§10.3).
    let mut replay = format!("ascript test <file> --seed {seed}");
    if let Some(t) = frozen_ms {
        replay.push_str(&format!(" --frozen-time {t}"));
    }
    replay.push_str(&format!(" --filter \"{name}\""));
    s.push_str(&format!("\n  replay: {replay}"));
    s
}

// ─────────────────────────────────────────────────────────────────────────────
// the seeded drawing primitives (edge-biased)
// ─────────────────────────────────────────────────────────────────────────────

/// Draw an integer in `[min, max]` with edge bias: roughly 1 draw in 4 comes from
/// the BOUNDARY POOL (`{min, max, 0, ±1}` clamped into range) instead of a uniform
/// draw. This is the fuzzgen philosophy surfaced (`src/fuzzgen/`): boundary values
/// far more often than uniform sampling, because bugs cluster at boundaries.
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
