// EXEC Task 10 — the spawn/wake microbenchmark (the executor's headline workload).
// Three phases, each isolating a cost the bespoke executor targets:
//   (a) 200k spawn+await round trips — per-spawn allocation + same-thread wake.
//   (b) 50k gather-of-4 — fan-out spawn + structured-join bookkeeping.
//   (c) 200k awaits of an already-resolved future — the resolved-await fast path.
// Deterministic + both-engine-runnable; prints one combined elapsed_ms (the A/B
// harness greps the first elapsed_ms). Under the bespoke executor a same-thread
// wake is a VecDeque push (no kevent round-trip); under tokio it pays the full
// task-harness + reactor-park path.
import * as task from "std/task"
import * as time from "std/time"

async fn unit(x) {
  return x * 2 + 1
}

let t0 = time.monotonic()

// (a) spawn + await round trips.
let suma = 0
for (i in 0..200000) {
  suma = suma + await unit(i)
}

// (b) gather-of-4 fan-out.
let sumb = 0
for (i in 0..50000) {
  let rs = await task.gather([unit(i), unit(i + 1), unit(i + 2), unit(i + 3)])
  sumb = sumb + rs[0] + rs[1] + rs[2] + rs[3]
}

// (c) await an already-resolved future repeatedly (resolved-await fast path).
let f = unit(7)
let r = await f
let sumc = 0
for (i in 0..200000) {
  sumc = sumc + r
}

let t1 = time.monotonic()
print(`spawn_wake: a=${suma} b=${sumb} c=${sumc} elapsed_ms=${t1 - t0}`)
