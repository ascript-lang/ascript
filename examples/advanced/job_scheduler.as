// job_scheduler.as — a cron-driven background job, deterministically.
//
// `cron.schedule(expr, fn, opts?)` spawns a background task that fires `fn` on
// the cron cadence. The handle exposes `running()` / `stop()` (graceful) /
// `close()` (abort). Under `ascript run` the loop sleeps on the REAL wall clock,
// so the number of fires within a window is timing-dependent — therefore this
// example DOES NOT print fire counts (which would be non-deterministic). Instead
// it demonstrates the handle lifecycle (all deterministic) and previews the
// upcoming fire times with `cron.nextN` against a FIXED anchor.
//
// Under `--seed/--frozen-time` (or a workflow) the virtual clock fast-forwards
// and fire times become replay-deterministic BY CONSTRUCTION — that timing path
// is covered by tests/cron.rs (schedule under frozen time); here we keep the
// observable output stable for the corpus.
import * as cron from "std/cron"
import * as date from "std/date"

let utc = (ms) => date.format(date.fromEpochMs(ms), "%Y-%m-%d %H:%M (%a) UTC")

async fn main() {
  let expr = "*/30 9-17 * * 1-5" // every 30 min, 9am–5pm, Mon–Fri

  // Preview the next runs deterministically (fixed anchor, not now()).
  let anchor = 1700038800000 // 2023-11-15 09:00 UTC (a Wednesday)
  let [upcoming, previewErr] = cron.nextN(expr, 4, {after: anchor})
  print(`schedule: "${expr}"`)
  print(`preview err: ${previewErr}`)
  print("upcoming runs:")
  for (r of upcoming) {
    print(`  ${utc(int(r))}`)
  }

  // Spawn the live background job. We stop it immediately so the program
  // terminates and the output stays deterministic (no real fires observed).
  let fired = 0
  let [job, scheduleErr] = cron.schedule(expr, () => {
    fired = fired + 1
  }, {tzOffset: 0})
  if (scheduleErr != nil) {
    print(`schedule error: ${scheduleErr.message}`)
    return
  }
  print(`job running after schedule(): ${job.running()}`)

  // Graceful stop, then hard close (releases the background task).
  job.stop()
  print(`job running after stop(): ${job.running()}`)
  job.close()

  // A bad expression is a Tier-1 error from schedule (it's often config data).
  let [_bad, badErr] = cron.schedule("not a cron expr", () => {
  }, {})
  print(`bad expression errors: ${badErr != nil}`)
}

await main()
print("job_scheduler ok")
