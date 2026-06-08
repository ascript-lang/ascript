//! `std/bench` — micro-benchmarking utilities (core, no feature gate).
//!
//! - `bench.measure(fn, iterations?) -> {iterations, totalMs, avgMs, opsPerSec}`
//!   Runs `fn` `iterations` times (default 100), timing via a monotonic
//!   `std::time::Instant`. If `fn` returns a `Value::Future` (async fn), that
//!   future is driven to completion before the next iteration.
//!
//! - `bench.compare({name: fn, ...}, iterations?) -> array<{name, avgMs, opsPerSec}>`
//!   Runs `bench.measure` on each named function and returns the results sorted
//!   by avgMs ascending (fastest first).

use super::{arg, bi, want_number};
use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::Value;
use indexmap::IndexMap;
use std::time::Instant;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("measure", bi("bench.measure")),
        ("compare", bi("bench.compare")),
    ]
}

/// Default number of iterations for bench.measure.
const DEFAULT_ITERATIONS: u64 = 100;

impl Interp {
    /// Dispatch for `bench.*` builtin calls.
    pub(crate) async fn call_bench(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "measure" => self.bench_measure(args, span).await,
            "compare" => self.bench_compare(args, span).await,
            _ => Err(AsError::at(format!("bench has no function '{}'", func), span).into()),
        }
    }

    /// `bench.measure(fn, iterations?) -> {iterations, totalMs, avgMs, opsPerSec}`
    ///
    /// Runs `fn` `iterations` times (default 100). If the fn returns a Future
    /// (i.e. it is async), that future is driven to completion before the next
    /// iteration. Timing wraps the entire loop with a monotonic `Instant`.
    async fn bench_measure(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let callee = arg(args, 0);
        let iterations: u64 = match arg(args, 1) {
            Value::Nil => DEFAULT_ITERATIONS,
            v => {
                let n = want_number(&v, span, "bench.measure iterations")?;
                if n < 1.0 || n.fract() != 0.0 {
                    return Err(AsError::at(
                        "bench.measure: iterations must be a positive integer",
                        span,
                    )
                    .into());
                }
                n as u64
            }
        };

        let start = Instant::now();
        for _ in 0..iterations {
            let result = self.call_value(callee.clone(), vec![], span).await?;
            // Drive any returned Future to completion (async fn path).
            if let Value::Future(f) = result {
                f.get().await?;
            }
        }
        let total_ms = start.elapsed().as_secs_f64() * 1000.0;
        let avg_ms = if iterations > 0 {
            total_ms / iterations as f64
        } else {
            0.0
        };
        let ops_per_sec = if avg_ms > 0.0 {
            1000.0 / avg_ms
        } else {
            f64::INFINITY
        };

        let mut obj = IndexMap::new();
        obj.insert("iterations".to_string(), Value::Float(iterations as f64));
        obj.insert("totalMs".to_string(), Value::Float(total_ms));
        obj.insert("avgMs".to_string(), Value::Float(avg_ms));
        // Cap opsPerSec at a very large but representable number if infinite.
        let ops = if ops_per_sec.is_infinite() {
            1e15_f64
        } else {
            ops_per_sec
        };
        obj.insert("opsPerSec".to_string(), Value::Float(ops));
        Ok(Value::Object(crate::value::ObjectCell::new(obj)))
    }

    /// `bench.compare({name: fn, ...}, iterations?) -> array<{name, avgMs, opsPerSec}>`
    ///
    /// Runs `bench.measure` on each named function and returns results sorted
    /// by avgMs ascending (fastest first).
    async fn bench_compare(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let map_val = arg(args, 0);
        let entries = match &map_val {
            Value::Object(o) => o.borrow().clone(),
            _ => {
                return Err(AsError::at(
                    format!(
                        "bench.compare expects an object {{name: fn}}, got {}",
                        crate::interp::type_name(&map_val)
                    ),
                    span,
                )
                .into())
            }
        };

        let iterations_arg = arg(args, 1);
        let mut results: Vec<(String, f64, f64)> = Vec::new(); // (name, avgMs, opsPerSec)

        for (name, callee) in entries.iter() {
            let measure_args = match &iterations_arg {
                Value::Nil => vec![callee.clone()],
                it => vec![callee.clone(), it.clone()],
            };
            let stats = self.bench_measure(&measure_args, span).await?;
            if let Value::Object(o) = &stats {
                let o = o.borrow();
                let avg_ms = match o.get("avgMs") {
                    Some(Value::Float(n)) => *n,
                    _ => 0.0,
                };
                let ops = match o.get("opsPerSec") {
                    Some(Value::Float(n)) => *n,
                    _ => 0.0,
                };
                results.push((name.clone(), avg_ms, ops));
            }
        }

        // Sort by avgMs ascending (fastest first).
        results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        let out: Vec<Value> = results
            .into_iter()
            .map(|(name, avg_ms, ops_per_sec)| {
                let mut obj = IndexMap::new();
                obj.insert("name".to_string(), Value::Str(name.into()));
                obj.insert("avgMs".to_string(), Value::Float(avg_ms));
                obj.insert("opsPerSec".to_string(), Value::Float(ops_per_sec));
                Value::Object(crate::value::ObjectCell::new(obj))
            })
            .collect();

        Ok(Value::Array(crate::value::ArrayCell::new(out)))
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

    #[tokio::test]
    async fn measure_returns_stats_object() {
        let out = run(r#"
import * as bench from "std/bench"
let stats = await bench.measure(() => 1 + 1, 50)
print(stats.iterations)
print(stats.totalMs >= 0)
print(stats.avgMs >= 0)
print(stats.opsPerSec > 0)
"#)
        .await;
        assert_eq!(out, "50\ntrue\ntrue\ntrue\n");
    }

    #[tokio::test]
    async fn measure_default_iterations_is_100() {
        let out = run(r#"
import * as bench from "std/bench"
let stats = await bench.measure(() => nil)
print(stats.iterations)
"#)
        .await;
        assert_eq!(out.trim(), "100");
    }

    #[tokio::test]
    async fn measure_drives_async_fn() {
        // An async fn that increments a shared counter; running 10 iterations
        // should result in counter == 10.
        let out = run(r#"
import * as bench from "std/bench"
let counter = [0]
async fn inc() { counter[0] = counter[0] + 1 }
await bench.measure(inc, 10)
print(counter[0])
"#)
        .await;
        assert_eq!(out.trim(), "10");
    }

    #[tokio::test]
    async fn compare_returns_sorted_array() {
        let out = run(r#"
import * as bench from "std/bench"
let results = await bench.compare({fast: () => 1, slow: () => 1}, 5)
print(type(results))
print(results[0].name != nil)
print(results[0].avgMs >= 0)
print(results[0].opsPerSec > 0)
"#)
        .await;
        assert_eq!(out, "array\ntrue\ntrue\ntrue\n");
    }

    #[tokio::test]
    async fn measure_bad_iterations_panics() {
        let src = r#"
import * as bench from "std/bench"
let r = recover(() => bench.measure(() => 1, 0))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }
}
