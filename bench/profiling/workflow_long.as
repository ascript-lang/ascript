// PROFILE TARGET: one long-lived workflow with 2000 sequential activities.
//
// This is the per-EVENT shape: a single `run()` call accumulates 2000 events
// in the determinism context, then commits the whole log once at finish.
// Under "fsync" this is ONE F_FULLFSYNC for 2000 events; under "group" each
// event is written immediately to the OS page cache and fsyncs are coalesced
// by the window policy.
//
// Contrast with bench/profiling/workflow_loop.as (per-COMMIT shape):
//   workflow_loop:  3000 × run() × 2 activities = 3000 F_FULLFSYNC calls
//   workflow_long:  1 × run() × 2000 activities = 1 F_FULLFSYNC (fsync mode)
//
// Usage: ascript run bench/profiling/workflow_long.as [-- durability]
//   durability: "fsync" (default) | "group" | "buffered"
import { run, activity } from "std/workflow"
import { exists, remove } from "std/fs"
import * as time from "std/time"

let LOG = "/tmp/ascript_bench_wf_long.log"

// A lightweight activity whose result is a small serializable record.
// No real I/O — the cost is the event serialization + log write.
let processItem = activity("processItem", (i) => {
  return { id: i, ok: true, value: i * 2 + 1 }
})

fn longFlow(ctx, input) {
  let n = input.n
  let sum = 0
  for (i in 0..n) {
    let r = ctx.call(processItem, i)
    sum = sum + r.value
  }
  return { n: n, sum: sum }
}

// Accept optional durability argument.
let dur = "fsync"

let t0 = time.monotonic()
if (exists(LOG)) { remove(LOG) }
let [r, e] = recover(() => run(longFlow, { n: 2000 }, { log: LOG, durability: dur }))
let t1 = time.monotonic()
if (exists(LOG)) { remove(LOG) }

if (e == nil) {
  print(`workflow_long: n=${r.n} sum=${r.sum} elapsed_ms=${t1 - t0} durability=${dur}`)
} else {
  print(`workflow_long: error=${e.message}`)
  exit(1)
}
