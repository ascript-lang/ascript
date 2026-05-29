//! Interactive REPL (`ascript repl`).
//!
//! Keeps ONE `Interp` and ONE module `Environment` alive across all inputs so
//! bindings persist (`let x = 1` then later `x + 1`). Each input is lexed and
//! parsed to a `Vec<Stmt>`; if the last statement is an expression, the value of
//! that trailing expression is printed (unless it is `nil`). A `Control::Panic`
//! is reported but does NOT exit the loop — the environment stays intact and the
//! loop continues.

use std::io::{BufRead, IsTerminal};
use std::rc::Rc;

use crate::ast::Stmt;
use crate::env::Environment;
use crate::error::SourceInfo;
use crate::interp::{Control, Interp};
use crate::value::Value;

/// Run the interactive REPL until EOF (Ctrl-D) or Ctrl-C.
///
/// Uses `rustyline` line editing when stdin is a TTY; otherwise (piped input)
/// reads lines from stdin directly so non-interactive use still works.
pub async fn run_repl() -> std::io::Result<()> {
    let mut interp = Interp::new();
    // Persistent session scope: a child of the builtins env so REPL definitions
    // can shadow builtins and persist across lines (builtins resolve upward).
    let env = crate::interp::global_env().child();

    if std::io::stdin().is_terminal() {
        run_tty(&mut interp, &env).await
    } else {
        run_piped(&mut interp, &env).await
    }
}

/// Interactive path: rustyline editor with history.
async fn run_tty(interp: &mut Interp, env: &Environment) -> std::io::Result<()> {
    use rustyline::error::ReadlineError;
    use rustyline::DefaultEditor;

    let mut rl =
        DefaultEditor::new().map_err(|e| std::io::Error::other(e.to_string()))?;
    loop {
        match rl.readline(">> ") {
            Ok(line) => {
                let _ = rl.add_history_entry(line.as_str());
                eval_line(interp, env, &line).await;
            }
            // Ctrl-C / Ctrl-D both exit cleanly.
            Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => break,
            Err(e) => {
                return Err(std::io::Error::other(e.to_string()));
            }
        }
    }
    Ok(())
}

/// Non-TTY path: read lines straight from stdin (used by the piped test).
async fn run_piped(interp: &mut Interp, env: &Environment) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        eval_line(interp, env, &line).await;
    }
    Ok(())
}

/// Lex+parse one input line and evaluate it against the shared interpreter and
/// environment. Errors (lex/parse/panic) are reported and swallowed so the loop
/// continues with the environment intact.
async fn eval_line(interp: &mut Interp, env: &Environment, line: &str) {
    if line.trim().is_empty() {
        return;
    }
    let src_info = Rc::new(SourceInfo { path: "<repl>".to_string(), text: line.to_string() });

    let tokens = match crate::lexer::lex(line) {
        Ok(t) => t,
        Err(e) => {
            crate::diagnostics::report(&e.with_source(src_info));
            return;
        }
    };
    let mut program = match crate::parser::parse(&tokens) {
        Ok(p) => p,
        Err(e) => {
            crate::diagnostics::report(&e.with_source(src_info));
            return;
        }
    };

    // If the last statement is a bare expression, exec the preceding statements
    // and then evaluate it for its value (printed unless nil).
    let trailing = match program.last() {
        Some(Stmt::Expr(_)) => program.pop(),
        _ => None,
    };

    if let Err(Control::Panic(e)) = interp.exec(&program, env).await {
        flush_output(interp);
        crate::diagnostics::report(&e.with_source(src_info.clone()));
        return;
    }

    if let Some(Stmt::Expr(expr)) = trailing {
        match interp.eval_expr(&expr, env).await {
            Ok(value) => {
                flush_output(interp);
                if !matches!(value, Value::Nil) {
                    println!("{}", value);
                }
            }
            Err(Control::Panic(e)) => {
                flush_output(interp);
                crate::diagnostics::report(&e.with_source(src_info));
            }
            Err(Control::Propagate(_)) => flush_output(interp),
        }
    } else {
        flush_output(interp);
    }
}

/// Print and clear any output captured by `print` during this input.
fn flush_output(interp: &mut Interp) {
    if !interp.output.is_empty() {
        print!("{}", interp.output);
        interp.output.clear();
    }
}
