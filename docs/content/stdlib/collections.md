:::eyebrow Standard library

# Core & collections

The core and collection modules — `string`, `array`, `object`, `map`, `math`, `convert`, `bytes`, and `set` — are imported like any other standard module and called in **qualified form**. The collection or value you operate on is always the **first argument**:

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

### string.startsWith

Test whether a string begins with a given prefix.

- `s: string` — the source string
- `prefix: string` — the prefix to test
- Returns: `bool`

```ascript
string.startsWith("hello", "he")   // true
string.startsWith("hello", "lo")   // false
```

### string.endsWith

Test whether a string ends with a given suffix.

- `s: string` — the source string
- `suffix: string` — the suffix to test
- Returns: `bool`

```ascript
string.endsWith("hello", "lo")   // true
string.endsWith("hello", "he")   // false
```

### string.contains

Test whether a string contains a substring.

- `s: string` — the source string
- `sub: string` — the substring to search for
- Returns: `bool`

```ascript
string.contains("hello world", "lo wo")   // true
string.contains("hello world", "xyz")     // false
```

### string.chars

Split a string into an array of individual characters (Unicode scalar values).

- `s: string` — the source string
- Returns: `array` of single-character `string`

```ascript
string.chars("abc")   // ["a", "b", "c"]
```

### string.lines

Split a string into an array of lines. A trailing newline does not produce an extra empty element.

- `s: string` — the source string
- Returns: `array` of `string`

```ascript
string.lines("one\ntwo\nthree")   // ["one", "two", "three"]
```

### string.reverse

Return a string with its characters in reverse order.

- `s: string` — the source string
- Returns: `string`

```ascript
string.reverse("abc")   // "cba"
```

### string.count

Count the non-overlapping occurrences of a substring.

- `s: string` — the source string
- `sub: string` — the substring to count
- Returns: `number`

```ascript
string.count("banana", "a")   // 3
string.count("hello", "x")    // 0
```

### string.splitN

Split a string on a separator, returning at most `n` parts. The last part contains the remainder of the string unsplit.

- `s: string` — the source string
- `sep: string` — the separator
- `n: number` — maximum number of parts
- Returns: `array` of `string`

```ascript
string.splitN("a:b:c:d", ":", 2)   // ["a", "b:c:d"]
string.splitN("a:b:c:d", ":", 3)   // ["a", "b", "c:d"]
```

### string.codepoints

Return the string's Unicode scalar values as an `array<int>` (the Go "rune" model — AScript has no `char` type; a code point is just an `int`).

- `s: string` — the source string
- Returns: `array` of `int` (one per character)

```ascript
string.codepoints("Hi")   // [72, 105]
string.codepoints("é")    // [233]
```

### string.from_codepoints

Build a string from an array of Unicode scalar values (the inverse of `codepoints`). Each element must be an `int` (an integral `float` is accepted) in `0..=0x10FFFF` and **not** a surrogate (`0xD800..=0xDFFF`).

- `cps: array<int>` — the code points
- Returns: `string`

```ascript
string.from_codepoints([72, 105])   // "Hi"
string.from_codepoints([0x1F600])   // "😀"
```

> [!TIER2] Panics if any element is not an int or is not a valid Unicode scalar value.

### string.code_at

Return the Unicode scalar value (an `int`) at character index `i`.

- `s: string` — the source string
- `i: int` — a non-negative character index
- Returns: `int`

```ascript
string.code_at("ABC", 0)   // 65
string.code_at("ABC", 2)   // 67
```

> [!TIER2] Panics if the index is negative, not an int, or out of range.

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

### array.find

Return the first element for which the predicate returns truthy. Returns `nil` if nothing matches.

- `arr: array` — the source array
- `f: function` — predicate called as `f(item)`
- Returns: the matching element, or `nil`

```ascript
array.find([3, 1, 2, 4], x => x > 2)    // 3
array.find([1, 2, 3], x => x > 10)      // nil
```

### array.findIndex

Return the index of the first element for which the predicate returns truthy. Returns `-1` if nothing matches.

- `arr: array` — the source array
- `f: function` — predicate called as `f(item)`
- Returns: `number` — the index, or `-1`

```ascript
array.findIndex([3, 1, 2], x => x == 2)   // 2
array.findIndex([1, 2, 3], x => x > 10)   // -1
```

### array.some

Return `true` if the predicate returns truthy for at least one element.

- `arr: array` — the source array
- `f: function` — predicate called as `f(item)`
- Returns: `bool`

```ascript
array.some([1, 2, 3], x => x > 2)   // true
array.some([1, 2, 3], x => x > 9)   // false
```

### array.every

Return `true` if the predicate returns truthy for every element. Returns `true` for an empty array (vacuously true).

- `arr: array` — the source array
- `f: function` — predicate called as `f(item)`
- Returns: `bool`

```ascript
array.every([1, 2, 3], x => x > 0)   // true
array.every([1, 2, 3], x => x > 1)   // false
array.every([], x => false)           // true (vacuous)
```

### array.indexOf

Return the index of the first element equal to `needle` (structural equality). Returns `-1` if not found.

- `arr: array` — the source array
- `needle` — the value to search for
- Returns: `number` — the index, or `-1`

```ascript
array.indexOf([10, 20, 30], 20)   // 1
array.indexOf([10, 20, 30], 99)   // -1
```

### array.flat

Flatten nested arrays by `depth` levels (default 1).

- `arr: array` — the source array
- `depth: number` (optional) — how many levels to flatten; defaults to `1`
- Returns: a new `array`

```ascript
array.flat([[1], [2, 3], [4]])        // [1, 2, 3, 4]
array.flat([[1, [2, 3]], [4]], 2)     // [1, 2, 3, 4]
```

### array.flatMap

Apply `f` to every element and flatten the result one level, equivalent to `array.flat(array.map(arr, f))`.

- `arr: array` — the source array
- `f: function` — called as `f(item)`, must return an array
- Returns: a new `array`

```ascript
array.flatMap([1, 2, 3], x => [x, x * 10])   // [1, 10, 2, 20, 3, 30]
```

### array.reverse

Return a new array with the elements in reversed order. Does not mutate the original.

- `arr: array` — the source array
- Returns: a new `array`

```ascript
array.reverse([1, 2, 3])   // [3, 2, 1]
```

### array.concat

Concatenate any number of arrays into a single new array.

- `...arrays: array` — the arrays to concatenate
- Returns: a new `array`

```ascript
array.concat([1, 2], [3, 4], [5])   // [1, 2, 3, 4, 5]
```

### array.first

Return the first element of the array, or `nil` if the array is empty.

- `arr: array` — the source array
- Returns: the first element, or `nil`

```ascript
array.first([10, 20, 30])   // 10
array.first([])              // nil
```

### array.last

Return the last element of the array, or `nil` if the array is empty.

- `arr: array` — the source array
- Returns: the last element, or `nil`

```ascript
array.last([10, 20, 30])   // 30
array.last([])              // nil
```

### array.unique

Return a new array with duplicate elements removed, preserving the first occurrence order.

- `arr: array` — the source array
- Returns: a new `array`

```ascript
array.unique([3, 1, 2, 1, 4])   // [3, 1, 2, 4]
```

### array.take

Return the first `n` elements. If `n` exceeds the length, returns the whole array.

- `arr: array` — the source array
- `n: number` — number of elements to take
- Returns: a new `array`

```ascript
array.take([1, 2, 3, 4], 2)   // [1, 2]
array.take([1, 2], 10)         // [1, 2]
```

### array.drop

Return all elements after skipping the first `n`. If `n` exceeds the length, returns an empty array.

- `arr: array` — the source array
- `n: number` — number of elements to skip
- Returns: a new `array`

```ascript
array.drop([1, 2, 3, 4], 2)   // [3, 4]
```

### array.chunk

Split an array into consecutive chunks of size `size`. The last chunk may be smaller if the array does not divide evenly.

- `arr: array` — the source array
- `size: number` — chunk size (positive integer)
- Returns: `array` of `array`

> [!TIER2] Panics if `size` is not a positive integer.

```ascript
array.chunk([1, 2, 3, 4, 5], 2)   // [[1, 2], [3, 4], [5]]
```

### array.zip

Interleave two arrays element by element into an array of `[a, b]` pairs. Truncates to the shorter length.

- `a: array` — the first array
- `b: array` — the second array
- Returns: `array` of two-element `array` pairs

```ascript
array.zip([1, 2, 3], ["a", "b", "c"])   // [[1, "a"], [2, "b"], [3, "c"]]
array.zip([1, 2], ["a", "b", "c"])      // [[1, "a"], [2, "b"]]  (truncated)
```

### array.groupBy

Group elements by the return value of a key function. Returns a `map` whose keys are the distinct key values and whose values are arrays of matching elements.

- `arr: array` — the source array
- `f: function` — key function called as `f(item)`, must return a hashable value (`nil`, `bool`, `number`, or `string`)
- Returns: `map` — keys are the distinct key values; values are `array`

> [!TIER2] Panics if the key function returns a non-hashable value.

```ascript
import * as map from "std/map"
let groups = array.groupBy([1, 2, 3, 4, 5], x => x % 2)
map.get(groups, 1)   // [1, 3, 5]  (odd)
map.get(groups, 0)   // [2, 4]     (even)
```

### array.partition

Split an array into two arrays: elements that pass the predicate and elements that do not. Returns `[pass, fail]`.

- `arr: array` — the source array
- `f: function` — predicate called as `f(item)`
- Returns: `[array, array]` — `[pass, fail]`

```ascript
array.partition([1, 2, 3, 4, 5], x => x > 2)   // [[3, 4, 5], [1, 2]]
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

### object.fromEntries

Construct an object from an array of `[key, value]` pairs. Later pairs overwrite earlier ones for duplicate keys.

- `pairs: array` — array of `[string, value]` pairs
- Returns: a new `object`

> [!TIER2] Panics if any pair is not a two-element array, or if any key is not a string.

```ascript
object.fromEntries([["a", 1], ["b", 2]])   // {a: 1, b: 2}
```

### object.pick

Return a new object containing only the specified keys.

- `o: object | instance` — the source object or class instance (instance → its fields)
- `keys: array` — array of `string` keys to keep
- Returns: a new `object`

```ascript
object.pick({a: 1, b: 2, c: 3}, ["a", "c"])   // {a: 1, c: 3}

class Point { x: number = 0; y: number = 0; z: number = 0 }
object.pick(Point(), ["x", "z"])               // {x: 0, z: 0}
```

### object.omit

Return a new object with the specified keys removed.

- `o: object | instance` — the source object or class instance (instance → its fields)
- `keys: array` — array of `string` keys to remove
- Returns: a new `object`

```ascript
object.omit({a: 1, b: 2, c: 3}, ["b"])   // {a: 1, c: 3}

class Point { x: number = 0; y: number = 0; z: number = 0 }
object.omit(Point(), ["z"])               // {x: 0, y: 0}
```

### object.mapValues

Return a new object with each value transformed by `f`. The callback receives both the value and the key.

- `o: object | instance` — the source object or class instance (instance → its fields)
- `f: function` — called as `f(value, key)`, returns the new value
- Returns: a new `object`

```ascript
object.mapValues({a: 1, b: 2}, (v, k) => v * 10)   // {a: 10, b: 20}
object.mapValues({x: 1}, (v, k) => k)               // {x: "x"}

class Coords { lat: number = 0; lng: number = 0 }
object.mapValues(Coords(), (v, k) => "${k}=${v}")   // {lat: "lat=0", lng: "lng=0"}
```

### object.deepClone

Recursively clone an object (and any nested objects, arrays, or maps) into a fully independent copy.

- `o: object` — the source object
- Returns: a new deep copy

```ascript
let orig = {a: 1, b: {c: [1, 2]}}
let copy = object.deepClone(orig)
copy.b.c[0] = 99   // does not affect orig
```

### object.deepEqual

Recursively compare two values for structural equality. Two values are deeply equal if all nested structures and primitive values are equal.

- `a` — first value
- `b` — second value
- Returns: `bool`

```ascript
object.deepEqual({a: 1, b: [1, 2]}, {a: 1, b: [1, 2]})   // true
object.deepEqual({a: 1}, {a: 2})                           // false
```

### object.freeze

Shallow-freeze a mutable container (object, array, map, set, or class instance) **in place** and return it (so calls chain). After freezing, any in-place mutation — field/index assignment (`o.k = …`, `a[i] = …`), `array.push`/`pop`, `map.set`/`delete`, `set.add`/`delete`, etc. — is a runtime panic: `cannot mutate a frozen <kind>`. Freezing is **shallow** (a nested container stays mutable), **one-way** (there is no `unfreeze`), and **idempotent**. Freezing a non-container value is a no-op that returns it unchanged. A `deepClone` of a frozen value is a fresh, **unfrozen** copy.

- `x` — any value
- Returns: `x` (the same value, for chaining)

```ascript
let config = object.freeze({host: "localhost", port: 8080})
object.isFrozen(config)   // true
config.port = 9090        // panic: cannot mutate a frozen object

let outer = object.freeze([[1, 2]])
outer[0][0] = 99          // OK — freeze is shallow
```

### object.isFrozen

Whether the value is a frozen container. Returns `false` for any non-container value.

- `x` — any value
- Returns: `bool`

```ascript
object.isFrozen({a: 1})                 // false
object.isFrozen(object.freeze({a: 1}))  // true
object.isFrozen(42)                     // false
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

> **Deterministic mode (SP9).** Inside a `std/workflow` run (or under deterministic mode),
> `math.random`/`randomInt`/`shuffle`/`sample`, `uuid.v4`, and `crypto.randomBytes` draw from a
> per-`Interp` **seeded** PRNG that is recorded and replayed, so two same-seed runs are
> byte-identical. Outside deterministic mode the generator is the normal time-seeded one (no
> behavior change). See [Workflows](workflow).

### math.sin

Sine of an angle in radians.

- `x: number` — angle in radians
- Returns: `number`

```ascript
math.sin(0)          // 0
math.sin(math.pi)    // ≈ 0 (floating-point rounding)
```

### math.cos

Cosine of an angle in radians.

- `x: number` — angle in radians
- Returns: `number`

```ascript
math.cos(0)   // 1
```

### math.tan

Tangent of an angle in radians.

- `x: number` — angle in radians
- Returns: `number`

```ascript
math.tan(0)   // 0
```

### math.asin

Arc-sine (inverse sine). Returns a value in `[-π/2, π/2]`.

- `x: number` — value in `[-1, 1]`
- Returns: `number` — angle in radians

```ascript
math.asin(0)   // 0
math.asin(1)   // π/2 ≈ 1.5708
```

### math.acos

Arc-cosine (inverse cosine). Returns a value in `[0, π]`.

- `x: number` — value in `[-1, 1]`
- Returns: `number` — angle in radians

```ascript
math.acos(1)   // 0
math.acos(0)   // π/2 ≈ 1.5708
```

### math.atan

Arc-tangent (inverse tangent). Returns a value in `(-π/2, π/2)`.

- `x: number`
- Returns: `number` — angle in radians

```ascript
math.atan(0)   // 0
math.atan(1)   // π/4 ≈ 0.7854
```

### math.atan2

Two-argument arc-tangent. Returns the angle in radians between the positive x-axis and the point `(x, y)`, in `(-π, π]`.

- `y: number`
- `x: number`
- Returns: `number` — angle in radians

```ascript
math.atan2(1, 1)    // π/4 ≈ 0.7854
math.atan2(0, -1)   // π ≈ 3.1416
```

### math.exp

Euler's number raised to the power `x` (eˣ).

- `x: number`
- Returns: `number`

```ascript
math.exp(0)   // 1
math.exp(1)   // e ≈ 2.7183
```

### math.ln

Natural logarithm (base e).

- `x: number` — positive value
- Returns: `number`

```ascript
math.ln(1)          // 0
math.ln(math.e)     // 1
```

### math.log2

Base-2 logarithm.

- `x: number` — positive value
- Returns: `number`

```ascript
math.log2(8)    // 3
math.log2(1)    // 0
```

### math.log10

Base-10 logarithm.

- `x: number` — positive value
- Returns: `number`

```ascript
math.log10(1000)   // 3
math.log10(1)      // 0
```

### math.sign

Return `-1`, `0`, or `1` depending on the sign of `x`.

- `x: number`
- Returns: `number`

```ascript
math.sign(-5)   // -1
math.sign(0)    // 0
math.sign(3)    // 1
```

### math.trunc

Truncate toward zero (drop the fractional part).

- `x: number`
- Returns: `number`

```ascript
math.trunc(3.9)    // 3
math.trunc(-3.9)   // -3
```

### math.clamp

Clamp `x` to the closed interval `[lo, hi]`.

- `x: number`
- `lo: number` — lower bound
- `hi: number` — upper bound
- Returns: `number`

```ascript
math.clamp(5, 0, 3)    // 3  (above hi)
math.clamp(-1, 0, 3)   // 0  (below lo)
math.clamp(2, 0, 3)    // 2  (in range)
```

### math.hypot

Euclidean distance — square root of the sum of squares. Numerically stable for large values.

- `a: number`
- `b: number`
- Returns: `number`

```ascript
math.hypot(3, 4)   // 5
```

### math.gcd

Greatest common divisor of two non-negative integers.

- `a: number` — non-negative integer
- `b: number` — non-negative integer
- Returns: `number`

> [!TIER2] Panics if either argument is not a finite integer.

```ascript
math.gcd(12, 8)   // 4
math.gcd(7, 0)    // 7
```

### math.lcm

Least common multiple of two non-negative integers.

- `a: number` — non-negative integer
- `b: number` — non-negative integer
- Returns: `number`

> [!TIER2] Panics if either argument is not a finite integer.

```ascript
math.lcm(4, 6)    // 12
math.lcm(5, 0)    // 0
```

### math.sum

Sum all elements of a numeric array. Returns `0` for an empty array.

- `arr: array` — array of `number`
- Returns: `number`

> [!TIER2] Panics if any element is not a number.

```ascript
math.sum([1, 2, 3, 4])   // 10
math.sum([])              // 0
```

### math.mean

Arithmetic mean of a numeric array.

- `arr: array` — non-empty array of `number`
- Returns: `number`

> [!TIER2] Panics on an empty array or non-number elements.

```ascript
math.mean([1, 2, 3, 4])   // 2.5
```

### math.median

Median of a numeric array. For even-length arrays returns the mean of the two middle values.

- `arr: array` — non-empty array of `number`
- Returns: `number`

> [!TIER2] Panics on an empty array or non-number elements.

```ascript
math.median([3, 1, 2])      // 2
math.median([1, 2, 3, 4])   // 2.5
```

### math.variance

Population or sample variance of a numeric array. Pass `true` as the second argument for sample variance (Bessel's correction, denominator `n-1`); omit or pass a falsy value for population variance (denominator `n`).

- `arr: array` — array of `number`
- `sample: bool` (optional) — use sample variance; defaults to `false` (population)
- Returns: `number`

> [!TIER2] Panics on an empty array; panics for sample variance if the array has fewer than two elements.

```ascript
math.variance([2, 4, 4, 4, 5, 5, 7, 9])        // 4  (population)
math.variance([2, 4, 4, 4, 5, 5, 7, 9], true)   // 4.571…  (sample)
```

### math.stddev

Population or sample standard deviation. Same signature as `math.variance`; returns the square root of the variance.

- `arr: array` — array of `number`
- `sample: bool` (optional) — use sample stddev; defaults to `false` (population)
- Returns: `number`

> [!TIER2] Panics on an empty array; panics for sample stddev if the array has fewer than two elements.

```ascript
math.stddev([2, 4, 4, 4, 5, 5, 7, 9])   // 2  (population)
```

### math.randomInt

Return a uniformly distributed random integer in the **inclusive** range `[min, max]`.

- `min: number` — minimum value (integer)
- `max: number` — maximum value (integer, must be ≥ `min`)
- Returns: `number`

> [!TIER2] Panics if `min > max` or if either argument is not a finite integer.

```ascript
math.randomInt(1, 6)   // e.g. 4  (like rolling a die)
math.randomInt(5, 5)   // always 5
```

### math.shuffle

Return a new array with the elements in a random order (Fisher-Yates). Does not mutate the original.

- `arr: array` — the source array
- Returns: a new `array`

```ascript
math.shuffle([1, 2, 3, 4, 5])   // e.g. [3, 1, 5, 2, 4]
```

### math.choice

Return a uniformly random element from a non-empty array. Returns `nil` for an empty array.

- `arr: array` — the source array
- Returns: a random element, or `nil`

```ascript
math.choice(["rock", "paper", "scissors"])   // e.g. "paper"
```

> **Tip — `min`/`max` with arrays:** `math.min` and `math.max` are variadic (positional arguments), not array-taking. To find the min or max of an array use spread: `math.min(...arr)`, `math.max(...arr)`.

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

## std/set

An insertion-ordered hash set of **hashable** values (`nil`, `bool`, `number`, or `string`). Like `std/map`, there is no constructor syntax — the module functions are the only entry points. All operations are module-qualified.

> [!NOTE] Set elements must be hashable. Attempting to add an array, object, map, or other non-hashable value **panics (Tier-2)**.

```ascript
import * as set from "std/set"
```

### set.new

Create an empty set.

- Returns: a new `set`

```ascript
set.new()   // set {}
```

### set.from

Build a set from an array, deduplicating elements. Preserves the first occurrence order.

- `arr: array` — the source array; each element must be hashable
- Returns: a new `set`

> [!TIER2] Panics if `arr` is not an array or if any element is not hashable.

```ascript
import * as set from "std/set"

let s = set.from([1, 1, 2, 3])   // set {1, 2, 3}  — deduped, size 3
```

### set.add

Insert a value into the set. If the value is already present, this is a no-op. Returns the set itself for chaining.

- `s: set` — the set to mutate
- `value` — a hashable value
- Returns: `s`

> [!TIER2] Panics if `value` is not hashable.

```ascript
set.add(s, 42)   // returns s; s now contains 42
```

### set.has

Test whether a value is in the set.

- `s: set` — the set to query
- `value` — a hashable value
- Returns: `bool`

> [!TIER2] Panics if `value` is not hashable.

```ascript
set.has(set.from([1, 2, 3]), 2)   // true
set.has(set.from([1, 2, 3]), 9)   // false
```

### set.delete

Remove a value from the set, mutating it in place. Returns whether the value existed.

- `s: set` — the set to mutate
- `value` — a hashable value
- Returns: `bool` — `true` if the value existed and was removed

> [!TIER2] Panics if `value` is not hashable.

```ascript
let s = set.from([1, 2, 3])
set.delete(s, 2)   // true  (removed)
set.delete(s, 9)   // false (not present)
```

### set.size

Return the number of elements in the set. The built-in `len(s)` function also works on sets.

- `s: set` — the source set
- Returns: `number`

```ascript
set.size(set.from([1, 2, 3]))   // 3
len(set.from([1, 2, 3]))        // 3  (len works too)
```

### set.values

Return an array of the set's elements, in insertion order.

- `s: set` — the source set
- Returns: `array`

```ascript
let s = set.from(["c", "a", "b"])
set.values(s)   // ["c", "a", "b"]  — insertion order preserved
```

### set.union

Return a **new** set containing all elements from `a` and all elements from `b` not already in `a`. Preserves `a`'s element order first, then `b`'s new elements.

- `a: set` — first operand
- `b: set` — second operand
- Returns: a new `set`

```ascript
let a = set.from([1, 2, 3])
let b = set.from([2, 3, 4])
set.union(a, b)   // set {1, 2, 3, 4}
```

### set.intersection

Return a **new** set of elements that appear in **both** `a` and `b`. Preserves `a`'s element order.

- `a: set` — first operand
- `b: set` — second operand
- Returns: a new `set`

```ascript
let a = set.from([1, 2, 3])
let b = set.from([2, 3, 4])
set.intersection(a, b)   // set {2, 3}
```

### set.difference

Return a **new** set of elements that are in `a` but **not** in `b` (set subtraction: `a − b`). Preserves `a`'s element order.

- `a: set` — first operand
- `b: set` — second operand
- Returns: a new `set`

```ascript
let a = set.from([1, 2, 3])
let b = set.from([2, 3, 4])
set.difference(a, b)   // set {1}
```
