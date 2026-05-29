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
    interp.exec(&program, &env).await?;
    Ok(interp.output)
}
