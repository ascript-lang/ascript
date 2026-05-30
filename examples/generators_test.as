import * as time from "std/time"
import * as array from "std/array"
fn check(label, cond) {
  print(`${cond ? "PASS" : "FAIL"}  ${label}`)
}
fn* count(n) {
  let i = 1
  while (i <= n) {
    yield i
    i = i + 1
  }
}
let seen = []
for await (x in count(3)) {
  array.push(seen, x)
}
check("for await over a generator yields in order", seen[0] == 1 && seen[1] == 2 && seen[2] == 3)
let it = count(2)
check("next() returns the first value", it.next() == 1)
check("next() returns the second value", it.next() == 2)
check("next() returns nil when exhausted", it.next() == nil)
check("next() keeps returning nil after done", it.next() == nil)
let received = []
fn* echo() {
  let a = yield "q1"
  array.push(received, a)
  let b = yield "q2"
  array.push(received, b)
}
let g = echo()
check("first next() starts the body and returns the first yield", g.next() == "q1")
check("next(v) resumes and returns the next yield", g.next("a") == "q2")
g.next("b")
check("yield evaluates to the resumed value", received[0] == "a" && received[1] == "b")
fn* empty() {
  return
}
check("an empty generator's first next() is nil", empty().next() == nil)
let c = count(5)
check("next() before close works", c.next() == 1)
c.close()
check("next() after close is nil", c.next() == nil)
let afterClose = []
for await (x in c) {
  array.push(afterClose, x)
}
check("for await after close iterates nothing", len(afterClose) == 0)
async fn* ticks() {
  yield 1
  await time.sleep(1)
  yield 2
  yield 3
}
async fn* doubled(src) {
  for await (n in src) {
    yield n * 2
  }
}
let composed = []
async fn drive() {
  for await (v in doubled(ticks())) {
    array.push(composed, v)
  }
}
await drive()
check("async generators compose via for await", composed[0] == 2 && composed[1] == 4 && composed[2] == 6)
let infLog = []
fn* naturals() {
  let i = 0
  while (true) {
    yield i
    array.push(infLog, i)
    i = i + 1
  }
}
let pulled = []
for await (n in naturals()) {
  array.push(pulled, n)
  if (n >= 2) {
    break
  }
}
check("infinite generator with break pulls exactly what's consumed", pulled[0] == 0 && pulled[1] == 1 && pulled[2] == 2)
let neverLog = []
fn* neverRun() {
  array.push(neverLog, "ran")
  yield 1
}
neverRun()
check("a never-iterated generator's body does not run", len(neverLog) == 0)
print("done")
