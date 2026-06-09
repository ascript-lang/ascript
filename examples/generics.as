// Generics: type parameters on functions, classes, and enums — inferred at the
// call site and ERASED at runtime (a `T`-annotated slot accepts any value). The
// advisory checker uses them only statically: a provably-wrong ANNOTATED slot is
// a blocking error, while anything inferred or `any`-typed stays gradual (silent).
import * as array from "std/array"

// A generic function: `<A, B>` are inferred from the arguments. `fn(A) -> B` is
// a function-type annotation for the callback.
fn map<A, B>(xs: array<A>, f: fn(A) -> B): array<B> {
  let out = []
  for (x in xs) {
    array.push(out, f(x))
  }
  return out
}

let lengths = map(["a", "bb", "ccc"], (s) => len(s))
print(lengths) // [1, 2, 3]

// A combinator with TWO same-typed params `<T>(a: T, b: T)`. Calling it with
// MIXED numerics (`int` and `float`) stays gradual — `T` joins to `number`, no
// false positive — and runs fine because generics erase at runtime.
fn maxOf<T>(a: T, b: T): T {
  if (a > b) {
    return a
  }
  return b
}
print(maxOf(1, 2.0)) // 2.0
print(maxOf(7, 3)) // 7

// A generic class: `Box<T>` holds one value of the inferred element type.
class Box<T> {
  value: T
  fn get(): T {
    return self.value
  }
}

let b = Box(5)
print(b.get()) // 5

// A generic stack with `push`/`pop`. A `T?` return marks `pop` as nullable.
class Stack<T> {
  items: array<T> = []
  fn push(x: T) {
    array.push(self.items, x)
  }
  fn pop(): T? {
    if (len(self.items) == 0) {
      return nil
    }
    return array.pop(self.items)
  }
  fn size(): number {
    return len(self.items)
  }
}

let s = Stack()
s.push(10)
s.push(20)
print(s.size()) // 2
print(s.pop()) // 20
print(s.pop()) // 10
print(s.pop()) // nil

// A generic enum: `Option<T>` is the canonical optional-payload ADT.
enum Option<T> {
  Some(value: T),
  None,
}

fn unwrapOr<T>(o: Option<T>, fallback: T): T {
  return match o {
    Some(v) => v,
    Option.None => fallback,
  }
}

print(unwrapOr(Option.Some(7), 0)) // 7
print(unwrapOr(Option.None, -1)) // -1

// A bounded type parameter: `C: Container<T>` requires `C` to structurally
// satisfy `Container<T>` (have an `at(number) -> T` method). Conformance is
// structural — `IntList` satisfies it without an explicit `implements` clause.
interface Container<T> {
  fn at(i: number): T
}

class IntList {
  data: array<number> = []
  fn at(i: number): number {
    return self.data[i]
  }
}

fn first<T, C: Container<T>>(c: C): T {
  return c.at(0)
}

let list = IntList()
list.data = [100, 200, 300]
print(first(list)) // 100
