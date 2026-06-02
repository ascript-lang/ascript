pub mod ast;
pub mod check;
pub mod compile;
pub mod syntax;
pub mod coro;
pub mod diagnostics;
pub mod env;
pub mod error;
pub mod fmt;
pub mod interp;
pub(crate) mod lex_literals;
pub mod lexer;
#[cfg(feature = "lsp")]
pub mod lsp;
pub mod parser;
pub mod repl;
pub mod span;
pub mod stdlib;
pub mod task;
pub mod token;
pub mod value;
pub mod vm;

use crate::error::{AsError, SourceInfo};
use crate::interp::Interp;
pub use crate::interp::TestSummary;
use std::path::Path;
use std::rc::Rc;

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
    // CLI `run` streams `print` output live to stdout (so it appears immediately
    // and survives a later panic). Under `Live` there is no captured string, so
    // the success contract is `()` — the caller does not re-print anything.
    let interp = Rc::new(Interp::new_live());
    interp.set_cli_args(script_args);
    interp.install_self();
    let local = tokio::task::LocalSet::new();
    let result = local.run_until(interp.load_module(path)).await;
    local.await; // drain spawned tasks (structured join) — no-op until Phase 2
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
    let interp = Rc::new(Interp::new());
    interp.install_self();
    let local = tokio::task::LocalSet::new();
    let result: Result<TestSummary, AsError> = local
        .run_until(async {
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
        })
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
    // Run in a child of the builtins env so the program can shadow builtins
    // (`let len = 5`) and import names that collide with builtins.
    let env = crate::interp::global_env().child();
    let local = tokio::task::LocalSet::new();
    let result = local.run_until(interp.exec(&program, &env)).await;
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
        params: Vec::new(),
        ret: None,
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
        params: Vec::new(),
        ret: None,
    });
    let closure = Closure::new(proto);

    let interp = Rc::new(Interp::new());
    interp.install_self();
    let vm = Vm::new(interp.clone());
    let mut fiber = crate::vm::fiber::Fiber::new(closure);

    let local = tokio::task::LocalSet::new();
    let result = local.run_until(vm.run(&mut fiber)).await;
    local.await; // drain spawned tasks — no-op until later VM slices
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
