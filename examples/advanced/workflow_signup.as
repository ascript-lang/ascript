// std/workflow — durable execution via event-sourced replay (SP9 §2).
//
// A workflow is DETERMINISTIC code: control flow plus calls to ACTIVITIES through
// the workflow `ctx`. Non-deterministic effects (I/O, time, randomness) happen ONLY
// inside activities; the engine records each activity's RESULT to an append-only
// JSON event log. On `resume` after a crash, the workflow re-runs from the top but
// each recorded activity replays its result from the log instead of re-executing —
// so the workflow deterministically fast-forwards to where it left off. The
// continuation is reconstructed by replay, never serialized (no model-2b VM).
//
// Run: `ascript run examples/advanced/workflow_signup.as`
// This example is fully self-contained + deterministic, so it prints the same lines
// every run. The log is written to a temp file and cleaned up at the end.

import { run, resume, activity } from "std/workflow"
import { exists, remove } from "std/fs"

// A throwaway log path under the system temp dir.
let LOG = "/tmp/ascript_workflow_signup.log"

// --- activities: the ONLY place side effects / non-determinism live ---------

// "Fetch" a user record. In a real flow this would hit a database / HTTP API
// (a native handle that lives ONLY inside the activity, never crossing the log).
// Here it returns plain DATA so the result is serializable into the event log.
let fetchUser = activity("fetchUser", (id) => {
  return { id: id, name: `user-${id}`, plan: { name: "pro", price: 4200 } }
})

// "Charge" the user's card. Returns a transaction record (data, not a handle).
let chargeCard = activity("chargeCard", (amount) => {
  return { ok: true, amount: amount, txn: "txn-001" }
})

// --- the workflow: deterministic control flow + ctx-mediated effects --------

fn signupFlow(ctx, input) {
  // ctx.call records (first run) or replays (resume) each activity's result.
  let user = ctx.call(fetchUser, input.id)
  // ctx.now / ctx.random / ctx.uuid are the recorded virtual clock + seeded RNG,
  // so they replay identically — never call time.now() / math.random() directly.
  let at = ctx.now()
  let receipt = ctx.uuid()
  let charge = ctx.call(chargeCard, user.plan.price)
  return {
    user: user.name,
    plan: user.plan.name,
    charged: charge.amount,
    ok: charge.ok,
    hasReceipt: len(receipt) == 36,
    hasTimestamp: at > 0,
  }
}

// --- drive it: a clean record run, then an idempotent resume -----------------

// Start fresh.
if (exists(LOG)) {
  remove(LOG)
}

let [r1, err1] = recover(() => run(signupFlow, { id: 42 }, { log: LOG }))
if (err1 != nil) {
  print(`workflow failed: ${err1.message}`)
  exit(1)
}
print(`signup ok: ${r1.ok}`)
print(`user: ${r1.user}, plan: ${r1.plan}, charged: ${r1.charged}`)
print(`receipt present: ${r1.hasReceipt}, timestamp present: ${r1.hasTimestamp}`)

// Resume against the COMPLETED log: idempotent — re-runs nothing, returns the same
// recorded result. (A real resume after a crash would re-run the workflow from the
// top, replaying completed activities and executing only the not-yet-recorded tail.)
let [r2, err2] = recover(() => resume(signupFlow, { id: 42 }, { log: LOG }))
if (err2 != nil) {
  print(`resume failed: ${err2.message}`)
  exit(1)
}
print(`resume idempotent: ${r1.user == r2.user && r1.charged == r2.charged}`)

// Clean up the log file.
if (exists(LOG)) {
  remove(LOG)
}
print("done")
