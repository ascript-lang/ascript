# AScript — Language & Runtime Design Spec

- **Status:** Draft for review
- **Date:** 2026-05-29
- **File extension:** `.as`
- **Runtime:** Rust (single binary `ascript`)

---

## 1. Vision & Design Priorities

AScript is a small, dynamically-typed scripting language with **JavaScript-flavored
syntax**, a **batteries-included standard library**, and **optional runtime-checked
type annotations**, executed by a **tree-walking interpreter written in Rust**.

The guiding model is **"Lua-simple language, Go/Deno-class standard library"**:

- The *language core* stays as simple as Lua — a tree-walking interpreter, ~8 value
  kinds, gradual contracts, no hidden control flow.
- The *standard library and tooling* are deliberately rich, because Rust's crate
  ecosystem makes high-quality batteries cheap to include.

Design priorities, in strict order:

1. **Simplicity** — a beginner can hold the whole language in their head.
2. **Safety** — errors are explicit; mistakes fail loudly, not silently.
3. **Familiarity** — anyone who knows JavaScript can read AScript immediately.
4. **Performance** — adequate for scripting; never at the expense of the above.

### Non-goals (v1)

- No static type inference or compile-time type checking (types are runtime contracts).
- No bytecode VM or JIT (tree-walker only). **Superseded 2026-06-04:** a bytecode VM is now
  the **default** engine and the tree-walker is the byte-identical reference oracle (`--tree-walker`;
  CLAUDE.md architecture, `src/lib.rs` `vm_run_source`). JIT remains a non-goal.
- No multithreading in user code (single-threaded event loop; see §7).
- No macro system, operator overloading, or metaprogramming.
- No package manager / registry (deferred to a future spec).
- No audio or graphics/windowing in v1 — these imply a main-thread windowing event
  loop that conflicts with the single-threaded Tokio model (§7), plus heavy
  platform/GPU dependencies. Deferred to a future **"AScript Media"** spec / optional
  build (see §15).
- No tagged-union enums with typed payloads — enums are simple named variants only
  (§8.2); use a class for per-variant data.

---

## 2. Lexical Structure

- **Encoding:** source files are UTF-8.
- **Comments:** `// line` and `/* block */`.
- **Statement termination:** newline-terminated with lightweight automatic
  semicolon insertion (ASI-lite). Explicit `;` is allowed but never required. `;`
  is honored as an optional separator wherever newlines separate self-delimiting
  members — top-level/block statement lists **and class bodies** (so a one-line
  `class P { x: number; y: number }` parses). It never substitutes for `,`: enums,
  match arms, params, and array/object literals stay comma-delimited. The formatter
  always canonicalizes `;` back to newlines.
- **Identifiers:** `[A-Za-z_][A-Za-z0-9_]*`.
- **Keywords:** `let const fn return if else while for of in match async await
  class extends super self enum import export nil true false`.
- **Literals:**
  - Numbers: `42`, `3.14`, `1e9`, `0xFF`, `0b1010` (all become one `number` type).
  - Strings: `"double"`, `'single'`, and template strings `` `hi ${name}` ``.
  - Booleans: `true`, `false`.
  - Nil: `nil`.
  - Array: `[1, 2, 3]`.
  - Object: `{ key: value, "quoted": 1 }`.
- **Operators:** `+ - * / % ** == != < <= > >= && || ! ?? ?. = += -= *= /=`
  plus the Result-propagation postfix `?` (§6) and the range operator `..`.

Every token carries a **source span** (byte offsets + line/col). Spans flow through
the AST into runtime values where useful, so diagnostics (§10) can point at exact
source locations.

---

## 3. Syntax Overview

```ascript
// Bindings
let count = 0          // mutable
const name = "Ada"     // immutable (rebind is a compile-time error)

// Functions
fn add(a, b) { return a + b }
const double = (x) => x * 2          // arrow, expression body
const greet  = (who) => { return `hi ${who}` }  // arrow, block body

// Control flow
if (count > 0) { print("positive") } else { print("non-positive") }

while (count < 3) { count += 1 }

for (item of [10, 20, 30]) { print(item) }   // iterate values
for (i in 0..10) { print(i) }                // numeric range [0,10)

// match expression (returns a value)
const label = match count {
  0       => "zero",
  1 | 2   => "small",
  _       => "many",
}
```

### Grammar sketch (informal EBNF)

```
program     := item*
item        := import | export | statement
statement   := letDecl | constDecl | fnDecl | classDecl | enumDecl
             | ifStmt | whileStmt | forStmt
             | returnStmt | exprStmt | block
block       := "{" statement* "}"
exprStmt    := expr
expr        := assignment
assignment  := logicOr ( ("=" | "+=" | ...) assignment )?
logicOr     := logicAnd ( "||" logicAnd )*
... (standard precedence climbing) ...
unary       := ("!" | "-") unary | postfix
postfix     := primary ( call | index | member | "?" )*
primary     := literal | identifier | "(" expr ")" | arrayLit | objectLit
             | arrowFn | matchExpr | "await" expr
```

Precedence (high → low): `() [] . ?` → unary `! -` → `**` → `* / %` →
`+ -` → comparison → `==`/`!=` → `&&` → `||` → `??` → assignment.

### The spread operator `...`

`...expr` expands a container in place. It is valid in three positions:

```ascript
let more   = [0, ...base, 4]              // array literal
let config = {...defaults, port: 443}     // object literal
sum3(...nums)                             // call arguments
```

- **In an array literal**, `...x` requires `x` to be an `array`; its elements are
  inlined at that position. Spreads and plain items mix freely.
- **In an object literal**, `...o` requires `o` to be an `object`; its entries are
  merged. Merging is **later-value-wins** — a key appearing after a spread overrides
  it — while the key keeps its **first-seen position** (insertion order is preserved).
- **In a call**, `...args` requires `args` to be an `array`; its elements become
  positional arguments (and combine with `...rest` parameters, §5).

Spread is **strict**: spreading the wrong container kind (e.g. `[...5]`, `{...5}`, or
a non-array as call args) is a Tier-2 panic (§6). There is no array↔object coercion.

---

## 4. Value Model

AScript has **eight value kinds**:

| Kind | Description | Mutability |
|---|---|---|
| `nil` | absence of a value | — |
| `bool` | `true` / `false` | immutable |
| `number` | IEEE-754 float64 (one numeric type) | immutable |
| `string` | immutable UTF-8 text | immutable |
| `array` | ordered list, `[...]` | mutable, shared by reference |
| `object` | string-keyed record, `{...}` | mutable, shared by reference |
| `map` | hash map with arbitrary keys, `#{...}` | mutable, shared by reference |
| `function` | closure (incl. async fns and methods) | immutable |

Class instances are `object` values **tagged** with their class (§8).

**Map literals — `#{ keyExpr: valueExpr, … }`** build a `map` directly (no `std/map`
import). Unlike object literals, the part before `:` is an **expression evaluated to a
key** (so `#{ k: 1 }` keys by the *value* of `k`), keys may be any hashable value
(`nil`/`bool`/`number`/`string`), `#{}` is the empty map, and a repeated key is
later-value-wins. An unhashable key (e.g. an array) is a Tier-2 panic. Spread inside a
map literal (`#{ ...m }`) is not supported (a clean parse error). See §8.3.

**Reference semantics:** `array`, `object`, `map`, and class instances are heap
values shared by reference (assignment copies the handle, not the contents).
`nil`, `bool`, `number`, `string` are value-semantic.

**Truthiness:** only `nil` and `false` are falsy. `0`, `""`, `[]`, `{}` are truthy
(closer to Lua than JS — fewer surprises).

**Equality:** `==` is structural for `string`/`number`/`bool`/`nil`; identity-based
for `array`/`object`/`map`/`function`. No implicit type coercion across kinds
(`1 == "1"` is `false`).

**Safe access operators:** reading a field of `nil` or indexing out of bounds with
`[]` *panics* (Tier 2, §6). Two operators opt into safe, nil-returning access
instead: optional chaining `obj?.field` evaluates to `nil` when `obj` is `nil`
(short-circuiting the rest of the chain), and nil-coalescing `a ?? b` evaluates to
`b` when `a` is `nil`. Together `cfg?.db?.port ?? 5432` reads a deep optional path
with a default and never panics.

---

## 5. Type System — Gradual, Runtime-Checked Contracts

Type annotations are **optional**. When present, they are enforced **at runtime as
contracts** (not statically checked, not erased). This keeps the runtime small
while still catching type mistakes the moment they happen.

```ascript
fn add(a: number, b: number): number {
  return a + b
}

let userName: string = "ada"
const ids: array<number> = [1, 2, 3]
```

**Where contracts fire:**

- On a typed `let`/`const` binding (the assigned value is checked).
- On entry to a typed function parameter.
- On a typed function's `return`.

**Type grammar:**

```
type := "number" | "string" | "bool" | "nil" | "any" | "fn"
      | "array" "<" type ">"
      | "map" "<" type "," type ">"
      | "object"
      | "error"                 // an error object or nil  ( object | nil ), §6
      | "Result" "<" type ">"   // sugar for the tuple  [ type, error ]
      | "[" type ("," type)* "]" // fixed-length tuple, e.g. [number, error]
      | ClassName
      | EnumName                // an enum type; accepts any of its variants
      | type "|" type          // union, e.g.  number | nil
```

`any` disables checking for that position. Omitting an annotation is equivalent to
`any`. A failed contract is a **programmer bug**, not a recoverable error — it
**panics** (§6), it does not produce a Result.

**Parametric depth:** a contract is checked *eagerly and to its full declared
depth* at the check site. `array<number>` verifies the value is an array **and**
that every current element is a `number`; `map<string, array<number>>` recurses
likewise. This is O(n) in the collection size at the check point, which is
acceptable because checks happen only at typed binding/parameter/return sites, not
on every element access. Use `array` (unparameterized) or `any` to opt out of the
element scan.

### Rest parameters

A function's **last** parameter may be a rest collector `...name`, which gathers the
trailing positional arguments into a fresh array (empty `[]` when none are passed):

```ascript
fn sum(...nums: array<number>) {        // nums : array<number>
  let total = 0
  for (n in nums) { total = total + n }
  return total
}
sum(1, 2, 3, 4)   // 10
sum()             // 0

fn tagged(label, ...rest) { ... }       // fixed param + untyped rest
```

A rest parameter's type, if present, must be an **array type** (`array<T>`); each
collected argument is contract-checked against the element type `T` as it is gathered
(a mismatch panics, §6). A bare `...rest` (no annotation) is untyped. Declaring a rest
parameter that is not last, or giving it a non-array type, is an error. Spread (below,
§3) is the inverse: `f(...args)` expands an array back into positional arguments, so
`...rest` collectors and `...spread` round-trip. For `async fn` / `fn*`, arity and
rest-element contract errors surface lazily (when the future/generator is driven),
consistent with §7.

---

## 6. Error Handling — Result Values (Two Tiers)

AScript has **no exceptions** (`throw`/`try`/`catch` do not exist). Errors come in
two tiers with a sharp boundary.

### Tier 1 — Recoverable errors are *values*

Fallible functions return a two-element pair `[value, err]` (using ordinary array
destructuring — Go-style multiple returns, JS-shaped):

```ascript
let [data, err] = readFile("config.toml")
if (err != nil) {
  print("failed: " + err.message)
  return [nil, err]
}
use(data)
```

Helpers:

- `Ok(v)` → `[v, nil]`
- `Err(msg)` → `[nil, { message: msg }]` (an *error object*: an `object` with at
  least a `message` field; modules may attach more, e.g. `code`).

**Types:** the annotation `error` means `object | nil` (an error object or its
absence). `Result<T>` is sugar for the pair type `[T, error]`. So
`fn load(): Result<object>` is the readable, *correct* way to type a fallible
function — it permits both `[value, nil]` on success and `[nil, errObj]` on failure,
which a naive `[object, object]` would wrongly reject under the contract system (§5).

### The `?` propagation operator

The postfix `?` unwraps a Result, returning early from the **enclosing function**
if the error is non-nil:

```ascript
fn load(): Result<object> {
  let data = readFile("config.toml")?   // returns [nil, err] on failure
  let cfg  = toml.parse(data)?
  return Ok(cfg)
}
```

`expr?` evaluates `expr` to a `[value, err]` pair; if `err != nil` it makes the
enclosing function `return [nil, err]`, otherwise it evaluates to `value`. Using `?`
in a function that does not return a Result pair is a compile-time error.

### Destructuring `let` bindings

The Tier-1 `[value, err]` convention is read with **array destructuring**; the same
binding form also destructures objects.

**Array destructuring** binds positionally:

```ascript
let [data, err] = readFile("config.toml")   // the Result idiom
let [head, ...tail] = [10, 20, 30]           // head = 10, tail = [20, 30]
```

A trailing `...rest` collector gathers the remaining elements into a fresh array
(empty `[]` if none remain). The collector must be last.

**Object destructuring** binds by key from an `object` **or a class instance**:

```ascript
let user = {name: "Ada", role: "admin", "login count": 42}
let {name, role as r} = user            // name = "Ada", r = "admin"
let {"login count" as logins, missing} = user  // logins = 42, missing = nil
```

- A bare `{a}` binds local `a` to the value at key `"a"`; `{a as local}` renames via
  the soft keyword `as`. Keys are identifiers or string literals — quote any key that
  is not a bare identifier (`"login count" as logins`).
- A **missing key binds `nil`** (no panic), so destructuring is total over the keys
  it names.
- A trailing `...rest` collector gathers the **remaining** keys into a fresh object,
  excluding every key named in the pattern (matched against the source keys, by their
  original name). Insertion order of the source object is preserved; empty `{}` if
  nothing remains.

```ascript
let {id, ...meta} = {id: 7, role: "admin", active: true}
// id = 7, meta = {role: "admin", active: true}
```

Destructuring a non-object (object pattern) or a non-array (array pattern) is a
Tier-2 panic (§6) — there is no coercion between the two shapes.

### Tier 2 — Programmer bugs *panic*

Unrecoverable bugs do **not** return Results (that would force every call site to
check the impossible). Instead they **panic**: unwind to the runtime, print a
diagnostic + stack trace, and exit non-zero. Panics include:

- A failed type contract (§5).
- Indexing out of bounds via the *unchecked* accessor `arr[i]`
  (the checked accessor `arr.get(i)` returns `nil` instead).
- Calling a non-function, or reading a field of `nil`.
- An explicit `assert(cond, msg)` failure.
- **Exceeding the recursion-depth limit** (SP3). Deep non-yielding recursion (and a
  deeply nested expression) is capped at a fixed logical depth (`MAX_CALL_DEPTH`);
  over it the runtime raises `maximum recursion depth exceeded` **before** the
  native stack overflows — a clean, deterministic panic, NOT a process abort, and
  identical on both engines (the bytecode VM and the tree-walker oracle). This is a
  graceful guard, not unlimited recursion: truly unbounded recursion needs an
  explicit-stack VM and remains an architectural non-goal (see §7's non-goals).

Panics are **not catchable** in normal code — this keeps the value-based model
honest. A single host/REPL boundary, `recover(fn)`, runs `fn` and converts a panic
into a Result `[nil, err]` (this includes `maximum recursion depth exceeded` — it is
an ordinary recoverable panic); it exists for the REPL, the test runner, and
embedding, not for routine control flow.

**Bytecode-capacity compile errors (VM only).** A module too large for the bytecode
format — more than 65535 constants / function definitions / class definitions /
imports, a single function body whose jump spans > 32 KB of bytecode, or a string/
collection too large to serialize to `.aso` — is rejected at compile time with a
clean, actionable error (non-zero exit, never a process abort). These are honest
capacity limits of the compiled representation; the tree-walker (which has no
bytecode) runs such a module, a documented and correct asymmetry (it is the debug/
oracle engine, not a second production dialect).

---

## 7. Concurrency — async/await on a Single-Threaded Event Loop

AScript supports `async fn`, `await`, generators (`fn*` / `async fn*`), `yield`, and
`for await`. The implementation (**approach A**) makes the interpreter's core `eval` an
`async fn` in Rust running on a single-threaded Tokio executor, which *is* the event
loop. Concurrent tasks ride a `tokio::task::LocalSet` with `spawn_local`, which accepts
`!Send` futures — so the `Rc<RefCell<…>>` value model (§9) is preserved unchanged and
there is still **no user-visible multithreading**.

The key realization driving the whole design: Rust `async`/`.await` *is* a stackless
coroutine transform, and `eval` is *already* an `async fn`. So both real concurrency and
script-level generators fall out of the engine we already have — no `unsafe` stackful
coroutine crate, and no continuation-passing rewrite.

### 7.1 Tasks, eager scheduling, and real `await`

- Calling an `async fn` returns a **`future<T>`** value (§7.6) that is **eagerly
  scheduled**: it begins executing immediately as a task on the `LocalSet` and makes
  progress whenever the current task is parked at an `await`. Calling a non-`async` fn is
  unchanged — it runs inline to its `return`. The task's lifetime is **bound to its
  `future<T>` handle** — see §7.2 (cancel-on-drop).
- `await expr` drives `expr`'s future to completion and evaluates to its result. As a
  deliberate back-compat rule, **`await` on a non-future is the identity** (`await 5` is
  `5`, `await xs` is `xs`) — so older code and gradual typing keep working.
- Async stdlib functions (I/O, timers, sockets, HTTP, WebSockets) return awaitables that
  the Tokio runtime drives; `await`-ing them suspends only the current task, letting other
  ready tasks run.
- Synchronous programs never spawn a task and pay no async cost.
- User code is **single-threaded** — no data races, so heap values use `Rc<RefCell<…>>`
  rather than `Arc<Mutex<…>>` (§9). The single interpreter-internal rule the concurrency
  model adds is: *never hold a `RefCell` borrow across an `.await`* (a held borrow that
  outlives a suspension point could alias another task's mutation); the build enforces this
  with clippy's `await_holding_refcell_ref`.

Async composes with the Result model:

```ascript
async fn fetchUser(id: number): Result<object> {
  let resp = await http.get(`https://api.example.com/users/${id}`)?
  let user = json.parse(resp.body)?
  return Ok(user)
}
```

### 7.2 Structured concurrency — cancel-on-drop

A task's lifetime is **bound to its `future<T>` handle**. When the last handle referring to
a task is dropped, the task is **cancelled** — the dual of the consumer-driven generator
design (§7.4): work without an owner does not linger. Concretely:

- Calling an `async fn` and **discarding** the result cancels it: the handle drops at the
  end of the expression statement, aborting the task before its body runs further. An
  un-awaited-and-dropped call therefore does **not** run to completion, and its side effects
  do not happen.
- **`await`** holds the handle across the work, so an awaited call always completes.
- **`task.spawn(...)`** is the explicit opt-out: it *detaches* the task so it runs to
  completion (fire-and-forget) regardless of whether the returned handle is awaited or
  dropped.

This bounds memory by construction: a long-running loop that fires un-awaited async calls
cannot accumulate orphaned tasks (each is cancelled as its handle drops; finished/cancelled
tasks are reaped cooperatively). It also removes a footgun — there is no "work happens at
some surprising later point."

At program exit the runtime drives the `LocalSet` to completion so every task still alive
(awaited, held, or detached via `spawn`) finishes before the process exits; a panic in such
a task surfaces (it is not silently swallowed). The top-level program may itself be `async`;
the runtime drives it, then drains the rest of the `LocalSet`, to completion.

### 7.3 The `std/task` module

`std/task` exposes the concurrency primitives over `future<T>`:

```ascript
import { spawn, gather, race, timeout } from "std/task"

let a = spawn(fetchUser(1))               // schedule a task, get its future<T> handle
let b = spawn(fetchUser(2))
let [ua, ub]  = await gather([a, b])      // run an array of futures concurrently, results in order
let first     = await race([a, b])        // first to finish wins; the losers are cancelled
let [v, err]  = await timeout(500, slow()) // Result pair: [value, nil] or [nil, err] on deadline
```

- `spawn(futureOr0ArgFn)` **detaches** a task (the explicit opt-out of cancel-on-drop, §7.2)
  and returns its `future<T>`, so the work runs to completion even if the handle is dropped.
  Given a `future` it detaches and returns it; given a 0-arg function it calls it — which
  schedules it — detaches the result, and returns the future.
- `gather([futures])` takes an **array** of futures, awaits them all concurrently, and
  returns an **array** of their results in order (the structured "join all" combinator). The
  first error short-circuits and propagates via the panic / `?` channel. (Inputs run
  concurrently because each `async fn` call was already scheduled eagerly.)
- `race([futures])` takes an **array** of futures and resolves to the value of the first to
  complete; the **losers are cancelled** (their handles drop as `race` returns).
- `timeout(ms, future)` returns a Result pair (§6) — `[value, nil]` if `future` resolves
  before `ms`, else `[nil, err]` (a Tier-1 timeout error) when `ms` elapses first — and
  never panics on a missed deadline. On timeout the future handle drops, so the **timed-out
  work is cancelled** rather than left running.

### 7.4 Generators & coroutines

A generator function is written `fn*` (or `async fn*`); inside it, `yield e` produces a
value to the consumer and **suspends** at that point. A suspension point is, mechanically,
an internal `.await` on a single-consumer **rendezvous** between the generator's eval
future and its driver — the same technique `genawaiter`/`async-stream` use to build
coroutines on stable async. Calling a generator function does not run its body; it returns
a **`Value::Generator`** handle.

Generators are **bidirectional coroutines**: `gen.next(v)` resumes the paused `yield` and
makes that `yield` expression evaluate to `v` inside the generator, then runs until the
next `yield` or `return`. `next()` reports completion (the final `return` value vs. a
yielded value) so a consumer can tell "another value" from "done".

```ascript
fn* counter(start) {
  let n = start
  while true {
    let step = yield n        // value passed in via next(step)
    n = n + (step ?? 1)
  }
}

let g = counter(10)
g.next()        // -> 10
g.next(5)       // -> 15   (the paused `yield n` received 5)
```

`async fn*` generators may `await` between yields, so a generator can stream values pulled
from real I/O.

### 7.5 `for await`

`for await (x in e)` consumes any **async-iterable** and binds each produced value to `x`:

- a script generator (`fn*` / `async fn*`), driven via its internal rendezvous, or
- a native stream handle (e.g. a channel/SSE/WebSocket source exposing a `recv()`-style
  pull), driven by `await`-ing the next item.

```ascript
for await (chunk in sseStream) {
  print(chunk)
}
```

The loop suspends the current task between items (cooperative), and terminates when the
source signals end-of-stream / the generator `return`s.

### 7.6 The `future<T>` type

`future<T>` is a first-class **type** for contracts (§5): the eventual result of an async
computation, with element type `T`. `async fn fetchUser(...): Result<object>` has call type
`future<Result<object>>`; `await`-ing it yields the `Result<object>`. A bare `future`
(unparameterized) accepts any future. `future<T>` values are produced by calling an
`async fn`, by `spawn`, and by the `std/task` combinators.

### 7.7 Non-goals / deferred to a future engine

The following are **deliberate architectural boundaries** of approach A, not unfinished
work. Each is impossible under a stackless-async tree-walker and would require a different
execution engine; they are recorded here so the boundary is explicit. (See the ADR
`specs/adr/2026-05-30-async-generators.md`.)

- **Durable / serialize-to-disk continuations** — checkpointing a paused workflow to disk
  and resuming it in a later process. Async suspension state lives in the Rust stackframes
  the compiler generates; it is not a reified, serializable object. *Needs an
  explicit-stack VM with reified continuations (option **B2**, §below).*
- **Robust unbounded recursion over very deep data** — deep *non-yielding* script recursion
  still consumes the native call stack and can overflow it, because stackless async does not
  move recursion off the host stack. *Needs stackful coroutines (**B1**) or an explicit-stack
  VM (**B2**).*
- **Deterministic / replayable task scheduling** — Tokio owns task interleaving, so runs are
  not bit-for-bit reproducible and cannot be deterministically replayed. *Needs a custom
  scheduler over an explicit-stack VM (**B2**).*

---

## 8. Classes & Enums

### 8.1 Classes (JS-style, Single Inheritance)

```ascript
class Animal {
  fn init(name) { self.name = name }      // constructor
  fn speak() { return `${self.name} makes a sound` }
}

class Dog extends Animal {
  fn init(name, breed) {
    super.init(name)
    self.breed = breed
  }
  fn speak() { return super.speak() + " — woof" }
}

const d = Dog("Rex", "Husky")   // no `new` keyword; class is callable
print(d.speak())
```

- `self` is the receiver inside methods.
- `init` is the constructor; calling `ClassName(args)` allocates an instance,
  tags it with the class, and runs `init`.
- Single inheritance via `extends`; `super.method(...)` calls the parent.
- Method resolution walks the class chain; instance fields shadow nothing
  (fields and methods share the object namespace, methods found via the class tag).
- A class name is a valid **type** for contracts (§5): `fn walk(a: Animal) {...}`
  accepts `Animal` and its subclasses.

**Generator methods.** A method may be a generator — `fn*` or `async fn*`. Calling
it returns a generator bound to `self` (driven by `for await` / `.next()` /
`gen.close()`), exactly like a standalone generator (§6); `self`, arguments,
contracts, inheritance, and `super` all work as for an ordinary method.

```ascript
class Counter {
  fn init(start) { self.start = start }
  fn* upTo(n) {
    let i = self.start
    while (i <= n) { yield i
      i = i + 1 }
  }
}
for await (v in Counter(3).upTo(6)) { print(v) }   // 3 4 5 6
```

**Static methods.** A member declared `static fn name(...)` (also `static async fn`
and `static fn*`) is a **class-level** method called as `ClassName.name(args)` with
**no `self`** / no instance. Statics live in a **separate namespace** from instance
methods (an instance `c.x()` and a static `C.x()` may share a name), are
**inherited** up the superclass chain, and may construct instances or call other
statics. `super` is invalid inside a static (no instance/parent receiver).

```ascript
class Point {
  fn init(x, y) { self.x = x
    self.y = y }
  static fn origin() { return Point(0, 0) }   // sync factory
  fn sum() { return self.x + self.y }
}
print(Point.origin().sum())   // 0
```

Because construction is synchronous (`Point(...)` returns an instance, not a
future), the blessed pattern for **async construction** is a `static async fn
create(...)` factory returning a `future<T>` — `create` is a convention, not a
keyword:

```ascript
class User {
  fn init(name) { self.name = name }
  static async fn load(id) {
    let u = User("?")
    u.name = await fetchName(id)   // async work, then return the built instance
    return u
  }
}
let u = await User.load(42)
```

- An **`async fn init`** (or a generator **`fn* init`**) is **forbidden** —
  synchronous construction has no caller to `await` it and a generator constructor
  makes no sense. Both are a clean compile-time error on either engine (*"init must
  be a synchronous constructor; use a static async factory (e.g. `static async fn
  create()`)"*) — use a `static async fn create()` factory instead.
- The name **`from`** is **reserved** on classes (it collides with the built-in
  typed-parse `ClassName.from`, §5), so `static fn from` is an error.

**Optional method calls.** Postfix `?.` (§ safe-access) also guards **calls** —
`a?.m(args)`: when the receiver is `nil` the call yields `nil`, the rest of the
postfix chain is short-circuited, and the argument expressions are **not
evaluated**; when the receiver is non-`nil` it is an ordinary bound method call.

```ascript
let n = nil
print(n?.m(expensive()))   // nil — m is never reached, expensive() never runs
```

### 8.2 Enums (simple, named variants)

An `enum` declares a closed set of named variants. Variants may optionally carry a
backing value (a `number` or `string`); without one they are opaque, unique values.

```ascript
enum Color { Red, Green, Blue }                 // opaque variants
enum Status { Ok = 200, NotFound = 404, Err = 500 }  // number-backed
enum Mode { Read = "r", Write = "w" }           // string-backed
```

- **Access:** `Color.Red`, `Status.NotFound`.
- **Backing value:** `Status.NotFound.value == 404`. Opaque variants have
  `.value == nil`. The variant's name is available as `.name` (`"NotFound"`).
- **Equality:** variants are interned singletons — `Color.Red == Color.Red` is
  `true`, and a variant is never equal to a variant of another enum or to its raw
  backing value (`Status.Ok == 200` is `false`; compare `.value` for that).
- **Contracts:** an enum name is a type (§5). `fn paint(c: Color) {...}` panics
  unless given a `Color` variant.
- **`match`:** variants are matched by their qualified name, and `_` is the
  catch-all:

```ascript
fn describe(c: Color): string {
  return match c {
    Color.Red   => "warm",
    Color.Green => "cool",
    _           => "other",
  }
}
```

**Representation:** an enum variant is a *tagged value* carrying its enum tag,
variant name, and optional backing value — built on the same object-tagging
mechanism as class instances (§8.1), so it adds no new conceptual value kind to §4.
Enums are intentionally *simple*: no associated typed payloads and no methods
(tagged-union ADTs are a deliberate non-goal for v1; use a class if you need
per-variant data or behavior).

### 8.3 SP2 language additions

Five surface features added after the original draft, each implemented identically on
both engines (bytecode VM + the `--tree-walker` oracle) and byte-identical in output:

**`instanceof`** — a reserved binary operator at the comparison precedence tier
(`x instanceof C`, looser than `&&`, same tier as `<`/`>`). It tests class membership,
walking the superclass chain; a non-instance left side is always `false` (never panics).
The right side **must** be a class — a non-class right side is a Tier-2 panic.

```ascript
class Animal {}
class Dog extends Animal {}
print(Dog() instanceof Animal)   // true
print(Dog() instanceof Dog)      // true
print(5 instanceof Animal)       // false (never panics)
```

**Default parameters** — `fn f(a, b = expr)` (also on arrows, methods, `init`,
`async fn`, `fn*`). The default is evaluated at **call time**, left-to-right, and may
reference **earlier already-bound parameters** (and outer scope). A typed default is
contract-checked. An explicit `nil` argument **suppresses** the default (only a *missing*
argument triggers it). A required parameter may **not** follow a defaulted one. Minimum
arity = the leading run of no-default params; defaults compose with rest (`...xs`).

```ascript
fn greet(name, greeting = "Hello", times = 1) {
  return `${greeting}, ${name}` + (times > 1 ? ` x${times}` : "")
}
print(greet("Ada"))                  // Hello, Ada
print(greet("Ada", "Hi", 3))         // Hi, Ada x3
```

**`#{…}` map literals** — build a `map` directly (see §4): the key is an expression, keys
may be any hashable value, `#{}` is empty, repeated keys are later-wins, an unhashable key
panics (Tier 2), and spread inside `#{}` is unsupported.

```ascript
let scores = #{ "alice": 10, "bob": 7 }
let mixed = #{ 1: "one", true: "yes", nil: "none" }
```

**`object.freeze` / `object.isFrozen`** (`std/object`, a core module) — `freeze(x)`
shallow-freezes a mutable container (object/array/map/set/instance) **in place** and
returns it for chaining; after that, any in-place mutation is a Tier-2 panic
`cannot mutate a frozen <kind>`. Freezing is shallow (nested containers stay mutable),
one-way, idempotent, and a no-op on non-containers. `isFrozen(x)` reports it; a
`deepClone` of a frozen value is a fresh, unfrozen copy. (Full reference: §11.)

```ascript
import * as object from "std/object"
let cfg = object.freeze({host: "localhost", port: 8080})
print(object.isFrozen(cfg))   // true
// cfg.port = 9090            // would panic: cannot mutate a frozen object
```

**Records — auto-derived `init`** — a class that declares fields but writes **no `init`**
automatically gets a **positional constructor** over its fields, in declaration order
(base-class fields first under inheritance). A defaulted field becomes an optional trailing
parameter; each positional argument is contract-checked against its field's type. A class
that *does* declare `init` is unchanged. Arity is min = required fields, max = total fields.

```ascript
class Point { x: number; y: number = 0 }
let p = Point(3)          // y takes its default
print(p.x)                // 3
print(p.y)                // 0
```

> Field defaults (§8.1) may be **any expression**, including an inclusive range `1..=3`
> (eagerly `[1, 2, 3]`); only `yield` is rejected as a field default. See §8.1.

---

## 9. Module System — ESM-style

- `export` declarations: `export fn f() {}`, `export const X = 1`,
  `export class C {}`.
- Named imports: `import { f, X } from "./util"`.
- Namespace import: `import * as util from "./util"`.
- **No default exports** (keeps resolution trivial and unambiguous).
- One file = one module. Paths are resolved relative to the importing file;
  `std/*` paths resolve to built-in modules.
- Each module is **evaluated once** and cached; circular imports resolve to the
  partially-initialized module (a load-order error if a binding is used before init).

---

## 10. Developer Tooling (shipped in the `ascript` binary)

The single `ascript` binary is multi-command (`clap`-based CLI):

| Command | Purpose | Backing crate(s) |
|---|---|---|
| `ascript run file.as` | execute a program | — |
| `ascript repl` | interactive shell: history, line editing, multi-line input | `reedline`/`rustyline` |
| `ascript fmt [paths]` | canonical code formatter (idempotent) | own pretty-printer |
| `ascript test [paths]` | built-in test runner with assertions, runs under `recover()` | own + `recover()` |
| `ascript lsp` | Language Server: completion, hover, go-to-def, inline diagnostics | `tower-lsp` |

**Rich diagnostics** are a cross-cutting feature, not a command: every error
(lex/parse/contract/panic) renders as a colored, source-pointing message with
carets, spans, and hints, using the spans threaded through the pipeline.

- Backing crate: `ariadne` or `miette`.
- This is the highest-leverage DX investment and is required for v1.

The LSP and `fmt` reuse the same lexer/parser/AST as the interpreter — no second
front-end.

### 10.1 Tree-sitter Grammar

AScript ships an official **Tree-sitter grammar** as a first-class spec artifact:

- `grammar/tree-sitter-ascript/grammar.js` — the grammar definition.
- `grammar/tree-sitter-ascript/queries/highlights.scm` — syntax-highlighting
  queries using standard capture names (`@keyword`, `@function`, `@type`, …).
- (Future) `queries/locals.scm`, `queries/folds.scm`, `queries/indents.scm`.

**Why a second parser?** This grammar is *separate from* the interpreter's
recursive-descent parser (§12) by design. Tree-sitter parsers are **error-tolerant
and incremental** — they keep producing a usable syntax tree mid-edit, even with
syntax errors — which is exactly what editors and the LSP need. The interpreter's
parser instead optimizes for precise diagnostics on complete programs. The grammar
is the **single source of truth for syntax** and MUST stay in lockstep with the
grammar sketch in §3 and the lexical rules in §2.

**Consumers:**

- Editors with native Tree-sitter support (Neovim, Helix, Zed) — highlighting,
  structural selection, code folding.
- The AScript LSP (§10) — semantic tokens and structural navigation can be derived
  from the Tree-sitter tree, avoiding a bespoke editor parser.
- `ascript fmt` may optionally use the CST for layout-preserving formatting.

**Known follow-ups (documented in the grammar header):**

- Faithful newline-sensitive automatic semicolon insertion (ASI-lite, §2) needs an
  external scanner (`scanner.c`); the committed grammar approximates it with
  optional `;` terminators.
- A few grammar ambiguities (arrow-fn vs. parenthesized expr, object literal vs.
  block) are handled via `conflicts`/precedence and may need tuning at
  `tree-sitter generate` time.

A **conformance test** keeps the two parsers honest: the golden `.as` corpus (§13)
is parsed by both the interpreter and the generated Tree-sitter parser, and any file
that one accepts but the other rejects fails CI.

---

## 11. Standard Library

### 11.1 Always-global core

`print`, `len`, `type`, `assert`, `range`, `Ok`, `Err`, `recover`.

### 11.2 Importable modules

Legend: ⚡ = async (returns awaitables); ⬡ = backed by a Rust crate.

**Data & text**

| Module | Contents | Backing |
|---|---|---|
| `std/string` | split, join, slice, trim, case, find, replace, format, pad, repeat | — |
| `std/array` | map, filter, reduce, push, pop, slice, sort, contains, get | — |
| `std/object` | keys, values, entries, has, delete, merge | — |
| `std/map` | new, get, set, has, delete, keys, values, entries | — |
| `std/math` | abs, floor, ceil, round, sqrt, pow, min, max, random, pi, e | — |
| `std/convert` | parseNumber, parseInt, toString, coercions | — |
| `std/regex` | compile, test, find, findAll, replace, split | ⬡ `regex` |
| `std/json` | parse, stringify | ⬡ `serde_json` |
| `std/csv` | parse, stringify | ⬡ `csv` |
| `std/toml` | parse, stringify | ⬡ `toml` |
| `std/yaml` | parse, stringify | ⬡ `serde_yaml` |
| `std/encoding` | base64, hex, url-encode/decode, utf8↔bytes | ⬡ `base64`, `hex` |
| `std/bytes` | buffer type, read/write ints, endian handling | — |
| `std/uuid` | v4 (random), v7 (time-ordered) | ⬡ `uuid` |
| `std/log` | leveled structured logging — debug/info/warn/error, setLevel/setFormat (human/json), field merge, lazy thunks (§11.6) | — (reuses `std/json`) |

**Time & locale**

| Module | Contents | Backing |
|---|---|---|
| `std/time` | now, monotonic, sleep ⚡, durations | ⬡ `tokio` (sleep) |
| `std/date` | civil dates, parse/format, arithmetic, timezones | ⬡ `chrono`/`time` |
| `std/intl` | locale-aware number/currency/date formatting, case folding, basic collation (pragmatic subset of ICU) | ⬡ trimmed `icu4x` |

**System & data stores**

| Module | Contents | Backing |
|---|---|---|
| `std/fs` | read/write/append, exists, stat, mkdir, remove, walk dir, path manipulation, **grep** (recursive content search) | ⬡ `walkdir`, `grep`/`regex`, `ignore` |
| `std/process` | `run` (one-shot capture) + `spawn` (streaming handle), stdin/stdout/stderr, exit code, signals, cwd/env, timeout — cross-platform (§11.4) | ⬡ `tokio` process |
| `std/env` | get/set env vars, dotenv loading | ⬡ `dotenvy` |
| `std/crypto` | sha256/sha512, md5, hmac, random bytes, argon2/bcrypt password hashing | ⬡ RustCrypto / `ring` |
| `std/compress` | gzip/deflate compress & decompress, zip read/write | ⬡ `flate2`, `zip` |
| `std/sqlite` | open, exec, query, prepared statements, transactions | ⬡ `rusqlite` |

**Networking & servers** (all ⚡)

| Module | Contents | Backing |
|---|---|---|
| `std/net/tcp` | listener + stream (connect, read, write) | ⬡ `tokio` |
| `std/net/http` | modern client: methods, headers, query, JSON/form/multipart/streaming bodies, redirects, decompression, timeouts, retries, cookies, TLS, proxy, HTTP/1.1+2+3, **streaming response bodies**, and **Server-Sent Events** (`http.sse`) — full surface in §11.5 | ⬡ `reqwest` (+ `h3`/`quinn`) |
| `std/http/server` | routes, handlers, middleware, params | ⬡ `hyper` |
| `std/net/ws` | WebSocket client + server | ⬡ `tokio-tungstenite` |

**Terminal UI**

| Module | Contents | Backing |
|---|---|---|
| `std/tui` | raw mode, alt screen, screen buffer, key/mouse events, basic widgets & drawing | ⬡ `crossterm`/`ratatui` |

### 11.3 Stdlib design rules

- Networking/server/timer modules return awaitables and ride the §7 event loop.
- Fallible stdlib functions follow the Tier-1 Result convention (`[value, err]`);
  misuse (wrong arg type) is a Tier-2 panic via the contract system.
- Native (Rust-implemented) functions are exposed as ordinary `function` values
  so they are indistinguishable from user functions at the call site.
- `fs.grep(pattern, dir, opts?)` performs a recursive content search and returns
  `[matches, err]`, where each match is `{ path, line, column, text }`. `pattern`
  is a `std/regex` pattern; `opts` may set `glob` (filename filter), `ignoreCase`,
  `maxResults`, and `respectGitignore` (default `true`, via the `ignore` crate).
  It reuses the regex engine and the directory walker rather than introducing a new
  search stack.

### 11.4 Subprocesses (`std/process`)

Cross-platform (Linux / macOS / Windows) process execution, built on
`tokio::process` so it rides the §7 event loop. Two entry points share one options
object: `run` for one-shot capture (the ffmpeg case) and `spawn` for live,
long-running, or interactive processes.

**Portable by default:** a program plus an explicit **argument list** is passed
straight to the OS — no shell, no word-splitting, no quoting differences. A shell is
strictly opt-in.

#### One-shot: `run`

```ascript
import { run } from "std/process"
import * as bytes from "std/bytes"

async fn transcode(): Result<object> {
  // ffmpeg writes MP3 to stdout (pipe:1); capture it as raw bytes.
  let result = await run("ffmpeg",
    ["-i", "input.mp4", "-f", "mp3", "pipe:1"],
    { capture: "bytes", cwd: "/tmp" })?

  if (!result.success) {
    return Err(`ffmpeg exited ${result.code}: ${result.stderrText}`)
  }
  return Ok(result.stdout)   // bytes buffer
}
```

`run` resolves to a Tier-1 Result `[result, err]`. **Spawn failure** (binary not
found, permission denied, timeout) is the `err`. A **non-zero exit is NOT an error**
— it comes back as a normal `result` with `success == false` (pass `check: true` to
flip non-zero into an `err`). The `result` object:

| Field | Type | Meaning |
|---|---|---|
| `stdout` | `string` or `bytes` | captured stdout (kind set by `capture`) |
| `stderr` | `string` or `bytes` | captured stderr |
| `stderrText` | `string` | stderr decoded lossy-UTF-8 (always present, for messages) |
| `code` | `number \| nil` | exit code; `nil` if terminated by a signal |
| `signal` | `string \| nil` | terminating signal name on unix (e.g. `"SIGKILL"`); `nil` on Windows |
| `success` | `bool` | `code == 0` |

#### Options (shared by `run` and `spawn`)

| Option | Default | Notes |
|---|---|---|
| `cwd` | inherit | working directory for the child |
| `env` | inherit | object merged onto the parent env; `null` a key to unset |
| `clearEnv` | `false` | start from an empty environment instead of inheriting |
| `stdin` | none | `string` or `bytes` written to the child's stdin then closed (`run` only) |
| `capture` | `"string"` | `"string"` (lossy UTF-8) · `"bytes"` (raw — use for ffmpeg/binary) · `"inherit"` (child shares our stdio) · `"null"` (discard) |
| `shell` | `false` | run via `/bin/sh -c` (unix) or `cmd.exe /C` (Windows). Convenience only — the command string is **not portable**; prefer argv |
| `timeout` | none | milliseconds; on expiry the child is killed and `run` returns a `timeout` `err` |
| `check` | `false` | when `true`, a non-zero exit becomes a Tier-1 `err` |

#### Streaming / interactive: `spawn`

```ascript
import { spawn } from "std/process"

async fn tailLogs(): Result<nil> {
  let child = spawn("tail", ["-f", "/var/log/app.log"], { capture: "string" })?

  let line = await child.stdout.readLine()
  while (line != nil) {
    print(line)
    if (shouldStop()) { child.kill("TERM") ; break }
    line = await child.stdout.readLine()
  }
  let status = await child.wait()      // { code, signal, success }
  return Ok(nil)
}
```

`spawn` returns a Tier-1 Result whose value is a **child handle**:

| Member | Kind | Description |
|---|---|---|
| `pid` | `number` | OS process id |
| `stdin` | writer | `await child.stdin.write(data)`, `child.stdin.close()` (data is `string`/`bytes`) |
| `stdout` / `stderr` | async reader | `await r.read(n?)` → next chunk or `nil` at EOF; `await r.readLine()`; `await r.readToEnd()`. Chunk type matches `capture` |
| `wait()` | async | resolves to `{ code, signal, success }` when the child exits |
| `kill(sig?)` | sync | terminate; `sig` defaults to `"KILL"` (see signals below) |

#### Cross-platform behavior (explicit)

- **Executable lookup** uses the OS `PATH`. On Windows the usual extensions
  (`.exe`, `.com`) resolve automatically. **`.bat`/`.cmd` scripts on Windows are run
  through the shell** (a documented consequence of Windows batch handling), so use
  `shell: true` for them.
- **Signals:** `kill()` / `kill("KILL")` is forceful everywhere (SIGKILL on unix,
  `TerminateProcess` on Windows). `kill("TERM")`, `"INT"`, `"HUP"` send the
  corresponding POSIX signal on unix; **Windows has no POSIX signals**, so any
  non-KILL signal maps to forceful termination with a documented caveat.
- **Exit codes:** unix exit codes are `0..255`; a process killed by a signal reports
  `code == nil` and a non-nil `signal`. Windows exit codes may be larger and
  `signal` is always `nil`.
- **Output bytes are returned raw** — `capture: "string"` decodes UTF-8 lossily but
  does **not** normalize `\r\n`; the caller decides. Use `capture: "bytes"` for any
  binary payload (images, audio, video) to avoid lossy decoding — this is why the
  ffmpeg example pipes to `bytes`.
- **Shell quoting** differs between `/bin/sh` and `cmd.exe`; `shell: true` is
  therefore inherently non-portable and flagged as such in diagnostics/docs.

The API surface is portable; **portability of the commands themselves is the user's
responsibility** (e.g. a `bash` script won't run on a stock Windows host). The spec
documents this boundary rather than hiding it.

### 11.5 HTTP client (`std/net/http`)

A modern async HTTP client riding the §7 event loop. **Backing crate: `reqwest`**
(async) — chosen because it bundles the entire modern feature set (connection
pooling, redirects with policy, transparent decompression, cookies, proxy, TLS,
multipart, HTTP/2) on top of `hyper`/`h2`, and adds HTTP/3 via `quinn`/`h3` behind a
feature flag. Re-implementing pooling/redirects/decompression/retries/cookies/h2/h3
by hand on raw `hyper` would be an enormous, error-prone surface; `reqwest` is the
de-facto modern Rust client and the right batteries-included choice. (`std/http/server`
uses `hyper` directly; the client uses `reqwest`.)

#### Entry points

```ascript
import { get, post, request, sse } from "std/net/http"

// Convenience verbs (get/post/put/patch/delete/head/options) and a general `request`.
async fn fetchUser(id: number): Result<object> {
  let resp = await get(`https://api.example.com/users/${id}`, { timeout: { total: 5000 } })?
  if (!resp.ok) { return Err(`HTTP ${resp.status}`) }
  return Ok(await resp.json()?)
}
```

All client calls are async and return a Tier-1 Result `[resp, err]` (a connect/TLS/
timeout/DNS failure is the `err`; a non-2xx *response* is a normal `resp` with
`ok == false` — it is NOT an error unless `opts.errorOnStatus` is set).

#### Request options (`request(opts)` / per-verb `opts`)

| Option | Meaning |
|---|---|
| `method` | verb (set by the convenience functions) |
| `url` / `query` | URL; `query` is an object merged as the query string |
| `headers` | object of custom headers; `auth: { bearer }` / `{ basic: [user, pass] }` helpers |
| `body` | one of: `string` · `bytes` · `{ json: value }` · `{ form: object }` (urlencoded) · `{ multipart: [...] }` (form-data, incl. file parts) · `{ stream: source }` (streamed request body, see below) |
| `timeout` | `{ connect, read, total }` in ms (any subset) |
| `redirect` | `{ follow: bool, max: number }` or `"none"` — redirect policy (default: follow, max 10) |
| `retry` | `{ max, backoff: "exponential"\|"constant", baseDelay, retryOn: [statuses] }` — retries with backoff on connection errors + idempotent methods (default: off) |
| `decompress` | gzip/deflate/brotli/zstd auto-decoding (default `true`; sets `Accept-Encoding`, decodes transparently) |
| `tls` | `{ caBundle, clientCert, minVersion, sni, insecure }` (custom CA, client certs, min TLS version, SNI override; `insecure` disables verification — flagged) |
| `cookies` | `true` (per-client cookie jar) or a shared jar handle; sends/stores cookies across redirects + reuse |
| `proxy` | `"http://…"` / `"socks5://…"` / `"system"` (env-based) / `"none"` |
| `httpVersion` | `"auto"` (default, ALPN-negotiated) · `"1.1"` · `"2"` · `"3"` — pin the protocol |
| `stream` | `true` → return a streaming response body instead of buffering (see below) |
| `errorOnStatus` | when `true`, a non-2xx status becomes a Tier-1 `err` instead of a normal `resp` |
| `cancel` | a cancellation handle (see Cancellation) |

#### Response object

`{ status, ok (200–299), headers, version ("1.1"|"2"|"3"), url (final, post-redirect), cookies }`
plus body accessors:
- **Buffered** (default): `await resp.text()` · `await resp.bytes()` · `await resp.json()` — each a Result.
- **Streaming** (when `opts.stream: true`): `resp.body` is an async reader following the
  exact `std/process` idiom — `await resp.body.read(n?)` (next chunk, or `nil` at EOF),
  `await resp.body.readLine()`, `await resp.body.readToEnd()`; chunk type is `string`
  (UTF-8) or `bytes` per `opts.bodyMode` (`"string"` default · `"bytes"`).
- `resp.trailers` exposes HTTP/2 response trailers when present (see Deferrals).

#### Streaming bodies (chunked / streamable HTTP)

- **Response streaming:** `opts.stream: true` pulls the body incrementally via the reader
  above — no full buffering. **Backpressure-aware:** each `read`/`readLine` awaits the
  next chunk on the Tokio loop; the client does not over-read ahead of the consumer
  (`reqwest::Response::chunk()` yields as the consumer pulls), so a slow consumer
  naturally slows the transfer on the single-threaded event loop.
- **Request streaming:** `body: { stream: source }` where `source` is a `bytes` value, a
  `std/process`/file reader, or an async generator function `() => Result<bytes>` returning
  the next chunk (or `nil` to end). Sent as a chunked/streamed request body without
  buffering; writes await drain, so a slow upstream applies backpressure.

#### Server-Sent Events (`http.sse`)

A **first-class SSE client** as a dedicated entry point (NOT a flag on `request`) — SSE
has distinct framing/reconnect semantics that warrant their own type:

```ascript
let events = await sse("https://api.example.com/stream", { headers: { auth: { bearer: tok } } })?
let ev = await events.next()
while (ev != nil) {
  print(`${ev.event}: ${ev.data}`)   // ev = { event, data, id, retry }
  ev = await events.next()
}
```

`sse(url, opts)` returns `[stream, err]`. The stream:
- requests `Accept: text/event-stream` and consumes the response body as a stream.
- parses the SSE wire format: `event:` / `data:` / `id:` / `retry:` fields; **dispatches a
  buffered event on each blank-line boundary**; concatenates **multi-line `data:`** with
  `\n`; lines beginning `:` are comments (ignored).
- `await stream.next()` → `[{ event (default "message"), data, id, retry }, err]`, or `nil`
  when the stream ends and is not reconnecting.
- `stream.lastEventId` — the most recent `id:` seen; sent as `Last-Event-ID` on reconnect.
- **Auto-reconnect** (default on; `opts.reconnect: false` to disable): on disconnect, waits
  the server-provided `retry:` interval (or `opts.retryDefault`, default 3000 ms), reconnects
  with `Last-Event-ID`, and resumes `next()`. `opts.maxReconnects` caps attempts.
- `stream.close()` cancels the connection and ends the stream.

#### Protocol versions

HTTP/1.1 and HTTP/2 are always available (ALPN-negotiated over TLS, default `"auto"`).
**HTTP/3 (QUIC)** is supported via reqwest's `http3` feature (built on `quinn` + `h3`);
`httpVersion: "3"` pins it. `resp.version` reports the negotiated version (`"1.1"`/`"2"`/
`"3"`) so callers can assert or log it.

#### Full feature set (enumerated)

Methods (GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS) · custom headers · query params ·
basic/bearer auth helpers · JSON / urlencoded-form / multipart-form-data / raw bytes /
streamed request bodies · buffered & streaming response bodies · keep-alive +
connection pooling (default) · redirects with policy · gzip/deflate/brotli/zstd
decompression · connect/read/total timeouts · retries with backoff · cancellation · TLS
config (custom CA, client certs, min version, SNI, insecure) · cookie jar · proxy
(http/https/socks/system) · HTTP/1.1 + HTTP/2 + HTTP/3 with ALPN + version pin/report ·
SSE client · response trailers.

#### Deferrals (justified, with owning milestone)

| Deferred / best-effort | Why | Owner |
|---|---|---|
| **HTTP/3 (QUIC)** | `reqwest`'s `http3` is feature-gated + requires an unstable cfg and `quinn`; it works but is less battle-tested than h1/h2. Ships **behind a Cargo feature** (`http3`), default-off, with `httpVersion: "3"` opt-in. `"auto"`/`"1.1"`/`"2"` are the always-on baseline. | **M14** (feature-gated; promote to default when upstream stabilizes) |
| **Response trailers** | `reqwest`'s high-level API does not surface HTTP/2 trailers directly; reading them needs a `hyper`-level path. `resp.trailers` is **best-effort** — populated when the backend exposes them, else empty. | **M14** follow-up (drop to `hyper` if first-class trailers are required) |
| **SOCKS proxy** | Behind reqwest's `socks` feature. | **M14** (Cargo feature `socks`, default-on if it compiles cleanly cross-platform) |

Everything else in the enumerated set is in-scope for **M14 (Async I/O)** and on by
default.

### 11.6 Structured logging (`std/log`)

`std/log` is leveled, structured logging. Records emit to **stderr** under the CLI
`run` command (the `Live` output sink, §12.1) — keeping `print`'s stdout clean — and
buffer separately under `Capture` for tests.

```ascript
import * as log from "std/log"

log.setLevel("debug")                                  // default "info"
log.info("request", {method: "GET", path: "/users", ms: 12})
log.warn("slow query", {ms: 540})

log.setFormat("json")                                  // "human" (default) | "json"
log.info("saved", {userId: 7, ok: true})
log.debug(() => expensiveDetail())                     // thunk: only run if level passes
```

- **Levels:** `debug` < `info` < `warn` < `error`. `setLevel(name)` sets the
  threshold; calls below it are dropped. The **initial level** is read from the
  `ASCRIPT_LOG` environment variable (`debug`/`info`/`warn`/`error`), defaulting to
  `info`.
- **Record shape:** each `debug/info/warn/error(...)` call builds a record with a
  `level` and a `msg` plus arbitrary fields. The **first argument**, if not an object,
  becomes `msg`; subsequent **object** arguments are merged as fields; other
  non-object args also fold into `msg`. The reserved keys `level` and `msg` are
  authoritative — a user field of the same name can never clobber them.
- **Lazy thunk:** if the **first** argument is a function, it is the message and is
  invoked **only when the level passes** (after the filter), so a filtered
  `log.debug(() => …)` does no work. An `async fn` thunk is awaited.
- **Formats:** `setFormat("human")` (default) renders `[LEVEL] msg key=value …`;
  `setFormat("json")` emits one JSON object per line (JSON-lines).
- **Total serialization:** field values are serialized with a **lossy, never-panicking**
  `Value`→JSON conversion — cycles become `"[Circular]"`, functions `"<function>"`,
  `NaN`→`null` — so logging can never itself crash the program.

`std/log` is gated by a default-on **`log` Cargo feature** (which depends on `data`,
for the JSON serializer).

---

## 12. Runtime Architecture (Rust)

### 12.1 Pipeline

```
source(.as)
  → lexer      → tokens (with spans)
  → parser     → AST   (with spans)
  → resolver   → scope + module binding, ?-validity, const checks
  → interp     → async tree-walking evaluator on the Tokio executor
```

**`print` output sink.** Where `print` writes is chosen by the host via an
`OutputSink`:

- **`Live`** — used by the CLI `run` command. Each `print` is written straight to
  `stdout` as it executes, so a long-running program streams its output live (and any
  output already produced **survives a later panic** instead of being lost with a
  buffered string). `run_file` returns `Result<(), AsError>` — it owns no captured
  output.
- **`Capture`** — used by `run_source`, the REPL, and the test runner. Output is
  buffered into a string the host reads back, so embedders and tests can assert on it
  deterministically.

`std/log` records follow the same split: under `Live` they go to **stderr** (keeping
`print` on stdout clean); under `Capture` they buffer separately for tests.

**REPL session & multi-line input.** The REPL holds one `Interp` + one session
`Environment` for the whole session, so definitions (`let`/`fn`/`import`/classes)
**persist across lines**. Input that is *incomplete* — unclosed `{`/`(`/`[` (counted as
delimiter *tokens*, so `${…}` template braces never skew the count) or an unterminated
string/template at EOF — is buffered on a `..` continuation prompt until it parses, then
executed as one statement; a genuine balanced-but-wrong line is reported immediately.
Ctrl-C cancels a partial buffer; Ctrl-D ends the session.

### 12.2 Crate/module layout (workspace)

| Module / crate | Responsibility |
|---|---|
| `ascript-lexer` | tokenization, source spans |
| `ascript-ast` | AST node definitions |
| `ascript-parser` | recursive-descent / precedence-climbing parser |
| `ascript-resolver` | lexical scoping, module graph, static validations |
| `ascript-value` | `Value` enum, `Rc<RefCell<…>>` heap, equality/truthiness |
| `ascript-interp` | `async fn eval`, environments, call stack, panic/Result machinery |
| `ascript-stdlib` | all `std/*` modules (feature-gated by group) |
| `ascript-diagnostics` | span → rendered error (ariadne/miette) |
| `ascript-lsp` | language server (tower-lsp) over the shared front-end |
| `ascript-cli` | the `ascript` binary: run/repl/fmt/test/lsp |

### 12.3 Value representation (sketch)

```rust
enum Value {
    Nil,
    Bool(bool),
    Number(f64),
    Str(Rc<str>),
    Array(Rc<RefCell<Vec<Value>>>),
    Object(Rc<RefCell<Object>>),   // Object carries optional class tag
    Map(Rc<RefCell<HashMap<MapKey, Value>>>),
    Function(Rc<Function>),        // user closure or native fn, sync or async
}
```

Single-threaded ⇒ `Rc`/`RefCell`, never `Arc`/`Mutex`.

### 12.4 Dependency feature gating

Stdlib groups are Cargo features (`data`, `net`, `sql`, `tui`, `intl`, `crypto`,
`log` — the last depends on `data`) so an embedder can build a smaller `ascript`
without, say, SQLite or TUI.

---

## 13. Testing Strategy

- **Unit tests** per pipeline stage (lexer, parser, resolver) on small inputs.
- **Golden-file end-to-end suite:** each `examples/*.as` is paired with an expected
  `stdout` (and expected diagnostic for error cases); the runner executes and diffs.
- **Tier tests:** assert that recoverable failures produce Results and that
  programmer bugs panic with the correct diagnostic.
- **Stdlib tests:** written *in AScript* using the built-in `ascript test` runner,
  dogfooding the language.
- **Async tests:** drive timers/sockets/HTTP against in-process loopback servers.

---

## 14. Phased Build Plan

The stdlib is too large for one effort; the spec is implemented in tractable,
independently-testable phases (each gets its own implementation plan):

1. **Core language + interpreter** — lexer, parser, resolver, async tree-walker,
   value model, contracts, Result/`?`, panic tier, classes, enums, `match`,
   modules. Plus `ascript run`, rich diagnostics, REPL, and the **Tree-sitter
   grammar** (§10.1) authored alongside the syntax with a two-parser conformance
   test. *(Minimum viable language.)*
2. **Data & text stdlib** — string, array, object, map, math, convert, regex,
   json, csv, toml, yaml, encoding, bytes, uuid, time, date, intl. Plus
   `ascript fmt` and `ascript test`.
3. **System & data stores** — fs, process, env, crypto, compress, sqlite.
4. **Async I/O stack** — net/tcp → net/http (client) → http/server → net/ws,
   built in that dependency order.
5. **TUI** — std/tui.
6. **LSP** — `ascript lsp` over the shared front-end.

Phase 1 delivers a usable language; each later phase adds a coherent capability
band without touching the core.

---

## 15. Open Questions (to revisit, not blocking v1)

- Package manager / dependency resolution (deferred to a separate spec).
- Whether `map` literals get dedicated syntax or stay constructor-only.
- Source-map / debugger protocol support.
- Exact `icu4x` subset boundary for `std/intl`.
- **"AScript Media" (future spec):** audio (playback/synthesis via `rodio`/`cpal`)
  and 2D/3D graphics + windowing (via `macroquad`/`pixels`/`wgpu`). Must resolve the
  main-thread event-loop vs. Tokio-loop interaction before adoption — likely a
  distinct runtime mode rather than plain stdlib modules.

---

## 16. Example Program (tying it together)

```ascript
import { get } from "std/net/http"
import * as json from "std/json"

class WeatherClient {
  fn init(base: string) { self.base = base }

  async fn current(city: string): Result<object> {
    let resp = await get(`${self.base}/weather?q=${city}`)?
    let data = json.parse(resp.body)?
    return Ok({ city: city, tempC: data.temp })
  }
}

async fn main() {
  const client = WeatherClient("https://api.example.com")
  let [w, err] = await client.current("Cairo")
  if (err != nil) {
    print("error: " + err.message)
    return
  }
  print(`${w.city}: ${w.tempC}°C`)
}

await main()
```
