pub mod ast;
pub mod check;
pub mod compile;
pub mod coro;
// DBG Task 5b: the Debug Adapter Protocol (DAP) server over stdio. Feature-gated
// (`dap`, default-on) so `--no-default-features` builds none of it.
#[cfg(feature = "dap")]
pub mod dap;
pub mod det;
pub mod diagnostics;
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
    let interp = Rc::new(Interp::new());
    if let Some(caps) = caps {
        interp.set_caps(caps);
    }
    if let Some(map) = packages {
        interp.set_package_resolver(map);
    }
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
            match interp.run_registered_tests().await {
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
    result
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
    use crate::vm::chunk::FnProto;
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;

    let bytes = std::fs::read(path)
        .map_err(|e| AsError::new(format!("cannot read {}: {}", path.display(), e)))?;
    let chunk = crate::vm::chunk::Chunk::from_bytes_verified(&bytes)
        .map_err(|e| AsError::new(format!("cannot load {}: {}", path.display(), e)))?;

    let interp = Rc::new(Interp::new_live());
    interp.set_cli_args(script_args);
    // FFI §4.5: install the composed capability set before running any code.
    if let Some(caps) = caps {
        interp.set_caps(caps);
    }
    // Workers Spec A (.aso path): retain the raw bytes so `dispatch_worker_closure` can
    // re-parse them into the top-level chunk and build a worker code slice without source.
    interp.set_worker_aso_bytes(Rc::from(bytes.into_boxed_slice()));
    interp.install_self();
    let vm = Vm::new(interp.clone());
    // Resolve relative imports against the .aso's directory.
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
    // Workers Spec A: retain the source so a `worker fn` call can build its slice.
    interp.set_worker_source(src);
    let vm = Vm::with_specialize(interp.clone(), specialize);
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
