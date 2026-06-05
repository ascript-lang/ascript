:::eyebrow Standard library

# Validation & schema

`std/schema` is AScript's composable schema-validation library. A schema is a plain tagged object
(`{ __kind: "..." }`) that describes the expected shape and constraints of a value.
`schema.parse(schema, value)` validates the value against the schema and returns a Tier-1
`[value, err]` pair — no exceptions, no panics for expected validation failures.

> [!TIER1] `schema.parse` always returns `[value, err]`. On success `err` is `nil`; on failure
> `value` is `nil` and `err` is `{ path: string, message: string }` pointing at the first
> failed field.

```ascript
import * as schema from "std/schema"

let userSchema = schema.object({
  name: schema.minLength(schema.string(), 1),
  age:  schema.min(schema.number(), 0),
})

let [u, err] = schema.parse(userSchema, { name: "Ada", age: 36 })
// err == nil; u.name == "Ada"

let [bad, e] = schema.parse(userSchema, { name: "", age: 36 })
// bad == nil; e.path == "name"; e.message == "expected string with minLength 1, got length 0"
```

## Primitive constructors

Each constructor returns a schema object. Primitives can be combined with [constraint
methods](#constraints) to add bounds, length limits, patterns, or custom refinements.

| Constructor | Accepts |
|---|---|
| `schema.string()` | any `string` |
| `schema.number()` | any `number` |
| `schema.bool()` | `true` or `false` |
| `schema.nilType()` | only `nil` (name avoids the `nil` keyword) |
| `schema.any()` | anything — always passes |
| `schema.literal(v)` | exactly the value `v` (strict equality) |

```ascript
let [n, e] = schema.parse(schema.number(), 42)       // ok
let [b, e] = schema.parse(schema.bool(), "true")     // err — not a bool
let [v, e] = schema.parse(schema.literal("ok"), "ok") // ok
```

## Composite constructors

### schema.array(elemSchema)

Validates that the value is an array and that every element matches `elemSchema`. The validated
array (with each element passed through its schema) is returned.

```ascript
let tagsSchema = schema.array(schema.string())
let [[t1, t2], e] = schema.parse(tagsSchema, ["a", "b"])   // ok
```

### schema.object(fields)

Validates an object against a map of field schemas. Missing optional fields default to `nil` (or
their [`schema.default`](#schemadefault) value); extra keys are ignored unless [`schema.strict`](#schemastrictobjectschema)
is applied.

- `fields` — an object literal mapping field names to schemas.

```ascript
let pointSchema = schema.object({ x: schema.number(), y: schema.number() })
let [pt, e] = schema.parse(pointSchema, { x: 1, y: 2, extra: "ignored" })
// pt == { x: 1, y: 2 }; extra key is silently dropped from the result
```

### schema.strict(objectSchema)

Wraps an object schema to reject any key **not** declared in its `fields`. Returns a
`[nil, err]` pair naming the unexpected key.

```ascript
let strict = schema.strict(schema.object({ x: schema.number() }))
let [bad, e] = schema.parse(strict, { x: 1, y: 2 })
// bad == nil; e.path == "y"; e.message mentions "not allowed in strict object"
```

### schema.map(keySchema, valSchema)

Validates a `map` (or an object treated as a string-keyed map) against key and value schemas.
Each entry is validated; the result is a `map` value.

```ascript
let countsSchema = schema.map(schema.string(), schema.number())
let [m, e] = schema.parse(countsSchema, { apples: 3, oranges: 7 })
// m is a Map<string, number>
```

### schema.optional(innerSchema)

Accepts `nil` (passes it through) or any value that matches `innerSchema`.

```ascript
let maybeStr = schema.optional(schema.string())
let [v1, _] = schema.parse(maybeStr, nil)      // v1 == nil
let [v2, _] = schema.parse(maybeStr, "hello")  // v2 == "hello"
```

### schema.union(schemas)

Tries each schema in order; the first match wins. If none match, returns an error naming all
tried kinds.

```ascript
let numOrStr = schema.union([schema.number(), schema.string()])
let [v, e] = schema.parse(numOrStr, 42)      // ok — number matched first
let [v, e] = schema.parse(numOrStr, "hi")    // ok — string matched
let [v, e] = schema.parse(numOrStr, true)    // err — neither matched
```

### schema.oneOf(values)

Validates that the value is **exactly** one of the listed values (strict equality). This is the
enum-like constructor — the name `oneOf` is used because `enum` is a reserved keyword.

```ascript
let roleSchema = schema.oneOf(["admin", "editor", "viewer"])
let [r, e] = schema.parse(roleSchema, "admin")     // ok
let [r, e] = schema.parse(roleSchema, "root")      // err
```

## Fluent method chaining

Refiners and `parse` can be called as **methods** on a schema value, in addition to the free
functions — both forms are valid and produce identical results (the method is the same operation,
just `s.op(...)` instead of `schema.op(s, ...)`):

```ascript
// Fluent: each call returns a new schema, so refiners chain.
let username = schema.string().minLength(3).maxLength(12).pattern("^[a-z0-9_]+$")
let [v, err] = username.parse("ada_lovelace")   // ["ada_lovelace", nil]
let [v2, e2] = username.parse("ab")             // [nil, err]  (minLength)

// Free-function form — exactly equivalent, still fully supported.
let same = schema.pattern(schema.maxLength(schema.minLength(schema.string(), 3), 12), "^[a-z0-9_]+$")
schema.parse(same, "ada_lovelace")              // same result
```

**Which ops are methods:** the refiners and composites whose first argument is the receiver
schema, plus the terminal `parse` — `minLength`, `maxLength`, `pattern`, `min`, `max`, `refine`,
`default`, `optional`, `strict`, `parse`. The **source constructors** (`string`, `number`, `bool`,
`nilType`, `any`, `literal`, `object`, `array`, `union`, `oneOf`, `map`, `fromClass`) do not take a
receiver schema, so they stay `schema.*(...)` module functions — they are the chain entry points.

> [!NOTE] **Call position only.** A schema method works when called (`s.minLength(3)`).
> Extract-then-call (`let f = s.minLength; f(3)`) is **not** supported: bare member access
> `s.minLength` reads the *stored constraint value* (e.g. the number `3`), not a bound method. This
> is the deliberate consequence of distinguishing call context from access context, which avoids a
> collision between a refiner method and the constraint field it stores. Fluent chaining always uses
> call position, so every intended use works.

## Collect all errors — `schema.parseAll`

`schema.parse` is **fail-fast**: it stops at the first failing field and returns a single
`{ path, message }` error. `schema.parseAll(schema, value[, options])` is the **collect-all** mode:
it keeps going and returns **every** validation error.

> [!TIER1] `schema.parseAll` returns `[value, errors]`. On success `errors` is `nil` and `value` is
> the validated value (exactly like `parse`). On failure `value` is `nil` and `errors` is an
> **array** of `{ path, message }` objects — one per leaf failure, in deterministic document order
> (object fields in declared order, array elements by index, map entries in iteration order). A
> failure always carries at least one error.

```ascript
let form = schema.object({
  name:  schema.minLength(schema.string(), 1),
  age:   schema.min(schema.number(), 0),
  email: schema.string(),
})

let [val, errs] = schema.parseAll(form, { name: "", age: -3, email: 42 })
// val == nil; errs has THREE entries:
//   { path: "name",  message: "expected string with minLength 1, got length 0" }
//   { path: "age",   message: "expected number >= 0 (min), got -3" }
//   { path: "email", message: "expected string, got number" }

for (e in errs) { print(`${e.path}: ${e.message}`) }

// Fluent method form works too:
let [_v, errs2] = form.parseAll({ name: "", age: -3, email: 42 })
```

Nested errors carry full dotted/indexed paths (`user.name`, `tags[1]`). The error messages are
**byte-identical** to `parse`'s — only the accumulation differs. A `union` failure stays a single
error (a union failure means "matched none", not N failures). A malformed schema or a panic from a
`refine` function still escalates as a Tier-2 panic — collect-all only accumulates Tier-1
validation mismatches, never swallows a programmer error.

`parseAll` is a method too — it appears in the same method set as `parse`.

## Constraints

Constraints are chainable modifiers that clone the schema and add a check. They are applied
**after** the base type check passes. Each is shown below as a free function; the equivalent
[method form](#fluent-method-chaining) (`s.min(0)`, `s.minLength(1)`, …) is also available.

### schema.min(s, n) / schema.max(s, n)

Require a `number` value to be `>= n` or `<= n`.

```ascript
let ageSchema = schema.min(schema.number(), 0)
let [v, e] = schema.parse(ageSchema, -1)    // err: expected number >= 0
```

### schema.minLength(s, n) / schema.maxLength(s, n)

Require a `string` value to have at least / at most `n` characters (Unicode scalar values),
or an `array` to have at least / at most `n` elements.

```ascript
let nameSchema = schema.minLength(schema.string(), 1)
let [v, e] = schema.parse(nameSchema, "")   // err: expected string with minLength 1
```

### schema.pattern(s, regexStr)

Require a `string` value to match the regular-expression pattern. The pattern is a standard
regex string.

> [!NOTE] `schema.pattern` enforcement requires the `data` Cargo feature (enabled by default).
> The constructor `schema.pattern(s, re)` is always available; without `data`, a stored pattern
> causes an `InvalidSchema` error at parse time rather than being silently ignored.

```ascript
let zipSchema = schema.pattern(schema.string(), "^[0-9]{5}$")
let [v, e] = schema.parse(zipSchema, "90210")   // ok
let [v, e] = schema.parse(zipSchema, "ABCDE")   // err: pattern not matched
```

### schema.refine(s, fn, message)

Adds a custom async predicate `fn(value) -> bool`. Called after the base type check passes. If
`fn` returns falsy, `err.message` is set to `message`. A panic from inside `fn` propagates as
a genuine Tier-2 panic, not a Tier-1 validation failure.

```ascript
fn looksLikeEmail(s) { return string.contains(s, "@") }

let emailSchema = schema.refine(schema.string(), looksLikeEmail, "must look like an email")
let [v, e] = schema.parse(emailSchema, "not-an-email")
// e.message == "must look like an email"
```

### schema.default(s, value)

When the incoming value is `nil`, substitute `value` and skip all further checks (trusting the
stored default). Non-nil values are validated normally.

```ascript
let withDefault = schema.default(schema.string(), "anonymous")
let [v, _] = schema.parse(withDefault, nil)     // v == "anonymous"
let [v, _] = schema.parse(withDefault, "Ada")   // v == "Ada"
```

## schema.parse

```
schema.parse(schema, value [, {coerce: true}]) -> [value, err]
```

The main entry point. Validates `value` against `schema` and returns a `[value, err]` pair.

- On success: `[validatedValue, nil]`
- On Tier-1 failure: `[nil, { path: string, message: string }]`
  - `path` — dot-and-bracket notation pointing at the first failed field: `""` for the root,
    `"address.city"` for a nested field, `"tags[2]"` for an array element.
  - `message` — human-readable description of the failure.
- A malformed schema (missing `__kind`) is a Tier-2 panic, not a Tier-1 failure.

### Coerce option

Pass `{ coerce: true }` as a third argument to enable conservative type coercions before kind
dispatch:

| Input | Target | Result |
|---|---|---|
| `"42"` (string) | `number` | `42` (parsed as f64) |
| `99` (number) | `string` | `"99"` |
| `"true"` (string) | `bool` | `true` |
| `"false"` (string) | `bool` | `false` |

All other combinations fall through to the normal type check.

```ascript
let [v, e] = schema.parse(schema.number(), "42", { coerce: true })
// v == 42; e == nil
```

## schema.fromClass

```
schema.fromClass(Class) -> objectSchema
```

Derives an `{ __kind: "object", fields: {...} }` schema from a class's declared field types.
Recurses into nested class fields, array element types, and map value types. Handles the
`T?` optional suffix, `Union(A, B)`, and primitive types.

Self-referential or mutually-recursive class graphs are detected and cycle edges fall back to a
bare object schema (accepts any object, rejects non-objects). Named class types that resolve
to a class in scope are fully expanded.

```ascript
class Point {
  x: number
  y: number
  label: string?
}

let pointSchema = schema.fromClass(Point)

let [pt, e] = schema.parse(pointSchema, { x: 3, y: 4 })
// pt.x == 3; pt.y == 4; e == nil

let [bad, e] = schema.parse(pointSchema, { x: "oops", y: 4 })
// bad == nil; e.path == "x"; e.message == "expected number, got string"
```

## json.parse(text, schema)

`json.parse` accepts a schema object (in addition to a [class](data)) as its second argument.
When a schema is passed, the JSON is decoded and the resulting value is immediately validated
against the schema. A decode failure and a schema mismatch are **fused into one Tier-1 pair** —
neither panics.

```ascript
import * as schema from "std/schema"
import * as json from "std/json"

let userSchema = schema.object({
  name: schema.minLength(schema.string(), 1),
  age:  schema.min(schema.number(), 0),
})

// Valid JSON + valid shape → success
let [u, e] = json.parse("{\"name\":\"Ada\",\"age\":36}", userSchema)
// u.name == "Ada"; e == nil

// Malformed JSON → err (fused)
let [bad, e] = json.parse("{bad json", userSchema)
// bad == nil; e != nil

// Valid JSON but schema mismatch → err (fused)
let [bad, e] = json.parse("{\"name\":\"\",\"age\":36}", userSchema)
// bad == nil; e.path == "name"
```

Because `?` composes with the pair, a validating loader is a one-liner:

```ascript
fn loadUser(text: string) {
  let user = json.parse(text, userSchema)?
  return Ok(user)
}
```

See `examples/validation.as` for a complete runnable walkthrough.
