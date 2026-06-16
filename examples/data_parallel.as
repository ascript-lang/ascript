// data_parallel.as — task.pmap / task.preduce intro
//
// `task.pmap(data, f, opts?)` dispatches chunks of the array across the worker
// pool and merges results in INPUT order — deterministic regardless of which
// isolate finishes first.
//
// `task.preduce(data, f, init, opts?)` folds each chunk with `f` (seeded by
// the chunk's first element), then combines the partial results with one final
// fold: f(…f(f(init, p0), p1)…). `f` must be ASSOCIATIVE for the result to
// equal a sequential reduce. Pin `{chunks: N}` to get identical bytes across
// machines with different core counts.
//
// Phase-0 CORRECTION: a `?`-propagation inside a `worker fn` body yields the
// `[nil, err]` PAIR as that element's result — NOT nil.  The pair merges into
// the output array like any value.
//
//   ascript run examples/data_parallel.as
import * as task from "std/task"
import * as math from "std/math"

worker fn square(n) {
  return n * n
}

worker fn add(a, b) {
  return a + b
}

// Panics on negative input — so we can demonstrate recover().
worker fn safeRecip(x) {
  if (x <= 0) {
    assert(false, `negative or zero input: ${x}`)
  }
  return 1.0 / x
}

// Returns a [value, err] pair via `?` propagation for invalid input.
worker fn checkedDouble(x) {
  if (x < 0) {
    let pair = [nil, {message: "negative not allowed", input: x}]
    pair? // `?` on a [nil, err] pair propagates: yields the pair as the element
  }
  return x * 2
}

fn main() {
  // ── 1. pmap: square each element, result in input order ──────────────────
  let inputs = [1, 2, 3, 4, 5, 6, 7, 8]
  let squares = await task.pmap(inputs, square, {chunks: 4})
  print(squares) // [1, 4, 9, 16, 25, 36, 49, 64]
  print(math.sum(squares)) // 204.0

  // ── 2. preduce: parallel sum with a non-zero init ─────────────────────────
  // `add` is associative, so preduce equals sequential reduce.
  // Pin {chunks: 4} for byte-identical output across core counts.
  let total = await task.preduce(inputs, add, 100, {chunks: 4})
  print(total) // 136  (100 + 1+2+…+8)

  // ── 3. panicking callback caught by recover ────────────────────────────────
  let bad = [3, -1, 2]
  let [_, err] = recover(() => await task.pmap(bad, safeRecip, {chunks: 4}))
  // err.message starts with the panic string
  print(err.message) // negative or zero input: -1

  // ── 4. empty-array edge — pool is never touched ───────────────────────────
  let emptyMap = await task.pmap([], square)
  let emptyReduce = await task.preduce([], add, 42, {chunks: 4})
  print(emptyMap) // []
  print(emptyReduce) // 42  (init returned directly, pool untouched)

  // ── 5. ? propagation inside callback → [nil, err] PAIR element ───────────
  // checkedDouble(-2) propagates its [nil,err] pair; 3 and 5 return normally.
  let mixed = await task.pmap([-2, 3, 5], checkedDouble, {chunks: 4})
  let [v0, e0] = mixed[0]
  print(v0) // nil
  print(e0.message) // negative not allowed
  print(mixed[1]) // 6
  print(mixed[2]) // 10
}

await main()
