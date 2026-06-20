# Values & types

This chapter is the authoritative inventory of AScript's runtime **values** and
the **numeric model**. It is grounded in the reference runtime (`src/value.rs`,
`src/interp.rs`). Terms are as defined in the [notation chapter](intro); the
syntax of literals is in the [lexical chapter](lexical), and the type *grammar*
is in the [grammar chapter](grammar).

A **value** is a runtime datum. Every value has exactly one **kind**, reported by
the `type(x)` builtin. Kinds are disjoint: there is no implicit cross-kind
coercion anywhere in the language.

## Value kinds

AScript has the following value kinds. The middle column is the exact string
`type(x)` returns.

| Kind | `type(x)` | Mutable? | Equality |
| --- | --- | --- | --- |
| nil | `nil` | — | structural |
| boolean | `bool` | — | structural |
| integer (i64) | `int` | — | structural (numeric) |
| float (f64) | `float` | — | structural (numeric) |
| decimal (exact) | `decimal` | — | structural (numeric) |
| string | `string` | no (immutable) | structural |
| function / closure / builtin | `function` | — | identity |
| array | `array` | yes | identity |
| object (insertion-ordered) | `object` | yes | identity |
| map | `map` | yes | identity |
| set | `set` | yes | identity |
| bytes | `bytes` | yes | identity |
| regex | `regex` | no | identity |
| native handle | _(per handle, e.g. `tcpStream`)_ | n/a | identity |
| enum | `enum` | no | identity |
| enum variant | `enum variant` | no | structural (payload) |
| class | `class` | no | identity |
| interface | `interface` | no | identity |
| instance | `instance` | yes | identity |
| future | `future` | n/a | identity |
| generator | `generator` | n/a | identity |

A few notes on the table:

- The three callable forms (a user `function`, a closure, a native `builtin`),
  bound methods, and class/enum constructor methods all report `type(x) ==
  "function"`. They are distinct internal representations of one user-facing kind.
- A **native handle** (an OS resource — a TCP stream, a child process, an HTTP
  body, an FFI symbol, …) reports a handle-specific string (`tcpStream`,
  `childProcess`, `httpBody`, …). Native handles are not embedded in the value
  union; a value references one by an opaque id. They are non-sendable across
  worker isolates and are reclaimed by deterministic destruction.
- An **interface** value is an immutable structural descriptor (a named method
  set), never a receiver. `v instanceof I` performs a structural conformance
  check (see the *Classes* chapter).
- A **frozen `shared`** value (the *Concurrency* chapter) reports its UNDERLYING
  kind: `type(shared.freeze({})) == "object"`. It is the only value kind that
  crosses a worker boundary by reference rather than by copy.

`object` preserves **insertion order** of its keys; `map` and `set` likewise
preserve insertion order. Iterating an object, map, or set yields its entries in
the order they were inserted.

## Numbers

AScript has two numeric **subtypes**, `int` and `float`, plus an exact `decimal`.

- An integer literal denotes an **`int`** — a signed 64-bit two's-complement
  integer. A numeric literal with a fractional part or a decimal exponent denotes
  a **`float`** — a 64-bit IEEE-754 double. The lexical forms are in the
  [lexical chapter](lexical).
- `number` is the annotation **supertype** `int | float`. It is not a runtime
  kind: `type(5)` is `int` and `type(5.0)` is `float`, never `number`.
- `decimal` is a separate exact kind with no literal syntax; it is constructed
  through `std/decimal`.

### Type-directed division

Division is **type-directed**. `int / int` truncates toward zero; if either
operand is a `float`, the result is a `float`.

```as
print(7 / 2)     // 3      (int / int truncates toward zero)
print(-7 / 2)    // -3     (toward zero, not toward -infinity)
print(7.0 / 2)   // 3.5    (a float operand promotes)
```

### Overflow traps

The operators `+`, `-`, `*`, `**`, and unary `-` MUST **trap** on `int`
overflow: an i64 overflow is a recoverable Tier-2 panic (`integer overflow in
'+'`), never a silent wraparound. The explicit wrapping operators `+%`, `-%`,
`*%` perform two's-complement wraparound instead.

```as
let big = 9223372036854775807   // i64::MAX
print(big +% 1)                 // -9223372036854775808  (wraps)
// print(big + 1)               // Tier-2 panic: integer overflow in '+'
```

### Bitwise and shift operators

The bitwise and shift operators `&`, `|`, `^`, `~`, `<<`, `>>` are **int-only**.
A float operand is a Tier-2 panic (`bitwise op requires int operands, got
float`). Code points are `int`s.

```as
print(6 & 3)     // 2
print(1 << 4)    // 16
print(~0)        // -1
```

### Printing

A `float` ALWAYS prints with a fractional digit; an `int` never does. This makes
the two subtypes visually distinguishable.

```as
print(5)         // 5
print(5.0)       // 5.0
print(5.0 / 1.0) // 5.0    (a float result stays a float)
```

Some `std/math` results that are integer-valued nonetheless carry the `float`
subtype and print with a trailing decimal (for example `sqrt(9.0)` prints
`3.0`); only the explicitly-integer helpers (`abs`, the rounding family, the
integer-division helpers, and the bit helpers) return an `int`.

## Truthiness

Truthiness is consulted by `if`, `while`, the ternary, and the logical
operators. The **falsy set** is exactly:

- `nil`
- `false`
- the number zero in any subtype: `0`, `0.0`, `-0.0`, `NaN`, and a zero
  `decimal`
- the empty string `""`

Every other value is **truthy**. In particular, **all collections are truthy,
including empty ones**: an empty array, object, map, or set is truthy. Query
emptiness with `len(x)`, not with truthiness.

```as
if ([]) { print("empty-array-truthy") }   // prints
if ({}) { print("empty-object-truthy") }  // prints
print(len([]))                            // 0
if (0) { print("t") } else { print("f") } // f  (0 is falsy)
if ("") { print("t") } else { print("f") }// f  ("" is falsy)
```

## Equality & identity

The `==` operator never coerces across kinds: `1 == "1"` is `false`.

- For `nil`, `bool`, the three numeric kinds, and `string`, `==` compares by
  **value**. Equality across the numeric subtypes is exact: `1 == 1.0` is `true`.
- For `array`, `object`, `map`, `set`, `bytes`, the callable kinds, `future`,
  `generator`, `regex`, and class `instance`s, `==` compares by **pointer
  identity**. Two structurally equal but distinct arrays are NOT `==`.
- A constructed **enum-variant payload** compares **structurally**: two
  `Shape.Circle(1.0)` values are `==`.

```as
print([1] == [1])   // false   (distinct arrays — identity)
print(1 == 1.0)     // true    (numeric — value)
print(1 == "1")     // false   (no cross-kind coercion)
```

## `instanceof` as a type guard

`x instanceof T` accepts a class, an interface, or one of the reserved type
names `int`, `float`, `number`, `string`, `bool` on the right. With a reserved
type name it is a runtime **type guard**: it tests the value's kind. With a
class it walks the nominal chain; with an interface it checks structural
conformance (see the *Classes* chapter). Any other right-hand side is a Tier-2
panic.

```as
print(5 instanceof int)       // true
print(5.0 instanceof float)   // true
print(5 instanceof number)    // true   (int is a member of number)
print(5.0 instanceof number)  // true
print(5 instanceof float)     // false
print("x" instanceof string)  // true
```

## Nullable types `T?`

A type annotation `T?` is sugar for the union `T | nil`: a slot annotated `T?`
admits a `T` or `nil`. `T?` is valid in every type position, including inside
`future<T>` and as a class-field marker (`name?: T`). The static checker tracks
nil-ness and warns on a `possibly-nil` dereference (see the *Types* chapter).

## Conformance

The value kinds, the numeric model, truthiness, and equality in this chapter are
exercised by:

- `examples/numbers.as` — integer literal forms, type-directed division, and the
  `int`/`float` print distinction.
- `examples/integers.as` — `int` semantics, overflow trapping, the wrapping
  operators, and int-only bitwise/shift operators.
- `examples/core_types.as` — the value-kind inventory and `type(x)` reporting.
- `tests/vm_differential.rs` — the equality/identity battery
  (`vm_equality_matches_treewalker`) asserts container identity vs numeric value
  equality four-mode byte-identically (tree-walker, specialized VM, generic VM,
  and `.aso`).

Run them with `target/release/ascript run examples/numbers.as` (and likewise for
`integers.as`, `core_types.as`); each matches its recorded golden.
