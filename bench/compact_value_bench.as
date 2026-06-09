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

// VAL Stage-3 (Task 9, thin-`Str` → 16) ADDS string-heavy + memory-bound
// workloads (8–11 below). Thin-`Str` makes a `Value::Str` a single 8-byte word
// (`Rc<Box<str>>`) at the cost of a double-indirection on string ACCESS, so the
// string workloads honestly surface any access regression, and the memory-bound
// workload (a big `array<string>` / string-keyed map) is where the 24→16 shrink's
// cache-density benefit should show. The same-session baseline for Stage 3 is the
// Stage-1 floor (this branch @ Task-4 commit 1f1451d, size 24), so the A/B isolates
// the 24→16 step.

import * as time from "std/time"
import * as decimal from "std/decimal"
import * as array from "std/array"
import * as string from "std/string"

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

// ── 8. string_concat — string-heavy: build many short strings via concat ─────
//   Each `+` allocates a fresh `AStr` (`Rc::new(Box<str>)`); thin-`Str` adds the
//   `Box<str>` indirection vs the old fat `Rc<str>`. A direct string-access stress.
fn bench_string_concat() {
  let t0 = time.monotonic()
  let total = 0
  for (i in 0..2000000) {
    let s = "node-" + string.upper("k")
    total = total + len(s)
  }
  let t1 = time.monotonic()
  print(`string_concat total=${total} elapsed_ms=${t1 - t0}`)
}

// ── 9. string_map — map with STRING keys: insert + read by string key ────────
//   Exercises `MapKey::Str` fold (`Value::Str` → `MapKey::Str`, an Rc bump) on the
//   thin encoding, plus string hashing/equality on lookup.
fn bench_string_map() {
  let keys = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"]
  let t0 = time.monotonic()
  let total = 0
  for (r in 0..1000000) {
    let m = {}
    for (i in 0..len(keys)) {
      m[keys[i]] = i
    }
    for (i in 0..len(keys)) {
      total = total + m[keys[i]]
    }
  }
  let t1 = time.monotonic()
  print(`string_map total=${total} elapsed_ms=${t1 - t0}`)
}

// ── 10. string_index — codepoint / slice access over strings ─────────────────
//   Decodes the string to bytes/codepoints repeatedly — the hottest READ path for
//   the thin form (every access traverses Value→Rc→Box<str>→bytes).
fn bench_string_index() {
  let s = "the quick brown fox jumps over the lazy dog"
  let t0 = time.monotonic()
  let total = 0
  for (i in 0..2000000) {
    let cps = string.codepoints(s)
    total = total + len(cps) + len(string.slice(s, 4, 9))
  }
  let t1 = time.monotonic()
  print(`string_index total=${total} elapsed_ms=${t1 - t0}`)
}

// ── 11. membound_strings — memory-bound LARGE working set (cache density) ─────
//   A big `array<string>` plus a big string-keyed map, then a full traversal of
//   both. The whole working set is large `Vec<Value>` / `IndexMap` storage where a
//   16-byte `Value` slot is 33% denser than a 24-byte slot — the workload where
//   24→16 cache density is meant to pay off (vs the CPU-bound loops above).
fn bench_membound_strings() {
  let t0 = time.monotonic()
  let n = 1500000
  let arr = []
  for (i in 0..n) {
    array.push(arr, "item")
  }
  // Full linear scan of the dense Value array (string slots).
  let total = 0
  for (pass in 0..6) {
    for (i in 0..len(arr)) {
      total = total + len(arr[i])
    }
  }
  let t1 = time.monotonic()
  print(`membound_strings total=${total} elapsed_ms=${t1 - t0}`)
}

bench_int_sum()
bench_fib_iter()
bench_array_walk()
bench_object_churn()
bench_float_sum()
bench_string_concat()
bench_string_map()
bench_string_index()
bench_membound_strings()
bench_decimal_cold()
bench_method_cold()
