:::eyebrow Language

# Type contracts

Type annotations in AScript are **optional** and **gradual**. When you write one, it is enforced at
runtime as a *contract* — checked at the moment a value crosses the boundary, never statically
checked and never erased. Omit them and the code runs exactly the same, untyped.

```ascript
fn add(a: number, b: number): number {
  return a + b
}

let userName: string = "ada"
const ids: array<number> = [1, 2, 3]
```

## Where contracts fire

A value is checked at exactly three kinds of boundary:

- a typed `let` / `const` binding, when it is initialized;
- a typed function **parameter**, on entry to the call;
- a typed function **return**, on the way out.

A contract failure is a [Tier-2 panic](errors) — it signals a bug, not a recoverable condition.

```ascript
fn scale(factor: number): number {
  return factor * 2
}

scale("3")   // panic: type contract violated: expected number, got string ("3")
```

## The type grammar

| Type | Accepts |
|---|---|
| `any` | anything (also the meaning of an omitted annotation) |
| `number` | a number |
| `string` | a string |
| `bool` | a boolean |
| `nil` | nil |
| `object` | an object |
| `fn` | a function or builtin |
| `error` | an object **or** nil |
| `array<T>` | an array whose every element satisfies `T` |
| `map<K, V>` | a map whose keys satisfy `K` and values satisfy `V` |
| `Result<T>` | a pair `[a, b]` where `a` is `T` or nil, and `b` is `error` |
| `[T1, T2, …]` | a fixed-length tuple, matched positionally |
| `T1 \| T2` | a union — satisfied if either side matches |
| `ClassName` | an instance of that class or any subclass |
| `EnumName` | any variant of that enum |
| `T?` | `T` **or** nil — the nullable suffix, sugar for `T \| nil` |

## The nullable suffix `T?`

A trailing `?` on any type makes it nullable: `T?` is exactly `T | nil`. It is valid in **every**
type position — `let` / `const` bindings, function parameters, return types, and class fields — and
renders canonically as `T?` (the formatter normalizes an explicit `T | nil` written this way only
when you spell it as the suffix; an explicit union is left as you wrote it).

```ascript
let a: number? = nil    // ok — nil satisfies number?
let b: number? = 42     // ok — and so does a number

fn pick(x: string?): string? {
  return x              // accepts and returns string-or-nil
}
```

Because `T?` is just `T | nil`, it composes with the rest of the grammar: `array<string?>` is an
array whose elements are each a string or nil, and a [class field](classes-enums) declared
`nickname: string?` is an optional field that defaults to nil.

## Contracts are checked eagerly and deeply

A container contract is verified to its full declared depth at the check site. `array<number>`
confirms the value is an array **and** scans every element; `map<string, array<number>>` recurses
likewise. This is O(n) at the boundary — opt out with a bare `array` / `object` / `any` when you
don't want the element scan.

```ascript
let nums: array<number> = [1, 2, 3]      // ok — every element is a number
let bad:  array<number> = [1, "two", 3]  // panic — element 1 is a string
```

## Typing fallible functions

Use `Result<T>` — not `[T, T]` — to type a function that returns a [result pair](errors). `Result<T>`
correctly permits both the success shape `[value, nil]` and the failure shape `[nil, errObj]`, where
a naïve `[T, T]` would reject the failure case.

```ascript
fn lookup(id: number): Result<object> {
  if (id < 0) { return Err("negative id") }
  return Ok({ id: id })
}
```

## Class and enum types

A class name is a valid contract type that accepts instances of that class and its subclasses. An
enum name accepts any variant of that enum. See [Classes, enums, match](classes-enums).

```ascript
class Shape {}
class Circle extends Shape {}

fn area(s: Shape): number { /* … */ return 0 }
area(Circle())   // ok — Circle is a Shape
```

> [!NOTE] Because contracts run at the boundary, they double as living, machine-checked
> documentation: the annotation can never silently drift out of sync with the code's actual
> behaviour, the way a comment can.
