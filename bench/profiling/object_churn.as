// PROFILE TARGET: recommendations #1 (hashing) + #3/#5 (allocation per value).
// Allocates a fresh 4-field object every iteration (500k) and reads its fields.
// Object construction inserts keys into an IndexMap (SipHash on insert) and
// allocates a Cc<ObjectCell>; this isolates the per-object alloc + hash cost that
// a faster hasher and cheaper value representation would cut.
import * as time from "std/time"

let t0 = time.monotonic()
let total = 0
for (i in 0..6000000) {
  let o = { id: i, name: "node", x: i * 2, y: i + 1 }
  total = total + o.x + o.y + o.id
}
let t1 = time.monotonic()
print(`object_churn: total=${total} elapsed_ms=${t1 - t0}`)
