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

/// Lex → parse → evaluate. Returns the program's captured output.
pub async fn run_source(src: &str) -> Result<String, AsError> {
    let tokens = lexer::lex(src)?;
    let program = parser::parse(&tokens)?;
    let mut interp = Interp::new();
    interp.exec(&program).await?;
    Ok(interp.output)
}
