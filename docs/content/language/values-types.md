:::eyebrow Language

# Values & types

AScript has a small, fixed set of value kinds. `type(v)` returns the kind name as a string.

## The value kinds

| Kind | `type(v)` | Literal | Prints as |
|---|---|---|---|
| Nil | `nil` | `nil` | `nil` |
| Boolean | `bool` | `true`, `false` | `true` / `false` |
| Int | `int` | `42`, `0xFF`, `0b1010`, `0o17`, `1_000` | `42`, `255` |
| Float | `float` | `3.14`, `1e9`, `.5` | always a decimal (`5.0`, `2.5`) |
| String | `string` | `"double"`, `'single'`, `` `template ${x}` `` | the raw text |
| Array | `array` | `[1, 2, 3]` | `[1, "two"]` |
| Object | `object` | `{ key: value, "quoted": 1 }` | `{a: 1, b: "x"}` |
| Map | `map` | `#{ keyExpr: value }` (or `std/map`) | `map {"a": 1}` |
| Set | `set` | *(constructed via `std/set`)* | `set {1, "two"}` |
| Decimal | `decimal` | *(constructed via `std/decimal`)* | `1.50`, `42` |
| Bytes | `bytes` | *(via `std/bytes`)* | `<bytes len N>` |
| Regex | `regex` | *(via `std/regex`)* | `<regex SOURCE>` |
| Function | `function` | `fn` / arrow / builtin | `<function name>` |
| Class | `class` | `class Name { … }` | `<class Name>` |
| Instance | `instance` | `Name(args)` | `<Name instance>` |
| Enum | `enum` | `enum Name { … }` | `<enum Name>` |
| Future | `future` | *(calling an `async fn`)* | `<future>` |
| Generator | `generator` | *(calling a `fn*` or `async fn*`)* | `<generator>` |

## Numbers

AScript has a small **numeric tower**: one user concept ("a number") realized as distinct runtime
subtypes so division can be type-directed and diagnostics stay clear.

| Subtype | `type(v)` | Representation | Literal form |
|---|---|---|---|
| `int` | `"int"` | 64-bit signed integer | `5`, `0xFF`, `0b1010`, `0o17`, `1_000` |
| `float` | `"float"` | IEEE-754 double | `5.0`, `1.5`, `.5`, `1e3` |
| `decimal` | `"decimal"` | exact base-10 | `decimal.from("0.1")` |

A literal with **no `.` and no exponent** is an `int`; a literal with a `.` or an exponent is a
`float`. Bases: decimal, hex (`0x`), binary (`0b`), and octal (`0o`); underscores group digits. The
annotation `number` means the union `int | float` (decimal is exact and opt-in — not part of `number`).

```ascript
print(0xFF)       // 255   (int)
print(0o17)       // 15    (int, octal)
print(0b1010)     // 10    (int)
print(1_000_000)  // 1000000
print(type(5))    // int
print(type(5.0))  // float
```

**Type-directed division.** `int / int` truncates toward zero; any `float` operand makes the whole
expression a `float`. There is no `//` operator.

```ascript
print(7 / 2)      // 3     (int / int → int, truncated)
print(-7 / 2)     // -3    (toward zero)
print(7.0 / 2)    // 3.5   (a float operand → float division)
```

**Checked overflow + wrapping operators.** `+ - * **` and unary `-` trap on i64 overflow with a
recoverable panic. Use the explicit wrapping operators `+%`, `-%`, `*%` for two's-complement modular
arithmetic (hashing, codecs).

**Bitwise / shift operators** (`& | ^ << >> ~`) are **int-only** (Go-style precedence). Code points
are `int`s (the Go "rune" model — no `char` type); convert with
[`string.codepoints`](../stdlib/collections#stringcodepoints) /
`string.from_codepoints` / `string.code_at`.

```ascript
let color = (0xAB << 16) | (0xCD << 8) | 0xEF
print((color >> 8) & 0xFF)   // 205
```

**Printing.** An `int` prints with no decimal (`5`); a `float` **always** prints with at least one
fractional digit (`5.0`, `1500.0`, `-0.0`) so the two subtypes are visually distinguishable
(`inf`/`-inf`/`NaN` are unchanged).

```ascript
print(5)          // 5
print(5.0)        // 5.0
print(10.0 / 2)   // 5.0
print([1.0, 2.0]) // [1.0, 2.0]
```

**Conversions.** `int(x)` and `float(x)` convert between the subtypes: `int(5.7)` truncates toward
zero → `5`; `float(3)` → `3.0`; `int("42")` / `float("3.5")` parse a string and return a Tier-1
`[value, err]` pair (`int("x")` → `[nil, err]`). An identity conversion (`int` of an `int`, `float` of
a `float`) is a no-op.

```ascript
print(int(5.7))     // 5    (truncates toward zero)
print(float(3))     // 3.0
print(int("42"))    // [42, nil]
print(int("x"))     // [nil, {message: ...}]
```

**Exact cross-subtype comparison.** `1 == 1.0` is `true`, but a large `int` not exactly representable
as a `float` compares exactly — no precision bug at the `2^53` boundary. A `float` at or above `2^63`
(`9223372036854775808.0` and up) is past the `int` range, so it is **never equal to any `int`** and
**never shares a map key** with one — e.g. `9223372036854775808.0 == 9223372036854775807` is `false`.

**Reflection / narrowing.** `x instanceof int` / `float` / `number` are runtime type guards (the
checker narrows `number` to the subtype in the guarded branch):

```ascript
fn describe(x: number): string {
  if (x instanceof int) { return "an int" }
  return "a float"
}
```

## Truthiness

The falsy values are `nil`, `false`, `0` (and `0.0`/`-0.0`/`NaN`), `0m` (zero decimal), and the empty
string `""`. **Everything else is truthy** — and crucially, **collections stay truthy even when empty**
(`[]`, `{}`, an empty map/set), so an empty-but-valid collection never silently reads as "no result".

```ascript
if (0)  { } else { print("0 is falsy") }       // falsy
if ("") { } else { print("\"\" is falsy") }    // falsy
if ([]) { print("[] is truthy") }              // truthy — query emptiness with len([])
```

## Equality

`==` is **structural** for `nil`, `bool`, `number`, and `string`. For the heap kinds — `array`,
`object`, `map`, `bytes`, `regex`, `function`, `class`, `instance` — it is **identity** (reference)
equality. There is no cross-kind coercion.

```ascript
1 == 1.0              // true
"a" == "a"            // true
1 == "1"             // false — no coercion between kinds

[1, 2] == [1, 2]      // false — two distinct arrays
let xs = [1, 2]
xs == xs              // true  — same reference
```

> [!NOTE] To compare arrays or objects by content, compare their elements/fields, or serialize both
> with `std/json` and compare the strings.

## Value vs reference semantics

`nil`, `bool`, `number`, and `string` are value-semantic (copied on assignment). `array`, `object`,
`map`, `set`, `bytes`, and class instances are heap values shared **by reference** — assignment copies the
handle, not the contents.

```ascript
let a = [1, 2, 3]
let b = a            // b and a refer to the SAME array
import * as array from "std/array"
array.push(b, 4)
print(a)             // [1, 2, 3, 4]
```

## Objects vs maps

An **object** is a string-keyed, insertion-ordered record — keys are written as identifiers or
quoted strings, and member access uses `.` or `["key"]`. A **map** (from [`std/map`](../stdlib/collections))
is a general collection whose keys may be `nil`, `bool`, `number`, or `string`. Reach for an object
for record-like data with known fields, and a map when keys are dynamic or non-string.

```ascript
let point = { x: 1, y: 2 }      // object
print(point.x)                  // 1

import * as map from "std/map"
let scores = map.new()
map.set(scores, 42, "the answer")   // numeric key — needs a map, not an object
```

### Map literals — `#{…}`

A **map literal** `#{ keyExpr: valueExpr, … }` builds a `map` directly, with no `std/map` import
needed. Unlike an object literal `{a: 1}` — where the key `a` is the literal name — a map-literal key
is an **evaluated expression**, so its *value* becomes the key:

```ascript
let scores = #{ "alice": 10, "bob": 7 }   // string keys
let mixed = #{ 1: "one", true: "yes", nil: "none" }   // number / bool / nil keys

let k = "dynamic"
let m = #{ k: 42, 1 + 1: "two" }   // keyed by "dynamic" and 2 — the VALUES
print(m)                           // map {"dynamic": 42, 2: "two"}
```

- `#{}` is the empty map.
- Keys follow the same rules as `std/map`: only `nil`, `bool`, `number`, `string` (and `decimal`)
  are hashable; numbers are canonicalized (`-0.0` → `0.0`, all `NaN` unified). Using a container,
  function, or instance as a key is a runtime error (`cannot use <type> as a map key`).
- **Later-key-wins:** a duplicate key keeps the last value while preserving the first-seen position,
  so `#{ 1: "a", 1: "b" }` is `map {1: "b"}`.
- Read values back with [`std/map`](../stdlib/collections) (`map.get`, `map.has`, `map.keys`, …);
  map literals interoperate fully with that API. (Spread `#{ ...m }` is not supported.)

## Sets

A **set** (from [`std/set`](../stdlib/collections#stdset)) is an insertion-ordered collection of **unique hashable values** (`nil`, `bool`, `number`, or `string`). There is no set literal — construct via `set.new()` or `set.from(array)`. The built-in `len(s)` function works on sets.

```ascript
import * as set from "std/set"

let s = set.from([1, 1, 2, 3])   // deduplicates: set {1, 2, 3}
set.add(s, 4)
set.has(s, 2)                    // true
len(s)                           // 4
set.values(s)                    // [1, 2, 3, 4]  (insertion order)
```

## Exact decimal numbers

A **decimal** (from [`std/decimal`](../stdlib/data#stddecimal)) is a 96-bit scaled-integer decimal value for exact arithmetic. There is no decimal literal — use `decimal.from(x)` or `decimal.parse(s)`. Once constructed, the standard `+`, `-`, `*`, `/`, `%`, and comparison operators work, with automatic coercion of `number` operands.

```ascript
import * as decimal from "std/decimal"

// floating-point cannot represent this — decimal can:
decimal.from("0.1") + decimal.from("0.2") == decimal.from("0.3")   // true

let price = decimal.from("9.99")
let tax   = decimal.from("0.08") * price
decimal.toString(decimal.round(tax, 2))   // "0.80"
```
