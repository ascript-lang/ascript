// EXEC Task 10 — race/abort characterization (the Task-9 reviewer's reap-latency
// finding). `task.race` over pure-COMPUTE async fns (no real timers) in a tight
// loop: each iteration spawns N resolver tasks, the first wins, the losers are
// aborted (cancel-on-drop). Measures the abort-cycle cost per race under the
// bespoke deferred-drop abort vs tokio. Deterministic (FIFO winner = the first
// arg, which resolves first). No timers, so it isolates abort bookkeeping from
// timer-cancellation latency.
import * as task from "std/task"
import * as time from "std/time"

async fn quick(x) {
  return x
}

let t0 = time.monotonic()
let wins = 0
for (i in 0..50000) {
  let w = await task.race([quick(i), quick(i + 1), quick(i + 2), quick(i + 3)])
  wins = wins + w
}
let t1 = time.monotonic()
print(`race_compute: wins=${wins} elapsed_ms=${t1 - t0}`)
