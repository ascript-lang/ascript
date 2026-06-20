# Statements & declarations

This chapter specifies AScript's statements and declarations: bindings,
destructuring, functions, control flow, `match`, and `defer`. The syntactic forms
are in the [grammar chapter](grammar); this chapter gives their runtime meaning.
Expression semantics are in the [expressions chapter](expressions).

A program is a sequence of statements (and the import/export items in the
*Modules* chapter). A statement ends at a newline; the semicolon `;` is an
optional separator (see the [lexical chapter](lexical)).

## Bindings

`let` introduces a mutable binding; `const` introduces an immutable one. A
`const` MUST have an initializer; a `let` MAY omit it (an uninitialized `let` is
`nil`). Loop variables and `fn`/`class`/`enum`/`import` bindings are immutable.

Two binding errors are **runtime-timed** — raised when the declaration or
assignment *executes*, not at compile time — so dead or uncalled code never
triggers them, and an assignment's right-hand side evaluates first:

- **Redeclaration of a module-scope global.** Re-declaring a top-level
  `let`/`const`/`fn`/`class`/`enum`/`import` name in the same module scope is the
  runtime error `'<name>' is already defined in this scope`. (A block-local `let`
  MAY shadow an outer binding; shadowing is not redeclaration.)
- **Assignment to an immutable binding.** Assigning to a `const` (or any
  immutable binding) at any scope is the runtime error `cannot assign to immutable
  binding '<name>'`.

```as
const k = 1
// k = 2          // runtime error: cannot assign to immutable binding 'k'

fn dead() { const k = 1; k = 2 }   // never runs → no error
print("ok")                        // ok
```

## Module-scope globals & late binding

A declaration directly at the top level of a module is a **module-scope global**.
A function body or a field default MAY reference a module-scope binding declared
*later* in the same module: resolution happens at **use** time, not at
declaration time.

```as
fn useLater() { return helper() }
fn helper()  { return 42 }
print(useLater())                  // 42  (helper resolved at call time)
```

## Destructuring

A `let`/`const` target MAY be an array or object destructuring pattern. A
non-matching container is a Tier-2 panic; there is no coercion.

- **Array destructuring** binds positionally; a trailing `...rest` collects the
  remaining elements into an array.
- **Object destructuring** binds by key from an object or instance. A missing key
  binds `nil`. A key MAY be renamed with `as` (`y as why`) and MAY be a quoted
  string. A trailing `...rest` collects the leftover keys into an object,
  preserving insertion order.

```as
let [a, b, ...rest] = [1, 2, 3, 4]
print(a, b, rest)                  // 1 2 [3, 4]

let {x, y as why, ...others} = {x: 1, y: 2, z: 3, w: 4}
print(x, why, others)              // 1 2 {z: 3, w: 4}

let {missing} = {}
print(missing)                     // nil
```

## Functions

A function is declared with `fn name(params) { … }`; the modifiers `async` (an
async function returning a `future`) and `fn*` (a generator) are specified in the
*Concurrency* chapter. Anonymous function expressions and arrows are in the
[expressions chapter](expressions).

Parameters MAY be typed, defaulted, or a rest parameter:

- A **typed** parameter (`x: int`) is contract-checked on entry (see the *Types*
  chapter).
- A **defaulted** parameter (`greeting = "hi"`) uses its default when the
  argument is omitted. Defaults are evaluated at call time, left-to-right, with
  earlier parameters in scope. A defaulted parameter MUST NOT be followed by a
  non-defaulted one.
- A **rest** parameter (`...xs`) MUST be last and collects the trailing arguments
  into an array; a typed rest (`...xs: array<T>`) is element-checked.

```as
fn greet(name, greeting = "hi") { return greeting + " " + name }
print(greet("a"))                  // hi a
print(greet("a", "yo"))            // yo a

fn sum(...xs) { let t = 0; for (x of xs) { t = t + x }; return t }
print(sum(1, 2, 3))                // 6
```

A `return` with no operand returns `nil`.

## Control flow

- **`if (cond) { … } else { … }`** — the condition is consulted for truthiness
  (see the [values chapter](values)); the `else` arm MAY be another `if`.
- **`while (cond) { … }`** — repeats the body while the condition is truthy.
- **`for (x of iter) { … }`** — iterates the values of an iterable (array, set,
  range, generator, …).
- **`for (i in a..b) { … }`** — iterates a range lazily.
- **`for await (x of stream) { … }`** — drives an asynchronous stream (see the
  *Concurrency* chapter).
- **`break`** / **`continue`** — exit or advance the nearest enclosing loop.
- **`return [expr]`** — returns from the enclosing function.

```as
for (x of [10, 20]) { print(x) }   // 10  20
for (i in 0..2)     { print(i) }   // 0   1
```

## `match`

A `match` is an expression: it tests its subject against arms top-to-bottom and
evaluates the first matching arm (first-arm-wins). Arms MAY use or-patterns and a
guard (`if`). The pattern forms, the bind-vs-compare rule, and static
exhaustiveness checking are specified in the *Patterns* chapter. A subject that
matches no arm at runtime is a Tier-2 panic on every engine.

## `defer`

`defer [await] <call>` schedules a **call** to run when the enclosing function
exits. `defer` is **call-only**: the operand MUST be a call expression (`defer x`
is a parse error). The callee and arguments are evaluated **at the `defer`
statement** (Go semantics — a `defer f(x)` snapshots `x`'s current value).

Deferred calls are scoped to the **function** (not the block — a `defer` inside an
`if` or loop still runs at function exit) and drain **LIFO** (last-deferred runs
first).

```as
fn work() {
  defer print("first-deferred")
  defer print("second-deferred")
  print("body")
}
work()
// body
// second-deferred
// first-deferred
```

The deferred call runs on normal return, on `?`-propagation, and on a panic
unwind. It does NOT run on `exit()`, on task cancellation, or on a generator's
last drop. `defer await f()` drives the returned future before the next older
defer; a bare `defer f()` whose call returns a future is a Tier-2 error (use
`defer await f()` or do async cleanup before exit). The panic-merge rules when a
deferred call itself panics are specified in the *Errors* chapter.

## Statement separators

A statement ends at a newline; `;` is an optional separator that MAY also separate
two statements on one line. `;` MUST NOT substitute for a comma. See the
[lexical chapter](lexical) for the full rule.

## Expression statements

Any expression may stand alone as a statement, evaluated for its effect. This
includes a bare anonymous function expression that is immediately invoked:

```as
fn() { print("hi from anon stmt") }()   // hi from anon stmt
```

## Conformance

The statements and declarations in this chapter are exercised by:

- `examples/functions.as` — function declarations, parameters, and `return`.
- `examples/default_params.as` — call-time default-parameter evaluation.
- `examples/rest.as` — rest parameters and trailing-argument collection.
- `examples/object_destructuring.as` — object destructuring with `as` rename,
  missing-key `nil`, and `...rest` collection.
- `tests/vm_differential.rs` — the destructuring batteries
  (`vm_destructure_array_matches_treewalker`,
  `vm_destructure_object_matches_treewalker`,
  `vm_destructure_type_errors_match_treewalker`) assert four-mode byte-identity
  (tree-walker, specialized VM, generic VM, and `.aso`).

Run the examples with `target/release/ascript run examples/functions.as` (and
likewise); each matches its recorded golden.
