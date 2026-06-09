// bench/compact_value_bench.as — VAL Stage-1 (compact Value) microbenchmark.
//
// Measures the wall-clock effect of shrinking `Value` from 32 to 24 bytes (the
// honest Stage-1 floor — fat `Str(Rc<str>)` is still the widest payload; 16 needs
// thin-`Str` at Task 9, 8 needs the NaN-box). Each workload prints a tagged line
//   <name> elapsed_ms=<float>
// that bench/run_compact_value_bench.sh parses. Every workload is run by the runner
// in BOTH VM modes (specialized + ASCRIPT_NO_SPECIALIZE=1 generic) and against the
// same-session pre-VAL baseline (main @ 612339c), so a regression in EITHER mode is
// surfaced (Gate 12 — a generic-mode regression is a VAL BUG, not a trade).
//
// Workloads:
//   1. int_sum      — scalar-heavy: a tight i64 accumulation loop (inline-scalar /
//                     ArithKind::Int fast path; no heap, no refcount).
//   2. fib_iter     — arithmetic-heavy: iterative Fibonacci over NUM ints
//                     (checked-add carrying through the int fast path).
//   3. array_walk   — array element churn: build + index-walk a large array
//                     (every slot is a Value — 24-byte slots are denser than 32).
//   4. object_churn — object churn: allocate a 4-field object per iteration and
//                     read its fields (Cc<ObjectCell> alloc + field reads).
//   5. float_sum    — scalar-heavy floats (ArithKind::Number fast path).
//
// Cold-path checks (Task 2 / Task 1 boxing must add NO measurable regression):
//   6. decimal_cold — decimal arithmetic now does an `Rc::new` per op (Decimal is
//                     boxed behind Rc<Decimal>). On a NON-decimal workload this
//                     never executes; here we measure it directly and report it
//                     honestly as the cold-path cost.
//   7. method_cold  — construct+dispatch a `ClassMethod` (a class static binding)
//                     and a `GeneratorMethod` (`gen.next`) repeatedly. These two
//                     variants were boxed (32→24); the extra indirection on these
//                     rare bindings must not measurably regress.

import * as time from "std/time"
import * as decimal from "std/decimal"
import * as array from "std/array"

// ── 1. int_sum — scalar-heavy i64 accumulation ───────────────────────────────
fn bench_int_sum() {
  let t0 = time.monotonic()
  let total = 0
  for (i in 0..20000000) {
    total = total + i
  }
  let t1 = time.monotonic()
  print(`int_sum total=${total} elapsed_ms=${t1 - t0}`)
}

// ── 2. fib_iter — arithmetic-heavy iterative Fibonacci over ints ─────────────
fn bench_fib_iter() {
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
  print(`fib_iter acc=${acc} elapsed_ms=${t1 - t0}`)
}

// ── 3. array_walk — array element churn (dense Value slots) ──────────────────
fn bench_array_walk() {
  let t0 = time.monotonic()
  let arr = []
  for (i in 0..2000000) {
    array.push(arr, i)
  }
  let total = 0
  for (i in 0..len(arr)) {
    total = total + arr[i]
  }
  let t1 = time.monotonic()
  print(`array_walk total=${total} elapsed_ms=${t1 - t0}`)
}

// ── 4. object_churn — per-iteration object alloc + field reads ───────────────
fn bench_object_churn() {
  let t0 = time.monotonic()
  let total = 0
  for (i in 0..4000000) {
    let o = { id: i, name: "node", x: i * 2, y: i + 1 }
    total = total + o.x + o.y + o.id
  }
  let t1 = time.monotonic()
  print(`object_churn total=${total} elapsed_ms=${t1 - t0}`)
}

// ── 5. float_sum — scalar-heavy float accumulation ───────────────────────────
fn bench_float_sum() {
  let t0 = time.monotonic()
  let total = 0.0
  for (i in 0..20000000) {
    total = total + 1.5
  }
  let t1 = time.monotonic()
  print(`float_sum total=${total} elapsed_ms=${t1 - t0}`)
}

// ── 6. decimal_cold — boxed-Decimal arithmetic cold path (Rc::new per op) ────
fn bench_decimal_cold() {
  let one = decimal.from("1.0")
  let t0 = time.monotonic()
  let acc = decimal.from("0.0")
  for (i in 0..1000000) {
    acc = acc + one
  }
  let t1 = time.monotonic()
  print(`decimal_cold acc=${decimal.toString(acc)} elapsed_ms=${t1 - t0}`)
}

// ── 7. method_cold — boxed ClassMethod / GeneratorMethod construct+dispatch ──
class Mul {
  static fn dbl(n) { return n * 2 }
}
fn* nat() {
  let i = 0
  while (true) { yield i; i = i + 1 }
}
fn bench_method_cold() {
  let g = nat()
  let t0 = time.monotonic()
  let total = 0
  for (i in 0..1000000) {
    let m = Mul.dbl     // construct a ClassMethod binding (boxed payload)
    let nx = g.next     // construct a GeneratorMethod binding (boxed payload)
    total = total + m(i) + nx()
  }
  let t1 = time.monotonic()
  print(`method_cold total=${total} elapsed_ms=${t1 - t0}`)
}

bench_int_sum()
bench_fib_iter()
bench_array_walk()
bench_object_churn()
bench_float_sum()
bench_decimal_cold()
bench_method_cold()
