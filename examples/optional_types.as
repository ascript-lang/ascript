// Nullable type suffix `T?` (sugar for `T | nil`) in every type position.
let a: number? = nil
let b: number? = 42
assert(a == nil, "a is nil")
assert(b == 42, "b is 42")

fn pick(x: string?): string? {
  return x
}
assert(pick(nil) == nil, "pick nil")
assert(pick("hi") == "hi", "pick hi")

print("optional_types ok")
