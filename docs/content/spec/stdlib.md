# The standard library

AScript is a **focused core with a Go-class standard library**. This chapter does
**not** enumerate the library — it is a short pointer that (1) designates the
per-module reference as the **normative** API documentation and (2) states the
**calling-convention rules** that hold across every `std/*` module. Those rules are
normative *here*; the individual function signatures and semantics are normative in
the reference.

## The stdlib reference is normative

The per-module reference under `docs/content/stdlib/` **is** the normative API
documentation for the standard library. This specification does not duplicate it:
where a function's parameters, return shape, or behaviour are described, the
reference governs. Start at the [standard-library overview](../stdlib/overview),
which indexes every domain page (collections, data & serialization, system, time,
networking, concurrency, and the rest).

Module **existence and ownership** are mechanically enforced — every `std/*` module
maps to exactly one owning reference page, and that mapping is drift-tested in both
directions, so a module can neither lack documentation nor be documented on two
pages. A standard-library module that a build does not include is an **unknown-import
error**, never a silent absence (see [Feature flags](#feature-flags)).

## Calling-convention rules (normative here)

The following rules hold for **every** standard-library function, regardless of
module:

- **Fallible functions return a Tier-1 `[value, err]` pair.** An operation that can
  fail for reasons outside the program's control — a parse that may reject its input,
  an I/O call, a network request — returns the two-element result pair defined in the
  [errors chapter](errors): `[value, nil]` on success, `[nil, error]` on failure. The
  error is a **value**, recoverable with `?` or pattern-matched directly — it is
  **not** a panic.

  ```as
  import * as json from "std/json"
  let [v, e] = json.parse("{bad")     // e != nil — a value, not a panic
  ```

- **Type misuse is a Tier-2 panic.** Passing an argument of the wrong **kind** to a
  native function — `math.abs("x")`, indexing with a non-integer — is a programmer
  error, not a recoverable outcome. It raises a source-pointed **Tier-2 panic** (see
  the [errors chapter](errors)) and exits non-zero unless `recover`ed. The pair
  convention is reserved for *input-dependent* failure, never for *misuse*.

- **Native functions ignore surplus positional arguments.** A `std/*` function is an
  ordinary `function` value; extra trailing positional arguments are **discarded**,
  not an error. (Too *few* arguments for a required parameter is the usual misuse
  panic.) The static checker's arity rule therefore flags only *too-few*.

- **OS-touching functions are capability-gated.** Any function that reads the
  filesystem, opens a socket, spawns a process, performs a foreign call, or reads the
  environment is governed by the corresponding capability per the
  [capability chapter](capabilities). The gate fires on the **call**, not the import.

- **Async functions return `future`s.** A standard-library function that performs
  concurrent or awaitable work returns a `future<T>` riding the single-threaded event
  loop, `await`ed like any other future (see the [concurrency chapter](concurrency)).

## Feature flags

The standard library is split into compile-time feature groups. A module **absent**
from a particular build is a clean **unknown-import error** at load — never a silent
no-op or a missing-symbol surprise. The default build includes the full batteries-set;
`--no-default-features` builds the bare language. The set of modules in a given build
is therefore a property of that build, surfaced honestly at import time.

## Always-global core

A small set of bindings is **always in scope** without an import, in every build
including `--no-default-features`: `print`, `len`, `type`, `assert`, `range`, `Ok`,
`Err`, and `recover`. These carry the contracts other chapters define — the
[values chapter](values) `len`/truthiness contract, the [errors chapter](errors)
`Ok`/`Err`/`recover` semantics. A program MAY shadow any of them with a local
binding (`let len = 5`).

## Conformance

The calling conventions in this chapter are exercised by:

- `examples/stdlib_completeness.as` — a broad sweep asserting the standard library's
  surface is present and behaves per the reference; it prints `stdlib completeness:
  all assertions passed`.
- `examples/stdlib.as` — a runnable tour of representative modules.

Verified directly:

- `target/release/ascript run examples/stdlib_completeness.as` runs to completion
  and prints its all-passed line.
- `target/release/ascript run examples/stdlib.as` runs to completion.
- Convention probes: `math.abs(-5, 99)` returns `5` (surplus argument discarded);
  `math.abs("x")` raises the Tier-2 panic `math.abs expects a number, got string`
  and exits non-zero; `json.parse("{bad")` returns a `[value, err]` pair with a
  non-`nil` error value (a Tier-1 result, not a panic).
