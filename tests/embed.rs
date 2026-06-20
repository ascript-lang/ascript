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

// ── Task 1.3: call / call_value / globals / load_archive + async variants ───

use ascript::embed::AsValue;

#[test]
fn call_a_global_function() {
    let iso = Isolate::builder().build().unwrap();
    iso.eval("fn add(a, b) { return a + b }").unwrap();
    let r = iso.call("add", &[AsValue::from(2i64), AsValue::from(3i64)]).unwrap();
    assert_eq!(r.as_int(), Some(5));
}

#[test]
fn call_auto_awaits_an_async_fn() {
    let iso = Isolate::builder().build().unwrap();
    iso.eval("async fn slow(x) { return x * 10 }").unwrap();
    // The call returns a future<T> (eager-scheduled); `call` drives it to completion.
    let r = iso.call("slow", &[AsValue::from(4i64)]).unwrap();
    assert_eq!(r.as_int(), Some(40));
}

#[test]
fn call_undefined_is_typed() {
    let iso = Isolate::builder().build().unwrap();
    let e = iso.call("nope", &[]).unwrap_err();
    assert!(matches!(e, EmbedError::Undefined(_)), "got {e:?}");
}

#[test]
fn call_non_callable_global_is_a_panic() {
    let iso = Isolate::builder().build().unwrap();
    iso.eval("let x = 7").unwrap();
    let e = iso.call("x", &[]).unwrap_err();
    // The engine's own "value is not callable" Tier-2 panic surfaces as Panic.
    assert!(matches!(e, EmbedError::Panic(_)), "got {e:?}");
}

#[test]
fn global_read_and_set_global() {
    let iso = Isolate::builder().build().unwrap();
    iso.eval("let n = 1").unwrap();
    assert_eq!(iso.global("n").unwrap().as_int(), Some(1));
    assert!(iso.global("missing").is_none());

    // set_global defines a NEW mutable global readable from a later eval.
    iso.set_global("injected", AsValue::from(99i64)).unwrap();
    assert_eq!(iso.eval("injected + 1").unwrap().as_int(), Some(100));
}

#[test]
fn call_value_on_a_function_handle() {
    let iso = Isolate::builder().build().unwrap();
    iso.eval("fn square(x) { return x * x }").unwrap();
    let f = iso.global("square").unwrap();
    let r = iso.call_value(&f, &[AsValue::from(6i64)]).unwrap();
    assert_eq!(r.as_int(), Some(36));
}

#[test]
fn load_archive_runs_compiled_bytes() {
    // Build a single-module `.aso`'s bytes from source (the `ascript build` artifact),
    // then run it on a fresh isolate via the from_bytes_verified trust boundary.
    let chunk = ascript::compile::compile_source("let v = 6\nv * 7\n").expect("compile");
    let bytes = chunk.to_bytes().expect("serialize .aso");

    let iso = Isolate::builder().build().unwrap();
    let r = iso.load_archive(&bytes).unwrap();
    assert_eq!(r.as_int(), Some(42), "archive program's trailing value");
}

#[test]
fn load_archive_corrupt_bytes_is_archive_error() {
    let iso = Isolate::builder().build().unwrap();
    let e = iso.load_archive(b"not a valid aso").unwrap_err();
    assert!(matches!(e, EmbedError::Archive(_)), "got {e:?}");
}

// ── §4.2 async variants: b1 (current-thread) + b2 (multi-thread, from test thread) ──

#[test]
fn eval_async_under_current_thread_localset() {
    // b1: a host with a current-thread runtime awaits eval_async inside run_until.
    let iso = Isolate::builder().build().unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    let v = rt.block_on(local.run_until(async {
        iso.eval_async("let a = 5\na + 2\n").await.unwrap()
    }));
    assert_eq!(v.as_int(), Some(7));
}

#[test]
fn call_async_under_multi_thread_block_on() {
    // b2: a host with a multi-thread runtime drives from a non-worker (test) thread via
    // LocalSet::block_on — the !Send future runs on the calling thread.
    let iso = Isolate::builder().build().unwrap();
    iso.eval("fn add(a, b) { return a + b }").unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    let v = local.block_on(&rt, async {
        iso.call_async("add", &[AsValue::from(10i64), AsValue::from(11i64)])
            .await
            .unwrap()
    });
    assert_eq!(v.as_int(), Some(21));
}
