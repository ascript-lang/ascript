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

`match` is an **expression**: it evaluates to a value. Use `|` to match alternatives and `_` as the
catch-all.

```ascript
const label = match count {
  0     => "zero",
  1 | 2 => "small",
  _     => "many",
}
```

It pairs naturally with enums:

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

Because `match` returns a value, it slots directly into a `return`, a `let`, or a larger expression —
no temporary mutable variable required.
