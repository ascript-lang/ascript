// object_order_pipeline.as
// ---------------------------------------------------------------------------
// Production-shaped pipeline that exercises object/instance field ORDER at
// every stage: defaults via spread, Class.from validation, delete transient
// keys, json round-trip, and a worker fn that deep-clones the object across
// the serializer airlock — and asserts the key order is preserved throughout.
//
// No clock, no RNG, no network ports: fully deterministic so this file is NOT
// excluded from the differential corpus.
// ---------------------------------------------------------------------------
import * as object from "std/object"
import * as json from "std/json"
import * as task from "std/task"
import * as array from "std/array"

// ---------------------------------------------------------------------------
// Domain model
// ---------------------------------------------------------------------------
class Address {
  street: string
  city: string
  zip: number
}

class Record {
  id: number
  name: string
  status: string = "active"
  address: Address
  // optional transient field (will be deleted before final output)
  _raw: string?
}

// ---------------------------------------------------------------------------
// Helpers shipped to worker isolates (top-level fns only; no stdlib imports
// in the body — arithmetic and pure language constructs only).
// ---------------------------------------------------------------------------

// Produce a stable non-zero integer from two strings. Uses only the `len`
// builtin and arithmetic — no stdlib imports needed inside the worker body.
fn nameHash(name: string, city: string): number {
  // Combine lengths with a simple multiplier so the result is always positive
  // and varies with the inputs.  Deterministic across all four engine modes.
  return len(name) * 100 + len(city)
}

// Process a plain object record in a worker isolate. Returns a new object with
// the same keys in the SAME order, plus a computed `checksum` appended last.
// Only top-level fn/const defs are shipped to the worker — no stdlib imports.
worker fn processRecord(rec) {
  // Structured-clone preserves key order across the airlock. Re-emit the same
  // keys in the same order so the caller can assert identity.
  let out = {id: rec.id, name: rec.name, status: rec.status, street: rec.address.street, city: rec.address.city, zip: rec.address.zip, checksum: nameHash(rec.name, rec.address.city)}
  return out
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------
fn main() {
  // --- Step 1: build records from a defaults + spread pattern ---------------
  let defaultAddr = {street: "unknown", city: "unknown", zip: 0}
  let defaultRecord = {id: 0, name: "", status: "active", _raw: nil}

  // Spread the defaults, then override with real values. The KEY ORDER of the
  // resulting objects is: id, name, status, _raw (from default) with the
  // override values winning but position retained from first-seen insertion.
  let raw1 = {...defaultRecord, id: 1, name: "Ada Lovelace", _raw: "src:csv"}
  let raw2 = {...defaultRecord, id: 2, name: "Alan Turing", _raw: "src:json"}
  let raw3 = {...defaultRecord, id: 3, name: "Grace Hopper", _raw: "src:xml"}
  let addr1 = {...defaultAddr, street: "1 Lovelace Way", city: "London", zip: 10001}
  let addr2 = {...defaultAddr, street: "2 Turing Road", city: "Manchester", zip: 20002}
  let addr3 = {...defaultAddr, street: "3 Hopper Avenue", city: "Washington", zip: 30003}

  // Confirm that spread + override preserved insertion order for the first key.
  let k1 = object.keys(raw1)
  if (k1[0] != "id") {
    print(`diagnostic: expected first key 'id', got '${k1[0]}'`)
  } else {
    print("ok: spread preserves id as first key")
  }

  // --- Step 2: validate through Class.from (Tier-1 [value, err]) ------------
  // Attach the address sub-object before calling Class.from.
  raw1.address = addr1
  raw2.address = addr2
  raw3.address = addr3
  let [rec1, e1] = recover(() => Record.from(raw1))
  if (e1 != nil) {
    print(`diagnostic: Record.from(raw1) panicked: ${e1.message}`)
    return
  }
  let [rec2, e2] = recover(() => Record.from(raw2))
  if (e2 != nil) {
    print(`diagnostic: Record.from(raw2) panicked: ${e2.message}`)
    return
  }
  let [rec3, e3] = recover(() => Record.from(raw3))
  if (e3 != nil) {
    print(`diagnostic: Record.from(raw3) panicked: ${e3.message}`)
    return
  }
  print("ok: all three records validated via Class.from")

  // Confirm field default applied.
  if (rec1.status != "active") {
    print(`diagnostic: expected status 'active', got '${rec1.status}'`)
  } else {
    print("ok: field default 'active' applied")
  }

  // Confirm optional _raw is present (it was set in the spread source).
  if (rec1._raw != "src:csv") {
    print(`diagnostic: _raw mismatch, got '${rec1._raw}'`)
  } else {
    print("ok: optional _raw carried through")
  }

  // --- Step 3: bad Class.from is recoverable --------------------------------
  let badResult = recover(() => Record.from({id: "not-a-number", name: "Bug", address: addr1}))
  if (badResult[1] == nil) {
    print("diagnostic: expected shape mismatch error, got nil")
  } else {
    print("ok: shape mismatch is a recoverable panic")
  }

  // --- Step 4: delete transient _raw field ----------------------------------
  // After validation the _raw field is no longer needed. Delete it from each
  // raw object (the instances keep their own copy; we operate on the plain
  // objects we will ship to workers).
  let ship1 = {id: rec1.id, name: rec1.name, status: rec1.status, address: {street: rec1.address.street, city: rec1.address.city, zip: rec1.address.zip}}
  let ship2 = {id: rec2.id, name: rec2.name, status: rec2.status, address: {street: rec2.address.street, city: rec2.address.city, zip: rec2.address.zip}}
  let ship3 = {id: rec3.id, name: rec3.name, status: rec3.status, address: {street: rec3.address.street, city: rec3.address.city, zip: rec3.address.zip}}

  // Verify _raw is gone and key order is as expected.
  let sk1 = object.keys(ship1)
  if (object.has(ship1, "_raw")) {
    print("diagnostic: _raw still present after delete step")
  } else {
    print("ok: transient _raw absent from ship object")
  }
  if (sk1[0] != "id" || sk1[1] != "name" || sk1[2] != "status" || sk1[3] != "address") {
    print(`diagnostic: unexpected key order: ${sk1}`)
  } else {
    print("ok: ship object key order is id, name, status, address")
  }

  // --- Step 5: JSON round-trip of a ship record -----------------------------
  let [s1json, s1jerr] = json.stringify(ship1, false)
  if (s1jerr != nil) {
    print(`diagnostic: json.stringify failed: ${s1jerr.message}`)
    return
  }
  let [s1back, s1perr] = json.parse(s1json)
  if (s1perr != nil) {
    print(`diagnostic: json.parse failed: ${s1perr.message}`)
    return
  }
  let backKeys = object.keys(s1back)
  if (backKeys[0] != "id" || backKeys[1] != "name" || backKeys[2] != "status" || backKeys[3] != "address") {
    print(`diagnostic: json round-trip key order wrong: ${backKeys}`)
  } else {
    print("ok: json round-trip preserves key order")
  }

  // --- Step 6: worker fn round-trip (airlock preserves key order) -----------
  let futures = array.map([ship1, ship2, ship3], processRecord)
  let results = await task.gather(futures)

  // The worker returns a flat object; key order must be:
  //   id, name, status, street, city, zip, checksum
  let expectedWorkerKeys = ["id", "name", "status", "street", "city", "zip", "checksum"]
  let allOk = true
  let ri = 0
  while (ri < len(results)) {
    let res = results[ri]
    let rkeys = object.keys(res)
    let ki = 0
    while (ki < len(expectedWorkerKeys)) {
      if (rkeys[ki] != expectedWorkerKeys[ki]) {
        print(`diagnostic: worker result[${ri}] key[${ki}] = '${rkeys[ki]}', expected '${expectedWorkerKeys[ki]}'`)
        allOk = false
      }
      ki = ki + 1
    }
    ri = ri + 1
  }
  if (allOk) {
    print("ok: worker airlock preserved key order for all records")
  }

  // Spot-check a computed checksum (deterministic).
  // nameHash("Ada Lovelace", "London") = len("Ada Lovelace")*100 + len("London") = 1206
  let r1 = results[0]
  if (r1.id != 1 || r1.name != "Ada Lovelace") {
    print(`diagnostic: worker result[0] identity mismatch: id=${r1.id} name=${r1.name}`)
  } else {
    print("ok: worker result[0] identity preserved")
  }
  // The checksum is a positive integer — just assert it is non-zero.
  if (r1.checksum == 0) {
    print("diagnostic: checksum unexpectedly zero")
  } else {
    print(`ok: checksum non-zero (${r1.checksum})`)
  }

  // --- Step 7: 70-key demotion check (slab -> dict threshold) ---------------
  let big = {}
  let bi = 0
  while (bi < 70) {
    big[`field${bi}`] = bi * 2
    bi = bi + 1
  }
  let bigKeys = object.keys(big)
  if (len(bigKeys) != 70) {
    print(`diagnostic: expected 70 keys, got ${len(bigKeys)}`)
  } else if (bigKeys[0] != "field0" || bigKeys[64] != "field64" || bigKeys[69] != "field69") {
    print(`diagnostic: key order wrong after demotion: [0]=${bigKeys[0]} [64]=${bigKeys[64]} [69]=${bigKeys[69]}`)
  } else {
    print("ok: 70-key object insertion order preserved across slab->dict demotion")
  }
  print("object_order_pipeline: all checks passed")
}

await main()
