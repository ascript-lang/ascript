// shared_heap_bench.as — SRV Part B headline number: zero-copy shared heap vs
// per-dispatch deep-clone.
//
// The worker airlock deep-clones EVERYTHING that crosses an isolate boundary
// (structured-clone of bytes). That is cheap for small per-request arguments but
// ruinous for a large read-only table that every isolate must read: a 5 MB routing
// table would be deep-copied into every isolate on every dispatch.
//
// `shared.freeze(table)` converts it ONCE into an immutable Arc graph. After that,
// handing the table to an isolate costs ONE Arc clone (a pointer + an atomic
// increment), independent of table size.
//
// This benchmark measures the per-dispatch cost of handing a table of N entries to
// a worker, two ways:
//   (A) DEEP-CLONE  — pass the plain (non-frozen) table; the airlock deep-copies it
//                     per call. Cost grows O(N).
//   (B) SHARED      — `shared.freeze` the table once, pass the Value::Shared; the
//                     airlock bumps an Arc per call. Cost is flat O(1).
//
// The worker returns only a tiny scalar (a single keyed lookup), so the measured
// per-call time is dominated by the ARGUMENT transport, isolating the airlock cost.
//
// Usage:
//   ASCRIPT_WORKERS=4 ascript run bench/shared_heap_bench.as
// Tagged output lines ("key=value") are parsed by run_shared_heap_bench.sh.

import * as shared from "std/shared"
import * as task from "std/task"
import * as time from "std/time"
import * as env from "std/env"

// Build a table of N entries: { "k0": 0, "k1": 1, ... }. Stands in for a routing
// table / feature-flag snapshot / geo-IP database — a large read-only structure.
fn buildTable(n: number): object {
  let t = {}
  let i = 0
  while (i < n) {
    t[`k${i}`] = i
    i = i + 1
  }
  return t
}

// The worker: a single keyed lookup into the table it was handed. The RESULT is a
// scalar, so the round-trip cost is dominated by the inbound table transport (the
// thing we are measuring), not the return.
worker fn lookup(table, key): number {
  let v = table[key]
  if (v == nil) { return -1 }
  return v
}

// Average per-call dispatch latency over `rounds` sequential calls, each handing
// `table` (frozen or plain) to a worker. Sequential (not gathered) so we measure
// per-call transport, not parallel overlap.
async fn avgPerCall(table, rounds: number): number {
  let start = time.monotonic()
  let i = 0
  while (i < rounds) {
    let r = await lookup(table, "k0")
    i = i + 1
  }
  let elapsed = time.monotonic() - start
  return elapsed / rounds
}

async fn main() {
  // Default sweep tops out at 200k so a CI run finishes in a few seconds (the
  // deep-clone path is O(N) per call, so a 1M table × 20 rounds is genuinely slow —
  // which is exactly the cost the shared heap eliminates). Set BENCH_BIG=1 to extend
  // the sweep to 500k/1M for the full headline curve.
  let big = env.get("BENCH_BIG") == "1"
  let sizes = big ? [10000, 50000, 100000, 500000, 1000000] : [5000, 10000, 25000, 50000]
  let rounds = 10

  print("# shared-heap zero-copy vs deep-clone: per-dispatch cost by table size")

  // Warm the pool once so the first measurement isn't charged isolate spawn.
  let warm = await lookup(shared.freeze({ k0: 0 }), "k0")

  for (n of sizes) {
    let plain = buildTable(n)
    let frozen = shared.freeze(plain)   // one-time freeze cost, measured separately

    // (A) deep-clone path: hand the PLAIN table each call.
    let cloneMs = await avgPerCall(plain, rounds)
    // (B) shared path: hand the FROZEN table each call (Arc bump).
    let sharedMs = await avgPerCall(frozen, rounds)

    let speedup = sharedMs > 0.0 ? cloneMs / sharedMs : 0.0
    print(`size=${n} clone_per_call_ms=${cloneMs} shared_per_call_ms=${sharedMs} speedup=${speedup}`)
  }

  // Freeze cost vs size (the one-time amortized cost paid before any dispatch).
  print("# freeze cost (one-time) by table size")
  for (n of sizes) {
    let plain = buildTable(n)
    let start = time.monotonic()
    let frozen = shared.freeze(plain)
    let freezeMs = time.monotonic() - start
    print(`size=${n} freeze_ms=${freezeMs}`)
  }
}

await main()
