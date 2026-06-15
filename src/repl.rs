//! Interactive REPL (`ascript repl`).
//!
//! There are TWO engines, selected at the CLI:
//!
//! - [`run_repl_vm`] (default) runs each line on the **bytecode VM**. ONE persistent
//!   [`Vm`](crate::vm::Vm) is kept alive across all inputs; its module-scope
//!   `user_globals` table IS the session scope. Each completed input is compiled to
//!   its own [`Chunk`](crate::vm::chunk::Chunk) via [`compile_source`](crate::compile::compile_source)
//!   and run on a fresh [`Fiber`](crate::vm::fiber::Fiber). A top-level
//!   `let`/`const`/`fn`/`class`/`enum`/`import` compiles to `DEFINE_GLOBAL`, so it
//!   persists in `user_globals`; a reference to a PRIOR line's binding compiles to
//!   `GET_GLOBAL` and resolves at run time against that table. This is the production
//!   path post-cutover.
//!
//! - [`run_repl_tree_walker`] runs each line on the legacy tree-walker
//!   (`--tree-walker` / `ASCRIPT_ENGINE=tree-walker`). It keeps ONE
//!   [`Interp`](crate::interp::Interp) and ONE module [`Environment`] alive. It is the
//!   differential oracle / debugging escape hatch.
//!
//! Both engines share observable behavior (verified by the piped-stdin tests in
//! `tests/cli.rs`): if the last statement is a bare expression, its value is printed
//! unless it is `nil`; a statement-only line prints nothing; a `Control::Panic` is
//! reported but does NOT exit the loop — the session scope stays intact and the loop
//! continues; a re-`let` of an existing top-level binding is rejected (`'<n>' is
//! already defined in this scope`) and the original binding survives; `exit()` ends
//! the loop cleanly. Bindings defined BEFORE a panic on the SAME line persist (the
//! define has already executed).
//!
//! Each line is evaluated inside a `tokio::task::LocalSet` so spawned tasks join
//! before the prompt returns.

use std::io::{BufRead, IsTerminal};
use std::rc::Rc;

use crate::ast::Stmt;
use crate::env::Environment;
use crate::error::{AsError, SourceInfo};
use crate::interp::{Control, Interp};
use crate::token::Tok;
use crate::value::ValueKind;
use crate::vm::chunk::FnProto;
use crate::vm::value_ext::{Closure, RunOutcome};
use crate::vm::Vm;

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

// ============================================================================
// Bytecode VM REPL (the default engine)
// ============================================================================

/// Run the interactive VM REPL until EOF (Ctrl-D) or Ctrl-C.
///
/// Keeps ONE persistent [`Vm`] over a live-output [`Interp`]: its `user_globals`
/// table persists across lines and IS the session scope. Output streams live
/// (`OutputSink::Live`), so a line's `print(..)` output appears in order, then the
/// trailing-expression value (if any) is printed after the line finishes.
pub async fn run_repl_vm() -> std::io::Result<()> {
    // Live output so `print` streams in order; the VM REPL prints a trailing
    // expression's value AFTER the line's body has run, preserving ordering.
    let interp = Rc::new(Interp::new_live());
    interp.install_self();
    let vm = Vm::new(interp);

    let result = if std::io::stdin().is_terminal() {
        run_tty_vm(&vm).await
    } else {
        run_piped_vm(&vm).await
    };
    // End-of-session cycle collection (mirrors the other VM entry points): reclaim
    // any leftover reference cycles created during the session for a clean shutdown.
    crate::gc::collect();
    result
}

/// Interactive VM path: rustyline editor with history.
async fn run_tty_vm(vm: &Rc<Vm>) -> std::io::Result<()> {
    use rustyline::error::ReadlineError;
    use rustyline::DefaultEditor;

    let mut rl = DefaultEditor::new().map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut buf = String::new();
    // Accumulated session source for `worker fn` slice building: each completed
    // line is appended here so `worker_source` always contains the full context
    // needed to recompile a `worker fn` in an isolate.
    let mut session_src = String::new();
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
                let exiting = eval_line_vm(vm, &buf, &mut session_src).await;
                buf.clear();
                if exiting {
                    return Ok(());
                }
            }
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

/// Non-TTY VM path: read lines straight from stdin (used by the piped tests).
async fn run_piped_vm(vm: &Rc<Vm>) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut buf = String::new();
    // Accumulated session source for `worker fn` slice building (see `eval_line_vm`).
    let mut session_src = String::new();
    for line in stdin.lock().lines() {
        let line = line?;
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(&line);
        if is_incomplete(&buf) {
            continue;
        }
        if eval_line_vm(vm, &buf, &mut session_src).await {
            return Ok(());
        }
        buf.clear();
    }
    // EOF: surface any leftover (e.g. an input that never closed its delimiter).
    if !buf.trim().is_empty() {
        eval_line_vm(vm, &buf, &mut session_src).await;
    }
    Ok(())
}

/// Compile one completed input to a [`Chunk`](crate::vm::chunk::Chunk) and run it
/// on the persistent [`Vm`]. The compiler already RETURNs a trailing bare
/// expression's value as the program's [`RunOutcome::Done`] value (and `Nil` for a
/// statement-terminated line), so the trailing-value behavior needs NO REPL re-parse
/// — we just print the `Done` value unless it is `Nil`. A `Control::Panic` is
/// reported and swallowed so the loop continues with the session scope intact (the
/// per-line fiber is discarded; the `Vm` and its `user_globals` persist). Returns
/// `true` when `exit()` was called and the loop should end.
async fn eval_line_vm(vm: &Rc<Vm>, line: &str, session_src: &mut String) -> bool {
    if line.trim().is_empty() {
        return false;
    }
    let src_info = Rc::new(SourceInfo {
        path: "<repl>".to_string(),
        text: line.to_string(),
    });

    // Compile the line to a top-level chunk. A lex/parse/compile error is reported
    // and the loop continues (no session-scope mutation happened).
    let chunk = match crate::compile::compile_source(line) {
        Ok(c) => c,
        Err(e) => {
            crate::diagnostics::report(&AsError::at(e.message, e.span).with_source(src_info));
            return false;
        }
    };

    // Workers Spec A: keep `worker_source` up-to-date so a `worker fn` defined on
    // an earlier REPL line can be recompiled into a code slice when called.  We
    // append this (successfully compiled) line to the accumulated session source and
    // record it on the interpreter.  We do this AFTER compilation succeeds (so
    // syntax-invalid lines are not accumulated) but BEFORE running (so the source
    // is available even when the line defines-then-calls a worker fn in one shot).
    if !session_src.is_empty() {
        session_src.push('\n');
    }
    session_src.push_str(line);
    vm.interp().set_worker_source(session_src);
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

    // Drive the line under a LocalSet so spawned async tasks join before the prompt
    // returns (structured concurrency).
    let local = tokio::task::LocalSet::new();
    let should_exit = local
        .run_until(crate::interp::telemetry_root_scope(async {
            match vm.run(&mut fiber).await {
                // The trailing-expression value (or `Nil` for a statement line) — the
                // compiler emitted `RETURN <value>`. Print it unless it is `Nil`,
                // matching the tree-walker REPL's trailing-expression rule.
                Ok(RunOutcome::Done(value)) => {
                    if !matches!(value.kind(), ValueKind::Nil) {
                        println!("{}", value);
                    }
                    false
                }
                Ok(RunOutcome::Yielded(_)) => {
                    // A top-level program cannot yield (no enclosing generator).
                    // Defensive: treat as a no-value line.
                    false
                }
                // A panic is reported but does NOT end the session — the per-line
                // fiber is discarded; the Vm + user_globals persist (incl. any
                // binding defined before the panic on this same line).
                Err(Control::Panic(e)) => {
                    crate::diagnostics::report(&e.with_source(src_info));
                    false
                }
                // A top-level `?` propagation simply ends the line (no print).
                Err(Control::Propagate(_)) => false,
                // exit() — end the REPL loop cleanly.
                Err(Control::Exit(_)) => true,
            }
        }))
        .await;
    local.await; // drain spawned tasks (structured join)
    should_exit
}

// ============================================================================
// Tree-walker REPL (the legacy / oracle engine)
// ============================================================================

/// Run the interactive tree-walker REPL until EOF (Ctrl-D) or Ctrl-C.
///
/// Uses `rustyline` line editing when stdin is a TTY; otherwise (piped input)
/// reads lines from stdin directly so non-interactive use still works.
pub async fn run_repl_tree_walker() -> std::io::Result<()> {
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
        .run_until(crate::interp::telemetry_root_scope(async {
            // DEFER §5.1: install a fresh defer list on the persistent session env for
            // THIS line, so a top-level `defer` typed at the REPL drains at line end.
            // The list is separate from variable bindings, so the session scope (vars)
            // is unaffected; each line installs a fresh empty list and drains it.
            let defer_list = env.install_defer_scope();
            match interp.exec(&program, env).await {
                Err(Control::Panic(e)) => {
                    interp.drain_session_defers(&defer_list).await;
                    flush_output(interp);
                    crate::diagnostics::report(&e.with_source(src_info.clone()));
                    return false;
                }
                // exit() — signal the REPL loop to end cleanly (defers skipped, §3.3).
                Err(Control::Exit(_)) => {
                    flush_output(interp);
                    return true;
                }
                _ => {}
            }

            if let Some(Stmt::Expr(expr)) = trailing {
                match interp.eval_expr(&expr, env).await {
                    Ok(value) => {
                        interp.drain_session_defers(&defer_list).await;
                        flush_output(interp);
                        if !matches!(value.kind(), ValueKind::Nil) {
                            println!("{}", value);
                        }
                    }
                    Err(Control::Panic(e)) => {
                        interp.drain_session_defers(&defer_list).await;
                        flush_output(interp);
                        crate::diagnostics::report(&e.with_source(src_info));
                    }
                    Err(Control::Propagate(_)) => {
                        interp.drain_session_defers(&defer_list).await;
                        flush_output(interp);
                    }
                    // exit() during trailing-expression evaluation — end the REPL
                    // (defers skipped, §3.3).
                    Err(Control::Exit(_)) => {
                        flush_output(interp);
                        return true;
                    }
                }
            } else {
                interp.drain_session_defers(&defer_list).await;
                flush_output(interp);
            }
            false
        }))
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
