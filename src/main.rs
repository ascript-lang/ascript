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
    /// Run a .as program (tree-walker) or a compiled .aso program (VM)
    Run {
        file: String,
        /// Trailing arguments forwarded to the script as `env.args()`.
        /// Hyphen-prefixed values (e.g. `--flag`) are also captured.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Compile a .as program to bytecode (.aso)
    Build {
        file: String,
        /// Output path (defaults to `<file-stem>.aso`).
        #[arg(long, short)]
        out: Option<String>,
    },
    /// Start the interactive REPL
    Repl,
    /// Format .as source files
    Fmt { files: Vec<String> },
    /// Statically check .as files (syntax + lints)
    Check {
        files: Vec<String>,
        /// Emit machine-readable JSON instead of human output.
        #[arg(long)]
        json: bool,
        /// Treat all warnings as errors (non-zero exit on any warning).
        #[arg(long)]
        deny_warnings: bool,
    },
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
        Command::Run { file, args } => {
            let path = std::path::Path::new(&file);
            // A `.aso` file is compiled bytecode → run it on the VM (no compile step).
            // A `.as` file runs through the tree-walker (the production path until the
            // VM cutover). The dispatch is purely by extension.
            let result = if path.extension().and_then(|e| e.to_str()) == Some("aso") {
                ascript::run_aso_file(path, &args).await
            } else {
                ascript::run_file(path, &args).await
            };
            match result {
                // Output already streamed live (OutputSink::Live).
                // `code` is 0 for normal exit or whatever `exit(n)` requested.
                Ok(code) => ExitCode::from(code as u8),
                Err(e) => {
                    ascript::diagnostics::report(&e);
                    ExitCode::from(1)
                }
            }
        }
        Command::Build { file, out } => {
            let out_path = out.as_deref().map(std::path::Path::new);
            match ascript::build_file(std::path::Path::new(&file), out_path) {
                Ok(written) => {
                    println!("compiled {} -> {}", file, written.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    ascript::diagnostics::report(&e);
                    ExitCode::from(1)
                }
            }
        }
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
                    Ok(src) => {
                        let parse = ascript::syntax::parser::parse(&src);
                        if !parse.errors.is_empty() {
                            eprintln!("error: {}: parse error; not formatting", file);
                            code = ExitCode::from(1);
                            continue;
                        }
                        let formatted = ascript::syntax::format_tree(&src);
                        if let Err(e) = std::fs::write(file, &formatted) {
                            eprintln!("error: could not write {}: {}", file, e);
                            code = ExitCode::from(1);
                        } else {
                            println!("formatted {}", file);
                        }
                    }
                    Err(e) => {
                        eprintln!("error: could not read {}: {}", file, e);
                        code = ExitCode::from(1);
                    }
                }
            }
            code
        }
        Command::Check {
            files,
            json,
            deny_warnings,
        } => {
            let mut any_error = false;
            let mut any_warning = false;
            for file in &files {
                let src = match std::fs::read_to_string(file) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("{}: {}", file, e);
                        any_error = true;
                        continue;
                    }
                };
                let analysis = ascript::check::analyze(&src);
                for d in &analysis.diagnostics {
                    match d.severity {
                        ascript::check::Severity::Error => any_error = true,
                        ascript::check::Severity::Warning => any_warning = true,
                        _ => {}
                    }
                }
                if json {
                    println!("{}", ascript::check::render::json(file, &analysis.diagnostics));
                } else {
                    print!(
                        "{}",
                        ascript::check::render::human(file, &src, &analysis.diagnostics)
                    );
                }
            }
            let fail = any_error || (deny_warnings && any_warning);
            if fail {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
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
