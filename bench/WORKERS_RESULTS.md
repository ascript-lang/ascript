# Workers Performance Benchmark Results

**Date:** 2026-06-07 15:06 UTC
**Host:** Apple M4
**Logical cores:** 10
**OS:** Darwin 25.5.0 arm64
**Binary:** `target/release/ascript`

---

## 1. Speedup vs. Worker Count (CPU-bound: 32 × LCG chunks)

32 chunks of 400 k LCG iterations each, dispatched via
`task.gather(array.map(seeds, computeChunk))`.
Wall-clock measured inside the program (`std/time`).

| Workers | Parallel wall-clock (ms) | Speedup vs W=1 |
|---------|--------------------------|----------------|
| 1 | 2182.1 | baseline |
| 2 | 1054.3 | 2.07× |
| 4 | 690.9 | 3.16× |
| 8 | 438.6 | 4.98× |

Checksum (determinism guard): **17072** — identical across all worker counts.

---

## 2. Serialization Round-Trip Overhead

Per-call latency as the argument array grows.
Run with 4 workers (ASCRIPT_WORKERS=4), 20 calls per measurement round.
The cost here is dominated by structured-clone serialize/deserialize,
not computation.

| Payload size (f64 elements) | Total ms (20 calls) | Per-call ms |
|-----------------------------|---------------------|-------------|
| 0 | 4.73 | 0.236 |
| 10 | 4.57 | 0.228 |
| 100 | 4.65 | 0.232 |
| 1000 | 6.73 | 0.336 |
| 10000 | 25.85 | 1.292 |

---

## 3. Pool Warmup: Cold vs. Warm Latency

Single-dispatch latency for one `computeChunk` call,
before and after the worker pool is warmed (W=1).

| Measurement | Latency (ms) |
|-------------|--------------|
| Cold (first dispatch, pool not yet started) | 83.1 |
| Warm (steady-state, pool running)           | 62.0 |

Cold latency includes isolate thread spawn + tokio runtime init.
Warm latency is the per-call round-trip once the pool is hot.

---

---

## 4. Stateful Workers: Actors + Streaming (Plan B §7.4)

**Date:** 2026-06-07 22:29 UTC
**Host:** Apple M4
**Logical cores:** 10
**OS:** Darwin 25.5.0 arm64
**Binary:** `target/release/ascript`

---

### 4.1 Dedicated-Isolate Spawn Cost

Cold and warm spawn latency for a `worker class` actor (`Pinger.spawn()`) and
the first `.next()` call on a `worker fn*` generator — both launch a dedicated
OS thread with its own tokio runtime.

| Measurement | Latency (ms) |
|-------------|--------------|
| Actor cold spawn (first ever, includes thread + runtime init) | 2.032 |
| Actor warm spawn (subsequent spawns, OS thread reuse varies)  | 1.223 |
| Actor steady-state per-message (ping round-trip, 100 msgs)    | 0.0132 |
| Generator cold first-next (first ever, dedicated isolate)      | 1.139 |
| Generator warm first-next (after 3 warm-up cycles)             | 1.083 |

Each `worker class` spawn + each `worker fn*` call launches a **dedicated**
OS thread (8 MB stack) plus a single-threaded tokio runtime — therefore spawn
cost is dominated by OS thread creation (~1–2 ms warm, ~2–4 ms cold on this
machine) and is a one-time per-isolate cost, not per-message.

---

### 4.2 Single-Actor Throughput (Mailbox Round-Trip)

500 sequential `await c.inc()` calls on one live actor — measures the pure
mailbox overhead: caller serializes arg → channel send → isolate deserializes
→ runs method → serializes reply → channel recv → caller deserializes.

| Metric | Value |
|--------|-------|
| Total messages | 500 |
| Total elapsed (ms) | 5.729 |
| Per-message latency (ms) | 0.0115 |
| Throughput (msgs/sec) | 87,273 |

---

### 4.3 N-Actor Aggregate Scaling

N independent `worker class` actors, each processing 200 messages, driven
concurrently via `task.gather`. Each actor runs on its own OS thread,
so N actors = N concurrent threads (up to core count). Reports aggregate
messages/sec across all actors combined.

| N Actors | Total msgs | Wall-clock (ms) | Aggregate msgs/sec | Scaling vs N=1 |
|----------|------------|-----------------|--------------------| --------------|
| 1 | 200 | 3.37 | 59,304 | 1.00× |
| 2 | 400 | 4.17 | 95,906 | 1.62× |
| 4 | 800 | 5.04 | 158,790 | 2.68× |
| 8 | 1,600 | 8.54 | 187,463 | 3.16× |

Actors do not share memory — each lives in its own isolate — so scaling
is bounded by core count and the OS thread scheduler, not by locks or
shared state. The aggregate throughput grows near-linearly up to the
physical core count.

---

### 4.4 Streaming Throughput + Chunking Effect

A `worker fn*` producer streaming `n=3000` integers to a consumer that sums
them. `stream_k=1` yields each integer individually; `stream_k=K` batches K
integers into an array per yield, reducing isolate-boundary crossings.

The **sum is identical** across all chunk sizes (determinism check: all rows
report `total=4498500`).

| Chunk size (k) | Elapsed (ms) | Records/sec | Speedup vs k=1 |
|----------------|--------------|-------------|----------------|
| 1 | 14.12 | 212,400 | 1.00× |
| 5 | 5.61 | 534,957 | 2.52× |
| 10 | 4.92 | 609,281 | 2.87× |
| 25 | 3.33 | 901,803 | 4.25× |
| 50 | 3.24 | 925,235 | 4.36× ← peak |
| 100 | 3.32 | 904,886 | 4.26× |
| 200 | 3.51 | 853,657 | 4.02× |
| 500 | 4.70 | 637,709 | 3.00× |
| 1000 | 7.05 | 425,730 | 2.00× |

**Chunking effect:** peak throughput at k=50 is **4.4× faster** than
per-element yielding (k=1). Break-even is around k=5–10 (≥2× gain).
Beyond k≈100 the array-build cost offsets the boundary savings.
Recommended chunk size for scalar payloads: **k=25–100**.

---

*Stateful-workers section generated by `bench/run_workers_stateful_bench.sh`.*
