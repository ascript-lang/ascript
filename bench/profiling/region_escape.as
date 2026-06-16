// REGION Phase-2 gate workload (spec §5.5 G2 — the worst case for the recycler).
//
// Two cohorts, both shaped to STRESS the recycler's runtime miss paths (not its
// happy path). The loops live INSIDE a function so the per-iteration object lands
// in a real frame SLOT (a `SetLocal`-overwrite kill site `region_candidates` can
// flag) — a top-level/module loop routes the var through GET_GLOBAL and is never a
// kill site, so it would silently exercise nothing (a measured fact of this build).
//
//   Cohort A — ESCAPE: every constructed object is `push`ed onto a RETAINED array.
//     At the would-be kill point the array holds a live ref, so strong_count >= 2
//     → the runtime guard MUST MISS. (The static pass ALSO disqualifies the Call
//     arg, so the miss may also be a static reject — either way recycled stays 0.)
//
//   Cohort B — ALIAS-then-OVERWRITE: the object is aliased into a SECOND live local
//     before its slot is overwritten, so at the kill point strong_count >= 2. This
//     is the case the spec calls out as "passes a naive static pass but the refcount
//     proof is the backstop" — it exercises the RUNTIME miss path specifically.
//
// G2 asserts this workload's wall does not regress > 5% region-on vs off, and that
// the recycler records the misses it claims (read via ASCRIPT_REGION_STATS).
import { push } from "std/array"
import * as time from "std/time"

// Cohort A: escape into a retained array — all kills MISS (live alias).
fn escape_cohort(n) {
  let kept = []
  let acc = 0
  for (i in 0..n) {
    let o = { v: i, w: i + 1 }
    push(kept, o)
    acc = acc + o.v
  }
  // touch the retained array so it cannot be optimized away
  return acc + len(kept) + kept[n / 2].w
}

// Cohort B: alias-then-overwrite. The SAME slot is aliased into `alias` and then
// overwritten in place, so at the in-iteration overwrite kill the dying cell has a
// live second ref (`alias`) → strong_count >= 2 → the RUNTIME guard MUST MISS
// (this is the path the spec's §3.3 refcount backstop exists for). The loop also
// has a uniquely-owned dying cell at the back-edge (the post-overwrite `o`), which
// IS recyclable — so this cohort exercises BOTH a runtime miss and a recycle.
fn alias_cohort(n) {
  let acc = 0
  for (i in 0..n) {
    let first = { x: i }
    let alias = first        // `first` now has a live second ref
    first = { x: i + 1 }     // overwrite `first`'s slot: dying cell aliased → MISS
    acc = acc + alias.x + first.x
  }
  return acc
}

let t0 = time.monotonic()
let total = 0
for (r in 0..40) {
  total = total + escape_cohort(50000)
  total = total + alias_cohort(50000)
}
let t1 = time.monotonic()
print(`region_escape: total=${total} elapsed_ms=${t1 - t0}`)
