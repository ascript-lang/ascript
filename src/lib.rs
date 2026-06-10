pub mod ast;
pub mod bundle;
pub mod check;
pub mod compile;
pub mod coro;
// DBG Task 5b: the Debug Adapter Protocol (DAP) server over stdio. Feature-gated
// (`dap`, default-on) so `--no-default-features` builds none of it.
#[cfg(feature = "dap")]
pub mod dap;
pub mod det;
pub mod diagnostics;
// DX D1: `ascript doc` — the API documentation generator (CST walk → doc model →
// HTML/Markdown). Static-only (never instantiates the interpreter); feature-gated
// (`doc`, default-on) so `--no-default-features` builds none of it.
#[cfg(feature = "doc")]
pub mod doc;
pub mod env;
pub mod error;
pub mod fmt;
// FUZZ: the grammar-aware source generator (the differential-fuzzing core asset).
// Feature-gated (`fuzzgen`, NON-default) + `cfg(test)` for the crate's own unit tests +
// `--cfg fuzzing` for a libFuzzer build — so it compiles into `ascript` ONLY in those
// dev/test contexts and NEVER in a normal/`--no-default-features` production build, and
// `arbitrary` (an optional dep behind `fuzzgen`) never enters the production graph. The
// `fuzzgen` feature is what an INTEGRATION test (`tests/property.rs`) reaches (it links the
// crate's normal build, which does not see `cfg(test)`); the crate's self-dev-dependency
// enables it (plan Task 4, spec §3.1).
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
pub mod fuzzgen;
pub mod gc;
pub mod interp;
pub(crate) mod lex_literals;
pub mod lexer;
#[cfg(feature = "lsp")]
pub mod lsp;
pub mod parser;
// DBG Task 7: the CPU sampling profiler's aggregation + output (speedscope JSON +
// collapsed folded-stacks). Feature-gated (`profile`, default-on); the publish seam
// itself lives on the VM (`Vm::publish_profile_frames`) behind the single
// `Vm.instrument` gate. `--no-default-features` builds none of this and `--profile`
// reports a clean rebuild hint.
#[cfg(feature = "profile")]
pub mod profile;
pub mod repl;
pub mod span;
pub mod stdlib;
pub mod syntax;
pub mod task;
// DX D2 Task 10: `--filter PATTERN` test-name filtering (substring or `/regex/`) +
// `--watch` import-graph scoping. Core (no feature gate); the regex branch is
// `data`/`sys`-gated and degrades to a clean error otherwise.
pub mod test_filter;
pub mod watch;
pub mod token;
pub mod value;
pub mod vm;
pub mod worker;

use crate::error::{AsError, SourceInfo};
use crate::interp::Interp;
pub use crate::interp::TestSummary;
#[cfg(feature = "telemetry")]
pub use crate::stdlib::telemetry::model::CapturedRequest;
/// Test seam: force the SP12 telemetry capture-mode send to "fail" (per-thread),
/// to exercise the error model (a flush failure is logged once + dropped, never
/// aborts the program). `#[doc(hidden)]` — not a public API.
#[doc(hidden)]
#[cfg(feature = "telemetry")]
pub fn telemetry_test_force_send_error(on: bool) {
    crate::stdlib::telemetry::set_test_force_send_error(on);
}
use std::path::Path;
use std::rc::Rc;

/// SP3 §B: run a `!Send` async closure on a worker thread with the enlarged
/// [`crate::interp::WORKER_STACK_SIZE`] stack, hosting a fresh single-threaded
/// tokio runtime + `LocalSet`. This is how the recursion-depth guard
/// (`MAX_CALL_DEPTH` logical frames) sits under native capacity with headroom: a
/// deeply-recursive program hits the clean catchable panic BEFORE the native stack
/// overflows. The `run` binary uses an enlarged stack via its own worker thread in
/// `main`; this helper is the in-process equivalent for tests / embedders that
/// drive the engines directly (`vm_run_source` / `run_source_exit`) and want the
/// same headroom. `make_fut` builds the (`!Send`) future INSIDE the worker thread,
/// so only `make_fut` (and the returned `R`) need be `Send`.
pub fn run_on_worker_stack<R, F, Fut>(make_fut: F) -> R
where
    R: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = R>,
{
    std::thread::Builder::new()
        .name("ascript-worker".to_string())
        .stack_size(crate::interp::WORKER_STACK_SIZE)
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build worker tokio runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, make_fut())
        })
        .expect("failed to spawn worker thread")
        .join()
        .expect("worker thread panicked")
}

/// Run a `.as` file as the entry module (with import resolution relative to it).
///
/// Returns the process exit code: `Ok(0)` for clean termination, `Ok(n)` when
/// the program calls `exit(n)`, `Err(e)` on a Tier-2 panic.
///
/// The program runs inside a `tokio::task::LocalSet` so it (and, from M17 Phase 2
/// on, any tasks it spawns) lives on the current-thread runtime. After the root
/// future completes we drive the LocalSet to drain spawned tasks — a no-op today.
///
/// `script_args` are the trailing command-line arguments after the file path
/// (only the script's own args — NOT the binary name or the file path).
/// Pass `&[]` if the caller provides no trailing args.
pub async fn run_file(path: &Path, script_args: &[String]) -> Result<i32, AsError> {
    run_file_with_packages(path, script_args, None, None).await
}

/// Like [`run_file`] (tree-walker) but installs a CLI-resolved package map (SP6)
/// before running, so a bare `import "pkg"` resolves through it. `None` = no
/// package resolver (every bare specifier is "unknown package"). `caps` (FFI §4.5)
/// is the CLI/manifest-composed initial capability set; `None` = all granted.
pub async fn run_file_with_packages(
    path: &Path,
    script_args: &[String],
    packages: Option<crate::interp::PackageMap>,
    caps: Option<crate::stdlib::caps::CapSet>,
) -> Result<i32, AsError> {
    // CLI `run` streams `print` output live to stdout (so it appears immediately
    // and survives a later panic). Under `Live` there is no captured string, so
    // the success contract is `()` — the caller does not re-print anything.
    let interp = Rc::new(Interp::new_live());
    interp.set_cli_args(script_args);
    // FFI §4.5: install the composed capability set before running any code.
    if let Some(caps) = caps {
        interp.set_caps(caps);
    }
    if let Some(map) = packages {
        interp.set_package_resolver(map);
    }
    interp.install_self();
    let local = tokio::task::LocalSet::new();
    let result = local.run_until(interp.load_module(path)).await;
    local.await; // drain spawned tasks (structured join) — no-op until Phase 2
                 // End-of-program cycle collection (V13-T3): the tree-walker shares
                 // the same `Cc` value model, so a final sweep here reclaims any
                 // leftover cycles on clean shutdown. Output already streamed (Live).
    crate::gc::collect();
    match result {
        Ok(_) => Ok(0),
        Err(crate::interp::Control::Panic(e)) => Err(e),
        Err(crate::interp::Control::Propagate(_)) => Ok(0),
        Err(crate::interp::Control::Exit(code)) => Ok(code),
    }
}

/// Load each file as a module (running its `test(...)` registrations) on a
/// single `Interp`, then run all registered tests and return a summary.
pub async fn run_tests(files: &[String]) -> Result<TestSummary, AsError> {
    run_tests_with_packages(files, None, None).await
}

/// Like [`run_tests`] but installs a CLI-resolved package map (SP6) so a bare
/// `import "pkg"` in a test file resolves through it. `None` = no resolver. `caps`
/// (FFI §4.5) is the CLI/manifest-composed capability set; `None` = all granted.
pub async fn run_tests_with_packages(
    files: &[String],
    packages: Option<crate::interp::PackageMap>,
    caps: Option<crate::stdlib::caps::CapSet>,
) -> Result<TestSummary, AsError> {
    run_tests_with_options(files, packages, caps, None, false, None).await
}

/// DX D2 Task 5 — the test runner with the `--parallel[=N]` option.
///
/// `parallel`:
///   - `None` → the SERIAL default: load every file into ONE `Interp` and run all tests
///     together (today's behavior, unchanged).
///   - `Some(n)` → dispatch each FILE to its own shared-nothing worker isolate, up to `n`
///     at a time, then aggregate the per-file summaries in DETERMINISTIC input-file order
///     (then registration order within a file). The printed summary AND exit code are
///     byte-identical regardless of which isolate finishes first (the §7 contract). A
///     single file (or `n <= 1`) degrades to the serial path — one isolate sequential is
///     the serial path with extra overhead, so we reuse the serial path directly.
pub async fn run_tests_with_options(
    files: &[String],
    packages: Option<crate::interp::PackageMap>,
    caps: Option<crate::stdlib::caps::CapSet>,
    parallel: Option<usize>,
    update_snapshots: bool,
    filter: Option<&str>,
) -> Result<TestSummary, AsError> {
    // Parallelize only when asked for >1 isolate AND there is more than one file. A
    // single file in one isolate is the serial path with extra cost — degrade to serial
    // (point 3: "a SINGLE file (or --parallel=1) degrades to today's serial path").
    match parallel {
        Some(n) if n >= 2 && files.len() >= 2 => {
            // DX D2 Task 8: ORPHAN detection requires the single-Interp touched-set,
            // which only the serial path has. Under `--parallel`, snapshot re-baseline
            // (`--update-snapshots`) still works per-isolate (the flag crosses the
            // airlock), but orphan detection/removal is a SERIAL-path feature (each
            // isolate sees only its own file's touches — it cannot tell a sibling
            // file's untouched snapshot from a genuine orphan). Documented asymmetry.
            //
            // DX D2 Task 10: the `--filter` is applied INSIDE each isolate (same parsed
            // filter raw, re-parsed per isolate across the airlock), so the filtered/
            // passed/failed aggregate is identical regardless of parallelism (§7).
            run_tests_parallel(files, packages, caps, n, update_snapshots, filter).await
        }
        _ => run_tests_serial(files, packages, caps, update_snapshots, filter).await,
    }
}

/// DX D2 Task 6 — run `files` as a test suite on the bytecode VM with LINE COVERAGE
/// armed, returning the aggregated [`TestSummary`] plus the rendered coverage report
/// string (text/lcov) or a path hint (html, written to `target/coverage/`).
///
/// **VM-only (documented asymmetry).** The normal `ascript test` path runs on the
/// tree-walker (the differential oracle); coverage is recorded via the patch-based
/// `Op::Break` trap on the `Vm.instrument` seam, so a coverage run executes on the VM
/// instead. Each test FILE runs in its own `Interp`+`Vm` (so proto identities are
/// per-chunk and the per-file table is independent); the per-file tables are MERGED in
/// stable order (order-independent set/map unions). Because each registered `test(...)`
/// closure is a `Value::Closure`, `run_registered_tests` re-enters the SAME armed `Vm`
/// (via `interp.vm()`), so coverage observes both the module body and every test body.
///
/// **Observation-only.** The trap marks the line covered, restores the original opcode,
/// and re-dispatches it — so the program's behavior + output are byte-identical to a
/// non-coverage run (the Gate-1 invariant).
pub async fn run_tests_with_coverage(
    files: &[String],
    packages: Option<crate::interp::PackageMap>,
    caps: Option<crate::stdlib::caps::CapSet>,
    filter: Option<&str>,
    format: crate::vm::coverage_report::CoverageFormat,
) -> Result<(TestSummary, String), AsError> {
    use crate::vm::instrument::CoverageTable;

    let filter = match filter {
        Some(raw) => Some(crate::test_filter::TestFilter::parse(raw).map_err(AsError::new)?),
        None => None,
    };
    let filter = filter.as_ref();

    let mut summary = TestSummary::default();
    let mut merged = CoverageTable::new();
    // For an HTML report we also keep each file's source text to color the line view.
    let mut sources: Vec<(String, String)> = Vec::new();

    for file in files {
        let path = std::path::Path::new(file);
        let (file_summary, table, src_pair) =
            run_one_file_with_coverage(path, packages.clone(), caps.clone(), filter).await?;
        // Stable input-file-order aggregation (mirrors the serial path's determinism).
        summary.passed += file_summary.passed;
        summary.failed += file_summary.failed;
        summary.filtered += file_summary.filtered;
        summary.failures.extend(file_summary.failures);
        merged.merge(&table);
        if let Some(pair) = src_pair {
            sources.push(pair);
        }
    }

    let report = render_coverage(&merged, format, &sources);
    Ok((summary, report))
}

/// Run ONE test file on a coverage-armed VM: compile, arm coverage over the proto tree,
/// run the module top-level (which registers `test(...)` closures), then run the
/// registered tests (re-entering the same VM), and reclaim the coverage table. Returns
/// the per-file summary, its coverage table, and its `(path, source)` pair.
async fn run_one_file_with_coverage(
    path: &std::path::Path,
    packages: Option<crate::interp::PackageMap>,
    caps: Option<crate::stdlib::caps::CapSet>,
    filter: Option<&crate::test_filter::TestFilter>,
) -> Result<
    (
        TestSummary,
        crate::vm::instrument::CoverageTable,
        Option<(String, String)>,
    ),
    AsError,
> {
    use crate::vm::chunk::FnProto;
    use crate::vm::instrument::{CoverageTable, Instrumentation};
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;

    let src = std::fs::read_to_string(path)
        .map_err(|e| AsError::new(format!("cannot read {}: {}", path.display(), e)))?;
    let src_info = Rc::new(SourceInfo {
        path: path.display().to_string(),
        text: src.clone(),
    });
    let chunk = crate::compile::compile_source(&src)
        .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?;
    // Bind the module source onto the whole proto tree so the coverage arming can read
    // each proto's path + line table.
    chunk.set_module_source(&src_info);

    let interp = Rc::new(Interp::new());
    if let Some(caps) = caps {
        interp.set_caps(caps);
    }
    interp.set_worker_source(&src);
    if let Some(map) = packages {
        interp.set_package_resolver(map);
    }
    interp.install_self();
    let vm = Vm::with_instrument(
        interp.clone(),
        Instrumentation {
            breakpoints: None,
            profiler: None,
            coverage: Some(CoverageTable::new()),
        },
    );
    if let Some(dir) = path.parent() {
        vm.set_module_dir(dir.to_path_buf());
    }

    let proto = Rc::new(FnProto {
        chunk,
        arity: 0,
        has_rest: false,
        is_async: false,
        is_generator: false,
        is_worker: false,
        owning_class: None,
        params: Vec::new(),
        ret: None,
        local_names: Vec::new(),
        debug_name: None,
    });
    // Arm coverage over the entry proto tree BEFORE running (patches each line's first
    // offset to Op::Break; the cold trap arm recovers + records each line on first hit).
    vm.arm_coverage(&proto);

    let closure = Closure::new(proto);
    let mut fiber = crate::vm::fiber::Fiber::new(closure);

    let local = tokio::task::LocalSet::new();
    let mut file_summary = TestSummary::default();
    let run_result: Result<(), AsError> = local
        .run_until(crate::interp::telemetry_root_scope(async {
            // Run the module body (registers tests; coverage records its lines).
            match vm.run(&mut fiber).await {
                Ok(RunOutcome::Done(_)) | Err(crate::interp::Control::Propagate(_)) => {}
                Ok(RunOutcome::Yielded(_)) => {
                    unreachable!("top-level program cannot yield")
                }
                Err(crate::interp::Control::Panic(e)) => {
                    return Err(e.with_source(src_info.clone()))
                }
                Err(crate::interp::Control::Exit(_)) => {
                    return Err(AsError::new("exit() called during test run"))
                }
            }
            // Run the registered tests (each re-enters the same armed VM).
            match interp.run_registered_tests_filtered(filter).await {
                Ok(s) => {
                    file_summary = s;
                    Ok(())
                }
                Err(crate::interp::Control::Exit(_)) => {
                    Err(AsError::new("exit() called during test run"))
                }
                Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info.clone())),
                Err(crate::interp::Control::Propagate(_)) => Ok(()),
            }
        }))
        .await;
    local.await;
    crate::gc::collect();
    run_result?;

    let table = vm.take_coverage().unwrap_or_default();
    Ok((file_summary, table, Some((src_info.path.clone(), src))))
}

/// Render the merged coverage table in the requested format. For `html` the report is a
/// self-contained tree written under `target/coverage/`; the returned string is a
/// human-readable hint pointing at the written index.
fn render_coverage(
    table: &crate::vm::instrument::CoverageTable,
    format: crate::vm::coverage_report::CoverageFormat,
    sources: &[(String, String)],
) -> String {
    use crate::vm::coverage_report::{render_html, render_lcov, render_text, CoverageFormat};
    match format {
        CoverageFormat::Text => render_text(table),
        CoverageFormat::Lcov => render_lcov(table),
        CoverageFormat::Html => {
            let html = render_html(table, sources);
            let dir = std::path::Path::new("target").join("coverage");
            let index = dir.join("index.html");
            match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&index, &html)) {
                Ok(()) => format!("coverage html written to {}\n", index.display()),
                // Fall back to emitting the HTML on stdout if the write fails (never
                // panic on a reachable path — e.g. a read-only filesystem).
                Err(e) => format!("warning: could not write {}: {e}\n{html}", index.display()),
            }
        }
    }
}

/// The serial test path: every file loaded into one `Interp`, all tests run together.
async fn run_tests_serial(
    files: &[String],
    packages: Option<crate::interp::PackageMap>,
    caps: Option<crate::stdlib::caps::CapSet>,
    update_snapshots: bool,
    filter: Option<&str>,
) -> Result<TestSummary, AsError> {
    // Parse the (CLI-validated) raw filter once for this serial run. A re-parse of an
    // already-validated filter cannot realistically fail, but a defensive `?` keeps the
    // path panic-free if it ever did.
    let filter = match filter {
        Some(raw) => Some(crate::test_filter::TestFilter::parse(raw).map_err(AsError::new)?),
        None => None,
    };
    let filter = filter.as_ref();
    let interp = Rc::new(Interp::new());
    if let Some(caps) = caps {
        interp.set_caps(caps);
    }
    if let Some(map) = packages {
        interp.set_package_resolver(map);
    }
    // DX D2 Task 8: enable snapshot re-baseline BEFORE any test code runs.
    interp.set_snapshot_update(update_snapshots);
    interp.install_self();
    let local = tokio::task::LocalSet::new();
    let result: Result<TestSummary, AsError> = local
        .run_until(crate::interp::telemetry_root_scope(async {
            for file in files {
                match interp.load_module(Path::new(file)).await {
                    Ok(_) | Err(crate::interp::Control::Propagate(_)) => {}
                    Err(crate::interp::Control::Panic(e)) => return Err(e),
                    // exit() during module load is a hard error for the test runner:
                    // report it (non-zero exit) rather than faking an empty all-pass
                    // summary. `ascript test` is not the place to terminate the process.
                    Err(crate::interp::Control::Exit(_)) => {
                        return Err(AsError::new("exit() called during test run"))
                    }
                }
            }
            match interp.run_registered_tests_filtered(filter).await {
                Ok(summary) => Ok(summary),
                // exit() inside a test is likewise a hard error: surface a clear
                // failure (non-zero exit) instead of an empty success summary.
                Err(crate::interp::Control::Exit(_)) => {
                    Err(AsError::new("exit() called during test run"))
                }
                Err(crate::interp::Control::Panic(e)) => Err(e),
                Err(crate::interp::Control::Propagate(_)) => Ok(TestSummary::default()),
            }
        }))
        .await;
    local.await; // drain spawned tasks — no-op until Phase 2

    // DX D2 Task 8 — ORPHAN snapshot detection (spec §6.2). After a full serial run we
    // know every `.snap` file an assertion TOUCHED; a `.snap` file in a touched
    // `__snapshots__/` dir that was NOT touched is an orphan (its assertion was
    // removed). We only run this on a SUCCESSFUL run (a load/exit error short-circuits
    // before all assertions executed, so the touched-set is incomplete — never flag
    // orphans from a partial run). Reported to stderr; removed only under
    // `--update-snapshots` (the one destructive path, gated).
    #[cfg(all(feature = "sys", feature = "data"))]
    if result.is_ok() {
        report_orphan_snapshots(&interp, update_snapshots);
    }
    result
}

/// DX D2 Task 8 — report (and, under `--update-snapshots`, REMOVE) orphan snapshot
/// files after a full serial run. Deterministic (sorted) output; never panics on an
/// unreadable/permission-denied path (the scan + removal both degrade cleanly).
#[cfg(all(feature = "sys", feature = "data"))]
fn report_orphan_snapshots(interp: &Interp, update_snapshots: bool) {
    let touched = interp.snapshots_touched();
    let orphans = crate::stdlib::assert_mod::find_orphan_snapshots(&touched);
    if orphans.is_empty() {
        return;
    }
    if update_snapshots {
        for orphan in &orphans {
            match crate::stdlib::assert_mod::remove_orphan_snapshot(orphan) {
                Ok(()) => eprintln!("removed orphan snapshot: {}", orphan.display()),
                Err(e) => eprintln!("warning: {e}"),
            }
        }
    } else {
        eprintln!(
            "warning: {} orphan snapshot file(s) found (no matching assertion this run):",
            orphans.len()
        );
        for orphan in &orphans {
            eprintln!("  {}", orphan.display());
        }
        eprintln!("  run with --update-snapshots to remove them");
    }
}

/// DX D2 Task 5 — the PARALLEL test path: each FILE runs in its own shared-nothing
/// worker isolate, up to `n` at a time (capped by `$ASCRIPT_WORKERS`/`num_cpus`), then
/// the per-file summaries are aggregated in DETERMINISTIC input-file order.
///
/// **Determinism (§7):** isolates finish in nondeterministic order, but results are slotted
/// back into a `Vec` BY INPUT INDEX, then folded in that order — so the aggregate summary
/// (passed/failed totals AND the failure list order) and therefore the printed output +
/// exit code are byte-identical regardless of completion order. Within a file the isolate
/// preserved the test-registration order, so the whole failure list is a stable function
/// of input order.
///
/// A file that fails to load / `exit()`s / dies in its isolate becomes a synthetic FAILED
/// entry (`"<file>": <reason>`) so the OTHER files' results are never lost, and the run
/// still reports a non-zero exit. No reachable path panics or unwraps.
async fn run_tests_parallel(
    files: &[String],
    packages: Option<crate::interp::PackageMap>,
    caps: Option<crate::stdlib::caps::CapSet>,
    n: usize,
    update_snapshots: bool,
    filter: Option<&str>,
) -> Result<TestSummary, AsError> {
    use std::sync::Arc;
    // Cap the in-flight isolate count: the requested `n`, bounded by the same
    // `$ASCRIPT_WORKERS`/`num_cpus` ceiling the pool uses (so `--parallel` never
    // oversubscribes beyond the worker budget), at least 1.
    let cap = worker_isolate_cap().min(n).max(1);
    let sem = Arc::new(tokio::sync::Semaphore::new(cap));

    let local = tokio::task::LocalSet::new();
    let per_file: Vec<crate::worker::testrun::FileRunResult> = local
        .run_until(crate::interp::telemetry_root_scope(async {
            // Spawn one local task per file; each acquires a permit (bounded concurrency)
            // then runs its file in an isolate. The task index pins the result slot, so
            // completion order is irrelevant to aggregation.
            let mut handles = Vec::with_capacity(files.len());
            for file in files {
                let sem = sem.clone();
                let packages = packages.clone();
                let caps = caps.clone();
                // DX D2 Task 10: ship the RAW filter string into the isolate (Send); each
                // isolate re-parses + applies it identically, so the filtered/passed/failed
                // aggregate is independent of `--parallel` (§7).
                let filter = filter.map(str::to_string);
                let path = std::path::PathBuf::from(file);
                handles.push(tokio::task::spawn_local(async move {
                    // A closed semaphore is unreachable here (we never close it); on the
                    // impossible error, treat the file as un-runnable rather than panic.
                    let _permit = match sem.acquire_owned().await {
                        Ok(p) => p,
                        Err(_) => {
                            return Err(
                                "internal: test scheduler semaphore closed".to_string()
                            )
                        }
                    };
                    crate::worker::testrun::run_test_file_in_isolate(
                        &path,
                        packages,
                        caps,
                        update_snapshots,
                        filter,
                    )
                    .await
                }));
            }
            // Await in INPUT order, slotting each result by index. A task that itself
            // panicked (a JoinError) becomes a clean per-file error, never a lost run.
            let mut results = Vec::with_capacity(handles.len());
            for (idx, h) in handles.into_iter().enumerate() {
                let r = match h.await {
                    Ok(r) => r,
                    Err(_) => Err(format!(
                        "test isolate for '{}' panicked",
                        files.get(idx).map(String::as_str).unwrap_or("<file>")
                    )),
                };
                results.push(r);
            }
            results
        }))
        .await;
    local.await;

    // Deterministic aggregation: fold per-file summaries in INPUT-FILE order. A file-level
    // error (load failure / exit / dead isolate) becomes a synthetic failed test attributed
    // to that file, so every file is accounted for and the run reports non-zero.
    let mut agg = TestSummary::default();
    for (file, result) in files.iter().zip(per_file) {
        match result {
            Ok(summary) => {
                agg.passed += summary.passed;
                agg.failed += summary.failed;
                agg.filtered += summary.filtered;
                agg.failures.extend(summary.failures);
            }
            Err(reason) => {
                agg.failed += 1;
                agg.failures.push((file.clone(), reason));
            }
        }
    }
    Ok(agg)
}

/// The isolate-count ceiling shared with the worker pool: `$ASCRIPT_WORKERS` if a positive
/// integer, else `num_cpus` (min 1). `--parallel=N` is clamped to this so it never
/// oversubscribes beyond the worker budget.
fn worker_isolate_cap() -> usize {
    std::env::var("ASCRIPT_WORKERS")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or_else(num_cpus::get)
        .max(1)
}

/// Lex → parse → evaluate in a fresh global environment. Returns captured output.
///
/// `exit(n)` is treated as a clean termination (the captured output is returned
/// and no error is raised). Use [`run_source_exit`] when you need the exit code.
pub async fn run_source(src: &str) -> Result<String, AsError> {
    run_source_exit(src).await.map(|(out, _)| out)
}

/// Like [`run_source`] but also returns the exit code requested by `exit(n)`, if any.
pub async fn run_source_exit(src: &str) -> Result<(String, Option<i32>), AsError> {
    let src_info = Rc::new(SourceInfo {
        path: "<input>".to_string(),
        text: src.to_string(),
    });
    let tokens = lexer::lex(src).map_err(|e| e.with_source(src_info.clone()))?;
    let program = parser::parse(&tokens).map_err(|e| e.with_source(src_info.clone()))?;
    let interp = Rc::new(Interp::new());
    interp.install_self();
    // Workers Spec A: retain the source so a `worker fn` call can build its slice.
    interp.set_worker_source(src);
    // Run in a child of the builtins env so the program can shadow builtins
    // (`let len = 5`) and import names that collide with builtins.
    let env = crate::interp::global_env().child();
    let local = tokio::task::LocalSet::new();
    let result = local
        .run_until(crate::interp::telemetry_root_scope(interp.exec(&program, &env)))
        .await;
    local.await; // drain spawned tasks — no-op until Phase 2
    match result {
        Ok(crate::interp::Flow::Break) => Err(AsError::new("'break' outside of a loop")),
        Ok(crate::interp::Flow::Continue) => Err(AsError::new("'continue' outside of a loop")),
        Ok(crate::interp::Flow::Normal) | Ok(crate::interp::Flow::Return(_)) => {
            Ok((interp.output(), None))
        }
        // A panic aborts the program with its diagnostic.
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        // A top-level `?` propagation simply ends the program.
        Err(crate::interp::Control::Propagate(_)) => Ok((interp.output(), None)),
        // exit(n) — return the captured output plus the exit code.
        Err(crate::interp::Control::Exit(code)) => Ok((interp.output(), Some(code))),
    }
}

/// SP9 §3 test/embedder seam: run `src` on the tree-walker in DETERMINISTIC mode
/// with the given `seed` (the eventual `--deterministic --seed N` CLI flag maps to
/// this path). The clock/RNG seams route through a fresh
/// [`crate::det::DeterminismContext`] in Record mode, so two runs with the same seed
/// produce byte-identical output (the determinism oracle, spec §3.5). `#[doc(hidden)]`
/// — not a stable public API.
#[doc(hidden)]
pub async fn run_source_deterministic(src: &str, seed: u64) -> Result<String, AsError> {
    let src_info = Rc::new(SourceInfo {
        path: "<input>".to_string(),
        text: src.to_string(),
    });
    let tokens = lexer::lex(src).map_err(|e| e.with_source(src_info.clone()))?;
    let program = parser::parse(&tokens).map_err(|e| e.with_source(src_info.clone()))?;
    let interp = Rc::new(Interp::new());
    interp.install_self();
    interp.enter_deterministic(seed);
    let env = crate::interp::global_env().child();
    let local = tokio::task::LocalSet::new();
    let result = local
        .run_until(crate::interp::telemetry_root_scope(interp.exec(&program, &env)))
        .await;
    local.await;
    match result {
        Ok(_) | Err(crate::interp::Control::Propagate(_)) => Ok(interp.output()),
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        Err(crate::interp::Control::Exit(_)) => Ok(interp.output()),
    }
}

/// Run `src` on the tree-walker and return the captured output PLUS the owning
/// `Rc<Interp>`, so a test can read interpreter-side state after the program
/// finishes (used by the SP12 `std/telemetry` capture-mode tests, which assert on
/// `interp.telemetry_capture()`). `#[doc(hidden)]` test seam — not a public API.
#[doc(hidden)]
#[cfg(feature = "telemetry")]
pub async fn run_source_with_interp(src: &str) -> Result<(String, Rc<Interp>), AsError> {
    let src_info = Rc::new(SourceInfo {
        path: "<input>".to_string(),
        text: src.to_string(),
    });
    let tokens = lexer::lex(src).map_err(|e| e.with_source(src_info.clone()))?;
    let program = parser::parse(&tokens).map_err(|e| e.with_source(src_info.clone()))?;
    let interp = Rc::new(Interp::new());
    interp.install_self();
    let env = crate::interp::global_env().child();
    let local = tokio::task::LocalSet::new();
    // Establish the root telemetry-span scope so top-level `telemetry.span` /
    // `startSpan` parenting works (per-task isolation; spec §9.3).
    let root = crate::interp::telemetry_root_scope(interp.exec(&program, &env));
    let result = local.run_until(root).await;
    local.await;
    match result {
        Ok(_) => Ok((interp.output(), interp)),
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        Err(crate::interp::Control::Propagate(_)) => Ok((interp.output(), interp)),
        Err(crate::interp::Control::Exit(_)) => Ok((interp.output(), interp)),
    }
}

/// Compile `src` to bytecode and run it on the VM, returning the value of the
/// program's trailing expression (VM plan V1).
///
/// This is the entry point that drives the new bytecode pipeline end-to-end
/// (compile → `FnProto`/`Closure`/`Fiber` → `Vm::run`). It is exposed (behind
/// `#[doc(hidden)]`) so the differential-test harness in V1-T7 can call it from
/// an integration test. The tree-walker remains the production path.
#[doc(hidden)]
pub async fn vm_eval_source(src: &str) -> Result<crate::value::Value, AsError> {
    use crate::vm::chunk::FnProto;
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;

    let src_info = Rc::new(SourceInfo {
        path: "<input>".to_string(),
        text: src.to_string(),
    });
    let chunk = crate::compile::compile_source(src)
        .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?;
    let proto = Rc::new(FnProto {
        chunk,
        arity: 0,
        has_rest: false,
        is_async: false,
        is_generator: false,
        is_worker: false,
        owning_class: None,
        params: Vec::new(),
        ret: None,
        local_names: Vec::new(),
        debug_name: None,
    });
    let closure = Closure::new(proto);

    let interp = Rc::new(Interp::new());
    interp.install_self();
    let vm = Vm::new(interp);
    let mut fiber = crate::vm::fiber::Fiber::new(closure);

    let local = tokio::task::LocalSet::new();
    let outcome = local
        .run_until(vm.run(&mut fiber))
        .await
        .map_err(|c| control_to_aserror(c).with_source(src_info))?;
    match outcome {
        RunOutcome::Done(v) => Ok(v),
        RunOutcome::Yielded(_) => unreachable!("top-level program cannot yield"),
    }
}

/// Compile `src` to bytecode and run it on the VM for its *side effects*,
/// returning captured stdout plus any `exit(n)` code (VM plan V2).
///
/// This mirrors [`run_source_exit`] exactly for the bytecode pipeline: the shared
/// [`Interp`] uses the Capture output sink, so `print` writes into a buffer that
/// [`Interp::output`] returns. The `Control` channels map identically to the
/// tree-walker: `Panic` → `Err`, `Propagate` → end the program (return captured
/// output), `Exit(code)` → return output plus the code. It is `#[doc(hidden)]` —
/// the production path remains the tree-walker.
#[doc(hidden)]
pub async fn vm_run_source(src: &str) -> Result<(String, Option<i32>), AsError> {
    vm_run_source_with(src, true).await
}

/// Like [`vm_run_source`] but with the VM's specialization fast paths DISABLED —
/// the `--no-specialize` kill switch (V11-T5). All inline caches and PEP-659
/// adaptive sites are skipped; every dispatch takes the generic path.
///
/// This is the "generic VM" half of the THREE-WAY DIFFERENTIAL: a non-specializing
/// VM run MUST be byte-identical to both the specializing VM ([`vm_run_source`])
/// and the tree-walker ([`run_source_exit`]). If generic and specialized ever
/// diverge, a specialization guard is wrong — the safety net catches it instantly.
#[doc(hidden)]
pub async fn vm_run_source_generic(src: &str) -> Result<(String, Option<i32>), AsError> {
    vm_run_source_with(src, false).await
}

/// FUZZ `.aso` round-trip seam (`#[doc(hidden)]` test API, not a stable surface):
/// compile `src` to a [`Chunk`], serialize it to `.aso` bytes ([`vm::Chunk::to_bytes`]),
/// deserialize + verify them back ([`vm::Chunk::from_bytes_verified`]), then run the
/// reconstituted chunk on the (specializing) VM capturing output — exactly the
/// `compile → serialize → deserialize → run` pipeline `ascript build` + a fresh
/// `ascript run file.aso` exercise, but in-memory so output is captured. The FUZZ
/// `.aso`-round-trip property asserts this is byte-identical to the direct
/// [`vm_run_source`] of the same `src` (the `.aso` path must equal the in-memory VM).
/// A serialize/verify error surfaces as an [`AsError`] so the property can compare it
/// against the direct run's outcome.
#[doc(hidden)]
pub async fn aso_roundtrip_run_source(src: &str) -> Result<(String, Option<i32>), AsError> {
    use crate::vm::chunk::FnProto;
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;

    let src_info = Rc::new(SourceInfo {
        path: "<input>".to_string(),
        text: src.to_string(),
    });
    let chunk = crate::compile::compile_source(src)
        .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?;
    // Serialize → bytes → deserialize+verify (the real `.aso` round-trip).
    let bytes = chunk
        .to_bytes()
        .map_err(|e| AsError::new(format!("cannot serialize bytecode: {e}")))?;
    let chunk = crate::vm::chunk::Chunk::from_bytes_verified(&bytes)
        .map_err(|e| AsError::new(format!("cannot load .aso round-trip: {e}")))?;

    let proto = Rc::new(FnProto {
        chunk,
        arity: 0,
        has_rest: false,
        is_async: false,
        is_generator: false,
        is_worker: false,
        owning_class: None,
        params: Vec::new(),
        ret: None,
        local_names: Vec::new(),
        debug_name: None,
    });
    let closure = Closure::new(proto);
    let interp = Rc::new(Interp::new());
    interp.install_self();
    interp.set_worker_source(src);
    let vm = Vm::with_specialize(interp.clone(), true);
    let mut fiber = crate::vm::fiber::Fiber::new(closure);

    let local = tokio::task::LocalSet::new();
    let result = local.run_until(vm.run(&mut fiber)).await;
    local.await;
    crate::gc::collect();
    match result {
        Ok(RunOutcome::Done(_)) => Ok((interp.output(), None)),
        Ok(RunOutcome::Yielded(_)) => unreachable!("top-level program cannot yield"),
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        Err(crate::interp::Control::Propagate(_)) => Ok((interp.output(), None)),
        Err(crate::interp::Control::Exit(code)) => Ok((interp.output(), Some(code))),
    }
}

/// FUZZ Task 5 — the `.aso` **runnable-accept** seam (`#[doc(hidden)]` test API, not a stable
/// surface). Decode + verify arbitrary `bytes` via [`vm::Chunk::from_bytes_verified`]; if (and
/// only if) the verifier ACCEPTS them, run the reconstituted chunk on the VM to completion.
///
/// This is the in-process body of the `aso_roundtrip` libFuzzer target's bounded runnable-accept
/// (spec §2.2): a *verified* chunk that crashes the VM host is a `verify.rs` gap (a security
/// finding). Rejected bytes (`Err`) return immediately. The program's output / value / a
/// script-level `Control::Panic` are all discarded — only HOST liveness matters, so a Rust-level
/// `panic!`/abort inside `run` propagates out (the fuzzer records it; libFuzzer's `-timeout`/
/// `-rss_limit_mb` bound a hang / runaway allocation, the halting-problem bound §9). The caller
/// is responsible for wrapping this on [`run_on_worker_stack`] (the 512 MB stack) and for any
/// time budget.
///
/// Gated to the SAME `cfg` as [`fuzzgen`] (test / the non-default `fuzzgen` feature the `fuzz/`
/// crate enables / a `--cfg fuzzing` libFuzzer build) so this fuzz-support seam is NEVER compiled
/// into the pure production binary.
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
#[doc(hidden)]
pub async fn aso_runnable_accept(bytes: &[u8]) {
    use crate::vm::chunk::FnProto;
    use crate::vm::Vm;

    let chunk = match crate::vm::chunk::Chunk::from_bytes_verified(bytes) {
        Ok(c) => c,
        // A clean rejection (bad magic/version/truncation or a verify failure) — nothing to run.
        Err(_) => return,
    };
    let proto = Rc::new(FnProto {
        chunk,
        arity: 0,
        has_rest: false,
        is_async: false,
        is_generator: false,
        is_worker: false,
        owning_class: None,
        params: Vec::new(),
        ret: None,
        local_names: Vec::new(),
        debug_name: None,
    });
    let closure = crate::vm::Closure::new(proto);
    let interp = Rc::new(Interp::new());
    interp.install_self();
    let vm = Vm::new(interp.clone());
    let mut fiber = crate::vm::fiber::Fiber::new(closure);

    let local = tokio::task::LocalSet::new();
    // Discard the outcome: a clean `RunOutcome`/`Control` (including a script-level Tier-2
    // `Panic`) is fine — only a HOST panic, which propagates past this `.await`, is the finding.
    let _ = local.run_until(vm.run(&mut fiber)).await;
    local.await;
    crate::gc::collect();
}

/// Compile a `.as` source file to a verified bytecode [`Chunk`] and write it to
/// `out` as a `.aso` file (VM plan V12-T4 — `ascript build`).
///
/// Returns the path written on success. A parse/resolve/compile error surfaces as
/// an [`AsError`] (with the file's source attached for diagnostics); the `.aso` is
/// only written when compilation succeeds. The chunk is verified before writing so
/// a produced `.aso` always passes [`vm::Chunk::from_bytes_verified`].
pub fn build_file(
    file: &Path,
    out: Option<&Path>,
    with_debug: bool,
) -> Result<std::path::PathBuf, AsError> {
    let src = std::fs::read_to_string(file)
        .map_err(|e| AsError::new(format!("cannot read {}: {}", file.display(), e)))?;
    let src_info = Rc::new(SourceInfo {
        path: file.display().to_string(),
        text: src.clone(),
    });
    let chunk = crate::compile::compile_source(&src)
        .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?;
    // DBG (v26): bind the module source onto the whole proto tree so a debug build
    // serializes line/variable info. Harmless for a `--strip` build (the source is
    // simply not written). `compile_source` does not bind a source itself.
    if with_debug {
        chunk.set_module_source(&src_info);
    }
    // Defensive: verify before writing so a produced `.aso` is always loadable.
    crate::vm::verify::verify(&chunk).map_err(|e| {
        AsError::new(format!(
            "internal: produced bytecode failed verification: {e}"
        ))
        .with_source(src_info)
    })?;
    let bytes = chunk
        .to_bytes_with_debug(with_debug)
        .map_err(|e| AsError::new(format!("cannot serialize bytecode: {e}")))?;
    let out_path = match out {
        Some(p) => p.to_path_buf(),
        None => file.with_extension("aso"),
    };
    std::fs::write(&out_path, &bytes)
        .map_err(|e| AsError::new(format!("cannot write {}: {}", out_path.display(), e)))?;
    Ok(out_path)
}

/// Run a compiled `.aso` file on the VM (VM plan V12-T4). Reads the bytes, verifies
/// the header + bytecode via [`vm::Chunk::from_bytes_verified`] (a version mismatch
/// or verify failure becomes a clear [`AsError`]), then runs its top-level on the
/// VM — NO compile step. Relative file imports resolve against the `.aso`'s parent
/// directory. Returns the process exit code, mirroring [`run_file`].
pub async fn run_aso_file(
    path: &Path,
    script_args: &[String],
    caps: Option<crate::stdlib::caps::CapSet>,
) -> Result<i32, AsError> {
    let bytes = std::fs::read(path)
        .map_err(|e| AsError::new(format!("cannot read {}: {}", path.display(), e)))?;
    let module_dir = path.parent().map(|d| d.to_path_buf());
    run_verified_aso(&bytes, script_args, caps, module_dir, &path.display().to_string()).await
}

/// BIN §2.4 — run an embedded (bundled) `.aso` payload. The startup shim
/// ([`try_run_embedded`]) calls this with the payload sliced out of `current_exe()` and the
/// program's argv (minus argv[0]). It runs through the SAME verified path as
/// [`run_aso_file`] (`from_bytes_verified` → `Vm`); relative imports resolve against the
/// executable's directory. Caps default to all-granted (a bundled program is a normal
/// launch — there is no CLI surface to deny on once embedded).
pub async fn run_embedded_aso(payload: &[u8], args: &[String]) -> Result<i32, AsError> {
    let module_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    run_verified_aso(payload, args, None, module_dir, "the embedded program").await
}

/// The shared verified-run body behind [`run_aso_file`] and [`run_embedded_aso`] (BIN Task
/// 2): `Chunk::from_bytes_verified` (the single trust boundary) → `Interp` setup →
/// `set_worker_aso_bytes` → `Vm` → `LocalSet` run → telemetry flush → GC → the
/// `RunOutcome`/`Control` exit-code map. `what` labels the load-error; `module_dir` resolves
/// relative imports. Borrow discipline mirrors the original (no `RefCell`/resource borrow
/// held across `.await` — Gate 4).
async fn run_verified_aso(
    payload: &[u8],
    script_args: &[String],
    caps: Option<crate::stdlib::caps::CapSet>,
    module_dir: Option<std::path::PathBuf>,
    what: &str,
) -> Result<i32, AsError> {
    use crate::vm::chunk::FnProto;
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;

    let chunk = crate::vm::chunk::Chunk::from_bytes_verified(payload)
        .map_err(|e| AsError::new(format!("cannot load {what}: {e}")))?;

    let interp = Rc::new(Interp::new_live());
    interp.set_cli_args(script_args);
    // FFI §4.5: install the composed capability set before running any code.
    if let Some(caps) = caps {
        interp.set_caps(caps);
    }
    // Workers Spec A (.aso path): retain the raw bytes so `dispatch_worker_closure` can
    // re-parse them into the top-level chunk and build a worker code slice without source.
    interp.set_worker_aso_bytes(Rc::from(payload));
    interp.install_self();
    let vm = Vm::new(interp.clone());
    // Resolve relative imports against the .aso's (or the executable's) directory.
    if let Some(dir) = module_dir {
        vm.set_module_dir(dir);
    }

    let proto = Rc::new(FnProto {
        chunk,
        arity: 0,
        has_rest: false,
        is_async: false,
        is_generator: false,
        is_worker: false,
        owning_class: None,
        params: Vec::new(),
        ret: None,
        local_names: Vec::new(),
        debug_name: None,
    });
    let closure = Closure::new(proto);
    let mut fiber = crate::vm::fiber::Fiber::new(closure);

    let local = tokio::task::LocalSet::new();
    let result = local
        .run_until(crate::interp::telemetry_root_scope(vm.run(&mut fiber)))
        .await;
    // SP12: flush any buffered telemetry on the existing shutdown path (spec §2).
    local.run_until(interp.telemetry_flush_on_exit()).await;
    local.await; // drain spawned tasks
                 // End-of-program cycle collection (V13-T3): reclaim any leftover
                 // reference cycles for a clean shutdown. The fiber's stack has been
                 // consumed by `run`, so this sweeps genuinely-dead cyclic garbage
                 // only — it cannot affect output (already emitted) or live data.
    crate::gc::collect();
    match result {
        Ok(RunOutcome::Done(_)) => Ok(0),
        Ok(RunOutcome::Yielded(_)) => unreachable!("top-level program cannot yield"),
        Err(crate::interp::Control::Panic(e)) => Err(e),
        Err(crate::interp::Control::Propagate(_)) => Ok(0),
        Err(crate::interp::Control::Exit(code)) => Ok(code),
    }
}

/// Run a `.as` source file on the bytecode VM (the production `run` path).
///
/// This mirrors [`run_aso_file`] exactly EXCEPT the [`vm::Chunk`] comes from
/// compiling the source ([`compile::compile_source`]) instead of deserializing a
/// `.aso`. Output streams live (`OutputSink::Live`); CLI args are forwarded; and
/// relative file imports resolve against the file's parent directory. Returns the
/// process exit code, mirroring [`run_file`]/[`run_aso_file`].
///
/// Unlike [`run_aso_file`] (whose chunk carries spans into the original `.aso`
/// build source), the source is attached to a Tier-2 panic here so its diagnostic
/// renders against the file the user just ran.
pub async fn run_file_on_vm(path: &Path, script_args: &[String]) -> Result<i32, AsError> {
    run_file_on_vm_with_packages(path, script_args, None, None).await
}

/// DX D4 §5.1: collect ALL recoverable parse diagnostics for a `.as` source file
/// (grammar + lexical), each as an [`AsError`] with the file's source bound for
/// caret rendering. The error-tolerant CST parser records every error and recovers
/// into a best-effort tree, so the run path can show them ALL at once (via
/// [`diagnostics::report_all`]) instead of bailing on the first the compiler hits.
/// An empty `Vec` means the parse was clean (run proceeds normally). `SyntaxError`
/// offsets are BYTE offsets (summed `t.text.len()` in `all_syntax_errors_in`), so
/// they are converted to CHAR offsets here — AScript `Span`s are CHAR offsets (the
/// renderer feeds them straight to char-mode ariadne).
pub fn collect_parse_errors(path: &Path) -> Vec<AsError> {
    let Ok(src) = std::fs::read_to_string(path) else {
        return Vec::new(); // a read error is handled by the runner's own report
    };
    let src_info = Rc::new(SourceInfo {
        path: path.display().to_string(),
        text: src.clone(),
    });
    let parsed = crate::syntax::parser::parse(&src);
    crate::syntax::all_syntax_errors_in(&parsed)
        .into_iter()
        .map(|e| {
            let span = crate::span::Span::new(
                byte_to_char_offset(&src, e.start),
                byte_to_char_offset(&src, e.end),
            );
            AsError::at(e.message, span).with_source(src_info.clone())
        })
        .collect()
}

/// Convert a BYTE offset into a CHAR offset within `src` (clamping a mid-codepoint
/// byte down to the largest char boundary `<= byte`). A small CORE copy of the
/// LSP's `byte_to_char` (which is feature-gated), used where a byte-native syntax
/// error must be rendered as a char `Span`.
fn byte_to_char_offset(src: &str, byte: usize) -> usize {
    let mut b = byte.min(src.len());
    while b > 0 && !src.is_char_boundary(b) {
        b -= 1;
    }
    src[..b].chars().count()
}

/// Like [`run_file_on_vm`] (VM) but installs a CLI-resolved package map (SP6)
/// before running, so a bare `import "pkg"` resolves through it. `None` = no
/// package resolver (every bare specifier is "unknown package"). `caps` (FFI §4.5)
/// is the CLI/manifest-composed initial capability set; `None` = all granted
/// (byte-identical default).
pub async fn run_file_on_vm_with_packages(
    path: &Path,
    script_args: &[String],
    packages: Option<crate::interp::PackageMap>,
    caps: Option<crate::stdlib::caps::CapSet>,
) -> Result<i32, AsError> {
    use crate::vm::chunk::FnProto;
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;

    let src = std::fs::read_to_string(path)
        .map_err(|e| AsError::new(format!("cannot read {}: {}", path.display(), e)))?;
    let src_info = Rc::new(SourceInfo {
        path: path.display().to_string(),
        text: src.clone(),
    });
    let chunk = crate::compile::compile_source(&src)
        .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?;
    // Bind the entry module's source onto its whole proto tree (SP4 §3) so a
    // panic raised in any of its functions renders its caret in this file even
    // when the error propagates up from a different module's call site.
    chunk.set_module_source(&src_info);

    let interp = Rc::new(Interp::new_live());
    interp.set_cli_args(script_args);
    // FFI §4.5: install the CLI/manifest-composed capability set (most-restrictive
    // -wins) BEFORE running any code. `None` → the default all-granted set is kept.
    if let Some(caps) = caps {
        interp.set_caps(caps);
    }
    // Workers Spec A: retain the source so a `worker fn` call can build its slice.
    interp.set_worker_source(&src);
    if let Some(map) = packages {
        interp.set_package_resolver(map);
    }
    interp.install_self();
    // VAL Task 4 (Gate 12 bench seam): `ASCRIPT_NO_SPECIALIZE=1` runs the CLI on
    // the GENERIC VM (every IC / adaptive-arith / global fast path skipped),
    // exactly as `vm_run_source_generic` does in tests. The two modes are asserted
    // byte-identical by the three-way differential, so this is a pure
    // measurement/debug seam — it changes speed, never observable behavior. Absent
    // or any non-"1" value keeps the default specialized VM (byte-identical default).
    let specialize = std::env::var("ASCRIPT_NO_SPECIALIZE").as_deref() != Ok("1");
    let vm = Vm::with_specialize(interp.clone(), specialize);
    // Resolve relative imports against the source file's directory.
    if let Some(dir) = path.parent() {
        vm.set_module_dir(dir.to_path_buf());
    }

    let proto = Rc::new(FnProto {
        chunk,
        arity: 0,
        has_rest: false,
        is_async: false,
        is_generator: false,
        is_worker: false,
        owning_class: None,
        params: Vec::new(),
        ret: None,
        local_names: Vec::new(),
        debug_name: None,
    });
    let closure = Closure::new(proto);
    let mut fiber = crate::vm::fiber::Fiber::new(closure);

    let local = tokio::task::LocalSet::new();
    let result = local
        .run_until(crate::interp::telemetry_root_scope(vm.run(&mut fiber)))
        .await;
    // SP12: flush any buffered telemetry on the existing shutdown path (spec §2).
    local.run_until(interp.telemetry_flush_on_exit()).await;
    local.await; // drain spawned tasks (structured join)
                 // End-of-program cycle collection (V13-T3): see `run_aso_file`.
    crate::gc::collect();
    match result {
        Ok(RunOutcome::Done(_)) => Ok(0),
        Ok(RunOutcome::Yielded(_)) => unreachable!("top-level program cannot yield"),
        // Attach the source so the panic's diagnostic points at this file.
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        Err(crate::interp::Control::Propagate(_)) => Ok(0),
        Err(crate::interp::Control::Exit(code)) => Ok(code),
    }
}

/// DBG Task 7: configuration for a CPU-profiled run, assembled from the
/// `--profile`/`--profile-hz`/`--profile-format`/`-o` CLI flags.
#[cfg(feature = "profile")]
pub struct ProfileConfig {
    /// Wall-clock sampling, or the deterministic (call-structure-driven) sample clock.
    pub mode: crate::vm::instrument::ProfileMode,
    /// The wall-clock sampling interval (ignored in deterministic mode).
    pub interval: std::time::Duration,
    /// The output artifact format.
    pub format: crate::profile::ProfileFormat,
    /// The output file path.
    pub out: std::path::PathBuf,
}

/// DBG Task 7: run a `.as` file on the VM under the CPU sampling profiler, then write
/// the aggregated profile (speedscope JSON or collapsed text) to `cfg.out`.
///
/// Behaviorally this is [`run_file_on_vm_with_packages`] with a [`ProfilerHook`] armed
/// on the VM via [`Vm::with_instrument`] — profiling is OBSERVATION-ONLY, so the
/// program's stdout/behavior/exit code are byte-identical to a non-profiled run (Gate
/// 9). On completion the sampler is stopped, the samples aggregated, and the file
/// written. Returns the same process exit code as the unprofiled path.
///
/// [`ProfilerHook`]: crate::vm::instrument::ProfilerHook
/// [`Vm::with_instrument`]: crate::vm::Vm::with_instrument
#[cfg(feature = "profile")]
pub async fn run_file_on_vm_profiled(
    path: &Path,
    script_args: &[String],
    packages: Option<crate::interp::PackageMap>,
    caps: Option<crate::stdlib::caps::CapSet>,
    cfg: ProfileConfig,
) -> Result<i32, AsError> {
    use crate::vm::chunk::FnProto;
    use crate::vm::instrument::{Instrumentation, ProfilerHook};
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;

    let src = std::fs::read_to_string(path)
        .map_err(|e| AsError::new(format!("cannot read {}: {}", path.display(), e)))?;
    let src_info = Rc::new(SourceInfo {
        path: path.display().to_string(),
        text: src.clone(),
    });
    let chunk = crate::compile::compile_source(&src)
        .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?;
    chunk.set_module_source(&src_info);

    let interp = Rc::new(Interp::new_live());
    interp.set_cli_args(script_args);
    if let Some(caps) = caps {
        interp.set_caps(caps);
    }
    interp.set_worker_source(&src);
    if let Some(map) = packages {
        interp.set_package_resolver(map);
    }
    interp.install_self();

    // Arm the profiler hook. In wallclock mode the sampler thread starts now (so it is
    // already sampling when the run begins); deterministic mode collects inline.
    let mut hook = ProfilerHook::new(cfg.mode, cfg.interval);
    hook.start();
    let vm = Vm::with_instrument(
        interp.clone(),
        Instrumentation {
            breakpoints: None,
            profiler: Some(hook),
            coverage: None,
        },
    );
    if let Some(dir) = path.parent() {
        vm.set_module_dir(dir.to_path_buf());
    }

    let proto = Rc::new(FnProto {
        chunk,
        arity: 0,
        has_rest: false,
        is_async: false,
        is_generator: false,
        is_worker: false,
        owning_class: None,
        params: Vec::new(),
        ret: None,
        local_names: Vec::new(),
        debug_name: None,
    });
    let closure = Closure::new(proto);
    let mut fiber = crate::vm::fiber::Fiber::new(closure);

    let local = tokio::task::LocalSet::new();
    let result = local
        .run_until(crate::interp::telemetry_root_scope(vm.run(&mut fiber)))
        .await;
    local.run_until(interp.telemetry_flush_on_exit()).await;
    local.await;
    crate::gc::collect();

    // Stop the sampler (joins the thread in wallclock mode) and aggregate. Reclaim the
    // hook out of the VM's instrumentation seam.
    let samples = match vm.take_profiler() {
        Some(hook) => hook.finish(),
        None => Vec::new(),
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("program")
        .to_string();
    let rendered = crate::profile::format_samples(&samples, cfg.format, &name);
    std::fs::write(&cfg.out, rendered).map_err(|e| {
        AsError::new(format!(
            "cannot write profile to {}: {}",
            cfg.out.display(),
            e
        ))
    })?;

    match result {
        Ok(RunOutcome::Done(_)) => Ok(0),
        Ok(RunOutcome::Yielded(_)) => unreachable!("top-level program cannot yield"),
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        Err(crate::interp::Control::Propagate(_)) => Ok(0),
        Err(crate::interp::Control::Exit(code)) => Ok(code),
    }
}

/// Shared body for [`vm_run_source`] (specialize = true) and
/// [`vm_run_source_generic`] (specialize = false). `specialize` is the kill-switch
/// flag threaded onto the [`Vm`]; the eventual CLI's `--no-specialize` maps to
/// `specialize = false` here.
async fn vm_run_source_with(src: &str, specialize: bool) -> Result<(String, Option<i32>), AsError> {
    vm_run_source_cfg(src, specialize, false, false).await
}

/// DBG Task 9 (zero-cost bench): run `src` on the SPECIALIZED VM with an EMPTY
/// [`Instrumentation`](crate::vm::instrument::Instrumentation) armed (`breakpoints`/
/// `profiler`/`coverage` all `None`) — the "attached debugger, idle" config. No byte is
/// patched (so the `Op::Break` trap arm is never reached) and the profiler is off, so
/// this exercises ONLY the `Vm.instrument == Some` overhead (the per-call push/pop
/// profiler None-check sees `Some`). The zero-cost gate asserts this is within timing
/// noise of [`vm_run_source`] (`instrument == None`). `#[doc(hidden)]` test API.
#[doc(hidden)]
pub async fn vm_run_source_armed_idle(src: &str) -> Result<(String, Option<i32>), AsError> {
    vm_run_source_cfg(src, true, true, false).await
}

/// DX D2 Task 7 (coverage zero-cost bench): run `src` on the SPECIALIZED VM with LINE
/// COVERAGE armed (`arm_coverage` patches each line's first offset to `Op::Break`). This
/// is bench config (3) — `--coverage` ON. Its overhead is REPORTED (not gated): each line
/// traps at most ONCE (then un-patches + runs free), so for a compute-bound loop the cost
/// is amortized and the steady state matches `vm_run_source` — the demonstration that the
/// patch-based design keeps coverage cheap. (Config (2), coverage-OFF == byte-identical to
/// baseline, is proven by [`vm_run_source_armed_idle`]'s gate: the instrument seam is
/// `None`-gated and the hot loop is untouched.) `#[doc(hidden)]` test API.
#[doc(hidden)]
pub async fn vm_run_source_coverage(src: &str) -> Result<(String, Option<i32>), AsError> {
    vm_run_source_cfg(src, true, false, true).await
}

async fn vm_run_source_cfg(
    src: &str,
    specialize: bool,
    armed: bool,
    coverage: bool,
) -> Result<(String, Option<i32>), AsError> {
    use crate::vm::chunk::FnProto;
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;

    let src_info = Rc::new(SourceInfo {
        path: "<input>".to_string(),
        text: src.to_string(),
    });
    let chunk = crate::compile::compile_source(src)
        .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?;
    let proto = Rc::new(FnProto {
        chunk,
        arity: 0,
        has_rest: false,
        is_async: false,
        is_generator: false,
        is_worker: false,
        owning_class: None,
        params: Vec::new(),
        ret: None,
        local_names: Vec::new(),
        debug_name: None,
    });
    let closure = Closure::new(proto.clone());

    let interp = Rc::new(Interp::new());
    interp.install_self();
    // Workers Spec A: retain the source so a `worker fn` call can build its slice.
    interp.set_worker_source(src);
    // The instrumentation config (DBG Task 9 / DX D2 Task 7 — the zero-cost bench seam):
    //   `coverage` → an armed CoverageTable + `arm_coverage` (config 3: --coverage on);
    //   `armed`    → an EMPTY instrumentation payload (the attached-but-idle config);
    //   else       → `instrument == None` (the production path).
    let vm = if coverage {
        let mut inst = crate::vm::instrument::Instrumentation::empty();
        inst.coverage = Some(crate::vm::instrument::CoverageTable::new());
        let vm = Vm::with_instrument(interp.clone(), inst);
        vm.arm_coverage(&proto);
        vm
    } else if armed {
        Vm::with_instrument(interp.clone(), crate::vm::instrument::Instrumentation::empty())
    } else {
        Vm::with_specialize(interp.clone(), specialize)
    };
    let mut fiber = crate::vm::fiber::Fiber::new(closure);

    let local = tokio::task::LocalSet::new();
    let result = local.run_until(vm.run(&mut fiber)).await;
    local.await; // drain spawned tasks — no-op until later VM slices
                 // End-of-program cycle collection (V13-T3): see `run_aso_file`. The
                 // output is already captured on `interp`, so a final sweep of dead
                 // cycles is observably invisible.
    crate::gc::collect();
    match result {
        Ok(RunOutcome::Done(_)) => Ok((interp.output(), None)),
        Ok(RunOutcome::Yielded(_)) => unreachable!("top-level program cannot yield"),
        // A panic aborts the program with its diagnostic.
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        // A top-level `?` propagation simply ends the program.
        Err(crate::interp::Control::Propagate(_)) => Ok((interp.output(), None)),
        // exit(n) — return the captured output plus the exit code.
        Err(crate::interp::Control::Exit(code)) => Ok((interp.output(), Some(code))),
    }
}

/// Map a VM [`crate::interp::Control`] outcome to an [`AsError`], mirroring how
/// the tree-walker entry points treat each channel.
fn control_to_aserror(c: crate::interp::Control) -> AsError {
    match c {
        crate::interp::Control::Panic(e) => e,
        crate::interp::Control::Propagate(_) => {
            AsError::new("unexpected '?' propagation at top level")
        }
        crate::interp::Control::Exit(code) => AsError::new(format!("exit({code}) at top level")),
    }
}
