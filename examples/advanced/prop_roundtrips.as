// Encode/decode LAWS expressed as properties (std/test).
//
// A "roundtrip law" says: decoding an encoded value recovers the original.
// Properties are the natural way to state such laws — the prop() runner checks
// them across many edge-biased random inputs instead of a handful of literals.
//
// Two laws here:
//   1. base64:  utf8Decode(base64Decode(base64Encode(s))) == s
//   2. json:    stringify(parse(stringify(x))) == stringify(x)   (idempotent)
//
// Both predicates RETURN A BOOL (required by the runner — a passing assert.*
// returns nil/falsy, which counts as a failure). Both DESTRUCTURE the [value,
// err] Tier-1 pairs the encode/decode/parse calls return and fold any error
// into `false`, so an unhandled fallible call can never masquerade as success.
//
// Every prop() passes an explicit { seed: N } so the run is byte-stable.
// Run with:  ascript test examples/advanced/prop_roundtrips.as
// A plain `ascript run` executes only the top-level demonstrations below
// (registered props run under `ascript test`).
import { prop, gen } from "std/test"
import * as encoding from "std/encoding"
import * as json from "std/json"

// ── Law 1: base64 is a lossless roundtrip over UTF-8 text ──
fn base64Roundtrips(s) {
  let enc = encoding.base64Encode(s) // string -> string (infallible)
  let dec = encoding.base64Decode(enc) // string -> [bytes, err]
  if (dec[1] != nil) {
    return false
  }
  let back = encoding.utf8Decode(dec[0]) // bytes  -> [string, err]
  if (back[1] != nil) {
    return false
  }
  return back[0] == s
}

// ── Law 2: JSON stringify/parse is idempotent on the serialized form ──
// Comparing the canonical strings sidesteps array/object reference equality
// and still proves the structure survives a full encode/decode cycle.
fn jsonRoundtrips(obj) {
  let s1 = json.stringify(obj) // value -> [string, err]
  if (s1[1] != nil) {
    return false
  }
  let parsed = json.parse(s1[0]) // string -> [value, err]
  if (parsed[1] != nil) {
    return false
  }
  let s2 = json.stringify(parsed[0])
  if (s2[1] != nil) {
    return false
  }
  return s1[0] == s2[0]
}

// Concrete demonstrations (deterministic top-level output for `ascript run`).
print(`base64 roundtrip "hello world": ${base64Roundtrips("hello world")}`)
print(`base64 roundtrip "" (empty): ${base64Roundtrips("")}`)
print(`json roundtrip {a,b,c}: ${jsonRoundtrips({a: 1, b: [2, 3], c: "x"})}`)
print(`json roundtrip nested: ${jsonRoundtrips({nested: {deep: [1, 2, {k: true}]}})}`)

// ── Registered properties (run under `ascript test`, explicit seeds) ──
prop("base64 roundtrips any string", [gen.string()], base64Roundtrips, {seed: 1})

// A fixed-shape generated object exercises the JSON law over varied payloads.
let objGen = gen.objectWith({id: gen.int(0, 1000000), name: gen.string({minLen: 0, maxLen: 12}), active: gen.bool(), tags: gen.arrayOf(gen.int(-100, 100))})
prop("json roundtrips an object shape", [objGen], jsonRoundtrips, {seed: 2})

print("roundtrip laws complete")
