:::eyebrow Standard library

# std/stream — lazy pull streams

`std/stream` is a **lazy, pull-based** stream library. Nothing happens until a
*terminal* drives the stream; every combinator is O(1) — it stores one extra
stage descriptor and returns a new stream handle.

```ascript
import * as stream from "std/stream"
```

## The laziness guarantee

Sources, combinators, and terminals interact like this:

```
source  →  [stage₁]  →  [stage₂]  →  …  →  terminal
```

A terminal calls `pull_next` on the chain. Each `pull_next` pulls one raw item
from the source and threads it through every stage in order. Only items the
terminal actually requests are ever produced. This means:

```ascript
// Despite a 1 000 000-element source, only 9 items are ever read from it
// (filter reads items 0..8 to find the 5 even ones; map runs exactly 5 times).
let s = stream.take(
  stream.map(stream.filter(stream.range(0, 1000000), x => x % 2 == 0), x => x * 3),
  5
)
let result = await stream.collect(s)   // [0, 6, 12, 18, 24]
```

## Single consumption

A stream is consumed by exactly one terminal. Internally the pull engine
advances a cursor / decrements counters in-place; after a `collect` (or any
other terminal) the cursor is past the end. Re-running a terminal on a drained
stream yields nothing:

```ascript
let s = stream.from([1, 2, 3])
await stream.collect(s)   // [1, 2, 3]
await stream.collect(s)   // [] — already drained
```

Branch before consuming if you need the same data in two places:
`stream.zip(stream.from(arr), stream.from(arr))`, or keep the source array and
build two `stream.from(arr)` streams.

---

## Sources

### `stream.from(source)`

Create a stream from an **array** (index-pull) or a **generator** (resume-pull).

```ascript
stream.from([1, 2, 3])

fn* nats() { let i = 0; while (true) { yield i; i = i + 1 } }
stream.from(nats())       // infinite lazy source from a generator
```

### `stream.range(start, end, step?)`

A numeric range `[start, end)` (`end` exclusive), following the same unified
range model as the `..` syntax:

- **Omitted `step`** — the direction is **inferred from the bounds**:
  `start < end` counts up by `+1`, `start > end` counts **down** by `-1`.
  So `stream.range(10, 1)` yields `10, 9, …, 2`.
- **Explicit `step`** — the step's **sign is honored**, but a sign that
  **disagrees** with the bounds direction is a mismatch and **panics** (it is
  *not* a silent empty range): `stream.range(1, 10, -2)` panics.
- `step` must be **finite and non-zero**; `step 0` / `±Infinity` / `NaN` panic.

```ascript
stream.range(0, 5)         // 0, 1, 2, 3, 4
stream.range(0, 10, 2)     // 0, 2, 4, 6, 8
stream.range(10, 0, -3)    // 10, 7, 4, 1
stream.range(10, 1)        // 10, 9, 8, 7, 6, 5, 4, 3, 2  (direction inferred)
stream.range(1, 10, -2)    // panic: step -2 moves away from end (10); range can never progress
```

---

## Lazy combinators

Every combinator takes an existing stream as its **first argument** and returns
a new stream. No items are pulled until a terminal runs.

### `stream.map(s, f)`

Apply `fn(value) -> value` to every item.

```ascript
await stream.collect(stream.map(stream.from([1, 2, 3]), x => x * 10))
// [10, 20, 30]
```

### `stream.filter(s, f)`

Keep only items where `fn(value)` is truthy.

```ascript
await stream.collect(stream.filter(stream.from([1, 2, 3, 4]), x => x % 2 == 0))
// [2, 4]
```

### `stream.take(s, n)`

Stop after at most `n` items. Short-circuits the source — no further items are
pulled once `n` is reached.

```ascript
await stream.collect(stream.take(stream.range(0, 1000000), 3))
// [0, 1, 2]
```

### `stream.drop(s, n)`

Skip the first `n` items, then pass through the rest.

```ascript
await stream.collect(stream.drop(stream.from([1, 2, 3, 4, 5]), 2))
// [3, 4, 5]
```

### `stream.flatMap(s, f)`

Call `fn(value)` for each item. `fn` must return an **array**; those elements
replace the original item in the output (one level of flattening).

```ascript
await stream.collect(stream.flatMap(stream.from([1, 2, 3]), x => [x, x * 10]))
// [1, 10, 2, 20, 3, 30]
```

### `stream.enumerate(s)`

Wrap each item as `[index, value]`, with indices starting at 0.

```ascript
await stream.collect(stream.enumerate(stream.from(["a", "b", "c"])))
// [[0, "a"], [1, "b"], [2, "c"]]
```

### `stream.zip(s, t)`

Pair items from `s` and `t` as `[a, b]`. The stream ends when **either side**
runs out. Both `s` and `t` are consumed lazily.

```ascript
await stream.collect(
  stream.zip(stream.from([1, 2, 3]), stream.from(["x", "y"]))
)
// [[1, "x"], [2, "y"]]  — stops when the shorter side ends
```

> [!NOTE] `stream.zip(s, s)` is rejected at build time with a Tier-2 panic
> (`stream.zip cannot zip a stream with itself`). Use two separate `from`
> calls if you want to zip equal arrays.

---

## Terminals

Terminals drive the pull. They are all `async` and return a plain value
(awaited with `await`).

### `stream.collect(s) -> array`

Drain the stream into an array.

```ascript
let items = await stream.collect(stream.range(1, 4))   // [1, 2, 3]
```

### `stream.forEach(s, f) -> nil`

Pull every item and call `fn(value)` for its side effect. Returns `nil`.

```ascript
await stream.forEach(stream.from([1, 2, 3]), x => print(x))
```

### `stream.reduce(s, f, init) -> value`

Fold with `fn(acc, value) -> acc`, starting from `init`.

```ascript
await stream.reduce(stream.from([1, 2, 3, 4]), (a, b) => a + b, 0)   // 10
```

### `stream.count(s) -> number`

Number of items the stream produces (drains the whole stream).

```ascript
await stream.count(stream.filter(stream.from([1, 2, 3, 4, 5]), x => x > 2))   // 3
```

### `stream.find(s, f) -> value | nil`

Return the **first** item where `fn(value)` is truthy. Short-circuits —
no further items are pulled after the match.

```ascript
await stream.find(stream.range(0, 1000000), x => x == 42)   // 42  (only pulls 43 items)
```

### `stream.first(s) -> value | nil`

Return the first item, or `nil` if the stream is empty. Pulls exactly one item.

```ascript
await stream.first(stream.from([10, 20, 30]))   // 10
await stream.first(stream.from([]))              // nil
```
