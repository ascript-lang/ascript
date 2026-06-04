// `instanceof` — runtime type test against a class (SP2 §1).
//
// `x instanceof C` is a comparison-tier binary operator yielding a bool: true iff
// `x` is an instance of `C` or any subclass of `C` (the `extends` chain is walked).
// A non-instance left operand (number, string, nil, …) is always false, never an
// error. The right operand must be a class.
class Animal {
  fn speak() {
    return "..."
  }
}

class Dog extends Animal {
  fn speak() {
    return "woof"
  }
}

class Cat extends Animal {
  fn speak() {
    return "meow"
  }
}

fn describe(x) {
  // Dispatch on the most specific class first, then the base class.
  if (x instanceof Dog) {
    return "a dog says " + x.speak()
  }
  if (x instanceof Cat) {
    return "a cat says " + x.speak()
  }
  if (x instanceof Animal) {
    return "some animal says " + x.speak()
  }
  return "not an animal"
}

let d = Dog()
let c = Cat()
let a = Animal()

print(describe(d)) // a dog says woof
print(describe(c)) // a cat says meow
print(describe(a)) // some animal says ...
print(describe(42)) // not an animal

// A subclass instance is `instanceof` its parent, but not the other way around.
print(d instanceof Animal) // true
print(a instanceof Dog) // false

// Non-instances are always false.
print("hello" instanceof Animal) // false
print(nil instanceof Animal) // false

// `instanceof` binds at the comparison tier, so it composes with `&&`.
print(d instanceof Animal && c instanceof Animal) // true
print("instanceof ok")
