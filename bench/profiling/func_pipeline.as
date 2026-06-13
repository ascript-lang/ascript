// PROFILE TARGET (PERF campaign blind spot): functional idioms. map/filter/reduce
// pipelines over realistic records — per-element callback re-entry, closure dispatch, small-object reads.
// NOTE: AScript array methods are module functions (array.filter/map/reduce), not method chains.
import * as time from "std/time"
import * as array from "std/array"

let records = []
for (i in 0..2000) {
  records = array.concat(records, [{ id: i, score: i % 97, group: i % 7, active: i % 3 != 0 }])
}

let t0 = time.monotonic()
let acc = 0
for (round in 0..2000) {
  let filtered = array.filter(records, (r) => r.active && r.score > 10)
  let mapped = array.map(filtered, (r) => r.score * 2 + r.group)
  let total = array.reduce(mapped, (a, b) => a + b, 0)
  acc = acc + total
}
let t1 = time.monotonic()
print(`func_pipeline: acc=${acc} elapsed_ms=${t1 - t0}`)
