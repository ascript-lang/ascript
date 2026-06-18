// resilience.as — std/resilience: the backend-hosting policy kit.
//
// Circuit breaker, token-bucket rate limiting (plain + keyed), bulkhead/load
// shedding, retry v2, fallback, singleflight, stampede-protected memoize, and
// deadline propagation — all per-isolate, all composable by plain nested calls.
//
// Every value printed below is a verdict, code, or counter — NOTHING is
// wall-clock-dependent — so the output is byte-identical across all four engines
// (tree-walker, specialized VM, generic VM, and compiled .aso).
//
//   ascript run examples/resilience.as
import * as resilience from "std/resilience"
import * as task from "std/task"
import * as time from "std/time"

// ── Circuit breaker — count-based window, fully deterministic ───────────────
// failureRate 0.5 over a window of 4 with minCalls 4: 2 ok + 2 fail = exactly
// 50% → the breaker opens. The next call is rejected with "breaker-open".
let b = resilience.breaker({name: "payments", failureRate: 0.5, window: 4, minCalls: 4, cooldownMs: 999999, halfOpenMax: 1})
fn ok() {
  return 42
}
fn fail() {
  return [nil, {message: "upstream down", code: "err"}]
}
b.call(ok)
b.call(ok)
b.call(fail)
b.call(fail)
print(b.state()) // open
let [_rv, re] = b.call(ok)
print(re.code) // breaker-open (rejected, not entered in window)
print(b.stats().rejected) // 1
b.reset() // ops/test hook → closed, window cleared
print(b.state()) // closed

// ── Rate limiter — token bucket; near-zero refill so no CI race ─────────────
// refillPerSec 0.001 means the bucket effectively never refills during the run,
// so the verdict sequence is exactly capacity-many trues then false.
let lim = resilience.limiter({name: "api", capacity: 2, refillPerSec: 0.001})
print(lim.tryAcquire()) // true
print(lim.tryAcquire()) // true
print(lim.tryAcquire()) // false — exhausted

// ── Keyed limiter — per-key isolation ───────────────────────────────────────
// Each key gets its own bucket; exhausting "A" never touches "B".
let perClient = resilience.keyedLimiter({capacity: 2, refillPerSec: 0.001, maxKeys: 100})
print(perClient.tryAcquire("A")) // true
print(perClient.tryAcquire("A")) // true
print(perClient.tryAcquire("A")) // false — key A exhausted
print(perClient.tryAcquire("B")) // true — key B still has a full bucket

// ── Bulkhead — concurrency cap with all-paths permit release ─────────────────
// A bulkhead caps concurrent executions (limit) and queues a bounded number of
// waiters (queue); overflow is shed immediately with "bulkhead-full". The
// load-shed path is concurrency-driven (it needs a permit held in-flight). Here
// we show the always-released-permit invariant deterministically: even when the
// wrapped fn PANICS, the permit is released, so the next run still succeeds.
let bh = resilience.bulkhead({name: "db", limit: 1, queue: 0})
fn boom() {
  assert(false, "kaboom")
}
fn okWork() {
  return 42
}
let [_bv, berr] = recover(() => bh.run(boom))
print(berr != nil) // true — the panic propagated (permit was released)
let [bv2, berr2] = bh.run(okWork)
print(bv2) // 42 — the released permit let this run
print(berr2) // nil

// ── Retry v2 — retryOn:"error" + retryIf, deterministic attempt count ───────
// A counter makes the attempt count exact: the fn fails twice then succeeds.
async fn retryDemo() {
  let attempts = [0]
  async fn flaky() {
    attempts[0] = attempts[0] + 1
    if (attempts[0] < 3) {
      return [nil, {message: "transient", code: "err"}]
    }
    return "recovered"
  }
  // retryIf returns true for transient errors → retried until success on attempt 3.
  let v = await task.retry(flaky, {attempts: 5, baseMs: 1, retryOn: "error", retryIf: (e) => e.code == "err"})
  print(v) // recovered
  print(attempts[0]) // 3
}
await retryDemo()

// ── Fallback — the terminal "always answer something" layer ─────────────────
// The primary fails; the fallback supplies a default. Returned as [value, nil].
fn primary() {
  return [nil, {message: "no data", code: "err"}]
}
let [fv, ferr] = resilience.fallback(primary, (e) => `fallback(${e.code})`)
print(fv) // fallback(err)
print(ferr) // nil

// ── Singleflight — collapse duplicate concurrent work ───────────────────────
// Two concurrent gets on the same key share ONE execution.
async fn singleflightDemo() {
  let calls = [0]
  async fn fetchUser() {
    calls[0] = calls[0] + 1
    return "user:42"
  }
  let f1 = resilience.singleflight("user:42", fetchUser)
  let f2 = resilience.singleflight("user:42", fetchUser)
  print(await f1 == await f2) // true — same result
  print(calls[0]) // 1 — fetchUser ran exactly once
}
await singleflightDemo()

// ── Memoize — stampede protection + a cache hit ─────────────────────────────
// Concurrent misses on one key collapse to a single fn run; a later get is a hit.
async fn memoizeDemo() {
  let runs = [0]
  async fn load() {
    runs[0] = runs[0] + 1
    return "value"
  }
  let cache = resilience.memoize({max: 100})
  let g1 = cache.get("k", load)
  let g2 = cache.get("k", load)
  await g1
  await g2
  print(runs[0]) // 1 — stampede collapsed to one run
  let [hit, _he] = await cache.get("k", load)
  print(hit) // value (served from cache)
  print(runs[0]) // 1 — the hit did not re-run load
}
await memoizeDemo()

// ── Deadline propagation — a budget that survives nested awaits ─────────────
// A body that sleeps 500ms under a 50ms deadline (10× margin) is cancelled; the
// caller gets "deadline-exceeded" and the body's post-sleep side effect never runs.
async fn deadlineDemo() {
  let ran = [false]
  let [v, err] = await resilience.deadline(50, async () => {
    await time.sleep(500)
    ran[0] = true // never runs — body cancelled at the deadline
    return "done"
  })
  print(v) // nil
  print(err.code) // deadline-exceeded
  print(ran[0]) // false

  // Nested deadlines only SHRINK — a callee can never extend its caller's budget.
  resilience.deadline(60000, () => {
    let outer = resilience.deadlineRemaining()
    resilience.deadline(120000, () => {
      let inner = resilience.deadlineRemaining()
      print(inner <= outer) // true — inner clamped to the outer budget
      return nil
    })
    return nil
  })

  // An already-expired deadline fast-fails WITHOUT running the body.
  let bodyRan = [0]
  let [_zv, zerr] = resilience.deadline(0, () => {
    bodyRan[0] = bodyRan[0] + 1
    return 7
  })
  print(zerr.code) // deadline-exceeded
  print(bodyRan[0]) // 0 — fn never called
}
await deadlineDemo()

// ── Validation misuse is a Tier-2 panic — recover() contains it ─────────────
// A non-positive capacity is programmer error (a panic), recoverable for the demo.
let [_cap, caperr] = recover(() => resilience.limiter({capacity: 0, refillPerSec: 10}))
print(caperr != nil) // true — misuse panicked and was recovered
print("resilience ok")
