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

## The `!` force-unwrap operator

The postfix `!` operator is the **dual of `?`**. It evaluates its operand to a result pair `[value,
err]`; if `err == nil` it yields `value`, and if `err != nil` it **panics**, carrying the original
error's message. Unlike `?` (which propagates up to the enclosing function), `!` asserts that the
operation *cannot* have failed — reach for it when a failure would be a bug, or at the outermost
boundary inside a `recover`.

```ascript
fn half(n) {
  if (n % 2 != 0) { return Err("odd") }
  return Ok(n / 2)
}

half(8)!                          // 4 — unwraps the success value
let [v, e] = recover(() => half(3)!)
// e.message == "odd" — the panic carries the original message
```

### Precedence with `await`

Both `?` and `!` bind **looser than `await`** (and looser than prefix unary `!x` / `-x`). That means
`await resp.json()!` parses as `(await resp.json())!` and `await resp.json(User)?` parses as
`(await resp.json(User))?` — **no parentheses needed**. (Prefix `!x` for logical-not is unaffected:
position disambiguates it from postfix `!`.)

```ascript
// Decode + validate an HTTP body, then unwrap or propagate — no parens:
let user = await resp.json(User)?                       // [stdlib/net]
// Equivalent primitive composition with `.from` and force-unwrap:
let user = recover(() => User.from(await resp.json()!))?
```

## Tier 2 — bugs panic

A **panic** is an unrecoverable programmer error. It unwinds the runtime, prints a diagnostic with a
source span, and exits non-zero. Panics are **not** results and are not caught by normal code. They
include:

- A failed [type contract](type-contracts).
- Indexing an array out of bounds with `arr[i]` (the checked `array.get(arr, i)` returns `nil`).
- Calling a non-function, or reading a field of `nil`.
- An explicit `assert(cond, msg)` failure.
- A `!` [force-unwrap](#the--force-unwrap-operator) on a failed result pair (recoverable via `recover`).
- A `ClassName.from(obj)` [shape mismatch](classes-enums) (recoverable, carries a field path).
- Misusing a builtin (e.g. `len` on a number).
- Exceeding the **recursion-depth limit** — `maximum recursion depth exceeded` (recoverable via
  `recover`). Deep non-terminating recursion (and deeply nested expressions) hit a fixed logical-depth
  cap and panic *cleanly* before the native stack overflows, instead of crashing the process. The same
  limit applies on both the bytecode VM and the tree-walker.

```ascript
let xs = [1, 2, 3]
print(xs[9])     // panic: index 9 out of bounds (len 3)
```

A non-terminating recursion is caught like any other panic:

```ascript
fn forever(n) { return forever(n + 1) }
let [_, err] = recover(() => forever(0))
print(err.message)   // maximum recursion depth exceeded
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

## `exit` — clean program termination

`exit(code?)` terminates the program immediately with the given integer exit status. The `code` must be an integer in `0..=255`; it defaults to `0`. `exit` unwinds the async runtime cleanly and is **not** catchable by `recover`.

```ascript
// exit with success
exit(0)

// exit with a non-zero status to signal failure to the shell
exit(1)

// default: exit(0)
exit()
```

`exit` is a global builtin — no import needed. Call it only after all output has been flushed (or at the very end of the program); because `print` output is buffered and flushed at normal program exit, an `exit` call in the middle of output may cause in-flight `print` calls to be dropped. For scripts that need to signal failure to the shell, `exit(1)` at the end is the conventional idiom.
