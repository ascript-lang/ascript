// Static worker fn: a class-level worker method dispatched to the pool.
// Task 8 made static dispatch work: the method body is shipped as a standalone
// fn, the class name is preserved across the isolate boundary for reconstruction.
import * as task from "std/task"
import * as array from "std/array"

class Img {
  static worker fn encode(px: number): number {
    // Simulate a CPU-bound encode step: apply a gain and offset.
    return px * 2 + 1
  }
}

fn main() {
  let pixels = [10, 20, 30]
  let futures = array.map(pixels, Img.encode)
  let encoded = await task.gather(futures)
  print(encoded)   // [21, 41, 61]
}

await main()
