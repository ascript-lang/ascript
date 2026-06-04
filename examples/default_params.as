// Default parameters (SP2 §2): a parameter may carry a default value, evaluated
// at CALL time, LEFT-TO-RIGHT, ONLY for an omitted trailing argument. A default
// may reference earlier already-bound params and the enclosing scope. Min-arity
// is the count of leading params with no default; defaulted params are optional.
// Runs byte-identically on the bytecode VM and the `--tree-walker` oracle.

// A simple default: `b` defaults to 10 when omitted.
fn add(a, b = 10) {
  return a + b
}
print(add(1)) // 11 (b defaults)
print(add(1, 2)) // 3  (b supplied, default suppressed)

// A default may reference an EARLIER param (left-to-right binding).
fn grow(a, b = a * 2, c = a + b) {
  return [a, b, c]
}
print(grow(5)) // [5, 10, 15]
print(grow(5, 1)) // [5, 1, 6]
print(grow(5, 1, 0)) // [5, 1, 0]

// A default may call a function (any expression is allowed).
fn base() {
  return 100
}
fn withBase(x, y = base()) {
  return x + y
}
print(withBase(1)) // 101
print(withBase(1, 5)) // 6

// Defaults compose with a rest parameter: `a` required, `b` defaulted, `xs`
// collects the rest.
fn pack(a, b = 2, ...xs) {
  return [a, b, xs]
}
print(pack(1)) // [1, 2, []]
print(pack(1, 9)) // [1, 9, []]
print(pack(1, 9, 8, 7)) // [1, 9, [8, 7]]

// An explicitly-passed argument (even `nil`) suppresses the default — only a
// MISSING trailing argument triggers it.
fn keep(a, b = 10) {
  return b
}
print(keep(1, nil)) // nil

// A typed default: both the default value and an explicit value are contract-
// checked against the declared type.
fn typed(a, b: number = 1) {
  return a + b
}
print(typed(2)) // 3
print(typed(2, 4)) // 6

// Arrow functions support defaults too.
let scale = (x, factor = 2) => x * factor
print(scale(5)) // 10
print(scale(5, 3)) // 15
print("default_params ok")
