pub mod ast;
pub mod diagnostics;
pub mod env;
pub mod error;
pub mod fmt;
pub mod interp;
pub mod lexer;
#[cfg(feature = "lsp")]
pub mod lsp;
pub mod parser;
pub mod repl;
pub mod span;
pub mod stdlib;
pub mod token;
pub mod value;

use crate::error::{AsError, SourceInfo};
use crate::interp::Interp;
pub use crate::interp::TestSummary;
use std::path::Path;
use std::rc::Rc;

/// Run a `.as` file as the entry module (with import resolution relative to it).
///
/// The program runs inside a `tokio::task::LocalSet` so it (and, from M17 Phase 2
/// on, any tasks it spawns) lives on the current-thread runtime. After the root
/// future completes we drive the LocalSet to drain spawned tasks — a no-op today.
pub async fn run_file(path: &Path) -> Result<String, AsError> {
    let interp = Rc::new(Interp::new());
    let local = tokio::task::LocalSet::new();
    let result = local.run_until(interp.load_module(path)).await;
    local.await; // drain spawned tasks (structured join) — no-op until Phase 2
    match result {
        Ok(_) => Ok(interp.output()),
        Err(crate::interp::Control::Panic(e)) => Err(e),
        Err(crate::interp::Control::Propagate(_)) => Ok(interp.output()),
    }
}

/// Load each file as a module (running its `test(...)` registrations) on a
/// single `Interp`, then run all registered tests and return a summary.
pub async fn run_tests(files: &[String]) -> Result<TestSummary, AsError> {
    let interp = Rc::new(Interp::new());
    let local = tokio::task::LocalSet::new();
    let result: Result<TestSummary, AsError> = local
        .run_until(async {
            for file in files {
                match interp.load_module(Path::new(file)).await {
                    Ok(_) | Err(crate::interp::Control::Propagate(_)) => {}
                    Err(crate::interp::Control::Panic(e)) => return Err(e),
                }
            }
            Ok(interp.run_registered_tests().await)
        })
        .await;
    local.await; // drain spawned tasks — no-op until Phase 2
    result
}

/// Lex → parse → evaluate in a fresh global environment. Returns captured output.
pub async fn run_source(src: &str) -> Result<String, AsError> {
    let src_info = Rc::new(SourceInfo { path: "<input>".to_string(), text: src.to_string() });
    let tokens = lexer::lex(src).map_err(|e| e.with_source(src_info.clone()))?;
    let program = parser::parse(&tokens).map_err(|e| e.with_source(src_info.clone()))?;
    let interp = Rc::new(Interp::new());
    // Run in a child of the builtins env so the program can shadow builtins
    // (`let len = 5`) and import names that collide with builtins.
    let env = crate::interp::global_env().child();
    let local = tokio::task::LocalSet::new();
    let result = local.run_until(interp.exec(&program, &env)).await;
    local.await; // drain spawned tasks — no-op until Phase 2
    match result {
        Ok(crate::interp::Flow::Break) => Err(AsError::new("'break' outside of a loop")),
        Ok(crate::interp::Flow::Continue) => Err(AsError::new("'continue' outside of a loop")),
        Ok(crate::interp::Flow::Normal) | Ok(crate::interp::Flow::Return(_)) => Ok(interp.output()),
        // A panic aborts the program with its diagnostic.
        Err(crate::interp::Control::Panic(e)) => Err(e.with_source(src_info)),
        // A top-level `?` propagation simply ends the program.
        Err(crate::interp::Control::Propagate(_)) => Ok(interp.output()),
    }
}
