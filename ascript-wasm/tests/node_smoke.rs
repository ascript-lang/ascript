//! WASM §6 — the RunResult-contract smoke table as `#[wasm_bindgen_test]`, run under
//! Node via `wasm-pack test --node`. This is the load-bearing Phase-2 gate: it proves
//! `run_program` honors §5.4 (deny-all caps, no-ANSI errors, platform refusals) and the
//! §6 contract table (output/ok/exitCode per program shape) on the REAL wasm artifact.

use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

// No `wasm_bindgen_test_configure!` — `wasm-pack test --node` runs in Node by default.

/// The deserialized mirror of the wrapper's private `RunResult` (camelCase from serde).
#[derive(serde::Deserialize, Debug)]
struct RunResult {
    ok: bool,
    output: String,
    error: Option<String>,
    diagnostics: Vec<String>,
    #[serde(rename = "exitCode")]
    exit_code: Option<i32>,
    #[serde(rename = "durationMs")]
    #[allow(dead_code)]
    duration_ms: f64,
}

/// Run a program through the wasm entry and decode the `JsValue` back into a `RunResult`.
async fn run(src: &str) -> RunResult {
    let v: JsValue = ascript_wasm::run_program(src.to_string()).await;
    serde_wasm_bindgen::from_value(v).expect("RunResult decodes")
}

/// No ANSI escape (ESC = U+001B) may appear in a JS-facing string (§5.4).
fn no_ansi(s: &str) {
    assert!(!s.contains('\u{1b}'), "string carries an ANSI escape: {s:?}");
}

#[wasm_bindgen_test]
async fn hello() {
    let r = run(r#"print("hello")"#).await;
    assert!(r.ok, "hello should run ok; got {r:?}");
    assert_eq!(r.output, "hello\n");
    assert!(r.error.is_none());
    assert!(r.diagnostics.is_empty());
    assert!(r.exit_code.is_none());
}

#[wasm_bindgen_test]
async fn async_gather_await() {
    // async fn + await + task.gather — the async engine end-to-end on wasm.
    let src = r#"
        import * as task from "std/task"
        async fn double(n: int): int { return n * 2 }
        let results = await task.gather([double(1), double(2), double(3)])
        for (r in results) { print(r) }
    "#;
    let r = run(src).await;
    assert!(r.ok, "async program should run ok; got {r:?}");
    // Pinned against the native VM (same engine): gather preserves input order.
    assert_eq!(r.output, "2\n4\n6\n");
}

#[wasm_bindgen_test]
async fn tier1_error_no_ansi() {
    // A Tier-2 panic (type error): not callable. ok:false, error non-null, NO ANSI.
    let r = run(r#"let x = 5; x()"#).await;
    assert!(!r.ok, "calling a non-function should fail; got {r:?}");
    let err = r.error.expect("error is non-null on failure");
    no_ansi(&err);
    assert!(!err.is_empty());
}

#[wasm_bindgen_test]
async fn compile_error_no_ansi() {
    // A syntax error → compile failure: diagnostics non-empty, no ANSI.
    let r = run(r#"let x = "#).await;
    assert!(!r.ok, "syntax error should fail; got {r:?}");
    assert!(!r.diagnostics.is_empty(), "compile error yields diagnostics");
    for d in &r.diagnostics {
        no_ansi(d);
    }
    if let Some(e) = &r.error {
        no_ansi(e);
    }
}

#[wasm_bindgen_test]
async fn cap_denied() {
    // §5.4 deny-ALL: the playground applies an all-five-denied CapSet at Interp construction.
    // We assert that directly via `std/caps` (a CORE module, present in the wasm feature set —
    // unlike fs/env/process/net/ffi, which aren't even COMPILED IN, an even stronger guarantee):
    // `caps.has(c)` reflects the live CapSet, so under deny-all every dangerous cap reads false.
    // This is the genuine deny-all proof — break `deny_all_dangerous()` in the wrapper and
    // `caps.has("env")` flips to true, failing this test (the anti-false-green property).
    let src = r#"
        import * as caps from "std/caps"
        for (c in ["fs", "net", "process", "ffi", "env"]) {
            print(`${c}=${caps.has(c)}`)
        }
    "#;
    let r = run(src).await;
    assert!(r.ok, "caps introspection should run; got {r:?}");
    assert_eq!(
        r.output, "fs=false\nnet=false\nprocess=false\nffi=false\nenv=false\n",
        "deny-all must clear every dangerous capability; got {:?}",
        r.output
    );
}

#[wasm_bindgen_test]
async fn worker_unavailable() {
    // §5.3.7: a `worker fn` call refuses with the platform error on wasm.
    let src = r#"
        worker fn sq(n: int): int { return n * n }
        let r = await sq(7)
        print(r)
    "#;
    let r = run(src).await;
    assert!(!r.ok, "workers are unavailable on wasm; got {r:?}");
    let err = r.error.expect("worker call surfaces an error");
    no_ansi(&err);
    assert!(
        err.contains("workers are not available on this platform (wasm)"),
        "expected the worker platform error, got: {err}"
    );
}

#[wasm_bindgen_test]
async fn timer_unavailable() {
    // §5.3.4: interval/timer RESOURCES refuse on wasm (v1 non-goal).
    let r = run(r#"import { interval } from "std/time"; let t = interval(100)"#).await;
    assert!(!r.ok, "time.interval is unavailable on wasm; got {r:?}");
    let err = r.error.expect("interval surfaces an error");
    no_ansi(&err);
    assert!(
        err.contains("time.interval is not available on this platform (wasm)"),
        "expected the timer platform error, got: {err}"
    );
}

#[wasm_bindgen_test]
async fn deep_recursion_clean_panic() {
    // §5.3.5: unbounded recursion hits the engine depth guard with a clean Tier-2 panic
    // ("maximum recursion depth exceeded"), NOT a wasm shadow-stack trap (which would
    // surface as an opaque `RuntimeError: unreachable` and fail to decode).
    let src = r#"
        fn rec(n: int): int { return rec(n + 1) }
        print(rec(0))
    "#;
    let r = run(src).await;
    assert!(!r.ok, "unbounded recursion should panic; got {r:?}");
    let err = r.error.expect("recursion surfaces an error");
    no_ansi(&err);
    assert!(
        err.contains("maximum recursion depth exceeded"),
        "expected the recursion-depth guard, got a different error (a wasm trap?): {err}"
    );
}

#[wasm_bindgen_test]
async fn exit_code() {
    let r = run(r#"print("before"); exit(3)"#).await;
    assert!(r.ok, "exit(n) is a clean termination; got {r:?}");
    assert_eq!(r.exit_code, Some(3));
}

#[wasm_bindgen_test]
async fn gc_cycle_smoke() {
    // Build + drop a cyclic object graph — the cycle-collecting GC must reclaim it on
    // wasm without a panic (the Phase-0 GC smoke, kept permanently).
    let src = r#"
        fn build() {
            let a = { next: nil }
            let b = { next: a }
            a.next = b
        }
        for (i in 0..100) { build() }
        print("ok")
    "#;
    let r = run(src).await;
    assert!(r.ok, "gc cycle smoke should run ok; got {r:?}");
    assert_eq!(r.output, "ok\n");
}

#[wasm_bindgen_test]
async fn gc_cycle_then_second_program() {
    // Cross-isolate regression (WASM §5.3): a cycle-building program followed by a SECOND
    // program in the SAME long-lived wasm instance must NOT corrupt the shared gcmodule
    // object space. `wasm_run_source` skips the per-isolate `collect_thread_cycles()` on
    // wasm precisely because repeating it across isolates reads a dangling box → a hard
    // `RuntimeError: memory access out of bounds`. This test is the guard for that decision.
    let first = run(r#"fn b() { let a = { n: nil }; let c = { n: a }; a.n = c }
        for (i in 0..50) { b() }
        print("first")"#)
    .await;
    assert!(first.ok, "cycle-building first program: {first:?}");
    assert_eq!(first.output, "first\n");
    let second = run(r#"print("second")"#).await;
    assert!(
        second.ok,
        "a second program after a cycle program must not OOB the shared GC space: {second:?}"
    );
    assert_eq!(second.output, "second\n");
}

#[wasm_bindgen_test]
async fn sleep_completes() {
    // §5.3.3: the `time.sleep` shim (JS setTimeout future on wasm) drives to completion.
    let r = run(r#"import { sleep } from "std/time"; await sleep(20); print("woke")"#).await;
    assert!(r.ok, "time.sleep should complete on wasm; got {r:?}");
    assert_eq!(r.output, "woke\n");
}

#[wasm_bindgen_test]
async fn recursion_near_limit_completes() {
    // §5.3.5 recursion calibration: a run just BELOW the wasm MAX_CALL_DEPTH (1000 − 10)
    // must COMPLETE — proving the shadow stack (raised via -zstack-size) holds the budget
    // the depth guard advertises. The recursion threads a counter so the depth is real.
    let src = r#"
        fn rec(n: int, depth: int): int {
            if (n == 0) { return depth }
            return rec(n - 1, depth + 1)
        }
        print(rec(990, 0))
    "#;
    let r = run(src).await;
    assert!(
        r.ok,
        "recursion at depth 990 (< MAX_CALL_DEPTH 1000) must complete, not trap; got {r:?}"
    );
    assert_eq!(r.output, "990\n");
}

