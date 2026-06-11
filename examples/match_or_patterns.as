// Or-patterns (`|` alternatives) in `match`. An arm fires when ANY alternative
// matches; the alternatives are tried in order. Every alternative of a binding
// or-pattern must bind the SAME set of names, because the arm body has a single
// use of each bound name — `Circle(r) | Square(r) => r` reads the `r` that
// whichever alternative matched bound.

enum Shape {
  Circle(radius: int),
  Square(side: int),
  Rect(w: int, h: int),
  Empty,
}

// A binding or-pattern: both alternatives bind `r`, so the body can use it
// regardless of which variant matched.
fn dimension(s: Shape): int {
  return match s {
    Shape.Circle(r) | Shape.Square(r) => r,
    Shape.Rect(w, h) => w + h,
    Shape.Empty => 0,
  }
}

print(dimension(Shape.Circle(2)))   // 2
print(dimension(Shape.Square(5)))   // 5
print(dimension(Shape.Rect(w: 3, h: 4)))  // 7
print(dimension(Shape.Empty))       // 0

// A binding or-pattern WITH a guard: the guard runs after the bind, so it can
// reference the bound name. A guard failure falls through to the next alternative
// / arm, exactly like the tree-walker.
fn classify(s: Shape): string {
  return match s {
    Shape.Circle(r) | Shape.Square(r) if r > 10 => "big",
    Shape.Circle(r) | Shape.Square(r) => "small",
    _ => "other",
  }
}

print(classify(Shape.Circle(2)))    // small
print(classify(Shape.Square(42)))   // big
print(classify(Shape.Empty))        // other

// A NON-binding or-pattern over plain values: literal alternatives compare, no
// bindings involved.
fn weekday(n: int): string {
  return match n {
    1 | 2 | 3 | 4 | 5 => "weekday",
    6 | 7 => "weekend",
    _ => "invalid",
  }
}

print(weekday(3))   // weekday
print(weekday(7))   // weekend
print(weekday(9))   // invalid
