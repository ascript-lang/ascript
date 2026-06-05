// std/msgpack + std/cbor — binary serialization (SP5 §4).
//
// Both modules expose the same two functions:
//   encode(value) -> bytes        // serialize any data Value to compact bytes
//   decode(bytes) -> [value, err] // Tier-1: malformed input -> err channel
//   decode(bytes, Class) -> [value, err]  // typed decode via validate_into
import * as msgpack from "std/msgpack"
import * as cbor from "std/cbor"
import * as encoding from "std/encoding"

let blob = encoding.utf8Encode("hi")
let original = { name: "Ada", nums: [1, 2, 3], ok: true, blob: blob }

// ── MessagePack round-trip ─────────────────────────────────────────────────
let mp = msgpack.encode(original)
print(`msgpack encoded ${len(mp)} bytes`)
let [mv, merr] = msgpack.decode(mp)
assert(merr == nil, `msgpack decode err: ${merr}`)
assert(mv.name == "Ada", "msgpack name")
assert(mv.nums[1] == 2, "msgpack nested array")
assert(mv.ok == true, "msgpack bool")
assert(encoding.utf8Decode(mv.blob)[0] == "hi", "msgpack bytes round-trip")
print(`msgpack: ${mv.name} ${mv.nums[1]}`)

// ── CBOR round-trip ────────────────────────────────────────────────────────
let cb = cbor.encode(original)
print(`cbor encoded ${len(cb)} bytes`)
let [cv, cerr] = cbor.decode(cb)
assert(cerr == nil, `cbor decode err: ${cerr}`)
assert(cv.name == "Ada", "cbor name")
assert(encoding.utf8Decode(cv.blob)[0] == "hi", "cbor bytes round-trip")
print(`cbor: ${cv.name} ${cv.nums[2]}`)

// ── Typed decode into a class ──────────────────────────────────────────────
class Point {
  x: number
  y: number
}
let encoded = msgpack.encode({ x: 3, y: 4 })
let [pt, perr] = msgpack.decode(encoded, Point)
assert(perr == nil, `typed decode err: ${perr}`)
assert(pt.x == 3 && pt.y == 4, "typed point")
print(`typed: (${pt.x}, ${pt.y})`)

// ── Malformed bytes → Tier-1 err (no panic) ────────────────────────────────
let [bad, berr] = cbor.decode(encoding.hexDecode("1f")[0])
assert(bad == nil, "malformed cbor: nil value")
assert(berr != nil, "malformed cbor: err set")
print(`malformed cbor rejected: ${berr.message}`)

print("binary_serialization: all assertions passed")
