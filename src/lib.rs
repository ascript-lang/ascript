pub mod ast;
pub mod env;
pub mod error;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod span;
pub mod token;
pub mod value;

use crate::error::AsError;
use crate::interp::Interp;
use std::path::Path;

/// Run a `.as` file as the entry module (with import resolution relative to it).
pub async fn run_file(path: &Path) -> Result<String, AsError> {
    let mut interp = Interp::new();
    match interp.load_module(path).await {
        Ok(_) => Ok(interp.output),
        Err(crate::interp::Control::Panic(e)) => Err(e),
        Err(crate::interp::Control::Propagate(_)) => Ok(interp.output),
    }
}

/// Lex → parse → evaluate in a fresh global environment. Returns captured output.
pub async fn run_source(src: &str) -> Result<String, AsError> {
    let tokens = lexer::lex(src)?;
    let program = parser::parse(&tokens)?;
    let mut interp = Interp::new();
    let env = crate::interp::global_env();
    match interp.exec(&program, &env).await {
        Ok(crate::interp::Flow::Break) => Err(AsError::new("'break' outside of a loop")),
        Ok(crate::interp::Flow::Continue) => Err(AsError::new("'continue' outside of a loop")),
        Ok(crate::interp::Flow::Normal) | Ok(crate::interp::Flow::Return(_)) => Ok(interp.output),
        // A panic aborts the program with its diagnostic.
        Err(crate::interp::Control::Panic(e)) => Err(e),
        // A top-level `?` propagation simply ends the program.
        Err(crate::interp::Control::Propagate(_)) => Ok(interp.output),
    }
}
