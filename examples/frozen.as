// object.freeze / object.isFrozen (SP2 §4): a SHALLOW, one-way runtime freeze
// of a mutable container (object / array / map / set / instance). After freezing,
// any in-place mutation is a Tier-2 panic; freezing returns the value (chainable)
// and is byte-identical on the bytecode VM and the `--tree-walker` oracle.
import * as object from "std/object"
import * as array from "std/array"
import * as map from "std/map"

// isFrozen reports false for a fresh container, true after freezing.
let config = {host: "localhost", port: 8080}
print(object.isFrozen(config)) // false
let same = object.freeze(config)
print(object.isFrozen(config)) // true
print(same == config) // true — freeze returns the value for chaining

// Freezing is shallow: a nested container is still mutable.
let outer = [[1, 2]]
object.freeze(outer)
outer[0][0] = 99
print(outer) // [[99, 2]]

// A non-container freeze is a no-op that returns the value unchanged.
print(object.freeze(42)) // 42
print(object.isFrozen(42)) // false

// A deepClone of a frozen container is itself UNFROZEN (fresh copy).
let frozen = object.freeze({a: 1})
let clone = object.deepClone(frozen)
print(object.isFrozen(clone)) // false
clone.a = 2 // OK — the clone is not frozen
print(clone) // {a: 2}

// Non-frozen containers mutate normally; freezing is idempotent.
let live = [1]
array.push(live, 2)
print(live) // [1, 2]
let m = map.new()
object.freeze(m)
object.freeze(m) // idempotent — still frozen, no error
print(object.isFrozen(m)) // true

print("frozen ok")
