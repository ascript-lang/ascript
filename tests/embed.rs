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

// A trivial use of EmbedError so the import is exercised even before eval lands;
// replaced by real eval-error tests in Task 1.2.
#[test]
fn embed_error_is_an_error_type() {
    fn assert_error<E: std::error::Error>() {}
    assert_error::<EmbedError>();
}
