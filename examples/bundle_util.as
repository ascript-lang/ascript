// A small library module imported by `bundle_multimodule.as`. It only DEFINES
// bindings (no top-level side effects), so importing it prints nothing and running
// it standalone is a deterministic no-op — both are fine for the conformance corpus.
//
// Used by the self-contained-bundles feature to exercise `compile_archive` walking a
// relative import edge (`bundle_multimodule.as` → `./bundle_util`).

export fn greet(name: string): string {
    return `Hello, ${name}!`
}

export fn shout(text: string): string {
    // (kept method-free so the module needs no stdlib imports)
    return `${text}!!!`
}

// Intentionally UNUSED export: nothing imports `whisper`, so it is a real drop
// target for the bundle tree-shaker (the shaken archive omits it). It never runs,
// so it changes no program output — the existing run-output assertions still hold.
export fn whisper(text: string): string {
    return text
}
