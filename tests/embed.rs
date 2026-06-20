//! EMBED Unit A — the `ascript::embed` facade core (spec §3, §7).
//!
//! Integration tests for the host-embedding API: builder construction, the `!Send`
//! isolate model, blocking eval + nested-runtime detection, call/globals/load_archive.
#![cfg(feature = "embed")]

use ascript::embed::{EmbedError, Isolate};

#[test]
fn builder_constructs_and_isolate_is_not_send() {
    // The model IS the product (spec §1): an Isolate holds `Rc<Vm>` and an owned
    // current-thread runtime — it must be `!Send + !Sync` by construction.
    static_assertions::assert_not_impl_any!(ascript::embed::Isolate: Send, Sync);
    let iso = Isolate::builder().build().expect("default build");
    drop(iso);
}

#[test]
fn builder_is_chainable_with_defaults() {
    // The builder methods are additive and chain; `build()` validates + constructs.
    let iso = Isolate::builder()
        .output(ascript::embed::OutputMode::Capture)
        .args(&["prog", "a", "b"])
        .build()
        .expect("configured build");
    drop(iso);
}

#[test]
fn embed_error_is_an_error_type() {
    fn assert_error<E: std::error::Error>() {}
    assert_error::<EmbedError>();
}

// ── Task 1.2: blocking eval + nested-runtime detection ──────────────────────

#[test]
fn eval_trailing_expression_and_session_persistence() {
    let iso = Isolate::builder().build().unwrap();
    // A statement-terminated input → trailing value is nil.
    assert!(iso.eval("let x = 2").unwrap().is_nil());
    // The binding from the FIRST eval is visible in the SECOND — session persists.
    assert_eq!(iso.eval("x + 1").unwrap().as_int(), Some(3));
}

#[test]
fn eval_panic_survives_session_and_compile_error_mutates_nothing() {
    let iso = Isolate::builder().build().unwrap();
    iso.eval("let a = 1").unwrap();
    // A Tier-2 runtime panic (undefined name) → EmbedError::Panic; session survives.
    let e = iso.eval("nosuch()").unwrap_err();
    assert!(matches!(e, EmbedError::Panic(_)), "got {e:?}");
    // A compile error → EmbedError::Compile; no session mutation.
    let e = iso.eval("let oops = ").unwrap_err();
    assert!(matches!(e, EmbedError::Compile(_)), "got {e:?}");
    // The session is intact: `a` is still bound.
    assert_eq!(iso.eval("a").unwrap().as_int(), Some(1));
}

#[test]
fn eval_exit_is_typed_and_isolate_survives() {
    let iso = Isolate::builder().build().unwrap();
    let e = iso.eval("exit(3)").unwrap_err();
    assert!(matches!(e, EmbedError::Exit(3)), "got {e:?}");
    // The isolate stays usable after exit (the host decides what exit means).
    assert_eq!(iso.eval("1 + 1").unwrap().as_int(), Some(2));
}


#[test]
fn eval_capture_output_mode_buffers_print() {
    let iso = Isolate::builder()
        .output(ascript::embed::OutputMode::Capture)
        .build()
        .unwrap();
    iso.eval("print(\"hello\")").unwrap();
    assert_eq!(iso.take_output(), "hello\n");
    // The buffer drained on take; a second take is empty.
    assert_eq!(iso.take_output(), "");
}

#[tokio::test]
async fn blocking_eval_inside_runtime_is_a_typed_error() {
    let iso = Isolate::builder().build().unwrap();
    // Calling blocking eval from inside an ambient tokio runtime would panic in
    // tokio; the guard converts it to a typed error instead.
    assert!(matches!(iso.eval("1").unwrap_err(), EmbedError::NestedRuntime));
}
