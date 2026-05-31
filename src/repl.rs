//! Interactive REPL (`ascript repl`).
//!
//! Keeps ONE `Interp` and ONE module `Environment` alive across all inputs so
//! bindings persist (`let x = 1` then later `x + 1`). Each input is lexed and
//! parsed to a `Vec<Stmt>`; if the last statement is an expression, the value of
//! that trailing expression is printed (unless it is `nil`). A `Control::Panic`
//! is reported but does NOT exit the loop — the environment stays intact and the
//! loop continues.
//!
//! Each line is evaluated inside a `tokio::task::LocalSet` so spawned tasks (from
//! M17 Phase 2 on) join before the prompt returns; today the drain is a no-op.

use std::io::{BufRead, IsTerminal};
use std::rc::Rc;

use crate::ast::Stmt;
use crate::env::Environment;
use crate::error::{AsError, SourceInfo};
use crate::interp::{Control, Interp};
use crate::token::Tok;
use crate::value::Value;

/// Should the REPL buffer more lines? True only for unclosed delimiters or an
/// unterminated string/template at EOF — NOT for genuine mid-line syntax errors.
/// Counts delimiter TOKENS so `${...}` template braces never skew the depth.
fn is_incomplete(src: &str) -> bool {
    match crate::lexer::lex(src) {
        Ok(tokens) => {
            let mut depth: i32 = 0;
            for t in &tokens {
                match t.tok {
                    Tok::LBrace | Tok::LParen | Tok::LBracket => depth += 1,
                    Tok::RBrace | Tok::RParen | Tok::RBracket => depth -= 1,
                    // A template with an OPEN interpolation lexes Ok (e.g. `${`
                    // → `TemplateStart` with no closing brace), so balance it
                    // like a delimiter. A COMPLETE template nets to 0:
                    // `a ${x} b` is Start(+1)..End(-1); a multi-interp
                    // `a${x}b${y}c` is Start(+1)..Middle(0)..End(-1).
                    Tok::TemplateStart(_) => depth += 1, // opened an interpolation
                    Tok::TemplateEnd(_) => depth -= 1,   // closed the last interpolation
                    Tok::TemplateMiddle(_) => {}         // closes one + opens one → net 0
                    _ => {}
                }
            }
            depth > 0
        }
        Err(e) => is_unterminated_at_eof(&e, src),
    }
}

/// Distinguish an unterminated string/template at EOF (→ keep buffering) from a
/// genuine bad-character lex error (→ report now). The lexer raises
/// `"unterminated string"` / `"unterminated template string"` only when the
/// scan runs off the end of input, so the message is a precise EOF signal.
/// Deliberately conservative: any other lex error returns false (report rather
/// than hang). Note: an unterminated *block comment* is intentionally NOT
/// treated as incomplete here (spec: string/template at EOF only).
fn is_unterminated_at_eof(e: &AsError, _src: &str) -> bool {
    e.message == crate::lexer::ERR_UNTERMINATED_STRING
        || e.message == crate::lexer::ERR_UNTERMINATED_TEMPLATE
}

/// Run the interactive REPL until EOF (Ctrl-D) or Ctrl-C.
///
/// Uses `rustyline` line editing when stdin is a TTY; otherwise (piped input)
/// reads lines from stdin directly so non-interactive use still works.
pub async fn run_repl() -> std::io::Result<()> {
    let interp = Rc::new(Interp::new());
    interp.install_self();
    // Persistent session scope: a child of the builtins env so REPL definitions
    // can shadow builtins and persist across lines (builtins resolve upward).
    let env = crate::interp::global_env().child();

    if std::io::stdin().is_terminal() {
        run_tty(&interp, &env).await
    } else {
        run_piped(&interp, &env).await
    }
}

/// Interactive path: rustyline editor with history.
async fn run_tty(interp: &Interp, env: &Environment) -> std::io::Result<()> {
    use rustyline::error::ReadlineError;
    use rustyline::DefaultEditor;

    let mut rl = DefaultEditor::new().map_err(|e| std::io::Error::other(e.to_string()))?;
    // Accumulate physical lines while the input is incomplete (unclosed
    // delimiters or an unterminated string/template), prompting with `..`.
    let mut buf = String::new();
    loop {
        let prompt = if buf.is_empty() { ">> " } else { ".. " };
        match rl.readline(prompt) {
            Ok(line) => {
                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(&line);
                if is_incomplete(&buf) {
                    continue;
                }
                let _ = rl.add_history_entry(buf.as_str());
                let exiting = eval_line(interp, env, &buf).await;
                buf.clear();
                if exiting {
                    return Ok(());
                }
            }
            // Ctrl-C clears a partial buffer (cancels the entry) instead of
            // exiting; on an empty buffer it exits. Ctrl-D (Eof) always exits.
            Err(ReadlineError::Interrupted) => {
                if buf.is_empty() {
                    break;
                } else {
                    buf.clear();
                    continue;
                }
            }
            Err(ReadlineError::Eof) => {
                if !buf.is_empty() {
                    eprintln!("(discarded incomplete input)");
                }
                break;
            }
            Err(e) => {
                return Err(std::io::Error::other(e.to_string()));
            }
        }
    }
    Ok(())
}

/// Non-TTY path: read lines straight from stdin (used by the piped test).
async fn run_piped(interp: &Interp, env: &Environment) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut buf = String::new();
    for line in stdin.lock().lines() {
        let line = line?;
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(&line);
        if is_incomplete(&buf) {
            continue;
        }
        if eval_line(interp, env, &buf).await {
            return Ok(());
        }
        buf.clear();
    }
    // EOF: surface any leftover (e.g. an input that never closed its delimiter).
    if !buf.trim().is_empty() {
        eval_line(interp, env, &buf).await;
    }
    Ok(())
}

/// Lex+parse one input line and evaluate it against the shared interpreter and
/// environment. Errors (lex/parse/panic) are reported and swallowed so the loop
/// continues with the environment intact. Returns `true` if `exit()` was called
/// and the REPL loop should end.
async fn eval_line(interp: &Interp, env: &Environment, line: &str) -> bool {
    if line.trim().is_empty() {
        return false;
    }
    let src_info = Rc::new(SourceInfo {
        path: "<repl>".to_string(),
        text: line.to_string(),
    });

    let tokens = match crate::lexer::lex(line) {
        Ok(t) => t,
        Err(e) => {
            crate::diagnostics::report(&e.with_source(src_info));
            return false;
        }
    };
    let mut program = match crate::parser::parse(&tokens) {
        Ok(p) => p,
        Err(e) => {
            crate::diagnostics::report(&e.with_source(src_info));
            return false;
        }
    };

    // If the last statement is a bare expression, exec the preceding statements
    // and then evaluate it for its value (printed unless nil).
    let trailing = match program.last() {
        Some(Stmt::Expr(_)) => program.pop(),
        _ => None,
    };

    // Drive each input under a LocalSet so spawned tasks join before the prompt
    // returns (no-op until Phase 2).
    let local = tokio::task::LocalSet::new();
    let should_exit = local
        .run_until(async {
            match interp.exec(&program, env).await {
                Err(Control::Panic(e)) => {
                    flush_output(interp);
                    crate::diagnostics::report(&e.with_source(src_info.clone()));
                    return false;
                }
                // exit() — signal the REPL loop to end cleanly.
                Err(Control::Exit(_)) => {
                    flush_output(interp);
                    return true;
                }
                _ => {}
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
                    // exit() during trailing-expression evaluation — end the REPL.
                    Err(Control::Exit(_)) => {
                        flush_output(interp);
                        return true;
                    }
                }
            } else {
                flush_output(interp);
            }
            false
        })
        .await;
    local.await;
    should_exit
}

/// Print and clear any output captured by `print` during this input.
fn flush_output(interp: &Interp) {
    if !interp.output_is_empty() {
        print!("{}", interp.output());
        interp.clear_output();
    }
}

#[cfg(test)]
mod tests {
    use super::is_incomplete;

    #[test]
    fn detects_incomplete_input() {
        assert!(is_incomplete("class P {"));
        assert!(is_incomplete("fn f() {"));
        assert!(is_incomplete("let o = {"));
        assert!(is_incomplete("let a = [1,"));
        assert!(is_incomplete("print("));
        assert!(!is_incomplete("let x = 1"));
        assert!(!is_incomplete("class P { x: number }"));
        assert!(!is_incomplete("print(1 + 2)"));
        assert!(!is_incomplete("}")); // too many closers → not incomplete (real error)
        assert!(is_incomplete("let s = `hello")); // unterminated template → incomplete
        assert!(!is_incomplete("let s = `a ${x} b`")); // complete template w/ braces → balanced
                                                       // Open interpolation. `${` and `${x` lex Ok as TemplateStart with no
                                                       // closing brace → caught by the TemplateStart depth bump. `a${x}b`
                                                       // lexes Err-unterminated → caught by is_unterminated_at_eof.
        assert!(is_incomplete("let f = `${"));
        assert!(is_incomplete("let f = `a${x}b")); // open second interp / unterminated tail
        assert!(!is_incomplete("let s = `a${x}b${y}c`")); // complete multi-interp → balanced
    }
}
