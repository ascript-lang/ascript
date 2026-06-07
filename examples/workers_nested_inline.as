// Inline nesting: a worker fn calling another worker fn inside an isolate.
// When `inner` is invoked from within the isolate running `outer`, the runtime
// detects it is already inside an isolate and executes `inner` inline — no
// re-dispatch, no pool round-trip, no deadlock.
//
// The call chain: outer(4) → inner(4) returns 5 → 5 * 10 = 50.

worker fn inner(n: number): number {
  return n + 1
}

worker fn outer(n: number): number {
  // Called from inside an isolate: inner runs inline (no deadlock).
  let bumped = await inner(n)
  return bumped * 10
}

fn main() {
  print(await outer(4))   // (4+1)*10 = 50
}

await main()
