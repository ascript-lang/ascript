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

### `assert.isTrue(x)`

Fail if `x` is falsy (`false` or `nil`).

### `assert.isFalse(x)`

Fail if `x` is truthy (anything other than `false` or `nil`).

### `assert.isNil(x)`

Fail if `x` is not `nil`.

### `assert.notNil(x)`

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

### `assert.contains(haystack, needle)`

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

### `assert.matches(value, regex)`

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

### `assert.throws(fn) -> errValue`

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

### `assert.throwsWith(fn, substr) -> errValue`

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
