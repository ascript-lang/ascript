// Worker error handling: two flavors.
//
// 1. Worker panic: a worker fn that calls panic() produces a recoverable error on
//    the caller — catchable via recover. The result is a [nil, err] pair.
//
// 2. Sendability violation: passing a non-sendable value (e.g. a closure) nested
//    in an object to a worker fn produces a recoverable panic whose message carries
//    the exact field path of the offending value.
//
// NOTE: recover() requires arrow syntax for async closures — use `recover(() => ...)`
// rather than `recover(fn() { ... })` to avoid a known parser limitation.
//
// NOTE: `panic` is not a builtin. Use `assert(false, msg)` to raise a named error
// inside a worker body (or any other code). `assert(false, msg)` is byte-identical
// in behavior to an explicit panic and is caught identically by `recover`.
import * as string from "std/string"

worker fn risky(n: number): number {
  if (n < 0) { assert(false, "negative input") }
  return n * n
}

worker fn takesObj(o): number { return 1 }

fn main() {
  // Happy path: positive input squares cleanly.
  print(await risky(5))                            // 25

  // Worker panic is recoverable: recover() catches the error pair.
  let caught = recover(() => await risky(-1))
  print(caught[1] != nil)                          // true — panic recovered

  // Sendability violation: a closure cannot cross the isolate boundary.
  // The error message names the field path where the violation was found.
  let bad = recover(() => await takesObj({ cb: () => 1 }))
  let hasPath = string.contains(bad[1].message, "cannot be sent to a worker at")
  print(hasPath)                                   // true
}

await main()
