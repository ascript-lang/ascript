// Range `..` as a general expression and `let` without an initializer.

// `..` builds an eager half-open array<number>.
let r = 0..5
print(r)

// Sum a range value with a declare-then-assign accumulator.
let total
total = 0
for (i in r) {
  total = total + i
}
print(total)

// `..` works anywhere an expression is allowed: as a call argument, and with
// precedence tighter than comparison but looser than `+`.
print((1 + 1)..4)

// Typed declaration without an initializer; assigned later.
let count: number
count = len(r)
print(count)

// Literal range in a for-in still uses the lazy loop path.
let doubled = 0
for (j in 0..3) {
  doubled = doubled + j * 2
}
print(doubled)
