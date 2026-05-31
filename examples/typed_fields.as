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

print("typed_fields ok")
