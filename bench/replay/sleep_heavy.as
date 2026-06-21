// bench/replay/sleep_heavy.as
//
// REPLAY replay-speed headline — the sleep-heavy workload. 25 × time.sleep(20)
// = 500 ms of WALL sleep when run PLAIN. The honest finding this workload
// surfaces: under BOTH --record AND --replay the clock is the SP9 VIRTUAL clock,
// so time.sleep advances virtual time INSTANTLY (no real wall sleep) in either
// mode. So the dramatic speedup is PLAIN (real sleeps) → record/replay (virtual,
// instant), and record≈replay for the sleep component. The script prints the
// VIRTUAL elapsed so all three modes are output-identical (parity check).
import * as time from "std/time"

let t0 = time.monotonic()
for (i in 0..25) {
  time.sleep(20)
}
let t1 = time.monotonic()
print(`sleep_heavy: virtual_elapsed_ms=${t1 - t0}`)
