// PROFILE TARGET: recommendation #2 (inline async completion).
// Calls an async fn that does pure compute and never actually suspends, then
// `await`s it — 200k times. Under today's EAGER model every call allocates a
// future + result cell + tokio task + args vec BEFORE running a trivial body.
// This bench should be dominated by spawn/alloc/scheduler cost, not arithmetic —
// i.e. it measures exactly the tax #2 removes.
import * as time from "std/time"

async fn compute(x) {
  return x * 2 + 1
}

let t0 = time.monotonic()
let sum = 0
for (i in 0..400000) {
  sum = sum + await compute(i)
}
let t1 = time.monotonic()
print(`async_inline: sum=${sum} elapsed_ms=${t1 - t0}`)
