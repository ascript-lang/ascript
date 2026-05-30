import * as task from "std/task"
import * as time from "std/time"
import * as array from "std/array"
fn check(label, cond) {
  print(`${cond ? "PASS" : "FAIL"}  ${label}`)
}
async fn work(ms, value) {
  await time.sleep(ms)
  return value
}
let results = await task.gather([work(40, "a"), work(10, "b"), work(20, "c")])
check("gather preserves input order", results[0] == "a" && results[1] == "b" && results[2] == "c")
let raceLog = []
async fn slowLoser() {
  await time.sleep(60)
  array.push(raceLog, "loser ran")
  return "slow"
}
async fn fastWinner() {
  return "fast"
}
let winner = await task.race([slowLoser(), fastWinner()])
check("race returns the first to finish", winner == "fast")
await time.sleep(120)
check("race cancels the loser", len(raceLog) == 0)
let [okVal, okErr] = await task.timeout(100, work(10, "done"))
check("timeout passes a value through in time", okVal == "done" && okErr == nil)
let toLog = []
async fn slowJob() {
  await time.sleep(60)
  array.push(toLog, "job ran")
  return "x"
}
let [lateVal, lateErr] = await task.timeout(5, slowJob())
check("timeout returns an error past the deadline", lateVal == nil && lateErr != nil)
await time.sleep(120)
check("timeout cancels the timed-out work", len(toLog) == 0)
let spawnLog = []
async fn background() {
  await time.sleep(10)
  array.push(spawnLog, "bg done")
  return "bg"
}
task.spawn(background())
check("spawn returns before the work finishes", len(spawnLog) == 0)
await time.sleep(60)
check("spawn detaches: the task still runs", len(spawnLog) == 1)
let handle = task.spawn(work(5, "tracked"))
check("an awaited spawned handle yields its value", (await handle) == "tracked")
let dropLog = []
async fn orphan() {
  await time.sleep(10)
  array.push(dropLog, "orphan ran")
}
orphan()
await time.sleep(60)
check("un-awaited un-held call is cancelled (not orphaned)", len(dropLog) == 0)
print("done")
