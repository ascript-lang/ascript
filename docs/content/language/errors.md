:::eyebrow Language

# Errors & results

AScript has **no exceptions** — there is no `throw`, `try`, or `catch`. Instead, errors come in two
clearly separated tiers.

## Tier 1 — recoverable errors are values

A fallible operation returns a two-element pair `[value, err]` (Go-style multiple returns), consumed
with array destructuring:

```ascript
import * as fs from "std/fs"

let [text, err] = fs.read("config.toml")
if (err != nil) {
  print(`could not read config: ${err.message}`)
  return
}
print(text)
```

On success, `err` is `nil` and `value` holds the result. On failure, `value` is `nil` and `err` is an
**error object** — an object with at least a `message` field (some modules attach more, such as a
`code`).

### Building results

Two global builtins construct result pairs:

```ascript
Ok(42)            // [42, nil]
Ok()              // [nil, nil]
Err("not found")  // [nil, { message: "not found" }]
```

A function that can fail should return `Ok(...)` / `Err(...)`:

```ascript
fn half(n: number): Result<number> {
  if (n % 2 != 0) { return Err("not even") }
  return Ok(n / 2)
}
```

## The `?` propagation operator

The postfix `?` operator unwraps a result pair, returning early on failure. It evaluates its operand
to `[value, err]`; if `err != nil` it makes the **enclosing function** `return [nil, err]`, otherwise
it evaluates to `value`.

```ascript
import * as fs from "std/fs"
import * as toml from "std/toml"

fn loadConfig(path: string): Result<object> {
  let text = fs.read(path)?      // returns [nil, err] early if the read fails
  let cfg  = toml.parse(text)?   // …and again if parsing fails
  return Ok(cfg)
}
```

This collapses the repetitive `if (err != nil) { return [nil, err] }` boilerplate into a single
character, while keeping every error path explicit.

> [!NOTE] `?` is only valid inside a function whose return value is a result pair. Using it elsewhere
> is an error. It composes with `await`: `await fetchUser(id)?` awaits, then propagates.

## Tier 2 — bugs panic

A **panic** is an unrecoverable programmer error. It unwinds the runtime, prints a diagnostic with a
source span, and exits non-zero. Panics are **not** results and are not caught by normal code. They
include:

- A failed [type contract](type-contracts).
- Indexing an array out of bounds with `arr[i]` (the checked `array.get(arr, i)` returns `nil`).
- Calling a non-function, or reading a field of `nil`.
- An explicit `assert(cond, msg)` failure.
- Misusing a builtin (e.g. `len` on a number).

```ascript
let xs = [1, 2, 3]
print(xs[9])     // panic: index 9 out of bounds (len 3)
```

> [!TIER2] Panics signal *caller bugs*, not runtime conditions you should handle in control flow. If
> a failure is expected (a missing file, bad user input), the API returns a Tier-1 result instead.

## `assert`

`assert(cond, msg?)` panics when `cond` is falsy. The message defaults to `"assertion failed"`. It is
the right tool for invariants and tests, not for validating untrusted input.

```ascript
assert(total >= 0, "total must never be negative")
```

## `recover` — the one boundary

`recover(fn)` is the single bridge from a panic back to a value. It calls `fn` with no arguments; on
success it returns `[result, nil]`, and if `fn` panics it converts that panic into `[nil, errObj]`.

```ascript
let [value, err] = recover(() => riskyComputation())
if (err != nil) {
  print(`recovered from: ${err.message}`)
}
```

`recover` exists for the REPL, the test runner (`ascript test` wraps each case in it), and embedding
hosts — **not** for routine control flow. Reach for Tier-1 results and `?` in ordinary code; reserve
`recover` for the outermost boundary where a crash would otherwise be unacceptable.
