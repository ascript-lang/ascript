# Classes, enums, interfaces & generics

This chapter specifies AScript's user-defined types: **classes** (with fields,
methods, statics, and inheritance), **algebraic enums** (unit and payload
variants), **structural interfaces**, and **generics**. The syntactic forms are in
the [grammar chapter](grammar); this chapter gives their runtime meaning. Pattern
matching over enum variants is in the [patterns chapter](patterns); the type
contracts that field and parameter annotations impose are in the *Types* chapter.

## Classes

A class is declared with `class Name { … }`. The class name resolves to a
**callable value**: invoking it constructs an instance. If the body declares
`init`, construction runs `init` with the constructor arguments; `self` inside a
method refers to the receiver instance.

```as
class Animal {
  init(name) { self.name = name }
  fn speak() { return self.name + " makes a sound" }
}
let a = Animal("Rex")
print(a.speak())            // Rex makes a sound
```

A class supports **single inheritance** via `extends`. A subclass method MAY call
the corresponding superclass method with `super.method(...)`; method resolution
walks the inheritance chain. An `init` declared as `async fn init` or `fn* init`
is a compile error — construction is synchronous and non-generating.

```as
class Dog extends Animal {
  fn speak() { return super.speak() + " (woof)" }
}
print(Dog("Rex").speak())   // Rex makes a sound (woof)
```

`v instanceof C`, where `C` is a class, is a **nominal** check: it is `true` iff
`v` is an instance of `C` or of a subclass of `C`. (`instanceof` against an
interface is structural — see *Interfaces* below.)

## Statics

A `static fn` lives in a **separate namespace** on the class itself, reached as
`Name.method(...)` rather than on instances. Statics are inherited by subclasses.
A static method has no receiver instance and MUST NOT use `super`.

```as
class Counter {
  static fn zero() { return 0 }
}
print(Counter.zero())       // 0
```

## Typed fields & records

A class body MAY declare **field schemas**. A field declaration is one of:

- **required** — `id: number`;
- **optional** — `name: T?` (sugar for `T | nil`) or the marker form `name?: T`;
- **defaulted** — `role: string = "guest"`.

A field's declared type is **checked on assignment**, including assignments made
inside `init`. A class whose body is field-schemas-only (no `init`) gets an
**auto-derived positional constructor**: required fields become required
positional parameters in declaration order, and defaulted fields become optional
trailing parameters. This is the *record* shape.

```as
class Point { x: int; y: int }
let p = Point(3, 4)         // auto-derived positional init
print(p.x, p.y)            // 3 4
```

## `ClassName.from`

`ClassName.from(obj, strict=false)` validates a plain object (or instance)
**into** an instance of the class. It runs `validate_into`, which:

- recurses into nested class fields, `array<Class>` elements, and `map<K, Class>`
  values;
- applies declared field defaults for absent keys;
- raises a **recoverable field-path panic** (a Tier-2 panic naming the failing
  dotted/indexed selector, e.g. `user.roles[2]`) on a shape or type mismatch;
- with `strict=true`, rejects unexpected keys;
- does **NOT** run `init`.

The same `validate_into` machinery powers typed parsing: `json.parse(text, Class)`
and `resp.json(Class)` fuse decoding and shape validation into one Tier-1
`[value, err]` pair (see the [errors chapter](errors)). The class is passed as an
ordinary value argument — there are no runtime type arguments.

## Enums

An `enum` declares a set of **variants**. A variant is one of:

- **unit** — `Point`, or with an explicit backing value `Red = 2`. Unit variants
  are interned and registration-free; `v.name` reflects the variant name and
  `v.value` reflects the backing value.
- **payload** — `Pair(int, int)` (positional) or `Circle(radius: float)` (named).
  A variant is uniformly named **xor** positional; each field's type is required;
  a variant never carries both a `= backing` and a `(…)` payload.

A payload variant is a **first-class constructor**: `Shape.Circle(2.0)` validates
the argument arity and field types (via `validate_into`) and produces a
constructed variant. Constructed payload variants compare **structurally** with
`==`. `v.value` reflects the payload (a named-fields object, or a stable array for
positional payloads); named fields are also readable directly (`c.radius`).

```as
enum Shape {
  Point,
  Circle(radius: float),
}
let c = Shape.Circle(2.0)
print(c.radius)            // 2.0
print(c == Shape.Circle(2.0))   // true
```

**Exhaustiveness over an enum-typed subject is checked statically** — the checker
emits `non-exhaustive-match` (default severity **Error**) for a `match` that does
not cover every variant of a subject it can prove is enum-typed; the runtime
`MatchNoArm` backstop is unchanged. Because a bare unqualified unit-variant pattern
**shadow-binds** (the *bind-vs-compare* rule of the [patterns chapter](patterns)),
write unit variants **qualified** (`Shape.Point`) in exhaustiveness-relevant
matches; the checker warns with `enum-variant-binding-shadow` on the unqualified
form.

## Interfaces

An `interface` declares a named **method set** — method signatures with no
bodies. `interface` is a reserved keyword; the modifiers `async`/`fn*`/`static`/
`worker` are rejected on interface methods. An interface name resolves to an
immutable, identity-equal descriptor value; it is never a receiver.

`v instanceof I`, where `I` is an interface, is a **structural conformance**
check. In v1, conformance is **method name + arity compatibility**: `v` conforms
iff it is a class instance whose class declares, for every interface method, a
method of the same name with a compatible parameter arity. Only class instances
can conform.

- **`extends`** composes interfaces: the method set is the transitive union of the
  interface and everything it extends. Composition is flattened lazily (so an
  extended interface MAY be a forward reference) with a runtime cycle guard.
- **`implements`** on a class is **documentation only**: it is recorded for the
  checker and never affects runtime conformance, which stays purely structural. A
  class that conforms structurally conforms whether or not it declares
  `implements`; a class that declares `implements` but does not match structurally
  does **not** conform.
- An **interface-typed annotation** is a runtime contract: the value is checked
  with the same conformance predicate.
- An interface **value** is not sendable across worker boundaries — shipping one
  raises a field-path panic.

```as
interface Greeter { fn greet(name) }
class Formal { fn greet(name) { return "Good day, " + name } }
print(Formal() instanceof Greeter)    // true  (name + arity match)
```

## Generics & erasure

Type parameters MAY appear on functions, classes, enums, and interfaces (e.g.
`class Box<T> { v: T }`). Generics are checked **statically** (the checker's
`CheckTy::{Var, FnSig, ClassApp, EnumApp, Interface}` lattice, argument-driven
inference, and invariant parameterized applications — see the *Types* chapter)
and **erased at runtime**:

- a `T`-typed slot performs **no** runtime check (it accepts any value);
- generic instantiation creates **no** distinct runtime type — `Box<int>` and
  `Box<string>` are one runtime class;
- bytecode carries no type arguments, so the `.aso` format is unchanged by
  generics.

```as
class Box<T> { v: T }
let b = Box(1)
b.v = "now a string"        // T is erased — no runtime contract fires
print(b.v)                  // now a string
```

## Conformance

The constructs in this chapter are exercised by:

- `examples/oop.as` — classes, `init`, methods, single inheritance, `super`, and
  nominal `instanceof`.
- `examples/records.as` — auto-derived positional constructors for field-only
  classes.
- `examples/static_methods.as` — static methods and their separate namespace.
- `examples/typed_fields.as` — typed field schemas checked on assignment.
- `examples/typed_parse.as` — `validate_into` powering typed parse.
- `examples/enums_adt.as` and `examples/enums_negative_backing.as` — unit and
  payload variants, first-class payload constructors, `.value` reflection, and
  structural payload equality.
- `examples/interfaces.as` and `examples/advanced/interface_dispatch.as` —
  structural conformance, `extends`, and `implements`.
- `examples/generics.as` — generic functions and classes (erased at runtime).
- `tests/check.rs` — the static-checker corpus and exhaustiveness pins
  (`non_exhaustive_missing_variant_is_error_naming_it`,
  `exhaustive_all_variants_is_clean`).

Run each example with `target/release/ascript run examples/oop.as` (and likewise);
each matches its recorded golden. The runtime-erasure behavior above is reproduced
directly: a `Box<T>` field reassigned across types runs without a contract panic on
both engines.
