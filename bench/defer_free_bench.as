// bench/defer_free_bench.as — defer-free workloads for same-session A/B.
//
// These are the SAME workloads as defer_bench.as but with NO defer statements.
// Run on BOTH the baseline (main) and the candidate (feat/defer-statement) to
// prove that defer-free code shows ZERO regression.
//
// Every workload prints:   <name> elapsed_ms=<float>

import * as time from "std/time"

// ── int_sum — tight i64 accumulation ────────────────────────────────────────
fn bench_int_sum_free() {
  let t0 = time.monotonic()
  let total = 0
  for (i in 0..20000000) {
    total = total + i
  }
  let t1 = time.monotonic()
  print(`int_sum_free total=${total} elapsed_ms=${t1 - t0}`)
}

// ── fib_iter — iterative Fibonacci ──────────────────────────────────────────
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

// ── object_churn — per-iteration object alloc + field reads ──────────────────
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

// ── method_dispatch — class instance method calls ────────────────────────────
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

// ── call_overhead — shallow function call overhead ───────────────────────────
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

bench_int_sum_free()
bench_fib_iter_free()
bench_object_churn_free()
bench_method_dispatch_free()
bench_call_overhead_free()
