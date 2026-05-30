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
