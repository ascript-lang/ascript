// object_order_stress.as
// ---------------------------------------------------------------------------
// Stress-test object/instance field ORDER. Every section prints the object
// so the differential corpus bites on key ordering across all four modes:
//   tree-walker == specialized-VM == generic-VM == .aso
// ---------------------------------------------------------------------------
import * as object from "std/object"

// === 1. Literal order ===
print("--- 1. literal order ---")
let lit = {a: 1, b: 2, c: 3}
print(lit)
// Duplicate key: later value wins, FIRST position retained.
let dup = {a: 1, b: 9, a: 2}
print(dup) // {a: 2, b: 9}  (first position for 'a')

// === 2. Add-order via dot-set and bracket-set ===
print("--- 2. add-order dot and bracket ---")
let o2 = {x: 10}
o2.y = 20
o2["z"] = 30
print(o2) // {x: 10, y: 20, z: 30}

// === 3. Spread: later-wins-first-position + self-spread ===
print("--- 3. spread merge ---")
let base = {a: 1, b: 2}
let over = {b: 99, c: 3}
let merged = {...base, ...over}
print(merged) // {a: 1, b: 99, c: 3}
// Self-spread then override: spreading 'o' twice — second wins for any
// overlapping key; 'z' is added last.
let self2 = {...base, ...base, z: 0}
print(self2) // {a: 1, b: 2, z: 0}

// === 4. Rest collection ===
print("--- 4. rest collection ---")
let full = {a: 1, b: 2, c: 3, d: 4}
let {a, ...rest} = full
print(a) // 1
print(rest) // {b: 2, c: 3, d: 4}

// === 5. delete then re-add (stale-shape reproducer) ===
print("--- 5. delete + re-add ---")
let del5 = {x: 10, y: 20, z: 30}
object.delete(del5, "y")
print(del5) // {x: 10, z: 30}
del5.y = 99
print(del5) // {x: 10, z: 30, y: 99}  (y re-added at tail)
print(del5.x) // 10
print(del5.z) // 30
print(del5.y) // 99

// === 6. fromEntries / entries round-trip preserves order (core, no json) ===
print("--- 6. entries round-trip ---")
let rto = {first: 1, second: 2, third: 3}
// Rebuild the object from its own entries: insertion order must survive.
let rtBack = object.fromEntries(object.entries(rto))
print(rtBack) // {first: 1, second: 2, third: 3}
print(object.keys(rtBack)) // [first, second, third]

// === 7. 70-key loop-built object (crosses SLAB_MAX_KEYS=64, forces demotion) ===
print("--- 7. 70-key object (slab -> dict demotion) ---")
let big = {}
let i = 0
while (i < 70) {
  big[`k${i}`] = i
  i = i + 1
}
print(len(big)) // 70
// First and last keys in insertion order.
print(big.k0) // 0
print(big.k69) // 69
// Confirm insertion order via object.keys on a SLICE of the big object.
let bigKeys = object.keys(big)
print(bigKeys[0]) // k0
print(bigKeys[10]) // k10
print(bigKeys[64]) // k64  (first key after demotion threshold)
print(bigKeys[69]) // k69

// === 8. object.keys / values / entries on a small object ===
print("--- 8. keys / values / entries ---")
let kve = {alpha: 1, beta: 2, gamma: 3}
print(object.keys(kve)) // [alpha, beta, gamma]
print(object.values(kve)) // [1, 2, 3]
let ents = object.entries(kve)
// Each entry is [key, value]; verify the first entry.
print(ents[0]) // [alpha, 1]
print(ents[1]) // [beta, 2]
print(ents[2]) // [gamma, 3]

// === 9. Same matrix on CLASS INSTANCES ===
print("--- 9. class instances ---")

class Base {
  inherited: string = "base_default"
}

class Widget extends Base {
  id: number
  label: string = "unlabeled"
  color: string?
  fn init(id, label) {
    self.id = id
    self.label = label
    self.extra = "added_in_init" // undeclared field added in init
  }
}

let w = Widget(1, "btn")
print(w)
// Verify each field reads correctly.
print(w.id) // 1
print(w.label) // btn
print(w.inherited) // base_default
print(w.color) // nil
print(w.extra) // added_in_init

// Post-construction undeclared field add.
w.dynamic = "post"
print(w.dynamic) // post
print(w) // includes dynamic key

// Snapshot the instance's public fields as a plain object and confirm the key
// order survives an entries -> fromEntries round-trip (core, no json feature).
let wSnap = {id: w.id, label: w.label, inherited: w.inherited, extra: w.extra, dynamic: w.dynamic}
print(wSnap)
// Keys must arrive in insertion order: id, label, inherited, extra, dynamic.
let wBack = object.fromEntries(object.entries(wSnap))
print(object.keys(wBack))

print("object_order_stress: all done")
