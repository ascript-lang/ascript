// bundle_caps_util.as
// ---------------------------------------------------------------------------
// A tiny, side-effect-free helper library imported by `bundle_caps.as`. It is
// imported BOTH ways across the example — as a namespace (`import * as guard`)
// for `guard.report` AND by name (`import { capLabel }`) for the formatter — so
// the program exercises both import forms over a real relative-import edge (the
// shape a multi-module bundle embeds via `compile_archive`).
//
// It only DEFINES bindings (no top-level statements), so importing it prints
// nothing and running it standalone is a deterministic no-op — corpus-safe.
// Everything here is CORE (no stdlib imports), so it runs in every feature
// config, including `--no-default-features`.
// ---------------------------------------------------------------------------

// Render a capability + its current grant state as one stable, human-readable line.
export fn capLabel(name: string, granted: bool): string {
    let state = granted ? "granted" : "denied"
    return `  ${name}: ${state}`
}

// Print a labelled section header followed by each capability's state. Pure I/O
// over already-decided values, so the output is fully deterministic.
export fn report(title: string, names: array<string>, states: array<bool>) {
    print(title)
    let i = 0
    while (i < len(names)) {
        print(capLabel(names[i], states[i]))
        i = i + 1
    }
}
