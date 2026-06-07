// PROFILE TARGET: recommendations #1 (faster hashing) + allocation pressure.
// Stringify + parse a small nested object 100k times. Parsing rebuilds Objects
// (IndexMap inserts → SipHash) and allocates strings; stringify walks + allocates.
// This is the "glue code" shape a batteries-included scripting language actually
// runs, and should expose hashing + allocator + GC cost rather than dispatch.
import * as json from "std/json"
import * as time from "std/time"

let obj = {
  name: "ascript",
  version: 11,
  tags: ["lang", "rust", "fast"],
  meta: { a: 1, b: 2, c: 3, d: 4 },
  nums: [1, 2, 3, 4, 5, 6, 7, 8],
}

let t0 = time.monotonic()
let acc = 0
for (i in 0..700000) {
  let [s, e1] = json.stringify(obj)
  let [back, e2] = json.parse(s)
  acc = acc + back.version
}
let t1 = time.monotonic()
print(`json_roundtrip: acc=${acc} elapsed_ms=${t1 - t0}`)
