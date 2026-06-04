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

> [!NOTE] The optional field above can be spelled two ways — `nickname: string?` or the marker form
> `nickname?: string`. **Both lower to the same node** (`string | nil`); the formatter normalizes the
> marker form to the canonical `nickname: string?`.

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
- The **right operand must be a class**. Anything else (`x instanceof 5`, `x instanceof nil`) is a
  Tier-2 panic: `instanceof requires a class on the right-hand side`.
- It binds at the relational tier (same as `< <= > >=`), looser than `+`/`-` and tighter than `&&`,
  so `a instanceof B && c` parses as `(a instanceof B) && c`.

## Enums

Enums are **simple named variants** — no payloads, no methods. A variant may carry an optional
backing value (a number or string).

```ascript
enum Color  { Red, Green, Blue }                    // opaque variants
enum Status { Ok = 200, NotFound = 404, Err = 500 } // number-backed
enum Mode   { Read = "r", Write = "w" }             // string-backed
```

Access a variant with `Enum.Variant`. Each variant exposes its `.name` and `.value`:

```ascript
print(Status.NotFound)         // Status.NotFound
print(Status.NotFound.value)   // 404
print(Status.NotFound.name)    // NotFound
print(Color.Red.value)         // nil — opaque variants have no backing value
```

Variants are interned singletons, so identity comparison just works. A variant never equals a
variant of another enum, nor its own raw backing value:

```ascript
Color.Red == Color.Red    // true
Status.Ok == 200          // false — a variant is not its backing number
```

> [!NOTE] Enums intentionally carry no per-variant data or behaviour. When you need that — a tagged
> union with typed payloads, or variant-specific methods — model it with a class hierarchy instead.

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
