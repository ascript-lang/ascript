// RT §2.2/§2.3(g): under the runtime-only build a number of items compile but are
// statically unreachable (their only callers are the gated-out source/toolchain entry
// points) — e.g. the tree-walker eval helpers, the source-running test entries, and the
// `Interp` source-load seams. The linker dead-strips them; this crate-level allow keeps
// the stub build clean WITHOUT carving them out textually (the spec's evidence-gated
// follow-up). Scoped to `ascript_rt` ONLY — normal builds keep the full dead-code lint.
#![cfg_attr(ascript_rt, allow(dead_code))]

pub mod ast;
pub mod bundle;
pub mod cache;
// RT §2.2 — the FRONT-END (parsers, compiler, checker, fmt, repl) is compiled OUT of
// the runtime-only `ascript-rt` bin under `cfg(ascript_rt)`. Normal builds (the cfg
// unset) are byte-identical.
#[cfg(not(ascript_rt))]
pub mod check;
/// The clap derive types (`Cli`, `Command`, `CapFlags`) + `cli_command()` —
/// the single source of truth for the CLI surface. Consumed by `src/main.rs`
/// for parsing and by `tests/docs_drift.rs` for drift introspection (spec §4.1).
#[cfg(not(ascript_rt))]
pub mod cli_surface;
#[cfg(not(ascript_rt))]
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
// RT §2.2: the tree-walker AST elision-marking pass (front-end product) — gated OUT of
// the runtime-only build (it consumes `crate::ast` + the checker's `ElisionSet`).
#[cfg(not(ascript_rt))]
pub mod elide_mark;
pub mod env;
pub mod error;
#[cfg(not(ascript_rt))]
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
#[cfg(not(ascript_rt))]
pub mod parser;
// DBG Task 7: the CPU sampling profiler's aggregation + output (speedscope JSON +
// collapsed folded-stacks). Feature-gated (`profile`, default-on); the publish seam
// itself lives on the VM (`Vm::publish_profile_frames`) behind the single
// `Vm.instrument` gate. `--no-default-features` builds none of this and `--profile`
// reports a clean rebuild hint.
#[cfg(feature = "profile")]
pub mod profile;
#[cfg(not(ascript_rt))]
pub mod repl;
pub mod span;
pub mod stdlib;
// RT §2.2: the CST front-end. Under the runtime-only build only the RUNTIME data types
// the VM needs survive (`syntax::kind` + `syntax::resolve::types`, e.g.
// `UpvalueDescriptor` which the VM/`.aso` carry); the parser/lexer/format/CST machinery
// is gated out INSIDE the module. Normal builds compile the whole front-end.
pub mod syntax;
pub mod task;
// DX D2 Task 10: `--filter PATTERN` test-name filtering (substring or `/regex/`) +
// `--watch` import-graph scoping. Core (no feature gate); the regex branch is
// `data`/`sys`-gated and degrades to a clean error otherwise.
pub mod test_filter;
#[cfg(not(ascript_rt))]
pub mod watch;
pub mod token;
pub mod value;
pub mod vm;
pub mod worker;
// RT §4.1–§4.2: module→feature table + archive std-import scanner.
// Toolchain-side only (data used by `ascript build --native`); excluded from
// stub builds via the `ascript_rt` cfg (set by `ASCRIPT_RT=1`, never by
// `cargo build`/`cargo test`, so normal builds are byte-identical).
// Also builds under `--no-default-features` (no optional-feature deps inside).
#[cfg(not(ascript_rt))]
pub mod rtstub;

use crate::error::AsError;
// RT §2.2: `SourceInfo` is only used by the gated-out source-running entry points.
#[cfg(not(ascript_rt))]
use crate::error::SourceInfo;
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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
    // RESIL §5.1: establish the root TASK_LOCALS scope (+ telemetry root scope when that
    // feature is on) so `resilience.deadline`/`withTrace`'s `TASK_LOCALS.try_with` finds
    // the cell in scope on the CLI tree-walker path — matching every other entry point
    // (run_source, the VM run paths). Without it the deadline/trace locals silently
    // no-op here, diverging from the VM on `ascript run file.as --tree-walker`.
    let result = local
        .run_until(crate::interp::ambient_root_scope(interp.load_module(path)))
        .await;
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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
        .run_until(crate::interp::ambient_root_scope(async {
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
        .run_until(crate::interp::ambient_root_scope(async {
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
        .run_until(crate::interp::ambient_root_scope(async {
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

#[cfg(not(ascript_rt))]
/// Lex → parse → evaluate in a fresh global environment. Returns captured output.
///
/// `exit(n)` is treated as a clean termination (the captured output is returned
/// and no error is raised). Use [`run_source_exit`] when you need the exit code.
pub async fn run_source(src: &str) -> Result<String, AsError> {
    run_source_exit(src).await.map(|(out, _)| out)
}

#[cfg(not(ascript_rt))]
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
        .run_until(crate::interp::ambient_root_scope(
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

#[cfg(not(ascript_rt))]
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
        .run_until(crate::interp::ambient_root_scope(
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

#[cfg(not(ascript_rt))]
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
    let root = crate::interp::ambient_root_scope(interp.exec_program(&program, &env));
    let result = local.run_until(root).await;
    local.await;
    match result {
        Ok(_) => Ok((interp.output(), interp)),
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        Err(crate::interp::Control::Propagate(_)) => Ok((interp.output(), interp)),
        Err(crate::interp::Control::Exit(_)) => Ok((interp.output(), interp)),
    }
}

#[cfg(not(ascript_rt))]
/// RESIL Gate-14 fix #1: run `src` on the SPECIALIZED VM and return the captured
/// output PLUS the owning `Rc<Interp>` (the shared interp the VM drives), so a test
/// can read interpreter-side state — used by the VM-mode telemetry span-lineage
/// regression test that proves a VM-mode async-fn body's span parents to the
/// spawning task's current span (the spawn-site `telemetry_scope` wrap added in
/// `src/vm/run.rs`). Mirrors [`run_source_with_interp`] but on the VM, and wraps the
/// run in [`crate::interp::ambient_root_scope`] so top-level `telemetry.span`
/// parenting works. `#[doc(hidden)]` test seam — not a public API.
#[doc(hidden)]
#[cfg(feature = "telemetry")]
pub async fn vm_run_source_with_interp(src: &str) -> Result<(String, Rc<Interp>), AsError> {
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
    let vm = Vm::new(interp.clone());
    let mut fiber = crate::vm::fiber::Fiber::new(closure);
    let local = tokio::task::LocalSet::new();
    let result = local
        .run_until(crate::interp::ambient_root_scope(vm.run(&mut fiber)))
        .await;
    local.await;
    crate::gc::collect();
    match result {
        Ok(RunOutcome::Done(_)) => Ok((interp.output(), interp)),
        Ok(RunOutcome::Yielded(_)) => unreachable!("top-level program cannot yield"),
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        Err(crate::interp::Control::Propagate(_)) => Ok((interp.output(), interp)),
        Err(crate::interp::Control::Exit(_)) => Ok((interp.output(), interp)),
    }
}

#[cfg(not(ascript_rt))]
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
        .run_until(crate::interp::ambient_root_scope(
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

#[cfg(not(ascript_rt))]
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
    // Workers Spec A: retain the source so a `worker fn` call can build its slice
    // (mirrors `run_source_exit` — required for any corpus file using workers).
    interp.set_worker_source(src);
    let env = crate::interp::global_env().child();
    let local = tokio::task::LocalSet::new();
    let result = local
        .run_until(crate::interp::ambient_root_scope(
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
/// Like [`vm_run_source`] but with DECODE FORCED on with threshold=0 — every
/// proto is decoded immediately, regardless of warmth, so even short programs
/// exercise the record driver once Task 4 lands. Pre-driver (INERT), this is
/// byte-identical to [`vm_run_source`]. `#[doc(hidden)]` — not a stable API.
#[doc(hidden)]
pub async fn vm_run_source_decoded_forced(src: &str) -> Result<(String, Option<i32>), AsError> {
    // decode=ON, threshold=0 (always decode immediately).
    vm_run_source_decode_cfg(src, true, true, true, 0).await
}

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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
        .run_until(crate::interp::ambient_root_scope(
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

#[cfg(not(ascript_rt))]
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
        .run_until(crate::interp::ambient_root_scope(
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

#[cfg(not(ascript_rt))]
/// DECODE §8.3: run `src` on the VM with DECODE FORCED (threshold=0) and return
/// a [`DecodeStats`] bundle containing the program output + all stat counters.
/// All counters are 0 until the corresponding task wires them up (INERT until
/// Task 4). `#[doc(hidden)]` — not a stable API.
#[cfg(any(test, feature = "fuzzgen", fuzzing))]
#[doc(hidden)]
pub async fn vm_run_source_decode_stats(src: &str) -> Result<DecodeStats, AsError> {
    vm_run_source_decode_stats_cfg(src, true, true, true, 0).await
}

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
/// **WARM B §3.1 — compile, train, harvest, and emit a PGO-carrying archive.**
///
/// Equivalent to [`build_file`] EXCEPT:
///
/// 1. The artifact is ALWAYS an `ASCRIPTA` archive (even for a single-module program —
///    spec §3.4; the PGO section lives outside the module table as a trailing section).
/// 2. The program is run once as a **training workload** immediately after compilation,
///    with output streaming live to stdout (`OutputSink::Live`), so the user sees the
///    training run in real time.
/// 3. After the training run completes (even if it panics — spec §3.4 bullet 3), the
///    VM's warmed inline caches and adaptive arithmetic state are snapshotted via
///    [`Vm::harvest_pgo`] and appended as a `ASPGO` trailing section.
///
/// The training panic is **absorbed**: the build succeeds with a partial section
/// (possibly empty) rather than propagating the error.
pub async fn build_file_with_pgo(
    file: &Path,
    out: Option<&Path>,
    with_debug: bool,
    caps: Option<crate::stdlib::caps::CapSet>,
    elide: bool,
) -> Result<std::path::PathBuf, AsError> {
    use crate::cache::compile_cache as cc;
    use crate::vm::archive::ModuleArchive;
    use crate::vm::chunk::FnProto;
    use crate::vm::pgo::append_section;
    use crate::vm::value_ext::Closure;
    use crate::vm::Vm;

    // ── Step 1: compile to an archive (ALWAYS archive, even single-module) ──
    let (mut archive, report) = compile_archive(file, with_debug, elide)?;
    archive.caps = caps.unwrap_or_else(crate::stdlib::caps::CapSet::all_granted);
    print_shake_report(&report);

    // Snapshot (key, sha256) pairs from the archive BEFORE encoding, so we can
    // supply them to `harvest_pgo` alongside the live proto references after the run.
    // sha256 is over the stored chunk bytes — the same value the seeder will validate.
    let module_meta: Vec<(String, [u8; 32])> = archive
        .modules
        .iter()
        .map(|(key, bytes)| (key.clone(), cc::sha256_bytes(bytes)))
        .collect();

    // ── Step 2: decode the entry chunk and set up the Vm ────────────────────
    // Decode the entry chunk (same trust boundary as `run file.aso`).
    let entry_bytes = archive
        .modules
        .get(archive.entry as usize)
        .map(|(_, b)| b.clone())
        .ok_or_else(|| AsError::new("archive entry index is out of range"))?;
    let entry_chunk =
        crate::vm::chunk::Chunk::from_bytes_verified(&entry_bytes)
            .map_err(|e| AsError::new(format!("cannot load archive entry: {e}")))?;

    // Build the Interp with LIVE output so training-run stdout streams to the
    // user's terminal (spec §3.4).
    let interp = Rc::new(Interp::new_live());
    let encoded_archive = archive.encode();
    // Stash archive + entry bytes for worker parity (mirrors `run_verified_archive`).
    interp.set_worker_archive_bytes(Rc::from(encoded_archive.as_slice()));
    interp.set_worker_aso_bytes(Rc::from(entry_bytes.as_slice()));
    interp.install_self();

    let vm = Vm::new(interp.clone());
    // Set module dir so archive-miss relative imports resolve on disk.
    if let Some(dir) = file.parent() {
        vm.set_module_dir(dir.to_path_buf());
    }
    vm.set_module_archive(Rc::new(
        ModuleArchive::decode(&encoded_archive)
            .map_err(|e| AsError::new(format!("cannot re-decode compiled archive: {e}")))?,
    ));

    // ── Step 3: drive the training run ──────────────────────────────────────
    // Keep an Rc<FnProto> alive so we can read its warmed side tables AFTER
    // the run.  The closure/fiber borrow the proto through the Rc; we hold
    // our own clone that outlives the fiber.
    let entry_proto = Rc::new(FnProto {
        chunk: entry_chunk,
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
    {
        let closure = Closure::new(Rc::clone(&entry_proto));
        let mut fiber = crate::vm::fiber::Fiber::new(closure);
        let local = tokio::task::LocalSet::new();
        // Absorb ALL outcomes (panic / propagate / exit) — a panicking training run
        // still produces a (possibly partial) PGO section (spec §3.4).
        let _ = local.run_until(vm.run(&mut fiber)).await;
        local.await;
        crate::gc::collect();
    }
    // `entry_proto` is still alive here; `fiber`/`closure` were dropped above.

    // ── Step 4: harvest the warmed IC state from the live proto tree ─────────
    // Build the slice that harvest_pgo expects: (key, sha256, &live_proto).
    // For now only the ENTRY module is harvested (the common case: single-module
    // programs and the entry of multi-module programs). Imported-module chunks are
    // loaded by the archive path and their protos are not currently surfaced here;
    // a future extension can walk `file_modules` to reach them.
    let entry_key = archive
        .modules
        .get(archive.entry as usize)
        .map(|(k, _)| k.as_str())
        .unwrap_or("");
    let entry_sha256 = module_meta
        .iter()
        .find(|(k, _)| k == entry_key)
        .map(|(_, h)| *h)
        .unwrap_or([0u8; 32]);

    let harvest_modules: &[(String, [u8; 32], &FnProto)] =
        &[(entry_key.to_owned(), entry_sha256, &entry_proto)];
    let pgo = vm.harvest_pgo(harvest_modules);

    // ── Step 5: assemble the artifact bytes ─────────────────────────────────
    // `encoded_archive` is the canonical ASCRIPTA bytes; append PGO as trailing section.
    let mut artifact_bytes = encoded_archive;
    let pgo_frame = pgo.encode();
    append_section(&mut artifact_bytes, &pgo_frame);

    // ── Step 6: write to disk ────────────────────────────────────────────────
    let out_path = match out {
        Some(p) => p.to_path_buf(),
        None => file.with_extension("aso"),
    };
    std::fs::write(&out_path, &artifact_bytes)
        .map_err(|e| AsError::new(format!("cannot write {}: {}", out_path.display(), e)))?;
    Ok(out_path)
}

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
/// WARM §2.5 — the COMPILE PATH's reachable-module enumeration, exposed as a
/// `#[doc(hidden)]` test seam. Returns `(logical_key, canonical_path)` for every
/// module `compile_archive_with_shake` would archive, in the SAME BFS order.
///
/// This is the drift-tripwire counterpart to [`crate::cache::collect_module_graph`]:
/// the cache keys/hashes the set `collect_module_graph` produces, while the archive
/// COMPILES the set this function reports. The two walks are equivalent today;
/// `tests/compile_cache.rs::collect_module_graph_matches_compile_path` asserts they
/// produce the IDENTICAL set (paths + logical keys), so a future edit that lets them
/// diverge (a false hit) fails CI loudly. Spec §2.5 wants ONE walk; this tripwire is
/// the WARM Task 3 option-(b) resolution (option (a) — a true single-source refactor —
/// was deferred as higher-risk: the cache walk compiles WITHOUT debug/elide purely to
/// read imports, while the archive walk threads `with_debug`/`elide` into stored bytes,
/// so collapsing them is a substantial refactor of `compile_archive_with_shake`).
///
/// It reuses `compile_archive_with_shake(entry, debug=true, shake=false, elide=false)`
/// — the SAME call the cache's miss path uses — and re-derives each module's canonical
/// path from its archived logical key by re-walking the graph. Rather than re-implement
/// the walk, it returns the archive's logical-key order paired with the canonical paths
/// the BFS visited (recorded into a side channel during the compile).
#[doc(hidden)]
pub fn compile_path_module_set(
    entry: &Path,
) -> Result<Vec<(String, std::path::PathBuf)>, AsError> {
    // The archive's `modules` carry logical keys in BFS order; we need the matching
    // canonical paths. `compile_archive_with_shake` does not return paths, so we run a
    // parallel BFS here using the SAME resolution primitives (`classify_specifier` +
    // `resolve_module_file`) it uses internally — this is the enumeration under test,
    // and it MUST stay in lockstep with the inline walk in `compile_archive_with_shake`.
    use crate::interp::SpecifierKind;
    use crate::vm::archive::{join_logical, logical_parent};
    use std::collections::HashMap;
    use std::path::PathBuf;

    struct Pending {
        path: PathBuf,
        key: String,
        logical_dir: String,
    }

    let interp = Interp::new();
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

    let mut result: Vec<(String, PathBuf)> = Vec::new();
    let mut seen: HashMap<PathBuf, usize> = HashMap::new();
    let mut queue: std::collections::VecDeque<Pending> = std::collections::VecDeque::new();
    queue.push_back(Pending {
        path: entry_canon.clone(),
        key: entry_key,
        logical_dir: String::new(),
    });
    seen.insert(entry_canon.clone(), 0);

    while let Some(item) = queue.pop_front() {
        let bytes = compile_verified_aso_bytes(&item.path, true, false)?;
        let chunk = crate::vm::chunk::Chunk::from_bytes_verified(&bytes).map_err(|e| {
            AsError::new(format!(
                "internal: re-decoding compiled module {} failed: {e:?}",
                item.path.display()
            ))
        })?;
        let this_disk_dir = item
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| entry_dir.clone());
        for imp in &chunk.imports {
            let source = imp.source();
            interp.set_module_dir(this_disk_dir.clone());
            match interp.classify_specifier(source) {
                SpecifierKind::Std => {}
                kind @ (SpecifierKind::Relative(_) | SpecifierKind::Package { .. }) => {
                    let target = match &kind {
                        SpecifierKind::Relative(t) => t.clone(),
                        SpecifierKind::Package { target, .. } => target.clone(),
                        _ => unreachable!("matched only Relative|Package"),
                    };
                    let dep_path = resolve_module_file(&target).map_err(|msg| {
                        AsError::new(format!(
                            "cannot resolve import '{source}' from {}: {msg}",
                            item.path.display()
                        ))
                    })?;
                    if !seen.contains_key(&dep_path) {
                        let dep_key = match &kind {
                            SpecifierKind::Package { .. } => join_logical("pkg", source),
                            _ => join_logical(&item.logical_dir, source),
                        };
                        let dep_logical_dir = logical_parent(&dep_key);
                        let reserved = seen.len();
                        seen.insert(dep_path.clone(), reserved);
                        queue.push_back(Pending {
                            path: dep_path,
                            key: dep_key,
                            logical_dir: dep_logical_dir,
                        });
                    }
                }
                SpecifierKind::UnknownPackage(key) => {
                    return Err(AsError::new(format!(
                        "unknown package '{key}' — add it with 'ascript add' \
                         (imported from {})",
                        item.path.display()
                    )));
                }
            }
        }
        result.push((item.key, item.path));
    }
    Ok(result)
}

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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
/// Pure path resolution (no compiler), but reached ONLY from the gated-out compile/cache
/// paths (archive build, module-set compile, the cache rebind) — so it is non-rt only.
#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
/// WARM Task 1: expose `resolve_module_file` as a `pub(crate)` shim so
/// `src/cache/mod.rs` can follow the same resolution logic without duplicating it.
/// Identical behaviour to the private version above.
pub(crate) fn resolve_module_file_pub(target: &std::path::Path) -> Result<std::path::PathBuf, String> {
    resolve_module_file(target)
}

#[cfg(not(ascript_rt))]
/// WARM Task 1: compile `source` (already read from `path`) to verified `.aso` bytes,
/// WITHOUT debug info or elision, purely to extract the import table for the cache
/// module-graph walk. Uses the same verification path as `compile_verified_aso_bytes`
/// but avoids a redundant disk read.
///
/// `pub(crate)` — consumed by `src/cache/mod.rs`; not a public API.
pub(crate) fn compile_verified_aso_bytes_from_source_for_cache(
    path: &std::path::Path,
    source: &str,
) -> Result<Vec<u8>, AsError> {
    let src_info = Rc::new(SourceInfo {
        path: path.display().to_string(),
        text: source.to_string(),
    });
    let chunk = crate::compile::compile_source(source)
        .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?;
    crate::vm::verify::verify(&chunk).map_err(|e| {
        AsError::new(format!(
            "internal: produced bytecode failed verification: {e}"
        ))
        .with_source(src_info)
    })?;
    chunk
        .to_bytes_with_debug(false)
        .map_err(|e| AsError::new(format!("cannot serialize bytecode: {e}")))
}

/// RT §4.4/§9.2 — the option bundle for [`build_native`]. Refactored from a positional
/// signature into a struct ONCE (RT Task 5) so later tasks add fields without re-touching
/// every call site. [`Default`] reproduces TODAY's exact behavior (no target, automatic
/// host-tier selection with the `current_exe()` stub, no report) — every existing
/// `--native` build is byte-identical under the default opts.
///
/// Fields beyond `compress`/`tier`/`report_json` are placeholders for later tasks
/// (`stub`/`exact`/`oci`/`no_fetch` — Tasks 6–9); they are reserved here so the
/// signature is stable.
#[cfg(not(ascript_rt))]
#[derive(Debug, Clone, Default)]
pub struct NativeBuildOpts {
    /// `--target` triple (RT Task 7 un-rejects this; still rejected for now). `None` ⇒
    /// the host platform.
    pub target: Option<String>,
    /// `--tier` override (RT §4.4). `None` ⇒ automatic nearest-superset selection.
    pub tier: Option<crate::rtstub::tiers::Tier>,
    /// `--compress` — zstd-compress the embedded payload (RT §7).
    pub compress: bool,
    /// `--report-json <PATH|->` — emit the §9.2 JSON build report. `None` ⇒ no JSON
    /// (the stderr human report still prints for every `--native` build). `Some("-")`
    /// ⇒ stdout.
    pub report_json: Option<String>,
    /// `--stub <path>` — an explicit local stub (RT §5.4 rung 1). `None` ⇒ walk the rest
    /// of the ladder (cache/fetch/sibling/current_exe).
    pub stub: Option<std::path::PathBuf>,
    /// `--no-fetch` — skip the network rung (RT §5.4 rung 3). Availability fall-through.
    pub no_fetch: bool,
    /// `--strip` — omit the optional DBG debug section from the embedded payload (RT §2.3e:
    /// a stripped bundle degrades to span-less panic messages). `false` ⇒ debug info is
    /// included (today's default for `--native`).
    pub strip: bool,
    /// `--exact` — build the stub with EXACTLY the required features via a local
    /// `cargo build` of `ascript-rt` (§4.5). Requires `$ASCRIPT_SRC` to point at a
    /// matching source checkout. Mutually exclusive with `--tier` and `--stub`.
    /// `false` ⇒ the default ladder (rung 0 skipped).
    pub exact: bool,
    /// `--oci` — produce an OCI Image Layout tarball instead of a bare native binary
    /// (RT §8). Implies `--native`. Output is `<stem>.tar` (or the `-o` path). Requires
    /// `cfg(feature = "compress")` (flate2/gzip); without it a clean error is emitted.
    /// `false` (default) ⇒ the standard `build_native` bundle output.
    pub oci: bool,
    /// `--oci-tag` — the image reference tag for the OCI `index.json` annotation
    /// (`org.opencontainers.image.ref.name`). Defaults to `<stem>:latest`.
    pub oci_tag: Option<String>,
}

#[cfg(not(ascript_rt))]
/// BIN §2.2 — `ascript build --native app.as -o app`: produce a self-contained native
/// executable that bundles the whole runtime + the compiled program. This is **bundling, not
/// AOT**: the output is a copy of the running runtime (`current_exe()`) with the *verified*
/// `.aso` payload + a trailing [`bundle`] footer appended; at startup the runtime reads its
/// own footer and runs the payload through the SAME `from_bytes_verified` path as
/// `run file.aso`.
///
/// `--target` is parsed-but-rejected in v1 (host-only — RT Task 7 un-rejects it). The
/// default output is the source stem with NO extension (`.exe` on Windows). On Unix the
/// output is `chmod +x`; on macOS it is ad-hoc signed (mandatory on arm64 — appending
/// invalidated the stub's signature).
///
/// RT §4.4/§4.6/§9.2: selects the logical stub tier from the program's std imports (the
/// actual stub stays `current_exe()` until Task 7's ladder lands), prints the §4.6 stderr
/// build report, and emits the §9.2 JSON report when `opts.report_json` is set.
pub async fn build_native(
    file: &Path,
    out: Option<&Path>,
    caps: Option<crate::stdlib::caps::CapSet>,
    elide: bool,
    opts: &NativeBuildOpts,
) -> Result<std::path::PathBuf, AsError> {
    let compress = opts.compress;
    let with_debug = !opts.strip;

    // RT §6.3: validate the `--target` triple against the published set (§3.3). An unknown
    // triple is rejected up front with an error LISTING the supported targets — never a
    // silent ignore or a generic clap failure. A `--target` equal to the host is accepted
    // and equivalent to omitting it.
    if let Some(t) = &opts.target {
        if !crate::rtstub::select::SUPPORTED_TARGETS.contains(&t.as_str()) {
            return Err(AsError::new(format!(
                "unknown --target '{t}': not a published target. Supported targets are: {}",
                crate::rtstub::select::SUPPORTED_TARGETS.join(", ")
            )));
        }
    }

    // RT §8 — `--oci` early validation: requires the `compress` feature (flate2 for gzip)
    // and a `*-unknown-linux-musl` target (scratch-base = statically linked binary). When
    // `--target` is omitted, default to `<host-arch>-unknown-linux-musl`. Reject every
    // non-musl triple with a clear error naming the musl equivalent.
    #[cfg(feature = "compress")]
    let oci_effective_target: Option<String> = if opts.oci {
        let triple = opts.target.clone().unwrap_or_else(|| {
            // Default: host arch + linux-musl.
            #[cfg(target_arch = "aarch64")]
            { "aarch64-unknown-linux-musl".to_string() }
            #[cfg(target_arch = "x86_64")]
            { "x86_64-unknown-linux-musl".to_string() }
            #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
            { "x86_64-unknown-linux-musl".to_string() }
        });
        // musl check: reject if the triple does not contain "-musl"
        if !triple.contains("-musl") {
            return Err(AsError::new(
                crate::rtstub::oci::oci_target_rejection_message(&triple)
            ));
        }
        Some(triple)
    } else {
        None
    };
    // When `compress` feature is absent, `--oci` cannot be used (clean error).
    #[cfg(not(feature = "compress"))]
    if opts.oci {
        return Err(AsError::new(
            "ascript build --oci requires compress support — rebuild with the \
             'compress' Cargo feature (enabled by default)"
        ));
    }
    // Under no-compress, this variable is unused but must exist for the code below.
    #[cfg(not(feature = "compress"))]
    let _oci_effective_target: Option<String> = None;

    // Step 1: the payload is the SAME verified bytes a `build` produces — a bare `ASO\0`
    // chunk for a single-module program (byte-identical to today's bundle) or an
    // `ASCRIPTA` archive embedding the whole import graph for a multi-module program
    // (so the bundled binary runs from an empty directory). The `stub || payload ||
    // footer` framing below is unchanged: the payload is opaque to the bundler.
    let (mut archive, report) = compile_archive(file, with_debug, elide)?;

    // RT §4.1/§4.4: select the logical stub tier from the archive's OWN import facts
    // (chunk-level truth, never the source). This drives the build report's tier +
    // unused-feature delta. The actual stub is still `current_exe()` here (Task 7 wires
    // the resolution ladder); selecting the tier now lets the report show the SELECTED
    // tier and the would-be savings even while the stub is the full toolchain binary.
    let std_imports = crate::rtstub::std_features::collect_std_imports(&archive);
    let selection = crate::rtstub::select::select(&std_imports, opts.tier)
        .map_err(|e| AsError::new(format!("tier selection failed: {e}")))?;

    let caps_all_granted = caps.is_none();
    let multi_or_capped = archive.modules.len() > 1 || caps.is_some();
    let payload_format = if multi_or_capped { "archive" } else { "aso" };
    let module_count = archive.modules.len();
    let shake_digest_hex = if multi_or_capped {
        Some(hex_digest(&archive.shake_digest))
    } else {
        None
    };

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
        compile_verified_aso_bytes(file, with_debug, elide)? // bare ASO\0 — byte-identical to today
    };

    let payload_uncompressed_len = payload.len() as u64;

    // RT §7: optional zstd compression of the payload, AFTER the (verified) encode and
    // BEFORE the append. `--compress` wraps the payload as `uncompressed_len:u64 || frame`
    // and sets `FLAG_ZSTD` (→ footer version 2). Without `--compress` the payload and footer
    // are BIT-IDENTICAL to a pre-RT bundle (flags=0, version=1) — the reproducibility floor.
    let (payload, footer_flags) = if compress {
        let raw_len = payload.len();
        let compressed = crate::bundle::compress_payload(&payload).map_err(AsError::new)?;
        eprintln!(
            "compressed payload {} -> {} bytes ({:.1}%)",
            raw_len,
            compressed.len(),
            if raw_len == 0 { 0.0 } else { 100.0 * compressed.len() as f64 / raw_len as f64 }
        );
        (compressed, crate::bundle::FLAG_ZSTD)
    } else {
        (payload, 0u16)
    };

    // Step 2: resolve the stub the payload is appended to.
    //
    // Rung 0 (--exact, §4.5): when the user passed --exact, invoke a local cargo build
    // of `ascript-rt` with EXACTLY the required features and content-address the result.
    // This bypasses all other rungs. Mutually exclusive with --stub/--tier (enforced by
    // clap); the exact module handles detection errors, signing, and the exact-index cache.
    //
    // Rungs 1–5 (--stub → cache → fetch → dev sibling → current_exe): walked when --exact
    // is not set. See `resolve_stub` for the integrity-vs-availability contract.
    let (stub, sign_locally, stub_sha256, stub_origin) = if opts.exact {
        let required_set: std::collections::BTreeSet<&str> = selection
            .required
            .iter()
            .map(|s| s.as_str())
            .collect();
        let exact_result = crate::rtstub::exact::build_exact(
            &required_set,
            opts.target.as_deref(),
            &crate::rtstub::exact::DetectContext::real(),
        )
        .map_err(AsError::new)?;
        let bytes = std::fs::read(&exact_result.bytes_path).map_err(|e| {
            AsError::new(format!(
                "cannot read exact stub {}: {}",
                exact_result.bytes_path.display(),
                e
            ))
        })?;
        // The exact stub was already signed (macOS) and is in the cache — never re-sign here.
        (bytes, false, exact_result.sha256, "--exact")
    } else {
        // Rungs 1–5: the standard five-rung ladder.
        let resolve_opts = crate::rtstub::select::ResolveOpts {
            target: opts.target.clone(),
            tier: selection.tier,
            stub: opts.stub.clone(),
            no_fetch: opts.no_fetch,
            required_features: selection.required.clone(),
            demanding: std_imports
                .iter()
                .filter_map(|spec| {
                    crate::rtstub::std_features::STD_MODULE_FEATURES
                        .iter()
                        .find(|(m, _)| *m == spec.as_str())
                        .and_then(|(_, f)| f.map(|feat| (spec.clone(), feat.to_string())))
                })
                .collect(),
        };
        let resolved = crate::rtstub::select::resolve_stub(&resolve_opts)
            .await
            .map_err(AsError::new)?;
        let stub_bytes = std::fs::read(&resolved.bytes_path).map_err(|e| {
            AsError::new(format!(
                "cannot read the resolved stub {}: {}",
                resolved.bytes_path.display(),
                e
            ))
        })?;
        // RT §6.2 / BIN sign-before-append rule: we ONLY ad-hoc sign locally for the
        // current_exe rung (a stub produced on this mac host whose signature the append
        // would otherwise invalidate). A fetched / --stub / sibling / --exact stub is
        // appended AS-IS — those arrive pre-signed (release-time CI, or our own exact
        // build), and the sign-before-append rule means an append never invalidates a
        // signature computed over the clean stub's [0, stub_len).
        let sign = resolved.origin == "current_exe";
        let sha = resolved.sha256.clone();
        let origin = resolved.origin;
        (stub_bytes, sign, sha, origin)
    };
    let stub_size = stub.len() as u64;
    let payload_sha256 = hex_digest(&sha256_bytes(&payload));

    // Step 3: choose the output path (source stem; `.exe` for a *-windows-* TARGET regardless
    // of host; NEVER `.aso`).
    let windows_target = opts
        .target
        .as_deref()
        .map(|t| t.contains("windows"))
        .unwrap_or(cfg!(windows));

    // The file stem (used for both the native bundle name and the OCI tar name).
    let stem_str: String = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "app".to_string());
    #[cfg(not(feature = "compress"))]
    let _ = &stem_str; // used only by the compress-gated OCI path

    // RT §8: when `--oci` is active, the user's `-o` path (or the default) is the FINAL OCI
    // tarball destination. The intermediate native bundle must go to a distinct temp path so
    // that `write_oci_tar`'s atomic rename does not collide with (or overwrite) the native
    // bundle before we can read it. We build the native bundle at `native_bundle_tmp`, then
    // read its bytes, write the OCI tar to `oci_out`, and delete `native_bundle_tmp`.
    #[cfg(feature = "compress")]
    let (out_path, oci_out_opt): (std::path::PathBuf, Option<std::path::PathBuf>) = if opts.oci {
        // OCI output: user's -o or <stem>.tar
        let oci_out = match out {
            Some(p) => p.to_path_buf(),
            None => {
                let mut p = std::path::PathBuf::from(&stem_str);
                p.set_extension("tar");
                p
            }
        };
        // Intermediate native bundle: a temp path alongside the OCI output.
        let native_tmp = {
            let mut p = oci_out.clone();
            p.set_extension(format!("native.{}.tmp", std::process::id()));
            p
        };
        (native_tmp, Some(oci_out))
    } else {
        let p = match out {
            Some(p) => p.to_path_buf(),
            None => {
                let stem = file
                    .file_stem()
                    .map(|s| s.to_owned())
                    .unwrap_or_else(|| std::ffi::OsString::from("app"));
                let mut p = std::path::PathBuf::from(stem);
                if windows_target {
                    p.set_extension("exe");
                }
                p
            }
        };
        (p, None)
    };
    #[cfg(not(feature = "compress"))]
    let out_path = match out {
        Some(p) => p.to_path_buf(),
        None => {
            let stem = file
                .file_stem()
                .map(|s| s.to_owned())
                .unwrap_or_else(|| std::ffi::OsString::from("app"));
            let mut p = std::path::PathBuf::from(stem);
            if windows_target {
                p.set_extension("exe");
            }
            p
        }
    };
    #[cfg(not(feature = "compress"))]
    let _oci_out_opt: Option<std::path::PathBuf> = None;

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
    // RT §6.2 — the BIN sign-before-append rule: ad-hoc sign ONLY the current_exe rung (a
    // stub freshly copied from this running mac binary, whose signature the append would
    // otherwise invalidate → SIGKILL on arm64). Fetched / `--stub` / sibling stubs are
    // appended AS-IS: they arrive pre-signed, and because the signature's `codeLimit` covers
    // only the clean stub's [0, stub_len), an append to the trailing overlay never
    // invalidates it. Signing them here would re-stamp our host identity (wrong for a cross
    // target) and is unnecessary. A no-op on non-macOS regardless.
    if sign_locally {
        crate::bundle::adhoc_sign_macos(&tmp_path).map_err(AsError::new)?;
    }

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
        footer_flags,
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

    // RT §8 — `--oci` path: if the user asked for an OCI image tarball, READ the just-written
    // native bundle back, feed it into the OCI writer, produce the OCI tar at `oci_out`, then
    // remove the intermediate native bundle. The native bundle was built to `out_path` (a temp
    // path distinct from `oci_out` — see Step 3 above — to avoid collisions). `write_oci_tar`
    // writes atomically via its own temp-rename, so the OCI tar appears atomically at `oci_out`.
    #[cfg(feature = "compress")]
    if let (Some(ref effective_target), Some(ref oci_out)) =
        (&oci_effective_target, &oci_out_opt)
    {
        let bundle_bytes = std::fs::read(&out_path).map_err(|e| {
            AsError::new(format!("cannot read bundle for OCI wrapping {}: {}", out_path.display(), e))
        })?;
        // Remove the intermediate native bundle (it was the temp path, not the user's output).
        let _ = std::fs::remove_file(&out_path);

        let arch = crate::rtstub::oci::oci_arch_from_triple(effective_target)
            .map_err(AsError::new)?;
        let tag = opts.oci_tag.clone().unwrap_or_else(|| format!("{stem_str}:latest"));

        crate::rtstub::oci::write_oci_tar(&bundle_bytes, arch, &tag, oci_out)
            .map_err(AsError::new)?;

        let oci_size = std::fs::metadata(oci_out).map(|m| m.len()).unwrap_or(0);
        println!(
            "packaged {} -> {} ({} bytes)",
            file.display(),
            oci_out.display(),
            oci_size
        );

        // RT §9.2: assemble the build report for the OCI tarball.
        let oci_bytes = std::fs::read(oci_out).map_err(|e| {
            AsError::new(format!("cannot read OCI tar for report: {}", e))
        })?;
        let report = crate::rtstub::report::BuildReport {
            source: file.display().to_string(),
            output: oci_out.display().to_string(),
            output_sha256: hex_digest(&sha256_bytes(&oci_bytes)),
            target: Some(effective_target.clone()),
            tier: selection.tier,
            tier_source: selection.source,
            selection: selection.clone(),
            payload: crate::rtstub::report::PayloadInfo {
                format: payload_format,
                compressed: compress,
                size: payload.len() as u64,
                uncompressed_size: payload_uncompressed_len,
                sha256: payload_sha256,
            },
            stub: crate::rtstub::report::StubInfo {
                origin: stub_origin,
                sha256: stub_sha256,
                size: stub_size,
            },
            module_count,
            shake_digest: shake_digest_hex,
            caps_all_granted,
        };
        eprint!("{}", report.render_stderr());
        if let Some(dest) = &opts.report_json {
            let json = report.to_json();
            if dest == "-" {
                println!("{json}");
            } else {
                std::fs::write(dest, json.as_bytes()).map_err(|e| {
                    AsError::new(format!("cannot write --report-json {dest}: {e}"))
                })?;
            }
        }
        return Ok(oci_out.clone());
    }

    println!(
        "bundled {} -> {} ({} bytes)",
        file.display(),
        out_path.display(),
        total
    );

    // RT §4.6/§9.2: assemble the build report. The artifact sha256 is computed over the
    // FINAL bytes on disk (deterministic given the inputs — §9.1). The report contains
    // NO timestamps, so a double-build yields byte-identical JSON.
    let final_bytes = std::fs::read(&out_path).map_err(|e| {
        AsError::new(format!("cannot read {} for the build report: {}", out_path.display(), e))
    })?;
    let report = crate::rtstub::report::BuildReport {
        source: file.display().to_string(),
        output: out_path.display().to_string(),
        output_sha256: hex_digest(&sha256_bytes(&final_bytes)),
        target: opts.target.clone(),
        tier: selection.tier,
        tier_source: selection.source,
        selection: selection.clone(),
        payload: crate::rtstub::report::PayloadInfo {
            format: payload_format,
            compressed: compress,
            size: payload.len() as u64,
            uncompressed_size: payload_uncompressed_len,
            sha256: payload_sha256,
        },
        stub: crate::rtstub::report::StubInfo {
            origin: stub_origin,
            sha256: stub_sha256,
            size: stub_size,
        },
        module_count,
        shake_digest: shake_digest_hex,
        caps_all_granted,
    };
    // Human report → stderr (the `bundled … -> …` line above stays on stdout).
    eprint!("{}", report.render_stderr());
    // JSON report → the requested sink (`-` = stdout) when `--report-json` was passed.
    if let Some(dest) = &opts.report_json {
        let json = report.to_json();
        if dest == "-" {
            println!("{json}");
        } else {
            std::fs::write(dest, json.as_bytes()).map_err(|e| {
                AsError::new(format!("cannot write --report-json {dest}: {e}"))
            })?;
        }
    }

    Ok(out_path)
}

/// Compute the sha256 of `bytes` (RT §9.2 — artifact/stub/payload identity).
#[cfg(not(ascript_rt))]
fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

/// Lowercase-hex encode a 32-byte digest (RT §9.2).
#[cfg(not(ascript_rt))]
fn hex_digest(d: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(64);
    for b in d {
        let _ = write!(s, "{b:02x}");
    }
    s
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

/// BIN §2.3 / RT §2.4 — the pre-clap startup shim, factored out of `src/main.rs` so
/// BOTH the toolchain `ascript` bin AND the runtime-only `ascript-rt` bin call ONE
/// implementation. If THIS executable is a native bundle (a trailing `ASCRIPTB`
/// footer over a valid payload region), read the payload, run it through the embedded
/// path, and return `Some(exit_code)`. A plain launch (no footer) returns `None` and
/// the caller falls through to its own argv handling, byte-identical to before.
///
/// Cost on the NON-bundle path (every normal launch): a `current_exe()` resolve +
/// open + stat + a single `FOOTER_SIZE`-byte tail read — it never loads the whole
/// image. Any I/O failure BEFORE footer confirmation (open / stat / footer read /
/// `validate_footer`) is treated as "not a bundle" → `None` (it may be a plain
/// launch). Once the `ASCRIPTB` magic is confirmed, a payload-read failure is a
/// REPORTED error (`Some(1)`), NOT a silent fall-through — the binary IS a bundle, so
/// a confusing "missing subcommand" usage error would be wrong. A run error reports
/// the diagnostic and returns `Some(1)`.
pub async fn run_embedded_if_bundled() -> Option<i32> {
    use std::io::{Read, Seek, SeekFrom};
    const FOOTER_SIZE: usize = crate::bundle::FOOTER_SIZE;

    let exe = std::env::current_exe().ok()?;
    let mut f = std::fs::File::open(&exe).ok()?;
    let exe_len = f.metadata().ok()?.len();
    if exe_len < FOOTER_SIZE as u64 {
        return None;
    }
    // Read ONLY the trailing footer (cheap), validate against the file length. RT §7.2:
    // the verdict is three-way — NotABundle (silent clap fall-through, pre-RT behavior),
    // Bundle (run it), or Refused (the bytes ARE a bundle but a version/flags problem makes
    // it unrunnable → a REPORTED error, never a confusing "missing subcommand").
    f.seek(SeekFrom::End(-(FOOTER_SIZE as i64))).ok()?;
    let mut footer = [0u8; FOOTER_SIZE];
    f.read_exact(&mut footer).ok()?;
    let (offset, len, flags) = match crate::bundle::validate_footer(&footer, exe_len) {
        crate::bundle::FooterCheck::NotABundle => return None,
        crate::bundle::FooterCheck::Refused(msg) => {
            eprintln!("error: {msg}");
            return Some(1);
        }
        crate::bundle::FooterCheck::Bundle { offset, len, flags } => (offset, len, flags),
    };

    // It IS a bundle (`ASCRIPTB` confirmed) — from here a read failure is REPORTED,
    // never a silent fall-through.
    let mut payload = vec![0u8; len as usize];
    if let Err(e) = f
        .seek(SeekFrom::Start(offset))
        .and_then(|_| f.read_exact(&mut payload))
    {
        eprintln!("error: failed to read embedded program: {e}");
        return Some(1);
    }
    // RT §7: decompress the payload BEFORE the `ASO\0`/`ASCRIPTA` magic dispatch when the
    // bundle is compressed. `validate_footer` has already guaranteed the codec is available
    // (it refuses `FLAG_ZSTD` on a zstd-less stub), so a decompress failure here is genuine
    // corruption → a reported error.
    if flags & crate::bundle::FLAG_ZSTD != 0 {
        match crate::bundle::decompress_payload(&payload) {
            Ok(raw) => payload = raw,
            Err(e) => {
                eprintln!("error: {e}");
                return Some(1);
            }
        }
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run_embedded_aso(&payload, &args).await {
        Ok(code) => code,
        Err(e) => {
            crate::diagnostics::report(&e);
            1
        }
    };
    Some(code)
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
        // WARM B §3.3: the PGO section (if any) is a self-described trailing section AFTER
        // the module table. Scan it from the archive end. A corrupt/absent section ⇒ `None`
        // ⇒ the program warms normally (the seeder is a no-op on `None`).
        let section = decode_trailing_pgo(payload, &archive);
        return run_verified_archive(archive, script_args, caps, module_dir, what, section).await;
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

    // A bare `ASO\0` chunk has no trailing-section mechanism (the only PGO carrier is the
    // archive container — spec §3.4), so there is never a section to seed here.
    run_entry_proto_to_exit(&interp, &vm, chunk, None).await
}

/// WARM B §3.3 — decode the optional PGO trailing section from an archive's raw bytes.
///
/// The section rides AFTER the module table (`ModuleArchive::decode` ignores trailing
/// bytes — the Task-0 pin). We locate the archive end by re-encoding the decoded archive
/// (canonical, deterministic — BNDL §4.5) and scan from there. Returns `None` if absent,
/// version-mismatched, or malformed (⇒ warm normally; never a load failure).
fn decode_trailing_pgo(
    payload: &[u8],
    archive: &crate::vm::archive::ModuleArchive,
) -> Option<crate::vm::pgo::PgoSection> {
    let archive_end = archive.encode().len();
    if archive_end >= payload.len() {
        return None; // no trailing bytes
    }
    crate::vm::pgo::find_and_decode_pgo(payload, archive_end)
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
    pgo_section: Option<crate::vm::pgo::PgoSection>,
) -> Result<i32, AsError> {
    use crate::vm::Vm;

    // The entry module's verified chunk is the program start, decoded through the SAME
    // `from_bytes_verified` trust boundary the disk `.aso` path uses. A bounds-check on the
    // entry index yields a clean error rather than a panic (decode already validates this,
    // but never index without a check on possibly-foreign data).
    let (entry_key, entry_bytes) = archive
        .modules
        .get(archive.entry as usize)
        // clone the entry chunk out before `archive` is moved into `Rc::new` below
        .map(|(k, b)| (k.clone(), b.clone()))
        .ok_or_else(|| {
            AsError::new(format!("cannot load {what}: archive entry index is out of range"))
        })?;
    // WARM B §3.3: the entry module's stored-chunk digest for the per-module seed gate.
    let entry_sha256 = crate::cache::compile_cache::sha256_bytes(&entry_bytes);
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

    let pgo_seed = pgo_section.as_ref().map(|section| PgoSeed {
        section,
        entry_key: &entry_key,
        entry_sha256: &entry_sha256,
        enabled: pgo_seeding_enabled(),
    });
    run_entry_proto_to_exit(&interp, &vm, chunk, pgo_seed).await
}

/// WARM B §3.3 — the seeding inputs threaded into [`run_entry_proto_to_exit`]: the decoded
/// PGO section, the entry module's logical key + stored-chunk digest (for the per-module
/// digest gate), and the resolved `enabled` flag (`vm.specialize && !ASCRIPT_NO_PGO`, or the
/// test seam's explicit value). Absent (`None`) when the artifact carries no PGO section.
struct PgoSeed<'a> {
    section: &'a crate::vm::pgo::PgoSection,
    entry_key: &'a str,
    entry_sha256: &'a [u8; 32],
    enabled: bool,
}

/// WARM B §3.3 — the `ASCRIPT_NO_PGO` kill switch. `true` (seed) unless the env var is set
/// to a non-empty value. Mirrors the `ASCRIPT_NO_SPECIALIZE`/`ASCRIPT_NO_DECODE` posture.
fn pgo_seeding_enabled() -> bool {
    !std::env::var("ASCRIPT_NO_PGO").is_ok_and(|v| !v.is_empty())
}

/// The shared run tail behind [`run_verified_aso`] and [`run_verified_archive`]: wrap the
/// entry `chunk` in a top-level proto, run it on a `LocalSet`, flush telemetry, end-of-run
/// GC, then map the `RunOutcome`/`Control` to a process exit code. Borrow discipline is the
/// callers' (no `RefCell`/resource borrow is held across the `.await` here).
async fn run_entry_proto_to_exit(
    interp: &Rc<Interp>,
    vm: &crate::vm::Vm,
    chunk: crate::vm::chunk::Chunk,
    pgo_seed: Option<PgoSeed<'_>>,
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
    // WARM B §3.3: seed the warmed side tables BEFORE first execution (all-sync; no borrow
    // held across the run below). Gated inside `seed_entry_from_section` on `vm.specialize`
    // and the caller's `seed` flag (the `ASCRIPT_NO_PGO` kill switch is folded into `seed`).
    if let Some(seed) = pgo_seed {
        vm.seed_entry_from_section(
            &proto.chunk,
            seed.section,
            seed.entry_key,
            seed.entry_sha256,
            seed.enabled,
        );
    }
    let closure = Closure::new(proto);
    let mut fiber = crate::vm::fiber::Fiber::new(closure);

    let local = tokio::task::LocalSet::new();
    let result = local
        .run_until(crate::interp::ambient_root_scope(vm.run(&mut fiber)))
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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
    // ELIDE §6.3 paranoid mode (default VM CLI path): when active, build the entry
    // module's ElisionSet and install it for contract-failure-path lookup. Paranoid
    // runs elide-OFF (the entry/import compiles above keep all checks because the
    // user opts in via ASCRIPT_ELIDE_PARANOID without --elide), so the set is
    // consulted ONLY when a retained check fails — zero hot-path cost. Mirrors the
    // tree-walker path in `run_file_with_packages`. Multi-module: covers the entry
    // module (imports get their own passes); sufficient for the §6 correctness gate.
    if paranoid_enabled() {
        let paranoid_set = crate::check::infer::elision_proofs(&src);
        interp.set_paranoid_set(paranoid_set);
    }
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
        .run_until(crate::interp::ambient_root_scope(vm.run(&mut fiber)))
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

#[cfg(not(ascript_rt))]
/// WARM A §2.1 — the cached `ascript run` front door (the default plain-`.as`+VM path).
///
/// Decides cacheability, looks up a content-addressed compiled artifact, and:
/// - **Hit** → run the cached, *verified* artifact through the SAME magic-routing path
///   ([`run_verified_aso`]) a `run file.aso`/bundle uses. Zero parse/resolve/compile.
/// - **Miss** → compile via
///   `compile_archive_with_shake(entry, /*debug*/true, /*shake*/false, /*elide*/false)`
///   with `archive.caps = CapSet::all_granted()` (the NEUTRAL floor — run-time caps
///   compose by monotone intersection in `run_verified_archive`, §2.6), publish the
///   artifact + manifest atomically, then run the FRESHLY-compiled bytes through the
///   SAME [`run_verified_aso`] path (hit and miss share ONE run path — no mode skew).
///
/// **FAIL-OPEN (load-bearing):** ANY cache-layer failure — a `Disabled` binary stamp, an
/// unreadable/hostile store, a keying error, an exotic import graph the walk rejects, a
/// publish IO error, or a verifier rejection of a poisoned slot — falls through to
/// [`run_file_on_vm_with_packages`] (today's uncached compile-and-run). The cache is an
/// optimization layered over an unchanged semantic path; it is NEVER the reason a run
/// errors. A verifier rejection additionally DELETES the poisoned slot first (fail
/// CLOSED to recompile), so the next run republishes a clean artifact (§2.7).
///
/// **Diagnostics parity (§2.4):** the entry path is part of the cache key, so a hit
/// always loads the artifact built for THIS invoking path. At PUBLISH time each archived
/// module's embedded debug source PATH is rebound to the string the from-source loader
/// would embed (`rebind_archive_module_paths_to_runtime`), so a cached run's panic carets
/// are byte-identical to the uncached run's — entry AND transitive modules. The
/// `same_content_different_path` and `panic_output_parity` batteries prove it.
///
/// `no_cache` (from `--no-cache` || `ASCRIPT_NO_COMPILE_CACHE=1`) bypasses the cache
/// entirely → no slot is created, byte-identical to the uncached path.
///
/// No `RefCell`/resource borrow is held across the `.await`s below: the cache-layer work
/// is the fully-synchronous [`try_cached_artifact`], which returns OWNED artifact bytes
/// before any await (Gate 4).
pub async fn run_file_on_vm_cached(
    path: &Path,
    script_args: &[String],
    packages: Option<crate::interp::PackageMap>,
    caps: Option<crate::stdlib::caps::CapSet>,
    no_cache: bool,
) -> Result<i32, AsError> {
    // Kill switch: `--no-cache` / `ASCRIPT_NO_COMPILE_CACHE=1` → today's uncached path.
    if no_cache {
        return run_file_on_vm_with_packages(path, script_args, packages, caps, ELIDE_DEFAULT_ON)
            .await;
    }

    // Attempt the cache layer. ANY failure returns `None` so we fall open below.
    // `caps` is consumed by both the cached and the uncached path, so it is cloned for
    // the lookup attempt and the original is threaded into whichever path runs.
    match try_cached_artifact(path, packages.as_ref(), path) {
        // CACHE HIT or freshly-published MISS — run the verified artifact bytes through
        // the SAME magic-routing path a `.aso`/bundle uses. `module_dir` is the entry's
        // parent so an archive-miss can still resolve a sibling on-disk source.
        Some(artifact_bytes) => {
            let module_dir = path.parent().map(std::path::Path::to_path_buf);
            run_verified_aso(
                &artifact_bytes,
                script_args,
                caps,
                module_dir,
                &path.display().to_string(),
            )
            .await
        }
        // FAIL-OPEN: the cache could not produce a runnable artifact (disabled, IO
        // error, exotic graph, publish failure, verifier rejection) → run uncached.
        None => {
            run_file_on_vm_with_packages(path, script_args, packages, caps, ELIDE_DEFAULT_ON).await
        }
    }
}

#[cfg(not(ascript_rt))]
/// WARM A §2.1 — the synchronous cache-layer core behind [`run_file_on_vm_cached`].
/// Returns `Some(artifact_bytes)` on a verified hit OR a successful miss-compile-publish,
/// and `None` on ANY cache-layer failure (the caller then runs uncached). This is the
/// single fail-open chokepoint: every `?`-style early return here is a `return None`, so
/// no cache error ever escapes as a run error. No `.await` happens inside — the bytes are
/// handed back to the async caller for the actual run.
fn try_cached_artifact(
    path: &Path,
    packages: Option<&crate::interp::PackageMap>,
    entry_arg: &Path,
) -> Option<Vec<u8>> {
    use crate::cache::compile_cache as cc;

    // The binary stamp invalidates on a rebuilt compiler. `Disabled` ⇒ cache off.
    let binary_stamp = cc::BinaryStamp::current();
    if binary_stamp.is_disabled() {
        return None;
    }

    // The entry path is part of the key (§2.4): canonicalize it (a missing entry ⇒ the
    // uncached path will surface the read error). A non-canonicalizable path ⇒ fail open.
    let entry_canon = path.canonicalize().ok()?;
    let entry_path = entry_canon.to_string_lossy().into_owned();

    // The package-map digest is part of the key so a lockfile / re-resolution change ⇒
    // a different key ⇒ miss (§2.9). NOTE: `compile_archive_with_shake` (the cache
    // artifact compiler) does not currently install a package RESOLVER, so a program that
    // imports a `{path=…}`/registry PACKAGE fails the archive walk and falls OPEN to the
    // uncached run on every invocation (it is never cached, hence never stale-hits). The
    // digest is keyed regardless so the day the archive builder gains package resolution
    // the invalidation is already correct. Relative-only programs (the common case) cache
    // fully. This is a documented fail-open limitation, not a stale-hit risk.
    let package_map_digest = match packages {
        Some(map) => cc::package_map_digest(map),
        None => [0u8; 32],
    };

    let key = cc::CompileCacheKey {
        key_schema: "ck1",
        aso_format_version: crate::vm::aso::ASO_FORMAT_VERSION,
        archive_version: crate::vm::archive::ARCHIVE_VERSION,
        binary_stamp,
        // v1 codegen-relevant flags: the cache artifact is ALWAYS the unshaken,
        // debug-carrying, non-elided archive (§2.6), so these are constant — but they
        // are enumerated so a future flag (e.g. ELIDE in the cache key) is mechanical,
        // and so the `flag_change_misses` battery can perturb them via the test seam.
        flags: cache_codegen_flags(),
        entry_path,
        package_map_digest,
    };
    let location_key = key.location_key();

    // ── Lookup ────────────────────────────────────────────────────────────────────
    // A HIT re-validates every source digest + the artifact digest, then runs the bytes
    // through the verifier in `run_verified_aso`. If the verifier later rejects them the
    // run errors — but `validate_manifest` already checked the artifact sha256, so a
    // bit-flip is caught at lookup (→ Miss → recompile). We still guard the verify step
    // on the miss path below.
    if let cc::LookupResult::Hit { artifact_bytes } = cc::lookup(&location_key) {
        // Defensive verify-on-hit: the FUZZ-hardened reader is the trust boundary. If a
        // valid-digest-but-unverifiable artifact somehow exists (e.g. a format the
        // current verifier rejects), treat the slot as poisoned: delete + fall to a
        // recompile (fail closed). A clean verify ⇒ hand the bytes back.
        if artifact_verifies(&artifact_bytes) {
            return Some(artifact_bytes);
        }
        cc::delete_slot(&location_key);
        // fall through to recompile
    }

    // ── Miss: compile the neutral-floor artifact + publish ─────────────────────────
    // Compile the unshaken, debug-carrying archive (the cache artifact shape, §2.6).
    // A compile error here is NOT a cache failure — it is a real program error, so we
    // must NOT swallow it into a silent miss (that would hide the diagnostic). Instead
    // fail open with `None`: the uncached path recompiles and surfaces the SAME error
    // with full diagnostics. (The double-compile on a broken program is acceptable —
    // a broken program is not the hot path.)
    let (mut archive, _report) =
        match compile_archive_with_shake(&entry_canon, /*debug*/ true, /*shake*/ false, /*elide*/ false)
        {
            Ok(v) => v,
            Err(_) => return None, // fail open — the uncached path re-reports the error
        };
    // NEUTRAL caps floor (§2.6): all-granted ∩ runtime = runtime, so a cached run's
    // effective caps are EXACTLY the CLI/manifest-composed set — byte-identical to the
    // uncached run. Caps are deliberately NOT in the key (they compose at run time).
    archive.caps = crate::stdlib::caps::CapSet::all_granted();

    // DIAGNOSTICS PARITY (§2.4): `compile_archive` embeds each module's CANONICAL path
    // in its debug section, but the from-source (uncached) run embeds the IMPORTER-JOINED
    // path the loader builds (`module_dir.join(specifier)` — e.g. `/dir/./model.as`).
    // To make a cached run's panic carets BYTE-IDENTICAL to the uncached run, rebind
    // every embedded module source PATH to the runtime-equivalent string before encoding.
    // Best-effort: a derivation failure leaves the canonical paths (still a runnable
    // artifact — the parity test drives this, and a mismatch is caught there, not here).
    rebind_archive_module_paths_to_runtime(&mut archive, entry_arg);
    let artifact_bytes = archive.encode();

    // Build the validation manifest from the SAME reachable set the cache keyer walks
    // (`collect_module_graph` — the §2.5 single-walk source; the drift tripwire test
    // proves it equals the compile path's set). A walk error ⇒ fail open (publish
    // nothing, run uncached).
    let graph = match crate::cache::collect_module_graph(&entry_canon) {
        Ok(g) => g,
        Err(_) => {
            // We still have a runnable artifact — run it, just don't publish (no
            // manifest means we can't validate later, so skip the publish entirely).
            return Some(artifact_bytes);
        }
    };
    let modules: Vec<cc::ManifestModule> = graph
        .iter()
        .map(|m| cc::ManifestModule {
            logical_key: m.logical_key.clone(),
            path: m.path.clone(),
            sha256: cc::sha256_bytes(m.source.as_bytes()),
        })
        .collect();
    let manifest = cc::CacheManifest {
        modules,
        artifact_sha256: cc::sha256_bytes(&artifact_bytes),
        created_unix_ms: cc::now_unix_ms(),
    };

    // Atomic publish. An IO error ⇒ fail open (we still return the runnable bytes — the
    // run succeeds, just unpublished; the next run will try to publish again).
    let _ = cc::publish(&location_key, &manifest, &artifact_bytes);
    Some(artifact_bytes)
}

#[cfg(not(ascript_rt))]
/// WARM A §2.4 — rebind every archive module's embedded debug source PATH to the string
/// the from-source (uncached) loader would embed, so a cached run's panic carets are
/// byte-identical to an uncached run's.
///
/// The from-source loader embeds:
/// - **entry:** the as-passed CLI path (`entry_arg.display()`); its `module_dir` is
///   `entry_arg.parent()` (the as-passed parent, possibly relative).
/// - **dependency:** `importer_module_dir.join(specifier).with_extension("as")`; its own
///   `module_dir` is `canonical(dep).parent()` (the loader keys/recurses by canonical path
///   but EMBEDS the importer-joined path).
///
/// This re-walks the graph mirroring `Vm::load_module_file` exactly (`module_dir.join` +
/// `.with_extension("as")` + `canonicalize` for dedup), producing the runtime path per
/// module. It then re-encodes each archive module with its path rebound. Best-effort: any
/// IO/decode failure leaves that module's canonical path untouched (still runnable; the
/// parity test catches a real mismatch).
fn rebind_archive_module_paths_to_runtime(
    archive: &mut crate::vm::archive::ModuleArchive,
    entry_arg: &Path,
) {
    use crate::interp::SpecifierKind;
    use std::collections::HashMap;
    use std::path::PathBuf;

    // Map canonical path → the runtime-embedded path string the loader would use.
    // Built by a BFS mirroring the loader's resolution. The archive's modules are keyed
    // by LOGICAL key (the `compile_archive` BFS order), but we rebind by matching each
    // archive module's canonical path — so we also record logical_key → runtime path.
    let mut runtime_path: HashMap<PathBuf, String> = HashMap::new();
    // logical_key → canonical path, so we can rebind archive modules (keyed by logical).
    let mut key_to_canon: HashMap<String, PathBuf> = HashMap::new();

    let interp = Interp::new();

    // The entry: embed the AS-PASSED path; its module_dir is the as-passed parent.
    let entry_embed = entry_arg.display().to_string();
    let entry_module_dir = entry_arg
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(""));
    let Ok(entry_canon) = entry_arg.canonicalize() else {
        return; // can't canonicalize → leave canonical paths (best-effort)
    };
    runtime_path.insert(entry_canon.clone(), entry_embed.clone());

    // Recover the entry's LOGICAL key the same way `compile_archive` does.
    let entry_logical = entry_canon
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "entry.as".to_string());
    key_to_canon.insert(entry_logical.clone(), entry_canon.clone());

    struct Pending {
        canon: PathBuf,
        logical_key: String,
        // The module_dir the loader uses to resolve THIS module's imports.
        module_dir: PathBuf,
    }
    let mut queue: std::collections::VecDeque<Pending> = std::collections::VecDeque::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    seen.insert(entry_canon.clone());
    queue.push_back(Pending {
        canon: entry_canon,
        logical_key: entry_logical,
        module_dir: entry_module_dir,
    });

    while let Some(item) = queue.pop_front() {
        // Read + compile this module purely to read its import specifiers (same as the
        // cache walk). Any failure → skip following its imports (best-effort rebind).
        let Ok(source) = std::fs::read_to_string(&item.canon) else {
            continue;
        };
        let Ok(bytes) = compile_verified_aso_bytes_from_source_for_cache(&item.canon, &source)
        else {
            continue;
        };
        let Ok(chunk) = crate::vm::chunk::Chunk::from_bytes_verified(&bytes) else {
            continue;
        };

        for imp in &chunk.imports {
            let spec = imp.source();
            interp.set_module_dir(item.module_dir.clone());
            match interp.classify_specifier(spec) {
                SpecifierKind::Std => {}
                kind @ (SpecifierKind::Relative(_) | SpecifierKind::Package { .. }) => {
                    // The loader EMBEDS `module_dir.join(spec).with_extension("as")`.
                    let requested = item.module_dir.join(spec);
                    let as_path = if requested.extension().is_some() {
                        requested.clone()
                    } else {
                        requested.with_extension("as")
                    };
                    let embed = as_path.display().to_string();
                    // It KEYS / recurses by the canonical path.
                    let target = match &kind {
                        SpecifierKind::Relative(t) => t.clone(),
                        SpecifierKind::Package { target, .. } => target.clone(),
                        _ => unreachable!(),
                    };
                    let Ok(dep_canon) = resolve_module_file(&target) else {
                        continue;
                    };
                    // First-importer-wins on the embedded path (matches the loader's
                    // `file_modules`-keyed once-only binding).
                    runtime_path.entry(dep_canon.clone()).or_insert(embed);
                    if seen.insert(dep_canon.clone()) {
                        let dep_key = match &kind {
                            SpecifierKind::Package { .. } => {
                                crate::vm::archive::join_logical("pkg", spec)
                            }
                            _ => crate::vm::archive::join_logical(
                                &crate::vm::archive::logical_parent(&item.logical_key),
                                spec,
                            ),
                        };
                        key_to_canon.insert(dep_key.clone(), dep_canon.clone());
                        let dep_module_dir = dep_canon
                            .parent()
                            .map(Path::to_path_buf)
                            .unwrap_or_else(|| PathBuf::from("."));
                        queue.push_back(Pending {
                            canon: dep_canon,
                            logical_key: dep_key,
                            module_dir: dep_module_dir,
                        });
                    }
                }
                SpecifierKind::UnknownPackage(_) => {}
            }
        }
    }

    // Rebind each archive module: decode → rebind path → re-encode (debug-carrying).
    for (logical_key, bytes) in archive.modules.iter_mut() {
        let Some(canon) = key_to_canon.get(logical_key) else {
            continue; // unknown key (e.g. package logical-dir drift) — leave as-is
        };
        let Some(new_path) = runtime_path.get(canon) else {
            continue;
        };
        let Ok(chunk) = crate::vm::chunk::Chunk::from_bytes_verified(bytes) else {
            continue;
        };
        chunk.rebind_source_path(new_path);
        if let Ok(reencoded) = chunk.to_bytes_with_debug(true) {
            *bytes = reencoded;
        }
    }
}

/// WARM A — the codegen-relevant flag list embedded in the [`cc::CompileCacheKey`].
///
/// v1 is constant (`debug=true, shake=false, elide=false`): the cache artifact is the
/// unshaken, debug-carrying, non-elided archive (§2.6). Behind a `#[doc(hidden)]` test
/// seam (`ASCRIPT_TEST_CACHE_FLAG_SALT`) the list can be PERTURBED so the `flag_change_misses`
/// battery can prove a codegen-flag change ⇒ a different key ⇒ a miss, without needing a
/// real second codegen flag to exist yet. The salt is read ONLY here and is absent on
/// every production run (the env var is unset), so the production key is unaffected.
fn cache_codegen_flags() -> Vec<(String, String)> {
    let mut flags = vec![
        ("debug".to_string(), "true".to_string()),
        ("shake".to_string(), "false".to_string()),
        ("elide".to_string(), "false".to_string()),
    ];
    // `#[doc(hidden)]` flag-change TEST SEAM (§5-A `flag_change_misses`): a non-empty
    // `ASCRIPT_TEST_CACHE_FLAG_SALT` adds a synthetic codegen flag, changing the key exactly
    // as a real flag flip would. Unset on every production run.
    if let Ok(salt) = std::env::var("ASCRIPT_TEST_CACHE_FLAG_SALT") {
        if !salt.is_empty() {
            flags.push(("__test_salt".to_string(), salt));
        }
    }
    flags
}

/// WARM A — verify a cached artifact through the SAME `from_bytes_verified` trust
/// boundary the runtime uses, WITHOUT running it. Returns `true` iff every module
/// (archive) or the single chunk (`ASO\0`) re-verifies. Hostile-input-safe: any decode
/// or verify failure ⇒ `false` (the caller treats the slot as poisoned → recompile).
fn artifact_verifies(bytes: &[u8]) -> bool {
    if bytes.starts_with(&crate::vm::archive::ARCHIVE_MAGIC) {
        let archive = match crate::vm::archive::ModuleArchive::decode(bytes) {
            Ok(a) => a,
            Err(_) => return false,
        };
        // Every embedded module must re-verify through the FUZZ-hardened reader.
        for (_key, mod_bytes) in &archive.modules {
            if crate::vm::chunk::Chunk::from_bytes_verified(mod_bytes).is_err() {
                return false;
            }
        }
        true
    } else {
        crate::vm::chunk::Chunk::from_bytes_verified(bytes).is_ok()
    }
}

#[cfg(not(ascript_rt))]
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
        .run_until(crate::interp::ambient_root_scope(vm.run(&mut fiber)))
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

#[cfg(not(ascript_rt))]
/// DECODE Task 4: run a `.as` FILE with decode DISABLED (byte dispatch). See
/// [`run_file_decode_cfg`]. `#[doc(hidden)]` — test API only.
#[doc(hidden)]
pub async fn run_file_no_decode(path: &Path) -> Result<i32, AsError> {
    use crate::vm::Vm;
    run_file_decode_cfg(path, false, Vm::DECODE_THRESHOLD).await
}

#[cfg(not(ascript_rt))]
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
        .run_until(crate::interp::ambient_root_scope(vm.run(&mut fiber)))
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

#[cfg(not(ascript_rt))]
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

/// WARM B §3.3/§5-B — the `#[doc(hidden)]` SEEDING test seam.
///
/// A white-box handle over a freshly-loaded PGO-carrying archive: it decodes the trailing
/// PGO section, builds a `Vm` + the entry `FnProto`, and (when `seed` is set AND the Vm is
/// specializing) seeds the entry chunk's side tables BEFORE first execution — exactly the
/// production archive-load path, but with the entry proto held live for inspection.
///
/// `seed`/`specialize` are threaded EXPLICITLY (not read from `ASCRIPT_NO_PGO`/the global
/// kill switch) so parallel tests never race a process-global env var (the LANE Task-2
/// convention). The handle exposes the install COUNT, pre/post-run side-table accessors, and
/// an async `run()` that drives the entry to completion and returns captured output.
#[doc(hidden)]
pub struct PgoSeedHandle {
    interp: Rc<Interp>,
    vm: Rc<crate::vm::Vm>,
    entry_proto: Rc<crate::vm::chunk::FnProto>,
    installed: usize,
}

impl PgoSeedHandle {
    /// Number of side-table entries the seeder installed (0 when `seed`/`specialize` is off,
    /// the section is absent, the digest mismatches, or every entry was skipped by a guard).
    pub fn installed(&self) -> usize {
        self.installed
    }

    /// White-box: the entry chunk's adaptive-arith state at byte offset `off`.
    pub fn entry_arith_cache(&self, off: usize) -> crate::vm::adapt::ArithCache {
        self.entry_proto.chunk.arith_cache(off)
    }

    /// White-box: the entry chunk's field inline cache at byte offset `off`.
    pub fn entry_field_ic(&self, off: usize) -> crate::vm::ic::InlineCache {
        self.entry_proto.chunk.field_ic(off)
    }

    /// White-box: the entry chunk's global cache at byte offset `off`.
    pub fn entry_global_cache(&self, off: usize) -> crate::vm::adapt::GlobalCache {
        self.entry_proto.chunk.global_cache(off)
    }

    /// Drive the entry proto to completion on a `LocalSet`, returning captured output.
    pub async fn run(&self) -> Result<String, AsError> {
        use crate::vm::value_ext::{Closure, RunOutcome};
        let closure = Closure::new(Rc::clone(&self.entry_proto));
        let mut fiber = crate::vm::fiber::Fiber::new(closure);
        let local = tokio::task::LocalSet::new();
        let result = local.run_until(self.vm.run(&mut fiber)).await;
        local.await;
        crate::gc::collect();
        match result {
            Ok(RunOutcome::Done(_)) | Ok(RunOutcome::Yielded(_)) => Ok(self.interp.output()),
            Err(crate::interp::Control::Panic(e)) => Err(e),
            Err(crate::interp::Control::Propagate(_)) => Ok(self.interp.output()),
            Err(crate::interp::Control::Exit(_)) => Ok(self.interp.output()),
        }
    }

    /// Like [`run`](Self::run) but returns the `(output, exit_code)` pair, mapping every
    /// `Control` channel exactly as the corpus differential modes (`vm_run_source` &c.) do —
    /// so the seeded-PGO differential mode is comparable to them byte-for-byte (an `exit(n)`
    /// surfaces the code; a top-level `?` propagation ends the program with `None`).
    pub async fn run_with_exit(&self) -> Result<(String, Option<i32>), AsError> {
        use crate::vm::value_ext::{Closure, RunOutcome};
        let closure = Closure::new(Rc::clone(&self.entry_proto));
        let mut fiber = crate::vm::fiber::Fiber::new(closure);
        let local = tokio::task::LocalSet::new();
        let result = local.run_until(self.vm.run(&mut fiber)).await;
        local.await;
        crate::gc::collect();
        match result {
            Ok(RunOutcome::Done(_)) => Ok((self.interp.output(), None)),
            Ok(RunOutcome::Yielded(_)) => unreachable!("top-level program cannot yield"),
            Err(crate::interp::Control::Panic(e)) => Err(e),
            Err(crate::interp::Control::Propagate(_)) => Ok((self.interp.output(), None)),
            Err(crate::interp::Control::Exit(code)) => Ok((self.interp.output(), Some(code))),
        }
    }
}

/// WARM B §3.3 — load `archive_bytes` (an `ASCRIPTA` archive, optionally with a trailing PGO
/// section) and seed the entry chunk per `seed`/`specialize`, returning a [`PgoSeedHandle`].
/// `#[doc(hidden)]` test/seam API — NOT a stable surface.
#[doc(hidden)]
pub fn pgo_seed_for_test(
    archive_bytes: &[u8],
    seed: bool,
    specialize: bool,
) -> Result<PgoSeedHandle, AsError> {
    use crate::vm::chunk::FnProto;
    use crate::vm::Vm;

    let archive = crate::vm::archive::ModuleArchive::decode(archive_bytes)
        .map_err(|e| AsError::new(format!("cannot decode archive: {e}")))?;
    let section = decode_trailing_pgo(archive_bytes, &archive);

    let (entry_key, entry_bytes) = archive
        .modules
        .get(archive.entry as usize)
        .map(|(k, b)| (k.clone(), b.clone()))
        .ok_or_else(|| AsError::new("archive entry index is out of range"))?;
    let entry_sha256 = crate::cache::compile_cache::sha256_bytes(&entry_bytes);
    let chunk = crate::vm::chunk::Chunk::from_bytes_verified(&entry_bytes)
        .map_err(|e| AsError::new(format!("cannot load archive entry: {e}")))?;

    let interp = Rc::new(Interp::new()); // captured output
    interp.set_worker_archive_bytes(Rc::from(archive.encode().as_slice()));
    interp.set_worker_aso_bytes(Rc::from(entry_bytes.as_slice()));
    interp.install_self();
    // Honor the explicit `specialize` flag (the generic-VM seed-skip path).
    let vm = Vm::with_specialize(interp.clone(), specialize);
    vm.set_module_archive(Rc::new(archive));

    let entry_proto = Rc::new(FnProto {
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

    // Seed BEFORE first execution — exactly the production order. `seed_entry_from_section`
    // is internally gated on `vm.specialize`, so `specialize=false` ⇒ 0 installs.
    let installed = match section.as_ref() {
        Some(sec) => {
            vm.seed_entry_from_section(&entry_proto.chunk, sec, &entry_key, &entry_sha256, seed)
        }
        None => 0,
    };

    Ok(PgoSeedHandle {
        interp,
        vm,
        entry_proto,
        installed,
    })
}

#[cfg(not(ascript_rt))]
/// WARM B §5-B(a) — compile `src` to a single-module `ASCRIPTA` archive, run it ONCE as a
/// training workload (capture mode), harvest the warmed side tables, and return the
/// PGO-carrying artifact bytes (archive + appended `ASPGO` trailing section). This is the
/// in-process, disk-less equivalent of `build_file_with_pgo` for a self-contained program;
/// the seeded differential + coverage seams below load these bytes through `pgo_seed_for_test`
/// (the exact production seed-at-load path). `#[doc(hidden)]` — not a stable API.
#[doc(hidden)]
pub async fn pgo_build_artifact_from_source(src: &str) -> Result<Vec<u8>, AsError> {
    use crate::vm::chunk::FnProto;
    use crate::vm::pgo::append_section;
    use crate::vm::value_ext::Closure;
    use crate::vm::Vm;

    // ── compile a single-module archive from source ──────────────────────────
    let chunk = crate::compile::compile_source(src)
        .map_err(|e| AsError::at(e.message, e.span))?;
    let entry_bytes = chunk
        .to_bytes_with_debug(true)
        .map_err(|e| AsError::new(format!("cannot serialize compiled chunk: {e}")))?;
    let entry_key = "<input>".to_string();
    let archive = crate::vm::archive::ModuleArchive::new(
        0,
        crate::stdlib::caps::CapSet::all_granted(),
        [0u8; 32],
        vec![(entry_key.clone(), entry_bytes.clone())],
    );
    let entry_sha256 = crate::cache::compile_cache::sha256_bytes(&entry_bytes);
    let encoded_archive = archive.encode();

    // ── training run (capture) + harvest ─────────────────────────────────────
    let pgo = {
        let interp = Rc::new(Interp::new());
        interp.set_worker_archive_bytes(Rc::from(encoded_archive.as_slice()));
        interp.set_worker_aso_bytes(Rc::from(entry_bytes.as_slice()));
        interp.set_worker_source(src);
        interp.install_self();
        let train_chunk = crate::vm::chunk::Chunk::from_bytes_verified(&entry_bytes)
            .map_err(|e| AsError::new(format!("cannot load training chunk: {e}")))?;
        let train_proto = Rc::new(FnProto {
            chunk: train_chunk,
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
        let vm = Vm::new(interp.clone());
        vm.set_module_archive(Rc::new(
            crate::vm::archive::ModuleArchive::decode(&encoded_archive)
                .map_err(|e| AsError::new(format!("cannot re-decode archive: {e}")))?,
        ));
        {
            let closure = Closure::new(Rc::clone(&train_proto));
            let mut fiber = crate::vm::fiber::Fiber::new(closure);
            let local = tokio::task::LocalSet::new();
            // Absorb ALL outcomes — a panicking training run still yields a (partial) section.
            let _ = local.run_until(vm.run(&mut fiber)).await;
            local.await;
            crate::gc::collect();
        }
        let harvest_modules: &[(String, [u8; 32], &FnProto)] =
            &[(entry_key.clone(), entry_sha256, &train_proto)];
        vm.harvest_pgo(harvest_modules)
    };

    let mut artifact_bytes = encoded_archive;
    append_section(&mut artifact_bytes, &pgo.encode());
    Ok(artifact_bytes)
}

#[cfg(not(ascript_rt))]
/// WARM B §5-B(a) — the SEEDED differential mode (Gate 15), in-process and from SOURCE.
///
/// Mirrors the production `build --pgo` → seed-at-load → run flow entirely in-process and
/// with CAPTURED output (so the corpus differential can compare it against the standard
/// modes' `(String, Option<i32>)`): build the PGO-carrying artifact
/// ([`pgo_build_artifact_from_source`]), then load it SEEDED through the production
/// [`pgo_seed_for_test`] path and run.
///
/// The byte-invisibility PROOF: the corpus differential asserts this equals tree-walker ==
/// spec == generic. A divergence means a seed bypassed a guard (the seeder is unsound).
/// `#[doc(hidden)]` — not a stable API.
#[doc(hidden)]
pub async fn pgo_seeded_run_from_source(src: &str) -> Result<(String, Option<i32>), AsError> {
    let artifact = pgo_build_artifact_from_source(src).await?;
    let handle = pgo_seed_for_test(&artifact, /*seed=*/ true, /*specialize=*/ true)?;
    let src_info = Rc::new(SourceInfo {
        path: "<input>".to_string(),
        text: src.to_string(),
    });
    // `PgoSeedHandle::run` captures output and maps every Control channel like the corpus
    // modes, but returns only the output String; re-derive the exit code by matching its
    // run path here would duplicate it, so reuse `run()` and report exit via the handle.
    handle
        .run_with_exit()
        .await
        .map_err(|e| e.with_source(src_info))
}

#[cfg(not(ascript_rt))]
/// WARM B §5-B(a) coverage seam (anti-false-green): build the PGO artifact for `src`, load it
/// SEEDED, and return the INSTALLED-COUNT (how many side-table entries the seeder installed).
/// The corpus coverage assertion sums this over the corpus to prove the seeded axis is NOT
/// dark (>0 seeds actually install). `#[doc(hidden)]` — not a stable API.
#[doc(hidden)]
pub async fn pgo_seeded_install_count_from_source(src: &str) -> Result<usize, AsError> {
    let artifact = pgo_build_artifact_from_source(src).await?;
    let handle = pgo_seed_for_test(&artifact, /*seed=*/ true, /*specialize=*/ true)?;
    Ok(handle.installed())
}

#[cfg(not(ascript_rt))]
/// WARM B §6 bench seam — the UNSEEDED counterpart of [`pgo_seeded_run_from_source`]: build
/// the SAME PGO-carrying artifact, but load it with seeding OFF (`seed=false`, the
/// `ASCRIPT_NO_PGO` kill-switch path). The cold-start microbench (`vm_bench`) times this
/// against the seeded run on the SAME artifact to measure the warm-up window the seeds
/// eliminate, in-process (unmasked by process startup). `#[doc(hidden)]` — not a stable API.
#[doc(hidden)]
pub async fn pgo_unseeded_run_from_source(src: &str) -> Result<(String, Option<i32>), AsError> {
    let artifact = pgo_build_artifact_from_source(src).await?;
    let handle = pgo_seed_for_test(&artifact, /*seed=*/ false, /*specialize=*/ true)?;
    let src_info = Rc::new(SourceInfo {
        path: "<input>".to_string(),
        text: src.to_string(),
    });
    handle
        .run_with_exit()
        .await
        .map_err(|e| e.with_source(src_info))
}

#[cfg(not(ascript_rt))]
/// WARM B §5-B(b) — the ADVERSARIAL-SEED axis (fuzzing the GUARDS, not the codec).
///
/// Compiles `src` to an entry chunk, then injects PSEUDO-RANDOM JUNK directly into the
/// chunk's side tables — `set_arith_cache` / `set_field_ic` / `set_global_cache` — BYPASSING
/// the seeder entirely (so wrong offsets, wrong kinds, wrong shape ids, wrong indices, and
/// bogus global values all land). The junk is derived from `junk` bytes (the fuzz input).
/// The run loop's guards (operand-kind re-check on arith, shape-id match on field IC, version
/// match on global cache) MUST absorb every lie → a junk seed can only deopt, never change
/// output. The caller asserts byte-identity against the unseeded modes.
///
/// This is strictly MORE adversarial than the seeder: the seeder derives field indices and
/// range-checks offsets; here we inject arbitrary indices/shapes/offsets at arbitrary sites.
/// `#[doc(hidden)]` — not a stable API.
#[doc(hidden)]
pub async fn pgo_adversarial_run_from_source(
    src: &str,
    junk: &[u8],
) -> Result<(String, Option<i32>), AsError> {
    use crate::vm::chunk::FnProto;
    use crate::vm::pgo::{PgoModule, PgoProto, PgoSection};
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;

    let src_info = Rc::new(SourceInfo {
        path: "<input>".to_string(),
        text: src.to_string(),
    });
    let chunk = crate::compile::compile_source(src)
        .map_err(|e| AsError::at(e.message, e.span).with_source(src_info.clone()))?;
    let entry_bytes = chunk
        .to_bytes_with_debug(true)
        .map_err(|e| AsError::new(format!("cannot serialize compiled chunk: {e}")))?;
    let entry_key = "<input>".to_string();
    let entry_sha256 = crate::cache::compile_cache::sha256_bytes(&entry_bytes);

    // Derive a pseudo-random stream from `junk` (a cheap LCG over the bytes).
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    for &b in junk {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(b as u64 + 1);
    }
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        state
    };

    // ── Build a JUNK PgoSection and seed it through the REAL `seed_chunk` ─────────
    //
    // WHY through `seed_chunk` (not raw `set_field_ic`): the §3.3 wire format carries NO field
    // index — the seeder DERIVES the index from a key list, and a field-IC shape id is minted
    // by INTERNING that key list. So the only states a (corrupt) profile can ever express are
    // (junk OFFSET, junk arith KIND tag, junk KEY-LIST, junk KEY-LIST-INDEX reference, junk
    // GLOBAL offset) — exactly what §5 names ("offset, kind/key-list/builtin-marker"). Raw
    // `set_field_ic` with a fabricated (shape_id, index) pair would test a state the wire
    // format CANNOT produce (a shape id that collides with a live shape but whose layout is
    // NOT that key list — the shape invariant the seeder relies on), which is not the threat
    // model. We therefore fuzz the seeder + run-loop guards AS SHIPPED: a junk profile in, the
    // real derivation + interning + guards, byte-identical output out.
    //
    // The junk KEY LISTS deliberately MIX (a) full real-looking multi-field layouts that
    // intern-COLLIDE with common object shapes (so the derived index is genuinely exercised —
    // INCLUDING reversed/permuted orders, which a broken derivation that trusts the key-list
    // index instead of deriving the name's position would mis-map → the liveness-relevant
    // case) and (b) garbage names (so most lists intern to fresh non-colliding shapes →
    // shape-miss). Both are corrupt-profile-expressible (interned → a real shape id).
    let mut key_lists: Vec<Vec<String>> = vec![
        // Real layouts the dangerous-shape corpus uses, in RECEIVER (insertion) ORDER so they
        // intern to the SAME shape id as the live object → the seeder actually installs at the
        // GetProp site and the DERIVED index is genuinely exercised. They sit at LIST indices
        // (0,1,2,…) deliberately DIFFERENT from the field positions, so a broken derivation
        // that trusted the key-list index instead of deriving the name's position would
        // mis-map (e.g. `.c` in `{a,b,c}` → list-idx 0 ≠ field pos 2) → the liveness case.
        // Also reversed orders (intern to a DIFFERENT shape → shape-miss, the other path).
        vec!["a".into(), "b".into(), "c".into()],
        vec!["x".into(), "y".into()],
        vec!["k".into(), "m".into()],
        vec!["first".into(), "second".into(), "third".into()],
        vec!["c".into(), "b".into(), "a".into()],
        vec!["b".into(), "a".into()],
    ];
    let junk_names = ["a", "b", "c", "d", "x", "y", "z", "k", "m", "first", "second", "third", "qqzz", "__nope__"];
    let n_extra = (next() % 5) as usize;
    for _ in 0..n_extra {
        let len = (next() % 4) as usize; // 0..3 keys
        key_lists.push(
            (0..len)
                .map(|_| junk_names[(next() as usize) % junk_names.len()].to_string())
                .collect(),
        );
    }
    let n_lists = key_lists.len();

    // Build junk PgoProtos. For each proto we target BOTH (a) the REAL op sites (so the seeder
    // installs where derivation + guards are exercised — every GetProp/SetProp gets a field
    // record referencing ALL key-lists, so a matching layout DOES install a derived index;
    // every arith op gets a junk kind tag; every GetGlobal gets a global record) and (b) random
    // offsets/indices for breadth. `all_lists` references every key-list, including the
    // receiver-order layouts that intern-collide with the live objects.
    let all_lists: Vec<u32> = (0..n_lists as u32).collect();
    let build_proto =
        |target: &crate::vm::chunk::Chunk, path: Vec<u32>, n: &mut dyn FnMut() -> u64| -> PgoProto {
            use crate::vm::opcode::Op;
            let code: Vec<u8> = target.code.to_vec();
            let tlen = code.len().max(1) as u32;
            let mut arith: Vec<(u32, u8)> = Vec::new();
            let mut fields: Vec<(u32, Vec<u32>)> = Vec::new();
            let mut globals: Vec<u32> = Vec::new();
            // (a) Real op sites.
            let mut off = 0usize;
            while off < code.len() {
                let Some(op) = Op::from_u8(code[off]) else { break };
                if matches!(op, Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Mod | Op::Pow) {
                    arith.push((off as u32, (n() % 6) as u8)); // junk kind tag 0..5 (some invalid)
                }
                if matches!(op, Op::GetProp | Op::SetProp) {
                    fields.push((off as u32, all_lists.clone()));
                }
                if matches!(op, Op::GetGlobal) {
                    globals.push(off as u32);
                }
                off += 1 + op.operand_width();
            }
            // (b) Random breadth (mid-instruction offsets, random list-index refs).
            for _ in 0..(n() % 6) {
                arith.push(((n() as u32) % tlen, (n() % 6) as u8));
            }
            for _ in 0..(n() % 6) {
                let n_idx = (n() % 3) as usize;
                let idxs = (0..n_idx).map(|_| (n() as u32) % (n_lists as u32 + 2)).collect();
                fields.push(((n() as u32) % tlen, idxs));
            }
            for _ in 0..(n() % 6) {
                globals.push((n() as u32) % tlen);
            }
            PgoProto {
                path,
                arith,
                fields,
                globals,
            }
        };
    let mut protos = vec![build_proto(&chunk, Vec::new(), &mut next)];
    for i in 0..chunk.protos.len() {
        protos.push(build_proto(&chunk.protos[i].chunk, vec![i as u32], &mut next));
    }
    // Occasionally point a proto path out of range (the seeder must skip it).
    protos.push(build_proto(&chunk, vec![9999], &mut next));

    let section = PgoSection {
        key_lists,
        modules: vec![PgoModule {
            module_key: entry_key.clone(),
            chunk_sha256: entry_sha256,
            protos,
        }],
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
    let interp = Rc::new(Interp::new());
    interp.install_self();
    interp.set_worker_source(src);
    let vm = Vm::new(interp.clone());
    // Seed the JUNK section through the REAL seeder (derivation + interning + digest gate) —
    // gated on `vm.specialize` + `seed=true`. The run-loop guards must absorb every lie.
    vm.seed_entry_from_section(&proto.chunk, &section, &entry_key, &entry_sha256, true);
    let closure = Closure::new(proto.clone());
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
