// typed_pipeline.as
// ---------------------------------------------------------------------------
// A typed data-processing pipeline demonstrating contract elision (ELIDE).
//
// The pipeline has two distinct halves:
//
//   TYPED INTERIOR — every function and binding is fully annotated. The static
//   checker proves these checks redundant; under `--elide` (or `ascript build
//   --elide`) the per-argument type contracts are removed from the bytecode.
//   The behavior is byte-identical with or without elision.
//
//   GRADUAL BOUNDARY — raw records arrive as `any`-typed values (e.g. parsed
//   JSON or an external API response). The boundary validator coerces and
//   checks each field; the call from untyped → typed code keeps its runtime
//   contract because the checker cannot prove the kind of an `any`-typed value.
//   The `recover` below demonstrates that the check still fires and catches
//   a malformed record at runtime.
//
// Run clean on both engines; fmt-idempotent; zero type-* diagnostics.
// ---------------------------------------------------------------------------
import * as array from "std/array"

// ---------------------------------------------------------------------------
// Domain types — the typed interior
// ---------------------------------------------------------------------------
class Record {
  id: int
  label: string
  value: int
}

// ---------------------------------------------------------------------------
// Typed interior: pure transforms
//
// Every parameter and return is annotated with a primitive ElideSafe type.
// At each call site below the arguments are literals or come from proven calls
// — all three elision conditions hold (ElideSafe destination, concrete Yes,
// anchored argument). Under --elide the per-argument checks are dropped;
// without --elide they run and pass silently.
// ---------------------------------------------------------------------------
fn normalize(value: int, floor: int, cap: int): int {
  if (value < floor) {
    return floor
  }
  if (value > cap) {
    return cap
  }
  return value
}

fn score(value: int, weight: int): int {
  // Annotated let — also a proven site. Both `value` and `weight` are
  // annotated params; int * int produces int — proven and elided under --elide.
  let raw: int = value * weight
  return normalize(raw, 0, 10000)
}

fn classify(s: int): string {
  if (s >= 8000) {
    return "high"
  }
  if (s >= 4000) {
    return "mid"
  }
  return "low"
}

fn labelFor(rec: Record): string {
  return `${rec.label}(${rec.id})`
}

// A scored result assembled inside the typed interior. The call to `score`
// with `rec.value` and an int literal is a proven site: `rec.value` is a
// typed field of a known Record (the call just checked it via Class.from),
// and the literal 12 is an anchored int.
fn processRecord(rec: Record): string {
  let s: int = score(rec.value, 12)
  let tier: string = classify(s)
  let lbl: string = labelFor(rec)
  return `${lbl}: score=${s} tier=${tier}`
}

// ---------------------------------------------------------------------------
// Gradual boundary
//
// Raw data arrives as `any`-typed objects (e.g. parsed from JSON or an
// external source). `validateRecord` uses `Record.from` to coerce and
// validate — if any field is missing or the wrong type it returns a
// [nil, err] pair. This is the boundary where elision does NOT apply: the
// `raw` argument is `any`-typed, so the call cannot be statically proven.
// ---------------------------------------------------------------------------
fn validateRecord(raw: any) {
  // Record.from(obj) validates each field at runtime.  The `[value, err]`
  // pair is the standard AScript fallible-call idiom.
  let rec = recover(() => Record.from(raw))
  if (rec[1] != nil) {
    return [nil, rec[1]]
  }
  return [rec[0], nil]
}

// ---------------------------------------------------------------------------
// Pipeline: accept an array of any-typed records, validate each at the
// boundary, and run the typed interior on every valid one.
// ---------------------------------------------------------------------------
fn runPipeline(inputs: any) {
  // inputs is any-typed — a real ingress would be json.parse output or a
  // network response body.  We walk it as an array.
  let results = []
  let errors = []
  for (raw of inputs) {
    let [rec, verr] = validateRecord(raw)
    if (verr != nil) {
      array.push(errors, verr.message)
      continue
    }
    // rec is now a validated Record — the typed interior runs proven fast.
    let line = processRecord(rec)
    array.push(results, line)
  }
  return [results, errors]
}

// ---------------------------------------------------------------------------
// Exercise the pipeline
// ---------------------------------------------------------------------------

// Well-formed records — these will pass the boundary check and be processed
// entirely inside the typed interior.
let goodInputs = [{id: 1, label: "alpha", value: 750}, {id: 2, label: "beta", value: 300}, {id: 3, label: "gamma", value: 900}]

let [results, errors] = runPipeline(goodInputs)
print("=== pipeline results ===")
for (line of results) {
  print(`  ${line}`)
}
// expect 3 results, 0 errors
assert(len(results) == 3, "expected 3 results")
assert(len(errors) == 0, "expected 0 errors")

// ---------------------------------------------------------------------------
// Demonstrate the boundary check: malformed records keep their runtime check
// and produce [nil, err] instead of panicking through to the typed interior.
// ---------------------------------------------------------------------------
// missing `id` field — Record.from will reject this
// wrong type in `value` field — int expected, string present
let mixedInputs = [{id: 10, label: "delta", value: 500}, {label: "bad-no-id", value: 200}, {id: 12, label: "epsilon", value: "not-a-number"}, {id: 13, label: "zeta", value: 100}]

let [mixedResults, mixedErrors] = runPipeline(mixedInputs)
print("\n=== mixed run ===")
for (line of mixedResults) {
  print(`  ok: ${line}`)
}
for (msg of mixedErrors) {
  print(`  boundary caught: ${msg}`)
}
// 2 valid records, 2 boundary rejections
assert(len(mixedResults) == 2, `expected 2 good results, got ${len(mixedResults)}`)
assert(len(mixedErrors) == 2, `expected 2 boundary errors, got ${len(mixedErrors)}`)

// ---------------------------------------------------------------------------
// Explicitly show that the gradual-boundary contract still fires:
// a directly-proven typed call with a bad-type any arg panics at the
// contract site, not silently in the typed interior.
// ---------------------------------------------------------------------------
let badRaw: any = "not-a-record-at-all"
let panicResult = recover(() => Record.from(badRaw))
assert(panicResult[1] != nil, "expected boundary panic from non-object")
print(`\nboundary guard fires: ${panicResult[1].message}`)

print("\ntyped_pipeline ok")
