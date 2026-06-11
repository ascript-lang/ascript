// Integer/float boundary at 2^63.
//
// `i64::MAX` is 9223372036854775807. The float 9223372036854775808.0 is 2^63 —
// one past the int range and NOT representable as an int. AScript compares the two
// subtypes EXACTLY, so the float is never equal to, and never shares a map key
// with, the largest int.

import * as map from "std/map"

let maxInt = 9223372036854775807     // i64::MAX
let twoPow63 = 9223372036854775808.0 // 2^63, out of int range

// Exact cross-subtype equality: never collapses 2^63 down to i64::MAX.
print(twoPow63 == maxInt) // false

// Distinct map keys: the float and the int do not fold together. A map literal
// `#{…}` keys by the evaluated expression's VALUE, so number keys are allowed.
let m = #{ twoPow63: "float", maxInt: "int" }
print(len(m))               // 2
print(map.get(m, twoPow63)) // float
print(map.get(m, maxInt))   // int

// The largest integral float still in int range (2^63 - 2048) does fold.
let inRange = 9223372036854773760.0
print(inRange == 9223372036854773760) // true
