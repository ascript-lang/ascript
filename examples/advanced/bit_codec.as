// bit_codec.as
// ---------------------------------------------------------------------------
// A self-hosted LEB128 variable-length integer codec — the kind of bit-exact,
// wrapping-and-bitwise work that was IMPOSSIBLE before AScript's numeric model
// (NUM). Every byte is an `int`; encoding shifts and masks with `<< >> & |`;
// the FNV-1a checksum uses the wrapping multiply `*%`; decoding is fully
// error-handled (truncated input and over-long encodings are recoverable
// errors, never panics).
//
// LEB128 (unsigned): split the value into 7-bit groups, little-endian; every
// byte but the last sets its high bit (0x80) as a "more bytes follow" marker.
// ---------------------------------------------------------------------------

import { push } from "std/array"

// ---- Encode: int -> array<int> (the wire bytes) ----------------------------
fn encodeVarint(value: int): array<int> {
  if (value < 0) {
    // This unsigned codec rejects negatives at the call boundary; a caller that
    // needs signed values would zig-zag encode first.
    return []
  }
  let out = []
  let v = value
  while (true) {
    let group = v & 0x7F      // low 7 bits
    v = v >> 7                // arithmetic shift; v >= 0 so it stays logical
    if (v == 0) {
      push(out, group)        // last byte: high bit clear
      break
    }
    push(out, group | 0x80)   // more bytes follow: set the continuation bit
  }
  return out
}

// ---- Decode: (bytes, start) -> [value, nextPos, err] -----------------------
// A Tier-1 style result triple: on success `err` is nil; on a malformed stream
// (truncated, or more than 64 bits of payload) `err` is a message string and
// `value` is 0.
fn decodeVarint(bytes: array<int>, start: int): array<any> {
  let result = 0
  let shift = 0
  let pos = start
  while (pos < len(bytes)) {
    let byte = bytes[pos]
    result = result | ((byte & 0x7F) << shift)
    pos = pos + 1
    if ((byte & 0x80) == 0) {
      return [result, pos, nil]          // continuation bit clear → done
    }
    shift = shift + 7
    if (shift >= 64) {
      return [0, pos, "varint exceeds 64 bits"]
    }
  }
  return [0, pos, "truncated varint"]
}

// ---- FNV-1a checksum over the encoded stream (wrapping arithmetic) ----------
// Proof that exact modular integer math works: the classic 64-bit FNV-1a hash
// needs wrapping multiply (`*%`) and bitwise XOR (`^`) — neither expressible
// without the numeric model.
fn fnv1a(bytes: array<int>): int {
  let h = 0x811C9DC5            // FNV offset basis (32-bit seed, fits an int)
  let prime = 0x01000193        // FNV prime
  for (b of bytes) {
    h = (h ^ b) *% prime         // *% wraps two's-complement — no overflow panic
  }
  return h & 0xFFFFFFFF          // fold to 32 bits
}

fn main() {
  print("=== LEB128 varint codec ===")

  // --- round-trip a spread of values ------------------------------------
  let samples = [0, 1, 127, 128, 300, 16384, 0xDEADBEEF, 9007199254740992]
  let allOk = true
  for (n of samples) {
    let wire = encodeVarint(n)
    let [decoded, pos, err] = decodeVarint(wire, 0)
    if (err != nil) {
      print(`  ${n}: decode error: ${err}`)
      allOk = false
    } else if (decoded != n || pos != len(wire)) {
      print(`  ${n}: ROUND-TRIP MISMATCH -> ${decoded} (pos ${pos})`)
      allOk = false
    } else {
      print(`  ${n} -> ${wire} -> ${decoded}  (checksum ${fnv1a(wire)})`)
    }
  }
  print(`round-trips ${allOk ? "all passed" : "FAILED"}`)

  // --- a packed stream of several varints, decoded in sequence ----------
  print("\n=== Packed stream ===")
  let stream = []
  for (n of [1, 200, 70000]) {
    for (b of encodeVarint(n)) { push(stream, b) }
  }
  print(`stream bytes: ${stream}`)
  let cursor = 0
  let values = []
  let streamOk = true
  while (cursor < len(stream)) {
    let [v, next, err] = decodeVarint(stream, cursor)
    if (err != nil) {
      print(`  decode failed at ${cursor}: ${err}`)
      streamOk = false
      break
    }
    push(values, v)
    cursor = next
  }
  if (streamOk) {
    print(`decoded values: ${values}`)
  }

  // --- error cases are recoverable, not panics --------------------------
  print("\n=== Malformed input ===")
  // A lone continuation byte (high bit set, no terminator) is truncated.
  let [_v1, _p1, truncErr] = decodeVarint([0x80], 0)
  print(`truncated -> ${truncErr}`)
  // Ten continuation bytes overflow the 64-bit payload budget.
  let tooLong = [0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80]
  let [_v2, _p2, longErr] = decodeVarint(tooLong, 0)
  print(`over-long  -> ${longErr}`)
}

main()
