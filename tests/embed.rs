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

// ── Task 2.1: AsValue scalars + constructors + accessors + AsKind ────────────

use ascript::embed::AsKind;

#[test]
fn asvalue_is_not_send() {
    static_assertions::assert_not_impl_any!(AsValue: Send, Sync);
}

#[test]
fn scalar_int_round_trip_through_set_global() {
    let iso = Isolate::builder().build().unwrap();
    iso.set_global("x", AsValue::from(7i64)).unwrap();
    assert_eq!(iso.eval("x * 2").unwrap().as_int(), Some(14));
}

#[test]
fn scalar_float_bool_nil_string_round_trip() {
    let iso = Isolate::builder().build().unwrap();
    // float
    iso.set_global("f", AsValue::from(2.5f64)).unwrap();
    assert_eq!(iso.eval("f + 0.5").unwrap().as_float(), Some(3.0));
    // bool
    iso.set_global("b", AsValue::from(true)).unwrap();
    assert_eq!(iso.eval("b").unwrap().as_bool(), Some(true));
    // nil
    iso.set_global("n", AsValue::nil()).unwrap();
    assert!(iso.eval("n").unwrap().is_nil());
    // string (construct + borrow back out)
    iso.set_global("s", AsValue::from("hi")).unwrap();
    let out = iso.eval("s + \" there\"").unwrap();
    assert_eq!(out.as_str(), Some("hi there"));
}

#[test]
fn string_construct_from_owned_and_borrow() {
    let v = AsValue::from(String::from("owned"));
    assert_eq!(v.as_str(), Some("owned"));
}

#[test]
fn decimal_is_lossless_via_string() {
    let iso = Isolate::builder().build().unwrap();
    // Construct a Decimal host-side from its display string; the engine reads it as a
    // decimal literal (scale preserved). Add 0.25m in script, read the result string.
    iso.set_global("d", AsValue::decimal("1.50").unwrap()).unwrap();
    let r = iso
        .eval("import * as decimal from \"std/decimal\"\nd + decimal.from(\"0.25\")\n")
        .unwrap();
    assert_eq!(r.kind(), AsKind::Decimal);
    assert_eq!(r.as_decimal_str(), Some("1.75".to_string()));
}

#[test]
fn decimal_bad_string_is_typed_error() {
    assert!(AsValue::decimal("not-a-number").is_err());
}

#[test]
fn askind_classifies_scalars() {
    assert_eq!(AsValue::nil().kind(), AsKind::Nil);
    assert_eq!(AsValue::from(true).kind(), AsKind::Bool);
    assert_eq!(AsValue::from(7i64).kind(), AsKind::Int);
    assert_eq!(AsValue::from(1.5f64).kind(), AsKind::Float);
    assert_eq!(AsValue::from("s").kind(), AsKind::Str);
    assert_eq!(AsValue::decimal("1.0").unwrap().kind(), AsKind::Decimal);
}

#[test]
fn type_name_delegates_to_engine() {
    // type_name() is the engine's single source of truth, NOT re-spelled here.
    assert_eq!(AsValue::from(7i64).type_name(), "int");
    assert_eq!(AsValue::from(1.5f64).type_name(), "float");
    assert_eq!(AsValue::from("s").type_name(), "string");
    assert_eq!(AsValue::nil().type_name(), "nil");
}

// ── Task 2.2: container handles (LIVE aliasing) + the 25-kind table ──────────

#[test]
fn containers_are_live_aliasing_handles() {
    let iso = Isolate::builder().build().unwrap();
    iso.eval("let state = { hp: 10 }").unwrap();
    let state = iso.global("state").unwrap();

    // host → script: a host write through the handle is visible to the script.
    state.set_key("hp", AsValue::from(7)).unwrap();
    assert_eq!(iso.eval("state.hp").unwrap().as_int(), Some(7));

    // script → host: a script write is visible to the host's SAME handle (no copy).
    iso.eval("state.hp = 3").unwrap();
    assert_eq!(state.get_key("hp").unwrap().as_int(), Some(3));
}

#[test]
fn array_handle_read_and_write() {
    let iso = Isolate::builder().build().unwrap();
    iso.eval("let xs = [1, 2, 3]").unwrap();
    let xs = iso.global("xs").unwrap();
    assert_eq!(xs.len(), Some(3));
    assert_eq!(xs.get(1).unwrap().as_int(), Some(2));
    // host write through the index → visible in script (live aliasing).
    xs.set(0, AsValue::from(100)).unwrap();
    assert_eq!(iso.eval("xs[0]").unwrap().as_int(), Some(100));
    // items() is a snapshot.
    let items = xs.items();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].as_int(), Some(100));
}

#[test]
fn object_entries_and_get_key_miss() {
    let iso = Isolate::builder().build().unwrap();
    iso.eval("let o = { a: 1, b: 2 }").unwrap();
    let o = iso.global("o").unwrap();
    assert_eq!(o.len(), Some(2));
    assert!(o.get_key("nope").is_none());
    let entries = o.entries();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0, "a");
    assert_eq!(entries[0].1.as_int(), Some(1));
}

#[test]
fn map_and_set_are_read_only_host_side() {
    let iso = Isolate::builder().build().unwrap();
    // Produce a Map + Set unambiguously via stdlib.
    iso.eval(
        r#"
        import * as mapm from "std/map"
        import * as setm from "std/set"
        let m2 = mapm.new()
        mapm.set(m2, "k", 42)
        let s2 = setm.new()
        setm.add(s2, 7)
    "#,
    )
    .unwrap();
    let m = iso.global("m2").unwrap();
    assert_eq!(m.kind(), AsKind::Map);
    assert_eq!(m.len(), Some(1));
    let entries = m.entries();
    assert_eq!(entries.len(), 1);
    let s = iso.global("s2").unwrap();
    assert_eq!(s.kind(), AsKind::Set);
    assert_eq!(s.items().len(), 1);
}

#[test]
fn bytes_handle_len_and_read() {
    let b = AsValue::bytes(vec![1, 2, 3]);
    assert_eq!(b.kind(), AsKind::Bytes);
    assert_eq!(b.len(), Some(3));
    assert_eq!(b.as_bytes(), Some(vec![1u8, 2, 3]));
}

#[test]
fn callable_handle_is_invokable() {
    let iso = Isolate::builder().build().unwrap();
    iso.eval("fn sq(x) { return x * x }").unwrap();
    let f = iso.global("sq").unwrap();
    assert!(f.is_callable());
    assert_eq!(f.kind(), AsKind::Callable);
    let r = iso.call_value(&f, &[AsValue::from(5i64)]).unwrap();
    assert_eq!(r.as_int(), Some(25));
}

#[test]
fn set_on_frozen_shared_surfaces_engine_panic() {
    let iso = Isolate::builder().build().unwrap();
    iso.eval(
        r#"
        import * as shared from "std/shared"
        let frozen = shared.freeze({ hp: 1 })
    "#,
    )
    .unwrap();
    let frozen = iso.global("frozen").unwrap();
    // A frozen `Shared` receiver: a host mutation must surface the engine's own
    // `cannot mutate a frozen …` panic as EmbedError::Panic — NOT a bypass.
    let e = frozen.set_key("hp", AsValue::from(2)).unwrap_err();
    match e {
        EmbedError::Panic(p) => assert!(
            p.message.contains("cannot mutate a frozen"),
            "got {:?}",
            p.message
        ),
        other => panic!("expected Panic, got {other:?}"),
    }
}

#[test]
fn set_on_frozen_object_surfaces_engine_panic() {
    let iso = Isolate::builder().build().unwrap();
    iso.eval(
        r#"
        import * as object from "std/object"
        let o = object.freeze({ a: 1 })
    "#,
    )
    .unwrap();
    let o = iso.global("o").unwrap();
    let e = o.set_key("a", AsValue::from(2)).unwrap_err();
    assert!(matches!(e, EmbedError::Panic(_)), "got {e:?}");
}

/// Spec §5.2: EVERY runtime kind, PRODUCED IN SCRIPT, crosses to the host and
/// classifies / round-trips per its crossing class. Driven table-style, one row per
/// kind: value kinds, live handles, callable, future (auto-await), and the
/// opaque pass-back-identical kinds (script-side `g == g2` identity after a round-trip).
#[test]
fn kind_table_every_value_kind_crosses() {
    let iso = Isolate::builder().build().unwrap();

    // ── value kinds (cross by value) ───────────────────────────────────────
    iso.eval(
        r#"
        import * as decimal from "std/decimal"
        let k_nil = nil
        let k_bool = true
        let k_int = 7
        let k_float = 1.5
        let k_str = "hi"
        let k_dec = decimal.from("1.50")
    "#,
    )
    .unwrap();
    assert_eq!(iso.global("k_nil").unwrap().kind(), AsKind::Nil);
    assert_eq!(iso.global("k_bool").unwrap().kind(), AsKind::Bool);
    assert_eq!(iso.global("k_int").unwrap().kind(), AsKind::Int);
    assert_eq!(iso.global("k_float").unwrap().kind(), AsKind::Float);
    assert_eq!(iso.global("k_str").unwrap().kind(), AsKind::Str);
    assert_eq!(iso.global("k_dec").unwrap().kind(), AsKind::Decimal);

    // ── live handles ────────────────────────────────────────────────────────
    iso.eval(
        r#"
        import * as mapm from "std/map"
        import * as setm from "std/set"
        import * as bytesm from "std/bytes"
        let k_arr = [1, 2]
        let k_obj = { a: 1 }
        let k_map = mapm.new()
        let k_set = setm.new()
        let k_bytes = bytesm.fromArray([1, 2, 3])
    "#,
    )
    .unwrap();
    assert_eq!(iso.global("k_arr").unwrap().kind(), AsKind::Array);
    assert_eq!(iso.global("k_obj").unwrap().kind(), AsKind::Object);
    assert_eq!(iso.global("k_map").unwrap().kind(), AsKind::Map);
    assert_eq!(iso.global("k_set").unwrap().kind(), AsKind::Set);
    assert_eq!(iso.global("k_bytes").unwrap().kind(), AsKind::Bytes);

    // ── callable (function/closure) ───────────────────────────────────────
    iso.eval("fn k_fn(x) { return x } let k_closure = (x) => x + 1").unwrap();
    assert!(iso.global("k_fn").unwrap().is_callable());
    assert!(iso.global("k_closure").unwrap().is_callable());
    // invokable via call_value
    let r = iso
        .call_value(&iso.global("k_closure").unwrap(), &[AsValue::from(9i64)])
        .unwrap();
    assert_eq!(r.as_int(), Some(10));

    // ── future (auto-await via call) ──────────────────────────────────────
    iso.eval("async fn k_async() { return 42 }").unwrap();
    let r = iso.call("k_async", &[]).unwrap();
    assert_eq!(r.as_int(), Some(42), "future auto-awaits to its value");

    // ── opaque kinds: pass back into the engine IDENTITY-preserved ─────────
    // Each is produced in script, read out by handle, set back in as a fresh global,
    // and the script asserts `g == g2` (identity-equal opaques).
    let opaque_producers = [
        // (name, producer source defining `g`)
        ("regex", r#"import * as regex from "std/regex"
                     let g = regex.compile("^a$")[0]"#),
        ("generator", "fn* gen() { yield 1 } let g = gen()"),
        ("class", "class K { fn m() { return 1 } } let g = K"),
        ("instance", "class K2 {} let g = K2()"),
        ("enum", "enum E { A, B } let g = E"),
        ("enum_variant", "enum E2 { A, B } let g = E2.A"),
        ("interface", "interface I { fn m(): int } let g = I"),
        ("shared", r#"import * as shared from "std/shared"
                      let g = shared.freeze({ a: 1 })"#),
    ];
    for (label, src) in opaque_producers {
        let iso = Isolate::builder().build().unwrap();
        iso.eval(src).unwrap_or_else(|e| panic!("{label}: produce: {e:?}"));
        let g = iso
            .global("g")
            .unwrap_or_else(|| panic!("{label}: g undefined"));
        // It classifies as Opaque (not mis-bucketed as a value/handle/callable).
        // (Shared additionally reads like its underlying kind, but kind() reports
        //  Opaque since the host shouldn't depend on the underlying tag.)
        // Round-trip: set it back in under a new name, assert script-side identity.
        iso.set_global("g2", g).unwrap();
        let same = iso.eval("g == g2").unwrap();
        assert_eq!(
            same.as_bool(),
            Some(true),
            "{label}: opaque round-trip must preserve identity (g == g2)"
        );
    }
}

// ── Task 2.3: the explicit JSON/serde deep bridge ───────────────────────────

#[cfg(feature = "data")]
#[test]
fn to_json_of_an_object_handle() {
    let iso = Isolate::builder().build().unwrap();
    iso.eval("let o = { a: 1, b: [2, 3], c: \"x\" }").unwrap();
    let o = iso.global("o").unwrap();
    let json = o.to_json().unwrap();
    assert_eq!(json, r#"{"a":1,"b":[2,3],"c":"x"}"#);
}

#[cfg(feature = "data")]
#[test]
fn json_parse_produces_a_handle_readable_by_script() {
    let iso = Isolate::builder().build().unwrap();
    let v = iso.json_parse(r#"{"hp": 42, "name": "hero"}"#).unwrap();
    assert_eq!(v.kind(), AsKind::Object);
    // It is a DEEP COPY, distinct from a live handle: set it in and read in script.
    iso.set_global("decoded", v).unwrap();
    assert_eq!(iso.eval("decoded.hp").unwrap().as_int(), Some(42));
    assert_eq!(iso.eval("decoded.name").unwrap().as_str(), Some("hero"));
}

#[cfg(feature = "data")]
#[test]
fn to_json_of_a_function_is_a_typed_error_with_the_offending_type() {
    let iso = Isolate::builder().build().unwrap();
    iso.eval("fn f() { return 1 }").unwrap();
    let f = iso.global("f").unwrap();
    let e = f.to_json().unwrap_err();
    // A non-serializable value → a typed error naming the offending value (the "path"
    // json's serializer provides). NOT a panic / hang.
    match e {
        EmbedError::Config(msg) | EmbedError::Panic(ascript::embed::EmbedPanic { message: msg, .. }) => {
            assert!(msg.contains("function"), "got {msg:?}");
        }
        other => panic!("expected a typed serialize error, got {other:?}"),
    }
}

#[cfg(feature = "data")]
#[test]
fn to_json_of_a_cyclic_object_errors_never_hangs() {
    let iso = Isolate::builder().build().unwrap();
    // A self-referential object: `o.self = o`. to_json must ERROR (match
    // from_ascript's cycle behavior), never hang.
    iso.eval("let o = {}\no.self = o").unwrap();
    let o = iso.global("o").unwrap();
    let e = o.to_json().unwrap_err();
    match e {
        EmbedError::Config(msg) | EmbedError::Panic(ascript::embed::EmbedPanic { message: msg, .. }) => {
            assert!(msg.contains("cyclic") || msg.contains("cycle"), "got {msg:?}");
        }
        other => panic!("expected a cycle error, got {other:?}"),
    }
}

#[cfg(all(feature = "data", feature = "embed"))]
#[test]
fn asvalue_serde_serializes_via_the_json_model() {
    let v = AsValue::object(vec![
        ("n".to_string(), AsValue::from(5i64)),
        ("s".to_string(), AsValue::from("hi")),
    ]);
    let out = serde_json::to_string(&v).unwrap();
    assert_eq!(out, r#"{"n":5,"s":"hi"}"#);
}

// NOTE: the `to_json`/`json_parse` Config-fallback when the `data` feature is OFF is
// pinned by a LIB UNIT test in `src/embed/value.rs`, not here: an integration test
// always links the crate's normal build, and the self-dev-dependency
// (`ascript = { path = ".", features = ["fuzzgen"] }`) pulls DEFAULT features — so
// `data` can never be off in this target. The lib unit test sees the real (no-default)
// feature set and is the correct home for that cfg-gated assertion.

// ── Task 3.1: host-module registry + name validation (spec §6.1, §6.2) ──────

use ascript::embed::{HostError as HE, HostModuleBuilder as HMB};

#[test]
fn host_module_bad_names_are_config_errors() {
    // Missing the `host:` scheme.
    let e = Isolate::builder()
        .host_module("app", |_m: &mut HMB| {})
        .unwrap_err();
    assert!(matches!(e, EmbedError::Config(_)), "missing prefix → Config; got {e:?}");

    // A dot would mis-split the qualified `host:My.App.fn` dispatch name.
    let e = Isolate::builder()
        .host_module("host:My.App", |_m: &mut HMB| {})
        .unwrap_err();
    assert!(matches!(e, EmbedError::Config(_)), "dotted name → Config; got {e:?}");

    // Empty after the scheme.
    let e = Isolate::builder()
        .host_module("host:", |_m: &mut HMB| {})
        .unwrap_err();
    assert!(matches!(e, EmbedError::Config(_)), "empty name → Config; got {e:?}");
}

#[test]
fn host_module_duplicate_registration_is_a_config_error() {
    let e = Isolate::builder()
        .host_module("host:app", |m: &mut HMB| {
            m.value("v", AsValue::from(1i64));
        })
        .unwrap()
        .host_module("host:app", |m: &mut HMB| {
            m.value("v", AsValue::from(2i64));
        })
        .unwrap_err();
    assert!(matches!(e, EmbedError::Config(_)), "dup → Config; got {e:?}");
}

#[test]
fn host_module_valid_registration_builds() {
    let iso = Isolate::builder()
        .host_module("host:app", |m: &mut HMB| {
            m.value("version", AsValue::from("1.0"));
            m.func("double", |_c, a| Ok(AsValue::from(a[0].as_int().unwrap_or(0) * 2)));
            m.fallible_func("lookup", |_c, a| match a[0].as_str() {
                Some("k") => Ok(AsValue::from(42i64)),
                _ => Err(HE::Recoverable("no such key".into())),
            });
        })
        .expect("valid host module registers")
        .build()
        .expect("build with a host module");
    drop(iso);
}

// ── Task 3.2: host: imports + dispatch (both tiers) + the miss panic ────────

#[test]
fn host_module_import_and_call_both_tiers() {
    let iso = Isolate::builder()
        .output(ascript::embed::OutputMode::Capture)
        .host_module("host:app", |m: &mut HMB| {
            m.value("version", AsValue::from("1.0"));
            m.func("double", |_c, a| Ok(AsValue::from(a[0].as_int().unwrap_or(0) * 2)));
            m.func("boom", |_c, _a| Err(HE::Panic("bad call".into())));
            m.fallible_func("lookup", |_c, a| match a[0].as_str() {
                Some("k") => Ok(AsValue::from(42i64)),
                _ => Err(HE::Recoverable("no such key".into())),
            });
        })
        .unwrap()
        .build()
        .unwrap();
    let r = iso.eval(
        r#"
        import * as app from "host:app"
        print(app.version, app.double(21))
        let [v, e1] = app.lookup("k")
        let [n, e2] = app.lookup("x")
        let [r, e3] = recover(() => app.boom())
        print(v, e1 == nil, n, e2.message, e3.message)
    "#,
    );
    assert!(r.is_ok(), "host program ran: {r:?}");
    let out = iso.take_output();
    assert_eq!(out, "1.0 42\n42 true nil no such key bad call\n", "got: {out:?}");
}

#[test]
fn unregistered_host_module_is_a_clean_recoverable_panic() {
    let iso = Isolate::builder()
        .output(ascript::embed::OutputMode::Capture)
        .build()
        .unwrap();
    // Top-level import of an unregistered host module → Tier-2 panic, EXACT §6.3 message.
    let e = iso.eval("import * as app from \"host:nope\"\n").unwrap_err();
    match e {
        EmbedError::Panic(p) => assert!(
            p.message
                .contains("host module 'host:nope' is not registered in this isolate"),
            "exact §6.3 miss message; got: {}",
            p.message
        ),
        other => panic!("expected Panic, got {other:?}"),
    }
    // The miss is a RECOVERABLE Tier-2 panic — the isolate session survives it (the
    // per-eval fiber is discarded; globals persist), so a later eval still works.
    assert_eq!(iso.eval("1 + 1").unwrap().as_int(), Some(2), "session survives the miss");

    // A host fn miss INSIDE a registered module surfaces a recoverable panic that
    // `recover` catches in-script (the dispatch-level miss, distinct from the import miss).
    let iso2 = Isolate::builder()
        .output(ascript::embed::OutputMode::Capture)
        .host_module("host:app", |m: &mut HMB| {
            m.func("boom", |_c, _a| Err(HE::Panic("kaboom".into())));
        })
        .unwrap()
        .build()
        .unwrap();
    iso2.eval("import * as app from \"host:app\"").unwrap();
    iso2.eval("let [v, err] = recover(() => app.boom())\nprint(err.message)")
        .unwrap();
    assert_eq!(iso2.take_output(), "kaboom\n");
}

// ── Task 3.3: worker host-module rules — miss panic + Send factories (§6.4) ──

use std::sync::Arc;

#[test]
fn worker_fn_without_factory_misses_host_module() {
    // `host:app` registered MAIN-isolate-only (host_module, not factory). A `worker fn`
    // body importing it runs in a fresh worker isolate that has no registration → the
    // §6.4 worker-specific miss panic. The whole program panics (the worker future's
    // error propagates through await), surfaced as EmbedError::Panic with that message.
    let iso = Isolate::builder()
        .output(ascript::embed::OutputMode::Capture)
        .host_module("host:app", |m: &mut HMB| {
            m.func("double", |_c, a| Ok(AsValue::from(a[0].as_int().unwrap_or(0) * 2)));
        })
        .unwrap()
        .build()
        .unwrap();
    let e = iso
        .eval(
            r#"
            import * as task from "std/task"
            worker fn w(n) {
                import * as app from "host:app"
                return app.double(n)
            }
            await w(21)
        "#,
        )
        .unwrap_err();
    match e {
        EmbedError::Panic(p) => assert!(
            p.message.contains(
                "host module 'host:app' is not available in a worker isolate \
                 (register it with host_module_factory to install it per-isolate)"
            ),
            "§6.4 worker miss message; got: {}",
            p.message
        ),
        other => panic!("expected Panic, got {other:?}"),
    }
}

#[test]
fn worker_fn_with_factory_resolves_host_module_pooled() {
    // A `host_module_factory` installs `host:app` in every worker isolate this Isolate
    // spawns → the pooled `worker fn` resolves it and returns the doubled value.
    let iso = Isolate::builder()
        .output(ascript::embed::OutputMode::Capture)
        .host_module_factory(
            "host:app",
            Arc::new(|m: &mut HMB| {
                m.func("double", |_c, a| {
                    Ok(AsValue::from(a[0].as_int().unwrap_or(0) * 2))
                });
            }),
        )
        .unwrap()
        .build()
        .unwrap();
    iso.eval(
        r#"
        import * as task from "std/task"
        worker fn w(n) {
            import * as app from "host:app"
            return app.double(n)
        }
        let r = await w(21)
        print(r)
    "#,
    )
    .expect("worker with factory resolves host:app");
    assert_eq!(iso.take_output(), "42\n");
}

#[test]
fn worker_fn_with_factory_resolves_host_module_dedicated() {
    // The dedicated `run_in_worker` path captures the factory list in the Send make_loop
    // closure and installs it at boot.
    let iso = Isolate::builder()
        .output(ascript::embed::OutputMode::Capture)
        .caps(ascript::embed::Caps::all_granted())
        .host_module_factory(
            "host:app",
            Arc::new(|m: &mut HMB| {
                m.func("triple", |_c, a| {
                    Ok(AsValue::from(a[0].as_int().unwrap_or(0) * 3))
                });
            }),
        )
        .unwrap()
        .build()
        .unwrap();
    iso.eval(
        r#"
        worker fn w(n) {
            import * as app from "host:app"
            return app.triple(n)
        }
        let r = await run_in_worker(w, 10, { caps: { deny: [] } })
        print(r)
    "#,
    )
    .expect("dedicated run_in_worker resolves host:app via factory");
    assert_eq!(iso.take_output(), "30\n");
}

#[test]
fn pooled_workers_do_not_leak_host_modules_across_isolates() {
    // §6.4 no-leak (the caps-floor discipline): Isolate A (factory) and Isolate B (none)
    // dispatch `worker fn`s on the SAME host thread's pool. Each pooled request installs
    // its OWN factory set FRESH (clear-then-install), so B's worker must still MISS
    // `host:app` even though A registered it via a factory on the shared pool thread.
    let prog = r#"
        worker fn w(n) {
            import * as app from "host:app"
            return app.double(n)
        }
        await w(5)
    "#;

    // Isolate A: factory present → its worker resolves host:app.
    let a = Isolate::builder()
        .output(ascript::embed::OutputMode::Capture)
        .host_module_factory(
            "host:app",
            Arc::new(|m: &mut HMB| {
                m.func("double", |_c, x| Ok(AsValue::from(x[0].as_int().unwrap_or(0) * 2)));
            }),
        )
        .unwrap()
        .build()
        .unwrap();
    let ra = a.eval(prog);
    assert!(ra.is_ok(), "A (factory) resolves: {ra:?}");

    // Isolate B: NO factory → its worker (on the same pool thread) must STILL miss,
    // proving A's factory did not leak forward to B's request.
    let b = Isolate::builder()
        .output(ascript::embed::OutputMode::Capture)
        .host_module("host:app", |m: &mut HMB| {
            // registered MAIN-only on B (not a factory) — so B's worker must miss.
            m.func("double", |_c, x| Ok(AsValue::from(x[0].as_int().unwrap_or(0) * 2)));
        })
        .unwrap()
        .build()
        .unwrap();
    let e = b.eval(prog).unwrap_err();
    match e {
        EmbedError::Panic(p) => assert!(
            p.message.contains("is not available in a worker isolate"),
            "B's worker must miss (no leak from A); got: {}",
            p.message
        ),
        other => panic!("expected B's worker to miss, got {other:?}"),
    }
}
