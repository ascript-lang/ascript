// Records / auto-derived `init` (SP2 §5): a class that declares fields but has NO
// explicit `init` automatically gets a POSITIONAL constructor — its params are the
// declared fields in declaration order, a defaulted field becomes an optional
// trailing param, and each positional argument is contract-checked against its
// field's type. No new keyword: "record-ness" is implicit (fields + no init).
// Byte-identical on the bytecode VM and the `--tree-walker` oracle.
import * as object from "std/object"

// A plain record: `Point(x, y)` binds the fields positionally.
class Point {
  x: number
  y: number
}
let p = Point(3, 4)
print(p.x) // 3
print(p.y) // 4

// A defaulted field is an OPTIONAL trailing param: omit it to take the default,
// or pass it to override.
class Config {
  host: string
  port: number = 8080
}
print(Config("localhost").port) // 8080 — default
print(Config("localhost", 9000).port) // 9000 — overridden

// Inheritance: the constructor takes the base fields FIRST, then the subclass's,
// in declaration order. `Point3` extends `Point`, so it is `Point3(x, y, z)`.
class Point3 extends Point {
  z: number
}
let q = Point3(1, 2, 3)
print(q.x) // 1
print(q.y) // 2
print(q.z) // 3

// A record instance is a normal class instance: `instanceof` sees the whole chain.
print(p instanceof Point) // true
print(q instanceof Point) // true (inherited)
print(q instanceof Point3) // true

// A class WITH an explicit `init` is UNCHANGED — no auto-init is derived; the
// init body runs as written.
class Counter {
  n: number = 0
  fn init(start) {
    self.n = start + 1
  }
}
print(Counter(10).n) // 11

// Records compose with `object.freeze` (SP2 §4): freeze a record instance to make
// it immutable.
let frozen = object.freeze(Point(7, 8))
print(object.isFrozen(frozen)) // true
print(frozen.x) // 7
print("records ok")
