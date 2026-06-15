// decode_hot_loop.as
// ---------------------------------------------------------------------------
// DECODE — decoded-dispatch + superinstruction fusion (Units A + B).
//
// This program is a production-shaped hot loop that exercises the DECODE
// execution path: once a proto runs warm, the VM builds a lazy DECODED side
// representation of its bytecode (records, jump targets pre-resolved) and the
// sync lane drives those records instead of re-decoding bytes. Unit B then
// FUSES hot adjacent op pairs/triples into single records. The committed
// op-pair census (bench/DECODE_PAIR_CENSUS.md) made these the fused forms:
//
//   GetLocal;GetProp        — read a field off a local        (p.x)
//   GetLocal;GetProp;Add    — accumulate a field into a sum   (acc + p.x)
//   GetProp;Add             — add a field to the running total
//   GetLocal;GetLocal       — two locals back-to-back
//   GetLocal;Const          — local against a literal
//   Const;GetLocal          — literal against a local
//
// The body below is written to lean on exactly those shapes: small global
// functions called in a tight loop, plus an arithmetic pipeline over object
// fields. NO inlining is relied on (DECODE Unit C — speculative global-fn
// inlining — was evidence-dropped); the example proves the DECODED + FUSED
// path only.
//
// Everything is deterministic and run-to-completion (no I/O, no clock, no
// randomness), fully error-handled, and uses only the core language so it
// runs identically under `--no-default-features`. The output is byte-identical
// across run / --tree-walker / ASCRIPT_NO_SPECIALIZE=1 / ASCRIPT_NO_DECODE=1 /
// build+run .aso (the 7-way DECODE differential).
//
// (The breakpoint-during-hot-loop edge — a runtime byte patch invalidating a
// live decoded stream via the patch_epoch chokepoint — cannot be driven from
// an example program; it lives in tests/vm_decode.rs.)
// ---------------------------------------------------------------------------

// --- small global functions, called in the hot loop -----------------------
// These stay tiny so the loop body is dominated by GetLocal/GetProp/Add/Const
// records — the fused census shapes.
fn weight(p) {
  // GetLocal p; GetProp x  /  GetLocal p; GetProp y — the field-read fused form.
  return p.x * 3 + p.y * 7
}

fn blend(a, b) {
  // GetLocal a; GetLocal b — two-local fused form; Const folds in.
  return a * 2 + b - 1
}

fn clampStep(n, lo, hi) {
  // A small branchy helper exercising Const;GetLocal and GetLocal;Const.
  if (n < lo) {
    return lo
  }
  if (n > hi) {
    return hi
  }
  return n
}

// --- the data: a small fixed set of records --------------------------------
// Plain objects with `.x` / `.y` fields so the loop body emits GetProp records.
let points = [{x: 1, y: 2}, {x: 3, y: 5}, {x: 8, y: 13}, {x: 21, y: 34}, {x: 55, y: 89}]

// --- the hot loop ----------------------------------------------------------
// Run the pipeline many times over the fixed data so the proto goes warm and
// the decoded + fused path takes over. The accumulation is integer-exact, so
// the result is deterministic and identical on every engine mode.
fn runPipeline(iterations) {
  let total = 0
  let n = len(points)
  for (iter in 0..iterations) {
    let i = 0
    while (i < n) {
      let p = points[i]
      // GetLocal p; GetProp x; Add — the accumulate-a-field fused triple.
      let w = weight(p)
      let b = blend(p.x, p.y)
      let stepped = clampStep(w + b, 0, 10000)
      total = total + stepped
      i = i + 1
    }
  }
  return total
}

print("=== DECODE hot loop: decoded + fused dispatch ===")

// A modest iteration count: warm enough to exercise the decoded path, small
// enough to run fast and stay deterministic.
let result = runPipeline(2000)
print(`pipeline total over 2000 iterations: ${result}`)

// --- error handling: a guarded variant -------------------------------------
// A wrong-shaped record would Tier-2 panic on the missing field; recover wraps
// the call so a malformed input is a caught, reported result rather than an
// abort. (Use the arrow form per the recover carry-forward note in CLAUDE.md.)
fn weightStrict(p) {
  if (p.x == nil || p.y == nil) {
    // Tier-2 panic, but caught by the recover wrapper below.
    [][999]
  }
  return weight(p)
}

print("\n=== guarded pipeline (recover around a malformed record) ===")

let mixed = [{x: 4, y: 6}, {x: nil, y: 9}, {x: 2, y: 2}]
let [guarded, guardErr] = recover(() => {
  let s = 0
  let j = 0
  while (j < len(mixed)) {
    s = s + weightStrict(mixed[j])
    j = j + 1
  }
  return s
})
if (guardErr != nil) {
  print(`caught malformed record: ${guardErr.message}`)
} else {
  print(`guarded total (unexpected — no error): ${guarded}`)
}

// A clean guarded run after the caught panic proves the engine state is intact.
let cleanRecords = [{x: 1, y: 1}, {x: 2, y: 2}, {x: 3, y: 3}]
let [cleanTotal, cleanErr] = recover(() => {
  let s = 0
  let k = 0
  while (k < len(cleanRecords)) {
    s = s + weightStrict(cleanRecords[k])
    k = k + 1
  }
  return s
})
print(`clean guarded total: ${cleanTotal} (err: ${cleanErr})`)

// --- a match over the fused-loop result ------------------------------------
// Exercises a different record shape after the hot loop, keeping the program
// varied while staying deterministic.
let bucket = match result {
  0..1000 => "small",
  1000..1000000 => "medium",
  _ => "large",
}
print(`\nresult bucket: ${bucket}`)
