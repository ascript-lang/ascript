// bench/replay/proc_heavy.as
//
// REPLAY replay-speed — the process-spawn case. 30 × process.run("echo", ...)
// (Recorded-Plain). Under --record each spawn is a real fork/exec whose
// {stdout,stderr,code} is captured; under --replay the recorded result is
// returned with NO fork/exec, so replay collapses the OS process-spawn cost.
import * as process from "std/process"

let n = 0
for (i in 0..30) {
  let [r, e] = recover(() => process.run("echo", [`hi-${i}`]))
  if (e == nil) { n = n + 1 }
}
print(`proc_heavy: n=${n}`)
