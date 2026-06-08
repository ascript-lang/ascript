// Embarrassingly-parallel π estimate via deterministic LCG Monte Carlo.
//
// Each worker is seeded with a fixed integer so output is IDENTICAL across
// every engine (tree-walker, specialized-VM, generic-VM, .aso) and every run.
// The LCG (linear congruential generator) is a classic: state = (a*s + c) % m.
// Pairs of LCG values are interpreted as (x, y) ∈ [0, 1)²; a point is "inside
// the quarter-circle" when x²+y² ≤ 1. π ≈ 4 × (inside / total).
//
// NOTE: stdlib imports (e.g. `math.sum`) are NOT available inside worker fn
// bodies — the code slice only ships top-level fn/const defs. Arithmetic and
// control flow are the only tools available in the worker body; summing happens
// on the caller thread using `math.sum` after gather.
import * as task from "std/task"
import * as array from "std/array"
import * as math from "std/math"

// Count LCG samples that fall inside the unit-circle quarter. Pure arithmetic
// only — no stdlib imports needed in the worker body.
worker fn countInCircle(seed: number): number {
  let samples = 50000
  let state = seed
  let hits = 0
  let i = 0
  while (i < samples) {
    state = (state * 1103515245 + 12345) % 2147483648
    let x = state / 2147483648.0
    state = (state * 1103515245 + 12345) % 2147483648
    let y = state / 2147483648.0
    if (x * x + y * y <= 1.0) { hits = hits + 1 }
    i = i + 1
  }
  return hits
}

fn main() {
  // 8 fixed seeds → 8 * 50 000 = 400 000 total samples.
  // gather preserves input order so the total is always identical.
  let seeds = [1, 2, 3, 4, 5, 6, 7, 8]
  let futures = array.map(seeds, countInCircle)
  let hitCounts = await task.gather(futures)
  let totalHits = math.sum(hitCounts)
  let total = len(seeds) * 50000
  let piEst = 4.0 * totalHits / total
  // Round to 4 decimal places for a stable, deterministic string. `math.round`
  // returns an `int` (NUM §4), so divide by a `float` to keep real division.
  let piRounded = math.round(piEst * 10000) / 10000.0
  print(`pi estimate: ${piRounded}`)   // pi estimate: 3.1418
}

await main()
