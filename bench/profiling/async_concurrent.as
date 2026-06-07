// PROFILE TARGET: genuine concurrency (gather fan-out) without real I/O sleeps.
// Measures spawn + join + structured-concurrency bookkeeping cost when work
// actually runs as concurrent tasks. Complements async_inline.as: here the
// futures DO get scheduled, so this is the cost #2 keeps (vs the cost it removes).
import * as task from "std/task"
import * as time from "std/time"

async fn work(x) {
  return x * x
}

let t0 = time.monotonic()
let total = 0
for (i in 0..200000) {
  let rs = await task.gather([work(i), work(i + 1), work(i + 2), work(i + 3)])
  total = total + rs[0] + rs[1] + rs[2] + rs[3]
}
let t1 = time.monotonic()
print(`async_concurrent: total=${total} elapsed_ms=${t1 - t0}`)
