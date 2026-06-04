enum Shape { Circle, Square, Triangle }

class Animal {
  fn init(name) { self.name = name }
  fn describe() { return `${self.name} is an animal` }
}

class Dog extends Animal {
  fn init(name) { super.init(name) }
  fn describe() { return super.describe() + ", specifically a dog" }
  fn sound() { return "woof" }
}

// A generator method (`fn*`) bound to `self`: dispatched as `c.g()` it returns a
// generator that yields lazily, driven by `for await` / `.next()` — exactly like a
// standalone `fn*`. `super`/inheritance/override apply as for ordinary methods.
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

fn shapeName(s: Shape): string {
  return match s {
    Shape.Circle => "circle",
    Shape.Square => "square",
    _ => "other",
  }
}

let d: Animal = Dog("Rex")
print(d.describe())
print(d.sound())
print(shapeName(Shape.Square))
print(shapeName(Shape.Triangle))

let c = Counter(3)
for await (v in c.upTo(6)) {
  print(v)
}
