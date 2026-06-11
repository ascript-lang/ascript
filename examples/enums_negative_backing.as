// Enum backing values may be negative integers (NUM: integer literals are `int`).
// A negative backing is the only constant unary form allowed; `.value` reflects it.

enum Status {
  Error = -1,
  Ok = 0,
  Pending = 1,
}

print(Status.Error.value)    // -1
print(Status.Ok.value)       // 0
print(Status.Pending.value)  // 1

// Negative floats work too (the original constant-folding path).
enum Temp {
  Freezing = -2.5,
  Boiling = 100,
}

print(Temp.Freezing.value)   // -2.5
