// Parallel sample sort: split data into chunks, sort each chunk in a worker
// isolate, then k-way merge the sorted chunks on the caller thread.
//
// The result is fully deterministic: gather preserves chunk order, and the
// k-way merge is a deterministic sequential scan across cursor positions.
//
// NOTE: stdlib imports (e.g. `array.sort`) cannot be referenced inside a
// worker fn body because the code slice only ships top-level fn/const defs —
// not import bindings. Instead, a top-level helper `fn` is used; the closure
// builder ships it transitively along with the worker entry.
import * as task from "std/task"
import * as array from "std/array"

// Insertion sort used inside worker isolates — a pure top-level fn so it is
// included in the transitive code-slice shipped to each isolate.
fn insertionSort(arr: array<number>): array<number> {
  let n = len(arr)
  let i = 1
  while (i < n) {
    let key = arr[i]
    let j = i - 1
    while (j >= 0 && arr[j] > key) {
      arr[j + 1] = arr[j]
      j = j - 1
    }
    arr[j + 1] = key
    i = i + 1
  }
  return arr
}

// Sort a chunk in a worker isolate. insertionSort is shipped transitively.
worker fn sortChunk(chunk: array<number>): array<number> {
  return insertionSort(chunk)
}

// K-way merge on the caller thread: advance the minimum-value cursor each step
// until all cursors are exhausted. Produces a fully sorted array.
fn kwayMerge(chunks: array<array<number>>): array<number> {
  let n = len(chunks)
  let cursors = []
  let total = 0
  let i = 0
  while (i < n) {
    array.push(cursors, 0)
    total = total + len(chunks[i])
    i = i + 1
  }
  let out = []
  while (len(out) < total) {
    let best = nil
    let bestIdx = -1
    let j = 0
    while (j < n) {
      if (cursors[j] < len(chunks[j])) {
        let v = chunks[j][cursors[j]]
        if (best == nil || v < best) {
          best = v
          bestIdx = j
        }
      }
      j = j + 1
    }
    array.push(out, best)
    cursors[bestIdx] = cursors[bestIdx] + 1
  }
  return out
}

fn main() {
  let data = [9, 3, 7, 1, 8, 2, 6, 4, 5, 0, 11, 10]

  // Split into chunks of 4 using array.chunk, dispatch each to a worker isolate.
  let chunks = array.chunk(data, 4)
  let futures = array.map(chunks, sortChunk)
  let sortedChunks = await task.gather(futures)

  let result = kwayMerge(sortedChunks)
  print(result)   // [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]
}

await main()
