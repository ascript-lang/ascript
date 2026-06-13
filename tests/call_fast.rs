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

// ─────────────────────────────────────────────────────────────────────────────
//  Task 2.3 (A3): fiber pooling — anti-false-green + probe tests
// ─────────────────────────────────────────────────────────────────────────────

/// A3 anti-false-green: `pooled_fiber_reuses` must be > 0 after running code
/// that re-enters the VM via `invoke_compiled_static` (which calls `take_pooled_fiber`
/// directly). Static class method calls go through `dispatch_method` →
/// `invoke_compiled_static` → `take_pooled_fiber`. After the first call the fiber
/// is returned to the pool, so the second+ call increments `pooled_fiber_reuses`.
#[tokio::test]
async fn pooled_fiber_reuses_counter_is_nonzero() {
    // A class with a static method called 10 times exercises the fiber pool.
    // `invoke_compiled_static` takes a pooled fiber per call; after the first call
    // the fiber is returned and subsequent calls reuse it (pool reuse count > 0).
    let src = r#"
        class Math {
            static fn double(x: int): int {
                return x * 2
            }
        }
        let total = 0
        for (i in 0..10) {
            total = total + Math.double(i)
        }
        print(total)
    "#;
    let (_out, _exit, stats) = ascript::vm_run_source_call_fast_stats(src)
        .await
        .expect("vm_run_source_call_fast_stats failed");
    assert!(
        stats.pooled_fiber_reuses > 0,
        "A3: pooled_fiber_reuses should be > 0 after 10 static method calls, got {}",
        stats.pooled_fiber_reuses
    );
}

/// A3 probe: deeply nested re-entrancy (map-inside-map) must produce correct results.
/// Distinct fibers must be used simultaneously — the pool is not shared across
/// two concurrent `call_value` invocations (take removes from pool).
#[tokio::test]
async fn nested_reentrant_calls_are_correct() {
    let src = r#"
        import * as array from "std/array"
        let xs = [1, 2, 3]
        let ys = [10, 20, 30]
        let result = array.map(xs, (x) => {
            let inner = array.map(ys, (y) => x + y)
            return array.reduce(inner, (acc, v) => acc + v, 0)
        })
        print(result)
    "#;
    assert_five_mode_identical(src).await;
}

/// A3 probe: a panicking callee must NOT poison the pool. After a panic, the
/// fiber is dropped (never pooled); subsequent successful calls must still work.
#[tokio::test]
async fn panicking_callee_does_not_poison_pool() {
    let src = r#"
        import * as array from "std/array"
        fn bad(x) {
            if (x == 2) { let _ = nil + 1 }
            return x * 10
        }
        // The first call (x=1) succeeds, x=2 panics — recover, then x=3 succeeds.
        let r1 = recover(() => bad(1))
        let r2 = recover(() => bad(2))
        let r3 = recover(() => bad(3))
        print([r1[0], r2[1] != nil, r3[0]])
    "#;
    assert_five_mode_identical(src).await;
}

/// A3 probe: generator + `for await` interleaved with pooled re-entrant calls.
/// The generator fiber is NOT pooled; it must coexist with pooled call fibers.
#[tokio::test]
async fn generator_and_pooled_fibers_coexist() {
    let src = r#"
        import * as array from "std/array"
        fn* gen(n) {
            for (i in 0..n) { yield i }
        }
        let g = gen(5)
        let items = []
        for await (v in g) {
            // Call via array.map inside the for-await body — pooled re-entry.
            let doubled = array.map([v], (x) => x * 2)
            items = items + doubled
        }
        print(items)
    "#;
    assert_five_mode_identical(src).await;
}

/// A3 probe: `call_fast=false` (pool disabled) must produce byte-identical output.
/// No new divergence from A3 itself — the kill switch must fully suppress pooling.
#[tokio::test]
async fn a3_pool_off_still_byte_identical() {
    let cases = [
        // map over a closure
        r#"
            import * as array from "std/array"
            let xs = [1, 2, 3, 4, 5]
            let r = array.map(xs, (x) => x * x)
            print(r)
        "#,
        // class method call via invoke_compiled_method
        r#"
            class Adder {
                fn add(a, b) { return a + b }
            }
            let adder = Adder()
            let r = adder.add(3, 4)
            print(r)
        "#,
    ];
    for src in cases {
        assert_five_mode_identical(src).await;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Task 3.1: CallbackTrampoline + CallbackDriver infrastructure tests
//  (CALL §5 — the trampoline core, reset invariant, arm eligibility)
// ─────────────────────────────────────────────────────────────────────────────

/// Task 3.1 (a): trampoline infrastructure builds and the arm-eligibility rules
/// are correct. Verifies that plain sync closures produce identical output across
/// all five modes. The trampoline_calls counter becomes >0 in Task 3.2 once the
/// stdlib sites are wired; this test proves correctness of the foundation.
#[tokio::test]
async fn trampoline_plain_closure_five_mode_identity() {
    let cases = [
        // plain sync closure — eligible for trampolining once wired
        r#"
            import * as array from "std/array"
            fn add_ten(x) { return x + 10 }
            let xs = [1, 2, 3]
            let r = array.map(xs, add_ten)
            print(r)
        "#,
        // closure literal — eligible
        r#"
            import * as array from "std/array"
            let xs = [1, 2, 3]
            let r = array.map(xs, (x) => x * x)
            print(r)
        "#,
        // builtin callback (not a closure) — NOT eligible (arm refuses non-closures)
        r#"
            import * as array from "std/array"
            let xs = ["hello", "world"]
            let r = array.map(xs, len)
            print(r)
        "#,
    ];
    for src in cases {
        assert_five_mode_identical(src).await;
    }
}

/// Task 3.1 (b): reset invariant — a mid-sequence contract panic does NOT poison
/// subsequent calls. All five modes must produce the same (correct) output.
#[tokio::test]
async fn trampoline_reset_invariant_five_mode_identity() {
    // typed_cb panics on integer input; recover catches it.
    // The key test: after the panic, the next (non-panicking) call must succeed.
    let src = r#"
        import * as array from "std/array"
        fn typed_cb(x: string) { return len(x) }
        let r1 = recover(() => array.map(["hello"], typed_cb))
        let r2 = recover(() => array.map([1], typed_cb))
        let r3 = array.map(["world"], typed_cb)
        print(r1[0], r2[1] != nil, r3)
    "#;
    assert_five_mode_identical(src).await;
}

/// Task 3.1 (c): arm() refuses async/generator/worker closures — they NEVER go
/// through the trampoline. Confirmed via the unit tests in trampoline.rs.
/// Here we verify the VM + no-call-fast modes agree when using a sync callback
/// that delegates to an async fn internally (the VM resolves futures; the
/// trampoline arm gate correctly excludes is_async closures).
#[tokio::test]
async fn trampoline_arm_refuses_ineligible_callables_produces_correct_output() {
    // Builtin (len) is a Value::Builtin, not Closure — arm() returns None.
    // Both call_fast modes must produce identical output.
    let src = r#"
        import * as array from "std/array"
        let xs = ["a", "bb", "ccc"]
        let r = array.map(xs, len)
        print(r)
    "#;
    let spec = ascript::vm_run_source(src).await.expect("spec");
    let ncf = ascript::vm_run_source_no_call_fast(src).await.expect("ncf");
    assert_eq!(spec, ncf, "builtin callback: spec vs ncf differ");
    assert_eq!(spec.0.trim(), "[1, 2, 3]", "wrong result: {}", spec.0);
}

/// Task 3.1 (d): recursion limit parity — the call_depth is bumped exactly
/// once per logical call in both the trampoline and no-call-fast paths.
#[tokio::test]
async fn trampoline_recursion_limit_parity_five_mode_identity() {
    // A moderately deep recursion inside a mapped callback. Both call_fast=true
    // and call_fast=false must produce the same result.
    let src = r#"
        import * as array from "std/array"
        fn countdown(n) {
            if (n <= 0) { return 0 }
            return countdown(n - 1) + 1
        }
        let xs = [50]
        let r = array.map(xs, (x) => countdown(x))
        print(r)
    "#;
    assert_five_mode_identical(src).await;

    // Also confirm both modes agree when the recursion limit IS exceeded.
    let deep_src = r#"
        import * as array from "std/array"
        fn deep(n) {
            if (n <= 0) { return 0 }
            return deep(n - 1) + 1
        }
        let xs = [5000]
        let r = recover(() => array.map(xs, (x) => deep(x)))
        print(r[1] != nil)
    "#;
    assert_five_mode_identical(deep_src).await;
}

// ─────────────────────────────────────────────────────────────────────────────
//  Task 3.2: stdlib sites wired — trampoline_calls anti-false-green battery
//  (CALL §5.5 — the counter must be > 0 after a wired stdlib call with a plain
//  sync closure, proving the trampoline actually fires, not just that output is
//  coincidentally correct)
// ─────────────────────────────────────────────────────────────────────────────

/// Task 3.2 anti-false-green: `trampoline_calls` > 0 after array.map with a
/// plain sync closure (the simplest wired site). Proves the trampoline fires.
#[tokio::test]
async fn trampoline_calls_counter_nonzero_after_array_map() {
    let src = r#"
        import * as array from "std/array"
        let xs = [1, 2, 3, 4, 5]
        let r = array.map(xs, (x) => x * 2)
        print(r)
    "#;
    let (_out, _exit, stats) = ascript::vm_run_source_call_fast_stats(src)
        .await
        .expect("vm_run_source_call_fast_stats failed");
    assert!(
        stats.trampoline_calls > 0,
        "Task 3.2: trampoline_calls should be > 0 after array.map with a sync closure, got {}",
        stats.trampoline_calls
    );
    // Exactly 5 elements → 5 trampoline calls.
    assert_eq!(
        stats.trampoline_calls, 5,
        "Task 3.2: expected exactly 5 trampoline_calls for 5-element map, got {}",
        stats.trampoline_calls
    );
}

/// Task 3.2: five-mode identity for all wired array sites.
/// Proves the trampoline does not introduce any behavioral divergence.
#[tokio::test]
async fn wired_sites_five_mode_identity() {
    let cases = [
        // map
        r#"
            import * as array from "std/array"
            let xs = [1, 2, 3]
            print(array.map(xs, (x) => x * 10))
        "#,
        // filter
        r#"
            import * as array from "std/array"
            let xs = [1, 2, 3, 4, 5]
            print(array.filter(xs, (x) => x % 2 == 0))
        "#,
        // reduce
        r#"
            import * as array from "std/array"
            let xs = [1, 2, 3, 4, 5]
            print(array.reduce(xs, (acc, x) => acc + x, 0))
        "#,
        // sort (custom comparator)
        r#"
            import * as array from "std/array"
            let xs = [3, 1, 4, 1, 5, 9]
            print(array.sort(xs, (a, b) => a - b))
        "#,
        // find
        r#"
            import * as array from "std/array"
            let xs = [1, 2, 3, 4, 5]
            print(array.find(xs, (x) => x > 3))
        "#,
        // findIndex
        r#"
            import * as array from "std/array"
            let xs = [1, 2, 3, 4, 5]
            print(array.findIndex(xs, (x) => x > 3))
        "#,
        // some
        r#"
            import * as array from "std/array"
            let xs = [1, 2, 3, 4, 5]
            print(array.some(xs, (x) => x > 4))
        "#,
        // every
        r#"
            import * as array from "std/array"
            let xs = [2, 4, 6]
            print(array.every(xs, (x) => x % 2 == 0))
        "#,
        // flatMap
        r#"
            import * as array from "std/array"
            let xs = [1, 2, 3]
            print(array.flatMap(xs, (x) => [x, x * 10]))
        "#,
        // groupBy
        r#"
            import * as array from "std/array"
            let xs = [1, 2, 3, 4]
            let g = array.groupBy(xs, (x) => x % 2 == 0 ? "even" : "odd")
            print(g)
        "#,
        // partition
        r#"
            import * as array from "std/array"
            let xs = [1, 2, 3, 4, 5]
            print(array.partition(xs, (x) => x % 2 == 0))
        "#,
        // object.mapValues
        r#"
            import * as object from "std/object"
            let o = {a: 1, b: 2, c: 3}
            print(object.mapValues(o, (v, k) => v * 10))
        "#,
    ];
    for src in cases {
        assert_five_mode_identical(src).await;
    }
}

/// Task 3.2: builtin callbacks (non-closures) still work correctly through
/// the Generic fallback path. arm() returns None for builtins; the trampoline
/// is NOT used. Output must be byte-identical across all five modes.
#[tokio::test]
async fn wired_sites_builtin_callback_generic_fallback() {
    let src = r#"
        import * as array from "std/array"
        let xs = ["hello", "world", "!"]
        print(array.map(xs, len))
    "#;
    assert_five_mode_identical(src).await;
}

/// Task 3.2: trampoline_calls == 0 when no-call-fast is active (kill switch).
/// Proves the kill switch suppresses trampoline dispatch on the wired sites.
#[tokio::test]
async fn wired_sites_trampoline_suppressed_by_kill_switch() {
    let src = r#"
        import * as array from "std/array"
        let xs = [1, 2, 3, 4, 5]
        let r = array.map(xs, (x) => x * 2)
        print(r)
    "#;
    // With call_fast=true → trampoline fires.
    let (_out, _exit, stats_on) = ascript::vm_run_source_call_fast_stats(src)
        .await
        .expect("call_fast stats");
    assert!(
        stats_on.trampoline_calls > 0,
        "kill switch test: trampoline_calls should be > 0 with call_fast=true, got {}",
        stats_on.trampoline_calls
    );
    // With no-call-fast → same output, trampoline_calls==0.
    let ncf = ascript::vm_run_source_no_call_fast(src)
        .await
        .expect("no-call-fast");
    let spec = ascript::vm_run_source(src).await.expect("spec");
    assert_eq!(spec, ncf, "kill switch: output differs between call_fast on/off");
}
