// cron_next.as — compute cron fire times deterministically.
//
// `std/cron` is a 5-field Vixie-cron engine (`min hour dom month dow`). The
// schedule-firing helpers (`next`, `nextN`, `matches`) take an `after` epoch-ms
// anchor; passing a FIXED `after` (never `time.now()`) makes the computed times
// reproducible — so this example is a pure, deterministic computation.
//
// Each result is a Tier-1 `[value, err]` pair: a malformed expression (often
// user/config data) is a recoverable error, not a panic.
import * as cron from "std/cron"
import * as date from "std/date"

// A fixed UTC anchor: 2023-11-15 03:33 UTC (a Wednesday).
let after = 1700019180000

// Pretty-print an epoch-ms as a stable UTC civil-time string.
fn utc(ms) {
  return date.format(date.fromEpochMs(ms), "%Y-%m-%d %H:%M (%a) UTC")
}

// `parse` builds a reusable, inspectable schedule object (Tier-1).
let [_sched, parseErr] = cron.parse("0 9 * * 1-5")
print(`parse ok: ${parseErr == nil}`)

// next: the next "9am on a weekday" at or after the anchor.
let [n1, e1] = cron.next("0 9 * * 1-5", {after: after})
print(`next weekday 9am: ${utc(int(n1))} (err: ${e1})`)

// nextN: the next four runs of an every-15-minutes schedule.
let [runs, e2] = cron.nextN("*/15 * * * *", 4, {after: after})
print("next four */15 runs:")
for (r of runs) {
  print(`  ${utc(int(r))}`)
}

// tzOffset shifts the CIVIL-time interpretation (fixed offset, NO DST). Here
// "0 9 * * *" in UTC+120min (e.g. CEST) fires at 07:00 UTC.
let [n3, e3] = cron.next("0 9 * * *", {after: after, tzOffset: 120})
print(`daily 9am at UTC+120: ${utc(int(n3))}`)

// matches: does a given instant satisfy the schedule? 2023-11-15 09:00 UTC is a
// Wednesday, so the weekday-9am schedule matches it.
let [m1, _me1] = cron.matches("0 9 * * 1-5", 1700038800000)
print(`Wed 09:00 matches weekday-9am: ${m1}`)

// The Vixie DOM/DOW OR rule: when BOTH day-of-month and day-of-week are
// restricted, a day matches if EITHER does. "0 0 13 * 5" fires at midnight on
// the 13th OR on any Friday — so Friday the 10th (not the 13th) still matches.
let [m2, _me2] = cron.matches("0 0 13 * 5", 1699574400000)
print(`Fri the 10th matches "13th OR Friday": ${m2}`)

// An impossible schedule (Feb 30th) exhausts the 5-year scan → Tier-1 error.
let [_imp, impErr] = cron.next("0 0 30 2 *", {after: after})
print(`impossible schedule errors: ${impErr != nil}`)

print("cron_next ok")
