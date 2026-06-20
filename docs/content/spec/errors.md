# Errors: the two-tier model & `recover`

This chapter specifies how AScript reports and handles errors. There are **no
exceptions**: every erroneous condition is either a **Tier-1 recoverable result**
(an error *value*), a **Tier-2 panic** (an unrecoverable bug), or a clean
compile/verification error. This is the *no undefined behavior* guarantee of the
[notation chapter](intro). The syntactic forms (`?`, `!`) are in the
[grammar chapter](grammar) and their precedence in the
[expressions chapter](expressions).

## Tier 1 — recoverable results

A **fallible** operation returns a two-element pair `[value, err]`:

- on success, `[value, nil]`;
- on failure, `[nil, err]`, where `err` is an **error value** — an object carrying
  at least a `message` field (and often a `code`).

The pair is an ordinary array, so it destructures directly. The annotation `error`
is `object | nil`; `Result<T>` is `[T, error]`. The global constructors `Ok(v)`
and `Err(e)` build the success and failure pairs.

```as
let [v, e] = json.parse("not json")
print(v)                    // nil
print(e.message)            // (a parse-error message)
```

Tier-1 results come from **fused** decode/IO/parse operations: a single call both
performs the work and reports failure as a value, never a panic. Argument-type
*misuse* of such a function (passing the wrong kind entirely) is a Tier-2 panic,
not a Tier-1 result — the pair reports *operation* failure, not *programming*
bugs.

### `?` propagation

Postfix `?` (`expr?`) **propagates** a Tier-1 failure: if `expr` evaluates to a
`[nil, err]` pair, the enclosing function returns that pair immediately; otherwise
`?` yields the success `value`. Using `?` where the enclosing function cannot
return a pair is a compile-time error.

```as
fn read() { return [nil, {message: "oops"}] }
fn use() {
  let v = read()?           // failure here returns [nil, {message:"oops"}]
  return [v, nil]
}
let [v, e] = use()
print(e.message)            // oops
```

### `!` force-unwrap

Postfix `!` (`expr!`) **force-unwraps** a `[value, err]` pair: it yields `value`,
or — if the pair is a failure — raises a **recoverable** Tier-2 panic carrying the
**original error message**. Use `!` to assert that a fallible step cannot fail
here; a violated assertion surfaces as a recoverable panic with the underlying
message, not a silent `nil`.

`?` and `!` share one precedence tier between `**` and unary, so they bind
**looser** than `await` and prefix `!`/`-`: `await x!` parses as `(await x)!`.

## Tier 2 — panics

A **Tier-2 panic** signals an unrecoverable bug: a bad type, wrong arity, an
undefined name, a frozen-value mutation, an out-of-range index, division by zero,
an i64 arithmetic overflow on a trapping operator, or a no-arm `match`. A panic
unwinds to the host, prints a source-pointed diagnostic with a stack trace, and
exits non-zero. Panics are **not catchable** in normal control flow — the only
boundary that converts a panic to a value is `recover` (below).

Some panics are deliberately **recoverable**: they are clean panics (never a
process abort) that `recover` can capture. The force-unwrap failure above is one;
the recursion limits below are another.

### Recursion limits

Two depths are capped: the **call depth** and the **expression-nesting depth**.
Exceeding either raises the recoverable panic `maximum recursion depth exceeded`.
This is a **clean** panic — never a `SIGABRT`/process abort — and is
**byte-identical on every engine** (tree-walker, specialized VM, generic VM, and
`.aso`-compiled). An **uncaught** recursion panic exits with a normal non-zero
status (not the 134 of an abort).

```as
fn loop(n) { return loop(n + 1) }
let [v, e] = recover(() => loop(0))
print(e.message)            // maximum recursion depth exceeded
```

## `recover`

`recover(f)` is the **single host boundary** that converts a Tier-2 panic into a
Tier-1 value. It runs a **zero-argument callable** `f`:

- if `f` returns normally with value `x`, `recover` yields `[x, nil]`;
- if `f` raises a recoverable panic, `recover` yields `[nil, err]`, where `err`
  carries the panic's message.

`recover` accepts **any zero-argument callable**: an **arrow** `() => …`, an
**anonymous function expression** `fn() { … }`, or a **named function**. All three
forms behave identically.

```as
let [v1, e1] = recover(() => assert(false, "boom"))         // arrow
let [v2, e2] = recover(fn() { assert(false, "boom") })       // anonymous fn-expr
fn bad() { assert(false, "boom") }
let [v3, e3] = recover(bad)                                   // named fn
print(e1.message, e2.message, e3.message)                    // boom boom boom
```

> Anonymous function expressions were added to the language in this release; the
> earlier carry-forward limitation in which `recover(fn(){…})` failed (only the
> arrow form worked) is **fixed** — `recover` now takes any of the three callable
> forms above.

One restriction applies to anonymous function expressions: they **cannot declare a
return type**. `fn(): T { … }` is a parse error (`anonymous function expressions
cannot declare a return type` — use a named `fn` declaration for an enforced
return type, or drop the annotation). The arrow and named-function forms are
unaffected.

`recover` is the boundary for REPL sessions, tests, and embedding hosts that must
not abort on a script bug; it is **not** a general control-flow mechanism (use
Tier-1 results and `?` for expected failures).

## Conformance

The error model in this chapter is exercised by:

- `examples/result.as` — Tier-1 `[value, err]` results and `?` propagation.
- `examples/force_unwrap.as` — `!` force-unwrap, including the recoverable-panic
  path.
- `examples/deep_recursion.as` — deep-but-bounded recursion completing normally.
- `examples/advanced/typed_errors.as` — typed error objects and `?` propagation.
- `tests/cli.rs` — `anon_fn_expression_runs_on_both_engines` pins the `recover`
  contract across all three callable forms (arrow, anonymous `fn`-expression, and
  named `fn`), byte-identical on the tree-walker and the VM, including the
  carry-forward `recover(fn() { assert(false, "boom") })` case.

Run the examples with `target/release/ascript run examples/result.as` (and
likewise); each matches its recorded golden. The four-mode `recover(fn(){…})`
result is reproduced directly: `recover(fn() { assert(false, "boom") })[1].message`
prints `boom` on both engines, and the `maximum recursion depth exceeded` panic is
recoverable and identical across engines.
