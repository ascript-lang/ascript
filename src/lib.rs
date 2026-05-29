pub mod ast;
pub mod env;
pub mod error;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod span;
pub mod token;
pub mod value;

use crate::env::Environment;
use crate::error::AsError;
use crate::interp::Interp;

/// Lex → parse → evaluate in a fresh global environment. Returns captured output.
pub async fn run_source(src: &str) -> Result<String, AsError> {
    let tokens = lexer::lex(src)?;
    let program = parser::parse(&tokens)?;
    let mut interp = Interp::new();
    let env = Environment::global();
    match interp.exec(&program, &env).await? {
        crate::interp::Flow::Break => return Err(AsError::new("'break' outside of a loop")),
        crate::interp::Flow::Continue => return Err(AsError::new("'continue' outside of a loop")),
        crate::interp::Flow::Normal | crate::interp::Flow::Return(_) => {}
    }
    Ok(interp.output)
}
