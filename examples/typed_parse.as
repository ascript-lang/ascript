// json.parse(text, Class): parse JSON, then validate the decoded value against a
// class, fusing a parse failure and a shape mismatch into ONE Tier-1 [value, err]
// pair. With no class argument, json.parse behaves exactly as before.
import * as json from "std/json"

class User {
  id: number
  name: string
  nickname: string?
  role: string = "guest"
}

// Valid payload -> [instance, nil]: the validated instance carries declared
// fields, optionals default to nil, and field defaults are applied.
let [u, err] = json.parse("{\"id\": 1, \"name\": \"Ada\"}", User)
assert(err == nil, "valid payload has no error")
assert(u.id == 1, "id validated")
assert(u.name == "Ada", "name validated")
assert(u.nickname == nil, "optional defaults to nil")
assert(u.role == "guest", "field default applied")

// A `?` inside a Result-returning fn unwraps the instance or propagates the err.
fn loadUser(text) {
  let user = json.parse(text, User)?
  return Ok(user)
}
let [ok, e1] = loadUser("{\"id\": 2, \"name\": \"Grace\"}")
assert(e1 == nil, "loadUser ok")
assert(ok.name == "Grace", "unwrapped instance")

// Shape mismatch fuses into the err channel (NOT a panic): id must be a number.
let [bad, e2] = json.parse("{\"id\": \"x\", \"name\": \"Bug\"}", User)
assert(bad == nil, "shape mismatch -> nil value")
assert(e2 != nil, "shape mismatch -> err")
assert(e2.message != nil, "err has a message")

// Malformed JSON also surfaces in the err channel.
let [bad2, e3] = json.parse("{not json", User)
assert(bad2 == nil, "bad JSON -> nil value")
assert(e3 != nil, "bad JSON -> err")

// With NO class argument, json.parse returns the raw decoded value unchanged.
let [raw, e4] = json.parse("{\"id\": 9, \"name\": \"Lin\"}")
assert(e4 == nil, "raw parse ok")
assert(raw.id == 9, "raw object field")

print("typed_parse ok")
