// Static methods (SP1 §3): `static fn` / `static async fn` / `static fn*` are
// class-level members called as `C.name(args)` with NO `self`. They live in a
// SEPARATE namespace from instance methods (an instance `c.x()` and a static
// `C.x()` can coexist), are INHERITED up the superclass chain, and the blessed
// async-construction pattern is a `static async fn create()` factory returning a
// future. `from` is reserved (a static `from` would collide with typed-parse).
class Point {
  x: number = 0
  y: number = 0
  fn init(x, y) {
    self.x = x
    self.y = y
  }

  // A sync static factory: constructs and returns a configured instance.
  static fn origin() {
    return Point(0, 0)
  }

  // A static that calls another static + constructs.
  static fn diagonal(n) {
    let p = Point.origin()
    p.x = n
    p.y = n
    return p
  }

  // The blessed async-construction pattern: `static async fn create()` awaits
  // some work, then returns the built instance — invoked as `await Point.create()`.
  static async fn create(x, y) {
    let p = Point(x, y)
    p.x = await (x + 1)
    return p
  }

  // A static generator: `C.seq()` returns a generator driven by `for await`.
  static fn* seq(n) {
    let i = 0
    while (i < n) {
      yield i
      i = i + 1
    }
  }

  // An INSTANCE method named like a static would be — separate namespaces.
  fn sum() {
    return self.x + self.y
  }
}

// A subclass INHERITS the parent's statics (resolved up the chain) and can add
// its own.
class Origin3D extends Point {
  static fn label() {
    return "3D"
  }
}

fn main() {
  let o = Point.origin()
  print(o.sum()) // 0
  let d = Point.diagonal(4)
  print(d.sum()) // 8
  let c = await Point.create(10, 20)
  print(c.x) // 11
  print(c.sum()) // 31

  // A static generator consumed via for-await.
  let total = 0
  for await (v in Point.seq(4)) {
    total = total + v
  }
  print(total) // 0+1+2+3 = 6

  // Inheritance: the subclass resolves the parent's static up the chain, and
  // also exposes its own.
  print(Origin3D.origin().sum()) // 0  (inherited Point.origin)
  print(Origin3D.label()) // 3D (own static)

  // The built-in typed-parse `.from` coexists with user statics.
  let parsed = Point.from({x: 2, y: 3})
  print(parsed.sum()) // 5
  print("static_methods ok")
}

await main()
