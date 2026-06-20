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
