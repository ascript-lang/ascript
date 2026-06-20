// The SCRIPT side of the c-host embedding example (EMBED §12).
//
// Like rust-host/game.as, this is NOT runnable via `ascript run` — it imports
// `host:plugin`, registered at runtime by main.c. A plain CLI run raises the
// recoverable `host module 'host:plugin' is not registered in this isolate` panic
// (asserted by tests/cli.rs). It is NOT a vm_differential corpus member (the corpus
// enumerates examples/*.as + examples/advanced/*.as, non-recursive).

import * as plugin from "host:plugin"

// A pure transform that calls a C host FUNC (host:plugin.scale) with a userdata bias.
fn transform(x) {
  return plugin.scale(x)
}

// Exercises the FALLIBLE tier: host:plugin.checked returns the [value, err] pair —
// err non-nil when the argument is 0.
fn checked(x) {
  let [v, err] = plugin.checked(x)
  if (err != nil) {
    print(`checked(${x}): err=${err.message}`)
    return -1
  }
  return v
}
