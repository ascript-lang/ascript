// A recursive algebraic enum: `Json` is a sum of scalar variants plus an `Arr`
// variant whose payload references `Json` itself. Recursive payloads can form
// cycles, so they are GC-traced; this example builds and renders a nested tree.
import * as array from "std/array"
import * as string from "std/string"

enum Json {
  Null,
  Bool(value: bool),
  Num(value: float),
  Str(value: string),
  Arr(items: array<Json>),   // recursive — payload references the enum itself
}

// Render a `Json` value to its textual form. The `match` is exhaustive over every
// variant; the `Arr` arm recurses through the payload array.
// NOTE: a bare unit-variant pattern (`Null`) would be an Option-C BINDING (it
// captures the subject), so unit variants are written QUALIFIED (`Json.Null`) to
// match by variant — the documented ADT rule for exhaustiveness-relevant matches.
fn render(j: Json): string {
  return match j {
    Json.Null => "null",
    Bool(b) => `${b}`,
    Num(n) => `${n}`,
    Str(s) => `"${s}"`,
    Arr(xs) => "[" + string.join(array.map(xs, render), ",") + "]",
  }
}

// A small nested document built from the variants.
let doc = Json.Arr([
  Json.Null,
  Json.Bool(true),
  Json.Num(42.0),
  Json.Str("hi"),
  Json.Arr([Json.Num(1.0), Json.Num(2.0)]),
])

print(render(doc))
// [null,true,42.0,"hi",[1.0,2.0]]

// Structural equality over SCALAR-payload variants: two constructions with equal
// scalar payloads are equal. (A payload that contains an Array/Object compares that
// container by identity — AScript's container-equality rule — so two distinct
// `Arr([…])` are not equal even with equal elements; scalar payloads are.)
print(Json.Num(1.0) == Json.Num(1.0))           // true
print(Json.Num(1.0) == Json.Num(2.0))           // false
print(Json.Str("x") == Json.Str("x"))           // true

// Reflection: a payload variant's `.name` and `.value` (the payload as data). `Arr`
// is a named single-field variant, so `.value` is the Object `{items: […]}`.
print(doc.name)                                  // Arr
print(len(doc.value))                            // 1  (one field: `items`)
