// PROFILE TARGET (ELIDE baseline): fully-annotated version of call_heavy.as.
// Identical workload — same 3 fns, same 2M-call hot loop — but every param,
// return type, and let is explicitly annotated with int/float types so the
// ELIDE spec's (E)(Y)(A) predicate has proven sites to elide.
// Under --no-elide this is byte-identical to the untyped twin; under elide-on
// the per-arg check_type_env calls are removed for the proven sites.
import * as time from "std/time"

fn add(a: int, b: int): int {
  return a + b
}
fn scale(x: int): int {
  return add(x, x)
}
fn step(x: int): int {
  return add(scale(x), 1)
}

let t0: float = time.monotonic()
let sum: int = 0
for (i in 0..2000000) {
  sum = add(sum, step(i % 1000))
}
let t1: float = time.monotonic()
print(`call_heavy_typed: sum=${sum} elapsed_ms=${t1 - t0}`)
