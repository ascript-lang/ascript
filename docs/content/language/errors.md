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
- Spreading the wrong container kind — e.g. `[...5]` or `{...5}` — is a runtime panic (spreading
  into an array requires an array; spreading into an object requires an object).
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

`exit` is a global builtin — no import needed.

Under the CLI `ascript run` command, `print` streams **live to stdout** — output appears immediately
and is retained even if the program later panics. Calling `exit` at any point is therefore safe: all
previously printed output has already been written.

In capture mode (the REPL and `ascript test`), output goes to an internal buffer that is flushed at
the end of the run. In that context, `exit` bypasses normal cleanup and in-flight buffered output
may be dropped — but this is rarely a concern in practice, since test assertions are checked before
any `exit` is reached.

For scripts that need to signal failure to the shell, `exit(1)` at the end is the conventional idiom.

## Cleanup with `defer`

`defer <call>` registers a function call to run when the **enclosing function exits — by any
route**: normal return, `?`-propagation, or panic unwind. It is the answer to the recurring
problem where a `?` early-exit silently skips cleanup below it.

```ascript
import * as fs from "std/fs"
import * as io from "std/io"

fn copy(srcPath: string, dstPath: string): Result<number> {
  let [src, err] = fs.open(srcPath)
  if (err != nil) { return Err(err.message) }
  defer src.close()                      // runs on EVERY exit below, including the ?s

  let [dst, derr] = fs.create(dstPath)
  if (derr != nil) { return Err(derr.message) }   // src.close() runs here
  defer dst.close()                               // LIFO: dst closes before src

  let n = io.copy(dst, src)?             // propagation runs BOTH defers
  return Ok(n)
}
```

### The statement form

```
defer [await] <call>
```

`defer` accepts **call expressions only** — `defer f()`, `defer obj.close()`,
`defer a?.flush()`, `defer (() => { … })()` (the inline-block form). Expressions with no call
(`defer x`, `defer f`) are a parse error, because a deferred non-call has no side effect and is
a silent bug. `defer` is a **reserved keyword**.

**Evaluation timing (Go semantics):** the callee and all arguments are evaluated **immediately**
when the `defer` statement executes — only the *call itself* is deferred. This means
`defer f(x)` snapshots the current value of `x`; if `x` is later mutated the deferred call sees
the snapshot, not the mutation. To observe later mutations, use the arrow-IIFE form:
`defer (() => f(x))()` (the closure captures `x` by reference if it is mutated anywhere).

### Scope and ordering

The defer stack is **per function activation** — not per block. A `defer` inside an `if` branch
or a loop body runs at function exit, not at the end of the block. Defers inside a loop
accumulate one entry per iteration; they all run at function exit in LIFO order. (The
`defer-in-loop` lint warns about this accumulation pattern.)

Defers run **LIFO** (last registered, first run) and **innermost-frame-first** during unwind:
if `f` calls `g` and both register defers, `g`'s defers run when `g` exits (before returning to
`f`), and `f`'s defers run when `f` exits.

### When defers run — the frame-exit matrix

| Exit route | Defers run? |
|---|---|
| Normal return | **yes** |
| `return v` | **yes** — after `v` is computed |
| `?` propagation | **yes** — the `[nil, err]` pair is the in-flight outcome |
| Panic unwind | **yes** — every frame between the raise and the `recover` boundary drains |
| `exit()` | **NO** — `exit` means "terminate now"; defers are skipped, matching Go's `os.Exit` |
| Task cancellation (handle drop) | **NO** — see [async cleanup](#async-cleanup-with-defer-await) below |
| Generator `close()` / last-drop | **NO** — `close()` is synchronous; defers require the async engine |

**Cleanup that must survive cancellation belongs on the resource's deterministic Drop** — every
native handle (files, sockets, database connections, processes) already has one. `defer` is for
script-level ordering; the native safety net is independent and unaffected.

**`recover` interplay:** `recover(f)` observes the panic only *after* every frame inside `f` has
drained. The `[nil, err]` pair that `recover` returns carries the final merged panic message.

### A defer call cannot modify the return value

The return value (or propagating pair) is fully computed before any defer runs. A deferred call
that *panics* can replace it (rule 1 below), but an ordinary deferred call that just returns a
value cannot change what the function returns. Results of deferred calls are always discarded —
if you need to handle an error from a cleanup call, use the arrow-IIFE form and handle it there.

### Panics inside deferred calls

When a deferred call panics, the outcome depends on what was already in flight:

1. **In-flight normal return or `?`-propagation:** the defer's panic **becomes** the frame's
   outcome — the return value or propagating pair is discarded and the panic unwinds. (A Tier-2
   bug outranks a Tier-1 expected error.)
2. **In-flight panic (already unwinding):** the **original panic wins**. The new message is
   appended as a suppressed note — exact format:
   `<original> (suppressed panic in deferred call: <new>)` — so the root cause is always
   visible. Multiple suppressed panics append left-to-right in drain order.
3. **Remaining defers always run** regardless of whether a previous deferred call panicked.
   Cleanup must not be lost because other cleanup failed.

### Async cleanup with `defer await`

A bare `defer f()` where `f` returns a future is a **runtime error** — discarding the future
would cancel the async work instantly (cancel-on-drop). The explicit form is required:

```ascript
defer await teardown()    // drives the future to completion before the next (older) defer
```

`defer await f()` evaluates `f`'s arguments at the `defer` statement, marks the entry as
awaited, and drives the resulting future to completion before advancing to older defers.
`defer await` on a synchronous call is harmless (awaiting a non-future is identity).

**Cancellation during `defer await`:** if the enclosing task is cancelled while a deferred
`await` is suspended, the remaining (older) defers do not run — this is the same cancellation
rule above, observed mid-drain.

The `defer-async-call` lint warns statically when a bare `defer` is used with a call that
resolves to a known `async fn` — a provable future-return caught before runtime.

### Examples

- `examples/defer.as` — file close, LIFO, `?` interplay, `defer await`, evaluation timing
- `examples/advanced/defer_resources.as` — multi-resource acquisition, panic-unwind + `recover`
  observation of the merged message, the generator-owner pattern
