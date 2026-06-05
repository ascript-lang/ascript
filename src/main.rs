use clap::{Parser, Subcommand};
use std::process::ExitCode;

mod lint_config_toml;
#[cfg(feature = "pkg")]
mod pkg;

#[derive(Parser)]
#[command(name = "ascript", about = "The AScript interpreter")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a .as program (bytecode VM) or a compiled .aso program (VM)
    Run {
        /// Run a `.as` file on the legacy tree-walker engine instead of the
        /// bytecode VM (the differential oracle / debugging escape hatch). Must
        /// precede the file. Equivalent to `ASCRIPT_ENGINE=tree-walker`; the flag
        /// takes precedence over the env var. Ignored for `.aso` (always VM).
        #[arg(long = "tree-walker")]
        tree_walker: bool,
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
    Repl {
        /// Run the REPL on the legacy tree-walker engine instead of the bytecode
        /// VM (the differential oracle / debugging escape hatch). Equivalent to
        /// `ASCRIPT_ENGINE=tree-walker`; the flag takes precedence over the env
        /// var. Default → the bytecode VM (the production path post-cutover).
        #[arg(long = "tree-walker")]
        tree_walker: bool,
    },
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
        /// Promote a lint rule to error severity (repeatable). E.g.
        /// `--deny unused-binding`. `syntax-error` is always an error already.
        #[arg(long = "deny", value_name = "RULE")]
        deny: Vec<String>,
        /// Force a lint rule to warning severity (repeatable).
        #[arg(long = "warn", value_name = "RULE")]
        warn: Vec<String>,
        /// Suppress a lint rule entirely (repeatable). `--allow syntax-error` is
        /// accepted but a no-op (syntax errors are always reported).
        #[arg(long = "allow", value_name = "RULE")]
        allow: Vec<String>,
        /// Apply safe autofixes in place (currently `unused-import` removal).
        /// Re-running is idempotent. Mutually exclusive with `--fix-dry-run`.
        #[arg(long)]
        fix: bool,
        /// Show the autofixes that `--fix` would apply (a unified diff, or the
        /// planned edits under `--json`) WITHOUT writing any file. Mutually
        /// exclusive with `--fix`.
        #[arg(long = "fix-dry-run")]
        fix_dry_run: bool,
    },
    /// Run .as test files
    Test { files: Vec<String> },
    /// Run the language server (LSP over stdio)
    #[cfg(feature = "lsp")]
    Lsp,
}

// SP3 §B: run the whole program on a worker thread with an enlarged
// (`WORKER_STACK_SIZE` = 512 MB) stack so the recursion-depth guard
// (`MAX_CALL_DEPTH` = 3000 logical frames) sits comfortably UNDER native capacity
// with > 2× headroom even for the tree-walker's large debug-build frames — the
// guard then converts a deep recursion into a clean catchable panic BEFORE the
// native stack overflows (no SIGABRT). A thread stack is virtual address space;
// only touched pages are committed, so a shallow program pays nothing. The runtime
// stays single-threaded (`current_thread` + `LocalSet`), matching spec §7 and the
// interpreter's `?Send` (Rc-friendly) futures.
fn main() -> ExitCode {
    let worker = std::thread::Builder::new()
        .name("ascript-main".to_string())
        .stack_size(ascript::interp::WORKER_STACK_SIZE)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            rt.block_on(real_main())
        })
        .expect("failed to spawn worker thread");
    worker.join().expect("worker thread panicked")
}

/// Fold an analysis's diagnostics into the run's exit-status accumulators: an
/// `Error`-severity diagnostic fails the run; a surviving `Warning` trips the run
/// only when the file's effective config asks (`--deny-warnings` / toml).
fn tally(
    analysis: &ascript::check::Analysis,
    config: &ascript::check::LintConfig,
    any_error: &mut bool,
    deny_warnings_tripped: &mut bool,
) {
    for d in &analysis.diagnostics {
        match d.severity {
            ascript::check::Severity::Error => *any_error = true,
            ascript::check::Severity::Warning => {
                if config.deny_warnings {
                    *deny_warnings_tripped = true;
                }
            }
            _ => {}
        }
    }
}

/// A machine-readable JSON array of the planned `--fix-dry-run` edits for `path`:
/// `[{"path","start","end","replacement"}, ...]`. Hand-rolled (serde-free) to
/// match the existing JSON renderer's posture.
fn fix_edits_json(path: &str, edits: &[ascript::check::TextEdit]) -> String {
    fn esc(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out
    }
    let mut out = String::from("[");
    for (i, e) in edits.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"path\":\"{}\",\"start\":{},\"end\":{},\"replacement\":\"{}\"}}",
            esc(path),
            e.range.start,
            e.range.end,
            esc(&e.replacement)
        ));
    }
    out.push(']');
    out
}

async fn real_main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Run {
            tree_walker,
            file,
            args,
        } => {
            let path = std::path::Path::new(&file);
            // A `.aso` file is compiled bytecode → run it on the VM (no compile step).
            // A `.as` file is compiled to bytecode and run on the VM as well (this is
            // the production path post-cutover). The tree-walker is kept as the
            // differential oracle and remains reachable as a debugging escape hatch
            // via EITHER the `--tree-walker` flag OR `ASCRIPT_ENGINE=tree-walker`,
            // which route `.as` back to `run_file`. The flag takes precedence over the
            // env var; unset/absent (default) = VM. `.aso` is always the VM.
            let is_aso = path.extension().and_then(|e| e.to_str()) == Some("aso");
            let use_tree_walker =
                tree_walker || std::env::var("ASCRIPT_ENGINE").as_deref() == Ok("tree-walker");
            let result = if is_aso {
                ascript::run_aso_file(path, &args).await
            } else if use_tree_walker {
                ascript::run_file(path, &args).await
            } else {
                ascript::run_file_on_vm(path, &args).await
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
        Command::Repl { tree_walker } => {
            // Default → the bytecode VM REPL (production path). The legacy
            // tree-walker REPL stays reachable via `--tree-walker` OR
            // `ASCRIPT_ENGINE=tree-walker` (flag takes precedence).
            let use_tree_walker =
                tree_walker || std::env::var("ASCRIPT_ENGINE").as_deref() == Ok("tree-walker");
            let result = if use_tree_walker {
                ascript::repl::run_repl_tree_walker().await
            } else {
                ascript::repl::run_repl_vm().await
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("repl error: {}", e);
                    ExitCode::from(1)
                }
            }
        }
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
            deny,
            warn,
            allow,
            fix,
            fix_dry_run,
        } => {
            // `--fix` and `--fix-dry-run` are mutually exclusive (writing vs.
            // previewing) — reject both together as a usage error.
            if fix && fix_dry_run {
                eprintln!("error: --fix and --fix-dry-run are mutually exclusive");
                return ExitCode::from(2);
            }
            // Validate every CLI rule code against the known set up front. An
            // unknown code is a usage error (distinct from a lint failure) —
            // reject it before analyzing anything.
            for code in deny.iter().chain(warn.iter()).chain(allow.iter()) {
                if !ascript::check::LintConfig::is_known_code(code.as_str()) {
                    eprintln!(
                        "error: unknown lint rule '{}' (known rules: {})",
                        code,
                        ascript::check::RULE_CODES.join(", ")
                    );
                    return ExitCode::from(2);
                }
            }

            // Overlay CLI flags onto a config (CLI > toml > rule default). Called
            // per-file AFTER the file's `ascript.toml [lint]` table has seeded the
            // config, so a CLI flag re-applies over (wins per-rule) any toml entry.
            // `deny_warnings` is additive: CLI can only turn it on.
            let overlay_cli = |config: &mut ascript::check::LintConfig| {
                for code in &deny {
                    config.deny(code.as_str());
                }
                for code in &warn {
                    config.warn(code.as_str());
                }
                for code in &allow {
                    config.allow(code.as_str());
                }
                if deny_warnings {
                    config.deny_warnings = true;
                }
            };

            let mut any_error = false;
            // A surviving warning fails the run only when its file's effective
            // config (CLI `--deny-warnings` OR toml `deny_warnings = true`) asks
            // for it. Tracked per-file so a toml-only `deny_warnings` still bites.
            let mut deny_warnings_tripped = false;
            for file in &files {
                let src = match std::fs::read_to_string(file) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("{}: {}", file, e);
                        any_error = true;
                        continue;
                    }
                };
                // Seed the config from the nearest `ascript.toml [lint]`, then
                // overlay the CLI flags. A toml problem (malformed / wrong type /
                // unknown rule) is a clear, file-named usage error → exit 2.
                let mut config = match lint_config_toml::config_for_file(std::path::Path::new(file))
                {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::from(2);
                    }
                };
                overlay_cli(&mut config);
                let analysis = ascript::check::analyze::analyze_with_config(&src, &config);

                // `--fix` / `--fix-dry-run`: collect the allowlisted autofixes and
                // either preview them (dry-run) or apply them in place. After
                // applying, the file is RE-ANALYZED so exit status reflects the
                // post-fix state (a file whose only issue was an auto-fixed import
                // exits clean).
                if fix || fix_dry_run {
                    let edits = ascript::check::collect_fixes(&analysis);
                    let after = ascript::check::apply_edits(&src, &edits);
                    if fix_dry_run {
                        if json {
                            println!("{}", fix_edits_json(file, &edits));
                        } else if after != src {
                            print!("{}", ascript::check::fix::render_diff(file, &src, &after));
                        }
                        // Dry-run: exit status reflects the CURRENT (un-fixed) analysis.
                        tally(&analysis, &config, &mut any_error, &mut deny_warnings_tripped);
                        continue;
                    }
                    // `--fix`: write back only if changed, report, then re-analyze.
                    if after != src {
                        if let Err(e) = std::fs::write(file, &after) {
                            eprintln!("error: could not write {}: {}", file, e);
                            any_error = true;
                            continue;
                        }
                        if !json {
                            println!("fixed {} issue(s) in {}", edits.len(), file);
                        }
                    }
                    let post = ascript::check::analyze::analyze_with_config(&after, &config);
                    tally(&post, &config, &mut any_error, &mut deny_warnings_tripped);
                    // Render the REMAINING (post-fix) diagnostics so the user sees
                    // what `--fix` could not resolve.
                    if json {
                        println!("{}", ascript::check::render::json(file, &post.diagnostics));
                    } else {
                        print!(
                            "{}",
                            ascript::check::render::human(file, &after, &post.diagnostics)
                        );
                    }
                    continue;
                }

                tally(&analysis, &config, &mut any_error, &mut deny_warnings_tripped);
                if json {
                    println!(
                        "{}",
                        ascript::check::render::json(file, &analysis.diagnostics)
                    );
                } else {
                    print!(
                        "{}",
                        ascript::check::render::human(file, &src, &analysis.diagnostics)
                    );
                }
            }
            let fail = any_error || deny_warnings_tripped;
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
