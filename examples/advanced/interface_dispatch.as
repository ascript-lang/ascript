// Production-shaped interface dispatch: a `Codec` abstraction (encode/decode)
// selected at RUNTIME by structural conformance, with every fallible step on the
// `[value, err]` convention. The point: `pick_codec` cares that a value HAS the
// `encode`/`decode` methods — `instanceof Codec` — not where it sits in a class
// tree. New codecs conform structurally with no base class to inherit.
//
// This exercises only the RUNTIME half of interfaces (structural `instanceof`,
// `implements`, an interface-typed parameter as a contract). Static interface
// type-checking is a later milestone (TYPE).
import * as json from "std/json"
import * as strings from "std/string"

// The behavioral contract: a value that can round-trip a string payload.
interface Codec {
  fn name(): string
  fn encode(s): string
  fn decode(s): string
}

// EXPLICIT conformance — UpperCodec asserts it implements Codec.
class UpperCodec implements Codec {
  fn name(): string {
    return "upper"
  }
  fn encode(s): string {
    return strings.upper(s)
  }
  fn decode(s): string {
    return strings.lower(s)
  }
}

// STRUCTURAL conformance — ReverseCodec never names Codec, but it has all three
// methods, so `instanceof Codec` is still true and `pick_codec` accepts it.
class ReverseCodec {
  fn name(): string {
    return "reverse"
  }
  fn encode(s): string {
    return strings.reverse(s)
  }
  fn decode(s): string {
    return strings.reverse(s)
  }
}

// A type that is deliberately NOT a codec (missing `decode`): it must be rejected
// at the conformance gate, NOT crash mid-pipeline.
class HalfCodec {
  fn name(): string {
    return "half"
  }
  fn encode(s): string {
    return s
  }
}

// Select a codec by name from a registry of candidate values. The conformance
// check is the gate: a candidate that does not structurally satisfy `Codec` is
// skipped, so a bad entry can never reach the encode/decode path. Returns the
// `[value, err]` pair convention.
fn pick_codec(registry, want: string) {
  for (entry in registry) {
    if (entry instanceof Codec && entry.name() == want) {
      return [entry, nil]
    }
  }
  return [nil, `no codec named '${want}'`]
}

// Round-trip a payload through the chosen codec. `codec: Codec` is a runtime
// CONTRACT — a non-conforming argument is rejected the same way a class annotation
// would reject it, so callers cannot smuggle in a non-codec.
fn roundtrip(codec: Codec, payload: string) {
  let wire = codec.encode(payload)
  let back = codec.decode(wire)
  return [{wire: wire, back: back}, nil]
}

// The registry mixes a conforming-explicit, a conforming-structural, and a
// non-conforming value. `instanceof Codec` filters the last one out.
let registry = [UpperCodec(), ReverseCodec(), HalfCodec()]

// Happy path: resolve "upper", round-trip a payload, propagate failures with `?`.
fn demo(want: string, payload: string) {
  let codec = pick_codec(registry, want)?
  let result = roundtrip(codec, payload)?
  return [`${codec.name()}: ${payload} -> ${result.wire} -> ${result.back}`, nil]
}

let a = demo("upper", "Hello")
if (a[1] == nil) {
  print(a[0]) // upper: Hello -> HELLO -> hello
} else {
  print(`error: ${a[1]}`)
}

let b = demo("reverse", "abc")
if (b[1] == nil) {
  print(b[0]) // reverse: abc -> cba -> abc
} else {
  print(`error: ${b[1]}`)
}

// Failure path: "half" is in the registry but does NOT conform to Codec (no
// `decode`), so `pick_codec` skips it and reports a clean error — no crash.
let c = demo("half", "data")
print(c[0] == nil) // true
print(c[1]) // no codec named 'half'

// Missing name: also a clean `[nil, err]`.
let d = demo("zip", "data")
print(d[1]) // no codec named 'zip'

// How many registry entries actually conform to the Codec contract?
let conforming = 0
for (entry in registry) {
  if (entry instanceof Codec) {
    conforming = conforming + 1
  }
}
print(`conforming codecs: ${conforming}`) // conforming codecs: 2

// A non-instance is never a codec.
print(42 instanceof Codec) // false

// Persist a manifest of the available codecs via the JSON stdlib (proves the
// example links a real stdlib module like the other advanced files). `stringify`
// returns the `[value, err]` pair; `!` force-unwraps the value on success.
let names = []
for (entry in registry) {
  if (entry instanceof Codec) {
    names = [...names, entry.name()]
  }
}
let manifest = json.stringify({codecs: names})!
print(strings.contains(manifest, "upper")) // true
print("interface dispatch ok")
