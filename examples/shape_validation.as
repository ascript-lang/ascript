// ClassName.from(obj): validate a raw object into a checked instance,
// recursing into nested class fields, with optional/defaulted fields,
// and recoverable failures.
class Address {
  street: string
  zip: number
}

class User {
  id: number
  name: string
  nickname: string?
  role: string = "guest"
  address: Address
}

let good = { id: 1, name: "Ada", address: { street: "1 Lovelace Way", zip: 90210 } }
let u = User.from(good)
assert(u.id == 1, "id")
assert(u.role == "guest", "role default")
assert(u.nickname == nil, "nickname optional")
assert(u.address.zip == 90210, "nested validated")

// A shape mismatch is a recoverable panic carrying a field path.
let r = recover(() => User.from({ id: 1, name: "Bug", address: { street: "x", zip: "nope" } }))
assert(r[1] != nil, "bad zip rejected")
assert(r[1].message != nil, "error has a message")

print("shape_validation ok")
