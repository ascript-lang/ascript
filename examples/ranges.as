// Ranges: `..` exclusive, `..=` inclusive, sequence direction, and signed `step`.
// Asserts the canonical truth table (design spec §3.5); exits 0 on both engines.

// Arrays compare by identity, so `arrayEq` checks contents element by element.
fn arrayEq(a: array<number>, b: array<number>): bool {
  if (len(a) != len(b)) {
    return false
  }
  for (i in 0..len(a)) {
    if (a[i] != b[i]) {
      return false
    }
  }
  return true
}

// --- Bare `..` is a SEQUENCE: direction is inferred from the bounds. ---------

// Ascending, exclusive: stops before the upper bound.
assert(arrayEq(1..5, [1, 2, 3, 4]), "1..5 ascending exclusive")

// Ascending, inclusive: includes the upper bound.
assert(arrayEq(1..=5, [1, 2, 3, 4, 5]), "1..=5 ascending inclusive")

// Descending bare range counts DOWN (start > end), exclusive endpoint.
assert(arrayEq(5..1, [5, 4, 3, 2]), "5..1 descending exclusive")

// Descending, inclusive.
assert(arrayEq(5..=1, [5, 4, 3, 2, 1]), "5..=1 descending inclusive")

// Empty / single-element edges: start == end never disagrees with direction.
assert(arrayEq(5..5, []), "5..5 is empty")
assert(arrayEq(5..=5, [5]), "5..=5 is a single element")

// --- `step`: signed; sign sets the direction; sign must agree with bounds. ---

// Positive step, ascending.
assert(arrayEq(1..10 step 2, [1, 3, 5, 7, 9]), "1..10 step 2")
assert(arrayEq(1..=10 step 2, [1, 3, 5, 7, 9]), "1..=10 step 2 (10 not on stride)")

// Negative step, descending.
assert(arrayEq(10..1 step -2, [10, 8, 6, 4, 2]), "10..1 step -2")
assert(arrayEq(10..=1 step -2, [10, 8, 6, 4, 2]), "10..=1 step -2")

// A step that overshoots simply stops; it is not an error.
assert(arrayEq(1..10 step 100, [1]), "1..10 step 100 overshoots to a single element")

// --- Float steps materialize too (rounding caveats apply for big ranges). ----
let quarters = 0..=1 step 0.25
assert(arrayEq(quarters, [0, 0.25, 0.5, 0.75, 1]), "0..=1 step 0.25")

// --- Value position: a range materializes to an `array<number>`. -------------
let r = 0..5
assert(len(r) == 5, "value range has length 5")
let total = 0
for (i in r) {
  total = total + i
}
assert(total == 10, "sum of 0..5 is 0+1+2+3+4 = 10")

// `..` is also valid in for-range directly (lazy, no intermediate array).
let count = 0
for (j in 10..=1 step -3) {
  count = count + 1
}
assert(count == 4, "10..=1 step -3 yields 10,7,4,1")

// --- Match patterns: stepped ranges are strided membership (anchor = start). -

// The stepped arm matches {1, 3, 5, 7, 9}; the plain arm catches the rest in range.
fn classify(n: number): string {
  return match n {
    1..=10 step 2 => "odd 1..10",
    1..=10 => "even 1..10",
    _ => "out of range",
  }
}
assert(classify(1) == "odd 1..10", "1 is on the odd stride")
assert(classify(9) == "odd 1..10", "9 is on the odd stride")
assert(classify(4) == "even 1..10", "4 is in range but off the stride")
assert(classify(10) == "even 1..10", "10 is inclusive but off the odd stride")
assert(classify(11) == "out of range", "11 is past the inclusive bound")

print("All range assertions passed.")
