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
}

// Single-threaded runtime matches spec §7's single-threaded event loop and the
// interpreter's `?Send` (Rc-friendly) futures.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { file } => match ascript::run_file(std::path::Path::new(&file)).await {
            Ok(output) => {
                print!("{}", output);
                ExitCode::SUCCESS
            }
            Err(e) => {
                ascript::diagnostics::report(&e);
                ExitCode::from(1)
            }
        },
        Command::Repl => {
            eprintln!("repl: implemented in a later step");
            ExitCode::from(1)
        }
        Command::Fmt { .. } => {
            eprintln!("fmt: implemented in a later step");
            ExitCode::from(1)
        }
        Command::Test { .. } => {
            eprintln!("test: implemented in a later step");
            ExitCode::from(1)
        }
    }
}
