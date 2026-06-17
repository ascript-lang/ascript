// std/workflow — durability modes: "group" for order-processing pipelines.
//
// This example demonstrates the THREE durability modes with a production-shaped
// order-processing workflow:
//
//   "fsync"   (default) — whole-log snapshot at finish + F_FULLFSYNC per commit.
//                         Completed commits are never lost. A crash mid-run loses
//                         the whole in-flight run; resume re-executes all activities.
//
//   "group"   (new)     — per-event append at each recording call, fsyncs coalesced
//                         by window policy (default: 128 events or 50 ms). A crash
//                         loses nothing — records reach the OS page cache immediately.
//                         Power loss loses at most the unsynced tail (window-bounded).
//
//   "buffered"          — whole-log snapshot at finish, no explicit fsync. Fastest
//                         but weakest: a crash OR power loss may lose in-flight work.
//
// Loss-window contract (spec §4.2):
//   kill -9 mid-run    | fsync: whole run lost  | group: nothing lost
//   power loss         | fsync: nothing lost (committed) | group: ≤ window tail lost
//   activity semantics | ALL modes: at-least-once — design activities to be IDEMPOTENT
//
// The default is FULL DURABILITY ("fsync"). "group" and "buffered" are explicit
// opt-ins per workflow — there is no global default that silently relaxes durability.
//
// Run: ascript run examples/advanced/workflow_durability.as
//      ascript run --tree-walker examples/advanced/workflow_durability.as
// (output is identical on both engines — the log path is private to this process)
//
// Note: this example writes a durable log to /tmp. It is excluded from the
// parallel multi-oracle conformance corpus (EXAMPLE_SKIPS SharedExternalState)
// because concurrent corpus runs sharing the same /tmp path race each other's
// cleanup. Run it in isolation: `ascript run examples/advanced/workflow_durability.as`.
import { run, resume, activity } from "std/workflow"
import { exists, remove } from "std/fs"

// --- activities: idempotent by design ----------------------------------------
//
// Activities are AT-LEAST-ONCE in every durability mode. A crash between the
// side effect and the log append re-executes the activity on resume. Design every
// activity so that running it twice produces the same result (idempotency):
//
//   - Use database upsert semantics rather than blind insert.
//   - Include an order-id / idempotency-key on payment API calls.
//   - Guard shipping: "ship if not already shipped for order X".
//
// The activities below are pure functions (no real network I/O) so they are
// trivially idempotent. In production, the result returned to the workflow is
// what matters — it gets replayed from the log on resume without re-executing.
let validateOrder = activity("validateOrder", (order) => {
  // Production: validate payment method, check inventory, etc.
  // IDEMPOTENT: validate is read-only; safe to re-run.
  if (order.amount <= 0) {
    return {ok: false, reason: "amount must be positive"}
  }
  return {ok: true, orderId: order.id, validated: true}
})

let reserveInventory = activity("reserveInventory", (order) => {
  // Production: decrement stock with a conditional update (idempotent via orderId).
  // "If reservation for orderId already exists, return existing record."
  return {reserved: true, sku: order.sku, qty: order.qty, reservationId: `res-${order.id}`}
})

let chargePayment = activity("chargePayment", (amount, orderId) => {
  // Production: charge via payment gateway with idempotency key = orderId.
  // "If charge for orderId already exists, return existing receipt."
  return {charged: true, amount: amount, txnId: `txn-${orderId}`, idempotencyKey: orderId}
})

let fulfillOrder = activity("fulfillOrder", (orderId, reservationId) => {
  // Production: trigger warehouse pick+pack with idempotent shipment creation.
  return {shipped: true, orderId: orderId, trackingId: `track-${orderId}`}
})

// --- the workflow: deterministic control flow ---------------------------------
fn processOrder(ctx, input) {
  // Step 1: validate
  let validation = ctx.call(validateOrder, input)
  if (!validation.ok) {
    return {ok: false, reason: validation.reason}
  }

  // Step 2: reserve inventory
  let reservation = ctx.call(reserveInventory, input)

  // Step 3: charge payment (idempotency key = orderId guards against double-charge)
  let payment = ctx.call(chargePayment, input.amount, input.id)
  if (!payment.charged) {
    return {ok: false, reason: "payment failed"}
  }

  // Step 4: fulfill — trigger shipment
  let shipment = ctx.call(fulfillOrder, input.id, reservation.reservationId)

  // ctx.now() records the virtual clock — deterministic across record/replay.
  let at = ctx.now()
  return {ok: true, orderId: input.id, txnId: payment.txnId, trackingId: shipment.trackingId, reservationId: reservation.reservationId, processedAt: at > 0}
}

// --- helpers -----------------------------------------------------------------
fn run_order(order, label, log_path, durability) {
  if (exists(log_path)) {
    remove(log_path)
  }
  let [result, err] = recover(() => run(processOrder, order, {log: log_path, durability: durability}))
  if (err != nil) {
    print(`[${label}] ERROR: ${err.message}`)
    return nil
  }
  return result
}

fn resume_order(order, label, log_path) {
  let [result, err] = recover(() => resume(processOrder, order, {log: log_path}))
  if (err != nil) {
    print(`[${label}/resume] ERROR: ${err.message}`)
    return nil
  }
  return result
}

// --- main: demo all three durability modes -----------------------------------
let ORDER = {id: "ord-2026-001", sku: "widget-pro", qty: 3, amount: 4200}
let LOG = "/tmp/ascript_workflow_durability.log"

// --- "group" mode: the recommended choice for high-throughput pipelines ------
//
// Each ctx.call immediately appends its event to the OS page cache. A kill -9
// loses nothing; power loss loses at most the unsynced tail (≤ 50 ms / 128 events
// by default). Activities replay from the log on resume — no re-execution for the
// persisted prefix.
//
// groupWindowMs (default 50) and groupMaxEvents (default 128) control the fsync
// coalescing policy. Tighter window = less power-loss exposure; wider = fewer fsyncs.
let r1 = run_order(ORDER, "group", LOG, "group")
if (r1 != nil) {
  print(`[group] ok=${r1.ok} order=${r1.orderId} txn=${r1.txnId}`)
  print(`[group] tracking=${r1.trackingId} reservation=${r1.reservationId}`)
  print(`[group] processedAt present=${r1.processedAt}`)

  // Resume against completed log: idempotent — returns the recorded result without
  // re-running any activity. In a real crash-recovery scenario, resume would
  // re-execute only the activities whose results are NOT yet in the log.
  let r1b = resume_order(ORDER, "group", LOG)
  if (r1b != nil) {
    print(`[group/resume idempotent] same-txn=${r1.txnId == r1b.txnId}`)
  }
}
if (exists(LOG)) {
  remove(LOG)
}

// --- "fsync" mode (default): maximum durability, one fsync per commit --------
//
// The log is rewritten as an atomic temp+rename snapshot at finish, with
// F_FULLFSYNC + directory fsync. Completed commits are never lost. A crash
// mid-run loses the entire in-flight run; resume re-executes all activities.
// Use this when per-commit durability is required and throughput is secondary.
let r2 = run_order(ORDER, "fsync", LOG, "fsync")
if (r2 != nil) {
  print(`[fsync] ok=${r2.ok} order=${r2.orderId} txn=${r2.txnId}`)
  print(`[fsync] tracking=${r2.trackingId} reservation=${r2.reservationId}`)
}
if (exists(LOG)) {
  remove(LOG)
}

// --- "buffered" mode: no explicit fsync, OS-asynchronous writeback -----------
//
// The log is rewritten as a whole-log snapshot at finish with NO fsync.
// Fastest, but weakest: a crash or power loss between commits may lose the
// in-flight run AND recent completed commits (OS writeback horizon, typically
// seconds to minutes). Use only where durability is not required.
let r3 = run_order(ORDER, "buffered", LOG, "buffered")
if (r3 != nil) {
  print(`[buffered] ok=${r3.ok} order=${r3.orderId} txn=${r3.txnId}`)
  print(`[buffered] tracking=${r3.trackingId} reservation=${r3.reservationId}`)
}
if (exists(LOG)) {
  remove(LOG)
}

print("done")
