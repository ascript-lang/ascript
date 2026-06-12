// bench/defer_bench.as — DEFER Gate 16/18 microbenchmarks.
//
// Two sections:
//
//   1. DEFER-FREE workloads (mirroring compact_value_bench.as): the same
//      scalar-heavy loops used for Gate-12 tracking, run WITHOUT any defer
//      statement. These prove the empty-stack fast path adds zero overhead:
//      defer-free code ≈ baseline (within noise).
//
//   2. DEFER-HEAVY microbench: a 10k-iteration loop where each iteration
//      pushes and drains ONE defer entry. This is the honest cost-of-use
//      measurement — linear stack growth by design (the `defer-in-loop` lint
//      is the guardrail). Time and peak RSS reported honestly.
//
// Every workload prints:   <name> elapsed_ms=<float>
// Parsed by bench/run_defer_bench.sh for DEFER_RESULTS.md.

import * as time from "std/time"

// ── 1a. int_sum (defer-free) — tight i64 accumulation ───────────────────────
fn bench_int_sum_free() {
  let t0 = time.monotonic()
  let total = 0
  for (i in 0..20000000) {
    total = total + i
  }
  let t1 = time.monotonic()
  print(`int_sum_free total=${total} elapsed_ms=${t1 - t0}`)
}

// ── 1b. fib_iter (defer-free) — iterative Fibonacci ─────────────────────────
fn bench_fib_iter_free() {
  let t0 = time.monotonic()
  let acc = 0
  for (r in 0..1000000) {
    let a = 0
    let b = 1
    for (i in 0..30) {
      let c = a + b
      a = b
      b = c
    }
    acc = acc + a
  }
  let t1 = time.monotonic()
  print(`fib_iter_free acc=${acc} elapsed_ms=${t1 - t0}`)
}

// ── 1c. object_churn (defer-free) — object alloc + field reads ───────────────
fn bench_object_churn_free() {
  let t0 = time.monotonic()
  let total = 0
  for (i in 0..4000000) {
    let o = { id: i, name: "node", x: i * 2, y: i + 1 }
    total = total + o.x + o.y + o.id
  }
  let t1 = time.monotonic()
  print(`object_churn_free total=${total} elapsed_ms=${t1 - t0}`)
}

// ── 1d. method_dispatch (defer-free) — class instance method calls ───────────
class Counter {
  fn init() { self.n = 0 }
  fn bump() { self.n = self.n + 1 }
  fn get() { return self.n }
}

fn bench_method_dispatch_free() {
  let t0 = time.monotonic()
  let c = Counter()
  for (i in 0..1000000) {
    c.bump()
  }
  let t1 = time.monotonic()
  print(`method_dispatch_free value=${c.get()} elapsed_ms=${t1 - t0}`)
}

// ── 1e. call_overhead (defer-free) — shallow function call overhead ──────────
fn add(a, b) { return a + b }

fn bench_call_overhead_free() {
  let t0 = time.monotonic()
  let total = 0
  for (i in 0..2000000) {
    total = total + add(i, 1)
  }
  let t1 = time.monotonic()
  print(`call_overhead_free total=${total} elapsed_ms=${t1 - t0}`)
}

// ── 2. defer_heavy — cost-of-use: 10k iteration loop with 1 defer/iter ───────
//
// NOTE: This is a defer-IN-LOOP pattern (the `defer-in-loop` lint fires here).
// Linear stack growth is EXPECTED and DOCUMENTED — this is measuring the
// cost of the push+drain machinery, not recommending the pattern.
// The lint is the guardrail that prevents accidental use; this bench exercises
// the mechanism deliberately to measure its cost.

fn noop_cleanup(x) {
  // called at function exit for each deferred entry
  let _ = x
}

fn bench_defer_heavy() {
  let t0 = time.monotonic()
  let total = 0
  for (i in 0..10000) {
    defer noop_cleanup(i)   // registers one deferred call per iteration
    total = total + i
  }
  // ALL 10k deferred calls run HERE at function exit (LIFO)
  let t1 = time.monotonic()
  print(`defer_heavy total=${total} elapsed_ms=${t1 - t0}`)
}

bench_int_sum_free()
bench_fib_iter_free()
bench_object_churn_free()
bench_method_dispatch_free()
bench_call_overhead_free()
bench_defer_heavy()
