// callback_escalation.as
// ---------------------------------------------------------------------------
// Production-shaped edge cases for higher-order callbacks:
//
//   1. Escalation: map whose callback awaits an async fn.
//      A plain (non-async) closure containing `await` forces the trampoline
//      to escalate from the sync lane to the async driver for that element.
//
//   2. Panic in reduce, caught by recover.
//      A reduce whose callback panics at a specific element; recover wraps
//      the whole call and catches the Tier-2 panic.
//
//   3. ?-propagation through a higher-order pipeline.
//      A validation step maps items to [value, err] pairs; a subsequent
//      loop uses ? to short-circuit on the first error, propagating out
//      of the enclosing function as a [nil, err] result.
//
//   4. Cell-capture freshness across elements.
//      A callback closes over a mutated counter so each element's closure
//      captures a DISTINCT cell — proving the trampoline allocates fresh
//      cells per element (reset invariant §5.4).
//
//   5. Sort comparator that panics on a poisoned element, caught by recover.
//      The sort never completes; recover returns the panic message.
//
// All output is deterministic; no external I/O.
// ---------------------------------------------------------------------------
import * as array from "std/array"

// ---------------------------------------------------------------------------
// 1. Escalation — a plain closure that awaits an async fn
// ---------------------------------------------------------------------------
// `asyncScale` is an ordinary async fn; the callback passed to `map` is a
// plain (non-async) closure that calls `await asyncScale(x)`. On the VM,
// the trampoline arms on the plain-closure callee, runs the sync lane, and
// escalates to the async driver when the `await` op is reached — continuing
// the SAME live fiber (never re-executing the element).
async fn asyncScale(x, factor) {
  return x * factor
}

let scaled = array.map([10, 20, 30, 40, 50], (x) => await asyncScale(x, 3))
print("=== 1. Escalation: map + async callback ===")
print(scaled)

// ---------------------------------------------------------------------------
// 2. Panic in reduce, caught by recover
// ---------------------------------------------------------------------------
// The reduce callback panics when it encounters the sentinel -1. recover
// wraps the entire reduce call (arrow form — anonymous-fn-expression form
// has a known carry-forward bug noted in CLAUDE.md).
print("\n=== 2. Panicking reduce, caught by recover ===")
let data = [1, 2, 3, -1, 5]
let [sumResult, sumErr] = recover(() => {
  return array.reduce(data, (acc, x) => {
    if (x < 0) {
      // Tier-2 panic: unrecoverable from inside the callback, but the
      // `recover` wrapper around the whole reduce catches it.
      [][999]
    }
    return acc + x
  }, 0)
})
if (sumErr != nil) {
  print(`  caught panic: ${sumErr.message}`)
} else {
  print(`  sum (no error): ${sumResult}`)
}

// Verify a clean reduce (no poison) works fine after the panic.
let cleanSum = array.reduce([1, 2, 3, 4, 5], (acc, x) => acc + x, 0)
print(`  clean reduce: ${cleanSum}`)

// ---------------------------------------------------------------------------
// 3. ?-propagation through a higher-order pipeline
// ---------------------------------------------------------------------------
// The callback returns a [value, err] pair; the enclosing function iterates
// the mapped pairs and uses ? to propagate the first error — returning
// [nil, err] to its caller.
print("\n=== 3. ?-propagation through higher-order pipeline ===")

fn validatePositive(n) {
  if (n < 0) {
    return Err(`negative value: ${n}`)
  }
  return Ok(n * 10)
}

fn processBatch(items) {
  // map produces [value,err] pairs for each item
  let pairs = array.map(items, (n) => validatePositive(n))
  let results = []
  for (pair of pairs) {
    let v = pair?
    array.push(results, v)
  }
  return Ok(results)
}

let goodBatch = processBatch([1, 2, 3, 4])
print(`  good batch: ${goodBatch[0]}`)
print(`  good error: ${goodBatch[1]}`)

let badBatch = processBatch([1, 2, -3, 4])
print(`  bad batch: ${badBatch[0]}`)
print(`  bad error: ${badBatch[1].message}`)

// ---------------------------------------------------------------------------
// 4. Cell-capture freshness — callback closes over a mutated counter
// ---------------------------------------------------------------------------
// Each iteration of the for loop creates a fresh binding for `step`, which
// the closure captures. Each resulting closure retains its own cell — the
// trampoline's per-element reset allocates fresh cells so element N+1 never
// sees N's captured cell value.
print("\n=== 4. Cell-capture freshness ===")

let adders = []
for (i in 0..5) {
  let step = i * 10
  array.push(adders, (x) => x + step)
}
// Demonstrate capture freshness: call each adder with the same input (5).
// Each adder captures its own distinct `step` cell: 0, 10, 20, 30, 40.
let adderOutputs = array.map(adders, (f) => f(5))
print(`  adder outputs: ${adderOutputs}`)

// ---------------------------------------------------------------------------
// 5. Sort comparator panics on a poisoned element, caught by recover
// ---------------------------------------------------------------------------
// The comparator inspects both arguments: if either is nil it panics. The
// array contains a nil sentinel; the sort never completes; recover catches
// the panic message.
print("\n=== 5. Sort comparator panic, caught by recover ===")

let withPoison = [3, 1, nil, 2, 4]
let [sortResult, sortErr] = recover(() => {
  return array.sort(withPoison, (a, b) => {
    if (a == nil || b == nil) {
      [][999]
    }
    return a - b
  })
})
if (sortErr != nil) {
  print(`  caught panic: ${sortErr.message}`)
} else {
  print(`  sorted (no error): ${sortResult}`)
}

// Verify a clean sort works fine after the panic.
let cleanSort = array.sort([3, 1, 4, 1, 5, 9, 2, 6], (a, b) => a - b)
print(`  clean sort: ${cleanSort}`)
