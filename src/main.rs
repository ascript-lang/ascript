use clap::{Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "ascript", about = "The AScript interpreter")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a .as program
    Run { file: String },
    /// Start the interactive REPL
    Repl,
    /// Format .as source files
    Fmt { files: Vec<String> },
    /// Run .as test files
    Test { files: Vec<String> },
    /// Run the language server (LSP over stdio)
    #[cfg(feature = "lsp")]
    Lsp,
}

// Single-threaded runtime matches spec §7's single-threaded event loop and the
// interpreter's `?Send` (Rc-friendly) futures.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { file } => match ascript::run_file(std::path::Path::new(&file)).await {
            // Output already streamed live by `run_file` (OutputSink::Live).
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                ascript::diagnostics::report(&e);
                ExitCode::from(1)
            }
        },
        Command::Repl => match ascript::repl::run_repl().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("repl error: {}", e);
                ExitCode::from(1)
            }
        },
        Command::Fmt { files } => {
            let mut code = ExitCode::SUCCESS;
            for file in &files {
                match std::fs::read_to_string(file) {
                    Ok(src) => match ascript::fmt::format_source(&src) {
                        Ok(formatted) => {
                            if let Err(e) = std::fs::write(file, &formatted) {
                                eprintln!("error: could not write {}: {}", file, e);
                                code = ExitCode::from(1);
                            } else {
                                println!("formatted {}", file);
                            }
                        }
                        Err(e) => {
                            eprintln!("error: could not format {}: {}", file, e);
                            code = ExitCode::from(1);
                        }
                    },
                    Err(e) => {
                        eprintln!("error: could not read {}: {}", file, e);
                        code = ExitCode::from(1);
                    }
                }
            }
            code
        }
        Command::Test { files } => match ascript::run_tests(&files).await {
            Ok(summary) => {
                for (name, message) in &summary.failures {
                    println!("FAIL {}: {}", name, message);
                }
                println!("ok. {} passed; {} failed", summary.passed, summary.failed);
                if summary.failed > 0 {
                    ExitCode::from(1)
                } else {
                    ExitCode::SUCCESS
                }
            }
            Err(e) => {
                ascript::diagnostics::report(&e);
                ExitCode::from(1)
            }
        },
        #[cfg(feature = "lsp")]
        Command::Lsp => {
            ascript::lsp::run_server().await;
            ExitCode::SUCCESS
        }
    }
}
