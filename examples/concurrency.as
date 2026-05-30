import * as task from "std/task"
import * as time from "std/time"
async fn work(label, ms, value) {
  await time.sleep(ms)
  return value
}
let results = await task.gather([work("a", 20, 1), work("b", 20, 2), work("c", 20, 3)])
print(results)
let winner = await task.race([work("slow", 50, "slow"), work("fast", 5, "fast")])
print(winner)
let [ok, e1] = await task.timeout(50, work("quick", 5, "done"))
print(ok)
print(e1)
let [late, e2] = await task.timeout(5, work("toolong", 50, "never"))
print(late)
print(e2 != nil)
let pending = task.spawn(work("bg", 10, "background"))
print("started")
print(await pending)
