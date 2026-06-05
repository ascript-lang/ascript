// SP9 §1 — robust unbounded recursion. Deep native re-entry (a recursive
// higher-order callback chain) that USED to overflow the native stack now grows
// the heap-backed stack on demand and runs to completion, bounded only by the
// logical recursion cap. Run: `ascript run examples/deep_recursion.as`.
import { map } from "std/array"

fn sumDown(n) {
  if (n <= 0) {
    return 0
  }
  // The recursion re-enters the engine through `map`'s native callback funnel —
  // exactly the path SP9's stacker guard keeps from SIGABRTing.
  return map([n], (x) => x + sumDown(x - 1))[0]
}

print(sumDown(800))
