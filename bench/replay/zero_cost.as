// bench/replay/zero_cost.as
//
// REPLAY Gate 12/17 — the zero-cost-when-off workload. Effect-LIGHT but
// stdlib-TOUCHING: a tight loop that calls into `call_stdlib` (math + string
// builtins) on every iteration. Run PLAIN (no --record/--replay), this pays
// exactly ONE extra cost vs a pre-REPLAY binary: the `trace_active()`
// `Cell<bool>` read at the top of each stdlib dispatch (mirrors the caps
// `all_granted()` short-circuit). If that read is not free, this workload taxes.
//
// Sized (3M iters) so wall time is well above the centisecond `/usr/bin/time`
// granularity and run-to-run noise, making a per-call Cell tax visible if real.
import * as math from "std/math"
import * as str from "std/string"

let acc = 0.0
let s = "hello-world-replay"
for (i in 0..3000000) {
  acc = acc + math.abs(math.sqrt(i + 1.0) - 2.0)
  acc = acc + math.max(i % 7, 3)
  if (i % 1000 == 0) {
    acc = acc + len(str.upper(s))
  }
}
print(`zero_cost: acc=${acc > 0.0}`)
