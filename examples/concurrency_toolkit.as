import * as sync from "std/sync"
import * as task from "std/task"
import * as time from "std/time"
import * as array from "std/array"
let ch = sync.channel()
async fn producer() {
  let i = 1
  while (i <= 5) {
    await sync.send(ch, i)
    i = i + 1
  }
  sync.close(ch)
}
let producerHandle = task.spawn(producer())
let sum = 0
let count = 0
let v = await sync.recv(ch)
while (v != nil) {
  sum = sum + v
  count = count + 1
  v = await sync.recv(ch)
}
await producerHandle
assert(count == 5, "channel: received 5 values")
assert(sum == 15, "channel: sum is 1+2+3+4+5 = 15")
let bch = sync.channel(2)
async fn boundedProducer() {
  let i = 10
  while (i <= 13) {
    await sync.send(bch, i)
    i = i + 1
  }
  sync.close(bch)
}
let bHandle = task.spawn(boundedProducer())
let received = []
let bv = await sync.recv(bch)
while (bv != nil) {
  array.push(received, bv)
  bv = await sync.recv(bch)
}
await bHandle
assert(len(received) == 4, "bounded channel: 4 values received")
assert(received[0] == 10, "bounded channel: first value is 10")
assert(received[3] == 13, "bounded channel: last value is 13")
let sem = sync.semaphore(2)
async fn guarded(n) {
  return await sync.withPermit(sem, async () => {
  await time.sleep(5)
  return n * 2
})
}
let semResults = await task.gather([guarded(1), guarded(2), guarded(3), guarded(4)])
assert(len(semResults) == 4, "semaphore: gather returned 4 results")
assert(semResults[0] == 2, "semaphore: result[0] is 2")
assert(semResults[1] == 4, "semaphore: result[1] is 4")
assert(semResults[2] == 6, "semaphore: result[2] is 6")
assert(semResults[3] == 8, "semaphore: result[3] is 8")
assert(sync.available(sem) == 2, "semaphore: all permits restored after gather")
let attemptCount = 0
async fn flaky() {
  attemptCount = attemptCount + 1
  if (attemptCount < 3) {
    assert(false, `flaky: simulated failure on attempt ${attemptCount}`)
  }
  return "success"
}
let retryResult = await task.retry(flaky, { attempts: 5, baseMs: 1 })
assert(retryResult == "success", "retry: returned success value")
assert(attemptCount == 3, "retry: took exactly 3 attempts")
let alwaysFails = async () => {
  assert(false, "always fails")
}
let [retryOk, retryErr] = recover(() => {
  await task.retry(alwaysFails, { attempts: 3, baseMs: 1 })
  return nil
})
assert(retryOk == nil, "retry exhausted: value is nil")
assert(retryErr != nil, "retry exhausted: error is not nil")
let iv = time.interval(20)
let tickCount = 0
let ivStart = time.monotonic()
while (tickCount < 3) {
  await iv.tick()
  tickCount = tickCount + 1
}
let ivElapsed = time.monotonic() - ivStart
assert(tickCount == 3, "interval: ticked 3 times")
assert(ivElapsed >= 30, "interval: at least 2 full periods elapsed")
let fireLog = []
let debouncedPush = time.debounce(n => {
  array.push(fireLog, n)
}, 30)
debouncedPush(1)
debouncedPush(2)
debouncedPush(3)
debouncedPush(4)
await time.sleep(80)
assert(len(fireLog) == 1, "debounce: burst of 4 collapsed to 1 call")
assert(fireLog[0] == 4, "debounce: last-call value 4 was the one that fired")
let throttleLog = []
let throttledPush = time.throttle(n => {
  array.push(throttleLog, n)
}, 40)
throttledPush(10)
throttledPush(20)
throttledPush(30)
throttledPush(40)
throttledPush(50)
assert(len(throttleLog) == 1, "throttle: only 1st call fires in burst")
assert(throttleLog[0] == 10, "throttle: leading-edge value is 10")
await time.sleep(60)
throttledPush(99)
await time.sleep(10)
assert(len(throttleLog) == 2, "throttle: fires again after window expires")
assert(throttleLog[1] == 99, "throttle: second fire value is 99")
print("concurrency toolkit: all assertions passed")
