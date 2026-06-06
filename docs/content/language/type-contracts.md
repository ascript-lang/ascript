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
| `future<T>` | a `future` value (the result of calling an `async fn`) |
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

## Static type checking (advisory)

`ascript check` (and your editor, via the language server) layers an **advisory, gradual type
checker** over the same annotations. It is **static and advisory only** — it runs no code, never
changes runtime behaviour, and never gates execution. A program with type warnings still runs and
still produces identical output on both engines. Its job is to *predict* a likely runtime contract
violation before you run the program.

It is **gradual**: anything it cannot prove stays silent. An unannotated parameter, an `any`-typed
value, a value flowing through `any`, an `import`ed/stdlib result, or any expression whose type the
checker can't determine is treated as `any` and never produces a warning. Idiomatic untyped AScript
stays completely quiet — only *provably* wrong code is flagged.

It emits three diagnostics, all default-**Warning** and all configurable like every other lint
(`// ascript-ignore[type-mismatch]`, `--deny`/`--warn`/`--allow`, the `ascript.toml [lint]` table):

- **`type-mismatch`** — a value provably the wrong type for an **annotated slot**: a typed `let`/
  `const` initializer, a typed parameter at a call, a typed `return`, or a typed class-field default.

  ```ascript
  let count: number = "ten"          // type-mismatch: expected `number`, found `string`
  fn area(r: number): number { return r * r }
  let label = "5"
  area(label)                        // type-mismatch: argument 1 expects `number`, found `string`
  ```

- **`type-error`** — an operation provably ill-typed *regardless* of a declared slot: arithmetic on
  a provably non-numeric (and non-`string`-for-`+`) operand.

  ```ascript
  let name: string = "ada"
  let n = name - 1                   // type-error: arithmetic operand is `string`, not a number
  ```

- **`possibly-nil`** — a `T?` value dereferenced (member access, arithmetic, …) without a guard. It
  fires **only** when the receiver is provably `T?` *and* no narrowing applies, so it is shippable
  enabled-by-default.

  ```ascript
  fn inc(x: number?): number {
    return x + 1                     // possibly-nil: x is `number?` and may be nil here
  }
  ```

### Narrowing

The checker is **flow-sensitive**: a guard *narrows* a binding's type for the branch it dominates,
so a guarded `T?` deref is silent. The recognized forms are:

```ascript
fn ok(x: number?): number {
  if (x != nil) { return x + 1 }     // then-branch: x is `number` — silent
  return 0
}

fn ok2(x: number?): number {
  if (x == nil) { return 0 }         // early return …
  return x + 1                       // … so the tail sees x as `number` — silent
}

fn ok3(x: number?): number {
  let y = x ?? 0                     // ?? narrows the left operand to non-nil
  return y + 1
}

fn ok4(x: number?): number {
  if (x) { return x + 1 }            // truthiness narrows away nil only
  return 0
}
```

`match` arms narrow the subject to each arm's pattern, and (with the `instanceof` operator) an
`if (x instanceof Dog)` narrows `x` to `Dog` in the then-branch. Narrowing keys off the resolved
binding, not the name, so it never leaks across an aliasing `let` or a closure boundary.

### Local inference

Bindings without an annotation are **inferred** from their initializer, and a same-file function's
return type is inferred from its `return`s — so a downstream typed slot can still be checked without
you annotating everything:

```ascript
fn id(x: number) { return x }        // inferred return: number
let y = id(1)                        // y : number (inferred)
let z: string = y                    // type-mismatch: expected `string`, found `number`
```

Inference is **intra-procedural and in-file**: parameters default to `any`, and a cross-module
callee's result is `any` (the checker draws the same module line SP4 drew for arity — in-file yes,
cross-module no).

> [!NOTE] **Deprecation.** The older `contract-mismatch` (literal-argument-only) and
> `field-default-type` (literal-field-default-only) lints are **subsumed** by `type-mismatch`, which
> checks *any* synthesizable expression, not just literals. Both legacy codes still fire on their
> exact old cases for one release (so an existing `ascript.toml` naming them keeps working) and the
> new pass suppresses its own duplicate `type-mismatch` at the same span; prefer `type-mismatch`
> going forward. In a later release the legacy rules become accepted-but-no-op config aliases.
