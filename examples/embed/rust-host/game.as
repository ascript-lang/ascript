// The SCRIPT side of the rust-host embedding example (EMBED §12).
//
// This file is NOT runnable via `ascript run` — it imports `host:game`, a module
// the embedding host registers at runtime. A plain CLI run misses that registry
// and raises the recoverable panic
//   `host module 'host:game' is not registered in this isolate`
// (asserted by tests/cli.rs). It is loaded + driven by main.rs, which DOES register
// the module. It is therefore also NOT a vm_differential corpus member (the corpus
// enumerates examples/*.as + examples/advanced/*.as, non-recursive — never
// examples/embed/**).

import * as game from "host:game"

// Module-scope state the SCRIPT owns. The host reads/writes this object across the
// boundary by CONTAINER HANDLE (get_key/set_key) — same ObjectCell, no deep copy.
let state = { tick: 0, score: 0, log: [] }

// A pure per-tick update returning a state delta. The host applies the delta to the
// shared `state` object by handle, so both sides observe the same cell.
fn on_tick(n) {
  // Call a host FUNC (host:game.log) — its return is ignored, it just records.
  game.log(`tick ${n}`)
  // Call a host FALLIBLE FUNC (host:game.rand_seeded) — returns the [value, err]
  // Tier-1 pair. A negative seed is the documented failure (we never pass one here).
  let [r, err] = game.rand_seeded(n)
  if (err != nil) {
    return { delta: 0, note: `rand failed: ${err.message}` }
  }
  return { delta: r, note: `ok` }
}

// An async fn — `Isolate::call` AUTO-AWAITS the returned future (§3.3). Exercises the
// auto-await path: the host calls `on_save` synchronously and gets the resolved value.
async fn on_save() {
  return `saved at tick ${state.tick} with score ${state.score}`
}

// EDGE: a host fn that ALWAYS raises HostError::Panic, wrapped in `recover` (the
// arrow form — the bare-fn-expression recover gotcha). The script observes the
// host panic as a recoverable [nil, err] pair instead of aborting.
fn probe_host_panic() {
  let [v, err] = recover(() => game.boom("detonate"))
  if (err != nil) {
    return `recovered host panic: ${err.message}`
  }
  return `boom did not panic (unexpected): ${v}`
}

// EDGE: a capabilities-denial probe. The isolate is built deny-all, so `fs.read`
// is denied (`capability 'fs' denied`). The script observes the denial via the
// Tier-1 [content, err] pair fs.read returns (or via recover for a Tier-2 form).
import * as fs from "std/fs"
fn probe_caps_denial() {
  let [content, err] = recover(() => fs.read("/etc/hostname"))
  if (err != nil) {
    return `fs denied: ${err.message}`
  }
  return `fs allowed (unexpected): read ${len(content)} bytes`
}
