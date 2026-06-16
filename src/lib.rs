pub mod ast;
pub mod bundle;
pub mod check;
/// The clap derive types (`Cli`, `Command`, `CapFlags`) + `cli_command()` —
/// the single source of truth for the CLI surface. Consumed by `src/main.rs`
/// for parsing and by `tests/docs_drift.rs` for drift introspection (spec §4.1).
pub mod cli_surface;
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
pub mod elide_mark;
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

/// **ELIDE §5 — the measured default for contract elision on the `run`/`build`/
/// `test` SOURCE paths.**
///
/// The §5.1 DECISION (recorded with numbers in `bench/ELIDE_RESULTS.md` and the
/// spec): the per-module collector cost is small in absolute terms (median 0.13 ms,
/// and a real compute-bound A/B is a WIN — typed `call_heavy` −6%, untyped ≈0), BUT
/// running it on EVERY source invocation fails BOTH §5.1 budget criteria as
/// measured on the example corpus:
/// - **≤ 2% corpus geomean:** measured **+6.99%** (68-file runnable example corpus,
///   on/off, release) — the demo corpus is dominated by ms-runtime programs where a
///   one-shot parse+resolve+infer collector pass is a large startup fraction.
/// - **≤ 1 ms collector for a ≤500-line module:** the 266-line `all_features.as`
///   already costs **1.42 ms** — over the absolute budget below 300 lines.
///
/// Per the spec's honesty mandate (plan Task 4.1 Step 2: "outside → default OFF …
/// not a failure"), elision therefore ships **default-OFF**, opt-in via `--elide` /
/// `ASCRIPT_ELIDE=1`. `ascript build` is the natural elide-on surface (a one-shot
/// compile whose cost is amortised over every later run of the durable `.aso`).
///
/// Flip this one constant to make elision default-on (e.g. once the collector shares
/// the compiler's parse+resolve, closing the startup gap). The kill-switch / opt-out
/// path stays byte-identical to pre-ELIDE regardless.
pub const ELIDE_DEFAULT_ON: bool = false;

/// **ELIDE §5.2 — resolve whether contract elision is active for a source run.**
///
/// Precedence (most-specific wins, force-OFF beats opt-ON — the `--no-specialize`
/// discipline):
/// 1. `--no-elide` flag OR `ASCRIPT_NO_ELIDE=1` → **OFF** (explicit force-off).
/// 2. `--elide` flag OR `ASCRIPT_ELIDE=1` → **ON** (the opt-in, since default-off).
/// 3. otherwise → the measured [`ELIDE_DEFAULT_ON`] decision.
///
/// When this returns `false`, the collector never runs, the compiler is fed `None`,
/// and the marker never marks — output bytecode and AST are byte-identical to
/// pre-ELIDE (the zero-cost-when-off contract). When `true`, output is still
/// byte-IDENTICAL behaviorally (elision is invisible); only proven checks are dropped.
pub fn elide_enabled(elide_flag: bool, no_elide_flag: bool) -> bool {
    // Force-off wins (defensive: an explicit kill switch always overrides opt-in).
    if no_elide_flag || std::env::var("ASCRIPT_NO_ELIDE").as_deref() == Ok("1") {
        return false;
    }
    if elide_flag || std::env::var("ASCRIPT_ELIDE").as_deref() == Ok("1") {
        return true;
    }
    ELIDE_DEFAULT_ON
}

/// **ELIDE §6.3 — resolve whether paranoid proof-violation mode is active.**
///
/// When `true`, the runtime retains the per-module [`ElisionSet`] and escalates
/// any contract failure at a proven site to a `ELIDE proof violated …` panic
/// (a checker soundness bug). OFF by default; opt-in via `ASCRIPT_ELIDE_PARANOID=1`.
/// Note: paranoid mode compiles/marks as elide-OFF (full checks retained) —
/// the ElisionSet is used only on the failure path, zero hot-path cost.
pub fn paranoid_enabled() -> bool {
    std::env::var("ASCRIPT_ELIDE_PARANOID").as_deref() == Ok("1")
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
    run_file_with_packages(path, script_args, None, None, ELIDE_DEFAULT_ON).await
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
    elide: bool,
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
    // ELIDE §4.3/§5: enable the per-module AST marking pass on the tree-walker.
    // `load_module` runs `elision_proofs` + `mark_program` per module (entry AND
    // imports flow through it), so per-module scoping is automatic. Off → the
    // markers never run, byte-identical to pre-ELIDE.
    interp.set_elide_mode(elide);
    // ELIDE §6.3 paranoid mode: when active, build the ElisionSet from the source
    // and install it for contract-failure-path lookup. Runs elide-OFF (no marking),
    // so all contract checks are retained — the set is used ONLY when a check fails.
    // NOTE: for multi-module programs, the CLI paranoid mode only covers the entry
    // module (imported modules get separate passes via load_module). This is sufficient
    // for correctness-gate purposes — the corpus test confirms zero escalations.
    if paranoid_enabled() {
        if let Ok(src) = std::fs::read_to_string(path) {
            let paranoid_set = crate::check::infer::elision_proofs(&src);
            interp.set_paranoid_set(paranoid_set);
        }
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
    run_tests_with_options(files, packages, caps, None, false, None, ELIDE_DEFAULT_ON).await
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
    elide: bool,
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
            // ELIDE §4.6: the parallel path dispatches each FILE to its own worker
            // ISOLATE; worker slices NEVER elide (full checks in the isolate), so the
            // `elide` decision is intentionally NOT propagated across the airlock here.
            let _ = elide;
            run_tests_parallel(files, packages, caps, n, update_snapshots, filter).await
        }
        _ => run_tests_serial(files, packages, caps, update_snapshots, filter, elide).await,
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
        name_span: None,
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
    elide: bool,
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
    // ELIDE §4.3/§5: enable the per-module marking pass for the serial test run
    // (each test FILE flows through `load_module`, which marks per-module). Off →
    // byte-identical to pre-ELIDE.
    interp.set_elide_mode(elide);
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
    // DEFER §2.3: use `exec_program` (installs + drains the top-level defer frame)
    // so top-level `defer` statements run at program end.
    let result = local
        .run_until(crate::interp::telemetry_root_scope(
            interp.exec_program(&program, &env),
        ))
        .await;
    local.await; // drain spawned tasks — no-op until Phase 2
    match result {
        // Any `Ok(Flow)` is normal program completion. `exec_program` converts a
        // top-level `break`/`continue` into `Err(Control::Panic("… outside of a
        // loop"))` before returning, so the `Flow::Break`/`Flow::Continue` variants
        // never reach here (no separate arms needed).
        Ok(_) => Ok((interp.output(), None)),
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
    // DEFER §2.3: exec_program installs the top-level defer frame.
    let result = local
        .run_until(crate::interp::telemetry_root_scope(
            interp.exec_program(&program, &env),
        ))
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
    // DEFER §2.3: exec_program installs the top-level defer frame.
    let root = crate::interp::telemetry_root_scope(interp.exec_program(&program, &env));
    let result = local.run_until(root).await;
    local.await;
    match result {
        Ok(_) => Ok((interp.output(), interp)),
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        Err(crate::interp::Control::Propagate(_)) => Ok((interp.output(), interp)),
        Err(crate::interp::Control::Exit(_)) => Ok((interp.output(), interp)),
    }
}

/// ELIDE §4.3 test seam: run a pre-parsed (and possibly AST-mutated) `Vec<Stmt>`
/// on the tree-walker. Returns `Ok(output)` on success or `Err(panic_message)`
/// on a Tier-2 panic. Used by `tests/elide.rs` Task 3.1 to inject `elide_args =
/// true` on `ExprKind::Call` nodes before execution — the marking pass sets the
/// flag in production; this seam lets tests do it surgically without needing access
/// to `pub(crate)` internals.
///
/// `#[doc(hidden)]` test seam — not a public API.
#[doc(hidden)]
pub async fn tw_run_stmts(
    stmts: Vec<crate::ast::Stmt>,
) -> Result<String, String> {
    let interp = Rc::new(Interp::new());
    interp.install_self();
    let env = crate::interp::global_env().child();
    let local = tokio::task::LocalSet::new();
    let result = local
        .run_until(crate::interp::telemetry_root_scope(
            interp.exec_program(&stmts, &env),
        ))
        .await;
    local.await;
    match result {
        Ok(_) | Err(crate::interp::Control::Propagate(_)) | Err(crate::interp::Control::Exit(_)) => {
            Ok(interp.output())
        }
        Err(crate::interp::Control::Panic(e)) => Err(e.message),
    }
}

/// ELIDE §4.3 Task 3.2 test seam: parse `src`, run `elision_proofs` to build the
/// `ElisionSet`, call `mark_program` on the AST, then execute on the tree-walker.
/// Returns `(output, MarkCounts)` on success or `Err(panic_message)` on Tier-2
/// panic. The counts let callers assert count parity against `vm_run_source_elided`
/// and the raw `ElisionSet::len()`.
///
/// `#[doc(hidden)]` test seam — not a public API.
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
#[doc(hidden)]
pub async fn tw_run_source_elided(src: &str) -> Result<(String, crate::elide_mark::MarkCounts), String> {
    let tokens = match lexer::lex(src) {
        Ok(t) => t,
        Err(e) => return Err(e.message),
    };
    let mut program = match parser::parse(&tokens) {
        Ok(p) => p,
        Err(e) => return Err(e.message),
    };
    let set = crate::check::infer::elision_proofs(src);
    let counts = crate::elide_mark::mark_program(&mut program, &set);
    let interp = Rc::new(Interp::new());
    interp.install_self();
    let env = crate::interp::global_env().child();
    let local = tokio::task::LocalSet::new();
    let result = local
        .run_until(crate::interp::telemetry_root_scope(
            interp.exec_program(&program, &env),
        ))
        .await;
    local.await;
    match result {
        Ok(_) | Err(crate::interp::Control::Propagate(_)) | Err(crate::interp::Control::Exit(_)) => {
            Ok((interp.output(), counts))
        }
        Err(crate::interp::Control::Panic(e)) => Err(e.message),
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
        name_span: None,
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

/// Like [`vm_run_source`] but with the LANE sync driver DISABLED — the
/// `ASCRIPT_NO_SYNC_LANE=1` kill switch (LANE §6.1). Every instruction takes the
/// async driver path regardless of whether it is in the sync subset. Observable
/// behavior is byte-identical to [`vm_run_source`]; only throughput differs.
///
/// Used by the differential test (`no_sync_lane_entry_point_runs_byte_identically`)
/// and any future lane-correctness checks. `#[doc(hidden)]` — not a stable API.
#[doc(hidden)]
pub async fn vm_run_source_no_sync_lane(src: &str) -> Result<(String, Option<i32>), AsError> {
    vm_run_source_cfg(src, true, false, false, false).await
}

/// Like [`vm_run_source`] but with the CALL fast paths DISABLED — the
/// `ASCRIPT_NO_CALL_FAST=1` kill switch (CALL §8.1). All CALL fast paths
/// (A2 in-place binding, A3 fiber pooling, B trampoline) are suppressed;
/// specialization and the sync lane remain active so this isolates a CALL
/// divergence from an IC/adaptive or lane divergence. Observable behavior is
/// byte-identical to [`vm_run_source`]; only throughput differs.
///
/// Used by the differential test (`no_call_fast_mode_runs_byte_identically`)
/// and the fifth differential mode. `#[doc(hidden)]` — not a stable API.
#[doc(hidden)]
pub async fn vm_run_source_no_call_fast(src: &str) -> Result<(String, Option<i32>), AsError> {
    // specialize=true, armed=false, coverage=false, sync_lane=true, call_fast=false
    vm_run_source_cfg_call_fast(src, true, false, false, true, false).await
}

/// CALL §8.3: like [`vm_run_source`] but also returns the `CallFastStats`
/// counters after the run completes. Used by `tests/call_fast.rs` to assert
/// that A2 (`inplace_binds > 0`) actually fires over the corpus (anti-false-green
/// gate). `#[doc(hidden)]` — not a stable API.
#[doc(hidden)]
pub async fn vm_run_source_call_fast_stats(
    src: &str,
) -> Result<(String, Option<i32>, crate::vm::run::CallFastStats), AsError> {
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
        name_span: None,
    });
    let closure = Closure::new(proto);
    let interp = Rc::new(Interp::new());
    interp.install_self();
    interp.set_worker_source(src);
    // specialize=true, sync_lane=true, call_fast=true (the production call_fast path).
    let vm = Vm::with_flags(interp.clone(), true, true, true);
    let mut fiber = crate::vm::fiber::Fiber::new(closure);
    let local = tokio::task::LocalSet::new();
    let result = local.run_until(vm.run(&mut fiber)).await;
    local.await;
    crate::gc::collect();
    let stats = vm.call_fast_stats();
    let pair = match result {
        Ok(RunOutcome::Done(_)) => Ok((interp.output(), None)),
        Ok(RunOutcome::Yielded(_)) => unreachable!("top-level program cannot yield"),
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        Err(crate::interp::Control::Propagate(_)) => Ok((interp.output(), None)),
        Err(crate::interp::Control::Exit(code)) => Ok((interp.output(), Some(code))),
    }?;
    Ok((pair.0, pair.1, stats))
}

/// SHAPE §3.5: like [`vm_run_source`] but also returns the storage-mode counters
/// `(slab_constructed, dict_constructed, demotions)` after the run completes.
/// Used by `tests/vm_differential.rs` to assert Gate 15 (both modes + demotion
/// exercised over the corpus — anti-false-green).  `#[doc(hidden)]` — not a
/// stable API.  Compiled only under `test`/`fuzzgen`/`fuzzing`.
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
#[doc(hidden)]
pub async fn vm_run_source_obj_mode_stats(
    src: &str,
) -> Result<(String, Option<i32>, (u64, u64, u64)), AsError> {
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
        name_span: None,
    });
    let closure = Closure::new(proto);
    let interp = Rc::new(Interp::new());
    interp.install_self();
    interp.set_worker_source(src);
    // specialize=true, sync_lane=true, call_fast=true — the production path so
    // the slab warm-hit path (`exec_new_object` lit_shapes) actually fires.
    let vm = Vm::with_flags(interp.clone(), true, true, true);
    let mut fiber = crate::vm::fiber::Fiber::new(closure);
    let local = tokio::task::LocalSet::new();
    let result = local.run_until(vm.run(&mut fiber)).await;
    local.await;
    crate::gc::collect();
    let mode_stats = vm.obj_mode_stats();
    let pair = match result {
        Ok(RunOutcome::Done(_)) => Ok((interp.output(), None)),
        Ok(RunOutcome::Yielded(_)) => unreachable!("top-level program cannot yield"),
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        Err(crate::interp::Control::Propagate(_)) => Ok((interp.output(), None)),
        Err(crate::interp::Control::Exit(code)) => Ok((interp.output(), Some(code))),
    }?;
    Ok((pair.0, pair.1, mode_stats))
}

/// Like [`vm_run_source`] but also returns the LANE counters (LANE §6.4).
///
/// Returns `(output, exit_code, lane_sync_ops, lane_bursts)`.
///
/// `lane_sync_ops` = total bytecode instructions retired inside the sync lane;
/// `lane_bursts`   = number of times the sync driver was entered.
///
/// Both counters are 0 until Task 4 wires up the burst driver. Once wired, the
/// coverage assertion suite uses this to prove the lane is actually running
/// (`sync_ops > 0` on the corpus, `≥ 1_000_000` on a tight loop). `#[doc(hidden)]`
/// — not a stable API.
#[doc(hidden)]
pub async fn vm_run_source_lane_stats(
    src: &str,
) -> Result<(String, Option<i32>, u64, u64), AsError> {
    vm_run_source_cfg_stats(src, true, false, false, true, true).await
}

/// Like [`vm_run_source_lane_stats`] but with `sync_lane = false` (LANE §6.4).
///
/// Allows the coverage assertion to check that `sync_lane == false` ⟹
/// `lane_sync_ops == 0` even after Task 4 wires up the driver. `#[doc(hidden)]`.
#[doc(hidden)]
pub async fn vm_run_source_lane_stats_no_lane(
    src: &str,
) -> Result<(String, Option<i32>, u64, u64), AsError> {
    vm_run_source_cfg_stats(src, true, false, false, false, true).await
}

// ─────────────────────────────────────────────────────────────────────────────
// DECODE Task 2 — public wrapper struct + five test entry points
// ─────────────────────────────────────────────────────────────────────────────

/// **DECODE §8.3 — per-run stat bundle returned by the decode-stats test entries.**
///
/// Wired counters:
/// - RecordSource driver (Unit A): `decoded_ops`, `decoded_bytes`, `stack_ops`
/// - Unit B fusion:                `fused_ops`
/// - `inline_hits`/`inline_misses` (Unit C) and `tos_ops` (Unit D) stay 0 —
///   those units were EVIDENCE-DROPPED, so the counters are permanently inert.
///
/// The `output` and `exit_code` fields carry the program's normal result so the
/// test can assert correctness while also inspecting the counters.
///
/// `#[doc(hidden)]` — test API only; not a stable public surface.
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct DecodeStats {
    /// Captured stdout of the program run.
    pub output: String,
    /// Process exit code, or `None` for a normal return.
    pub exit_code: Option<i32>,
    /// Total records retired by the `RecordSource` driver.
    pub decoded_ops: u64,
    /// Fused superinstruction records retired (Unit B).
    pub fused_ops: u64,
    /// INERT (Unit C evidence-dropped) — permanently 0.
    pub inline_hits: u64,
    /// INERT (Unit C evidence-dropped) — permanently 0.
    pub inline_misses: u64,
    /// Total bytes of decoded record streams resident in memory at end-of-run.
    pub decoded_bytes: u64,
    /// Fiber-stack push + pop operations retired by the record driver (the §7.3
    /// stack-traffic gate input).
    pub stack_ops: u64,
    /// INERT (Unit D evidence-dropped) — permanently 0.
    pub tos_ops: u64,
}

/// Like [`vm_run_source`] but with DECODE DISABLED — the `ASCRIPT_NO_DECODE=1`
/// kill switch (DECODE Task 2). Observable behavior is byte-identical to
/// [`vm_run_source`]; only throughput may differ once Task 4 wires up the driver.
///
/// Used by the differential test (`decode_entry_points_exist_and_are_inert_pre_driver`)
/// and the DECODE-mode differential batteries. `#[doc(hidden)]` — not a stable API.
#[doc(hidden)]
pub async fn vm_run_source_no_decode(src: &str) -> Result<(String, Option<i32>), AsError> {
    use crate::vm::Vm;
    // specialize=true, sync_lane=true, call_fast=true; decode=OFF.
    vm_run_source_decode_cfg(src, false, true, true, Vm::DECODE_THRESHOLD).await
}

/// Like [`vm_run_source`] but with DECODE FORCED on with threshold=0 — every
/// proto is decoded immediately, regardless of warmth, so even short programs
/// exercise the record driver once Task 4 lands. Pre-driver (INERT), this is
/// byte-identical to [`vm_run_source`]. `#[doc(hidden)]` — not a stable API.
#[doc(hidden)]
pub async fn vm_run_source_decoded_forced(src: &str) -> Result<(String, Option<i32>), AsError> {
    // decode=ON, threshold=0 (always decode immediately).
    vm_run_source_decode_cfg(src, true, true, true, 0).await
}

/// ELIDE §4.2 / §5.1: compile `src` with contract-elision (proven sites identified
/// by the static checker are compiled without the corresponding runtime checks),
/// then run on the VM with full specialization. Observable output is byte-identical
/// to [`vm_run_source`] for programs the elision predicate is sound for — a
/// behavioral regression means the proof predicate is wrong, not the run path.
///
/// Used by the ELIDE end-to-end tests in `tests/elide.rs` and the differential
/// correctness gate (Phase 4). `#[doc(hidden)]` — not a stable API.
#[doc(hidden)]
pub async fn vm_run_source_elided(src: &str) -> Result<(String, Option<i32>), AsError> {
    use crate::check::infer::elision_proofs;
    use crate::compile::compile_source_with_elision;
    use crate::vm::chunk::FnProto;
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;

    let src_info = Rc::new(SourceInfo {
        path: "<input>".to_string(),
        text: src.to_string(),
    });
    let elide_set = elision_proofs(src);
    let chunk = compile_source_with_elision(src, Some(&elide_set))
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
        name_span: None,
    });
    let closure = Closure::new(proto.clone());
    let interp = Rc::new(Interp::new());
    interp.install_self();
    interp.set_worker_source(src);
    let vm = Vm::with_flags(interp.clone(), true, true, true);
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

/// ELIDE §4.3 Task 3.2: like [`vm_run_source_elided`] but with VM specialization
/// DISABLED (generic path). Used by the four-mode smoke test to confirm the
/// generic-VM elided path produces byte-identical output.
/// `#[doc(hidden)]` — not a stable API.
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
#[doc(hidden)]
pub async fn vm_run_source_elided_generic(src: &str) -> Result<(String, Option<i32>), AsError> {
    use crate::check::infer::elision_proofs;
    use crate::compile::compile_source_with_elision;
    use crate::vm::chunk::FnProto;
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;

    let src_info = Rc::new(SourceInfo {
        path: "<input>".to_string(),
        text: src.to_string(),
    });
    let elide_set = elision_proofs(src);
    let chunk = compile_source_with_elision(src, Some(&elide_set))
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
        name_span: None,
    });
    let closure = Closure::new(proto.clone());
    let interp = Rc::new(Interp::new());
    interp.install_self();
    interp.set_worker_source(src);
    // specialize=false → generic (non-specializing) path
    let vm = Vm::with_flags(interp.clone(), false, true, true);
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

/// ELIDE §6.3: compile `src` WITHOUT elision (paranoid mode runs elide-OFF) but
/// build the [`ElisionSet`] from the proof phase and install it on the `Interp`
/// for contract-failure-path paranoid lookup. Returns `(output, exit_code)`.
/// Any proven site that fails at runtime escalates to "ELIDE proof violated …".
/// `#[doc(hidden)]` — not a stable API.
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
#[doc(hidden)]
pub async fn vm_run_source_paranoid(src: &str) -> Result<(String, Option<i32>), AsError> {
    use crate::check::infer::elision_proofs;
    use crate::vm::chunk::FnProto;
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;

    let src_info = Rc::new(SourceInfo {
        path: "<input>".to_string(),
        text: src.to_string(),
    });
    // Paranoid = elide-OFF compile (full checks retained) + ElisionSet retained.
    let paranoid_set = elision_proofs(src);
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
        name_span: None,
    });
    let closure = Closure::new(proto.clone());
    let interp = Rc::new(Interp::new());
    interp.install_self();
    interp.set_worker_source(src);
    interp.set_paranoid_set(paranoid_set);
    let vm = Vm::with_flags(interp.clone(), true, true, true);
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

/// ELIDE §6.3 test-only helper: parse `src` with the legacy parser and return the
/// span of the first top-level `ExprKind::Call` statement. Panics if none found.
/// Used so the injection seam injects the EXACT same span the runtime uses for the
/// call — matching by the same char-offset key that [`Interp::maybe_paranoid_escalate`]
/// looks up in the `calls` set.
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
fn first_call_span_in_source(src: &str) -> crate::span::Span {
    use crate::ast::{ExprKind, Stmt};
    let tokens = lexer::lex(src).expect("lex in first_call_span_in_source");
    let stmts = parser::parse(&tokens).expect("parse in first_call_span_in_source");
    for stmt in &stmts {
        if let Stmt::Expr(expr) = stmt {
            if matches!(&expr.kind, ExprKind::Call { .. }) {
                return expr.span;
            }
        }
    }
    panic!("no top-level Call expression found in source");
}

/// ELIDE §6.3 test-only: like [`vm_run_source_paranoid`] but additionally injects
/// a FAKE call-site proof span so the FIRST contract failure at the call expression
/// is treated as a proven site and escalates. The fake span is calculated from the
/// source by finding the FIRST top-level `Call` expression using the legacy parser,
/// ensuring it matches the span the runtime anchors contract panics to.
///
/// This simulates a checker soundness bug (the checker claimed a call was safe when
/// it is not). Expected to return the panic message containing the escalation prefix.
/// `#[doc(hidden)]` — not a stable API.
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
#[doc(hidden)]
pub async fn vm_run_source_paranoid_with_fake_call_proof(src: &str) -> String {
    use crate::check::infer::elision_proofs;
    use crate::vm::chunk::FnProto;
    use crate::vm::value_ext::Closure;
    use crate::vm::Vm;

    let paranoid_set = elision_proofs(src);
    let chunk = crate::compile::compile_source(src)
        .map_err(|e| e.message)
        .unwrap_or_else(|e| panic!("compile failed: {e}"));
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
        name_span: None,
    });
    let closure = Closure::new(proto.clone());
    let interp = Rc::new(Interp::new());
    interp.install_self();
    interp.set_worker_source(src);
    interp.set_paranoid_set(paranoid_set);
    // Inject a FAKE proof: find the first top-level call expression span using the
    // legacy parser (the same char-offset convention the runtime uses) and inject it
    // into the `calls` set. This ensures `maybe_paranoid_escalate` sees an exact match
    // when the contract fails, triggering the "ELIDE proof violated" escalation.
    let call_span = first_call_span_in_source(src);
    interp.inject_paranoid_call_span(call_span.start as u32, call_span.end as u32);
    let vm = Vm::with_flags(interp.clone(), true, true, true);
    let mut fiber = crate::vm::fiber::Fiber::new(closure);
    let local = tokio::task::LocalSet::new();
    let result = local.run_until(vm.run(&mut fiber)).await;
    local.await;
    crate::gc::collect();
    match result {
        Err(crate::interp::Control::Panic(e)) => e.message,
        Ok(_) => String::from("(no panic — escalation did not fire)"),
        Err(crate::interp::Control::Propagate(_)) => String::from("(propagated — no panic)"),
        Err(crate::interp::Control::Exit(_)) => String::from("(exit — no panic)"),
    }
}

/// ELIDE §6.3 tree-walker paranoid run: like [`vm_run_source_paranoid`] but on
/// the tree-walker engine. Returns the captured output, or `Err(message)` on panic.
/// `#[doc(hidden)]` — not a stable API.
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
#[doc(hidden)]
pub async fn tw_run_source_paranoid(src: &str) -> Result<String, String> {
    use crate::check::infer::elision_proofs;

    let tokens = match lexer::lex(src) {
        Ok(t) => t,
        Err(e) => return Err(e.message),
    };
    let program = match parser::parse(&tokens) {
        Ok(p) => p,
        Err(e) => return Err(e.message),
    };
    // Paranoid = elide-OFF (no mark_program call) + ElisionSet retained.
    let paranoid_set = elision_proofs(src);
    let interp = Rc::new(Interp::new());
    interp.install_self();
    interp.set_paranoid_set(paranoid_set);
    let env = crate::interp::global_env().child();
    let local = tokio::task::LocalSet::new();
    let result = local
        .run_until(crate::interp::telemetry_root_scope(
            interp.exec_program(&program, &env),
        ))
        .await;
    local.await;
    match result {
        Ok(_) | Err(crate::interp::Control::Propagate(_)) | Err(crate::interp::Control::Exit(_)) => {
            Ok(interp.output())
        }
        Err(crate::interp::Control::Panic(e)) => Err(e.message),
    }
}

/// ELIDE §6.3 test-only (TW): like [`tw_run_source_paranoid`] but additionally
/// injects a FAKE call-site proof span to trigger escalation on contract failure.
/// Returns the panic message string. `#[doc(hidden)]` — not a stable API.
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
#[doc(hidden)]
pub async fn tw_run_source_paranoid_with_fake_call_proof(src: &str) -> String {
    use crate::check::infer::elision_proofs;

    let tokens = match lexer::lex(src) {
        Ok(t) => t,
        Err(e) => return format!("lex error: {e}"),
    };
    let program = match parser::parse(&tokens) {
        Ok(p) => p,
        Err(e) => return format!("parse error: {e}"),
    };
    let paranoid_set = elision_proofs(src);
    let interp = Rc::new(Interp::new());
    interp.install_self();
    interp.set_paranoid_set(paranoid_set);
    // Inject a FAKE proof: find the first top-level call expression span (the same
    // char-offset convention the tree-walker uses for `expr.span`) and inject it into
    // the `calls` set so `maybe_paranoid_escalate` sees an exact match.
    let call_span = first_call_span_in_source(src);
    interp.inject_paranoid_call_span(call_span.start as u32, call_span.end as u32);
    let env = crate::interp::global_env().child();
    let local = tokio::task::LocalSet::new();
    let result = local
        .run_until(crate::interp::telemetry_root_scope(
            interp.exec_program(&program, &env),
        ))
        .await;
    local.await;
    match result {
        Err(crate::interp::Control::Panic(e)) => e.message,
        Ok(_) => String::from("(no panic — escalation did not fire)"),
        Err(crate::interp::Control::Propagate(_)) => String::from("(propagated — no panic)"),
        Err(crate::interp::Control::Exit(_)) => String::from("(exit — no panic)"),
    }
}

/// DECODE §8.3: run `src` on the VM with DECODE FORCED (threshold=0) and return
/// a [`DecodeStats`] bundle containing the program output + all stat counters.
/// All counters are 0 until the corresponding task wires them up (INERT until
/// Task 4). `#[doc(hidden)]` — not a stable API.
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
#[doc(hidden)]
pub async fn vm_run_source_decode_stats(src: &str) -> Result<DecodeStats, AsError> {
    vm_run_source_decode_stats_cfg(src, true, true, true, 0).await
}

/// DECODE §8.3 variant: like [`vm_run_source_decode_stats`] but with DECODE OFF.
/// Used to prove `decoded_ops == 0` when the kill switch is active.
/// `#[doc(hidden)]` — not a stable API.
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
#[doc(hidden)]
pub async fn vm_run_source_decode_stats_no_decode(src: &str) -> Result<DecodeStats, AsError> {
    use crate::vm::Vm;
    vm_run_source_decode_stats_cfg(src, false, true, true, Vm::DECODE_THRESHOLD).await
}

/// **DECODE §5.1 (Unit B part 1): run `src` in CENSUS mode** — DECODE FORCED
/// (threshold = 0, every proto decoded so the real record stream is seen) with the
/// pair/triple census armed. Returns `(counts, total_records)` where a PAIR key is
/// `(CENSUS_NO_PREV, prev, op)` and a TRIPLE key is `(prev2, prev, op)`; the harness
/// (`tests/decode_census.rs`) merges per-program drains into a global aggregate and
/// ranks them. FULLY `#[cfg(feature = "decode-census")]` — this entry point and the
/// whole counting apparatus DO NOT EXIST in a default build (the JIT-spec §2.1
/// "not there" discipline). `#[doc(hidden)]` — not a stable API.
/// **DECODE §5.1: the PAIR sentinel** — the first key-slot value that marks a census
/// entry as a 2-gram `(prev, op)` rather than a 3-gram `(prev2, prev, op)`. Re-exported
/// for `tests/decode_census.rs` (the only consumer). `#[doc(hidden)]`.
#[cfg(feature = "decode-census")]
#[doc(hidden)]
pub const CENSUS_NO_PREV: u16 = crate::vm::decode::CENSUS_NO_PREV;

#[cfg(feature = "decode-census")]
#[doc(hidden)]
pub async fn vm_run_source_census(
    src: &str,
) -> Result<(crate::vm::decode::CensusCounts, u64), AsError> {
    use crate::vm::value_ext::RunOutcome;
    use crate::vm::Vm;

    let src_info = Rc::new(SourceInfo {
        path: "<input>".to_string(),
        text: src.to_string(),
    });
    let chunk = crate::compile::compile_source(src)
        .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?;
    let proto = Rc::new(crate::vm::chunk::FnProto {
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
        name_span: None,
    });
    let closure = crate::vm::value_ext::Closure::new(proto);
    let interp = Rc::new(Interp::new());
    interp.install_self();
    interp.set_worker_source(src);
    // decode FORCED on (threshold = 0): every proto decodes immediately so the
    // record driver sees the real stream the census counts.
    let vm = Vm::with_all_flags(interp.clone(), true, true, true, true, true, true, 0);
    vm.arm_census();
    let mut fiber = crate::vm::fiber::Fiber::new(closure);
    let local = tokio::task::LocalSet::new();
    let result = local.run_until(vm.run(&mut fiber)).await;
    local.await;
    crate::gc::collect();
    // Surface a program error so the harness can SKIP the file (e.g. a feature-
    // unavailable import) without aborting the whole census run.
    match result {
        Ok(RunOutcome::Done(_)) => {}
        Ok(RunOutcome::Yielded(_)) => unreachable!("top-level program cannot yield"),
        Err(crate::interp::Control::Panic(e)) => return Err(e.with_source(src_info)),
        Err(crate::interp::Control::Propagate(_)) => {}
        Err(crate::interp::Control::Exit(_)) => {}
    }
    Ok(vm.take_census().unwrap_or_default())
}

/// Shared body for the DECODE test entries: runs `src` with explicit decode
/// kill-switch values, no instrumentation (decode paths are orthogonal to DBG),
/// and returns the plain `(output, exit_code)` pair.
async fn vm_run_source_decode_cfg(
    src: &str,
    decode: bool,
    decode_inline: bool,
    decode_tos: bool,
    decode_threshold: u16,
) -> Result<(String, Option<i32>), AsError> {
    use crate::vm::value_ext::RunOutcome;
    use crate::vm::Vm;

    let src_info = Rc::new(SourceInfo {
        path: "<input>".to_string(),
        text: src.to_string(),
    });
    let chunk = crate::compile::compile_source(src)
        .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?;
    let proto = Rc::new(crate::vm::chunk::FnProto {
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
        name_span: None,
    });
    let closure = crate::vm::value_ext::Closure::new(proto);
    let interp = Rc::new(Interp::new());
    interp.install_self();
    interp.set_worker_source(src);
    // specialize=true, sync_lane=true, call_fast=true (production defaults);
    // decode flags and threshold set explicitly — no env read (parallel-test hygiene).
    let vm = Vm::with_all_flags(interp.clone(), true, true, true, decode, decode_inline, decode_tos, decode_threshold);
    let mut fiber = crate::vm::fiber::Fiber::new(closure);
    let local = tokio::task::LocalSet::new();
    let result = local.run_until(vm.run(&mut fiber)).await;
    local.await;
    crate::gc::collect();
    let pair = match result {
        Ok(RunOutcome::Done(_)) => Ok((interp.output(), None)),
        Ok(RunOutcome::Yielded(_)) => unreachable!("top-level program cannot yield"),
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        Err(crate::interp::Control::Propagate(_)) => Ok((interp.output(), None)),
        Err(crate::interp::Control::Exit(code)) => Ok((interp.output(), Some(code))),
    }?;
    Ok(pair)
}

/// Shared body for the DECODE stats test entries: runs `src` with explicit decode
/// flags and returns a [`DecodeStats`] bundle. Compiled only under
/// `#[cfg(any(test, feature = "fuzzgen", fuzzing))]`.
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
async fn vm_run_source_decode_stats_cfg(
    src: &str,
    decode: bool,
    decode_inline: bool,
    decode_tos: bool,
    decode_threshold: u16,
) -> Result<DecodeStats, AsError> {
    use crate::vm::value_ext::RunOutcome;
    use crate::vm::Vm;

    let src_info = Rc::new(SourceInfo {
        path: "<input>".to_string(),
        text: src.to_string(),
    });
    let chunk = crate::compile::compile_source(src)
        .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?;
    let proto = Rc::new(crate::vm::chunk::FnProto {
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
        name_span: None,
    });
    let closure = crate::vm::value_ext::Closure::new(proto);
    let interp = Rc::new(Interp::new());
    interp.install_self();
    interp.set_worker_source(src);
    let vm = Vm::with_all_flags(interp.clone(), true, true, true, decode, decode_inline, decode_tos, decode_threshold);
    let mut fiber = crate::vm::fiber::Fiber::new(closure);
    let local = tokio::task::LocalSet::new();
    let result = local.run_until(vm.run(&mut fiber)).await;
    local.await;
    crate::gc::collect();
    // Read the DECODE stat counters before consuming result (they are already
    // stable: the run is complete, no borrow outstanding).
    let inner = vm.decode_stats_inner();
    let pair = match result {
        Ok(RunOutcome::Done(_)) => Ok((interp.output(), None)),
        Ok(RunOutcome::Yielded(_)) => unreachable!("top-level program cannot yield"),
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        Err(crate::interp::Control::Propagate(_)) => Ok((interp.output(), None)),
        Err(crate::interp::Control::Exit(code)) => Ok((interp.output(), Some(code))),
    }?;
    Ok(DecodeStats {
        output: pair.0,
        exit_code: pair.1,
        decoded_ops: inner.decoded_ops,
        fused_ops: inner.fused_ops,
        inline_hits: inner.inline_hits,
        inline_misses: inner.inline_misses,
        decoded_bytes: inner.decoded_bytes,
        stack_ops: inner.stack_ops,
        tos_ops: inner.tos_ops,
    })
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
        name_span: None,
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
        name_span: None,
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
    caps: Option<crate::stdlib::caps::CapSet>,
    elide: bool,
) -> Result<std::path::PathBuf, AsError> {
    // SELF-CONTAINED-BUNDLES (Task 1.5): a multi-module program is emitted as an
    // `ASCRIPTA` archive embedding the whole reachable import graph (so `run out.aso`
    // works from a directory WITHOUT the sources). A single-module program recompiles
    // the lone entry to a bare `ASO\0` chunk — byte-identical to the pre-archive
    // artifact, so existing `.aso` goldens/tests stay valid.
    // ELIDE §4.2/§5: `build` elides under the same default / kill switch as `run`.
    let (mut archive, report) = compile_archive(file, with_debug, elide)?;
    // SELF-CONTAINED-BUNDLES (Task 3.2, ARTIFACT-FORMAT RULE): emit an `ASCRIPTA` archive
    // when the graph has >1 module OR the caps are restricted (`caps.is_some()`); emit the
    // bare `ASO\0` chunk ONLY when single-module AND `caps` is `None`. This gives the
    // embedded capability floor a home EVERYWHERE it is set — including a single-module
    // `.aso` built with `--deny` — while keeping the common unrestricted single-module build
    // byte-identical to today (a bare chunk, so existing goldens/tests stay valid).
    let bytes = if archive.modules.len() > 1 || caps.is_some() {
        // Embed the composed CapSet into the archive manifest BEFORE encoding (Task 3.2
        // enforces it at runtime via `restrict_with`). `None` → all granted (the
        // byte-identical placeholder). Set on the returned archive so `compile_archive`'s
        // signature is untouched.
        archive.caps = caps.unwrap_or_else(crate::stdlib::caps::CapSet::all_granted);
        // Surface the tree-shaking summary on STDERR (never stdout — stdout carries only
        // `compiled … -> …`, keeping the program corpus byte-clean). Harmless for a
        // single-module archive (the report is then a one-module summary).
        print_shake_report(&report);
        archive.encode() // ASCRIPTA — embed the module graph + the caps floor
    } else {
        // Unrestricted single-module: a bare `ASO\0` chunk, byte-identical to today.
        // RECOMPILE (do NOT reuse `archive.modules[0].1`): `compile_archive` compiles the
        // entry under its CANONICALIZED absolute path for stable dedup identity, which a
        // debug `.aso` embeds as the source path — leaking the build machine's layout. A
        // fresh compile from the as-passed `file` keeps the relative source path, so the
        // single-module artifact stays byte-identical to the pre-archive output (and stays
        // decoupled from archive internals as Phase 2 evolves `compile_archive`). `build` is
        // a one-shot CLI compile, so re-compiling this one file is negligible.
        compile_verified_aso_bytes(file, with_debug, elide)? // bare ASO\0 — byte-identical to today
    };
    let out_path = match out {
        Some(p) => p.to_path_buf(),
        None => file.with_extension("aso"),
    };
    std::fs::write(&out_path, &bytes)
        .map_err(|e| AsError::new(format!("cannot write {}: {}", out_path.display(), e)))?;
    Ok(out_path)
}

/// The shared compile → verify → serialize front half of [`build_file`] and
/// [`build_native`] (BIN §2.2 step 1): read the source, [`compile::compile_source`], bind the
/// module source for debug info, [`vm::verify::verify`] (so a produced `.aso` is always
/// loadable), then `to_bytes_with_debug`. Returns the verified `.aso` byte vector. The native
/// bundle embeds these EXACT bytes, so the embedded payload is byte-identical to a `build`
/// artifact (four-mode parity stays free; no `.aso` format change).
fn compile_verified_aso_bytes(file: &Path, with_debug: bool, elide: bool) -> Result<Vec<u8>, AsError> {
    let src = std::fs::read_to_string(file)
        .map_err(|e| AsError::new(format!("cannot read {}: {}", file.display(), e)))?;
    let src_info = Rc::new(SourceInfo {
        path: file.display().to_string(),
        text: src.clone(),
    });
    // ELIDE §4.2/§5: `ascript build` runs the collector under the same default /
    // kill switch as `run`, so the produced `.aso`/native bundle KEEPS the win
    // (the `CallElided` opcode is durable). Off → byte-identical to pre-ELIDE.
    let chunk = if elide {
        let set = crate::check::infer::elision_proofs(&src);
        crate::compile::compile_source_with_elision(&src, Some(&set))
            .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?
    } else {
        crate::compile::compile_source(&src)
            .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?
    };
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
    chunk
        .to_bytes_with_debug(with_debug)
        .map_err(|e| AsError::new(format!("cannot serialize bytecode: {e}")))
}

/// BNDL §3 — walk a program's import graph from `entry` and compile every reachable
/// user/package module into a [`ModuleArchive`] (Phase 1, Task 1.3). Later phases
/// consult this archive at runtime (so a multi-file program needs no source tree on
/// disk), tree-shake it, and embed the composed `CapSet`.
///
/// # The walk
///
/// A breadth-first worklist over the import graph. The entry is compiled first; each
/// module's compiled chunk is decoded back (through the SAME `from_bytes_verified`
/// trust boundary the runtime uses) only to read its `imports`. Every `import`
/// specifier is classified via [`Interp::classify_specifier`]:
///
/// - `std/*` → SKIPPED (native Rust, linked into the runtime, never archived).
/// - relative / package → resolved to a file path, deduped by **canonical path**, and
///   (if new) compiled and enqueued so its OWN imports are walked transitively.
/// - unknown package → a clean [`AsError`] (the program references a package that is
///   not installed; Phase 1 installs no resolver, so every bare specifier lands here).
///
/// Dedup happens BEFORE recursing, so a cycle (`A` imports `B` imports `A`) terminates.
///
/// # The logical-key convention (load-bearing — Task 1.4 MUST match it)
///
/// Each module's archive key is its **lexical logical path relative to the entry
/// file's directory**, normalized to forward slashes — NOT an absolute, canonicalized
/// path (which would leak the build machine's layout and break cross-machine
/// portability, spec §3.3). It is computed purely from `import` specifiers:
///
/// - the entry's key is its file name (e.g. `bundle_multimodule.as`); its logical
///   directory is the archive-namespace root (`""`);
/// - an import with specifier `S` from a module whose logical directory is `D` keys
///   the imported module at `lexically_normalize(D `join` S)` with a default `.as`
///   extension — e.g. `./bundle_util` from the root keys `bundle_util.as`.
///
/// `.`/`..` segments are resolved LEXICALLY ([`join_logical`]): a `..` cancels the
/// preceding segment, but a `..` that escapes the logical root is **PRESERVED VERBATIM**
/// — so a module in a subdirectory importing `../shared` produces the stable key
/// `../shared.as`. That key is still machine-independent (relative to the entry dir, no
/// absolute leak) and stays unique (`../a` ≠ `a`); Task 1.4 MUST reproduce the same
/// `..`-preserving join. (Package specifiers are NOT importer-relative — they key under
/// a stable `pkg/<specifier>` namespace, §below.)
///
/// This is exactly what the Task 1.4 runtime loader can reproduce: it resolves an
/// import against the importer's *logical* directory (the archive-relative dir), not
/// the on-disk absolute dir, and normalizes the same way. Dedup identity is the
/// canonical on-disk path (so two specifiers reaching the same file collapse to one
/// module, matching `load_file_module`'s cache identity), while the STORED key stays
/// machine-independent.
///
/// `caps` is a default all-granted [`CapSet`] placeholder here — the build commands
/// (`build_file`/`build_native`) OVERRIDE `archive.caps` with the composed capability set
/// (`compose_caps`: CLI `--deny`/`--sandbox`/carve-outs + `ascript.toml`) before encoding, and
/// `run_verified_archive` enforces it at run (monotone `restrict_with`). `shake_digest` is the
/// reproducible 32-byte sha256 of the tree-shake report
/// ([`crate::compile::shake::ShakeReport::digest`]). The report is RETURNED alongside the
/// archive so the caller (`build_file`/`build_native`) can print a human-readable
/// tree-shaking summary to stderr.
///
/// `with_debug` is threaded into every module's compile: the stored chunk bytes are
/// what actually RUN at bundle time (the archive replaces the source tree), so debug
/// info (line/variable tables for panic diagnostics) is preserved per the caller's
/// choice, exactly as a single-module `build`/`--native` would.
pub fn compile_archive(
    entry: &Path,
    with_debug: bool,
    elide: bool,
) -> Result<(crate::vm::archive::ModuleArchive, crate::compile::shake::ShakeReport), AsError> {
    compile_archive_with_shake(entry, with_debug, true, elide)
}

/// Like [`compile_archive`], but with the pass-2 TREE-SHAKE toggleable. This is the
/// load-bearing TEST seam for the shaken-vs-unshaken differential (Phase 2, Task 2.5):
/// `compile_archive(entry, dbg)` is exactly `compile_archive_with_shake(entry, dbg, true)`,
/// and `shake = false` skips pass-2 pruning entirely so each LIBRARY module keeps its full
/// pass-1 bytes (every top-level declaration present). Building BOTH forms and asserting
/// their runs are byte-identical isolates SHAKING as the only variable — same archive walk,
/// same logical keys, only pruning toggled.
///
/// When `shake = false` the returned [`ShakeReport`](crate::compile::shake::ShakeReport) is
/// still computed (so a caller can compare it to the shaken report), but it is NOT applied
/// to the stored bytes — the report is purely informational for the no-shake build.
///
/// `#[doc(hidden)]` test/seam API — production callers use the 2-arg [`compile_archive`].
#[doc(hidden)]
pub fn compile_archive_with_shake(
    entry: &Path,
    with_debug: bool,
    shake: bool,
    elide: bool,
) -> Result<(crate::vm::archive::ModuleArchive, crate::compile::shake::ShakeReport), AsError> {
    use crate::vm::archive::ModuleArchive;
    use std::collections::HashMap;
    use std::path::PathBuf;

    // A work item: the file to compile, the importer-relative logical key the archive
    // stores it under, and the logical directory imports inside it resolve against.
    struct Pending {
        path: PathBuf,
        key: String,
        logical_dir: String,
    }

    // An `Interp` is used ONLY as the host for `classify_specifier` (it reads the
    // importer's `module_dir` to resolve a relative specifier to a path). No code runs.
    // No package resolver is installed in Phase 1, so a bare package specifier
    // classifies as `UnknownPackage` → a clean error below.
    let interp = Interp::new();

    // The entry file anchors the logical namespace. Canonicalize it for a stable dedup
    // identity and to derive the entry directory all relative imports resolve against.
    let entry_canon = entry
        .canonicalize()
        .map_err(|e| AsError::new(format!("cannot read {}: {}", entry.display(), e)))?;
    let entry_dir = entry_canon
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let entry_key = entry_canon
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "entry.as".to_string());

    let mut modules: Vec<(String, Vec<u8>)> = Vec::new();
    // The decoded chunk + on-disk source path for each module, parallel to `modules`
    // by index — pass 2 (tree-shake) re-compiles each LIBRARY module's source under
    // its keep-set, and the shaker reads each chunk's `imports`.
    let mut graph_chunks: Vec<crate::vm::chunk::Chunk> = Vec::new();
    let mut module_paths: Vec<PathBuf> = Vec::new();
    // Per-module RESOLVED import edges (target = module index), parallel to `modules`.
    // Filled as the BFS resolves each import specifier to its dedup'd target index.
    let mut module_edges: Vec<Vec<crate::compile::shake::ImportEdge>> = Vec::new();
    // Dedup by canonical path → the module's index in `modules`. Keyed by canonical
    // path (not logical key) so two specifiers reaching the same file collapse to one.
    let mut seen: HashMap<PathBuf, usize> = HashMap::new();

    let mut queue: std::collections::VecDeque<Pending> = std::collections::VecDeque::new();
    queue.push_back(Pending {
        path: entry_canon.clone(),
        key: entry_key,
        logical_dir: String::new(),
    });
    seen.insert(entry_canon.clone(), 0); // reserve index 0 for the entry (filled below)

    // The entry is enqueued first and BFS dequeues it first, so it is always archived at
    // index 0 — no need to track which dequeued module was the entry.
    let entry: u32 = 0;

    while let Some(item) = queue.pop_front() {
        // Compile this module to verified `.aso` bytes (reusing the SAME path `build`
        // uses, so the stored chunk always re-verifies). The stored bytes are what RUN
        // (the archive replaces the source tree), so debug info is preserved per the
        // caller's `with_debug` choice — NOT dropped — matching a single-module build.
        let bytes = compile_verified_aso_bytes(&item.path, with_debug, elide)?;

        // Decode the just-produced chunk to read its import table. These are OUR OWN
        // freshly-verified bytes, so this never sees hostile input.
        let chunk = crate::vm::chunk::Chunk::from_bytes_verified(&bytes).map_err(|e| {
            AsError::new(format!(
                "internal: re-decoding compiled module {} failed: {e:?}",
                item.path.display()
            ))
        })?;

        // Record this module. Indices are assigned in `seen` in monotonic queue order,
        // and BFS pops in that same order, so the reserved index always equals the
        // current `modules` length — append lands exactly at this module's slot.
        let this_index = *seen
            .get(&item.path)
            .expect("every queued module is pre-registered in `seen`");
        debug_assert_eq!(this_index, modules.len());
        modules.push((item.key.clone(), bytes));
        module_paths.push(item.path.clone());
        // The edges for THIS module are accumulated below as imports resolve.
        debug_assert_eq!(module_edges.len(), this_index);
        let mut this_edges: Vec<crate::compile::shake::ImportEdge> = Vec::new();

        // The directory inside this module that its imports resolve against on disk.
        let this_disk_dir = item
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| entry_dir.clone());

        for imp in &chunk.imports {
            let source = imp.source();
            // Resolve the specifier relative to THIS module's on-disk directory.
            interp.set_module_dir(this_disk_dir.clone());
            match interp.classify_specifier(source) {
                crate::interp::SpecifierKind::Std => {
                    // Native stdlib — linked in, never archived (no shake edge).
                }
                kind @ (crate::interp::SpecifierKind::Relative(_)
                | crate::interp::SpecifierKind::Package { .. }) => {
                    let target = match &kind {
                        crate::interp::SpecifierKind::Relative(t) => t.clone(),
                        crate::interp::SpecifierKind::Package { target, .. } => target.clone(),
                        _ => unreachable!("matched only Relative|Package"),
                    };
                    let dep_path = resolve_module_file(&target).map_err(|msg| {
                        AsError::new(format!(
                            "cannot resolve import '{source}' from {}: {msg}",
                            item.path.display()
                        ))
                    })?;
                    // The lexical, machine-independent archive key for this dependency.
                    //  - A RELATIVE import keys relative to the IMPORTER's logical dir
                    //    (`./util` from the root → `bundle_util.as`).
                    //  - A PACKAGE import is NOT importer-relative: it keys under a stable
                    //    `pkg/<specifier>` namespace so the same package resolves to the
                    //    same key regardless of which module imported it (the store-relative
                    //    logical id of spec §3.3). Phase 1 installs no resolver, so the
                    //    package branch is currently unreachable (bare specifiers land in
                    //    `UnknownPackage` below) — this keeps Phase 4 correct-by-construction.
                    let dep_key = match &kind {
                        crate::interp::SpecifierKind::Package { .. } => {
                            crate::vm::archive::join_logical("pkg", source)
                        }
                        _ => crate::vm::archive::join_logical(&item.logical_dir, source),
                    };
                    let dep_logical_dir = crate::vm::archive::logical_parent(&dep_key);

                    // Resolve (or reserve) the dedup'd archive index this import targets.
                    // A diamond / cycle dep is ALREADY in `seen`; a fresh one reserves the
                    // next monotonic index and is enqueued. Either way we know the target
                    // index NOW, so the shake edge is recorded for both.
                    let target_index = if let Some(&idx) = seen.get(&dep_path) {
                        idx // already archived (or queued) — dedup terminates cycles
                    } else {
                        // `seen` maps every known module (recorded or queued) to a unique,
                        // monotonically assigned index, so the next free index is `seen.len()`.
                        let reserved = seen.len();
                        seen.insert(dep_path.clone(), reserved);
                        queue.push_back(Pending {
                            path: dep_path,
                            key: dep_key,
                            logical_dir: dep_logical_dir,
                        });
                        reserved
                    };
                    this_edges.push(import_desc_to_edge(imp, target_index));
                }
                crate::interp::SpecifierKind::UnknownPackage(key) => {
                    return Err(AsError::new(format!(
                        "unknown package '{key}' — add it with 'ascript add' \
                         (imported from {})",
                        item.path.display()
                    )));
                }
            }
        }
        graph_chunks.push(chunk);
        module_edges.push(this_edges);
    }

    // ── Phase 2, Task 2.3: tree-shake. ─────────────────────────────────────────
    // Compute the per-module keep-set (the reachable closure of top-level names) and
    // RE-EMIT each LIBRARY module dropping unreferenced INERT top-level declarations.
    // The ENTRY (index 0) is kept WHOLE — its pass-1 bytes are stored UNCHANGED, so a
    // single-module program (and the entry of any program) is byte-identical to today.
    // A re-compile RE-READS each library module's source from disk (the pass-1 source
    // string isn't preserved — `compile_verified_aso_bytes` is self-contained), so a
    // library module is compiled TWICE: O(2N) compiles for N library modules. Fine for
    // a one-shot build; worth a pass-1-source cache later if build time matters. The
    // keep-set's closure property guarantees no dropped name is referenced by kept code
    // (no dangling globals).
    let graph: Vec<crate::compile::shake::ModuleNode> = modules
        .iter()
        .zip(graph_chunks)
        .zip(module_edges)
        .map(|(((key, _bytes), chunk), edges)| crate::compile::shake::ModuleNode {
            key: key.clone(),
            chunk,
            edges,
        })
        .collect();
    let mut reach = crate::compile::shake::compute_reachable(&graph);
    // reach.report drives BOTH the reproducible manifest digest (below) and the
    // stderr tree-shaking summary the caller prints — it is RETURNED to the caller.

    // Pre-render each pin's `<importer_key>:line:col` location against the IMPORTER's
    // source (the module whose namespace use forced the pin). We hold the sources here;
    // the printer then needs none. Reading the source is best-effort — a read failure
    // simply leaves `location` as `None` (the printer falls back to the bare key). The
    // rendered location is deliberately NOT part of the digest (it is redundant with the
    // importer key + char span the digest already covers).
    for pin in &mut reach.report.pins {
        if let Some(path) = module_paths.get(pin.importer) {
            if let Ok(src) = std::fs::read_to_string(path) {
                pin.location =
                    Some(crate::compile::shake::render_pin_location(&pin.importer_key, &src, pin.span));
            }
        }
    }

    // The no-shake TEST seam (`shake == false`, Task 2.5) skips this loop entirely: every
    // library module keeps its full pass-1 bytes. The report above is still COMPUTED (so a
    // caller can diff it against the shaken report) but never APPLIED — the differential
    // builds both forms and asserts their runs are byte-identical.
    if shake {
        for (idx, (_key, bytes)) in modules.iter_mut().enumerate() {
            if idx == 0 {
                continue; // entry kept whole — byte-identical to today
            }
            // The keep-set should exist for every module index; a missing one is an internal
            // invariant break — defensively keep the module WHOLE rather than over-prune.
            let Some(keep) = reach.keep.get(&idx) else {
                continue;
            };
            let path = &module_paths[idx];
            let pruned = compile_pruned_aso_bytes(path, keep, with_debug, elide)?;
            *bytes = pruned;
        }
    }

    // The manifest digest is the reproducible sha256 of the shake report (machine-
    // independent: logical keys + dropped names + pins, NO absolute paths — same source
    // ⇒ same digest). The report is then returned for the caller's stderr summary.
    let shake_digest = reach.report.digest();
    let archive = ModuleArchive::new(
        entry,
        crate::stdlib::caps::CapSet::default(), // all-granted placeholder; build_file/build_native override archive.caps with the composed set before encoding
        shake_digest,
        modules,
    );
    Ok((archive, reach.report))
}

/// Convert a decoded [`crate::vm::chunk::ImportDesc`] into a tree-shaker
/// [`crate::compile::shake::ImportEdge`] targeting the already-resolved dedup'd module
/// index. A `Named` import contributes the imported export names as roots in the target;
/// a `Namespace` import contributes its alias (the shaker statically refines a
/// namespace-only `alias.foo` use down to the accessed exports, or pins the whole target
/// if the alias escapes — see [`crate::compile::shake::classify_namespace_use`]).
fn import_desc_to_edge(
    imp: &crate::vm::chunk::ImportDesc,
    target: usize,
) -> crate::compile::shake::ImportEdge {
    use crate::compile::shake::ImportEdge;
    match imp {
        crate::vm::chunk::ImportDesc::Named { names, .. } => ImportEdge::Named {
            target,
            names: names
                .iter()
                .map(|(export_name, _slot, _is_cell, _is_global)| Rc::from(export_name.as_str()))
                .collect(),
        },
        crate::vm::chunk::ImportDesc::Namespace { alias, .. } => ImportEdge::Namespace {
            target,
            alias: Rc::from(alias.as_str()),
        },
    }
}

/// Print a human-readable tree-shaking summary to STDERR (Task 2.4). Called by
/// `build_file`/`build_native` for MULTI-MODULE archives only (a single-module build
/// emits a bare chunk with no shaking, so there is nothing to report). STDERR keeps the
/// summary off the program's stdout / the `compiled … -> …` / `bundled … -> …` lines.
///
/// Format (everything deterministically ordered — modules by logical key, names sorted):
///
/// ```text
/// tree-shaking: dropped 3 declaration(s) across 2 module(s); 1 module(s) pinned
///   util.as: dropped 2 — dead, helper
///   math.as: dropped 1 — unused
///   kept all exports of 'config.as' — namespace 'm' is indexed/escapes at app.as:12:7
/// ```
///
/// A build with no drops and no pins prints a single `tree-shaking: nothing to drop`
/// line. The report is purely informational; it never fails the build.
fn print_shake_report(report: &crate::compile::shake::ShakeReport) {
    let total = report.total_dropped();
    let mods = report.modules_with_drops();
    let pins = report.pins.len();

    if total == 0 && pins == 0 {
        eprintln!("tree-shaking: nothing to drop");
        return;
    }

    eprintln!(
        "tree-shaking: dropped {total} declaration(s) across {mods} module(s); \
         {pins} module(s) pinned"
    );

    // Per-module drops, ordered by logical key (machine-independent), skipping modules
    // with no drops (the entry, and any fully-used library).
    let mut drops: Vec<&crate::compile::shake::ModuleDrops> = report
        .dropped
        .iter()
        .filter(|d| !d.names.is_empty())
        .collect();
    drops.sort_by(|a, b| a.key.cmp(&b.key));
    for d in &drops {
        let names: Vec<&str> = d.names.iter().map(|n| n.as_ref()).collect();
        eprintln!("  {}: dropped {} — {}", d.key, d.names.len(), names.join(", "));
    }

    // Pins, ordered by the pinned module's logical key.
    let mut pin_list: Vec<&crate::compile::shake::PinReason> = report.pins.iter().collect();
    pin_list.sort_by(|a, b| a.key.cmp(&b.key));
    for p in &pin_list {
        // `location` is pre-rendered (`compile_archive` holds the sources); fall back to
        // the bare importer key if a source read failed during the build.
        let at = p
            .location
            .clone()
            .unwrap_or_else(|| p.importer_key.clone());
        eprintln!(
            "  kept all exports of '{}' — namespace '{}' is indexed/escapes at {}",
            p.key, p.alias, at
        );
    }
}

/// Compile a LIBRARY module's source to verified `.aso` bytes, PRUNED to its
/// tree-shake `keep` set (Task 2.3). Mirrors [`compile_verified_aso_bytes`] exactly
/// (read source → compile → bind debug source → VERIFY → serialize) but routes the
/// compile through [`crate::compile::compile_source_with_keep`] so unreferenced INERT
/// top-level declarations are never emitted. The pruned chunk is RE-VERIFIED through the
/// same `vm::verify` boundary the runtime trusts — a pruning bug surfaces as a clean
/// error here, never a runtime crash.
fn compile_pruned_aso_bytes(
    file: &Path,
    keep: &std::collections::HashSet<Rc<str>>,
    with_debug: bool,
    elide: bool,
) -> Result<Vec<u8>, AsError> {
    let src = std::fs::read_to_string(file)
        .map_err(|e| AsError::new(format!("cannot read {}: {}", file.display(), e)))?;
    let src_info = Rc::new(SourceInfo {
        path: file.display().to_string(),
        text: src.clone(),
    });
    // ELIDE §4.2: a LIBRARY module is pruned to its keep-set AND elided in one
    // compile (the two transforms are orthogonal). Off → byte-identical to the
    // pre-ELIDE pruned path.
    let elide_set = if elide {
        Some(crate::check::infer::elision_proofs(&src))
    } else {
        None
    };
    let chunk = crate::compile::compile_source_with_keep_and_elision(&src, Some(keep), elide_set.as_ref())
        .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?;
    if with_debug {
        chunk.set_module_source(&src_info);
    }
    crate::vm::verify::verify(&chunk).map_err(|e| {
        AsError::new(format!(
            "internal: pruned bytecode failed verification: {e}"
        ))
        .with_source(src_info)
    })?;
    chunk
        .to_bytes_with_debug(with_debug)
        .map_err(|e| AsError::new(format!("cannot serialize pruned bytecode: {e}")))
}

/// Resolve a requested module path (already importer-joined, e.g. `<dir>/util.as` or an
/// extension-less stem) to the actual file on disk, returning its CANONICAL path. Mirrors
/// `load_file_module`'s `.as`/`.aso` precedence: an explicit extension is honored; a bare
/// stem prefers `<stem>.as`, then `<stem>.aso`. A missing file is an `Err(message)`.
fn resolve_module_file(target: &Path) -> Result<std::path::PathBuf, String> {
    use std::path::PathBuf;
    // `classify_specifier` already defaulted a bare specifier to `.as`, but a `Package`
    // target or an explicit extension may differ; be robust to both. Only an EXPLICIT
    // known module extension (`.as`/`.aso`) is honored literally — any other extension
    // (or none) is treated as a stem and resolved `.as`-then-`.aso`, so a path like
    // `mod.config` is never silently rewritten to `mod.aso`.
    let stem: PathBuf = target.to_path_buf();
    let ext = stem
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let candidates: [PathBuf; 2] = match ext.as_deref() {
        // An explicit `.as`/`.aso` — honor it literally first, then the sibling form.
        Some("as") => [stem.clone(), stem.with_extension("aso")],
        Some("aso") => [stem.clone(), stem.with_extension("as")],
        // No extension, or a non-module extension that is part of the file STEM:
        // resolve `<stem>.as` then `<stem>.aso` (the bare-specifier default path).
        _ => [stem.with_extension("as"), stem.with_extension("aso")],
    };
    for cand in &candidates {
        if let Ok(canon) = cand.canonicalize() {
            return Ok(canon);
        }
    }
    Err(format!(
        "looked for {} and {}",
        candidates[0].display(),
        candidates[1].display()
    ))
}

/// BIN §2.2 — `ascript build --native app.as -o app`: produce a self-contained native
/// executable that bundles the whole runtime + the compiled program. This is **bundling, not
/// AOT**: the output is a copy of the running runtime (`current_exe()`) with the *verified*
/// `.aso` payload + a trailing [`bundle`] footer appended; at startup the runtime reads its
/// own footer and runs the payload through the SAME `from_bytes_verified` path as
/// `run file.aso`.
///
/// `--target` is parsed-but-rejected in v1 (host-only). The default output is the source stem
/// with NO extension (`.exe` on Windows). On Unix the output is `chmod +x`; on macOS it is
/// ad-hoc signed (mandatory on arm64 — appending invalidated the stub's signature).
pub fn build_native(
    file: &Path,
    out: Option<&Path>,
    target: Option<&str>,
    caps: Option<crate::stdlib::caps::CapSet>,
    elide: bool,
) -> Result<std::path::PathBuf, AsError> {
    // v1: cross-compilation is parsed-but-cleanly-rejected (§3.2) — a SPECIFIC Tier-1 error
    // naming the requested triple, never a silent ignore or a generic clap failure.
    if let Some(t) = target {
        return Err(AsError::new(format!(
            "cross-compilation is not yet supported (BIN v1 bundles for the host platform \
             only). Build on a `{t}` host, or omit `--target` to bundle for this host."
        )));
    }

    // Step 1: the payload is the SAME verified bytes a `build` produces — a bare `ASO\0`
    // chunk for a single-module program (byte-identical to today's bundle) or an
    // `ASCRIPTA` archive embedding the whole import graph for a multi-module program
    // (so the bundled binary runs from an empty directory). The `stub || payload ||
    // footer` framing below is unchanged: the payload is opaque to the bundler.
    let (mut archive, report) = compile_archive(file, true, elide)?;
    // SELF-CONTAINED-BUNDLES (Task 3.2, ARTIFACT-FORMAT RULE — same rule as `build_file`):
    // bundle an `ASCRIPTA` archive when the graph has >1 module OR the caps are restricted
    // (`caps.is_some()`); bundle the bare `ASO\0` chunk ONLY when single-module AND `caps`
    // is `None`. So a `--native --deny X` of a single-module program now gets an archive
    // payload (the caps floor has a home and is enforced at run), while the common
    // unrestricted single-module bundle stays byte-identical to today.
    let payload = if archive.modules.len() > 1 || caps.is_some() {
        // Embed the composed CapSet into the archive manifest BEFORE encoding — consistent
        // with the plain `build` path (Task 3.2 enforces it at runtime). `None` → all
        // granted (byte-identical placeholder).
        // SECURITY NOTE (spec §10, macOS): for a `--native` bundle this caps blob rides the
        // footer PAYLOAD, which is appended AFTER the ad-hoc signature — so it is NOT covered
        // by that signature. Embedded caps are tamper-EVIDENT only, not tamper-proof; this is
        // acceptable for v1 because lowering one's own caps is not an attacker goal, and an
        // attacker who can rewrite the binary can replace the whole payload anyway.
        archive.caps = caps.unwrap_or_else(crate::stdlib::caps::CapSet::all_granted);
        // Surface the tree-shaking summary on STDERR (the `bundled … -> …` line stays on
        // stdout).
        print_shake_report(&report);
        archive.encode() // ASCRIPTA — embed the module graph + the caps floor
    } else {
        // Unrestricted single-module: a bare `ASO\0` chunk, byte-identical to today.
        // RECOMPILE (do NOT reuse `archive.modules[0].1`): `compile_archive` compiles the
        // entry under its CANONICALIZED absolute path, which a debug `.aso` embeds as the
        // source path (this is always a debug build → `with_debug=true`). A fresh compile
        // from the as-passed `file` keeps the relative source path, so the single-module
        // bundle stays byte-identical to the pre-archive output. `build` is one-shot, so
        // re-compiling this one file is negligible.
        compile_verified_aso_bytes(file, true, elide)? // bare ASO\0 — byte-identical to today
    };

    // Step 2: the stub is a byte-for-byte copy of the running runtime — but if THIS binary is
    // itself a bundle (a double-bundle: someone ran a bundled `ascript` as the builder, or a
    // future self-rebundle), strip the existing overlay first so the new output carries exactly
    // ONE payload+footer and is not double-sized. The clean stub is everything before the
    // existing payload offset; the recovered prefix is a footer-free runtime by construction.
    let exe = std::env::current_exe()
        .map_err(|e| AsError::new(format!("cannot locate the running executable: {e}")))?;
    let raw = std::fs::read(&exe)
        .map_err(|e| AsError::new(format!("cannot read the runtime {}: {}", exe.display(), e)))?;
    let stub = match crate::bundle::read_bundle_footer(&raw) {
        Some((offset, _len)) => raw[..offset].to_vec(), // strip old overlay → clean runtime
        None => raw,
    };

    // Step 3: choose the output path (source stem; `.exe` on Windows; NEVER `.aso`).
    let out_path = match out {
        Some(p) => p.to_path_buf(),
        None => {
            let stem = file
                .file_stem()
                .map(|s| s.to_owned())
                .unwrap_or_else(|| std::ffi::OsString::from("app"));
            let mut p = std::path::PathBuf::from(stem);
            if cfg!(windows) {
                p.set_extension("exe");
            }
            p
        }
    };

    // Step 4: build the bundle on a TEMP sibling, then atomically rename onto `out_path` at the
    // very end. Every prior step (write stub → chmod → macOS sign → append payload+footer)
    // touches ONLY the temp path, so a symlink-swap of `out_path` can no longer redirect the
    // chmod / sign / append onto an arbitrary file — all the TOCTOU windows collapse into the
    // single final `rename`. CRITICAL: the macOS ad-hoc sign still runs on the CLEAN stub
    // BEFORE the payload is appended, so the signature's `codeLimit` covers only the stub and
    // the loader ignores the trailing overlay — that ordering is preserved on the temp file.
    let tmp_path = {
        let mut p = out_path.clone();
        let ext = out_path
            .extension()
            .map(|e| format!("{}.{}.tmp", e.to_string_lossy(), std::process::id()))
            .unwrap_or_else(|| format!("{}.tmp", std::process::id()));
        p.set_extension(ext);
        p
    };
    // Any early return from here on must not leak the temp file: a tiny RAII guard removes it
    // unless `disarm`ed right before the successful rename.
    struct TmpGuard(std::path::PathBuf, bool);
    impl Drop for TmpGuard {
        fn drop(&mut self) {
            if self.1 {
                let _ = std::fs::remove_file(&self.0);
            }
        }
    }
    let mut guard = TmpGuard(tmp_path.clone(), true);

    std::fs::write(&tmp_path, &stub)
        .map_err(|e| AsError::new(format!("cannot write {}: {}", tmp_path.display(), e)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tmp_path)
            .map_err(|e| AsError::new(format!("cannot stat {}: {}", tmp_path.display(), e)))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tmp_path, perms).map_err(|e| {
            AsError::new(format!("cannot chmod +x {}: {}", tmp_path.display(), e))
        })?;
    }
    crate::bundle::adhoc_sign_macos(&tmp_path).map_err(AsError::new)?;

    // Step 5: append `payload || footer` AFTER the (now-signed) stub. `payload_offset` is the
    // on-disk size of the signed stub — signing rewrites `__LINKEDIT`, so it may differ from
    // `stub.len()`; read it back rather than assuming.
    let payload_offset = std::fs::metadata(&tmp_path)
        .map_err(|e| AsError::new(format!("cannot stat {}: {}", tmp_path.display(), e)))?
        .len();
    let footer = crate::bundle::write_footer(
        payload_offset,
        payload.len() as u64,
        crate::vm::aso::ASO_FORMAT_VERSION,
    );
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&tmp_path)
            .map_err(|e| AsError::new(format!("cannot open {} to append: {}", tmp_path.display(), e)))?;
        f.write_all(&payload)
            .and_then(|()| f.write_all(&footer))
            .map_err(|e| AsError::new(format!("cannot append payload to {}: {}", tmp_path.display(), e)))?;
    }

    // Step 6: atomic publish — a single `rename` makes the fully-built bundle appear at
    // `out_path` (replacing any prior file in one syscall). Only on success do we disarm the
    // cleanup guard so it does NOT delete the now-renamed file.
    std::fs::rename(&tmp_path, &out_path).map_err(|e| {
        AsError::new(format!(
            "cannot finalize {} (rename from {}): {}",
            out_path.display(),
            tmp_path.display(),
            e
        ))
    })?;
    guard.1 = false;

    let total = payload_offset + payload.len() as u64 + crate::bundle::FOOTER_SIZE as u64;
    println!(
        "bundled {} -> {} ({} bytes)",
        file.display(),
        out_path.display(),
        total
    );
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

/// SELF-CONTAINED-BUNDLES (Task 3.3): MONOTONE launch-time capability subtraction via the
/// `ASCRIPT_DENY` env var. A deployer can FURTHER restrict an already-built `.aso`/bundle at
/// launch (`ASCRIPT_DENY=fs ./app`) — it can ONLY subtract (`CapSet::deny`), NEVER re-grant.
/// Unset/empty/whitespace → `caps` unchanged (the common case, zero behavior change). An
/// unknown name is a clean STARTUP `AsError` (the `?` at each call site aborts before any code
/// runs — non-zero exit), matching the `--deny` error grammar in `compose_caps`. Wired ONLY
/// into the `.aso`/bundle launch paths (`run_verified_aso`/`run_verified_archive`); a source
/// run (`ascript run x.as`) restricts via CLI `--deny` instead.
fn apply_ascript_deny(
    mut caps: crate::stdlib::caps::CapSet,
) -> Result<crate::stdlib::caps::CapSet, AsError> {
    let raw = match std::env::var("ASCRIPT_DENY") {
        Ok(v) => v,
        Err(_) => return Ok(caps), // unset (or non-UTF-8) → no further restriction
    };
    if raw.trim().is_empty() {
        return Ok(caps);
    }
    for name in raw.split(',') {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        match crate::stdlib::caps::cap_name(name) {
            Some(cap) => caps.deny(cap), // MONOTONE: only ever subtracts
            None => {
                return Err(AsError::new(format!(
                    "ASCRIPT_DENY: unknown capability '{name}' (expected one of: fs, net, process, ffi, env)"
                )))
            }
        }
    }
    Ok(caps)
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
    use crate::vm::Vm;

    // SELF-CONTAINED-BUNDLES (Task 1.5): magic-dispatch. A multi-module `build`/`--native`
    // emits an `ASCRIPTA` archive embedding the whole graph; decode + run it via the
    // archive runner. A single-module artifact leads with `ASO\0` and falls through to the
    // unchanged single-chunk path below — byte-identical to the pre-archive run.
    if payload.starts_with(&crate::vm::archive::ARCHIVE_MAGIC) {
        let archive = crate::vm::archive::ModuleArchive::decode(payload)
            .map_err(|e| AsError::new(format!("cannot load {what}: {e}")))?;
        return run_verified_archive(archive, script_args, caps, module_dir, what).await;
    }

    let chunk = crate::vm::chunk::Chunk::from_bytes_verified(payload)
        .map_err(|e| AsError::new(format!("cannot load {what}: {e}")))?;

    let interp = Rc::new(Interp::new_live());
    interp.set_cli_args(script_args);
    // FFI §4.5: install the composed capability set before running any code. Start from the
    // passed-in caps (or all-granted for a native bare-chunk bundle with no CLI `--deny`),
    // then apply the MONOTONE `ASCRIPT_DENY` launch-time subtraction (Task 3.3) so even an
    // unrestricted bundle can be tightened by `ASCRIPT_DENY=fs ./app` — `?` makes an invalid
    // name a clean startup error (the program never runs). `script_args` is untouched.
    let caps = caps.unwrap_or_else(crate::stdlib::caps::CapSet::all_granted);
    let caps = apply_ascript_deny(caps)?;
    interp.set_caps(caps);
    // Workers Spec A (.aso path): retain the raw bytes so `dispatch_worker_closure` can
    // re-parse them into the top-level chunk and build a worker code slice without source.
    interp.set_worker_aso_bytes(Rc::from(payload));
    interp.install_self();
    let vm = Vm::new(interp.clone());
    // Resolve relative imports against the .aso's (or the executable's) directory.
    if let Some(dir) = module_dir {
        vm.set_module_dir(dir);
    }

    run_entry_proto_to_exit(&interp, &vm, chunk).await
}

/// SELF-CONTAINED-BUNDLES (Task 1.5) — the PRODUCTION runner for an `ASCRIPTA` module
/// archive (mirrors [`run_verified_aso`]'s single-chunk body, dispatched into from the
/// shared magic-routing above). The entry chunk is the program start; every reachable
/// module is embedded, so the program runs with NO source tree on disk.
///
/// Unlike the [`run_archive`] test seam (`Interp::new()`, captured output, cwd module
/// dir), this uses `Interp::new_live` (streamed output), honors the passed-in CLI `caps`,
/// and — CRUCIALLY — calls `vm.set_module_dir(dir)` BEFORE `set_module_archive` when a
/// `module_dir` is known (the `.aso`'s / executable's directory), so an archive MISS can
/// still resolve a sibling on-disk source (the Task 1.4 carry-forward). `set_module_archive`
/// then seeds the entry's logical dir to the archive root (`""`).
///
/// CAPS (Task 3.2, N4): the archive's embedded `archive.caps` floor is composed with the
/// passed-in run-time `caps` by MONOTONE INTERSECTION (`restrict_with`) and installed —
/// a run-time `--deny` can only narrow the floor, never re-grant a build-time denial. Full
/// archive→worker parity IS shipped (Task 1.6): the ENTRY chunk's bytes go to
/// `set_worker_aso_bytes` (so a worker fn's code slice still builds from the entry chunk),
/// AND the whole encoded archive is stashed via `set_worker_archive_bytes` so every worker
/// isolate decodes + installs it before re-running the program's top-level imports.
async fn run_verified_archive(
    archive: crate::vm::archive::ModuleArchive,
    script_args: &[String],
    caps: Option<crate::stdlib::caps::CapSet>,
    module_dir: Option<std::path::PathBuf>,
    what: &str,
) -> Result<i32, AsError> {
    use crate::vm::Vm;

    // The entry module's verified chunk is the program start, decoded through the SAME
    // `from_bytes_verified` trust boundary the disk `.aso` path uses. A bounds-check on the
    // entry index yields a clean error rather than a panic (decode already validates this,
    // but never index without a check on possibly-foreign data).
    let entry_bytes = archive
        .modules
        .get(archive.entry as usize)
        // clone the entry chunk out before `archive` is moved into `Rc::new` below
        .map(|(_, b)| b.clone())
        .ok_or_else(|| {
            AsError::new(format!("cannot load {what}: archive entry index is out of range"))
        })?;
    let chunk = crate::vm::chunk::Chunk::from_bytes_verified(&entry_bytes)
        .map_err(|e| AsError::new(format!("cannot load {what}: {e}")))?;

    // SELF-CONTAINED-BUNDLES (Task 3.2, N4): compose the archive's EMBEDDED capability floor
    // with the run-time (CLI/manifest) caps by MONOTONE INTERSECTION — a capability is
    // effective only if BOTH the build-time floor AND the run-time set grant it, so a
    // run-time flag can only narrow further and can NEVER re-grant what the build denied.
    // `restrict_with` borrows `archive.caps` by ref — call it BEFORE `archive` is moved
    // into `encode()` / `Rc::new` below. A native bundle passes `caps = None` (the startup
    // shim runs before clap, so there are no run-time `--deny` flags) → the effective set is
    // exactly the embedded floor; `run x.aso --deny X` intersects the floor with `{X denied}`.
    // The effective set is installed ALWAYS.
    let effective_caps = archive
        .caps
        .restrict_with(&caps.unwrap_or_else(crate::stdlib::caps::CapSet::all_granted));
    // SELF-CONTAINED-BUNDLES (Task 3.3): apply the MONOTONE `ASCRIPT_DENY` launch-time
    // subtraction on top of the composed floor — a native bundle has NO CLI `--deny` (the
    // startup shim runs before clap), so `ASCRIPT_DENY` is the only launch-time restriction
    // knob. It can ONLY subtract (never re-grant); `?` turns an invalid name into a clean
    // startup error before any code runs. Idempotent w.r.t. the floor (denying twice = denied).
    let effective_caps = apply_ascript_deny(effective_caps)?;

    let interp = Rc::new(Interp::new_live());
    interp.set_cli_args(script_args);
    // FFI §4.5 + BNDL N4: install the composed (embedded-floor ∩ CLI/manifest) capability
    // set before running any code.
    interp.set_caps(effective_caps);
    // Workers Spec A: retain the ENTRY chunk bytes so `dispatch_worker_closure` can re-parse
    // them to build a worker fn's code slice.
    interp.set_worker_aso_bytes(Rc::from(entry_bytes.as_slice()));
    // SELF-CONTAINED-BUNDLES Task 1.6: stash the WHOLE encoded archive so every worker isolate
    // decodes + installs it on its own `Vm` before re-running the program's top-level imports
    // (a worker that calls into an imported module would otherwise fail on the archive-less
    // isolate). Encode BEFORE `archive` is moved into `Rc::new` at `set_module_archive` below.
    interp.set_worker_archive_bytes(Rc::from(archive.encode().as_slice()));
    interp.install_self();
    let vm = Vm::new(interp.clone());
    // Seed the on-disk fallback dir BEFORE installing the archive: `set_module_archive`
    // overrides the entry's LOGICAL dir to the archive root, but an archive-miss still
    // resolves sibling sources against `module_dir` on disk (Task 1.4 carry-forward).
    if let Some(dir) = module_dir {
        vm.set_module_dir(dir);
    }
    vm.set_module_archive(Rc::new(archive));

    run_entry_proto_to_exit(&interp, &vm, chunk).await
}

/// The shared run tail behind [`run_verified_aso`] and [`run_verified_archive`]: wrap the
/// entry `chunk` in a top-level proto, run it on a `LocalSet`, flush telemetry, end-of-run
/// GC, then map the `RunOutcome`/`Control` to a process exit code. Borrow discipline is the
/// callers' (no `RefCell`/resource borrow is held across the `.await` here).
async fn run_entry_proto_to_exit(
    interp: &Rc<Interp>,
    vm: &crate::vm::Vm,
    chunk: crate::vm::chunk::Chunk,
) -> Result<i32, AsError> {
    use crate::vm::chunk::FnProto;
    use crate::vm::value_ext::{Closure, RunOutcome};

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
        name_span: None,
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
    run_file_on_vm_with_packages(path, script_args, None, None, ELIDE_DEFAULT_ON).await
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

/// Collect BLOCKING semantic diagnostics from the CST resolver that BOTH engines
/// must reject identically BEFORE running (the shared `run` gate, alongside
/// [`collect_parse_errors`]). A diagnostic is blocking iff its `blocking` flag is
/// set (today: the `or-pattern-binding` error — an or-pattern whose alternatives
/// bind different name sets) — a compile error, not a runtime divergence. The same
/// diagnostics are surfaced by the VM compiler (so a direct `vm_run_source` rejects
/// too) and by `ascript check`; routing the tree-walker `run` path through this gate
/// makes ALL of them byte-identical.
///
/// An empty `Vec` means there is nothing blocking (the run proceeds on either
/// engine). Returns `AsError`s with the file source bound for caret rendering and
/// CHAR-offset spans (resolver ranges are BYTE offsets, converted here).
pub fn collect_blocking_diagnostics(path: &Path) -> Vec<AsError> {
    let Ok(src) = std::fs::read_to_string(path) else {
        return Vec::new(); // a read error is handled by the runner's own report
    };
    let src_info = Rc::new(SourceInfo {
        path: path.display().to_string(),
        text: src.clone(),
    });
    let tree = crate::syntax::parse_to_tree(&src);
    let resolved = crate::syntax::resolve::resolve(&tree);
    resolved
        .diagnostics
        .iter()
        .filter(|d| d.blocking)
        .map(|d| {
            let span = crate::span::Span::new(
                byte_to_char_offset(&src, usize::from(d.range.start())),
                byte_to_char_offset(&src, usize::from(d.range.end())),
            );
            AsError::at(d.message.clone(), span).with_source(src_info.clone())
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
    elide: bool,
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
    // ELIDE §4.2/§5: compile the ENTRY module with contract elision when enabled.
    // The collector runs on this module's own source; the compiler drops proven
    // CheckLocal/return checks and emits CallElided at proven call sites. Off (the
    // default kill-switch state) → byte-identical to pre-ELIDE compilation.
    let chunk = if elide {
        let set = crate::check::infer::elision_proofs(&src);
        crate::compile::compile_source_with_elision(&src, Some(&set))
            .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?
    } else {
        crate::compile::compile_source(&src)
            .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?
    };
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
    // LANE §6.1: `ASCRIPT_NO_SYNC_LANE=1` suppresses the sync-lane driver on the
    // CLI path (worker isolates inherit this at construction via Vm::with_specialize
    // → with_lanes reading the same env). Speed-only, never observable behavior.
    let specialize = std::env::var("ASCRIPT_NO_SPECIALIZE").as_deref() != Ok("1");
    let vm = Vm::with_specialize(interp.clone(), specialize);
    // ELIDE §4.2: propagate the elision decision to the import loader so imported
    // modules are compiled with elision too (the entry module was already compiled
    // with it above). Off → imports compile byte-identically to pre-ELIDE.
    vm.set_elide(elide);
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
        name_span: None,
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

/// DECODE Task 4 (cross-module provenance test): run a `.as` FILE with an
/// explicit decode kill-switch + warmth threshold, returning the same exit/error
/// shape as [`run_file_on_vm_with_packages`]. Lets the §2.4 `last_fault_source`
/// hoisting be asserted across a real module boundary (forced-decode vs byte).
/// `#[doc(hidden)]` — test API only.
#[doc(hidden)]
pub async fn run_file_decode_cfg(
    path: &Path,
    decode: bool,
    decode_threshold: u16,
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
    chunk.set_module_source(&src_info);

    let interp = Rc::new(Interp::new_live());
    interp.set_worker_source(&src);
    interp.install_self();
    // Build a specialized VM, then override the decode kill switch + threshold so
    // the test can FORCE decode (threshold 0) or DISABLE it independent of env.
    let vm = Vm::with_all_flags(interp.clone(), true, true, true, decode, true, true, decode_threshold);
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
        name_span: None,
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
    match result {
        Ok(RunOutcome::Done(_)) => Ok(0),
        Ok(RunOutcome::Yielded(_)) => unreachable!("top-level program cannot yield"),
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        Err(crate::interp::Control::Propagate(_)) => Ok(0),
        Err(crate::interp::Control::Exit(code)) => Ok(code),
    }
}

/// DECODE Task 4: run a `.as` FILE with decode DISABLED (byte dispatch). See
/// [`run_file_decode_cfg`]. `#[doc(hidden)]` — test API only.
#[doc(hidden)]
pub async fn run_file_no_decode(path: &Path) -> Result<i32, AsError> {
    use crate::vm::Vm;
    run_file_decode_cfg(path, false, Vm::DECODE_THRESHOLD).await
}

/// DECODE Task 4: run a `.as` FILE with decode FORCED on (threshold 0). See
/// [`run_file_decode_cfg`]. `#[doc(hidden)]` — test API only.
#[doc(hidden)]
pub async fn run_file_decoded_forced(path: &Path) -> Result<i32, AsError> {
    run_file_decode_cfg(path, true, 0).await
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
        name_span: None,
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
/// LANE §6.1: `sync_lane` defaults `true` here (env is read inside
/// `Vm::with_specialize`); the explicit test helpers use `vm_run_source_cfg` directly.
async fn vm_run_source_with(src: &str, specialize: bool) -> Result<(String, Option<i32>), AsError> {
    // sync_lane = true here: the env-read default inside with_specialize applies;
    // we pass it explicitly so vm_run_source_cfg doesn't re-read the env.
    vm_run_source_cfg(src, specialize, false, false, true).await
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
    vm_run_source_cfg(src, true, true, false, true).await
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
    vm_run_source_cfg(src, true, false, true, true).await
}

/// Shared VM-run body. Parameters:
/// - `specialize`: V11-T5 kill switch — when `false`, all IC/adaptive fast paths skipped.
/// - `armed`:      DBG zero-cost bench — attach an empty instrumentation payload.
/// - `coverage`:   DX D2 Task 7 — arm line coverage (patches breakpoints).
/// - `sync_lane`:  LANE §6.1 kill switch — when `false`, sync-lane driver is suppressed.
/// - `call_fast`:  CALL §8.1 kill switch — when `false`, all CALL fast paths skipped.
///   NOTE: `armed`/`coverage` force the instrumentation path which uses
///   `Vm::with_instrument` (always specialize=true, sync_lane from env). When
///   `armed` or `coverage` is true, `sync_lane` and `call_fast` are irrelevant
///   (instrumentation path uses its own constructor).
async fn vm_run_source_cfg(
    src: &str,
    specialize: bool,
    armed: bool,
    coverage: bool,
    sync_lane: bool,
) -> Result<(String, Option<i32>), AsError> {
    vm_run_source_cfg_call_fast(src, specialize, armed, coverage, sync_lane, specialize).await
}

/// Like [`vm_run_source_cfg`] but with explicit `call_fast` control (CALL §8.1).
async fn vm_run_source_cfg_call_fast(
    src: &str,
    specialize: bool,
    armed: bool,
    coverage: bool,
    sync_lane: bool,
    call_fast: bool,
) -> Result<(String, Option<i32>), AsError> {
    let (output, exit, _, _) =
        vm_run_source_cfg_stats(src, specialize, armed, coverage, sync_lane, call_fast).await?;
    Ok((output, exit))
}

/// Like [`vm_run_source_cfg`] but also returns the LANE counters (LANE §6.4):
/// `(output, exit_code, lane_sync_ops, lane_bursts)`.
async fn vm_run_source_cfg_stats(
    src: &str,
    specialize: bool,
    armed: bool,
    coverage: bool,
    sync_lane: bool,
    call_fast: bool,
) -> Result<(String, Option<i32>, u64, u64), AsError> {
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
        name_span: None,
    });
    let closure = Closure::new(proto.clone());

    let interp = Rc::new(Interp::new());
    interp.install_self();
    // Workers Spec A: retain the source so a `worker fn` call can build its slice.
    interp.set_worker_source(src);
    // The instrumentation config (DBG Task 9 / DX D2 Task 7 — the zero-cost bench seam):
    //   `coverage` → an armed CoverageTable + `arm_coverage` (config 3: --coverage on);
    //   `armed`    → an EMPTY instrumentation payload (the attached-but-idle config);
    //   else       → `instrument == None` (the production path, sync_lane respected).
    // LANE §6.1: instrument paths use with_instrument (which calls with_specialize,
    // which reads the env for sync_lane). The plain path uses with_lanes to set sync_lane
    // explicitly without a second env read.
    let vm = if coverage {
        let mut inst = crate::vm::instrument::Instrumentation::empty();
        inst.coverage = Some(crate::vm::instrument::CoverageTable::new());
        let vm = Vm::with_instrument(interp.clone(), inst);
        vm.arm_coverage(&proto);
        vm
    } else if armed {
        Vm::with_instrument(interp.clone(), crate::vm::instrument::Instrumentation::empty())
    } else {
        // LANE §6.1 / CALL §8.1: use with_flags so both lane and call_fast kill
        // switches are set explicitly (no env re-read for test entry points).
        Vm::with_flags(interp.clone(), specialize, sync_lane, call_fast)
    };
    let mut fiber = crate::vm::fiber::Fiber::new(closure);

    let local = tokio::task::LocalSet::new();
    let result = local.run_until(vm.run(&mut fiber)).await;
    local.await; // drain spawned tasks — no-op until later VM slices
                 // End-of-program cycle collection (V13-T3): see `run_aso_file`. The
                 // output is already captured on `interp`, so a final sweep of dead
                 // cycles is observably invisible.
    crate::gc::collect();
    // LANE §6.4: read counters after the run completes (Task 4 wires these up;
    // for now they are always 0).
    let lane_sync_ops = vm.lane_sync_ops();
    let lane_bursts = vm.lane_bursts();
    let pair = match result {
        Ok(RunOutcome::Done(_)) => Ok((interp.output(), None)),
        Ok(RunOutcome::Yielded(_)) => unreachable!("top-level program cannot yield"),
        // A panic aborts the program with its diagnostic.
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        // A top-level `?` propagation simply ends the program.
        Err(crate::interp::Control::Propagate(_)) => Ok((interp.output(), None)),
        // exit(n) — return the captured output plus the exit code.
        Err(crate::interp::Control::Exit(code)) => Ok((interp.output(), Some(code))),
    }?;
    Ok((pair.0, pair.1, lane_sync_ops, lane_bursts))
}

/// **SELF-CONTAINED-BUNDLES Phase 2 (Task 2.5) test seam.** Run a multi-file program from
/// DISK on the specialized VM with CAPTURED stdout, resolving relative `import`s against the
/// entry file's directory (`set_module_dir`) — NO archive installed, so every import hits
/// disk and NOTHING is tree-shaken. This is the inherently-unshaken baseline (B) the
/// shaken-vs-unshaken differential compares the archive run against. It mirrors
/// [`vm_run_source`]'s capture but is file/dir-aware (the bare `vm_run_source` can't load
/// relative imports). `#[doc(hidden)]` test API.
#[doc(hidden)]
pub async fn vm_run_file_captured(entry: &Path) -> Result<(String, Option<i32>), AsError> {
    use crate::vm::chunk::FnProto;
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;

    let src = std::fs::read_to_string(entry)
        .map_err(|e| AsError::new(format!("cannot read {}: {}", entry.display(), e)))?;
    let src_info = Rc::new(SourceInfo {
        path: entry.display().to_string(),
        text: src.clone(),
    });
    let chunk = crate::compile::compile_source(&src)
        .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?;
    // Match production (`run_file_on_vm_with_packages`): bind the module source onto the proto
    // tree so a Tier-2 panic renders with source context, instead of silently diverging.
    chunk.set_module_source(&src_info);
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
        name_span: None,
    });
    let closure = Closure::new(proto);

    let interp = Rc::new(Interp::new());
    interp.install_self();
    interp.set_worker_source(&src);
    let vm = Vm::new(interp.clone());
    // Resolve relative imports against the entry's directory (disk loader, no archive).
    if let Some(dir) = entry.parent() {
        vm.set_module_dir(dir.to_path_buf());
    }
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

/// **SELF-CONTAINED-BUNDLES Phase 1 (Task 1.4).** Run a program PURELY from an in-memory
/// [`ModuleArchive`] — its entry chunk plus every reachable module is embedded, so the
/// program runs with NO source tree on disk. The entry chunk (`archive.modules[entry]`)
/// is the start; every relative `import` it (transitively) makes is satisfied by an
/// archive lookup by logical key in [`Vm::load_file_module`] (NOT disk), proving the
/// runtime loader reproduces the exact key `compile_archive` stored.
///
/// Output is CAPTURED (returned), like [`vm_run_source`], so a test can assert it equals
/// the on-disk run. The embedded chunks pass through `from_bytes_verified` (the SAME trust
/// boundary as `run file.aso`), so a corrupt embedded chunk is a clean error.
///
/// Task 1.5 wires up WHO installs the archive (build/`--native`/run dispatch); this is the
/// loader-facing seam that 1.5 and the headline test drive. `#[doc(hidden)]` test/seam API.
#[doc(hidden)]
pub async fn run_archive(
    archive: crate::vm::archive::ModuleArchive,
) -> Result<(String, Option<i32>), AsError> {
    use crate::vm::chunk::FnProto;
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;

    // The entry module's verified chunk is the program start. Decode through the SAME
    // trust boundary the disk `.aso` path uses. Clone the entry bytes OUT (bounds-checked,
    // clean error) before `archive` is moved into `Rc::new` below — they are stashed as the
    // worker `.aso` bytes too (Task 1.6 parity, mirroring `run_verified_archive`).
    let entry_bytes = archive
        .modules
        .get(archive.entry as usize)
        .map(|(_, b)| b.clone())
        .ok_or_else(|| AsError::new("archive entry index is out of range"))?;
    let chunk = crate::vm::chunk::Chunk::from_bytes_verified(&entry_bytes)
        .map_err(|e| AsError::new(format!("cannot load archive entry module: {e}")))?;

    let interp = Rc::new(Interp::new());
    // SELF-CONTAINED-BUNDLES Task 1.6: stash the whole encoded archive so a worker isolate
    // spawned by this captured-output run installs it before re-running top-level imports,
    // AND the ENTRY chunk bytes so a worker fn's code slice can build from them — together
    // these give full archive→worker parity in the test path too (it mirrors
    // `run_verified_archive`). Encode BEFORE `archive` moves into `Rc::new` below.
    interp.set_worker_archive_bytes(Rc::from(archive.encode().as_slice()));
    interp.set_worker_aso_bytes(Rc::from(entry_bytes.as_slice()));
    interp.install_self();
    let vm = Vm::new(interp.clone());
    // Install the archive so every relative import resolves from memory by logical key.
    // The entry's logical dir is seeded to the archive root ("") by `set_module_archive`.
    // `module_dir` is INTENTIONALLY left at cwd: an archive is expected to be self-contained,
    // so a disk fallback never fires here. A future path-carrying dispatch (Task 1.5, e.g.
    // `ascript run app.aso`) MUST call `set_module_dir` to the archive's parent so an
    // archive-miss can still resolve sibling sources on disk.
    vm.set_module_archive(Rc::new(archive));

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
        name_span: None,
    });
    let closure = Closure::new(proto);
    let mut fiber = crate::vm::fiber::Fiber::new(closure);

    let local = tokio::task::LocalSet::new();
    let result = local.run_until(vm.run(&mut fiber)).await;
    local.await;
    crate::gc::collect();
    match result {
        Ok(RunOutcome::Done(_)) => Ok((interp.output(), None)),
        Ok(RunOutcome::Yielded(_)) => unreachable!("top-level program cannot yield"),
        Err(crate::interp::Control::Panic(e)) => Err(e),
        Err(crate::interp::Control::Propagate(_)) => Ok((interp.output(), None)),
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
