// Stateful-workers performance benchmark (Plan B, §7.4).
//
// Measures:
//   1. Actor throughput: messages/sec to a SINGLE actor (mailbox round-trip cost).
//   2. N-actor scaling: aggregate messages/sec when N independent actors run in
//      parallel (N = 1, 2, 4, 8; driven via task.gather).
//   3. Streaming throughput + chunking effect: records/sec for a `worker fn*`
//      at per-element yielding (k=1) vs per-chunk yielding at k=5,10,25,50,100,200,
//      500,1000. Quantifies the "yield chunks, not elements" guidance.
//   4. Dedicated-isolate spawn latency: cold + warm actor spawn; cold + warm
//      generator first-next; steady-state per-message latency.
//
// Usage:
//   target/release/ascript run bench/workers_stateful_bench.as
//   Tagged output lines ("key=value") are parsed by run_workers_stateful_bench.sh.
//
// NOTE: worker bodies may only use pure top-level fns + builtins (no imports).
//       All helpers used inside worker bodies are defined as top-level fns below.

import * as task from "std/task"
import * as time from "std/time"

// ─── PURE HELPERS usable inside worker bodies ────────────────────────────────

// LCG step (CPU-bound arithmetic — same formula as Plan A bench).
fn lcgStep(s: number): number {
  return (s * 1103515245 + 12345) % 2147483648
}

// Build a numeric array [start, start+1, …) of at most k elements, stopping at n.
// Uses only the spread operator (builtin, no stdlib import) so worker bodies can call it.
fn buildChunkArr(start: number, k: number, n: number): array<number> {
  let chunk = []
  let j = 0
  while (j < k && start + j < n) {
    chunk = [...chunk, start + j]
    j = j + 1
  }
  return chunk
}

// ─── ACTOR DEFINITIONS ───────────────────────────────────────────────────────

// A minimal actor with a single counter — benchmarks pure mailbox cost with no
// payload serialization beyond a scalar number.
worker class Counter {
  n: number = 0
  fn inc(): number {
    self.n = self.n + 1
    return self.n
  }
}

// A lightweight actor for scaling tests — does one LCG step per message.
worker class LcgWorker {
  s: number = 1
  fn step(x: number): number {
    self.s = lcgStep(self.s + x)
    return self.s % 1000
  }
}

// Minimal actor for spawn-latency measurement — no computation, just returns.
worker class Pinger {
  fn ping(): number { return 1 }
}

// ─── STREAMING WORKER DEFINITIONS ────────────────────────────────────────────

// Per-element: each yield crosses the isolate boundary once per integer.
worker fn* streamElements(n: number) {
  let i = 0
  while (i < n) {
    yield i
    i = i + 1
  }
}

// Per-chunk: batch k elements into an array before each yield.
// Uses buildChunkArr (pure top-level fn, no stdlib import needed).
worker fn* streamChunks(n: number, k: number) {
  let i = 0
  while (i < n) {
    let chunk = buildChunkArr(i, k, n)
    i = i + len(chunk)
    yield chunk
  }
}

// ─── BENCH 1: Single-actor throughput ────────────────────────────────────────
// Measures sequential mailbox round-trip latency: spawn one actor, send `msgs`
// messages in a tight loop, measure total wall-clock, derive msgs/sec.
async fn benchSingleActor() {
  let msgs = 500

  // Warmup: spawn + 20 messages to ensure the isolate is hot.
  let wc = await Counter.spawn()
  let wi = 0
  while (wi < 20) {
    let _ = await wc.inc()
    wi = wi + 1
  }
  wc.close()

  // Timed run.
  let c = await Counter.spawn()
  let t0 = time.monotonic()
  let i = 0
  while (i < msgs) {
    let _ = await c.inc()
    i = i + 1
  }
  let elapsed = time.monotonic() - t0
  c.close()

  let perMsgMs = elapsed / msgs
  let msgsPerSec = 1000.0 / perMsgMs

  print(`single_actor_msgs=${msgs}`)
  print(`single_actor_elapsed_ms=${elapsed}`)
  print(`single_actor_per_msg_ms=${perMsgMs}`)
  print(`single_actor_msgs_per_sec=${msgsPerSec}`)
}

// ─── BENCH 2: N-actor aggregate scaling ──────────────────────────────────────
// Spawns N independent actors simultaneously via task.gather, each processing
// `msgsEach` messages. Reports aggregate messages/sec (sum of all messages / wall-clock).
// Tests whether the actor runtime scales linearly across cores.
async fn driveOneLcgActor(msgsEach: number): number {
  let w = await LcgWorker.spawn()
  let i = 0
  let last = 0
  while (i < msgsEach) {
    last = await w.step(i)
    i = i + 1
  }
  w.close()
  return last
}

async fn benchNActors(n: number, msgsEach: number) {
  // Build the job list: N concurrent driveOneLcgActor futures.
  let jobs = []
  let i = 0
  while (i < n) {
    jobs = [...jobs, driveOneLcgActor(msgsEach)]
    i = i + 1
  }
  let t0 = time.monotonic()
  let _ = await task.gather(jobs)
  let elapsed = time.monotonic() - t0
  let totalMsgs = n * msgsEach
  let aggMsgsPerSec = 1000.0 * totalMsgs / elapsed
  print(`n_actors=${n} msgs_each=${msgsEach} total_msgs=${totalMsgs} elapsed_ms=${elapsed} agg_msgs_per_sec=${aggMsgsPerSec}`)
}

async fn benchActorScaling() {
  let msgsEach = 200
  // Warmup: 1 actor, 50 msgs.
  let wjobs = [driveOneLcgActor(50)]
  let _ = await task.gather(wjobs)

  await benchNActors(1, msgsEach)
  await benchNActors(2, msgsEach)
  await benchNActors(4, msgsEach)
  await benchNActors(8, msgsEach)
}

// ─── BENCH 3: Streaming throughput + chunking effect ─────────────────────────
// Measures records/sec for streamElements (per-element, k=1) vs streamChunks at
// several chunk sizes. Both producers emit the same N integers and the consumer
// accumulates their sum — total must agree (determinism check).
async fn benchStreaming() {
  let n = 3000   // total records per run

  // Per-element baseline (k=1).
  let total0 = 0
  let t0 = time.monotonic()
  for await (x in streamElements(n)) {
    total0 = total0 + x
  }
  let elapsed0 = time.monotonic() - t0
  let rps0 = 1000.0 * n / elapsed0
  print(`stream_k=1 n=${n} elapsed_ms=${elapsed0} recs_per_sec=${rps0} total=${total0}`)

  // Chunk sizes to sweep: 5, 10, 25, 50, 100, 200, 500, 1000.
  let chunkSizes = [5, 10, 25, 50, 100, 200, 500, 1000]
  let ki = 0
  while (ki < len(chunkSizes)) {
    let k = chunkSizes[ki]
    let total = 0
    let t1 = time.monotonic()
    for await (chunk in streamChunks(n, k)) {
      let ci = 0
      while (ci < len(chunk)) {
        total = total + chunk[ci]
        ci = ci + 1
      }
    }
    let elapsed = time.monotonic() - t1
    let rps = 1000.0 * n / elapsed
    print(`stream_k=${k} n=${n} elapsed_ms=${elapsed} recs_per_sec=${rps} total=${total}`)
    ki = ki + 1
  }
}

// ─── BENCH 4: Dedicated-isolate spawn + steady-state latency ─────────────────
// Measures:
//   a. Cold actor spawn (very first spawn in the process).
//   b. Warm actor spawn (after a couple spawn/close cycles).
//   c. Steady-state per-message latency (actor running, sequential pings).
//   d. Cold generator first-next latency.
//   e. Warm generator first-next latency.
async fn benchSpawnLatency() {
  // a. Cold actor spawn.
  let t0 = time.monotonic()
  let a1 = await Pinger.spawn()
  let actorColdMs = time.monotonic() - t0
  a1.close()

  // b. Warm actor spawn — do 3 cycles first.
  let heat = 0
  while (heat < 3) {
    let wa = await Pinger.spawn()
    let _ = await wa.ping()
    wa.close()
    heat = heat + 1
  }
  let t1 = time.monotonic()
  let a2 = await Pinger.spawn()
  let actorWarmMs = time.monotonic() - t1
  a2.close()

  // c. Steady-state per-message latency.
  let aa = await Pinger.spawn()
  // Warmup pings.
  let wpi = 0
  while (wpi < 10) {
    let _ = await aa.ping()
    wpi = wpi + 1
  }
  let msgCount = 100
  let t2 = time.monotonic()
  let mi = 0
  while (mi < msgCount) {
    let _ = await aa.ping()
    mi = mi + 1
  }
  let actorSteadyMs = (time.monotonic() - t2) / msgCount
  aa.close()

  print(`spawn_actor_cold_ms=${actorColdMs}`)
  print(`spawn_actor_warm_ms=${actorWarmMs}`)
  print(`spawn_actor_steady_msg_ms=${actorSteadyMs}`)

  // d. Cold generator first-next.
  let t3 = time.monotonic()
  let g1 = streamElements(10)
  let _ = await g1.next()
  let genColdMs = time.monotonic() - t3
  g1.close()

  // e. Warm generator first-next — 3 warm-up cycles.
  let gwarm = 0
  while (gwarm < 3) {
    let wg = streamElements(3)
    let _ = await wg.next()
    wg.close()
    gwarm = gwarm + 1
  }
  let t4 = time.monotonic()
  let g2 = streamElements(10)
  let _ = await g2.next()
  let genWarmMs = time.monotonic() - t4
  g2.close()

  print(`spawn_gen_cold_ms=${genColdMs}`)
  print(`spawn_gen_warm_ms=${genWarmMs}`)
}

// ─── MAIN ─────────────────────────────────────────────────────────────────────
// Run spawn-latency FIRST so cold-spawn measurements are truly cold,
// before any other actor/generator is touched.
async fn main() {
  await benchSpawnLatency()
  await benchSingleActor()
  await benchActorScaling()
  await benchStreaming()
}

await main()
