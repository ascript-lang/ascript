//! WASM §7.3 — negative space: the wasm port changes NOTHING native-observable.
//!
//! These tests pin the invariants that Gate W-1 ("native byte-identical") rests on,
//! read straight from source so they track intent, not a stale copy:
//! - `ASO_FORMAT_VERSION` is NOT bumped on this branch (WASM is no-`.aso`-change);
//! - the compiler / serializer / opcode paths carry ZERO `target_family = "wasm"`
//!   cfg (the wasm portability lives in `platform.rs`, the worker/stack guards, and
//!   the target-dep tables — never in the platform-independent codegen path);
//! - the Cargo target-dep split keeps `tokio`/`rustyline` on the non-wasm row with
//!   today's exact native feature set (so native resolution is unchanged).
use std::fs;

#[test]
fn aso_format_version_unchanged() {
    // Read the constant from source so the pin tracks intent, not a copy. The value
    // seen at branch time is 29 (ELIDE's `Op::CallElided`); bumping it on this branch
    // is a spec violation (WASM §7.3: no `.aso` change).
    let src = fs::read_to_string("src/vm/aso.rs").unwrap();
    let line = src
        .lines()
        .find(|l| l.contains("pub const ASO_FORMAT_VERSION"))
        .expect("ASO_FORMAT_VERSION declaration present");
    let at_branch: u32 = 29;
    assert!(
        line.contains(&at_branch.to_string()),
        "WASM must not bump ASO_FORMAT_VERSION (spec §7.3); saw line: {line}"
    );
}

#[test]
fn no_wasm_cfg_in_chunk_or_serializer() {
    // The compiler/serializer/opcode paths stay platform-independent (spec §7.3):
    // wasm portability never leaks into the codegen path.
    for f in [
        "src/vm/chunk.rs",
        "src/vm/aso.rs",
        "src/vm/opcode.rs",
        "src/compile/mod.rs",
    ] {
        let src = fs::read_to_string(f).unwrap();
        assert!(
            !src.contains("target_family = \"wasm\""),
            "wasm cfg leaked into {f} (spec §7.3: codegen path is platform-independent)"
        );
    }
}

#[test]
fn cargo_target_dep_split_keeps_native_row() {
    // Gate W-1: the target-dep tables must keep `tokio` (with today's exact native
    // feature set incl. `rt-multi-thread`) and `rustyline` on the NON-wasm row so
    // native dependency resolution is byte-identical.
    let toml = fs::read_to_string("Cargo.toml").unwrap();
    assert!(
        toml.contains("[target.'cfg(not(target_family = \"wasm\"))'.dependencies]"),
        "the non-wasm target-dep table must exist (WASM §5.3.2)"
    );
    assert!(
        toml.contains("[target.'cfg(target_family = \"wasm\")'.dependencies]"),
        "the wasm target-dep table must exist (WASM §5.3.2)"
    );
    // The non-wasm tokio row must retain rt-multi-thread (the native feature set is
    // unchanged — only the wasm row drops it as vestigial).
    let non_wasm_table = toml
        .split("[target.'cfg(not(target_family = \"wasm\"))'.dependencies]")
        .nth(1)
        .expect("non-wasm table body");
    let non_wasm_body = non_wasm_table
        .split("\n[target")
        .next()
        .expect("non-wasm table delimited");
    assert!(
        non_wasm_body.contains("rt-multi-thread"),
        "native tokio row must keep rt-multi-thread (Gate W-1: native feature set unchanged)"
    );
    assert!(
        non_wasm_body.contains("rustyline"),
        "rustyline must stay on the non-wasm row (it pulls native fd-lock; WASM §5.3.2)"
    );
}
