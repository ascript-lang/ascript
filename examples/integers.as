// integers.as — the AScript integer model (NUM).
//
// `int` (i64) is the default for integer literals; division is type-directed
// (`int / int` truncates toward zero); overflow is CHECKED by default with
// explicit wrapping operators (`+% -% *%`) as the escape hatch; bitwise/shift
// operators use Go-style precedence; Unicode code points are `int`s.

import { codepoints, from_codepoints, code_at } from "std/string"
import { map } from "std/array"

// ---- Literals: every base + underscores ------------------------------------
let dec = 1_000_000          // decimal with digit separators
let hex = 0xFF               // 255
let bin = 0b1010             // 10
let oct = 0o17               // 15
print(dec)                   // 1000000
print(hex)                   // 255
print(bin)                   // 10
print(oct)                   // 15
print(type(hex))             // int

// ---- Type-directed division (truncates toward zero) ------------------------
print(7 / 2)                 // 3   (int / int → int)
print(-7 / 2)                // -3  (truncate toward zero, not floor)
print(7.0 / 2)               // 3.5 (a float operand → float division)
print(7 % 3)                 // 1   (remainder, sign follows dividend)
print(-7 % 3)                // -1

// ---- Checked overflow + explicit wrapping ----------------------------------
let maxInt = 9223372036854775807   // i64::MAX
// `+ - * **` TRAP on overflow with a recoverable Tier-2 panic.
let overflowed = recover(() => maxInt + 1)
print(overflowed)            // [nil, {message: "integer overflow in '+'"}]
// The wrapping operators wrap two's-complement and never panic.
print(maxInt +% 1)           // -9223372036854775808  (wraps to i64::MIN)

// ---- Bitwise packing / unpacking (a real 24-bit RGB color) -----------------
fn pack(r: int, g: int, b: int): int {
  return (r << 16) | (g << 8) | b
}
fn channel(rgb: int, shift: int): int {
  return (rgb >> shift) & 0xFF
}
let color = pack(0xAB, 0xCD, 0xEF)
print(color)                 // 11259375
print(channel(color, 16))    // 171  (0xAB)
print(channel(color, 8))     // 205  (0xCD)
print(channel(color, 0))     // 239  (0xEF)
print(~0)                    // -1   (bitwise NOT)

// ---- Code points are ints (the Go rune model — no char type) ---------------
let pts = codepoints("hi")           // [104, 105]
print(pts)
let shouted = from_codepoints(map(pts, (c) => c - 32))
print(shouted)                       // "HI"
print(code_at("ABC", 1))             // 66  (the codepoint of 'B')
