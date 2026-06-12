// PROFILE TARGET (PERF campaign blind spot): call-dominated code. Tiny plain
// functions in a hot loop — per-call cost (arg vec, cells vector, frame push/pop, contract check).
import * as time from "std/time"

fn add(a, b) { return a + b }
fn scale(x) { return add(x, x) }
fn step(x) { return add(scale(x), 1) }

let t0 = time.monotonic()
let sum = 0
for (i in 0..2000000) {
  sum = add(sum, step(i % 1000))
}
let t1 = time.monotonic()
print(`call_heavy: sum=${sum} elapsed_ms=${t1 - t0}`)
