// Workers performance benchmark harness.
//
// Measures:
//   1. Speedup vs cores: a gather of N CPU-bound chunks; the shell driver runs
//      this at ASCRIPT_WORKERS=1,2,4,8 and records wall-clock for comparison.
//   2. Serialization overhead vs payload size: per-call round-trip cost as
//      arg/result array size grows.
//   3. Pool warmup: first-call (cold) vs steady-state (warm) latency.
//
// Usage:
//   ASCRIPT_WORKERS=N ascript run bench/workers_bench.as
//   Tagged output lines ("key=value") are parsed by run_workers_bench.sh.
//
// NOTE: Worker fn bodies may only use pure top-level fns + builtins (no imports).
// All helpers used inside worker bodies are defined as top-level fns below.
import * as task from "std/task"
import * as array from "std/array"
import * as time from "std/time"

// ─── CPU WORKLOAD (LCG compute, pure arithmetic) ─────────────────────────────
// Run iters iterations of a linear-congruential sequence starting from seed.
// Returns the final state modulo 1000. This is the canonical CPU-bound task.
// Each chunk does ~200k multiply+add+mod operations — heavy enough that 32
// chunks take ~500ms-2s on a single core and parallelism is clearly visible.
fn lcgRun(seed: number, iters: number): number {
  let s = seed
  let i = 0
  while (i < iters) {
    s = (s * 1103515245 + 12345) % 2147483648
    i = i + 1
  }
  return s % 1000
}

// Worker entry: run the LCG and return the checksum.
worker fn computeChunk(seed: number): number {
  return lcgRun(seed, 400000)
}

// ─── PAYLOAD WORKLOAD (serialization round-trip) ──────────────────────────────
// Receives an array of numbers, returns their sum. The worker does trivial work;
// the cost is dominated by structured-clone serialize/deserialize of the array.
fn sumArray(arr: array<number>): number {
  let s = 0
  let i = 0
  let n = len(arr)
  while (i < n) {
    s = s + arr[i]
    i = i + 1
  }
  return s
}

worker fn payloadWorker(arr: array<number>): number {
  return sumArray(arr)
}

// ─── HELPER: build a numeric array of given length ───────────────────────────
fn makeArray(n: number): array<number> {
  let out = []
  let i = 0
  while (i < n) {
    array.push(out, i)
    i = i + 1
  }
  return out
}

// ─── SPEEDUP SECTION ─────────────────────────────────────────────────────────
// Gather 32 CPU chunks in parallel. The shell driver measures wall-clock at
// different ASCRIPT_WORKERS values and computes speedup vs the 1-worker baseline.
// We report raw elapsed_ms from inside the program for accurate inner timing.
async fn benchSpeedup() {
  let numChunks = 32
  let seeds = makeArray(numChunks)

  // Warmup: one cold pass to get the pool running before timing.
  let warmup1 = await task.gather(array.map(seeds, computeChunk))
  if (len(warmup1) != numChunks) { print("warmup error") }

  // Timed measurement: parallel gather of all 32 chunks.
  let parStart = time.monotonic()
  let parResults = await task.gather(array.map(seeds, computeChunk))
  let parMs = time.monotonic() - parStart

  // Sanity check: verify checksum is deterministic (same regardless of workers).
  let checksum = sumArray(parResults)

  print(`parallel_ms=${parMs}`)
  print(`checksum=${checksum}`)
}

// ─── SERIALIZATION OVERHEAD SECTION ─────────────────────────────────────────
// Measure per-call round-trip cost at increasing payload sizes.
// Payload sizes (number of f64 elements): 0, 10, 100, 1000, 10000.
// These are run with 4 workers to be representative of steady-state pool use.
async fn benchPayload() {
  let sizes = [0, 10, 100, 1000, 10000]
  let reps = 20   // number of parallel calls per measurement

  let si = 0
  while (si < len(sizes)) {
    let n = sizes[si]
    let payload = makeArray(n)

    // Build reps copies of the payload to dispatch in parallel.
    let payloads = []
    let pi = 0
    while (pi < reps) {
      array.push(payloads, payload)
      pi = pi + 1
    }

    // Warmup round.
    let warmup2 = await task.gather(array.map(payloads, payloadWorker))
    if (len(warmup2) != reps) { print("payload warmup error") }

    // Timed round.
    let t0 = time.monotonic()
    let payloadRes = await task.gather(array.map(payloads, payloadWorker))
    let elapsed = time.monotonic() - t0
    let perCallMs = elapsed / reps

    if (len(payloadRes) != reps) { print("payload result error") }
    print(`payload_size=${n} total_ms=${elapsed} per_call_ms=${perCallMs}`)
    si = si + 1
  }
}

// ─── WARMUP SECTION ──────────────────────────────────────────────────────────
// Compare cold first-call latency vs steady-state (warm pool) latency.
// The "cold" measurement happens at program start before any worker is used.
async fn benchWarmup() {
  // COLD: time the very first worker dispatch (pool not yet warmed).
  let coldStart = time.monotonic()
  let coldRes = await computeChunk(1)
  let coldMs = time.monotonic() - coldStart
  if (coldRes == nil) { print("cold dispatch error") }

  // WARM: after the pool is up, time a single dispatch in steady state.
  // Do a few pool-heating dispatches first.
  let seeds8 = makeArray(8)
  let heat1 = await task.gather(array.map(seeds8, computeChunk))
  let heat2 = await task.gather(array.map(seeds8, computeChunk))
  if (len(heat1) == 0 || len(heat2) == 0) { print("heat error") }

  let warmStart = time.monotonic()
  let warmRes = await computeChunk(1)
  let warmMs = time.monotonic() - warmStart
  if (warmRes == nil) { print("warm dispatch error") }

  print(`warmup_cold_ms=${coldMs}`)
  print(`warmup_warm_ms=${warmMs}`)
}

// ─── MAIN ─────────────────────────────────────────────────────────────────────
// NOTE: warmup must run BEFORE benchSpeedup for the cold/warm comparison to be
// valid — it grabs the cold-first-dispatch measurement first.
async fn main() {
  await benchWarmup()
  await benchSpeedup()
  await benchPayload()
}

await main()
