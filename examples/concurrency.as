// NOTE: the timing margins below are deliberately WIDE (work vs timeout/race ≥ 100×).
// This example is in the golden corpus, so its output must be deterministic even when
// the full test suite runs in parallel and starves the scheduler. A tight margin (e.g.
// a 5ms op under a 50ms timeout) can flake — the op gets delayed past the timeout under
// load. Keep the margins generous; timeouts/races cancel the loser early, so a wide
// bound adds no real runtime.
import * as task from "std/task"
import * as time from "std/time"
async fn work(label, ms, value) {
  await time.sleep(ms)
  return value
}
let results = await task.gather([work("a", 20, 1), work("b", 20, 2), work("c", 20, 3)])
print(results)
let winner = await task.race([work("slow", 200, "slow"), work("fast", 5, "fast")])
print(winner)
let [ok, e1] = await task.timeout(2000, work("quick", 5, "done"))
print(ok)
print(e1)
let [late, e2] = await task.timeout(5, work("toolong", 500, "never"))
print(late)
print(e2 != nil)
let pending = task.spawn(work("bg", 10, "background"))
print("started")
print(await pending)
