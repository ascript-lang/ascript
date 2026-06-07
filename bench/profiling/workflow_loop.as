// PROFILE TARGET: the durable-workflow path (event-sourced run + replay).
// Runs a 2-activity workflow 3000 times, each writing/reading an append-only
// JSON event log on disk. Exercises the determinism context, JSON serialization
// of events, and fs I/O — the realistic cost profile of durable execution, which
// is I/O- and serialization-bound rather than dispatch-bound.
import { run, resume, activity } from "std/workflow"
import { exists, remove } from "std/fs"
import * as time from "std/time"

let LOG = "/tmp/ascript_bench_wf.log"

let fetchUser = activity("fetchUser", (id) => {
  return { id: id, name: `user-${id}`, price: 4200 }
})
let chargeCard = activity("chargeCard", (amount) => {
  return { ok: true, amount: amount }
})

fn flow(ctx, input) {
  let user = ctx.call(fetchUser, input.id)
  let receipt = ctx.uuid()
  let charge = ctx.call(chargeCard, user.price)
  return { ok: charge.ok, who: user.name, amount: charge.amount, hasReceipt: len(receipt) == 36 }
}

let t0 = time.monotonic()
let ok = 0
for (i in 0..3000) {
  if (exists(LOG)) { remove(LOG) }
  let [r, e] = recover(() => run(flow, { id: i }, { log: LOG }))
  if (e == nil && r.ok) { ok = ok + 1 }
}
let t1 = time.monotonic()
if (exists(LOG)) { remove(LOG) }
print(`workflow_loop: ok=${ok} elapsed_ms=${t1 - t0}`)
