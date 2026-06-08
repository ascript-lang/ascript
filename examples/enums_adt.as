// Algebraic enums (ADT): variants that carry typed payloads — positional, named,
// and unit — constructed as first-class values and destructured by `match`.
import * as array from "std/array"

enum Shape {
  Circle(radius: float),      // named payload (single field)
  Rect(w: float, h: float),   // named payload (multiple fields)
  Pair(int, int),             // positional payload
  Point,                      // unit variant (payload-less)
}

// Exhaustive area over every variant shape. A positional pattern binds the fields
// in declaration order; `Point` is the unit variant.
fn area(s: Shape): float {
  return match s {
    Circle(r) => 3.14159 * r * r,
    Rect(w, h) => w * h,
    Pair(a, b) => float(a) * float(b),
    Shape.Point => 0.0,
  }
}

let c = Shape.Circle(2.0)
let rect = Shape.Rect(w: 3.0, h: 4.0)   // multi-field named variant: named args
let p = Shape.Pair(3, 4)
let pt = Shape.Point

print(area(c))      // 12.56636
print(area(rect))   // 12.0
print(area(p))      // 12.0
print(area(pt))     // 0.0

// Reflection: `.value` of a payload variant is its data (Object for named, Array
// for positional); a named field is also readable directly.
print(c.value)    // {radius: 2.0}
print(c.radius)   // 2.0
print(p.value)    // [3, 4]
print(c.name)     // Circle

// Structural equality: two constructions with equal payloads are equal.
print(c == Shape.Circle(2.0))   // true
print(c == Shape.Circle(3.0))   // false

// First-class constructor: `Shape.Circle` is a callable value usable in `map`.
let circles = array.map([1.0, 2.0, 3.0], Shape.Circle)
print(circles[0].radius)   // 1.0
print(circles[2].radius)   // 3.0

// Named destructuring (with rename) + a guard.
fn describe(s: Shape): string {
  return match s {
    Rect(w: ww, h: hh) if ww == hh => "square",
    Rect(w: ww, h: hh) => "rectangle",
    Circle(_) => "circle",
    _ => "other",
  }
}
print(describe(Shape.Rect(w: 5.0, h: 5.0)))   // square
print(describe(Shape.Rect(w: 3.0, h: 4.0)))   // rectangle
print(describe(Shape.Circle(1.0)))            // circle
