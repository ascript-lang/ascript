// bench/replay/effect_heavy.as
//
// REPLAY Gate 18 — the effect-HEAVY workload. Each iteration touches four
// recorded/seamed effect sites: fs.write + fs.read (Recorded-Plain),
// math.random (RNG-seamed), time.now (clock-seamed). Under --record the engine
// captures one event per effect into the in-memory buffer (the thing to watch
// for RSS); under --replay every fs call returns the recorded bytes with NO
// real disk access, so replay wall-time drops to compute-only.
//
// The directory is fixed + reused (20 rotating files) so RECORD does real I/O
// against a warm working set (not dominated by file creation), and REPLAY does
// none.
import * as fs from "std/fs"
import * as math from "std/math"
import * as time from "std/time"

let dir = "/tmp/ascript_replay_effdir"
let acc = 0.0
for (i in 0..2000) {
  let p = `${dir}/f${i % 20}.txt`
  let [w, we] = fs.write(p, `data-${i}-${math.random()}-${time.now()}`)
  let [c, e] = fs.read(p)
  if (e == nil) { acc = acc + len(c) }
}
print(`effect_heavy: acc=${acc > 0.0}`)
