:::eyebrow Standard library

# std/assert — test assertions

`std/assert` provides **rich, composable assertion helpers** for use inside
`test(name, fn)` bodies and any other code that should fail loudly when an
invariant is violated.

```ascript
import * as assert from "std/assert"
```

> [!NOTE] `std/assert` is distinct from the **global `assert(cond, msg?)`
> builtin**, which is always available without an import. The global builtin
> takes a single truthy/falsy condition; `std/assert` adds deep equality,
> comparisons, container membership, approximate equality, and panic capture.

Every assertion **passes silently** on success and raises a **Tier-2 panic**
(`Control::Panic`) with a descriptive, value-showing message on failure. Inside
a `test()` body the runner catches the panic and reports it as a test failure.

## Deep equality

`assert.eq` and `assert.ne` use **structural deep equality**, not reference
equality. Two arrays `[1, 2]` and `[1, 2]` are equal even though they are
distinct heap objects:

```ascript
assert.eq([1, [2, 3]], [1, [2, 3]])   // ✅  passes
assert.eq([1, [2, 3]], [1, [2, 4]])   // ❌  panics — deep mismatch
```

This is equivalent to JSON equality: `{a: 1}` equals `{a: 1}` regardless of
when or how the object was created.

---

## Equality assertions

### `assert.eq(a, b, msg?)`

Fail if `a` and `b` are not deeply equal. Optional `msg` is prepended to the
error message.

```ascript
assert.eq(1 + 1, 2)
assert.eq({ x: [1, 2] }, { x: [1, 2] })
assert.eq("hello", "hello", "greeting should match")
```

On a container mismatch the failure message includes a **structural diff** (see
[below](#structural-diff)) instead of a flat `expected X got Y` dump.

### `assert.deepEq(a, b, msg?)`

An explicit alias for `assert.eq` — identical semantics (deep structural
equality with the same structural-diff failure message). Use it where you want
the deep-equality intent spelled out at the call site.

```ascript
assert.deepEq([1, { a: 2 }], [1, { a: 2 }])
```

### `assert.ne(a, b, msg?)`

Fail if `a` and `b` ARE deeply equal.

```ascript
assert.ne(1, 2)
assert.ne([1], [2])
```

---

## Boolean / nil assertions

### `assert.isTrue(value)`

Fail if `x` is falsy (`false` or `nil`).

### `assert.isFalse(value)`

Fail if `x` is truthy (anything other than `false` or `nil`).

### `assert.isNil(value)`

Fail if `x` is not `nil`.

### `assert.notNil(value)`

Fail if `x` is `nil`. Useful after a function that should have returned a value.

```ascript
assert.isTrue(len([1, 2]) == 2)
assert.isFalse(1 > 10)
assert.isNil(nil)
assert.notNil("something")
```

---

## Comparison assertions

All four take two numbers (or `Decimal` values).

### `assert.gt(a, b)` — fail if `a <= b`
### `assert.gte(a, b)` — fail if `a < b`
### `assert.lt(a, b)` — fail if `a >= b`
### `assert.lte(a, b)` — fail if `a > b`

```ascript
assert.gt(10, 5)
assert.gte(5, 5)
assert.lt(3, 7)
assert.lte(7, 7)
```

---

## Container membership

### `assert.contains(container, item)`

| Haystack type | Needle type | Check |
|---|---|---|
| `string` | `string` | substring search |
| `array` | any value | membership (deep equal) |
| `object` | `string` | key presence |
| `map` | any hashable value | key presence |

```ascript
assert.contains("hello world", "world")
assert.contains([1, 2, 3], 2)
assert.contains({ name: "Ada" }, "name")
```

### `assert.matches(value, pattern)`

Fail unless the string `value` matches `regex`. The pattern may be a compiled
`regex` value or a pattern string (compiled on the fly, like `regex.test`). A
non-string `value`, an invalid pattern, or a non-match each fail with a clear
message showing the value and the pattern.

```ascript
import * as regex from "std/regex"

assert.matches("hello123", "[a-z]+[0-9]+")     // string pattern
assert.matches("42", regex.compile("^\\d+$")[0])  // compiled regex
```

---

## Approximate equality

### `assert.approxEq(a, b, epsilon?)`

Fail if `|a - b| > epsilon`. Default `epsilon` is `1e-9`. Useful for
floating-point results where exact equality is unreliable.

```ascript
assert.approxEq(0.1 + 0.2, 0.3)          // passes (1e-9 epsilon)
assert.approxEq(1.0, 1.05, 0.1)           // passes (custom epsilon 0.1)
assert.approxEq(1.0, 2.0)                 // ❌  fails
```

---

## Capturing a panic

### `assert.throws(f) -> errValue`

Call `fn` with no arguments. If `fn` (or any async fn it returns) panics, the
error is **caught and returned** as an object `{ message }` — the same shape
`recover` returns. If `fn` completes without panicking, `assert.throws` itself
panics.

```ascript
let e = assert.throws(() => assert.eq(1, 99))
assert.contains(e.message, "assert.eq failed")
```

`assert.throws` drives any returned `future<T>` to completion before checking
for a panic, so it works with async functions too:

```ascript
async fn risky() { let _ = [][0] }  // out-of-bounds panic
let e = await assert.throws(risky)
assert.notNil(e.message)
```

### `assert.throwsWith(f, msg) -> errValue`

Like `assert.throws`, but **also** asserts the recovered error message contains
`substr`. A throw whose message does NOT contain `substr` fails (showing the
actual message); a `fn` that does not throw fails. Drives a returned
`future<T>` to completion exactly like `assert.throws`.

```ascript
let e = assert.throwsWith(() => assert.eq(1, 99), "assert.eq failed")
assert.contains(e.message, "1 != 99")
```

---

## Structural diff

When `assert.eq` / `assert.deepEq` or `assert.snapshot` find a **container**
mismatch, the failure message includes a path-qualified structural diff
computed over the same deep-equality traversal — so "equal per `assert.eq`"
exactly means "empty diff". The diff lists, recursively:

- `<path>: <old> → <new>` — a changed value (object key or array index),
- `+ <path>: <new>` — a key/index present only in the actual value,
- `- <path>: <old>` — a key/index present only in the expected value,

with `.key` for object keys and `[i]` for array indices, e.g.
`.users[0].name: a → b`. Object keys are reported in insertion order
(deterministic). The diff is depth-bounded and cycle-safe, so a deeply nested or
self-referential structure never overflows the stack.

```text
assert.eq failed: ... != ...
diff (expected → actual):
.users[0].name: a → b
+ .extra: 1
- .gone: 2
```

---

## Snapshot testing

### `assert.snapshot(name, value)`

*(Requires features `sys` + `data`.)*

Compare `value` (serialized to stable pretty-JSON) against a stored snapshot
file in `__snapshots__/<name>.snap`. On the **first run** the file is written
and the assertion passes; subsequent runs compare against the stored content.

```ascript
assert.snapshot("user_shape", { id: 1, name: "Ada", role: "guest" })
```

**Updating snapshots:** set the environment variable
`ASCRIPT_UPDATE_SNAPSHOTS=1` (any non-empty string) before running — the stored
files are overwritten and the run passes.

> [!NOTE] `assert.snapshot` is not exported under `--no-default-features`
> (the `sys` and `data` features are both required). Under a stripped build the
> `snapshot` is simply absent from the module's exports (so calling it is a "value is not callable" Tier-2 panic).

**Failure diff.** On a mismatch the message leads with the path-qualified
[structural diff](#structural-diff) (best for shape changes) and then shows a
text diff of the stored vs new payload. When the `diff` feature is enabled
(default-on), that text section is a **unified `@@` hunk**
(`diff.unified(stored, new, {fromFile: "stored", toFile: "new"})`, truncated past
200 lines with a `… N more lines` tail) — the same patch format `git diff` and
`patch` speak, so a snapshot drift reads like a code review. Without the feature
the older raw `--- stored --- / --- new ---` dump is used instead; the behavior is
identical either way, only the message text improves. See
[`std/diff`](utilities#stddiff).

---

## Using std/assert inside test() bodies

```ascript
import * as assert from "std/assert"

test("deep array equality", () => {
  assert.eq([1, 2, 3], [1, 2, 3])
  assert.ne([1, 2], [1, 3])
})

test("stream result shape", () => {
  let e = assert.throws(() => assert.eq([1], [2]))
  assert.contains(e.message, "assert.eq failed")
})
```

Run with `ascript test file.as`. See [the CLI docs](../cli) for details on the
test runner.

---

## Deterministic test runs

`ascript test` can run **deterministically** so that a failure replays exactly.
Two independent, composable flags:

| Flag | Effect |
|---|---|
| `--seed <U64>` | Each test body gets a **fresh, identical** RNG stream — `math.random*`, `uuid.v4`, and `crypto.randomBytes`/salts all draw from the same seeded sequence every run, independent of test order or `--filter`. |
| `--frozen-time <RFC3339\|EPOCH_MS>` | Freezes the virtual clock for test bodies — `time.now`/`time.monotonic`/`date.now` return the fixed instant and `time.sleep` returns instantly. Accepts an RFC3339 timestamp (e.g. `2026-01-02T03:04:05Z`, needs the `datetime` feature) or a raw epoch-ms integer (every build). A malformed value is a clean error. |

Both are optional and usable together. `--seed` alone also freezes time at the
seed-derived deterministic epoch; `--frozen-time` alone implies seed `0`. With
**neither** flag the runner is byte-identical to the pre-deterministic default
(the inert discipline — nothing changes).

```text
ascript test billing_test.as --seed 42
ascript test billing_test.as --seed 42 --frozen-time 2026-01-02T03:04:05Z
ascript test billing_test.as --frozen-time 1672531200000
```

> [!NOTE] **Only test bodies are deterministic.** Module **top-level** load runs
> on the real clock and RNG — a module-level `let now = time.now()` or
> `let id = uuid.v4()` is **not** frozen/seeded (freezing load-time would change
> module constants in surprising ways). Move any clock/RNG you want pinned
> **inside** the `test()` / `prop()` body.

---

## Property testing (std/test)

`std/test` is a **core** module (available even in a minimal build) for
**property-based testing**: instead of asserting on a handful of literals, you
state a property that should hold for *all* inputs and let the runner check it
across many edge-biased random draws, shrinking any failure to a minimal
counterexample.

```ascript
import { prop, gen } from "std/test"
```

### Generators

A **generator** is an inert tagged object (`{__gen: "int", ...}` — no new value
kind) that you compose with combinators. Generators do nothing until the runner
**draws** from them with a deterministic, edge-biased sampler — the same
boundary-favouring philosophy as the internal fuzzer, so corner cases (`min`,
`max`, `0`, `±1`, empty/single collections, unicode boundaries) are hit far more
often than uniform sampling would.

| Combinator | Produces |
|---|---|
| `gen.int(min?, max?)` | an integer in `[min, max]`, biased toward boundaries (`0`, `±1`, `min`, `max`, `±2^53`, i64 bounds) |
| `gen.float(min?, max?)` | a float in `[min, max]` with the same boundary bias |
| `gen.bool()` | `true` / `false` |
| `gen.string(opts?)` | a string; `opts = {minLen?, maxLen?, charset?}` where `charset` is `"ascii"` (default), `"alpha"`, `"alphanumeric"`, `"digit"`, `"unicode"`, or a literal set of characters |
| `gen.constant(v)` | always `v` |
| `gen.oneOf(...gens\|values)` | one of the given generators-or-values (or one element of a single array argument) |
| `gen.frequency(pairs)` | a weighted choice over an array of `[weight, generator]` pairs |
| `gen.arrayOf(g, opts?)` | an array whose elements come from `g`; `opts = {minLen? 0, maxLen? 32}` |
| `gen.objectWith({k: g, ...})` | a fixed-shape object, drawing each field from its own generator |
| `gen.map(g, fn)` | applies `fn` to each value drawn from `g` |
| `gen.filter(g, pred, opts?)` | redraws from `g` until `pred` holds; `opts = {maxDiscard? 100}` (exhausting it is a Tier-1 error) |
| `gen.nilOr(g)` | `nil` or a value from `g` |

```ascript
let smallInt = gen.int(0, 100)
let names = gen.arrayOf(gen.string({ minLen: 1, maxLen: 8, charset: "alpha" }))
let users = gen.objectWith({ id: gen.int(1, 999), active: gen.bool() })
```

Generators nest freely (`gen.arrayOf(gen.objectWith(...))`) up to a bounded
recursion depth.

### `test.prop(name, generators, fn, opts?)`

Register a property test (into the **same** table `test()` uses — so `--filter`,
`--parallel`, and coverage all work). Each of `opts.runs` iterations draws one
value per generator and calls `fn(...values)`.

- `generators` is an **array** of generators (positional → `fn(a, b, ...)`) or
  an **object** of `name → generator` (→ `fn({name: value, ...})`).
- `opts` (all optional): `{ runs?: 100, seed?: u64, maxShrinks?: 500 }`.

```ascript
import { prop, gen } from "std/test"

prop("addition commutes", [gen.int(), gen.int()],
  (a, b) => a + b == b + a, { seed: 1 })
```

> [!WARNING] **A property must RETURN A BOOL.** A **falsy** return (incl. `nil`)
> or a **Tier-2 panic** counts as a failure (so `assert.*` works inside the body
> — a failing assert panics). But beware: a passing `assert.eq` returns `nil`,
> which is **falsy** — so a body that *ends* in `assert.eq(...)` is read as a
> failure. End the body with an explicit boolean. Likewise, a body that ends in
> an **unhandled fallible call** returns a `[value, err]` Tier-1 pair, which the
> runner also treats as a failure — **destructure the pair and return a bool**
> instead:
>
> ```ascript
> // ✅  destructure the [value, err] pair, fold errors into the predicate
> prop("base64 roundtrips", [gen.string()], (s) => {
>   let dec = encoding.base64Decode(encoding.base64Encode(s))
>   if (dec[1] != nil) { return false }
>   return encoding.utf8Decode(dec[0])[0] == s
> }, { seed: 1 })
> ```

### Seed precedence & replay

The seed is chosen as **`opts.seed` > `--seed` > a fresh random seed**. A random
seed is **printed on failure** so any run is replayable. On failure the report
prints the **shrunken counterexample**, the failing iteration, the shrink count,
and the seed line, plus the exact replay invocation:

```text
FAIL my property: property failed
  counterexample: ...
  failing iteration: 7
  shrinks: 4
  seed: 1234567890 (from --seed)
  replay: ascript test file.as --seed 1234567890 --filter "my property"
```

### Shrinking

On failure the runner greedily searches for a **simpler** still-failing input,
re-running `fn` (deterministically re-seeded per candidate) and keeping any
candidate that still fails, to a fixpoint or `maxShrinks`. The strategy is
honest and bounded:

- **int/float** — shrink toward `0` by halving the distance, then flip sign
  toward positive;
- **string/array** — drop the tail by halves, then shrink interior runs, then
  shrink elements pointwise;
- **objectWith** — shrink field values pointwise (shape is fixed);
- **oneOf** — prefer earlier alternatives; **nilOr** — try `nil`, then shrink
  the inner value;
- **map/filter** — shrink the *source* and re-apply the mapping (filter discards
  respect `maxDiscard`).

It is a greedy local minimiser, not a global one — it finds *a* small
counterexample, not provably *the* smallest.

See `examples/property_testing.as` and `examples/advanced/prop_roundtrips.as` for
runnable properties (encode/decode roundtrip laws), and [the CLI docs](../cli)
for the test runner and its flags.
