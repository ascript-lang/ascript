// Map literals (SP2 §3): `#{ keyExpr: valueExpr, … }` evaluates to a real
// `Value::Map` with ARBITRARY evaluated keys — unlike object literals `{a: 1}`
// (where `a` is the literal key name), a map-literal key is the VALUE of an
// expression. Runs byte-identically on the bytecode VM and the `--tree-walker`
// oracle.
import * as map from "std/map"

// `#{}` is an empty map; populated maps keep string/number/bool/nil keys.
let empty = #{}
print(type(empty)) // map
print(empty) // map {}
let scores = #{ "alice": 10, "bob": 7 }
print(type(scores)) // map
print(map.get(scores, "alice")) // 10
print(map.get(scores, "bob")) // 7

// Numeric, bool, and nil keys are all allowed (Map canonicalization rules).
let mixed = #{ 1: "one", true: "yes", nil: "none" }
print(map.get(mixed, 1)) // one
print(map.get(mixed, true)) // yes
print(map.get(mixed, nil)) // none

// The KEY is EVALUATED: `k` keys by its VALUE "dynamic", not the name "k".
let k = "dynamic"
let dyn = #{ k: 42, 1 + 1: "two" }
print(map.get(dyn, "dynamic")) // 42
print(map.get(dyn, 2)) // two

// Later-key-wins: a duplicate key keeps the LAST value (first-seen position).
let dup = #{ 1: "a", 1: "b" }
print(map.get(dup, 1)) // b

// Maps interoperate with the whole std/map API.
let m = #{ "x": 1 }
map.set(m, "y", 2)
print(map.has(m, "y")) // true
print(map.keys(m)) // ["x", "y"]
print("map_literals ok")
