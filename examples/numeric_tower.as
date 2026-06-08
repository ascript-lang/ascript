// numeric_tower.as — the AScript numeric tower (NUM): int / float / decimal,
// promotion rules, conversions, exact cross-subtype comparison, and the
// `instanceof int|float|number` type guards.

import * as decimal from "std/decimal"
import { trunc, floor, ceil, round, abs, floordiv, divmod, ceildiv } from "std/math"

// ---- Three distinct runtime subtypes, one user concept ("a number") --------
print(type(5))               // int
print(type(5.0))             // float
print(type(decimal.from(5))) // decimal

// The `type()` reflection above is the authoritative subtype distinction; a
// `float` ALWAYS prints with a trailing decimal so the two subtypes are
// visually distinguishable (an `int` prints with none).
print(5)                     // 5
print(5.0)                   // 5.0

// ---- Promotion: any mixed int/float arithmetic promotes the int to float ---
print(1 + 2)                 // 3    (int + int → int)
print(type(1 + 2.0))         // float  (int promoted → float result)
print(10 / 4)                // 2    (int / int truncates)
print(10.0 / 4)              // 2.5  (a float operand → real division)

// ---- decimal interop (exact, opt-in; NOT part of `number`) -----------------
let price = decimal.from("0.1")
let total = price + decimal.from("0.2")
print(decimal.toString(total))           // 0.3   (exact — no float drift)
// An int promotes into decimal arithmetic exactly.
print(decimal.toString(decimal.from("1.5") + 2))   // 3.5

// ---- Rounding → int (NUM §4: floor/ceil/round/trunc return an `int`) --------
print(trunc(3.9))            // 3   (float → int, toward zero)
print(trunc(-3.9))           // -3
print(floor(-3.1))           // -4  (floor rounds down)
print(ceil(3.1))             // 4   (ceil rounds up)
print(round(2.5))            // 3   (round half away from zero)
print(type(floor(3.9)))      // int (a rounding fn yields an int, not a float)

// ---- abs is subtype-preserving (the ONLY such math fn) ---------------------
print(abs(-5))               // 5     (int in → int out)
print(type(abs(-5)))         // int
print(abs(-2.5))             // 2.5   (float in → float out)
print(type(abs(-2.5)))       // float

// ---- Integer division helpers (int → int) ----------------------------------
print(floordiv(-7, 2))       // -4    (floors toward −∞, unlike truncating `/`)
print(ceildiv(7, 2))         // 4     (rounds toward +∞)
print(divmod(17, 5))         // [3, 2]   (q floored, r matching: 17 == 3*5 + 2)

// ---- Exact cross-subtype comparison (no lossy promotion) -------------------
print(1 == 1.0)              // true   (equal mathematical value)
print(2 < 2.5)               // true
// The 2^53 boundary: an int not exactly representable as f64 compares EXACTLY.
let n = 9007199254740993     // 2^53 + 1
print(n == 9007199254740992) // false  (exact — they are different integers)
print(n == n)                // true

// ---- instanceof int|float|number as runtime type guards --------------------
fn describe(x: number): string {
  if (x instanceof int) { return "an int" }
  if (x instanceof float) { return "a float" }
  return "some number"
}
print(describe(42))          // an int
print(describe(3.14))        // a float
print(5 instanceof int)      // true
print(5 instanceof float)    // false
print(5.0 instanceof float)  // true
print(5 instanceof number)   // true
print(5.0 instanceof number) // true
