//! SP3 — runtime-robustness limit tests.
//!
//! Two classes of large-but-valid input that USED to crash the process (a Rust
//! `panic!`/`.expect` → `SIGABRT`, exit 134) must now fail CLEANLY:
//!
//! - §A bytecode-capacity overflow (const pool / proto / class-proto / import
//!   table > `u16::MAX`, jump displacement > 32 KB) → a clean `CompileError`
//!   (returned `Err`, non-134 exit), never a panic.
//! - §B deep recursion / deep expression nesting → a catchable Tier-2 panic
//!   `maximum recursion depth exceeded`, byte-identical on both engines, before the
//!   native stack overflows (the differential cases live in `tests/vm_differential.rs`;
//!   the no-SIGABRT CLI guard + the margin guard live here).

use std::process::Command;

// =============================================================================
// §A — capacity overflow → clean CompileError
// =============================================================================

/// A program declaring `n` DISTINCT fractional number constants (so the pool's
/// structural de-dup does not collapse them). `n > 65535` overflows the const pool.
fn gen_distinct_consts(n: usize) -> String {
    let mut s = String::with_capacity(n * 16);
    for i in 0..n {
        s.push_str(&format!("let _v{i} = {i}.5\n"));
    }
    s
}

/// A program defining `n` distinct top-level functions (overflows the proto table).
fn gen_distinct_fns(n: usize) -> String {
    let mut s = String::with_capacity(n * 18);
    for i in 0..n {
        s.push_str(&format!("fn _f{i}() {{ return {i} }}\n"));
    }
    s
}

/// A program defining `n` distinct top-level classes (overflows the class-proto table).
fn gen_distinct_classes(n: usize) -> String {
    let mut s = String::with_capacity(n * 16);
    for i in 0..n {
        s.push_str(&format!("class _C{i} {{}}\n"));
    }
    s
}

#[tokio::test]
async fn const_pool_overflow_is_clean_error() {
    let src = gen_distinct_consts(70_000);
    let err = ascript::vm_run_source(&src)
        .await
        .expect_err("oversize module must error, not succeed");
    assert!(
        err.message.contains("65535 constants"),
        "expected const-pool capacity message, got: {}",
        err.message
    );
}

#[tokio::test]
async fn proto_table_overflow_is_clean_error() {
    let src = gen_distinct_fns(70_000);
    let err = ascript::vm_run_source(&src)
        .await
        .expect_err("oversize module must error, not succeed");
    assert!(
        err.message.contains("65535 function definitions"),
        "expected proto-table capacity message, got: {}",
        err.message
    );
}

#[tokio::test]
async fn class_proto_overflow_is_clean_error() {
    let src = gen_distinct_classes(70_000);
    let err = ascript::vm_run_source(&src)
        .await
        .expect_err("oversize module must error, not succeed");
    assert!(
        err.message.contains("65535 class definitions"),
        "expected class-proto capacity message, got: {}",
        err.message
    );
}

#[tokio::test]
async fn import_overflow_is_clean_error() {
    // 70_000 distinct namespace imports (distinct local names → distinct import
    // descriptors, so they do not collapse). The overflow trips at COMPILE time
    // (import-table emit), before any module resolution, so this is a clean
    // `CompileError` regardless of whether the module exists.
    let mut src = String::with_capacity(70_000 * 40);
    for i in 0..70_000 {
        src.push_str(&format!("import * as _i{i} from \"std/math\"\n"));
    }
    let err = ascript::vm_run_source(&src)
        .await
        .expect_err("oversize module must error, not succeed");
    assert!(
        err.message.contains("65535 imports"),
        "expected import-table capacity message, got: {}",
        err.message
    );
}

#[tokio::test]
async fn jump_displacement_overflow_is_clean_error() {
    // A single `if` whose THEN body emits > 32 KB of bytecode forces the
    // forward jump-over-then displacement past the `i16` range. Each `print(0)`
    // is a handful of bytes; tens of thousands of them blow the 32 KB window.
    let mut body = String::with_capacity(60_000 * 9);
    for _ in 0..60_000 {
        body.push_str("  print(0)\n");
    }
    let src = format!("if (true) {{\n{body}}}\n");
    let err = ascript::vm_run_source(&src)
        .await
        .expect_err("oversize function body must error, not succeed");
    assert!(
        err.message.contains("function body too large"),
        "expected jump-displacement capacity message, got: {}",
        err.message
    );
}

/// The generic VM (kill-switch off) must reject the same oversize module just as
/// cleanly — capacity is a compile-time property, independent of specialization.
#[tokio::test]
async fn const_pool_overflow_is_clean_on_generic_vm_too() {
    let src = gen_distinct_consts(70_000);
    let err = ascript::vm_run_source_generic(&src)
        .await
        .expect_err("oversize module must error on the generic VM too");
    assert!(
        err.message.contains("65535 constants"),
        "expected const-pool capacity message (generic VM), got: {}",
        err.message
    );
}

/// Negative-sweep guard (SP3 §A6): `src/vm/{chunk,aso}.rs` must contain ZERO
/// production (non-`mod tests`) capacity `panic!`/`.expect` sites. Trips if a
/// future capacity `.expect("…exceed…")` / `panic!("…range…")` is re-introduced.
#[test]
fn no_capacity_panics_in_chunk_or_aso() {
    for path in ["src/vm/chunk.rs", "src/vm/aso.rs"] {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        // Cut the file at the start of its `mod tests` so test-only panics (which
        // are legitimate assertions) are excluded from the scan.
        let prod = match text.find("mod tests") {
            Some(i) => &text[..i],
            None => &text[..],
        };
        for (lineno, line) in prod.lines().enumerate() {
            let l = line.trim_start();
            // Capacity panics named the limit with "exceed"/"range"/"u16::MAX"/"u32::MAX".
            let is_capacity_panic = (l.contains(".expect(") || l.contains("panic!("))
                && (l.contains("exceed")
                    || l.contains("out of i16 range")
                    || l.contains("u16::MAX")
                    || l.contains("u32::MAX"));
            assert!(
                !is_capacity_panic,
                "{path}:{} re-introduced a capacity panic: {line}",
                lineno + 1
            );
        }
    }
}

/// CLI exit guard (SP3 §A6): the oversize-const program run through the BUILT
/// binary exits with the normal error code (NOT 134 / a panic abort) and prints
/// the actionable remedy on stderr.
#[test]
fn cli_oversize_module_exits_cleanly_not_134() {
    let file = std::env::temp_dir().join("ascript_sp3_oversize_consts.as");
    std::fs::write(&file, gen_distinct_consts(70_000)).unwrap();

    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin).arg("run").arg(&file).output().unwrap();

    assert!(
        !output.status.success(),
        "oversize module must fail, not succeed"
    );
    assert_ne!(
        output.status.code(),
        Some(134),
        "oversize module must NOT abort with SIGABRT (exit 134); got a clean error instead"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("65535 constants"),
        "expected actionable const-pool message on stderr, got: {stderr}"
    );
}

// =============================================================================
// §B — recursion-depth guard: no SIGABRT, clean catchable panic, both engines
// =============================================================================

/// A self-recursive driver to logical depth `n` (parenthesized condition so the
/// legacy tree-walker front-end accepts it).
fn rec_src(n: usize) -> String {
    format!("fn f(n) {{\n  if (n <= 0) {{ return 0 }}\n  return 1 + f(n - 1)\n}}\nprint(f({n}))\n")
}

/// Run `src` through the BUILT binary on the given engine (`""` = VM default,
/// `"tree-walker"` = `--tree-walker`). Returns (exit code, stderr).
fn run_cli(src: &str, tree_walker: bool) -> (Option<i32>, String) {
    // A unique path per call (engine + a hash of the source) so parallel tests
    // never clobber each other's temp file.
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    tree_walker.hash(&mut h);
    let file = std::env::temp_dir().join(format!(
        "ascript_sp3_recursion_{}_{:x}.as",
        if tree_walker { "tw" } else { "vm" },
        h.finish()
    ));
    std::fs::write(&file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let mut cmd = Command::new(bin);
    cmd.arg("run");
    if tree_walker {
        cmd.arg("--tree-walker");
    }
    cmd.arg(&file);
    let output = cmd.output().unwrap();
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// SP3 §B7 no-SIGABRT guard: an over-limit recursion exits with the clean
/// recoverable-panic code (NOT 134 / a stack-overflow abort) and prints the fixed
/// message on stderr — on BOTH `ascript run` (VM) and `--tree-walker`. The built
/// binary runs on the enlarged worker stack so the guard fires before native
/// overflow.
#[test]
fn cli_over_limit_recursion_is_clean_not_134_both_engines() {
    let src = rec_src(8000);
    for tree_walker in [false, true] {
        let (code, stderr) = run_cli(&src, tree_walker);
        let who = if tree_walker { "tree-walker" } else { "vm" };
        assert_ne!(
            code,
            Some(134),
            "{who}: over-limit recursion must NOT abort with SIGABRT (exit 134)"
        );
        assert_eq!(code, Some(1), "{who}: expected the clean error exit (1)");
        assert!(
            stderr.contains("maximum recursion depth exceeded"),
            "{who}: expected recursion-limit message on stderr, got: {stderr}"
        );
    }
}

/// A recursion comfortably UNDER the cap exits 0 with the correct output on BOTH
/// engines (proves the guard does not fire early — the limit/stack pair has room).
#[test]
fn cli_under_limit_recursion_succeeds_both_engines() {
    let src = rec_src(500);
    for tree_walker in [false, true] {
        let (code, _stderr) = run_cli(&src, tree_walker);
        let who = if tree_walker { "tree-walker" } else { "vm" };
        assert_eq!(code, Some(0), "{who}: under-limit recursion should exit 0");
    }
}

// =============================================================================
// §C (SP9 §1) — robust unbounded recursion: deep NATIVE re-entry no longer
// SIGABRTs. These programs nest native Rust frames at the four re-entry points
// (`call_value` for HOF callbacks, generator `resume_vm`, method dispatch,
// `compile_expr`/`eval_expr` for nested expressions) to a depth that overflows
// the DEFAULT test-thread stack (~8 MB) BEFORE SP9 — exit 134 / SIGABRT — yet is
// comfortably UNDER the `MAX_CALL_DEPTH` logical cap (3000). After SP9's
// `stacker::maybe_grow` guard at each re-entry point, they succeed BYTE-IDENTICALLY
// on BOTH engines, run directly on the test thread (NOT the enlarged worker stack),
// proving the guard — not the worker stack — is what prevents the native overflow.
//
// Depth rationale: at ~82 KB / logical native frame (SP3 §B6) the default test
// thread holds ~95 re-entry frames before overflow, so a depth of 1500 (well over
// that, well under the 3000 cap) is a faithful "used-to-SIGABRT, now-succeeds"
// reproducer. The programs are written to terminate quickly once the stack grows.

/// A recursion whose every level re-enters `Vm::call_value`'s `Value::Closure`
/// arm through an `array.map` HOF callback (`src/vm/run.rs` `call_value` :2848).
/// Each level is a fresh large native frame (map native + call_value + run).
fn deep_hof_src(n: usize) -> String {
    format!(
        "import {{ map }} from \"std/array\"\nfn rec(n) {{\n  if (n <= 0) {{ return 0 }}\n  return map([n], (x) => rec(x - 1))[0]\n}}\nprint(rec({n}))\n"
    )
}

/// A recursion through deep GENERATOR composition: a generator whose body drives
/// (resumes) a nested generator, nesting `coro::resume_vm` native frames.
fn deep_generator_src(n: usize) -> String {
    format!(
        "fn* g(n) {{\n  if (n <= 0) {{\n    yield 0\n  }} else {{\n    let inner = g(n - 1)\n    let r = inner.next()\n    while (r != nil) {{\n      yield r\n      r = inner.next()\n    }}\n  }}\n}}\nlet total = 0\nlet gen = g({n})\nlet v = gen.next()\nwhile (v != nil) {{\n  total = total + v\n  v = gen.next()\n}}\nprint(total)\n"
    )
}

/// A recursion through non-IC METHOD dispatch (`invoke_compiled_method` /
/// `vm_construct` re-entry into `Vm::run`).
fn deep_method_src(n: usize) -> String {
    format!(
        "class R {{\n  fn rec(n) {{\n    if (n <= 0) {{ return 0 }}\n    let inner = R()\n    return inner.rec(n - 1)\n  }}\n}}\nprint(R().rec({n}))\n"
    )
}

/// A deeply nested SOURCE expression: `((((…1…))))`. Exercises the synchronous
/// compiler `compile_expr` (`src/compile/mod.rs:984`) and the tree-walker
/// `eval_expr` (`src/interp.rs:2252`). Depth is in PAREN nesting, not calls;
/// 2500 is over the native test-thread budget but under the 3000 cap.
fn deep_paren_src(n: usize) -> String {
    let mut s = String::with_capacity(n * 2 + 16);
    s.push_str("print(");
    for _ in 0..n {
        s.push('(');
    }
    s.push('1');
    for _ in 0..n {
        s.push(')');
    }
    s.push_str(")\n");
    s
}

/// Assert a program runs to SUCCESS with byte-identical stdout + exit on all
/// three engines (tree-walker, specialized VM, generic VM), DIRECTLY on the test
/// thread (no enlarged worker stack). Before SP9 these SIGABRT (the test harness
/// would die); after SP9 they succeed. `expected` is the exact stdout.
async fn assert_deep_succeeds_three_way(src: &str, expected: &str) {
    let tw = ascript::run_source(src).await.expect("tree-walker: deep recursion must succeed (SP9)");
    assert_eq!(tw, expected, "tree-walker stdout mismatch");
    let (vm, code) = ascript::vm_run_source(src)
        .await
        .expect("specialized VM: deep recursion must succeed (SP9)");
    assert_eq!(vm, expected, "specialized VM stdout mismatch");
    assert_eq!(code, None, "specialized VM exit");
    let (gen, gcode) = ascript::vm_run_source_generic(src)
        .await
        .expect("generic VM: deep recursion must succeed (SP9)");
    assert_eq!(gen, expected, "generic VM stdout mismatch");
    assert_eq!(gcode, None, "generic VM exit");
}

#[tokio::test]
async fn deep_hof_callback_recursion_no_sigabrt_three_way() {
    assert_deep_succeeds_three_way(&deep_hof_src(1500), "0\n").await;
}

#[tokio::test]
async fn deep_generator_composition_no_sigabrt_three_way() {
    assert_deep_succeeds_three_way(&deep_generator_src(1500), "0\n").await;
}

#[tokio::test]
async fn deep_method_dispatch_recursion_no_sigabrt_three_way() {
    assert_deep_succeeds_three_way(&deep_method_src(1500), "0\n").await;
}

#[tokio::test]
async fn deep_nested_expression_no_sigabrt_three_way() {
    assert_deep_succeeds_three_way(&deep_paren_src(2500), "1\n").await;
}

/// Boundary parity (SP3, re-verified under SP9): the logical cap is still THE
/// ceiling on both engines — `f(MAX-1)` succeeds and `f(MAX+slack)` fails with the
/// clean `maximum recursion depth exceeded` panic, identically on all three
/// engines, run on the enlarged worker stack (as the binary does) so the native
/// stack is never the limiting factor.
#[test]
fn recursion_cap_is_the_ceiling_both_engines_under_sp9() {
    // Just under the cap → success on the worker stack (3000 deep needs the big
    // stack on the tree-walker even with stacker, since stacker only grows at
    // re-entry funnels — plain script recursion uses the heap frames + the worker
    // stack exactly as SP3 established).
    let under = rec_src(2900);
    let over = rec_src(8000);
    let (tw_under, vm_under, gen_under) = ascript::run_on_worker_stack(move || async move {
        let tw = ascript::run_source_exit(&under).await.map(|(o, _)| o).map_err(|e| e.message);
        let vm = ascript::vm_run_source(&under).await.map(|(o, _)| o).map_err(|e| e.message);
        let gen = ascript::vm_run_source_generic(&under).await.map(|(o, _)| o).map_err(|e| e.message);
        (tw, vm, gen)
    });
    assert_eq!(tw_under, Ok("2900\n".to_string()), "tree-walker under cap");
    assert_eq!(vm_under, Ok("2900\n".to_string()), "specialized VM under cap");
    assert_eq!(gen_under, Ok("2900\n".to_string()), "generic VM under cap");

    let (tw_over, vm_over, gen_over) = ascript::run_on_worker_stack(move || async move {
        let tw = ascript::run_source_exit(&over).await.map(|(o, _)| o).map_err(|e| e.message);
        let vm = ascript::vm_run_source(&over).await.map(|(o, _)| o).map_err(|e| e.message);
        let gen = ascript::vm_run_source_generic(&over).await.map(|(o, _)| o).map_err(|e| e.message);
        (tw, vm, gen)
    });
    assert_eq!(
        tw_over,
        Err("maximum recursion depth exceeded".to_string()),
        "tree-walker over cap → clean panic"
    );
    assert_eq!(
        vm_over,
        Err("maximum recursion depth exceeded".to_string()),
        "specialized VM over cap → clean panic"
    );
    assert_eq!(
        gen_over,
        Err("maximum recursion depth exceeded".to_string()),
        "generic VM over cap → clean panic"
    );
}

/// SP3 §B7 margin guard: the tree-walker has the LARGEST per-frame native budget
/// of either engine. The depth guard fires AT `MAX_CALL_DEPTH` logical units; at
/// that moment the tree-walker's native stack holds ~`MAX_CALL_DEPTH`
/// `run_body`+`eval_expr` frames. If the `WORKER_STACK_SIZE` were too small the
/// process would SIGABRT BEFORE the guard fires; the fact that an over-limit
/// recursion instead returns the CLEAN catchable panic proves the
/// stack-size/limit pair has headroom. Runs on the enlarged worker stack via
/// `run_on_worker_stack`, exactly as the `run` binary does.
#[tokio::test(flavor = "current_thread")]
async fn margin_guard_treewalker_over_limit_clean_panic_no_overflow() {
    // Well over the cap on the tree-walker (which trips earlier than the VM because
    // it also counts runtime expression nesting). The guard MUST fire before the
    // native stack overflows.
    let src = rec_src(20_000);
    let summary = ascript::run_on_worker_stack(move || async move {
        // Reduce to a `Send` summary before crossing the join (`AsError` is `!Send`).
        ascript::run_source_exit(&src)
            .await
            .map_err(|e| e.message)
    });
    match summary {
        Err(msg) => assert_eq!(
            msg, "maximum recursion depth exceeded",
            "tree-walker over-limit recursion must yield the clean recursion panic"
        ),
        Ok(out) => panic!("expected a clean recursion panic, but the run succeeded: {out:?}"),
    }
}
