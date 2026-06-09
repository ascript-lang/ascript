:::eyebrow Language

# Classes, enums, match

## Classes

AScript has classes with fields, methods, single inheritance, and a constructor named `init`. There
is no `new` keyword — the class itself is callable.

```ascript
class Animal {
  fn init(name) {
    self.name = name
  }
  fn speak() {
    return `${self.name} makes a sound`
  }
}

const a = Animal("Rex")    // calls init
print(a.speak())           // Rex makes a sound
```

- `self` is the receiver inside a method.
- `init` runs when you call the class; assign fields with `self.field = …`.
- Methods may be `async fn`.

### Generator methods

A method may be a generator — `fn*` or `async fn*`. Calling it returns a generator
bound to `self`, driven by `for await` / `.next()` / `gen.close()`, exactly like a standalone
generator. `self`, arguments, type contracts, inheritance, and `super` all work as for an ordinary
method.

```ascript
class Counter {
  fn init(start) { self.start = start }
  fn* upTo(n) {
    let i = self.start
    while (i <= n) {
      yield i
      i = i + 1
    }
  }
}

let c = Counter(3)
for await (v in c.upTo(6)) {
  print(v)   // 3 4 5 6
}
```

### Static methods & the async factory

A member declared `static fn name(...)` (also `static async fn` and `static fn*`) is a
**class-level** method, called as `C.name(args)` with **no `self`** / no instance. Static methods
live in a **separate namespace** from instance methods (an instance `c.x()` and a static `C.x()` may
share a name), are **inherited** up the superclass chain, and may construct instances or call other
statics. `super` is not valid inside a static (there is no instance/parent receiver).

```ascript
class Point {
  fn init(x, y) { self.x = x
    self.y = y }
  static fn origin() { return Point(0, 0) }   // sync factory
  fn sum() { return self.x + self.y }
}
print(Point.origin().sum())   // 0
```

Because construction is synchronous (`Point(...)` returns an instance, not a future), the blessed
pattern for **async construction** is a `static async fn create(...)` factory that returns a
`future` — `create` is a convention, not a keyword:

```ascript
class User {
  fn init(name) { self.name = name }
  static async fn load(id) {
    let u = User("?")
    u.name = await fetchName(id)   // do async work, then return the built instance
    return u
  }
}
let u = await User.load(42)
```

> An **`async fn init`** (or a generator **`fn* init`**) is forbidden — synchronous construction has
> no caller to `await` it, and a generator constructor makes no sense. Both are a clean compile-time
> error (*"init must be a synchronous constructor; use a static async factory (e.g. `static async fn
> create()`)"*) on either engine — use a `static async fn create()` factory instead. The name
> **`from`** is reserved on classes (it collides
> with the built-in typed-parse `ClassName.from`), so `static fn from` is an error.

### Typed fields

A class body may declare fields with [contract types](type-contracts) before its methods. There are
three kinds:

```ascript
class User {
  id: number              // required
  name: string            // required
  nickname: string?       // optional (nullable) — defaults to nil
  role: string = "guest"  // defaulted — applied at construction
  fn init(id, name) {
    self.id = id
    self.name = name
  }
}
```

- **Required** (`id: number`) — a declared type with no default.
- **Optional** (`nickname: string?`) — a [nullable](type-contracts) type; absent values become nil.
- **Defaulted** (`role: string = "guest"`) — the default is applied when the instance is built.

Declared field types are **checked on assignment**, including inside `init` — assigning a string to
`self.id` panics with a [type-contract](type-contracts) error. Defaults are applied at construction.
Fields you never declare stay fully **dynamic** (the gradual rule): you can still assign arbitrary
`self.whatever = …` without a declaration.

A field default can be **any expression** — including an [inclusive range](syntax#ranges) `1..=3`
(which eagerly materializes to `[1, 2, 3]`), a stepped range `0..=10 step 2`, a binary/ternary/
template expression, an arrow, or a `match`. The default is evaluated when the instance is built (and
again, identically, by `Class.from(obj)`):

```ascript
class Grid {
  cells: array<number> = 1..=3    // [1, 2, 3] at construction
  scale: number = 2
}
print(Grid().cells)               // [1, 2, 3]
```

The only field-default expression that is **rejected** is `yield` (it is never valid outside a
generator body) — both engines reject it symmetrically.

> [!NOTE] The optional field above can be spelled two ways — `nickname: string?` or the marker form
> `nickname?: string`. **Both lower to the same node** (`string | nil`); the formatter normalizes the
> marker form to the canonical `nickname: string?`.

### Records — an init-less class auto-derives a constructor

A class that declares fields but writes **no `init`** automatically gets a **positional constructor**
over its fields — no new keyword, "record-ness" is just *fields + no `init`*:

```ascript
class Point {
  x: number
  y: number
}
const p = Point(3, 4)   // auto-derived: binds x=3, y=4 positionally
print(p.x)              // 3
```

- The parameters are the declared fields **in declaration order** (and, with [inheritance](#inheritance),
  base-class fields come **first**).
- A **defaulted field becomes an optional trailing parameter** — omit it to take the default, or pass
  it to override:

  ```ascript
  class Config {
    host: string
    port: number = 8080
  }
  print(Config("localhost").port)        // 8080 (default)
  print(Config("localhost", 9000).port)  // 9000 (overridden)
  ```

- Each positional argument is **contract-checked** against its field's type, exactly like a
  hand-written `init` that assigns `self.f = arg` — a wrong type is a [type-contract](type-contracts)
  panic, and too few / too many arguments is the same arity panic as any function call.
- A class **with an explicit `init` is unchanged** — no auto-constructor is derived; your `init` runs
  as written. A class with no fields *and* no `init` keeps its zero-argument constructor.

> [!NOTE] The auto-derived constructor is synthesized at construction time from the class's declared
> fields, so it works identically whether you `ascript run`, `ascript build` + run the `.aso`, or use
> the `--tree-walker` engine; `ascript check` validates record-construction arity too.

### `ClassName.from` — validate a raw object into an instance

`ClassName.from(obj, strict = false)` turns an untrusted plain object (for example, the result of
[`json.parse`](../stdlib/data)) into a checked instance. It:

- validates every declared field against its type, building a **field path** (e.g.
  `user.address.zip`) for error messages;
- **recurses** into nested class fields, `array<Class>` elements, and `map<K, Class>` values —
  including an **Object→Map boundary coercion**, so a raw JSON dictionary `{ "home": {...} }`
  validates into a `map<string, Address>` field;
- applies field **defaults** and lets optional fields fall to nil;
- **does not** run `init` (it constructs a validated instance directly);
- on a shape mismatch, raises a **recoverable** [panic](errors) carrying the field path — wrap it in
  `recover` (or use `!`) to turn it into a Tier-1 result.

With `strict = true`, unknown keys in the source object are rejected; the default (`false`) ignores
them.

```ascript
class Address {
  street: string
  zip: number
}
class User {
  id: number
  name: string
  nickname: string?
  role: string = "guest"
  address: Address                    // nested class — recursively validated
  places: map<string, Address>        // JSON dictionary → map<string, Address>
}

let u = User.from({
  id: 1,
  name: "Ada",
  address: { street: "1 Lovelace Way", zip: 90210 },
  places: { home: { street: "1 Lovelace Way", zip: 90210 } },
})
// u.role == "guest" (default), u.nickname == nil, u.address.zip == 90210

// A shape mismatch is a recoverable panic carrying a field path.
let [user, err] = recover(() => User.from({ id: 1, name: "Bug", address: { street: "x", zip: "nope" } }))
// err.message points at address.zip
```

See [typed parse](../stdlib/data) for the fused `json.parse(text, User)` shortcut that combines
decoding and validation into one Tier-1 result.

### Inheritance

A class may `extend` one parent and call up with `super`:

```ascript
class Dog extends Animal {
  fn init(name, breed) {
    super.init(name)       // run the parent constructor
    self.breed = breed
  }
  fn speak() {
    return super.speak() + " — woof"   // call the parent method
  }
}

const d = Dog("Rex", "Husky")
print(d.speak())           // Rex makes a sound — woof
print(d.breed)             // Husky
```

Method resolution walks the inheritance chain from the instance's class upward. A class name is a
valid [contract type](type-contracts) that accepts the class and any subclass.

### `instanceof` — runtime class test

`x instanceof C` is a comparison-tier binary operator that returns a `bool`: `true` when `x` is an
instance of `C` **or any subclass of `C`** (the `extends` chain is walked), and `false` otherwise.

```ascript
class Animal {}
class Dog extends Animal {}

const d = Dog()
print(d instanceof Dog)        // true
print(d instanceof Animal)     // true  — a subclass instance IS-A parent
print(Animal() instanceof Dog) // false — but not the other way around
```

- A **non-instance** left operand — a number, string, `nil`, an object, an enum value — is always
  `false`, never an error: `print(5 instanceof Animal)` prints `false`.
- The **right operand must be a class or an [interface](#interfaces)**. Anything else
  (`x instanceof 5`, `x instanceof nil`) is a Tier-2 panic:
  `instanceof requires a class or interface on the right-hand side`.
- It binds at the relational tier (same as `< <= > >=`), looser than `+`/`-` and tighter than `&&`,
  so `a instanceof B && c` parses as `(a instanceof B) && c`.

## Interfaces

An **interface** is a named **set of method requirements** — a behavioral contract. A value
**conforms** to an interface if it structurally has those methods. Conformance is **structural**: a
class needs no declaration to satisfy an interface, conformance is retroactive (a class from another
module conforms automatically), and one value may conform to arbitrarily many interfaces at once. No
inheritance required.

```ascript
interface Reader {
  fn read(b): int
}

interface Writer {
  fn write(b): int
}

// File never names Reader, but it conforms — it has a matching `read`.
class File {
  fn read(b): int { return len(b) }
}

const f = File()
print(f instanceof Reader)   // true  — structural conformance
print(5 instanceof Reader)   // false — a number has no `read` method
```

A method **requirement** is `fn name(params): ret` with **no body**. Parameters and the return may be
typed (the types are documentation at runtime in v1; the static checker tightens them in a later
milestone). Requirements are separated by newlines or `;`. Generator/async/static/worker modifiers on
a requirement are rejected.

### `instanceof` against an interface

`v instanceof Reader` is a **structural conformance check**: `true` iff `v` is a class instance whose
class exposes every method the interface requires (by name and call-shape in v1). Only class instances
can conform — a bare object, an enum, `nil`, or a number is always `false`.

### Optional `implements` — asserted intent

A class may **assert** conformance with `implements`. It is documentation plus (in a later milestone)
a declaration-site checker guarantee; at runtime it stores no tag and changes nothing — `instanceof`
runs the same structural check whether or not `implements` is present.

```ascript
class Socket implements Reader, Writer {
  fn read(b): int { return len(b) }
  fn write(b): int { return len(b) }
}

print(Socket() instanceof Reader)   // true — exactly as if `implements` were absent
```

A class with a matching method set conforms **whether or not** it says `implements` — that is the
whole point of structural typing.

### Composition via `extends`

An interface may **extend** others to require the **union** of their method sets. Interface `extends`
is composition (distinct from a class's single-superclass `extends`, though the keyword is the same):

```ascript
interface ReadWriter extends Reader, Writer {}

const s = Socket()
print(s instanceof ReadWriter)   // true — Socket has both read and write
```

### Interface-typed annotations are runtime contracts

An interface name is valid anywhere a type is — a parameter, return, field, or `let` annotation. At
runtime it is a **contract**: a non-conforming value is rejected with the same Tier-2 panic a class
annotation gives.

```ascript
fn copy(src: Reader, dst: Writer): int {
  const n = src.read([1, 2, 3])
  return dst.write([1, 2, 3, n])
}
```

> [!NOTE] **Runtime now, static later.** The runtime conformance check is **permissive and
> structural** — it checks method name + call-shape (arity), not parameter or return *types*. Full
> *static* interface type-checking (proving a `Reader`-typed argument conforms at compile time, and a
> blocking `implements-violation` diagnostic when an `implements` clause is not satisfied) lands with
> the type-system milestone. This is the same gradual seam the language already has: runtime contracts
> are structural-and-permissive, the checker is strict-on-annotations.

> [!NOTE] **Deferred to a later milestone.** Default method *bodies*, required *fields* on an
> interface, and *generic* interfaces (`interface Iterator<T>`) are not in this version. An interface
> v1 is a behavioral method set only.

See the runnable examples [`examples/interfaces.as`](https://github.com/ascript-lang/ascript/blob/main/examples/interfaces.as)
and [`examples/advanced/interface_dispatch.as`](https://github.com/ascript-lang/ascript/blob/main/examples/advanced/interface_dispatch.as).

## Enums

An enum is a **closed sum of named variants**. A variant is one of three shapes: a **unit** variant
(payload-less, optionally with a backing number/string), a **positional-payload** variant, or a
**named-payload** variant.

```ascript
enum Color  { Red, Green, Blue }                    // unit variants
enum Status { Ok = 200, NotFound = 404, Err = 500 } // number-backed units
enum Mode   { Read = "r", Write = "w" }             // string-backed units
```

Access a unit variant with `Enum.Variant`. Each exposes its `.name` and `.value`:

```ascript
print(Status.NotFound)         // Status.NotFound
print(Status.NotFound.value)   // 404
print(Status.NotFound.name)    // NotFound
print(Color.Red.value)         // nil — a bare unit variant has no backing value
```

Unit variants are interned singletons, so identity comparison just works. A variant never equals a
variant of another enum, nor its own raw backing value:

```ascript
Color.Red == Color.Red    // true
Status.Ok == 200          // false — a variant is not its backing number
```

### Algebraic enums — variants with typed payloads

A variant can carry **typed data**. Fields are either all **named** (`Circle(radius: float)`) or
all **positional** (`Pair(int, int)`) — mixing the two in one variant is a parse error. A field type
is required.

```ascript
enum Shape {
  Circle(radius: float),      // named payload (single field)
  Rect(w: float, h: float),   // named payload (multiple fields)
  Pair(int, int),             // positional payload
  Point,                      // unit variant (payload-less)
}
```

A payload variant is a **constructor**: referencing `Shape.Circle` without calling it yields a
first-class callable value; calling it validates the payload (arity + field types, the same engine
`Class.from` uses) and produces a constructed variant.

```ascript
let c = Shape.Circle(2.0)
print(c)          // Shape.Circle(radius: 2.0)
print(c.name)     // Circle
print(c.value)    // {radius: 2.0} — named payload reflects as an Object
print(c.radius)   // 2.0 — named fields are also readable directly

let p = Shape.Pair(3, 4)
print(p)          // Shape.Pair(3, 4)
print(p.value)    // [3, 4] — positional payload reflects as an Array
```

A **multi-field named** variant must be constructed with named arguments (`Shape.Rect(w: 3.0, h:
4.0)`); a single named field also accepts a bare positional call (`Shape.Circle(2.0)`). A wrong
payload is a recoverable error naming the field path:

```ascript
let bad = recover(() => Shape.Circle("x"))
print(bad[1].message)   // Shape.Circle.radius: expected float, got string
```

Because a constructor is an ordinary value, it composes — e.g. as the mapping function over an array:

```ascript
import * as array from "std/array"
let circles = array.map([1.0, 2.0, 3.0], Shape.Circle)
print(circles[1].radius)   // 2.0
```

Unlike unit variants (identity-equal), **constructed payload variants compare structurally** — equal
enum, equal variant name, equal payload:

```ascript
print(Shape.Circle(2.0) == Shape.Circle(2.0))   // true
print(Shape.Circle(2.0) == Shape.Circle(3.0))   // false
```

> [!NOTE] A payload that holds an Array/Object compares that container by AScript's container rule
> (by identity), so two `Pair`-of-distinct-arrays are not equal even with equal elements; scalar
> payloads compare by value.

### Recursive enums

A variant payload may reference the enum itself, so enums model trees directly:

```ascript
import * as array from "std/array"
import * as string from "std/string"

enum Json {
  Null,
  Bool(value: bool),
  Num(value: float),
  Str(value: string),
  Arr(items: array<Json>),    // recursive — payload references the enum itself
}

fn render(j: Json): string {
  return match j {
    Json.Null => "null",
    Bool(b) => `${b}`,
    Num(n) => `${n}`,
    Str(s) => `"${s}"`,
    Arr(xs) => "[" + string.join(array.map(xs, render), ",") + "]",
  }
}

print(render(Json.Arr([Json.Num(1.0), Json.Bool(true), Json.Str("hi")])))
// [1.0,true,"hi"]
```

Recursive payloads can form cycles; they are cycle-collected like any other container.

### Typed errors

A `Result` in AScript is the `[value, err]` pair. With algebraic enums the **error slot becomes a
typed sum** — strictly better than a bare string. `?` / `!` are unchanged (they inspect the pair
*shape*, not the error's kind), and the caller `match`es the error enum **exhaustively** (see below)
so no error case is silently dropped:

```ascript
enum DbError { NotFound(key: string), Timeout(ms: int), Conn(detail: string) }

fn explain(e: DbError): string {
  return match e {
    NotFound(key) => `no such key: ${key}`,
    Timeout(ms) => `timed out after ${ms}ms`,
    Conn(detail) => `connection failed: ${detail}`,
  }
}

print(explain(DbError.NotFound("ada")))   // no such key: ada
```

See `examples/advanced/typed_errors.as` for the full `[value, err]` + `?` flow.

## Match

`match` is an **expression**: it evaluates to a value and slots directly into a `return`, a `let`,
or any larger expression — no temporary mutable variable required.

Arms are tried top to bottom; the first match wins. Each arm is:

```
pattern (| pattern)* (if guard)? => body
```

### Pattern forms

#### Wildcard `_`

Matches anything and binds nothing. Usually the last arm.

```ascript
match x { 0 => "zero", _ => "other" }
```

#### Value patterns

A literal (`0`, `"admin"`, `true`) or any expression (enum reference, member access, arithmetic) is
evaluated and compared with `==`.

```ascript
match status {
  Status.Ok      => "ok",
  Status.NotFound => "not found",
  _               => "other",
}
```

#### Bare identifiers — Option C (defined → compare, undefined → bind)

A bare identifier in a pattern is resolved at match time using **Option C**:

- **Defined in scope** (e.g. a `const`, a previously bound variable) → the identifier's value is
  looked up and compared with `==`. Behaves like a value pattern.
- **Undefined in scope** (a fresh name) → the subject is **captured** into that binding for the
  arm body. Behaves like a binding.

```ascript
const NOT_FOUND = 404    // defined in scope

fn label(status: number): string {
  return match status {
    NOT_FOUND => "not found",          // defined → compare (status == 404)
    code if code >= 500 => `error ${code}`,  // undefined → bind, then guard uses it
    other => `status ${other}`,       // undefined → bind as catch-all
  }
}
```

The distinction is determined at the match site, not the definition site — so a `const` imported
from another module is still a comparison, while any name not yet in scope becomes a fresh binding.

#### Ranges

`start..=end` (inclusive) and `start..end` (exclusive) match a numeric subject in the range.

```ascript
match n {
  1..=9   => "single digit",   // 1, 2, … 9
  10..100 => "double digit",   // 10, 11, … 99
  _       => "other",
}
```

A `step` makes the pattern a **strided membership** test: `start..end step k` matches a subject `x`
when `x` is in range *and* `(x − start)` is a whole multiple of `k` (the anchor is `start`):

```ascript
fn classify(n: number): string {
  return match n {
    1..=10 step 2 => "odd in 1..10",   // matches 1, 3, 5, 7, 9
    1..=10        => "even in 1..10",  // catches the rest in range
    _             => "out of range",
  }
}
```

#### Array patterns

`[p0, p1, …]` matches an array with exactly that many elements (unless a trailing rest is
present). Each position is itself a pattern and may bind, compare, or wildcard.

An optional `...name` rest at the end collects the remaining elements into a new array; `...`
(unnamed) ignores them.

```ascript
match xs {
  []              => "empty",
  [x]             => `one: ${x}`,           // binds x
  [first, ...rest] => `head ${first}, ${len(rest)} more`,  // binds first + rest
}
```

#### The `[value, err]` idiom

AScript's fallible calls return a `[value, err]` pair. Match on it directly:

```ascript
match pair {
  [v, nil] => `ok: ${v}`,   // nil error → success; v is bound
  [_, e]   => `err: ${e}`,  // any error → wildcard the value, bind the error
}
```

#### Object patterns

`{key, key2: subpat, …}` matches an object (or class instance) that contains the listed keys.

- **Shorthand `{key}`** — always **binds** `key` to the field value, regardless of Option C.
  Shorthand is unambiguously destructuring.
- **`{key: pattern}`** — matches the field value against a nested pattern (compare, bind, range,
  nested array/object, …).
- **`...rest`** — collects the unclaimed keys into a new object.

```ascript
match req {
  {method, path} => `${method} ${path}`,   // shorthand binds both
  _ => "?",
}

match user {
  {role: "admin"}      => "is admin",           // sub-pattern compare
  {role: r, name: n}   => `${r} — ${n}`,        // bind r and n
  {role: r}            => `role ${r}`,           // bind r; name not required
  _                    => "no role",
}

match event {
  {type: "click", x, y, ...extra} => `click at ${x},${y}`,  // rest in object
  {type: t}                       => `event ${t}`,
}
```

#### Alternatives `|`

Separate multiple patterns with `|`. The arm fires if **any** pattern matches. Alternatives are
typically literals or value patterns that bind nothing; only the matched alternative's bindings are
in scope in the guard/body, so keep alternatives uniform (bind the same names) if you do bind.

```ascript
match day { "sat" | "sun" => "weekend", _ => "weekday" }
match n    { 0 | 1        => "tiny",    _ => "bigger"  }
```

#### Guards `pattern if condition`

An `if` guard is evaluated **after** the structural match, with any bound names in scope. A falsy
guard rejects the arm and matching continues.

```ascript
match n {
  x if x < 0   => "negative",
  x if x == 0  => "zero",
  x             => `positive ${x}`,
}
```

Guards work with any pattern, including array and object patterns:

```ascript
match pair {
  [a, b] if a == b => "equal",
  [a, b]           => "different",
}
```

### Enums and match

`match` pairs naturally with enums:

```ascript
fn describe(c: Color): string {
  return match c {
    Color.Red   => "warm",
    Color.Green => "cool",
    Color.Blue  => "cool",
    _           => "unknown",
  }
}

print(describe(Color.Green))   // cool
```

Because a variant name like `Color.Red` is a member-access expression, it is always a **value
pattern** (evaluated and compared with `==`), regardless of Option C.

#### Variant patterns — destructuring payloads

A payload variant is matched with a **variant pattern** that destructures its payload — positional
by index, named by field (with optional rename), and freely nested or guarded:

```ascript
fn area(s: Shape): float {
  return match s {
    Circle(r) => 3.14159 * r * r,         // positional bind of the single field
    Rect(w: ww, h: hh) if ww == hh => ww * ww,  // named + rename + guard
    Rect(w, h) => w * h,                  // positional bind of both fields
    Pair(a, b) => float(a) * float(b),
    Shape.Point => 0.0,                   // unit variant — written QUALIFIED (see below)
  }
}

print(area(Shape.Rect(w: 3.0, h: 4.0)))   // 12.0
```

`Circle(r)` and `Shape.Circle(r)` are both accepted for a payload pattern (the trailing `(…)` makes
it unambiguously a variant pattern). Inside the arm, the narrowed field types are known (`r: float`).

#### Exhaustiveness — a missing variant is a compile error

When the subject is statically known to be a specific enum, `match` must handle **every** variant —
by naming each one or by a catch-all (`_`, or a bare binding identifier). A missing case with no
catch-all is a **blocking** `non-exhaustive-match` error:

```ascript
enum Light { Red, Yellow, Green }
fn go(l: Light): string {
  return match l {
    Light.Red => "stop",
    Light.Green => "go",
    // error: match on enum 'Light' does not cover: Yellow
  }
}
```

A guarded-only arm does not count as covering its variant (the guard may fail). When the subject's
enum type can't be proven (gradual / untyped), the check stays silent. The runtime still panics on an
uncovered value as a dynamic backstop.

#### The bare-unit footgun — qualify unit variants

A **bare** unit-variant pattern (`Red =>`, no parens) collides with an Option-C binding identifier:
the runtime *binds* the subject to `Red` (a catch-all) rather than matching `Light.Red`. The checker
flags this as `enum-variant-binding-shadow`:

```ascript
match l {
  Red => "stop",        // warning: `Red` here BINDS the subject; write `Light.Red`
  Light.Green => "go",
}
```

The fix is to write unit variants **qualified** (`Light.Red`, `Shape.Point`) in an
exhaustiveness-relevant `match` — which is why every unit variant in the examples above is qualified.
Payload patterns (`Circle(r)`) and the qualified forms are never ambiguous.
