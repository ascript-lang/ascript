// typed_contracts.as
// ---------------------------------------------------------------------------
// Demonstrates how type annotations interact with contract elision (ELIDE).
//
// When `ascript run --elide` (or `ascript build --elide`) is used, the static
// type checker proves which runtime checks are redundant and removes them.
// The program's behavior is IDENTICAL either way — elision is invisible. The
// benefit is measured speed on call-heavy typed code.
//
// This example shows three categories:
//   1. Proven sites (parameter, let, return) — elided under --elide.
//   2. Nullable / union types — also proven when the chain is all-annotated.
//   3. The gradual boundary — an `any`-typed input that KEEPS its check.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 1. Proven parameter and return contracts
//
// `clamp`, `scale`, and `describe` are fully annotated (param types, return
// type, and the local `let`). Every argument at the three call sites below is
// a literal or comes from another proven call — all three conditions for proof
// hold: ElideSafe type, concrete Yes verdict, anchored argument.
// Under --elide the per-argument type-contract checks are removed from the
// bytecode. Without --elide the checks run and pass — same output.
// ---------------------------------------------------------------------------
fn clamp(value: int, lo: int, hi: int): int {
  if (value < lo) {
    return lo
  }
  if (value > hi) {
    return hi
  }
  return value
}

fn scale(factor: int, basis: int): int {
  let result: int = factor * basis
  // The annotated `let` above is a proven site too: factor and basis are
  // annotated params, arithmetic on two ints produces int — proven and
  // elided under --elide.
  return result
}

fn describe(n: int): string {
  if (n < 0) {
    return "negative"
  }
  if (n == 0) {
    return "zero"
  }
  return "positive"
}

// All three call-site argument chains are literals or proven calls:
let clamped: int = clamp(scale(3, 10), 0, 50)
print(clamped) // 30
print(describe(clamp(-5, 0, 100))) // zero (clamped from -5 to 0, which equals lo)
print(describe(clamp(1, 0, 100))) // positive
print(describe(scale(7, -3))) // negative (-21)

// ---------------------------------------------------------------------------
// 2. Nullable types and narrowing
//
// A `number?` annotation accepts int, float, or nil. Narrowing inside the
// nil-guard is also tracked by the checker: after `if (x != nil)` the
// binding is known non-nil, so using it in arithmetic is proven safe.
// ---------------------------------------------------------------------------
fn safeDouble(x: int?): int? {
  if (x == nil) {
    return nil
  }
  // After the nil-guard, x is narrowed to int — the return below is proven.
  return x * 2
}

print(safeDouble(21)) // 42
print(safeDouble(nil)) // nil

// ---------------------------------------------------------------------------
// 3. The gradual boundary — any-typed input keeps its contract check
//
// `processRaw` receives an `any`-typed value from outside the proven world.
// The call `typed(raw)` below passes an `any`-typed argument; the checker
// cannot prove its kind, so the site stays un-elided — the runtime check
// fires as usual and guards the typed interior.
//
// This is the key invariant: elision never removes a check at the boundary
// between typed and untyped code.
// ---------------------------------------------------------------------------
fn typed(n: int): int {
  return n * n
}

fn processRaw(raw: any) {
  // `raw` is any-typed — the call to `typed` cannot be proven; the
  // runtime contract check on `n: int` fires and catches wrong shapes.
  let r = recover(() => typed(raw))
  if (r[1] != nil) {
    print(`boundary check caught: ${r[1].message}`)
  } else {
    print(`result: ${r[0]}`)
  }
}

processRaw(9) // result: 81
processRaw("hello") // boundary check caught: type contract violated…
print("typed_contracts ok")
