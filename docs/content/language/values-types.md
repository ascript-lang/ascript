:::eyebrow Language

# Values & types

AScript has a small, fixed set of value kinds. `type(v)` returns the kind name as a string.

## The value kinds

| Kind | `type(v)` | Literal | Prints as |
|---|---|---|---|
| Nil | `nil` | `nil` | `nil` |
| Boolean | `bool` | `true`, `false` | `true` / `false` |
| Number | `number` | `42`, `3.14`, `1e9`, `0xFF`, `0b1010` | minimal form (`7`, `2.5`) |
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

## Numbers

There is exactly **one** numeric type. Every literal — decimal, float, exponent, hex (`0xFF`), binary
(`0b1010`), and digit-grouped (`1_000_000`) — becomes an IEEE-754 64-bit float.

```ascript
print(0xFF)       // 255
print(0b1010)     // 10
print(1e3)        // 1000
print(1_000_000)  // 1000000
print(7.0)        // 7   — integer-valued floats print without a fractional part
```

## Truthiness

Only `nil` and `false` are falsy. **Everything else is truthy** — including `0`, `""`, `[]`, and
`{}`. This is deliberately stricter (and less surprising) than JavaScript.

```ascript
if (0)  { print("zero is truthy") }   // this runs
if ("") { print("empty is truthy") }  // this runs too
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
`map`, `bytes`, and class instances are heap values shared **by reference** — assignment copies the
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
