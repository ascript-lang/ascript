// bench/data_parallel_bench.as — PAR (data-parallel) benchmark harness.
//
// Measures (spec §6 / plan Task 4.1):
//   (a) Scaling: sequential for-map vs task.pmap vs hand-rolled gather(array.map(...))
//       over 32 chunks of 400k-iteration LCG work, at ASCRIPT_WORKERS = 1/2/4/8.
//   (b) Break-even sweep: per-element work at ~0 / 1k / 10k / 100k / 400k LCG iters,
//       fixed len=32, reporting the crossover where pmap beats sequential.
//   (c) Frozen vs plain input: pmap over 10k/100k/1M-element data both ways + freeze cost.
//   (d) preduce: add-reduce scaling + the chunks+1 overhead at small len.
//
// Usage:
//   ASCRIPT_WORKERS=N ascript run bench/data_parallel_bench.as
//   Tagged output lines ("key=value") are parsed by run_data_parallel_bench.sh.
//
// NOTE: Worker fn bodies may only use pure top-level fns + builtins (no imports).
// DESIGN NOTE: Only one worker fn per helper to avoid cross-slice double-definition
// of shared helpers in pool isolates (the pool reuses isolates across different
// worker fn dispatches; two slices that both define lcgRun would conflict).
// Solution: parameterized worker fns carry all they need in their args.
import * as task from "std/task"
import * as shared from "std/shared"
import * as array from "std/array"
import * as time from "std/time"

// ─── LCG helper (used by worker fns and sequential path) ─────────────────────
fn lcgRun(seed, iters) {
  let s = seed
  let i = 0
  while (i < iters) {
    s = (s * 1103515245 + 12345) % 2147483648
    i = i + 1
  }
  return s % 1000
}

// ─── Worker fns ──────────────────────────────────────────────────────────────
// ONE worker fn dispatches 400k-iter LCG (scaling + break-even 400k).
// Break-even at smaller iters uses SEPARATE parameterized fns that inline the work.
// Each worker fn is self-contained: no shared helper between distinct fns.

// Scaling + break-even 400k (the canonical heavy-work fn).
worker fn computeChunk(seed) { return lcgRun(seed, 400000) }

// Break-even 0-iters (pure element mod — trivial, no lcgRun dep).
worker fn trivialWork(seed) { return seed % 1000 }

// Break-even 1k-iters: inline the LCG loop to avoid cross-slice dep collision.
worker fn work1kInline(seed) {
  let s = seed
  let i = 0
  while (i < 1000) {
    s = (s * 1103515245 + 12345) % 2147483648
    i = i + 1
  }
  return s % 1000
}

// Break-even 10k-iters: inline.
worker fn work10kInline(seed) {
  let s = seed
  let i = 0
  while (i < 10000) {
    s = (s * 1103515245 + 12345) % 2147483648
    i = i + 1
  }
  return s % 1000
}

// Break-even 100k-iters: inline.
worker fn work100kInline(seed) {
  let s = seed
  let i = 0
  while (i < 100000) {
    s = (s * 1103515245 + 12345) % 2147483648
    i = i + 1
  }
  return s % 1000
}

// Frozen/plain input worker (trivial transform — the cost is airlock, not CPU).
worker fn readElemInline(x) { return (x * 2) % 1000 }

// preduce worker (associative add).
worker fn addReducer(a, b) { return a + b }

// ─── HELPERS ─────────────────────────────────────────────────────────────────
fn makeSeeds(n) {
  let out = []
  let i = 0
  while (i < n) {
    array.push(out, i)
    i = i + 1
  }
  return out
}

fn sumArr(arr) {
  let s = 0
  let i = 0
  let n = len(arr)
  while (i < n) {
    s = s + arr[i]
    i = i + 1
  }
  return s
}

// Sequential map of lcgRun over seeds, returning elapsed ms.
fn seqMapMs(seeds, iters) {
  let t = time.monotonic()
  let i = 0
  let n = len(seeds)
  while (i < n) {
    let _ = lcgRun(seeds[i], iters)
    i = i + 1
  }
  return time.monotonic() - t
}

// ─── (a) SCALING ─────────────────────────────────────────────────────────────
// Compare SEQ / PMAP / GATHER on 32 × 400k-iters LCG — same-session A/B.
async fn benchScaling() {
  let numChunks = 32
  let seeds = makeSeeds(numChunks)

  // Warmup: cold gather to boot the pool (pays cold-spawn cost here, not in timing).
  let wup = await task.gather(array.map(seeds, computeChunk))
  if (len(wup) == 0) { print("warmup error") }

  // SEQ: sequential for-loop in the caller, no workers.
  let seqT0 = time.monotonic()
  let seqCs = 0
  let ssi = 0
  while (ssi < len(seeds)) {
    seqCs = seqCs + lcgRun(seeds[ssi], 400000)
    ssi = ssi + 1
  }
  let seqMs = time.monotonic() - seqT0

  // PMAP: task.pmap chunks the work across the pool.
  let pmapT0 = time.monotonic()
  let pmapR = await task.pmap(seeds, computeChunk)
  let pmapMs = time.monotonic() - pmapT0
  let pmapCs = sumArr(pmapR)

  // GATHER: hand-rolled gather(array.map(...)) — one pool round-trip per element.
  let gatherT0 = time.monotonic()
  let gatherR = await task.gather(array.map(seeds, computeChunk))
  let gatherMs = time.monotonic() - gatherT0
  let gatherCs = sumArr(gatherR)

  let csOk = (seqCs % 1000000 == pmapCs % 1000000) && (pmapCs == gatherCs)
  print(`scaling_seq_ms=${seqMs}`)
  print(`scaling_pmap_ms=${pmapMs}`)
  print(`scaling_gather_ms=${gatherMs}`)
  print(`scaling_checksum=${pmapCs}`)
  print(`scaling_checksum_ok=${csOk}`)
}

// ─── (b) BREAK-EVEN SWEEP ────────────────────────────────────────────────────
// Each measurement uses its own worker fn (no shared lcgRun dep across fns).
// The sequential side uses lcgRun directly, which is fine (same scope).
async fn breakEvenTrivial(seeds) {
  let pt = time.monotonic()
  let pr = await task.pmap(seeds, trivialWork)
  let pms = time.monotonic() - pt
  let sms = seqMapMs(seeds, 0)
  return {seq: sms, pmap: pms, ok: len(pr) == len(seeds)}
}

async fn breakEven1k(seeds) {
  let pt = time.monotonic()
  let pr = await task.pmap(seeds, work1kInline)
  let pms = time.monotonic() - pt
  let sms = seqMapMs(seeds, 1000)
  return {seq: sms, pmap: pms, ok: len(pr) == len(seeds)}
}

async fn breakEven10k(seeds) {
  let pt = time.monotonic()
  let pr = await task.pmap(seeds, work10kInline)
  let pms = time.monotonic() - pt
  let sms = seqMapMs(seeds, 10000)
  return {seq: sms, pmap: pms, ok: len(pr) == len(seeds)}
}

async fn breakEven100k(seeds) {
  let pt = time.monotonic()
  let pr = await task.pmap(seeds, work100kInline)
  let pms = time.monotonic() - pt
  let sms = seqMapMs(seeds, 100000)
  return {seq: sms, pmap: pms, ok: len(pr) == len(seeds)}
}

async fn breakEven400k(seeds) {
  let pt = time.monotonic()
  let pr = await task.pmap(seeds, computeChunk)
  let pms = time.monotonic() - pt
  let sms = seqMapMs(seeds, 400000)
  return {seq: sms, pmap: pms, ok: len(pr) == len(seeds)}
}

async fn benchBreakEven() {
  let seeds = makeSeeds(32)
  // Warmup: ensure pool is warm from benchScaling; one trivial pass to ship this fn too.
  let wu = await task.pmap(seeds, trivialWork)
  if (len(wu) == 0) { print("warmup error") }

  let r0 = await breakEvenTrivial(seeds)
  print(`breakeven iters=0 seq_ms=${r0.seq} pmap_ms=${r0.pmap}`)

  let r1k = await breakEven1k(seeds)
  print(`breakeven iters=1000 seq_ms=${r1k.seq} pmap_ms=${r1k.pmap}`)

  let r10k = await breakEven10k(seeds)
  print(`breakeven iters=10000 seq_ms=${r10k.seq} pmap_ms=${r10k.pmap}`)

  let r100k = await breakEven100k(seeds)
  print(`breakeven iters=100000 seq_ms=${r100k.seq} pmap_ms=${r100k.pmap}`)

  let r400k = await breakEven400k(seeds)
  print(`breakeven iters=400000 seq_ms=${r400k.seq} pmap_ms=${r400k.pmap}`)
}

// ─── (c) FROZEN vs PLAIN INPUT ───────────────────────────────────────────────
// Pmap over 10k / 100k / 1M-element data, frozen and plain, + freeze cost.
// Uses readElemInline which has no shared dep with computeChunk — safe.
async fn benchFrozenVsPlain() {
  let sizes = [10000, 100000, 1000000]

  // Build arrays + report one-time freeze cost.
  let arrs = []
  let frozens = []
  let si = 0
  while (si < len(sizes)) {
    let n = sizes[si]
    let a = makeSeeds(n)
    let ft0 = time.monotonic()
    let f = shared.freeze(a)
    let freezeMs = time.monotonic() - ft0
    array.push(arrs, a)
    array.push(frozens, f)
    print(`frozen_vs_plain_freeze n=${n} freeze_ms=${freezeMs}`)
    si = si + 1
  }

  // Warmup with medium frozen array to ship readElemInline.
  let wu2 = await task.pmap(frozens[1], readElemInline)
  if (len(wu2) == 0) { print("warmup error") }

  let si2 = 0
  while (si2 < len(sizes)) {
    let n = sizes[si2]

    // Frozen: Arc-bump crossing — flat per chunk regardless of N (SRV §1).
    let tf = time.monotonic()
    let fr = await task.pmap(frozens[si2], readElemInline)
    let frozenMs = time.monotonic() - tf

    // Plain: airlock deep-copy per chunk — grows with N.
    let tp = time.monotonic()
    let pr = await task.pmap(arrs[si2], readElemInline)
    let plainMs = time.monotonic() - tp

    let ok = len(fr) == n && len(pr) == n
    print(`frozen_vs_plain n=${n} frozen_ms=${frozenMs} plain_ms=${plainMs} ok=${ok}`)
    si2 = si2 + 1
  }
}

// ─── (d) preduce SCALING ─────────────────────────────────────────────────────
// addReducer has no shared dep with computeChunk — safe.
async fn benchPreduce() {
  let s32 = makeSeeds(32)
  let wu3 = await task.preduce(s32, addReducer, 0)  // warmup
  if (wu3 == nil) { print("warmup error") }

  let sizes = [32, 64, 128]
  let ri = 0
  while (ri < len(sizes)) {
    let n = sizes[ri]
    let s = makeSeeds(n)
    let expected = (n * (n - 1)) / 2

    let t0 = time.monotonic()
    let result = await task.preduce(s, addReducer, 0)
    let ms = time.monotonic() - t0

    print(`preduce n=${n} result_ok=${result == expected} ms=${ms}`)
    ri = ri + 1
  }

  // Small-len: chunks+1 dispatch overhead at n=8.
  let tiny = makeSeeds(8)
  let expectedTiny = 28  // 0+1+2+3+4+5+6+7

  let tc1 = time.monotonic()
  let rc1 = await task.preduce(tiny, addReducer, 0, {chunks: 1})
  let msc1 = time.monotonic() - tc1

  let tc4 = time.monotonic()
  let rc4 = await task.preduce(tiny, addReducer, 0, {chunks: 4})
  let msc4 = time.monotonic() - tc4

  print(`preduce_small n=8 chunks=1 result_ok=${rc1 == expectedTiny} ms=${msc1}`)
  print(`preduce_small n=8 chunks=4 result_ok=${rc4 == expectedTiny} ms=${msc4}`)
}

// ─── MAIN ─────────────────────────────────────────────────────────────────────
async fn main() {
  await benchScaling()
  await benchBreakEven()
  await benchFrozenVsPlain()
  await benchPreduce()
}

await main()
