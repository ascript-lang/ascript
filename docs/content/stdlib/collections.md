:::eyebrow Standard library

# Core & collections

The core and collection modules — `string`, `array`, `object`, `map`, `math`, `convert`, and `bytes` — are imported like any other standard module and called in **qualified form**. The collection or value you operate on is always the **first argument**:

```ascript
import * as array from "std/array"

let nums = [1, 2, 3]
let doubled = array.map(nums, fn(x) { x * 2 })   // [2, 4, 6]
```

There is **no method-call convention** for these modules: you write `array.map(arr, fn)`, never `arr.map(fn)`. Likewise `string.slice(s, 1, 4)`, `object.keys(o)`, and so on. Each module is imported with its own `import * as <name> from "std/<name>"` line.

> [!NOTE] Most functions panic (Tier-2) on wrong argument *types*. A few return recoverable `[value, err]` pairs (Tier-1) — these are flagged.

## std/string

String manipulation. All indices and lengths count **characters** (Unicode scalar values), not bytes.

```ascript
import * as string from "std/string"
```

### string.split

Split a string into an array of substrings on a separator. An empty separator splits into individual characters.

- `s: string` — the string to split
- `sep: string` — the separator
- Returns: `array` of `string`

```ascript
string.split("a,b,c", ",")   // ["a", "b", "c"]
string.split("abc", "")      // ["a", "b", "c"]
```

### string.join

Join an array into a single string, inserting a separator between elements. Non-string elements are converted to their display form.

- `arr: array` — the elements to join
- `sep: string` — the separator
- Returns: `string`

```ascript
string.join(["a", "b", "c"], "-")   // "a-b-c"
```

### string.slice

Extract a substring between two character indices. Negative indices count from the end; the end argument defaults to the string length. If `start >= end`, the result is empty.

- `s: string` — the source string
- `start: number` — start index (negative counts from end)
- `end: number` (optional) — end index, exclusive; defaults to length
- Returns: `string`

```ascript
string.slice("hello", 1, 4)   // "ell"
string.slice("hello", -2)     // "lo"
```

### string.trim

Remove leading and trailing whitespace.

- `s: string` — the source string
- Returns: `string`

```ascript
string.trim("  hi  ")   // "hi"
```

### string.upper

Convert a string to uppercase.

- `s: string` — the source string
- Returns: `string`

```ascript
string.upper("aB")   // "AB"
```

### string.lower

Convert a string to lowercase.

- `s: string` — the source string
- Returns: `string`

```ascript
string.lower("aB")   // "ab"
```

### string.find

Find the character index of the first occurrence of a substring. Returns `-1` if not found.

- `s: string` — the string to search
- `sub: string` — the substring to find
- Returns: `number` — character index, or `-1`

```ascript
string.find("hello", "ll")   // 2
string.find("hello", "z")    // -1
```

### string.replace

Replace the **first** occurrence of a substring. An empty `from` returns the input unchanged.

- `s: string` — the source string
- `from: string` — the substring to replace
- `to: string` — the replacement
- Returns: `string`

```ascript
string.replace("a.b.c", ".", "-")      // "a-b.c"  (first only)
string.replaceAll("a.b.c", ".", "-")   // "a-b-c"  (all)
```

### string.replaceAll

Replace **all** occurrences of a substring. An empty `from` returns the input unchanged.

- `s: string` — the source string
- `from: string` — the substring to replace
- `to: string` — the replacement
- Returns: `string`

```ascript
string.replaceAll("a.b.c", ".", "-")   // "a-b-c"  (every occurrence)
```

### string.format

Substitute positional arguments into a template. Each `{}` consumes the next argument in order; `{{` and `}}` are literal braces. Arguments are converted to their display form.

- `template: string` — the template, with `{}` placeholders
- `...args` — values substituted in order
- Returns: `string`

> [!TIER2] Panics on too few arguments for the placeholders in the template.

```ascript
string.format("{} + {} = {}", 1, 2, 3)   // "1 + 2 = 3"
string.format("{{literal}}")             // "{literal}"
```

### string.padStart

Pad the start of a string with a fill string until it reaches a target character width. Returns the input unchanged if it is already at least `width` characters, or if `fill` is empty.

- `s: string` — the source string
- `width: number` — target width in characters
- `fill: string` (optional) — fill string, defaults to a single space
- Returns: `string`

```ascript
string.padStart("7", 3, "0")   // "007"
```

### string.padEnd

Pad the end of a string with a fill string until it reaches a target character width. Returns the input unchanged if it is already at least `width` characters, or if `fill` is empty.

- `s: string` — the source string
- `width: number` — target width in characters
- `fill: string` (optional) — fill string, defaults to a single space
- Returns: `string`

```ascript
string.padEnd("7", 3)   // "7  "
```

### string.repeat

Concatenate `n` copies of a string. The count is truncated toward zero.

- `s: string` — the string to repeat
- `n: number` — non-negative repeat count
- Returns: `string`

> [!TIER2] Panics if the count is negative.

```ascript
string.repeat("ab", 3)   // "ababab"
```

## std/array

Array operations. The callback-taking functions (`map`, `filter`, `reduce`, `sort`) invoke user functions you supply.

```ascript
import * as array from "std/array"
```

### array.map

Apply a function to every element, producing a new array.

- `arr: array` — the source array
- `f: function` — called as `f(item)`
- Returns: a new `array`

```ascript
array.map([1, 2, 3], fn(x) { x * 2 })   // [2, 4, 6]
```

### array.filter

Keep only the elements for which the predicate returns a truthy value.

- `arr: array` — the source array
- `f: function` — predicate called as `f(item)`
- Returns: a new `array`

```ascript
array.filter([1, 2, 3, 4], fn(x) { x > 2 })   // [3, 4]
```

### array.reduce

Fold an array into a single accumulated value, left to right.

- `arr: array` — the source array
- `f: function` — called as `f(acc, item)`
- `init` — the initial accumulator value
- Returns: the final accumulator

```ascript
array.reduce([1, 2, 3], fn(acc, x) { acc + x }, 0)   // 6
```

### array.push

Append an element to an array, mutating it in place. Returns the new length.

- `arr: array` — the array to mutate
- `item` — the value to append
- Returns: `number` — the new length

```ascript
let a = [1, 2]
array.push(a, 3)   // 3   (a is now [1, 2, 3])
```

### array.pop

Remove and return the last element, mutating the array in place. Returns `nil` if the array is empty.

- `arr: array` — the array to mutate
- Returns: the removed element, or `nil`

```ascript
array.pop([1, 2, 3])   // 3
```

### array.slice

Extract a subrange between two indices. Negative indices count from the end; the end argument defaults to the array length. If `start >= end`, the result is empty.

- `arr: array` — the source array
- `start: number` — start index (negative counts from end)
- `end: number` (optional) — end index, exclusive; defaults to length
- Returns: a new `array`

```ascript
array.slice([10, 20, 30, 40], 1, 3)   // [20, 30]
array.slice([10, 20, 30, 40], -2)     // [30, 40]
```

### array.sort

Return a new sorted array. Without a comparator, sorts a homogeneous array of numbers or strings in natural order. With a comparator, performs a stable sort: the comparator is called as `f(a, b)` and must return a number (negative if `a` should come before `b`).

- `arr: array` — the source array
- `cmp: function` (optional) — comparator `f(a, b) -> number`
- Returns: a new `array`

> [!TIER2] Without a comparator, panics on a mixed or non-number/non-string array. With a comparator, panics if the comparator returns a non-number.

```ascript
array.sort([3, 1, 2])                            // [1, 2, 3]
array.sort([3, 1, 2], fn(a, b) { b - a })        // [3, 2, 1]
```

### array.contains

Test whether an array contains a value, using structural equality.

- `arr: array` — the array to search
- `needle` — the value to look for
- Returns: `bool`

```ascript
array.contains([1, 2, 3], 2)   // true
```

### array.get

Read the element at an index. Returns `nil` for out-of-bounds, negative, or non-integer indices.

- `arr: array` — the source array
- `i: number` — the index
- Returns: the element, or `nil`

```ascript
array.get([10, 20], 0)    // 10
array.get([10, 20], 9)    // nil
```

## std/object

Operations on objects (string-keyed maps). Key iteration preserves insertion order.

```ascript
import * as object from "std/object"
```

### object.keys

Return an array of the object's keys, in insertion order.

- `o: object` — the source object
- Returns: `array` of `string`

```ascript
object.keys({a: 1, b: 2})   // ["a", "b"]
```

### object.values

Return an array of the object's values, in insertion order.

- `o: object` — the source object
- Returns: `array`

```ascript
object.values({a: 1, b: 2})   // [1, 2]
```

### object.entries

Return an array of `[key, value]` pairs, in insertion order.

- `o: object` — the source object
- Returns: `array` of `[string, value]` pairs

```ascript
object.entries({a: 1, b: 2})   // [["a", 1], ["b", 2]]
```

### object.has

Test whether the object contains a key.

- `o: object` — the source object
- `key: string` — the key to test
- Returns: `bool`

```ascript
object.has({a: 1}, "a")   // true
```

### object.delete

Remove a key, mutating the object in place. Preserves the order of the remaining keys. Returns whether the key existed.

- `o: object` — the object to mutate
- `key: string` — the key to remove
- Returns: `bool` — `true` if the key existed

```ascript
object.delete({a: 1}, "a")   // true
```

### object.merge

Merge any number of objects left to right into a **new** object; later keys overwrite earlier ones. The result is independent of the inputs. Zero arguments yields an empty object.

- `...objects: object` — the objects to merge
- Returns: a new `object`

```ascript
object.merge({a: 1, b: 2}, {b: 9, c: 3})   // {a: 1, b: 9, c: 3}
```

## std/map

The `Map` collection: insertion-ordered, with hashable keys (`nil`, `bool`, `number`, or `string`). Unlike objects, map keys are not restricted to strings.

```ascript
import * as map from "std/map"
```

### map.new

Create a new map. Optionally seed it from an array of `[key, value]` pairs.

- `seed: array` (optional) — array of `[key, value]` pairs
- Returns: a new `map`

> [!TIER2] Panics if the optional seed is not an array of two-element `[key, value]` pairs, or if any seed key is not hashable.

```ascript
map.new()                        // empty map
map.new([["a", 1], ["b", 2]])    // map seeded with two entries
```

### map.get

Read the value for a key. Returns `nil` if the key is absent.

- `m: map` — the source map
- `key` — a hashable key (`nil`, `bool`, `number`, or `string`)
- Returns: the value, or `nil`

> [!TIER2] Panics if the key is not hashable.

```ascript
let m = map.new([["a", 1]])
map.get(m, "a")   // 1
map.get(m, "z")   // nil
```

### map.set

Insert or update a key/value pair, mutating the map in place. Returns the map itself, so calls can be chained.

- `m: map` — the map to mutate
- `key` — a hashable key
- `value` — the value to store
- Returns: the `map`

> [!TIER2] Panics if the key is not hashable.

```ascript
let m = map.new()
map.set(m, "a", 1)   // returns m, now {"a": 1}
```

### map.has

Test whether the map contains a key.

- `m: map` — the source map
- `key` — a hashable key
- Returns: `bool`

```ascript
map.has(map.new([["a", 1]]), "a")   // true
```

### map.delete

Remove a key, mutating the map in place. Preserves the order of the remaining keys. Returns whether the key existed.

- `m: map` — the map to mutate
- `key` — a hashable key
- Returns: `bool` — `true` if the key existed

```ascript
map.delete(map.new([["a", 1]]), "a")   // true
```

### map.keys

Return an array of the map's keys, in insertion order.

- `m: map` — the source map
- Returns: `array`

```ascript
map.keys(map.new([["a", 1], ["b", 2]]))   // ["a", "b"]
```

### map.values

Return an array of the map's values, in insertion order.

- `m: map` — the source map
- Returns: `array`

```ascript
map.values(map.new([["a", 1], ["b", 2]]))   // [1, 2]
```

### map.entries

Return an array of `[key, value]` pairs, in insertion order.

- `m: map` — the source map
- Returns: `array` of `[key, value]` pairs

```ascript
map.entries(map.new([["a", 1]]))   // [["a", 1]]
```

## std/math

Numeric functions and constants. The module exposes two constants alongside its functions:

| Constant | Value |
| --- | --- |
| `math.pi` | π (3.14159…) |
| `math.e` | Euler's number (2.71828…) |

```ascript
import * as math from "std/math"
```

### math.abs

Absolute value.

- `x: number`
- Returns: `number`

```ascript
math.abs(-3)   // 3
```

### math.floor

Round down toward negative infinity.

- `x: number`
- Returns: `number`

```ascript
math.floor(2.9)   // 2
```

### math.ceil

Round up toward positive infinity.

- `x: number`
- Returns: `number`

```ascript
math.ceil(2.1)   // 3
```

### math.round

Round to the nearest integer (halves round away from zero).

- `x: number`
- Returns: `number`

```ascript
math.round(2.5)   // 3
```

### math.sqrt

Square root.

- `x: number`
- Returns: `number`

```ascript
math.sqrt(9)   // 3
```

### math.pow

Raise a base to an exponent.

- `base: number`
- `exp: number`
- Returns: `number`

```ascript
math.pow(2, 10)   // 1024
```

### math.min

Return the smallest of one or more arguments.

- `...nums: number` — at least one argument
- Returns: `number`

> [!TIER2] Panics if called with no arguments.

```ascript
math.min(1, 9, 4)   // 1
```

### math.max

Return the largest of one or more arguments.

- `...nums: number` — at least one argument
- Returns: `number`

> [!TIER2] Panics if called with no arguments.

```ascript
math.max(1, 9, 4)   // 9
```

### math.random

Return a pseudo-random number in the half-open range `[0, 1)`. The generator is fast but **not** cryptographic.

- Returns: `number` in `[0, 1)`

```ascript
math.random()   // e.g. 0.7421…
```

## std/convert

Parsing and coercions. The `parse*` functions return recoverable `[value, err]` pairs for bad input; the `to*` functions coerce or panic.

```ascript
import * as convert from "std/convert"
```

### convert.parseNumber

Parse a string as a floating-point number. Accepts scientific notation (`"1e3"`) and the IEEE-754 specials `"inf"`, `"-inf"`, and `"NaN"`. For untrusted input, prefer this over `toNumber`.

- `s: string` — the string to parse
- Returns: `[number, nil]` on success, or `[nil, error]` on failure

> [!TIER1] Returns `[value, err]` — destructure it.

```ascript
let [n, err] = convert.parseNumber("3.5")   // n = 3.5, err = nil
let [bad, e] = convert.parseNumber("abc")   // bad = nil, e is an error
```

### convert.parseInt

Parse a string as an integer in a given radix (2–36, default 10).

- `s: string` — the string to parse
- `radix: number` (optional) — base 2–36, defaults to 10
- Returns: `[number, nil]` on success, or `[nil, error]` on failure

> [!TIER1] Returns `[value, err]` — destructure it.

> [!TIER2] Panics if `radix` is outside the range 2–36.

```ascript
let [n, err] = convert.parseInt("ff", 16)   // n = 255, err = nil
let [m, e]   = convert.parseInt("42")       // m = 42,  e = nil
```

### convert.toString

Convert any value to its display string form.

- `v` — any value
- Returns: `string`

```ascript
convert.toString(7)        // "7"
convert.toString([1, 2])   // "[1, 2]"
```

### convert.toNumber

Coerce a value to a number. Numbers pass through; `true`/`false` become `1`/`0`; `nil` becomes `0`; strings are parsed. The contract is "this **is** a number-like value" — use `parseNumber` for untrusted input.

- `v` — a number, bool, nil, or numeric string
- Returns: `number`

> [!TIER2] Panics on a string that will not parse, or on any other non-coercible type (e.g. an array).

```ascript
convert.toNumber(true)    // 1
convert.toNumber(" 42 ")  // 42
```

### convert.toBool

Coerce any value to a boolean using AScript's truthiness rules.

- `v` — any value
- Returns: `bool`

```ascript
convert.toBool(0)     // true
convert.toBool(nil)   // false
```

## std/bytes

A mutable byte buffer with integer read/write and endian handling. Multi-byte integer operations take an endian argument — `"le"` (little-endian) or `"be"` (big-endian) — with `nil` defaulting to big-endian (network order):

| Endian | Meaning |
| --- | --- |
| `"le"` | little-endian |
| `"be"` | big-endian |
| `nil` (or omitted) | big-endian (network order) |

```ascript
import * as bytes from "std/bytes"
```

### bytes.alloc

Allocate a zero-filled byte buffer of a given length.

- `n: number` — non-negative integer length
- Returns: `bytes`

> [!TIER2] Panics if `n` is not a finite non-negative integer within range.

```ascript
bytes.alloc(3)   // bytes [0, 0, 0]
```

### bytes.fromArray

Build a byte buffer from an array of integers, each in `0..=255`.

- `arr: array` — array of integers `0..=255`
- Returns: `bytes`

> [!TIER2] Panics if any element is not an integer in `0..=255`.

```ascript
bytes.fromArray([1, 2, 3])   // bytes [1, 2, 3]
```

### bytes.toArray

Convert a byte buffer to an array of numbers.

- `b: bytes` — the buffer
- Returns: `array` of `number`

```ascript
bytes.toArray(bytes.alloc(3))   // [0, 0, 0]
```

### bytes.get

Read the byte at an index. Returns `nil` for out-of-bounds, negative, or non-integer indices.

- `b: bytes` — the buffer
- `i: number` — the index
- Returns: `number`, or `nil`

```ascript
let b = bytes.fromArray([10, 20])
bytes.get(b, 1)   // 20
bytes.get(b, 9)   // nil
```

### bytes.set

Write a single byte at an index, mutating the buffer in place.

- `b: bytes` — the buffer to mutate
- `i: number` — non-negative integer index
- `value: number` — byte value `0..=255`
- Returns: `nil`

> [!TIER2] Panics if `value` is out of `0..=255`, or if the index is out of bounds.

```ascript
let b = bytes.alloc(3)
bytes.set(b, 1, 255)   // b is now [0, 255, 0]
```

### bytes.slice

Extract a subrange of bytes. Negative indices count from the end; the end argument defaults to the length. If `start >= end`, the result is empty.

- `b: bytes` — the source buffer
- `start: number` — start index (negative counts from end)
- `end: number` (optional) — end index, exclusive; defaults to length
- Returns: a new `bytes`

```ascript
bytes.slice(bytes.fromArray([1, 2, 3, 4]), 1, 3)   // bytes [2, 3]
```

### bytes.concat

Concatenate any number of byte buffers into a new buffer.

- `...buffers: bytes` — the buffers to concatenate
- Returns: a new `bytes`

```ascript
bytes.concat(bytes.fromArray([1, 2]), bytes.fromArray([3]))   // bytes [1, 2, 3]
```

### bytes.readUint

Read an unsigned integer of `n` bytes from an offset, using the given endianness.

- `b: bytes` — the buffer
- `offset: number` — non-negative integer offset
- `n: number` — byte length, 1–8
- `endian: string` (optional) — `"le"` or `"be"`; `nil` defaults to big-endian
- Returns: `number`

> [!TIER2] Panics if `n` is outside 1–8, if `endian` is not `"le"`/`"be"`/`nil`, or if the read runs out of bounds.

```ascript
let b = bytes.fromArray([1, 2, 3, 4])
bytes.readUint(b, 0, 4, "be")   // 0x01020304 = 16909060
```

### bytes.writeUint

Write a non-negative integer of `n` bytes at an offset, using the given endianness. Mutates the buffer in place.

- `b: bytes` — the buffer to mutate
- `offset: number` — non-negative integer offset
- `value: number` — finite non-negative integer that fits in `n` bytes
- `n: number` — byte length, 1–8
- `endian: string` (optional) — `"le"` or `"be"`; `nil` defaults to big-endian
- Returns: `nil`

> [!TIER2] Panics if `n` is outside 1–8, if `value` is negative, non-finite, non-integer, or does not fit in `n` bytes, or if the write runs out of bounds.

```ascript
let b = bytes.alloc(4)
bytes.writeUint(b, 0, 16909060, 4, "be")   // b is now [1, 2, 3, 4]
```

### bytes.readInt

Read a signed integer of `n` bytes from an offset, using the given endianness. The value is sign-extended from the top bit of the `n`-byte field.

- `b: bytes` — the buffer
- `offset: number` — non-negative integer offset
- `n: number` — byte length, 1–8
- `endian: string` (optional) — `"le"` or `"be"`; `nil` defaults to big-endian
- Returns: `number`

> [!TIER2] Panics if `n` is outside 1–8, if `endian` is not `"le"`/`"be"`/`nil`, or if the read runs out of bounds.

```ascript
let b = bytes.fromArray([255, 255])
bytes.readInt(b, 0, 2, "be")   // -1
```

### bytes.writeInt

Write a signed integer of `n` bytes at an offset, using the given endianness. Mutates the buffer in place.

- `b: bytes` — the buffer to mutate
- `offset: number` — non-negative integer offset
- `value: number` — finite integer that fits in a signed `n`-byte field
- `n: number` — byte length, 1–8
- `endian: string` (optional) — `"le"` or `"be"`; `nil` defaults to big-endian
- Returns: `nil`

> [!TIER2] Panics if `n` is outside 1–8, if `value` is non-finite, non-integer, or out of range for a signed `n`-byte field, or if the write runs out of bounds.

```ascript
let b = bytes.alloc(2)
bytes.writeInt(b, 0, -1, 2, "be")   // b is now [255, 255]
```
