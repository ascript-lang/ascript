# Gradual typing & the soundness model

This chapter specifies AScript's type system: the **runtime contracts** that fire
at annotated boundaries, the **static checker** that analyses code without running
it, the **soundness model** that decides which static diagnostics block a build,
and the **erasure** of generic type parameters. AScript is **gradually typed** —
annotations are optional, untyped code is fully supported, and the two halves
interoperate without ceremony. Type *names* and the type grammar are defined in the
[grammar chapter](grammar); their runtime behaviour as values is in the
[values chapter](values).

## Contracts (runtime)

A **type contract** is the runtime check attached to a *syntactically annotated*
slot. Contracts fire at exactly four places:

- a typed binding (`let x: T = …` / `const x: T = …`),
- a typed parameter, on entry to the function body,
- a typed return (`fn f(): T`), on the value flowing out, and
- a typed class field, on every assignment **including inside `init`**.

A contract is checked **eagerly to the full declared depth**: an `array<int>`
annotation checks every element, a `map<string, User>` checks every value, and a
class-typed slot recurses into the declared shape. A value that satisfies the
contract passes through unchanged; a value that violates it raises a **Tier-2 panic**
(see the [errors chapter](errors)) of the form `type contract violated: expected
<T>, got <type>`. An **unannotated** slot, or one annotated `any`, performs **no**
runtime check — this is the gradual escape hatch: untyped code runs exactly as if
the type system did not exist.

```as
fn double(n: number): number { return n * 2 }
print(double(21))          // 42
let xs: array<int> = [1, 2, 3]   // each element checked
// double("x")             // Tier-2 panic: type contract violated: expected number, got string
```

## The type grammar

The full type grammar is normative in the [grammar chapter](grammar). In summary,
a type is one of: a primitive (`int`, `float`, `decimal`, `string`, `bool`, `bytes`,
`nil`); the numeric union `number` (≡ `int | float`); an optional `T?` (sugar for
`T | nil`); a union `A | B`; a parameterized container (`array<T>`, `map<K, V>`,
`set<T>`); a tuple `[A, B]`; the result alias `Result<T>` (≡ `[T, error]`); a future
`future<T>`; a function type; a class / enum / interface name; or a type-parameter
variable in scope. `error` is the alias `object | nil`.

## The static checker

AScript ships a **static gradual checker** invoked by `ascript check`. It is
**advisory by default** and, critically, **NEVER runs the program** — it performs no
evaluation, opens no resources, and observes no side effects. It runs one inference
pass over the resolved module and reports diagnostics with source spans. Engines do
not consult it; it changes no runtime behaviour.

## The soundness model

The checker's diagnostics fall into two severities, and the split **is** the
soundness model:

- A provable **`type-mismatch`** on a **syntactically annotated** slot (a typed
  `let`/`const`, a typed return, a typed parameter, or a class field default) is a
  **blocking `Error`**: `ascript check` exits non-zero. This is the sound core — a
  value the checker can *prove* wrong for a slot the author *explicitly* typed is a
  build-stopping defect.
- **`possibly-nil`** (a provable `T?` dereference without a guard), **`type-error`**
  (arithmetic on a provable non-number), and any mismatch in an **inferred** (un-annotated)
  context are advisory **`Warning`s** — surfaced, never blocking.

A project MAY downgrade the block: `ascript.toml`

```toml
[lint]
warn = ["type-mismatch"]
```

demotes `type-mismatch` from `Error` to `Warning`, restoring fully-advisory
behaviour.

### The gradual escape (zero false positives)

The checker emits **only on a provable `No`**. Three-valued compatibility is
`Yes` / `No` / `Unknown`, and an **unsolved or unbounded type variable resolves to
`Unknown`, never `No`**. Because only `No` emits, code the checker cannot fully
reason about — i.e. untyped code — receives **no type diagnostics at all**. This is
the central invariant:

> **A false positive on untyped code is a conformance bug.** Every program in
> `examples/**` MUST emit **zero** `type-*` and exhaustiveness diagnostics, in
> **both** feature configurations.

This invariant is pinned by the corpus gate (see [Conformance](#conformance)); a new
diagnostic on the untyped corpus is a defect in the compatibility/inference logic,
to be fixed by widening toward `Unknown`, never by relaxing the gate.

## Generic inference

Type parameters appear on functions, classes, enums, and interfaces
(`fn id<T>(x: T): T`, `class Box<T> { v: T }`). The checker infers instantiations
**argument-driven**: it freshens the type variables, unifies them against the
argument types (occurs-checked union-find), substitutes the solution back, and
checks the result. Interface **bounds** (`<T: Comparable>`) are discharged via the
same structural conformance predicate the [classes chapter](classes) uses for
`instanceof`.

Parameterized class and enum applications are **invariant**: `Box<int>` is **not**
assignable to `Box<number>` even though `int` is assignable to `number`. Function
types remain covariant in their return position.

## Erasure

Generic type parameters are **runtime-ERASED**. This is normative and load-bearing:

- a `T`-typed slot performs **no runtime check** (a `T` contract accepts anything),
- generic instantiation creates **no distinct runtime type** — `Box<int>` and
  `Box<string>` are the **same** runtime class, and
- compiled bytecode carries **no type arguments**.

Consequently the same source is byte-identical across all engines whether or not it
uses generics, and a value that the *static* checker would reject in a typed context
still *runs* if the relevant slot is a type parameter. (See the
[classes chapter](classes) for the construction/erasure details.)

```as
class Box<T> { v: T }
let b = Box(1)     // T inferred int statically; erased at runtime
b.v = "s"          // T-slot: no runtime contract — runs, prints "s"
print(b.v)
```

## Lint stability

The **blocking-vs-advisory soundness model** above — `type-mismatch` on an annotated
slot is an `Error`, everything else advisory, `Unknown`-never-`No` keeps untyped code
clean — is **STABLE**. The concrete **lint-code inventory** (the specific code names,
their default severities, and the `[lint]` table keys) is **EXPERIMENTAL** per the
stability chapter: codes may be added, renamed, or have defaults adjusted between
minor versions. Pin behaviour you depend on via the `[lint]` table rather than the
default severities.

## Conformance

The type system in this chapter is exercised by:

- `tests/check.rs` — the checker's behaviour, including the **corpus gate**
  (`type_checker_emits_no_type_diagnostics_on_the_corpus`) that asserts **zero**
  `type-*`/exhaustiveness diagnostics over the entire example corpus in both feature
  configurations (the gradual-escape invariant), the blocking-`Error` behaviour of a
  `type-mismatch` on an annotated slot, and the generic-inference cases.
- `examples/typed.as`, `examples/typed_fields.as`, `examples/optional_types.as`,
  `examples/typed_config.as`, `examples/generics.as` — runnable type-annotated and
  generic programs.

Verified directly:

- `target/release/ascript check examples/typed.as examples/typed_fields.as
  examples/optional_types.as examples/typed_config.as` exits **0** — zero
  diagnostics, the key invariant.
- A `let x: int = "s"` file → `ascript check` reports `[type-mismatch] Error:
  expected int, found string` and exits **non-zero**; the same file with the
  annotation removed (`let x = "s"`) reports **zero** diagnostics — the soundness
  split in action.
- `target/release/ascript run examples/generics.as` runs to completion; a
  `class Box<T>` with `b.v` reassigned to a different type runs without a runtime
  contract — erasure confirmed.
