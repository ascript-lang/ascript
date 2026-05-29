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
