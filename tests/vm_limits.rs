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
