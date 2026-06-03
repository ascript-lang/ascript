// Typed class fields: required, optional (T?), and defaulted.
class User {
  id: number
  name: string
  nickname: string?       // optional
  role: string = "guest"  // optional with default
  fn init(id, name) {
    self.id = id
    self.name = name
  }
}

let u = User(1, "Ada")
assert(u.id == 1, "id")
assert(u.name == "Ada", "name")
assert(u.nickname == nil, "nickname defaults to nil")
assert(u.role == "guest", "role default applied")

// Computed field defaults: any expression the language can evaluate may be a
// field default, not just a literal. The default is evaluated once per missing
// field — at construction AND on `Class.from({...})` typed-parse.
let PREFIX = "item-"
let BASE_PRICE = 100

class Product {
  id: number = 1 + 1                 // binary arithmetic
  tag: string = PREFIX + "x"         // string concat referencing a module const
  price: number = BASE_PRICE * 2     // arithmetic over a module const
  inStock: bool = BASE_PRICE > 0     // comparison
  label: string = `#${BASE_PRICE}`   // template interpolation
  tiers: array<number> = 1..4        // exclusive range -> [1, 2, 3]
  discounted: number = true ? 90 : BASE_PRICE  // ternary (lazy branches)
  fn init() {}
}

let p = Product()
assert(p.id == 2, "id default = 1 + 1")
assert(p.tag == "item-x", "tag default = PREFIX + \"x\"")
assert(p.price == 200, "price default = BASE_PRICE * 2")
assert(p.inStock == true, "inStock default = BASE_PRICE > 0")
assert(p.label == "#100", "label default = template")
assert(len(p.tiers) == 3 && p.tiers[0] == 1 && p.tiers[2] == 3, "tiers default = exclusive range")
assert(p.discounted == 90, "discounted default = ternary")

// The SAME computed defaults fill missing fields on the typed-parse path.
let q = Product.from({id: 42})
assert(q.id == 42, "from() keeps a provided field")
assert(q.tag == "item-x", "from() applies the computed tag default")
assert(q.price == 200, "from() applies the computed price default")
assert(len(q.tiers) == 3 && q.tiers[1] == 2, "from() applies the range default")

print("typed_fields ok")
