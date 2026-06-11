// bundle_caps.as
// ---------------------------------------------------------------------------
// The BUNDLE + CAPABILITIES story, as a single runnable program.
//
// A self-contained bundle (`ascript build --native`) ships its whole reachable
// module graph plus a capability FLOOR baked in at build time (`--deny net,fs`).
// This example demonstrates the same posture WITHOUT a build step: it uses the
// CORE `std/caps` API to IRREVERSIBLY self-restrict a capability at startup
// (`caps.drop`), then takes a graceful-degradation branch based on what is still
// granted — exactly the pattern a `--deny`-built bundle's code runs.
//
// It is multi-module: the sibling `bundle_caps_util.as` is imported BOTH ways —
// as a namespace (`guard`) and by name (`capLabel`) — so the program exercises
// the relative-import edge a bundle archive embeds, in both import forms.
//
// Everything used here is CORE (`std/caps` has no Cargo feature; the helpers use
// no stdlib), so the program runs identically in every feature config, including
// `--no-default-features`. Output is fully DETERMINISTIC (no clock, no RNG, no
// I/O beyond `print`), so it participates in the corpus byte-identity gates.
// ---------------------------------------------------------------------------

import * as caps from "std/caps"
import * as guard from "./bundle_caps_util"
import { capLabel } from "./bundle_caps_util"

// The set of capabilities this bundle declares it will operate WITHOUT — the
// "deny floor" a hardened bundle would bake in at build time via `--deny`.
const DENY_FLOOR: array<string> = ["net", "fs", "process"]

// A capability-aware operation. Rather than blindly calling a (feature-gated)
// privileged API and crashing when it is denied, the program QUERIES the cap
// first and degrades gracefully. `recover` wraps the work so that even an
// unexpected denial surfaces as a recoverable Tier-1 pair, never an abort —
// the resilient shape a production bundle wants.
fn loadConfig(): string {
    // Use the arrow form for the recover thunk (the blessed pattern).
    let [result, err] = recover(() => {
        if (caps.has("fs")) {
            // In a fully-granted run this branch would read a config file; here we
            // keep it I/O-free so the example stays deterministic + core-portable.
            return "config: loaded from disk"
        }
        // Graceful degradation: fs was dropped, fall back to a built-in default.
        return "config: fs denied — using built-in defaults"
    })
    if (err != nil) {
        // Defensive: a denial (or any failure) degrades to a safe constant.
        return `config: unavailable (${err.message})`
    }
    return result
}

fn main() {
    print("=== bundle capability posture ===")

    // 1. Snapshot the host posture (under `ascript run`, everything is granted).
    let before = caps.has("net")
    print(capLabel("net (before drop)", before))

    // 2. Self-impose the deny floor IRREVERSIBLY. `caps.drop` only ever NARROWS —
    //    there is no re-grant — so this is the same one-way restriction a
    //    `--deny`-built bundle starts life with. Dropping an already-denied cap is
    //    a harmless no-op, so iterating the floor is safe.
    for (name of DENY_FLOOR) {
        caps.drop(name)
    }

    // 3. Report the resulting posture for every floor capability via the sibling
    //    helper (namespace import), reading each state with `caps.has`.
    let states: array<bool> = []
    for (name of DENY_FLOOR) {
        states = [...states, caps.has(name)]
    }
    guard.report("dropped capabilities:", DENY_FLOOR, states)

    // 4. `caps.list()` returns the still-granted caps in stable order — proof the
    //    drop took effect and that `env`/`ffi` (not on the floor) remain.
    let remaining = caps.list()
    print(`still granted: ${remaining}`)

    // 5. Run the capability-aware operation. With `fs` dropped it deterministically
    //    takes the degraded branch — the resilient behavior a hardened bundle relies
    //    on instead of crashing.
    print(loadConfig())

    // 6. Re-confirm irreversibility: querying again still reports denied (there is
    //    no API that could have re-granted it mid-run).
    print(capLabel("net (after drop)", caps.has("net")))

    print("done.")
}

main()
