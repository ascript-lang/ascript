//! EMBED negative space + substrate pins (spec §11). EMBED adds NO language surface:
//! these pins prove the engine envelope is untouched for the life of the branch, and
//! document the shipped substrate `Isolate` composes (the REPL session model, the
//! `user_global` read hook, the `classify_specifier` import seam).
//!
//! These tests are feature-config-independent (they use only the always-on engine
//! surface), so they run green under both `cargo test` and
//! `cargo test --no-default-features`.

use ascript::value::ValueKind;

/// The `.aso` format does not move (spec: no opcode, no serialization change).
///
/// EMBED is a host-side facade — it adds no language surface, so the bytecode
/// format MUST NOT move. Read FROM SOURCE (`ascript::vm::aso::ASO_FORMAT_VERSION`)
/// — never hardcode the constant in two places.
///
/// NOTE: the EMBED plan text says 27 — it predates ELIDE's 28→29 bump. The CURRENT
/// merge-base value is 29 (`src/vm/aso.rs`). EMBED must not move it; this expected
/// drift from the plan is NOT a substrate problem.
#[test]
fn aso_format_version_pinned() {
    assert_eq!(
        ascript::vm::aso::ASO_FORMAT_VERSION,
        29,
        "EMBED must not bump the .aso format — a bump means an engine change leaked in"
    );
}

/// No new opcode (spec §11 "no language surface"). EMBED is a host-side facade — its
/// only two engine touches (the `SpecifierKind::Host` arm in `classify_specifier`; the
/// host-registry lookup on `call_stdlib`'s already-error fall-through arm) ride EXISTING
/// dispatch and add NO `Op`. The count is derived from `Op::from_u8` (the round-trip
/// decode door — every valid discriminant decodes; nothing else does), mirroring the
/// established `tests/par_negative_space.rs` / `shape_negative_space.rs` technique, with
/// the complementary "`CallElided` is still the last variant" check.
///
/// 121 is the merge-base value: DEFER added `DeferPush`+`DeferPushMethod` (→ 120), ELIDE
/// added `CallElided` (→ 121). Update this ONLY if a NON-EMBED feature adds an opcode —
/// for EMBED it must never move.
#[test]
fn no_new_opcode_for_embed() {
    const EXPECTED_OP_COUNT: usize = 121;

    let op_count = (0u16..=255)
        .filter(|&b| ascript::vm::opcode::Op::from_u8(b as u8).is_some())
        .count();

    assert_eq!(
        op_count, EXPECTED_OP_COUNT,
        "Op variant count changed — EMBED must add NO opcode (host modules dispatch \
         through the existing builtin/import rails); update EXPECTED_OP_COUNT only if a \
         non-EMBED feature added it."
    );

    // Complementary: `CallElided` (discriminant 120) is still the last variant, so the
    // count above isn't masking an added-then-removed pair.
    assert_eq!(
        ascript::vm::opcode::Op::CallElided as u16 + 1,
        EXPECTED_OP_COUNT as u16,
        "CallElided is no longer the last Op variant — a new opcode was appended after \
         it; EMBED appends none."
    );
}

/// The REPL substrate behaviors `Isolate::eval` lifts (spec §3.3): the trailing
/// expression's value is the program result, and an earlier binding is visible to a
/// later expression in the same session scope.
///
/// `Vm::new`/`install_self` are crate-internal (the integration-test boundary can't
/// build a persistent `Vm` directly), so the *observable* substrate is pinned through
/// the public `vm_eval_source` door, which internally performs the EXACT
/// `Interp::new` → `install_self` → `Vm::new` → `compile_source` → `Fiber` →
/// `vm.run` (under a `LocalSet`) dance `eval_line_vm` uses and that `Isolate::eval`
/// lifts in Task 1.2 — only persistence across SEPARATE calls (which needs a held
/// `Vm` and the crate-internal `install_self`) is what `Isolate` adds on top; that
/// held-`Vm` cross-call persistence is pinned directly by `Isolate::eval` in
/// `tests/embed.rs` (`eval("let x = 2"); eval("x + 1") == 3`).
#[tokio::test]
async fn pin_vm_trailing_expression_and_session_scope() {
    // A statement-terminated program → trailing value is `Nil`.
    let v = ascript::vm_eval_source("let x = 2\n").await.expect("eval");
    assert!(matches!(v.kind(), ValueKind::Nil), "statement program → Nil");

    // An earlier `let` binds for a later expression in the same module scope; the
    // trailing bare expression is the returned value.
    let v = ascript::vm_eval_source("let x = 2\nx + 1\n")
        .await
        .expect("eval");
    assert_eq!(v.as_int(), Some(3), "trailing expression value, earlier binding visible");
}

/// `user_global` is the read hook (the `vm/run.rs` doc comment says "REPL/embedders")
/// and `call_value` invokes a callable read out of it — the exact two-step
/// `Isolate::call` lifts. Observed end-to-end through `vm_eval_source`: a top-level
/// `fn` is a module-scope user-global, and calling it returns the computed value.
#[tokio::test]
async fn pin_user_global_read_and_call_value() {
    // Define a top-level fn (a user-global), then call it — the program's trailing
    // expression is `f(2, 5)`, proving the define→read→call path the embed `call`
    // (`user_global` → `call_value`) walks.
    let v = ascript::vm_eval_source("fn f(a, b) { return a + b }\nf(2, 5)\n")
        .await
        .expect("eval");
    assert_eq!(v.as_int(), Some(7), "user-global fn reads back and is callable");
}

/// EMBED §6.1 (FLIPPED in Task 3.2): `classify_specifier("host:app")` now classifies
/// as `SpecifierKind::Host` (checked before package classification — a package key can
/// never carry a `:`, so the reservation is structural). On a NON-embed CLI run no host
/// module is registered, so importing `"host:app"` raises the host-specific MISS panic
/// `host module 'host:app' is not registered in this isolate` (recoverable) — NOT the
/// old `unknown package 'host:app'` tail.
///
/// This was the Phase-0 `UnknownPackage` pin; Task 3.2 flipped it failing-test-first.
#[tokio::test]
async fn pin_host_specifier_classifies_as_host() {
    // `vm_run_source` runs the source on the VM and returns captured output; a
    // top-level import of an UNregistered host module is a Tier-2 panic (recoverable)
    // surfaced as an `Err(AsError)`.
    let err = ascript::vm_run_source("import * as app from \"host:app\"\n")
        .await
        .expect_err("importing an unregistered host:app must raise the miss panic");
    assert!(
        err.message
            .contains("host module 'host:app' is not registered in this isolate"),
        "host:app must classify as Host and raise the registry-miss panic; got: {}",
        err.message
    );
    // It must NOT be the old UnknownPackage tail.
    assert!(
        !err.message.contains("unknown package"),
        "host: no longer routes through UnknownPackage; got: {}",
        err.message
    );
}

/// EMBED Unit E (§12, Gate 9): the embedding examples live under `examples/embed/**`,
/// which the `vm_differential` corpus discovery — `examples/*.as` +
/// `examples/advanced/*.as`, NON-RECURSIVE (verified `tests/vm_differential.rs`
/// `all_corpus_examples`) — does NOT auto-claim. These scripts import `host:` modules
/// and are only runnable inside their host (main.rs / main.c), so they must NOT be
/// corpus members (there is no byte-identical CLI reference output). This pin mirrors
/// the exact discovery enumeration and asserts no `embed/` path is ever produced.
#[test]
fn examples_embed_is_excluded_from_corpus_discovery() {
    let root = env!("CARGO_MANIFEST_DIR");

    // Mirror vm_differential's `all_corpus_examples`: read ONLY the two flat dirs,
    // non-recursively, collecting `*.as` file names.
    let mut corpus: Vec<String> = Vec::new();
    for dir in ["examples", "examples/advanced"] {
        let p = std::path::Path::new(root).join(dir);
        let rd = std::fs::read_dir(&p).unwrap_or_else(|e| panic!("read_dir {dir}: {e}"));
        for entry in rd {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|x| x.to_str()) == Some("as") {
                corpus.push(format!(
                    "{dir}/{}",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }

    // No corpus entry may reference the embed subtree (it is a SUBDIR of `examples/`,
    // and the discovery is non-recursive, so this is structurally impossible — the pin
    // catches any future switch to a recursive walk that would silently absorb them).
    for entry in &corpus {
        assert!(
            !entry.contains("embed/"),
            "vm_differential corpus discovery picked up an embed example ({entry}); \
             examples/embed/** must stay OUT of the corpus (host:-only, host-driven). \
             If discovery became recursive, add these to EXAMPLE_SKIPS with a reason."
        );
    }

    // The embed example scripts DO exist on disk (so the pin is meaningful, not vacuous)
    // — they are simply not in the corpus enumeration.
    for rel in [
        "examples/embed/rust-host/game.as",
        "examples/embed/c-host/plugin.as",
    ] {
        let path = std::path::Path::new(root).join(rel);
        assert!(path.exists(), "embed example script missing: {rel}");
        // And they are NOT in the corpus list (the file name alone is also absent under
        // the flat-dir keys).
        let fname = path.file_name().unwrap().to_string_lossy();
        assert!(
            !corpus.iter().any(|c| c.ends_with(&*fname)),
            "embed script {fname} leaked into the flat-dir corpus enumeration"
        );
    }
}
