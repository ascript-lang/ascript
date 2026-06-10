//! `std/assert` — rich assertion helpers for test code.
//!
//! Each assertion passes silently on success and raises a Tier-2 panic
//! (`Control::Panic`) with a descriptive, value-showing message on failure.
//!
//! All container comparisons (`eq`, `ne`, `contains`) use structural
//! deep equality via `object::deep_equal`, so `assert.eq([1,2],[1,2])` passes
//! even though the two arrays are distinct heap objects.
//!
//! `assert.throws(fn) -> errValue` is async: it calls `fn`, drives any
//! returned `Value::Future` to completion, then checks whether the call
//! raised a `Control::Panic`.  On success it returns the caught error value
//! (the `{ message }` object that `recover` would have returned).  If `fn`
//! completes without panicking, `assert.throws` itself panics.

use super::arg;
use crate::error::AsError;
use crate::interp::{make_error, Control, Interp};
use crate::span::Span;
use crate::stdlib::object::deep_equal;
use crate::value::Value;
use rust_decimal::prelude::ToPrimitive;

// ── structural diff (spec §6.5) ──────────────────────────────────────────────

/// Maximum recursion depth for the structural diff. Beyond this depth the diff
/// stops descending and reports the differing node flatly. This is a hard guard
/// against pathologically deep (and, together with the visited set, cyclic)
/// structures — the diff NEVER stack-overflows or loops, it degrades to a flat
/// `expected ... got ...` line for the over-deep node.
const DIFF_MAX_DEPTH: usize = 64;

/// Produce a human-readable, path-qualified structural diff between two values
/// whose semantics mirror `object::deep_equal` EXACTLY: if `deep_equal(a, b)`
/// is `true` the returned string is empty; otherwise it lists, recursively, the
/// per-Object-key add/remove/change and per-Array-index change (and length
/// differences) that make them unequal.
///
/// Path syntax: `.key` for object keys, `[i]` for array indices, e.g.
/// `.users[0].name: "a" → "b"`. Lines:
///   - `<path>: <old> → <new>`   a CHANGE (both present, unequal)
///   - `+ <path>: <new>`         an ADD (present in `b`, absent in `a`)
///   - `- <path>: <old>`         a REMOVE (present in `a`, absent in `b`)
///
/// Cycle/over-depth safe: a visited-pair set (mirroring `deep_equal`'s `seen`)
/// short-circuits a back-edge as "equal" (matching `deep_equal`), and a depth
/// bound degrades to a flat node-level change line.
pub(crate) fn structural_diff(a: &Value, b: &Value) -> String {
    let mut out = String::new();
    let mut seen: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    diff_inner(a, b, &mut String::new(), 0, &mut seen, &mut out);
    out
}

/// Emit a single diff line for a CHANGE at `path` (root path renders as `(root)`).
fn diff_change_line(path: &str, old: &Value, new: &Value, out: &mut String) {
    let p = if path.is_empty() { "(root)" } else { path };
    out.push_str(&format!("{}: {} → {}\n", p, old, new));
}

fn diff_inner(
    a: &Value,
    b: &Value,
    path: &mut String,
    depth: usize,
    seen: &mut std::collections::HashSet<(usize, usize)>,
    out: &mut String,
) {
    // Equal per deep_equal ⟺ no diff. Checked first so number/MapKey/etc.
    // canonicalization is honoured exactly (e.g. 1 == 1.0 if deep_equal says so).
    if deep_equal(a, b) {
        return;
    }
    // Depth guard: degrade to a flat node-level change beyond the bound.
    if depth >= DIFF_MAX_DEPTH {
        diff_change_line(path, a, b, out);
        return;
    }
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            // Cycle guard mirroring deep_equal: a revisited pair is "equal".
            if !seen.insert((crate::gc::cc_addr(x), crate::gc::cc_addr(y))) {
                return;
            }
            let (x, y) = (x.borrow(), y.borrow());
            let n = x.len().min(y.len());
            for i in 0..n {
                let base = path.len();
                path.push_str(&format!("[{}]", i));
                diff_inner(&x[i], &y[i], path, depth + 1, seen, out);
                path.truncate(base);
            }
            // Length difference: extra removed / added indices.
            for i in n..x.len() {
                out.push_str(&format!("- {}[{}]: {}\n", path, i, x[i]));
            }
            for i in n..y.len() {
                out.push_str(&format!("+ {}[{}]: {}\n", path, i, y[i]));
            }
        }
        (Value::Object(x), Value::Object(y)) => {
            if !seen.insert((crate::gc::cc_addr(x), crate::gc::cc_addr(y))) {
                return;
            }
            let (x, y) = (x.borrow(), y.borrow());
            // Removed / changed keys, in `a`'s insertion order.
            for (k, va) in x.iter() {
                match y.get(k) {
                    Some(vb) => {
                        let base = path.len();
                        path.push('.');
                        path.push_str(k);
                        diff_inner(va, vb, path, depth + 1, seen, out);
                        path.truncate(base);
                    }
                    None => out.push_str(&format!("- {}.{}: {}\n", path, k, va)),
                }
            }
            // Added keys, in `b`'s insertion order.
            for (k, vb) in y.iter() {
                if !x.contains_key(k) {
                    out.push_str(&format!("+ {}.{}: {}\n", path, k, vb));
                }
            }
        }
        // Scalars, Maps, Sets, Instances, mismatched kinds, etc.: report flat.
        // (Maps/Sets/Instances are deep-equality containers but a keyed/ordered
        // diff over MapKey/Set is not in the §6.5 surface — a node-level change
        // line is the readable, deterministic choice.)
        _ => diff_change_line(path, a, b, out),
    }
}

// ── snapshot_impl (sys-gated; testable pure helper) ──────────────────────────

/// Core snapshot logic, parameterized by `dir` (base dir for `__snapshots__/`),
/// `name` (snapshot name), `serialized` (the value already stringified to JSON),
/// and `update` (whether to overwrite an existing snapshot).
///
/// Returns `Ok(())` on pass (first-run write, match, or update-mode write) or
/// `Err(message)` on mismatch.
///
/// Gated behind `cfg(all(feature = "sys", feature = "data"))`: requires `sys`
/// (filesystem I/O) and `data` (serde_json for pretty serialization).
#[cfg(all(feature = "sys", feature = "data"))]
pub(crate) fn snapshot_impl(
    dir: &std::path::Path,
    name: &str,
    serialized: &str,
    update: bool,
) -> Result<(), String> {
    // Sanitize the snapshot name: replace filesystem-unsafe chars with `_`.
    let safe_name: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();

    let snap_dir = dir.join("__snapshots__");
    let snap_file = snap_dir.join(format!("{}.snap", safe_name));

    if snap_file.exists() && !update {
        // Compare with stored snapshot.
        let stored = std::fs::read_to_string(&snap_file)
            .map_err(|e| format!("assert.snapshot: could not read snapshot file: {}", e))?;
        if stored != serialized {
            // Try a structural diff over the two JSON payloads (parse → Value →
            // structural_diff). Both were produced by serde_json::to_string_pretty
            // over json::from_ascript output, so they normally re-parse cleanly;
            // if either fails to parse, fall back to the raw text dump.
            let structural = match (
                serde_json::from_str::<serde_json::Value>(&stored),
                serde_json::from_str::<serde_json::Value>(serialized),
            ) {
                (Ok(sj), Ok(nj)) => {
                    let sv = crate::stdlib::json::to_ascript(&sj);
                    let nv = crate::stdlib::json::to_ascript(&nj);
                    let d = structural_diff(&sv, &nv);
                    if d.trim().is_empty() {
                        None
                    } else {
                        Some(d)
                    }
                }
                _ => None,
            };
            return Err(match structural {
                Some(d) => format!(
                    "assert.snapshot '{}' mismatch:\ndiff (stored → new):\n{}\n--- stored ---\n{}\n--- new ---\n{}",
                    name,
                    d.trim_end(),
                    stored,
                    serialized
                ),
                None => format!(
                    "assert.snapshot '{}' mismatch:\n--- stored ---\n{}\n--- new ---\n{}",
                    name, stored, serialized
                ),
            });
        }
        Ok(())
    } else {
        // First run (file absent) or update mode: write the snapshot.
        std::fs::create_dir_all(&snap_dir)
            .map_err(|e| format!("assert.snapshot: could not create __snapshots__ dir: {}", e))?;
        std::fs::write(&snap_file, serialized)
            .map_err(|e| format!("assert.snapshot: could not write snapshot file: {}", e))?;
        Ok(())
    }
}

pub fn exports() -> Vec<(&'static str, Value)> {
    use super::bi;
    #[allow(unused_mut)]
    let mut v = vec![
        ("eq", bi("assert.eq")),
        ("deepEq", bi("assert.deepEq")),
        ("ne", bi("assert.ne")),
        ("isTrue", bi("assert.isTrue")),
        ("isFalse", bi("assert.isFalse")),
        ("isNil", bi("assert.isNil")),
        ("notNil", bi("assert.notNil")),
        ("gt", bi("assert.gt")),
        ("gte", bi("assert.gte")),
        ("lt", bi("assert.lt")),
        ("lte", bi("assert.lte")),
        ("contains", bi("assert.contains")),
        ("approxEq", bi("assert.approxEq")),
        ("throws", bi("assert.throws")),
        ("throwsWith", bi("assert.throwsWith")),
    ];
    // assert.matches requires `data` (Value::Regex + the regex crate).
    #[cfg(feature = "data")]
    v.push(("matches", bi("assert.matches")));
    // assert.snapshot requires both `sys` (filesystem I/O) and `data` (JSON
    // serialization via serde_json).
    #[cfg(all(feature = "sys", feature = "data"))]
    v.push(("snapshot", bi("assert.snapshot")));
    v
}

/// Build the `assert.eq`/`assert.deepEq` failure message. When the structural
/// diff is non-empty (containers differ) it is appended for readability;
/// otherwise (scalar-vs-scalar or differing kinds) the flat `a != b` line is
/// enough.
fn eq_fail_message(a: &Value, b: &Value) -> String {
    let base = format!("assert.eq failed: {} != {}", a, b);
    let diff = structural_diff(a, b);
    if diff.trim().is_empty() {
        base
    } else {
        format!("{}\ndiff (expected → actual):\n{}", base, diff.trim_end())
    }
}

/// Helper: format a panic error with an optional user message prefix.
fn fail(base: impl Into<String>, user_msg: Option<&str>, span: Span) -> Control {
    let msg = match user_msg {
        Some(m) if !m.is_empty() => format!("{}: {}", m, base.into()),
        _ => base.into(),
    };
    AsError::at(msg, span).into()
}

impl Interp {
    /// Dispatch for `assert.*` builtin calls.
    pub(crate) async fn call_assert(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            // ── assert.eq(a, b, msg?) ─────────────────────────────────────────
            "eq" => {
                let a = arg(args, 0);
                let b = arg(args, 1);
                let msg = opt_str(args, 2);
                if !deep_equal(&a, &b) {
                    return Err(fail(eq_fail_message(&a, &b), msg.as_deref(), span));
                }
                Ok(Value::Nil)
            }
            // ── assert.deepEq(a, b, msg?) — alias for assert.eq ──────────────
            //
            // `assert.eq` is ALREADY structural (deep_equal). `deepEq` is a
            // named alias that makes the deep-equality intent explicit; it
            // shares the exact impl + the structural-diff failure message.
            "deepEq" => {
                let a = arg(args, 0);
                let b = arg(args, 1);
                let msg = opt_str(args, 2);
                if !deep_equal(&a, &b) {
                    return Err(fail(eq_fail_message(&a, &b), msg.as_deref(), span));
                }
                Ok(Value::Nil)
            }
            // ── assert.matches(value, regex) ─────────────────────────────────
            //
            // Assert a string `value` matches a regex (a `Value::Regex` or a
            // pattern string, compiled on the fly — same convention as
            // `regex.test`). A non-string value or non-match fails clearly.
            //
            // Gated on `data`: `Value::Regex` and the `regex` crate only exist
            // with it. Under `--no-default-features` `matches` is absent from the
            // exports and falls through to the catch-all "has no function" panic.
            #[cfg(feature = "data")]
            "matches" => {
                let value = arg(args, 0);
                let s = match &value {
                    Value::Str(s) => s.to_string(),
                    _ => {
                        return Err(AsError::at(
                            format!(
                                "assert.matches: value must be a string, got {}",
                                crate::interp::type_name(&value)
                            ),
                            span,
                        )
                        .into())
                    }
                };
                let pat_val = arg(args, 1);
                let (re, pat_src) = match &pat_val {
                    Value::Regex(r) => (r.re.clone(), r.source.clone()),
                    Value::Str(p) => match regex::Regex::new(p) {
                        Ok(re) => (re, p.to_string()),
                        Err(e) => {
                            return Err(AsError::at(
                                format!("assert.matches: invalid regex pattern: {}", e),
                                span,
                            )
                            .into())
                        }
                    },
                    _ => {
                        return Err(AsError::at(
                            format!(
                                "assert.matches: pattern must be a regex or string, got {}",
                                crate::interp::type_name(&pat_val)
                            ),
                            span,
                        )
                        .into())
                    }
                };
                if !re.is_match(&s) {
                    return Err(fail(
                        format!(
                            "assert.matches failed: {:?} does not match /{}/",
                            s, pat_src
                        ),
                        None,
                        span,
                    ));
                }
                Ok(Value::Nil)
            }
            // ── assert.ne(a, b, msg?) ─────────────────────────────────────────
            "ne" => {
                let a = arg(args, 0);
                let b = arg(args, 1);
                let msg = opt_str(args, 2);
                if deep_equal(&a, &b) {
                    return Err(fail(
                        format!("assert.ne failed: both equal {}", a),
                        msg.as_deref(),
                        span,
                    ));
                }
                Ok(Value::Nil)
            }
            // ── assert.isTrue(x) ──────────────────────────────────────────────
            "isTrue" => {
                let x = arg(args, 0);
                if !x.is_truthy() {
                    return Err(fail(
                        format!("assert.isTrue failed: {} is falsy", x),
                        None,
                        span,
                    ));
                }
                Ok(Value::Nil)
            }
            // ── assert.isFalse(x) ─────────────────────────────────────────────
            "isFalse" => {
                let x = arg(args, 0);
                if x.is_truthy() {
                    return Err(fail(
                        format!("assert.isFalse failed: {} is truthy", x),
                        None,
                        span,
                    ));
                }
                Ok(Value::Nil)
            }
            // ── assert.isNil(x) ───────────────────────────────────────────────
            "isNil" => {
                let x = arg(args, 0);
                if x != Value::Nil {
                    return Err(fail(
                        format!("assert.isNil failed: expected nil, got {}", x),
                        None,
                        span,
                    ));
                }
                Ok(Value::Nil)
            }
            // ── assert.notNil(x) ──────────────────────────────────────────────
            "notNil" => {
                let x = arg(args, 0);
                if x == Value::Nil {
                    return Err(fail("assert.notNil failed: got nil", None, span));
                }
                Ok(Value::Nil)
            }
            // ── assert.gt / gte / lt / lte ────────────────────────────────────
            "gt" | "gte" | "lt" | "lte" => {
                let a = arg(args, 0);
                let b = arg(args, 1);
                let (an, bn) = numeric_pair(&a, &b, func, span)?;
                let ok = match func {
                    "gt" => an > bn,
                    "gte" => an >= bn,
                    "lt" => an < bn,
                    "lte" => an <= bn,
                    _ => unreachable!(),
                };
                if !ok {
                    return Err(fail(
                        format!("assert.{} failed: {} {} {}", func, a, cmp_op(func), b),
                        None,
                        span,
                    ));
                }
                Ok(Value::Nil)
            }
            // ── assert.contains(haystack, needle) ─────────────────────────────
            "contains" => {
                let haystack = arg(args, 0);
                let needle = arg(args, 1);
                let found = match &haystack {
                    Value::Str(s) => {
                        // needle must be a string for substring search
                        match &needle {
                            Value::Str(n) => s.contains(n.as_ref()),
                            _ => {
                                return Err(AsError::at(
                                    format!(
                                        "assert.contains: string haystack needs a string needle, got {}",
                                        crate::interp::type_name(&needle)
                                    ),
                                    span,
                                )
                                .into())
                            }
                        }
                    }
                    Value::Array(a) => {
                        // membership by == (Value PartialEq)
                        a.borrow().iter().any(|elem| deep_equal(elem, &needle))
                    }
                    Value::Object(o) => {
                        // key presence — needle must be a string
                        match &needle {
                            Value::Str(k) => o.borrow().contains_key(k.as_ref()),
                            _ => {
                                return Err(AsError::at(
                                    format!(
                                        "assert.contains: object haystack needs a string key, got {}",
                                        crate::interp::type_name(&needle)
                                    ),
                                    span,
                                )
                                .into())
                            }
                        }
                    }
                    Value::Map(m) => {
                        // key presence — needle must be a hashable map key
                        match crate::value::MapKey::from_value(&needle) {
                            Some(k) => m.borrow().contains_key(&k),
                            None => {
                                return Err(AsError::at(
                                    format!(
                                        "assert.contains: map haystack needs a hashable key, got {}",
                                        crate::interp::type_name(&needle)
                                    ),
                                    span,
                                )
                                .into())
                            }
                        }
                    }
                    _ => {
                        return Err(AsError::at(
                            format!(
                                "assert.contains expects a string, array, object, or map as haystack, got {}",
                                crate::interp::type_name(&haystack)
                            ),
                            span,
                        )
                        .into())
                    }
                };
                if !found {
                    return Err(fail(
                        format!("assert.contains failed: {} not in {}", needle, haystack),
                        None,
                        span,
                    ));
                }
                Ok(Value::Nil)
            }
            // ── assert.approxEq(a, b, epsilon?) ──────────────────────────────
            "approxEq" => {
                let a = arg(args, 0);
                let b = arg(args, 1);
                // Both Number and Decimal are accepted (consistent with gt/gte/lt/lte).
                let (an, bn) = numeric_pair(&a, &b, "approxEq", span)?;
                let epsilon = match arg(args, 2) {
                    Value::Nil => 1e-9_f64,
                    // NUM §4: accept BOTH numeric subtypes.
                    ref v if v.is_number() => v.as_f64().unwrap_or(f64::NAN),
                    Value::Decimal(d) => d.to_f64().unwrap_or(f64::NAN),
                    v => {
                        return Err(AsError::at(
                            format!(
                                "assert.approxEq epsilon expects a number, got {}",
                                crate::interp::type_name(&v)
                            ),
                            span,
                        )
                        .into())
                    }
                };
                if (an - bn).abs() > epsilon {
                    return Err(fail(
                        format!(
                            "assert.approxEq failed: |{} - {}| = {} > epsilon {}",
                            a,
                            b,
                            (an - bn).abs(),
                            epsilon
                        ),
                        None,
                        span,
                    ));
                }
                Ok(Value::Nil)
            }
            // ── assert.throws(fn) -> errValue ─────────────────────────────────
            //
            // Calls fn with no arguments.  If fn returns a Value::Future (i.e.
            // it is an async fn), that future is driven to completion before
            // checking whether a panic occurred.  Pattern mirrors `recover` +
            // the `task.retry` future-drive idiom.
            "throws" => {
                let callee = arg(args, 0);
                let call_result = self.call_value(callee, vec![], span).await;
                // Drive any returned future to completion (async fn path).
                let result: Result<Value, Control> = match call_result {
                    Ok(Value::Future(f)) => f.get().await,
                    other => other,
                };
                match result {
                    Err(Control::Panic(e)) => {
                        // Return the error value (same shape recover returns).
                        Ok(make_error(Value::Str(e.message.into())))
                    }
                    Err(other) => {
                        // Propagate / Exit pass through unchanged.
                        Err(other)
                    }
                    Ok(_) => {
                        // fn returned normally — assert.throws should panic.
                        Err(AsError::at(
                            "assert.throws failed: expected fn to throw, but it returned normally",
                            span,
                        )
                        .into())
                    }
                }
            }
            // ── assert.throwsWith(fn, substr) -> errValue ────────────────────
            //
            // Like assert.throws, but ALSO asserts the recovered error message
            // CONTAINS `substr`. A throw with a non-matching message fails
            // (showing the actual message); NO throw fails. Mirrors the async
            // shape of assert.throws exactly.
            "throwsWith" => {
                let callee = arg(args, 0);
                let needle = match arg(args, 1) {
                    Value::Str(s) => s.to_string(),
                    v => {
                        return Err(AsError::at(
                            format!(
                                "assert.throwsWith: substr must be a string, got {}",
                                crate::interp::type_name(&v)
                            ),
                            span,
                        )
                        .into())
                    }
                };
                let call_result = self.call_value(callee, vec![], span).await;
                let result: Result<Value, Control> = match call_result {
                    Ok(Value::Future(f)) => f.get().await,
                    other => other,
                };
                match result {
                    Err(Control::Panic(e)) => {
                        let actual = e.message.clone();
                        if actual.contains(&needle) {
                            Ok(make_error(Value::Str(actual.into())))
                        } else {
                            Err(AsError::at(
                                format!(
                                    "assert.throwsWith failed: expected error message to contain {:?}, but got {:?}",
                                    needle, actual
                                ),
                                span,
                            )
                            .into())
                        }
                    }
                    Err(other) => Err(other),
                    Ok(_) => Err(AsError::at(
                        "assert.throwsWith failed: expected fn to throw, but it returned normally",
                        span,
                    )
                    .into()),
                }
            }
            // ── assert.snapshot(name, value) ─────────────────────────────────
            //
            // Gated behind `cfg(all(feature = "sys", feature = "data"))`: both
            // sys (fs I/O) and data (serde_json) are required. Under
            // --no-default-features the name falls through to the `_` catch-all
            // → "assert has no function 'snapshot'" (correct behaviour).
            #[cfg(all(feature = "sys", feature = "data"))]
            "snapshot" => {
                let name_val = arg(args, 0);
                let name = match &name_val {
                    Value::Str(s) => s.to_string(),
                    _ => {
                        return Err(AsError::at(
                            format!(
                                "assert.snapshot: name must be a string, got {}",
                                crate::interp::type_name(&name_val)
                            ),
                            span,
                        )
                        .into())
                    }
                };
                let value = arg(args, 1);

                // Serialize the value to a stable pretty-JSON string.
                // We call json::from_ascript directly (no std/json needed at runtime).
                let serialized = match crate::stdlib::json::from_ascript(&value, &mut Vec::new()) {
                    Ok(jv) => match serde_json::to_string_pretty(&jv) {
                        Ok(s) => s,
                        Err(e) => {
                            return Err(AsError::at(
                                format!("assert.snapshot: cannot serialize value: {}", e),
                                span,
                            )
                            .into())
                        }
                    },
                    Err(msg) => {
                        return Err(AsError::at(
                            format!("assert.snapshot: cannot serialize value: {}", msg),
                            span,
                        )
                        .into())
                    }
                };

                // Check env var for update mode (non-empty string → update).
                let update = std::env::var("ASCRIPT_UPDATE_SNAPSHOTS")
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);

                // Resolve snapshot dir relative to cwd.
                let cwd = std::env::current_dir().map_err(|e| {
                    AsError::at(
                        format!("assert.snapshot: cannot determine cwd: {}", e),
                        span,
                    )
                })?;

                match snapshot_impl(&cwd, &name, &serialized, update) {
                    Ok(()) => Ok(Value::Nil),
                    Err(msg) => Err(AsError::at(msg, span).into()),
                }
            }
            _ => Err(AsError::at(format!("assert has no function '{}'", func), span).into()),
        }
    }
}

// ── private helpers ──────────────────────────────────────────────────────────

/// Extract an optional user-provided message string (3rd/2nd arg etc.).
fn opt_str(args: &[Value], i: usize) -> Option<String> {
    match args.get(i) {
        Some(Value::Str(s)) => Some(s.to_string()),
        Some(Value::Nil) | None => None,
        Some(v) => Some(v.to_string()),
    }
}

/// Unwrap both values as numbers; panics with a clear message if either is not.
fn numeric_pair(a: &Value, b: &Value, func: &str, span: Span) -> Result<(f64, f64), Control> {
    // NUM §4: accept BOTH numeric subtypes (and Decimal).
    let an = match a {
        v if v.is_number() => v.as_f64().unwrap_or(f64::NAN),
        Value::Decimal(d) => d.to_f64().unwrap_or(f64::NAN),
        _ => {
            return Err(AsError::at(
                format!(
                    "assert.{} expects numbers, got {} for first argument",
                    func,
                    crate::interp::type_name(a)
                ),
                span,
            )
            .into())
        }
    };
    let bn = match b {
        v if v.is_number() => v.as_f64().unwrap_or(f64::NAN),
        Value::Decimal(d) => d.to_f64().unwrap_or(f64::NAN),
        _ => {
            return Err(AsError::at(
                format!(
                    "assert.{} expects numbers, got {} for second argument",
                    func,
                    crate::interp::type_name(b)
                ),
                span,
            )
            .into())
        }
    };
    Ok((an, bn))
}

fn cmp_op(func: &str) -> &'static str {
    match func {
        "gt" => ">",
        "gte" => ">=",
        "lt" => "<",
        "lte" => "<=",
        _ => "?",
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    async fn run(src: &str) -> String {
        crate::run_source(src)
            .await
            .expect("program should succeed")
    }

    async fn run_err(src: &str) -> String {
        crate::run_source(src)
            .await
            .expect_err("program should fail")
            .message
    }

    // ── assert.eq ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn eq_primitives_pass() {
        run(r#"
import * as assert from "std/assert"
assert.eq(1, 1)
assert.eq("hello", "hello")
assert.eq(true, true)
assert.eq(nil, nil)
"#)
        .await;
    }

    #[tokio::test]
    async fn eq_deep_arrays_pass() {
        run(r#"
import * as assert from "std/assert"
assert.eq([1, 2, 3], [1, 2, 3])
assert.eq([[1], [2]], [[1], [2]])
"#)
        .await;
    }

    #[tokio::test]
    async fn eq_deep_objects_pass() {
        run(r#"
import * as assert from "std/assert"
assert.eq({a: 1, b: 2}, {a: 1, b: 2})
"#)
        .await;
    }

    #[tokio::test]
    async fn eq_fails_with_panic() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.eq([1], [2]))
print(r[1] != nil)
print(r[1].message)
"#;
        let out = run(src).await;
        assert!(
            out.starts_with("true\n"),
            "expected 'true' first line, got: {out}"
        );
        assert!(
            out.contains("assert.eq failed"),
            "expected 'assert.eq failed' in: {out}"
        );
    }

    #[tokio::test]
    async fn eq_with_user_msg() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.eq(1, 2, "my message"))
print(r[1].message)
"#;
        let out = run(src).await;
        assert!(out.contains("my message"), "expected user msg in: {out}");
    }

    // ── assert.ne ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn ne_pass() {
        run(r#"
import * as assert from "std/assert"
assert.ne(1, 2)
assert.ne([1], [2])
"#)
        .await;
    }

    #[tokio::test]
    async fn ne_fails_when_equal() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.ne([1, 2], [1, 2]))
print(r[1] != nil)
"#;
        let out = run(src).await;
        assert_eq!(out, "true\n");
    }

    // ── assert.isTrue / assert.isFalse ──────────────────────────────────────

    #[tokio::test]
    async fn is_true_pass() {
        run(r#"
import * as assert from "std/assert"
assert.isTrue(1)
assert.isTrue("x")
assert.isTrue([])
"#)
        .await;
    }

    #[tokio::test]
    async fn is_true_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.isTrue(false))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    #[tokio::test]
    async fn is_false_pass() {
        run(r#"
import * as assert from "std/assert"
assert.isFalse(false)
assert.isFalse(nil)
"#)
        .await;
    }

    #[tokio::test]
    async fn is_false_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.isFalse(1))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    // ── assert.isNil / assert.notNil ────────────────────────────────────────

    #[tokio::test]
    async fn is_nil_pass() {
        run(r#"
import * as assert from "std/assert"
assert.isNil(nil)
"#)
        .await;
    }

    #[tokio::test]
    async fn is_nil_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.isNil(5))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    #[tokio::test]
    async fn not_nil_pass() {
        run(r#"
import * as assert from "std/assert"
assert.notNil(5)
assert.notNil(false)
assert.notNil(0)
"#)
        .await;
    }

    #[tokio::test]
    async fn not_nil_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.notNil(nil))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    // ── assert.gt / gte / lt / lte ─────────────────────────────────────────

    #[tokio::test]
    async fn cmp_pass() {
        run(r#"
import * as assert from "std/assert"
assert.gt(3, 2)
assert.gte(3, 3)
assert.lt(1, 2)
assert.lte(2, 2)
"#)
        .await;
    }

    #[tokio::test]
    async fn gt_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.gt(1, 2))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    #[tokio::test]
    async fn gt_type_misuse_panics() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.gt("a", "b"))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    // ── assert.contains ────────────────────────────────────────────────────

    #[tokio::test]
    async fn contains_string_pass() {
        run(r#"
import * as assert from "std/assert"
assert.contains("hello world", "ell")
"#)
        .await;
    }

    #[tokio::test]
    async fn contains_array_pass() {
        run(r#"
import * as assert from "std/assert"
assert.contains([1, 2, 3], 2)
assert.contains([[1], [2]], [1])
"#)
        .await;
    }

    #[tokio::test]
    async fn contains_object_key_pass() {
        run(r#"
import * as assert from "std/assert"
assert.contains({a: 1}, "a")
"#)
        .await;
    }

    #[tokio::test]
    async fn contains_string_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.contains("hello", "xyz"))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    #[tokio::test]
    async fn contains_array_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.contains([1, 2, 3], 99))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    #[tokio::test]
    async fn contains_object_missing_key_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.contains({a: 1}, "b"))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    #[tokio::test]
    async fn contains_map_key_pass() {
        run(r#"
import * as assert from "std/assert"
import * as map from "std/map"
let m = map.new()
map.set(m, "a", 1)
map.set(m, 2, "two")
assert.contains(m, "a")
assert.contains(m, 2)
"#)
        .await;
    }

    #[tokio::test]
    async fn contains_map_missing_key_fail() {
        let src = r#"
import * as assert from "std/assert"
import * as map from "std/map"
let m = map.new()
map.set(m, "a", 1)
let r = recover(() => assert.contains(m, "b"))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    // ── assert.approxEq ────────────────────────────────────────────────────

    #[tokio::test]
    async fn approx_eq_pass() {
        run(r#"
import * as assert from "std/assert"
assert.approxEq(0.1 + 0.2, 0.3)
assert.approxEq(1.0, 1.0)
"#)
        .await;
    }

    #[tokio::test]
    async fn approx_eq_custom_epsilon_pass() {
        run(r#"
import * as assert from "std/assert"
assert.approxEq(1.0, 1.05, 0.1)
"#)
        .await;
    }

    #[tokio::test]
    async fn approx_eq_accepts_decimal() {
        // approxEq must accept Decimal args (consistent with gt/gte/lt/lte).
        run(r#"
import * as assert from "std/assert"
import * as decimal from "std/decimal"
assert.approxEq(decimal.from("0.1"), decimal.from("0.1"))
"#)
        .await;
    }

    #[tokio::test]
    async fn approx_eq_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.approxEq(1, 2))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    // ── assert.throws ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn throws_catches_panic_and_returns_error() {
        // Use assert(false, "boom") with a different local alias to avoid shadowing.
        let out = run(r#"
import * as A from "std/assert"
let e = A.throws(() => assert(false, "boom"))
print(e.message)
"#)
        .await;
        assert!(out.contains("boom"), "expected 'boom' in: {out}");
    }

    #[tokio::test]
    async fn throws_works_with_assert_eq_failure() {
        run(r#"
import * as assert from "std/assert"
let e = assert.throws(() => assert.eq(1, 2))
assert.contains(e.message, "assert.eq failed")
"#)
        .await;
    }

    #[tokio::test]
    async fn throws_fails_when_no_panic() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.throws(() => 1))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    #[tokio::test]
    async fn throws_message_contains_expected_phrase() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.throws(() => 42))
print(r[1].message)
"#;
        let out = run(src).await;
        assert!(
            out.contains("expected fn to throw"),
            "expected diagnostic, got: {out}"
        );
    }

    // ── assert.throws with async fn ────────────────────────────────────────

    #[tokio::test]
    async fn throws_drives_async_fn() {
        // assert.throws drives the future returned by an async fn.
        // Use [][0] (out-of-bounds) to trigger a panic inside the async fn.
        let out = run(r#"
import * as assert from "std/assert"
async fn boom() {
    let _ = [][0]
}
let e = await assert.throws(boom)
print(e.message)
"#)
        .await;
        assert!(
            !out.trim().is_empty(),
            "expected a non-empty error message, got: {out}"
        );
    }

    #[tokio::test]
    async fn throws_fails_for_non_panicking_async_fn() {
        let src = r#"
import * as assert from "std/assert"
async fn ok() { return 42 }
let r = recover(() => assert.throws(ok))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    // ── error message format ────────────────────────────────────────────────

    #[tokio::test]
    async fn eq_message_shows_both_values() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.eq(42, 99))
print(r[1].message)
"#;
        let out = run(src).await;
        assert!(out.contains("42"), "expected '42' in: {out}");
        assert!(out.contains("99"), "expected '99' in: {out}");
    }

    #[tokio::test]
    async fn global_assert_still_works_without_import() {
        // Without importing std/assert, the global assert(cond) builtin still works.
        run(r#"assert(true)"#).await;
    }

    #[tokio::test]
    async fn global_assert_not_shadowed_with_different_alias() {
        // Importing std/assert under a different name leaves the global assert intact.
        run(r#"
import * as A from "std/assert"
assert(true)
A.eq(1, 1)
"#)
        .await;
    }

    #[tokio::test]
    async fn run_err_helper_works() {
        // Verify run_err captures panic messages (using out-of-bounds index).
        let msg = run_err("let _ = [][0]").await;
        assert!(!msg.is_empty(), "expected a non-empty error message");
    }

    // ── structural diff (unit) ──────────────────────────────────────────────

    #[test]
    fn diff_equal_is_empty() {
        use super::structural_diff;
        use crate::value::{ArrayCell, ObjectCell, Value};
        // Scalars.
        assert_eq!(structural_diff(&Value::Int(1), &Value::Int(1)), "");
        // 1 vs 1.0 — deep_equal-equal → empty diff (no spurious change).
        assert_eq!(structural_diff(&Value::Int(1), &Value::Float(1.0)), "");
        // Deep arrays.
        let a = Value::Array(ArrayCell::new(vec![Value::Int(1), Value::Int(2)]));
        let b = Value::Array(ArrayCell::new(vec![Value::Int(1), Value::Int(2)]));
        assert_eq!(structural_diff(&a, &b), "");
        // Objects (insertion order).
        let mut o1 = indexmap::IndexMap::new();
        o1.insert("a".to_string(), Value::Int(1));
        let mut o2 = indexmap::IndexMap::new();
        o2.insert("a".to_string(), Value::Int(1));
        assert_eq!(
            structural_diff(&Value::Object(ObjectCell::new(o1)), &Value::Object(ObjectCell::new(o2))),
            ""
        );
    }

    #[test]
    fn diff_object_add_remove_change() {
        use super::structural_diff;
        use crate::value::{ObjectCell, Value};
        let mut a = indexmap::IndexMap::new();
        a.insert("keep".to_string(), Value::Int(1));
        a.insert("change".to_string(), Value::Int(2));
        a.insert("gone".to_string(), Value::Int(3));
        let mut b = indexmap::IndexMap::new();
        b.insert("keep".to_string(), Value::Int(1));
        b.insert("change".to_string(), Value::Int(9));
        b.insert("extra".to_string(), Value::Int(4));
        let d = structural_diff(&Value::Object(ObjectCell::new(a)), &Value::Object(ObjectCell::new(b)));
        assert!(d.contains(".change: 2 → 9"), "change: {d}");
        assert!(d.contains("- .gone: 3"), "remove: {d}");
        assert!(d.contains("+ .extra: 4"), "add: {d}");
        assert!(!d.contains("keep"), "unchanged key should not appear: {d}");
    }

    #[test]
    fn diff_array_index_change_and_length() {
        use super::structural_diff;
        use crate::value::{ArrayCell, Value};
        // index change
        let a = Value::Array(ArrayCell::new(vec![Value::Int(1), Value::Int(2)]));
        let b = Value::Array(ArrayCell::new(vec![Value::Int(1), Value::Int(7)]));
        let d = structural_diff(&a, &b);
        assert!(d.contains("[1]: 2 → 7"), "index change: {d}");
        // length difference (extra in a → removed)
        let a2 = Value::Array(ArrayCell::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        let b2 = Value::Array(ArrayCell::new(vec![Value::Int(1)]));
        let d2 = structural_diff(&a2, &b2);
        assert!(d2.contains("- [1]: 2"), "removed idx 1: {d2}");
        assert!(d2.contains("- [2]: 3"), "removed idx 2: {d2}");
        // length difference (extra in b → added)
        let d3 = structural_diff(&b2, &a2);
        assert!(d3.contains("+ [1]: 2"), "added idx 1: {d3}");
        assert!(d3.contains("+ [2]: 3"), "added idx 2: {d3}");
    }

    #[test]
    fn diff_nested_path_qualified() {
        use super::structural_diff;
        use crate::value::{ArrayCell, ObjectCell, Value};
        // { users: [ { name: "a" } ] } vs { users: [ { name: "b" } ] }
        let mk = |name: &str| {
            let mut inner = indexmap::IndexMap::new();
            inner.insert("name".to_string(), Value::Str(name.into()));
            let arr = Value::Array(ArrayCell::new(vec![Value::Object(ObjectCell::new(inner))]));
            let mut top = indexmap::IndexMap::new();
            top.insert("users".to_string(), arr);
            Value::Object(ObjectCell::new(top))
        };
        let d = structural_diff(&mk("a"), &mk("b"));
        // Value::Display renders strings without quotes, so the change line is
        // `.users[0].name: a → b`.
        assert!(
            d.contains(".users[0].name: a → b"),
            "path-qualified nested: {d}"
        );
    }

    #[test]
    fn diff_cyclic_does_not_overflow() {
        use super::structural_diff;
        use crate::value::{ArrayCell, Value};
        // Build a self-referential array a = [a], and an equal-shaped b = [b].
        let a = ArrayCell::new(vec![]);
        a.borrow_mut().push(Value::Array(a.clone()));
        let b = ArrayCell::new(vec![]);
        b.borrow_mut().push(Value::Array(b.clone()));
        // deep_equal treats these as equal (back-edge short-circuit) → empty diff,
        // and crucially this must NOT stack-overflow / loop.
        let d = structural_diff(&Value::Array(a), &Value::Array(b));
        assert_eq!(d, "");
    }

    // ── assert.deepEq ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn deep_eq_pass() {
        run(r#"
import * as assert from "std/assert"
assert.deepEq([1, {a: 2}], [1, {a: 2}])
"#)
        .await;
    }

    #[tokio::test]
    async fn deep_eq_fail_shows_diff() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.deepEq({a: 1, b: 2}, {a: 1, b: 3}))
print(r[1].message)
"#;
        let out = run(src).await;
        assert!(out.contains("assert.eq failed"), "header: {out}");
        assert!(out.contains(".b: 2 → 3"), "diff line: {out}");
    }

    #[tokio::test]
    async fn eq_fail_includes_structural_diff() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.eq([1, 2, 3], [1, 9, 3]))
print(r[1].message)
"#;
        let out = run(src).await;
        assert!(out.contains("[1]: 2 → 9"), "expected diff line in: {out}");
    }

    // ── assert.matches (data-gated: Value::Regex) ───────────────────────────

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn matches_pattern_string_pass() {
        run(r#"
import * as assert from "std/assert"
assert.matches("hello123", "[a-z]+[0-9]+")
"#)
        .await;
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn matches_compiled_regex_pass() {
        run(r#"
import * as assert from "std/assert"
import * as regex from "std/regex"
let re = regex.compile("^\\d+$")[0]
assert.matches("42", re)
"#)
        .await;
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn matches_fail_shows_value_and_pattern() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.matches("abc", "[0-9]+"))
print(r[1].message)
"#;
        let out = run(src).await;
        assert!(out.contains("abc"), "value: {out}");
        assert!(out.contains("[0-9]+"), "pattern: {out}");
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn matches_non_string_value_fails() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.matches(42, "[0-9]+"))
print(r[1].message)
"#;
        let out = run(src).await;
        assert!(out.contains("must be a string"), "diagnostic: {out}");
    }

    // ── assert.throwsWith ────────────────────────────────────────────────────

    #[tokio::test]
    async fn throws_with_matching_substr_passes() {
        let out = run(r#"
import * as A from "std/assert"
let e = A.throwsWith(() => assert(false, "boom happened"), "boom")
print(e.message)
"#)
        .await;
        assert!(out.contains("boom"), "returns error: {out}");
    }

    #[tokio::test]
    async fn throws_with_non_matching_substr_fails() {
        let src = r#"
import * as A from "std/assert"
let r = recover(() => A.throwsWith(() => assert(false, "boom happened"), "kaboom"))
print(r[1].message)
"#;
        let out = run(src).await;
        assert!(out.contains("expected error message to contain"), "diag: {out}");
        assert!(out.contains("boom happened"), "shows actual: {out}");
    }

    #[tokio::test]
    async fn throws_with_no_throw_fails() {
        let src = r#"
import * as A from "std/assert"
let r = recover(() => A.throwsWith(() => 1, "x"))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    #[tokio::test]
    async fn throws_with_drives_async_fn() {
        // Mirror throws_drives_async_fn: trigger a panic via out-of-bounds index
        // inside the async fn (the `assert(false, ...)` carry-forward arg bug is
        // unrelated). The default index-OOB message contains "index".
        let out = run(r#"
import * as assert from "std/assert"
async fn boom() {
    let _ = [][0]
}
let e = await assert.throwsWith(boom, "index")
print(e.message)
"#)
        .await;
        assert!(!out.trim().is_empty(), "async path: {out}");
    }

    // ── assert.snapshot ───────────────────────────────────────────────────────
    // These tests operate on the snapshot_impl helper directly to avoid any
    // global cwd/env-var pollution between parallel tests.

    #[cfg(all(feature = "sys", feature = "data"))]
    mod snapshot_tests {
        use super::super::snapshot_impl;
        use std::path::PathBuf;

        fn tmp_dir(name: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!("ascript_snap_test_{}", name));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        #[test]
        fn first_run_writes_and_passes() {
            let dir = tmp_dir("first_run");
            // First call: file absent → write + pass (Ok)
            let r = snapshot_impl(&dir, "my_snap", "42", false);
            assert!(r.is_ok(), "first run should pass: {:?}", r);
            let snap_file = dir.join("__snapshots__").join("my_snap.snap");
            assert!(snap_file.exists(), "snapshot file should be created");
            assert_eq!(std::fs::read_to_string(&snap_file).unwrap(), "42");
        }

        #[test]
        fn second_run_same_value_passes() {
            let dir = tmp_dir("second_run");
            snapshot_impl(&dir, "my_snap", "hello", false).unwrap();
            let r = snapshot_impl(&dir, "my_snap", "hello", false);
            assert!(r.is_ok(), "matching second run should pass: {:?}", r);
        }

        #[test]
        fn mismatch_returns_error() {
            let dir = tmp_dir("mismatch");
            snapshot_impl(&dir, "my_snap", "stored_value", false).unwrap();
            let r = snapshot_impl(&dir, "my_snap", "different_value", false);
            assert!(r.is_err(), "mismatch should fail");
            let msg = r.unwrap_err();
            assert!(
                msg.contains("stored_value"),
                "error should show stored: {msg}"
            );
            assert!(
                msg.contains("different_value"),
                "error should show new: {msg}"
            );
        }

        #[test]
        fn mismatch_shows_structural_diff_for_json() {
            // When both stored + new parse as JSON objects, the mismatch message
            // carries a structural diff (per-key change), in addition to the raw
            // dump fallback.
            let dir = tmp_dir("structural_diff");
            snapshot_impl(&dir, "obj", "{\n  \"a\": 1,\n  \"b\": 2\n}", false).unwrap();
            let r = snapshot_impl(&dir, "obj", "{\n  \"a\": 1,\n  \"b\": 3\n}", false);
            assert!(r.is_err());
            let msg = r.unwrap_err();
            assert!(msg.contains("diff (stored → new)"), "has diff header: {msg}");
            assert!(msg.contains(".b: 2 → 3"), "structural change line: {msg}");
        }

        #[test]
        fn update_flag_overwrites() {
            let dir = tmp_dir("update_flag");
            snapshot_impl(&dir, "my_snap", "original", false).unwrap();
            // update=true → overwrite + pass
            let r = snapshot_impl(&dir, "my_snap", "updated", true);
            assert!(r.is_ok(), "update mode should pass: {:?}", r);
            let snap_file = dir.join("__snapshots__").join("my_snap.snap");
            assert_eq!(std::fs::read_to_string(&snap_file).unwrap(), "updated");
        }

        #[test]
        fn unsafe_name_chars_are_sanitized() {
            let dir = tmp_dir("sanitize");
            // Name with path-separator chars and spaces should be sanitized
            snapshot_impl(&dir, "a/b\\c d", "val", false).unwrap();
            let snap_dir = dir.join("__snapshots__");
            let entries: Vec<_> = std::fs::read_dir(&snap_dir).unwrap().collect();
            assert_eq!(entries.len(), 1, "should create exactly one file");
            let name = entries[0]
                .as_ref()
                .unwrap()
                .file_name()
                .into_string()
                .unwrap();
            assert!(!name.contains('/') && !name.contains('\\') && !name.contains(' '));
        }
    }
}
