// Production-shaped resource cleanup using `defer`.
//
// This example demonstrates the patterns that matter most in real programs:
//
//   §1  Multi-resource acquisition with per-resource `defer` — LIFO teardown
//       ensures the last resource opened is the first closed, even when an
//       intermediate step fails and the function exits early via `?`.
//
//   §2  Panic unwind observed through `recover`, including the §3.6 suppressed-
//       note format: when the body ALREADY panics and a deferred cleanup call
//       ALSO panics, `recover` sees the ORIGINAL message plus a
//       "(suppressed panic in deferred call: <new>)" suffix — root cause first.
//
//   §3  Generator-owner pattern (§4.3): the owner function defers `gen.close()`,
//       not the generator itself. When `close()` is called on a suspended
//       generator, the generator's own defers do NOT run (dropping a generator
//       mid-body is not an unwind path — it is a drop). The owner's `defer` is
//       the correct place for cleanup that must happen regardless of how much of
//       the stream was consumed.
//
// Uses std/fs temp files under /tmp so it runs everywhere. Fully deterministic
// output — NOT in EXAMPLE_SKIPS (no server, no port, no external state).
import * as fs from "std/fs"

const TMP = "/tmp/ascript_defer_resources"

// ---------------------------------------------------------------------------
// §1  Multi-resource LIFO teardown
//
// Three temp files are opened in order (source, dest, log). Each gets its own
// `defer` at the point of acquisition. LIFO means log closes first, then dest,
// then source — exactly the reverse of the open order. The root temp directory
// is deferred FIRST (registered before the files) so it runs LAST — after all
// three file defers have fired.
// ---------------------------------------------------------------------------
fn multiResourceDemo() {
  // Register root cleanup FIRST so it runs LAST (after all file defers).
  defer (() => {
    fs.remove(TMP, true)
    print("removed temp dir")
  })()
  let srcPath = fs.join(TMP, "source.txt")
  let dstPath = fs.join(TMP, "dest.txt")
  let logPath = fs.join(TMP, "log.txt")

  // Write source content.
  let [_, wErr] = fs.write(srcPath, "hello from source\n")
  if (wErr != nil) {
    return Err(`write source: ${wErr.message}`)
  }
  print("opened source")
  defer (() => {
    print("closed source")
  })()

  // Copy to destination.
  let [srcData, rErr] = fs.read(srcPath)
  if (rErr != nil) {
    return Err(`read source: ${rErr.message}`)
  }
  let [_w2, dErr] = fs.write(dstPath, srcData)
  if (dErr != nil) {
    return Err(`write dest: ${dErr.message}`)
  }
  print("opened dest")
  defer (() => {
    print("closed dest")
  })()

  // Open a log.
  let [_w3, lErr] = fs.write(logPath, "session started\n")
  if (lErr != nil) {
    return Err(`write log: ${lErr.message}`)
  }
  print("opened log")
  defer (() => {
    print("closed log")
  })()
  print("all three resources open — LIFO teardown on exit")
  return Ok(nil)
}

fn runSection1() {
  fs.remove(TMP, true)
  let [_, mkErr] = fs.mkdir(TMP, true)
  if (mkErr != nil) {
    print(`setup failed: ${mkErr.message}`)
    return
  }
  let r = multiResourceDemo()
  if (r[1] != nil) {
    print(`error: ${r[1].message}`)
  }
}

print("=== §1 multi-resource LIFO ===")
runSection1()
// opened source
// opened dest
// opened log
// all three resources open — LIFO teardown on exit
// closed log
// closed dest
// closed source
// removed temp dir

// ---------------------------------------------------------------------------
// §2  Panic unwind + suppressed-note format (§3.6)
//
// Rule 1: A deferred call that panics on an otherwise-normal return becomes the
//         new outcome (the return value is discarded).
//
// Rule 3: When the body itself panics AND a deferred call ALSO panics, recover
//         sees the ORIGINAL message (the root cause the user must act on) with
//         the deferred panic appended as a suppressed note:
//           "<original> (suppressed panic in deferred call: <new>)"
//         Multiple deferred panics append left-to-right in LIFO drain order.
// ---------------------------------------------------------------------------

// Rule 1: defer panic supersedes a normal return.
fn rule1_deferPanicsOnReturn() {
  defer (() => {
    let _ = [][0]
  })() // will panic during cleanup
  print("body completed normally")
  return "success"
}

// Rule 3: body panic + deferred panic → original + suppressed note.
fn rule3_bodyAndDeferBothPanic() {
  defer (() => {
    let _ = [][99]
  })() // deferred panic (suppressed)
  let _ = [][0] // body panic (root cause, wins)
}

fn runSection2() {
  print("\n=== §2 panic-unwind + suppressed note ===")
  let r1 = recover(rule1_deferPanicsOnReturn)
  print(`rule 1 — return value: ${r1[0]}`) // nil (discarded)
  print(`rule 1 — error: ${r1[1].message}`) // the defer's panic
  let r3 = recover(rule3_bodyAndDeferBothPanic)
  print(`rule 3 — message: ${r3[1].message}`)
}

runSection2()
// === §2 panic-unwind + suppressed note ===
// body completed normally
// rule 1 — return value: nil
// rule 1 — error: index 0 out of bounds (len 0)
// rule 3 — message: index 0 out of bounds (len 0) (suppressed panic in deferred call: index 99 out of bounds (len 0))

// ---------------------------------------------------------------------------
// §3  Generator-owner pattern
//
// When a generator is consumed by a CALLER, the CALLER should defer gen.close()
// — not the generator. The generator's own defers run only at BODY COMPLETION
// (normal return or unwind), NOT on close()/drop. The owner pattern ensures
// the generator handle is always closed, even if only a few items are consumed.
//
// Note (spec §4.3): gen.close() is synchronous and drops the body mid-suspend —
// it does not inject an unwind into the generator body. The generator's defers
// would only fire if the body ran to completion.
// ---------------------------------------------------------------------------
fn* recordStream() {
  let records = [{id: 1, value: "alpha"}, {id: 2, value: "beta"}, {id: 3, value: "gamma"}]
  for (rec of records) {
    yield rec
  }
}

// The owner acquires the generator and defers gen.close(). Whether it consumes
// all items or exits early (here: only the first two), the generator handle is
// properly closed at function exit.
async fn processPartialStream() {
  let gen = recordStream()
  defer gen.close() // owner's cleanup — runs even if not all items consumed
  let first = gen.next()
  print(`record 1: ${first.value}`)
  let second = gen.next()
  print(`record 2: ${second.value}`)
}

fn runSection3() {
  print("\n=== §3 generator-owner pattern ===")
  await processPartialStream()
  print("generator owner exited cleanly")
}

runSection3()
// === §3 generator-owner pattern ===
// record 1: alpha
// record 2: beta
// generator owner exited cleanly
print("\ndefer_resources ok")
