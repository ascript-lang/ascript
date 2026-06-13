//! CALL — kill-switch + coverage scaffolding (CALL §8.1 / plan Task 1.1).
//!
//! The `no_call_fast` mode is the fifth differential axis: `specialize=true,
//! sync_lane=true, call_fast=false`. With the kill switch off, every CALL fast
//! path (A2/A3/B) is suppressed; the output must be byte-identical to all other
//! modes. Phase 1 is INERT — the counters stay 0 and no dispatch-loop line is
//! changed — so this file only checks structural correctness and the new entry
//! point. Phases 2/3 will add coverage assertions (`inplace_binds > 0`, etc.)
//! and fuller edge-case batteries as each fast path lands.

/// Run `src` on all five engine modes and assert byte-identical outcomes:
/// tree-walker, specialized-VM, generic-VM, lane-off, AND no-call-fast.
///
/// Error outcomes (panics) compare by message, not span, so a panic that only
/// the no-call-fast mode produces is caught.
async fn assert_five_mode_identical(src: &str) {
    use ascript::error::AsError;

    fn norm(r: Result<(String, Option<i32>), AsError>) -> Result<(String, Option<i32>), String> {
        match r {
            Ok(pair) => Ok(pair),
            Err(e) => Err(e.message),
        }
    }

    let tw = norm(ascript::run_source_exit(src).await);
    let spec = norm(ascript::vm_run_source(src).await);
    let gen = norm(ascript::vm_run_source_generic(src).await);
    let nolane = norm(ascript::vm_run_source_no_sync_lane(src).await);
    let nocf = norm(ascript::vm_run_source_no_call_fast(src).await);

    assert_eq!(
        spec, gen,
        "generic-VM diverged from specialized-VM for `{src}`\n  spec: {spec:?}\n  gen:  {gen:?}"
    );
    assert_eq!(
        tw, spec,
        "specialized-VM diverged from tree-walker for `{src}`\n  tw:   {tw:?}\n  spec: {spec:?}"
    );
    assert_eq!(
        tw, nolane,
        "lane-off VM diverged from tree-walker for `{src}`\n  tw:     {tw:?}\n  nolane: {nolane:?}"
    );
    assert_eq!(
        tw, nocf,
        "no-call-fast VM diverged from tree-walker for `{src}`\n  tw:   {tw:?}\n  nocf: {nocf:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
//  The failing-test-first case: vm_run_source_no_call_fast must exist and
//  produce byte-identical output to the specialized VM.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn no_call_fast_mode_runs_byte_identically() {
    let src = r#"
        fn add(a, b) { return a + b }
        let xs = [1, 2, 3]
        let total = 0
        for (x in xs) {
            total = total + add(x, 10)
        }
        print(total)
    "#;
    let spec = ascript::vm_run_source(src).await.expect("spec vm");
    let ncf = ascript::vm_run_source_no_call_fast(src).await.expect("no-call-fast vm");
    assert_eq!(spec, ncf);
}

// ─────────────────────────────────────────────────────────────────────────────
//  Corpus of call-heavy programs: all five modes must agree.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn no_call_fast_mode_basic_function_calls() {
    let cases = [
        // simple call
        r#"fn f(a, b) { return a + b } print(f(1, 2))"#,
        // nested call
        r#"fn double(x) { return x * 2 } fn quad(x) { return double(double(x)) } print(quad(3))"#,
        // default params
        r#"fn greet(name, greeting = "hello") { return greeting + " " + name } print(greet("world"))"#,
        // rest params
        r#"fn sum(...args) { let s = 0; for (x in args) { s = s + x }; return s } print(sum(1, 2, 3, 4))"#,
        // closures
        r#"fn make_adder(n) { return (x) => x + n } let add5 = make_adder(5) print(add5(3))"#,
        // array map-like iteration
        r#"
            import * as array from "std/array"
            fn add(a, b) { return a + b }
            let xs = [1, 2, 3]
            let mapped = array.map(xs, (x) => add(x, 10))
            print(mapped)
        "#,
        // higher-order
        r#"
            import * as array from "std/array"
            let xs = [1, 2, 3, 4, 5]
            let total = array.reduce(xs, (acc, x) => acc + x, 0)
            print(total)
        "#,
        // many calls in a loop
        r#"fn inc(x) { return x + 1 } let n = 0 for (i in 0..100) { n = inc(n) } print(n)"#,
    ];
    for src in cases {
        assert_five_mode_identical(src).await;
    }
}

#[tokio::test]
async fn no_call_fast_mode_error_paths_identical() {
    // Error outcomes must also be identical (same panic message + exit code).
    let cases = [
        // arity errors
        r#"fn f(a, b) {} f(1)"#,
        r#"fn f(a, b) {} f(1, 2, 3)"#,
        // type contract
        r#"fn f(a: number) {} f("x")"#,
    ];
    for src in cases {
        assert_five_mode_identical(src).await;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Task 2.2 (A2): panic-wording parity battery
//  (CALL §3.4 — the in-place path must be byte-identical across all five modes)
// ─────────────────────────────────────────────────────────────────────────────

/// Arity/contract panic wording is byte-identical across all five modes for
/// every arg shape the in-place path touches (CALL §3.4). Covers the 7 cases
/// enumerated in the spec: too-few, too-many, default referencing earlier param,
/// too-few even with defaults, contract on 2nd arg, rest-fallback with contract
/// violation, and rest-fallback success.
#[tokio::test]
async fn arg_panic_wording_parity() {
    let cases = [
        // 1. exact arity, too few
        r#"fn f(a, b) {} f(1)"#,
        // 2. exact arity, too many
        r#"fn f(a, b) {} f(1, 2, 3)"#,
        // 3. default referencing earlier param (succeeds, prints 6)
        r#"fn f(a, b = a + 1) { print(b) } f(5)"#,
        // 4. too few even with default (a is required → "at least 1")
        r#"fn f(a, b = 1) {} f()"#,
        // 5. contract on 2nd arg (left-to-right order)
        r#"fn f(a: number, b: string) {} f(1, 2)"#,
        // 6. rest param, falls back — contract violation on rest element
        r#"fn f(a, ...rest: array<number>) { print(rest) } f(1, 2, "x")"#,
        // 7. rest param, falls back — succeeds
        r#"fn f(...rest) { print(rest) } f(1, 2, 3)"#,
    ];
    for src in cases {
        assert_five_mode_identical(src).await;
    }
}

/// A2 anti-false-green: `inplace_binds` counter must be > 0 after running
/// call-heavy code on a specialized VM with `call_fast=true`. This proves
/// the fast path actually fires, not just that the output is coincidentally
/// identical (CALL §8.3).
#[tokio::test]
async fn inplace_binds_counter_is_nonzero() {
    let src =
        r#"fn f(a, b) { return a + b } let s = 0 for (i in 0..100) { s = f(s, 1) } print(s)"#;
    let (_out, _exit, stats) = ascript::vm_run_source_call_fast_stats(src)
        .await
        .expect("vm_run_source_call_fast_stats failed");
    assert!(
        stats.inplace_binds > 0,
        "A2: inplace_binds should be > 0 after 100 qualifying calls, got {}",
        stats.inplace_binds
    );
}
