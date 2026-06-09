// A production-shaped generic `Result<T, E>` combinator pipeline: a sum type that
// is either an `Ok(value)` or an `Err(error)`, with `map`/`andThen`/`unwrapOr`
// combinators that thread the success type through while leaving the error type
// untouched. Generic type parameters are inferred at every call site and ERASED at
// runtime; the advisory checker uses them only statically (a provably-wrong
// ANNOTATED argument is a blocking error, anything inferred stays gradual/silent).
import * as array from "std/array"

// The result sum type. `Ok` carries a success payload of type `T`; `Err` carries an
// error payload of type `E`.
enum Result2<T, E> {
  Ok(value: T),
  Err(error: E),
}

// `map` transforms the success value, leaving an `Err` untouched. `<T, U, E>` are
// inferred: `T`/`E` from the result, `U` from the callback's return.
fn mapOk<T, U, E>(r: Result2<T, E>, f: fn(T) -> U): Result2<U, E> {
  return match r {
    Ok(v) => Result2.Ok(f(v)),
    Err(e) => Result2.Err(e),
  }
}

// `andThen` chains a fallible step: the callback itself returns a `Result2`.
fn andThen<T, U, E>(r: Result2<T, E>, f: fn(T) -> Result2<U, E>): Result2<U, E> {
  return match r {
    Ok(v) => f(v),
    Err(e) => Result2.Err(e),
  }
}

// `unwrapOr` collapses to a plain value, substituting a fallback on `Err`.
fn unwrapOr<T, E>(r: Result2<T, E>, fallback: T): T {
  return match r {
    Ok(v) => v,
    Err(_) => fallback,
  }
}

// A fallible validation step: a non-negative integer → `Ok`, else `Err`.
fn checkPositive(n: int): Result2<int, string> {
  if (n < 0) {
    return Result2.Err(`negative: ${n}`)
  }
  return Result2.Ok(n)
}

// The pipeline: validate, double, then format — short-circuiting on the first
// `Err`. Each `mapOk` infers its own success-type transition.
fn pipeline(n: int): Result2<string, string> {
  let checked = checkPositive(n)
  let doubled = mapOk(checked, (x) => x * 2)
  return mapOk(doubled, (x) => `value=${x}`)
}

print(unwrapOr(pipeline(21), "fallback")) // value=42
print(unwrapOr(pipeline(-3), "fallback")) // fallback

// `andThen` chains two fallible steps; an `Err` from the first short-circuits.
fn ensureEven(n: int): Result2<int, string> {
  if (n % 2 == 0) {
    return Result2.Ok(n)
  }
  return Result2.Err(`odd: ${n}`)
}

print(unwrapOr(andThen(checkPositive(8), ensureEven), -1)) // 8
print(unwrapOr(andThen(checkPositive(7), ensureEven), -1)) // -1
print(unwrapOr(andThen(checkPositive(-2), ensureEven), -1)) // -1

// An EXPLICIT type argument on a generic constructor call: `Box<string>("hi")`
// pins the element type up front rather than inferring it from the argument.
class Box<T> {
  value: T
  fn get(): T {
    return self.value
  }
}

let labeled = Box<string>("ready")
print(labeled.get()) // ready

// A bounded generic over a structural interface: `report` accepts any value with a
// `count() -> number` method (structural conformance — no explicit `implements`).
interface Sized {
  fn count(): number
}

class Bag {
  items: array<number> = []
  fn count(): number {
    return len(self.items)
  }
}

fn report<C: Sized>(c: C): string {
  return `items: ${c.count()}`
}

let bag = Bag()
array.push(bag.items, 1)
array.push(bag.items, 2)
array.push(bag.items, 3)
print(report(bag)) // items: 3
