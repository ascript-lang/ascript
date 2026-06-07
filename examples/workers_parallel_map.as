// Parallel map: square each number in its own worker isolate, gather in order.
// Uses worker fn for CPU-ish work dispatched to the pool; gather preserves input
// order so the output is deterministic regardless of which isolate finishes first.
import * as task from "std/task"
import * as array from "std/array"
import * as math from "std/math"

worker fn square(n: number): number {
  return n * n
}

fn main() {
  let inputs = [1, 2, 3, 4, 5, 6, 7, 8]
  let futures = array.map(inputs, square)
  let results = await task.gather(futures)
  print(results)                   // [1, 4, 9, 16, 25, 36, 49, 64]
  print(math.sum(results))         // 204
}

await main()
