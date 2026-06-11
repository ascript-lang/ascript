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
