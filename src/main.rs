use clap::{Parser, Subcommand};
use std::process::ExitCode;

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
        /// Offline-deterministic: resolve dependencies EXACTLY from `ascript.lock`
        /// (no network), failing on any drift, missing lock, or integrity
        /// mismatch. For CI / sandboxes.
        #[arg(long = "locked")]
        locked: bool,
        /// FFI §4.2: deny one or more capabilities (opt-out). Repeatable and/or
        /// comma-separated, e.g. `--deny ffi,process`. Valid names: fs, net,
        /// process, ffi, env. Composes (union of denials) with the manifest
        /// `[capabilities]` table; denial is monotone (CLI cannot re-grant).
        #[arg(long = "deny", value_name = "CAP", value_delimiter = ',')]
        deny: Vec<String>,
        /// FFI §4.2: deny ALL five dangerous capabilities (fs, net, process, ffi,
        /// env). Sugar for `--deny fs,net,process,ffi,env`.
        #[arg(long = "sandbox")]
        sandbox: bool,
        /// FFI §4.4: a granular net carve-out: `--deny-net=external` (allow
        /// loopback/private, block public) or `--deny-net=all`.
        #[arg(long = "deny-net", value_name = "MODE")]
        deny_net: Option<String>,
        /// FFI §4.4: a granular fs carve-out: `--deny-fs=write` (reads allowed,
        /// writes denied) or `--deny-fs=all`.
        #[arg(long = "deny-fs", value_name = "MODE")]
        deny_fs: Option<String>,
        /// DBG: run under the debugger — start a Debug Adapter Protocol (DAP) server
        /// over stdio for this program instead of running it normally. An editor's
        /// DAP client drives breakpoints/stepping/inspection. Requires the `dap`
        /// feature (default-on); a `--no-default-features` build reports a rebuild
        /// hint. The program stops at entry; output rides DAP `output` events.
        #[arg(long = "inspect")]
        inspect: bool,
        /// DBG: run under the CPU sampling profiler, writing a profile artifact on
        /// exit. v1 accepts only `cpu` (the arg is reserved for future modes). The
        /// program runs to completion normally — profiling is observation-only, so its
        /// stdout is byte-identical to a plain run. Requires the `profile` feature
        /// (default-on); a `--no-default-features` build reports a rebuild hint.
        #[arg(long = "profile", value_name = "MODE")]
        profile: Option<String>,
        /// DBG: the profile output path (default `profile.json`, or `profile.txt` for
        /// the collapsed format). Only meaningful with `--profile`.
        #[arg(long = "out", short = 'o', value_name = "FILE")]
        out: Option<String>,
        /// DBG: the profiler's wall-clock sample rate in samples/second (default 1000,
        /// i.e. ~1ms). Only meaningful with `--profile`.
        #[arg(long = "profile-hz", value_name = "N")]
        profile_hz: Option<u32>,
        /// DBG: the profile artifact format — `speedscope` (default, opens at
        /// speedscope.app) or `collapsed` (Brendan-Gregg folded stacks). Only
        /// meaningful with `--profile`. The special value `deterministic-speedscope` /
        /// `deterministic-collapsed` selects the deterministic (call-structure-driven,
        /// golden-stable) sample clock — used by goldens/tests, no wall-clock thread.
        #[arg(long = "profile-format", value_name = "FMT")]
        profile_format: Option<String>,
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
        /// Strip the optional DBG debug section (module source + per-proto
        /// line/variable info). Default: debug info is INCLUDED.
        #[arg(long = "strip")]
        strip: bool,
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
    /// Generate API documentation from `///` doc-comments (DX D1).
    #[cfg(feature = "doc")]
    Doc {
        /// Files or directories to document (default: the current directory,
        /// discovered like `check`). Resolves imports to document a whole project.
        paths: Vec<String>,
        /// Output directory (default `target/doc/`). Used for `--format html`.
        #[arg(long = "out", value_name = "DIR")]
        out: Option<String>,
        /// Output format: `html` (default; a self-contained `target/doc/` tree) or
        /// `md` (Markdown to stdout / `--out`).
        #[arg(long = "format", value_name = "FORMAT", default_value = "html")]
        format: String,
        /// Include non-exported declarations (default: public API only).
        #[arg(long = "private")]
        private: bool,
        /// Open the generated `index.html` after writing (best-effort, `sys`-gated).
        #[arg(long = "open")]
        open: bool,
        /// Doc-lint: write nothing; exit non-zero if a public declaration lacks a
        /// doc-comment (a CI gate), reporting the undocumented symbols.
        #[arg(long = "check")]
        check: bool,
    },
    /// Run .as test files
    Test {
        files: Vec<String>,
        /// Offline-deterministic: resolve dependencies EXACTLY from `ascript.lock`
        /// (no network), failing on drift / missing lock / integrity mismatch.
        #[arg(long = "locked")]
        locked: bool,
        /// FFI §4.2: deny one or more capabilities for the test run (opt-out).
        /// Comma-separated / repeatable. Composes with the manifest.
        #[arg(long = "deny", value_name = "CAP", value_delimiter = ',')]
        deny: Vec<String>,
        /// FFI §4.2: deny ALL five dangerous capabilities for the test run.
        #[arg(long = "sandbox")]
        sandbox: bool,
        /// DX D2: run each test FILE in its own shared-nothing worker isolate, in
        /// parallel. `--parallel` (no value) uses `num_cpus` isolates; `--parallel=N`
        /// caps at N (further clamped by `$ASCRIPT_WORKERS`). Omitted = serial (the
        /// default). The aggregated result + exit code are deterministic regardless of
        /// completion order; a single file degrades to the serial path.
        #[arg(
            long = "parallel",
            value_name = "N",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "0"
        )]
        parallel: Option<usize>,
    },
    /// Run the language server (LSP over stdio)
    #[cfg(feature = "lsp")]
    Lsp {
        /// Accepted for compatibility with LSP clients that pass `--stdio` (e.g. some
        /// VS Code client configs). stdio is the only transport, so this is a no-op.
        #[arg(long = "stdio")]
        stdio: bool,
    },
    /// Run the Debug Adapter Protocol (DAP) server over stdio. An editor's DAP client
    /// connects and drives launch/breakpoints/stepping. The program to debug comes
    /// from the `launch` request (or use `run --inspect <file>` to pre-set it).
    #[cfg(feature = "dap")]
    Dap {
        /// Accepted for compatibility with DAP clients that pass `--stdio`. stdio is
        /// the only transport, so this is a no-op.
        #[arg(long = "stdio")]
        stdio: bool,
    },
    /// Add a dependency to ascript.toml + lock (git/url/path spec).
    #[cfg(feature = "pkg")]
    Add {
        /// e.g. `github.com/owner/repo@v1.0.0`, `https://host/pkg-1.2.0.tar.gz`,
        /// or `../local-path`.
        spec: String,
    },
    /// Remove a dependency from ascript.toml + re-lock.
    #[cfg(feature = "pkg")]
    Remove { name: String },
    /// Resolve + fetch dependencies and write/verify ascript.lock.
    #[cfg(feature = "pkg")]
    Install {
        /// Install EXACTLY from ascript.lock (no network); fail on drift.
        #[arg(long = "locked")]
        locked: bool,
    },
    /// Raise pins + re-lock (optionally a single dependency).
    #[cfg(feature = "pkg")]
    Update { name: Option<String> },
    /// (Re)generate ascript.lock from the manifest.
    #[cfg(feature = "pkg")]
    Lock,
    /// Print the resolved dependency graph.
    #[cfg(feature = "pkg")]
    Tree,
    /// Re-hash the cache store against the lock integrity (fail-closed).
    #[cfg(feature = "pkg")]
    Verify,
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
/// FFI §4.2/§4.5: compose the initial [`CapSet`](ascript::stdlib::caps::CapSet)
/// from the CLI flags AND the nearest `ascript.toml` `[capabilities]` table.
///
/// Composition is **most-restrictive-wins** (denial is monotone): the manifest's
/// denials are applied first (the manifest floor), then the CLI's are unioned on
/// top. The CLI can therefore only ever ADD denials, never re-grant what the
/// manifest denied — exactly the spec's "CLI overrides manifest" within a monotone
/// model (CLI overriding means tightening further, never loosening).
///
/// Returns `Ok(None)` when nothing is denied (all granted → the byte-identical
/// default). A bad cap name / deny-mode (CLI or manifest) is a clean `Err`.
fn compose_caps(
    _path: &std::path::Path,
    deny: &[String],
    sandbox: bool,
    deny_net: Option<&str>,
    deny_fs: Option<&str>,
) -> Result<Option<ascript::stdlib::caps::CapSet>, String> {
    use ascript::stdlib::caps::{Cap, CapSet, FsDeny, FsScope, NetDeny, NetScope};

    // Start from the manifest floor (under the `pkg` feature), else all-granted.
    #[cfg(feature = "pkg")]
    let (mut set, had_manifest) = match pkg::manifest::Manifest::load_nearest(_path)? {
        Some((_, manifest)) => (manifest.capset()?, manifest.capabilities.is_some()),
        None => (CapSet::all_granted(), false),
    };
    #[cfg(not(feature = "pkg"))]
    let (mut set, had_manifest) = (CapSet::all_granted(), false);

    let mut had_cli = false;

    if sandbox {
        set.deny_all_dangerous();
        had_cli = true;
    }
    // `--deny a,b` (value_delimiter splits commas; the flag is also repeatable).
    for name in deny {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        match ascript::stdlib::caps::cap_name(name) {
            Some(cap) => {
                set.deny(cap);
                had_cli = true;
            }
            None => {
                return Err(format!(
                    "--deny: unknown capability '{name}' (expected one of: fs, net, process, ffi, env)"
                ))
            }
        }
    }
    // Granular net carve-out (CLI). A CLI carve-out tightens the net scope.
    if let Some(mode) = deny_net {
        let deny = match mode {
            "external" => NetDeny::External,
            "all" => NetDeny::All,
            other => {
                return Err(format!(
                    "--deny-net: expected 'external' or 'all', got '{other}'"
                ))
            }
        };
        set.set_net_scope(NetScope { deny, allow: Vec::new() });
        had_cli = true;
    }
    // Granular fs carve-out (CLI).
    if let Some(mode) = deny_fs {
        let deny = match mode {
            "write" => FsDeny::Write,
            "all" => FsDeny::All,
            other => {
                return Err(format!(
                    "--deny-fs: expected 'write' or 'all', got '{other}'"
                ))
            }
        };
        set.set_fs_scope(FsScope { deny, allow: Vec::new() });
        had_cli = true;
    }
    // Re-apply whole-cap CLI denials of fs/net AFTER scopes so `--deny net`
    // overrides a manifest carve-out (monotone: deny wins).
    for name in deny {
        if let Some(cap) = ascript::stdlib::caps::cap_name(name.trim()) {
            if matches!(cap, Cap::Fs | Cap::Net) {
                set.deny(cap);
            }
        }
    }
    if sandbox {
        set.deny_all_dangerous();
    }

    if had_cli || had_manifest {
        Ok(Some(set))
    } else {
        Ok(None) // nothing denied → keep the byte-identical default
    }
}

/// DBG Task 7: assemble a [`ascript::ProfileConfig`] from the `--profile`/`-o`/
/// `--profile-hz`/`--profile-format` flags and run the program profiled. `mode` v1
/// accepts only `cpu`. The `--profile-format` value also selects the sample clock:
/// `speedscope`/`collapsed` use the wall-clock sampler thread, while the
/// `deterministic-*` variants use the inline (call-structure-driven, golden-stable)
/// clock with no thread.
#[cfg(feature = "profile")]
#[allow(clippy::too_many_arguments)]
async fn run_profiled(
    path: &std::path::Path,
    args: &[String],
    packages: Option<ascript::interp::PackageMap>,
    caps: Option<ascript::stdlib::caps::CapSet>,
    mode: &str,
    out: Option<&str>,
    profile_hz: Option<u32>,
    profile_format: Option<&str>,
) -> Result<i32, ascript::error::AsError> {
    use ascript::profile::ProfileFormat;
    use ascript::vm::instrument::ProfileMode;

    if mode != "cpu" {
        return Err(ascript::error::AsError::new(format!(
            "unknown profile mode '{mode}' — v1 supports only 'cpu'"
        )));
    }

    // Resolve format + sample clock from --profile-format. `deterministic-*` selects
    // the inline sample clock (golden-stable, no thread); the bare names use the
    // wall-clock sampler thread.
    let (format, det) = match profile_format.unwrap_or("speedscope") {
        "speedscope" => (ProfileFormat::Speedscope, false),
        "collapsed" => (ProfileFormat::Collapsed, false),
        "deterministic-speedscope" => (ProfileFormat::Speedscope, true),
        "deterministic-collapsed" => (ProfileFormat::Collapsed, true),
        other => {
            return Err(ascript::error::AsError::new(format!(
                "unknown profile format '{other}' — expected 'speedscope' or 'collapsed' (optionally 'deterministic-' prefixed)"
            )));
        }
    };
    let pmode = if det {
        ProfileMode::Deterministic
    } else {
        ProfileMode::Wallclock
    };

    // Default sample rate 1000 Hz (~1ms); 0 is rejected (no division by zero).
    let hz = profile_hz.unwrap_or(1000);
    if hz == 0 {
        return Err(ascript::error::AsError::new(
            "--profile-hz must be a positive number of samples per second".to_string(),
        ));
    }
    let interval = std::time::Duration::from_nanos(1_000_000_000u64 / hz as u64);

    // Default output path depends on the format.
    let out_path = match out {
        Some(p) => std::path::PathBuf::from(p),
        None => std::path::PathBuf::from(match format {
            ProfileFormat::Speedscope => "profile.json",
            ProfileFormat::Collapsed => "profile.txt",
        }),
    };

    let cfg = ascript::ProfileConfig {
        mode: pmode,
        interval,
        format,
        out: out_path,
    };
    ascript::run_file_on_vm_profiled(path, args, packages, caps, cfg).await
}

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
            ascript::check::Severity::Warning if config.deny_warnings => {
                *deny_warnings_tripped = true;
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
            locked,
            deny,
            sandbox,
            deny_net,
            deny_fs,
            inspect,
            profile,
            out,
            profile_hz,
            profile_format,
            file,
            args,
        } => {
            // `--locked` only affects dependency resolution, which is `pkg`-gated.
            #[cfg(not(feature = "pkg"))]
            let _ = locked;
            let path = std::path::Path::new(&file);
            // FFI §4.2/§4.5: compose the initial CapSet from CLI flags + the
            // manifest `[capabilities]` table (most-restrictive-wins; denial is
            // monotone). `None` → all granted (byte-identical default).
            let caps = match compose_caps(path, &deny, sandbox, deny_net.as_deref(), deny_fs.as_deref()) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(1);
                }
            };
            // DBG: `--inspect` routes the program to the DAP server (break-on-entry,
            // editor-driven) instead of running it normally. Computed AFTER `compose_caps`
            // so the SAME capability set is enforced under the debugger (review F2 /
            // Gate-0 — no privilege escalation vs a normal run). The `dap` feature owns
            // this; without it, report a clean rebuild hint rather than silently running.
            if inspect {
                #[cfg(feature = "dap")]
                {
                    let program = std::path::PathBuf::from(&file);
                    return match ascript::dap::run_server(Some(program), args, caps) {
                        Ok(code) => ExitCode::from(code as u8),
                        Err(e) => {
                            eprintln!("dap error: {e}");
                            ExitCode::from(1)
                        }
                    };
                }
                #[cfg(not(feature = "dap"))]
                {
                    let _ = (&args, &caps);
                    eprintln!(
                        "error: `--inspect` requires the `dap` feature; rebuild with it enabled (it is on by default)"
                    );
                    return ExitCode::from(1);
                }
            }
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
            // SP6: ensure the lock is satisfied (MVS resolve + fetch-on-miss, or
            // `--locked` offline against ascript.lock), assemble the resolved
            // package map, and inject it so a bare `import "pkg"` resolves
            // identically on both engines. `.aso` runs skip this (a compiled
            // module's imports were resolved against its own dir). Under
            // `--no-default-features` the `pkg` feature is off → no map → bare
            // specifier is "unknown package".
            #[cfg(feature = "pkg")]
            let packages = if is_aso {
                None
            } else {
                match pkg::commands::ensure_lock(path, locked) {
                    Ok(map) => map,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::from(1);
                    }
                }
            };
            // `packages` is only computed under the `pkg` feature; pass `None`
            // otherwise. `caps` threads through every run path.
            #[cfg(not(feature = "pkg"))]
            let packages: Option<ascript::interp::PackageMap> = None;
            // DBG Task 7: `--profile cpu` runs the program on the VM under the sampling
            // profiler, then writes a profile artifact. Only for `.as` on the VM (not
            // `.aso`, not the tree-walker — the profiler hangs off the VM frame seam).
            // Without the flag the path is byte-for-byte unchanged. The `profile`
            // feature owns this; without it, a clean rebuild hint.
            if let Some(mode) = profile.as_deref() {
                if is_aso || use_tree_walker {
                    eprintln!(
                        "error: --profile is only supported for `.as` files on the bytecode VM (not .aso or --tree-walker)"
                    );
                    return ExitCode::from(1);
                }
                #[cfg(feature = "profile")]
                {
                    let result = run_profiled(
                        path,
                        &args,
                        packages,
                        caps,
                        mode,
                        out.as_deref(),
                        profile_hz,
                        profile_format.as_deref(),
                    )
                    .await;
                    return match result {
                        Ok(code) => ExitCode::from(code as u8),
                        Err(e) => {
                            ascript::diagnostics::report(&e);
                            ExitCode::from(1)
                        }
                    };
                }
                #[cfg(not(feature = "profile"))]
                {
                    let _ = (&packages, &caps, &out, &profile_hz, &profile_format, mode);
                    eprintln!(
                        "error: `--profile` requires the `profile` feature; rebuild with it enabled (it is on by default)"
                    );
                    return ExitCode::from(1);
                }
            }
            // `out`/`profile_hz`/`profile_format` are only consulted on the profiled
            // path above; keep them from tripping unused warnings on the normal path.
            let _ = (&out, &profile_hz, &profile_format);
            let result = if is_aso {
                ascript::run_aso_file(path, &args, caps).await
            } else if use_tree_walker {
                ascript::run_file_with_packages(path, &args, packages, caps).await
            } else {
                ascript::run_file_on_vm_with_packages(path, &args, packages, caps).await
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
        Command::Build { file, out, strip } => {
            let out_path = out.as_deref().map(std::path::Path::new);
            match ascript::build_file(std::path::Path::new(&file), out_path, !strip) {
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
                let mut config =
                    match ascript::check::config_toml::config_for_file(std::path::Path::new(file))
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
        #[cfg(feature = "doc")]
        Command::Doc {
            paths,
            out,
            format,
            private,
            open,
            check,
        } => run_doc(paths, out, format, private, open, check),
        Command::Test {
            files,
            locked,
            deny,
            sandbox,
            parallel,
        } => {
            // DX D2: `--parallel` (no value) → `num_cpus` isolates; `--parallel=N` → N;
            // absent → `None` (serial). The `default_missing_value = "0"` sentinel maps
            // the bare flag to "auto" (num_cpus); the runner clamps to `$ASCRIPT_WORKERS`.
            let parallel = parallel.map(|n| if n == 0 { num_cpus::get() } else { n });
            // FFI §4.2/§4.5: compose caps from CLI + the test files' nearest
            // manifest (no granular CLI carve-outs on `test`).
            let cap_path = files
                .first()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let caps = match compose_caps(&cap_path, &deny, sandbox, None, None) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(1);
                }
            };
            // SP6: ensure the lock is satisfied for the test files' nearest
            // manifest (resolve+fetch-on-miss, or `--locked` offline) and inject
            // the resolved package map so a bare `import "pkg"` in a test works.
            #[cfg(feature = "pkg")]
            let test_result = {
                let packages = match files.first() {
                    Some(first) => {
                        match pkg::commands::ensure_lock(std::path::Path::new(first), locked) {
                            Ok(map) => map,
                            Err(e) => {
                                eprintln!("error: {e}");
                                return ExitCode::from(1);
                            }
                        }
                    }
                    None => None,
                };
                ascript::run_tests_with_options(&files, packages, caps, parallel).await
            };
            #[cfg(not(feature = "pkg"))]
            let test_result = {
                let _ = locked;
                ascript::run_tests_with_options(&files, None, caps, parallel).await
            };
            match test_result {
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
            }
        }
        #[cfg(feature = "lsp")]
        Command::Lsp { .. } => {
            ascript::lsp::run_server().await;
            ExitCode::SUCCESS
        }
        #[cfg(feature = "dap")]
        Command::Dap { .. } => {
            // The DAP server is fully synchronous (it spawns the debuggee on its own
            // thread + runtime); the program comes from the `launch` request. Caps are
            // `None` (all-granted) here — `ascript dap` takes no CLI sandbox flags; a
            // sandboxed debug session uses `ascript run --inspect --sandbox <file>`,
            // which threads its composed CapSet through (review F2).
            match ascript::dap::run_server(None, Vec::new(), None) {
                Ok(code) => ExitCode::from(code as u8),
                Err(e) => {
                    eprintln!("dap error: {e}");
                    ExitCode::from(1)
                }
            }
        }
        #[cfg(feature = "pkg")]
        Command::Add { spec } => pkg_command_exit(pkg::commands::cmd_add(&spec)),
        #[cfg(feature = "pkg")]
        Command::Remove { name } => pkg_command_exit(pkg::commands::cmd_remove(&name)),
        #[cfg(feature = "pkg")]
        Command::Install { locked } => pkg_command_exit(pkg::commands::cmd_install(locked)),
        #[cfg(feature = "pkg")]
        Command::Update { name } => {
            pkg_command_exit(pkg::commands::cmd_update(name.as_deref()))
        }
        #[cfg(feature = "pkg")]
        Command::Lock => pkg_command_exit(pkg::commands::cmd_lock()),
        #[cfg(feature = "pkg")]
        Command::Tree => pkg_command_exit(pkg::commands::cmd_tree()),
        #[cfg(feature = "pkg")]
        Command::Verify => pkg_command_exit(pkg::commands::cmd_verify()),
    }
}

/// DX D1: `ascript doc` — discover `.as` files, build the doc model (static CST
/// walk, never the interpreter), and emit Markdown / HTML, or `--check` for
/// undocumented public symbols.
#[cfg(feature = "doc")]
fn run_doc(
    paths: Vec<String>,
    out: Option<String>,
    format: String,
    private: bool,
    open: bool,
    check: bool,
) -> ExitCode {
    use ascript::doc;
    use ascript::lsp::workspace::{discover_as_files, WorkspaceIndex};
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    // 1. Discover the source files (files passed directly, or recurse a directory).
    let roots: Vec<PathBuf> = if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths.iter().map(PathBuf::from).collect()
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for root in &roots {
        if root.is_dir() {
            files.extend(discover_as_files(root));
        } else if root.extension().and_then(|e| e.to_str()) == Some("as") {
            files.push(root.clone());
        } else {
            eprintln!("warning: skipping non-.as path '{}'", root.display());
        }
    }
    files.sort();
    files.dedup();
    if files.is_empty() {
        eprintln!("error: no .as files found");
        return ExitCode::from(1);
    }

    // 2. Read sources + build a workspace index for the exported-name sets.
    let mut sources: Vec<(PathBuf, String)> = Vec::new();
    for path in &files {
        match std::fs::read_to_string(path) {
            Ok(text) => sources.push((path.clone(), text)),
            Err(e) => {
                eprintln!("{}: {}", path.display(), e);
                return ExitCode::from(1);
            }
        }
    }
    let index = WorkspaceIndex::build_from_files(&sources);

    // The index canonicalizes paths lexically; resolve each source path the same
    // way to look up its exports.
    let exports_for = |path: &Path| -> HashSet<String> {
        let canon = lexical_canon(path);
        index
            .files
            .get(&canon)
            .map(|fi| fi.exports.keys().cloned().collect())
            .unwrap_or_default()
    };

    // 3. Build the doc model per file. Each module's display NAME is its path
    // RELATIVE to the common root of the whole input set (finding 1) — so two
    // same-stem files in different directories (`a/util.as`, `b/util.as`) become
    // `a/util` / `b/util` and get distinct output files + distinct index links
    // (the slug helper, shared by both md + html).
    let common_root = common_root(&files);
    let mut modules: Vec<doc::DocModule> = Vec::new();
    for (path, text) in &sources {
        let exports = exports_for(path);
        let name = doc::relative_module_name(path, &common_root);
        modules.push(doc::extract_module(path, &name, text, &exports, private));
    }
    // Drop empty modules (no documentable items) so the index stays focused, unless
    // a module carries a `//!` module doc.
    modules.retain(|m| !m.items.is_empty() || m.module_doc.is_some());
    modules.sort_by(|a, b| a.name.cmp(&b.name));

    // 4. `--check`: report undocumented public symbols, exit non-zero on any.
    if check {
        let mut missing = 0usize;
        for m in &modules {
            for sym in doc::undocumented_public(m) {
                println!("{}: undocumented public {}", m.path.display(), sym);
                missing += 1;
            }
        }
        if missing > 0 {
            eprintln!("error: {missing} undocumented public symbol(s)");
            return ExitCode::from(1);
        }
        println!("all public symbols are documented");
        return ExitCode::SUCCESS;
    }

    // 5. Emit.
    match format.as_str() {
        "md" => {
            let out_dir = out.as_deref().map(PathBuf::from);
            if let Some(dir) = &out_dir {
                if let Err(e) = std::fs::create_dir_all(dir) {
                    eprintln!("error: {}: {e}", dir.display());
                    return ExitCode::from(1);
                }
                for m in &modules {
                    let md = doc::markdown::render_module(m);
                    // Key the output file by the SHARED slug (same SoT as html +
                    // the index link), so same-stem modules never collide.
                    let file = dir.join(format!("{}.md", m.slug));
                    if let Err(e) = std::fs::write(&file, md) {
                        eprintln!("error: {}: {e}", file.display());
                        return ExitCode::from(1);
                    }
                }
                println!("wrote {} module(s) to {}", modules.len(), dir.display());
            } else {
                // No --out: stream Markdown to stdout (concatenated).
                for m in &modules {
                    print!("{}", doc::markdown::render_module(m));
                    println!();
                }
            }
            ExitCode::SUCCESS
        }
        "html" => {
            // DEFERRED (review finding 3, owner-noted): SYMBOL cross-linking — a use
            // of a documented symbol linking to its def page across modules (via the
            // workspace index's `ResolvedTarget` cross-file targets, plan Task 3) — is
            // NOT yet implemented. Today only index↔module navigation links exist. This
            // is a documented DX follow-up, not a silent drop (CLAUDE.md no-silent-
            // deferral rule); fully resolving every symbol reference to its page is the
            // larger remaining piece.
            let dir = out
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("target/doc"));
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!("error: {}: {e}", dir.display());
                return ExitCode::from(1);
            }
            // The shared stylesheet.
            if let Err(e) = std::fs::write(dir.join("style.css"), doc::html::STYLE_CSS) {
                eprintln!("error: writing style.css: {e}");
                return ExitCode::from(1);
            }
            // The index.
            if let Err(e) = std::fs::write(dir.join("index.html"), doc::html::render_index(&modules))
            {
                eprintln!("error: writing index.html: {e}");
                return ExitCode::from(1);
            }
            // Per-module pages.
            for m in &modules {
                let file = dir.join(doc::html::module_filename(m));
                if let Err(e) = std::fs::write(&file, doc::html::render_module(m)) {
                    eprintln!("error: {}: {e}", file.display());
                    return ExitCode::from(1);
                }
            }
            let index_path = dir.join("index.html");
            println!(
                "wrote {} module(s) to {}",
                modules.len(),
                dir.display()
            );
            if open {
                open_in_browser(&index_path);
            }
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("error: unknown --format '{other}' (expected 'html' or 'md')");
            ExitCode::from(2)
        }
    }
}

/// Lexically canonicalize a path (resolve `.`/`..`) WITHOUT touching the
/// filesystem, mirroring the workspace index's keying so `Doc` can look a source
/// file up by its indexed key.
#[cfg(feature = "doc")]
fn lexical_canon(path: &std::path::Path) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for comp in path.components() {
        use std::path::Component::*;
        match comp {
            CurDir => {}
            ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The longest common DIRECTORY prefix of the input files — used to derive each
/// module's root-relative display name (finding 1). With a single file, the root
/// is its parent directory (so the name is the bare stem). Empty input → the
/// current dir.
#[cfg(feature = "doc")]
fn common_root(files: &[std::path::PathBuf]) -> std::path::PathBuf {
    use std::path::{Path, PathBuf};
    // Each file's parent directory; the common root is the shared prefix of those.
    let dirs: Vec<&Path> = files.iter().filter_map(|f| f.parent()).collect();
    let Some((first, rest)) = dirs.split_first() else {
        return PathBuf::from(".");
    };
    let mut prefix: Vec<_> = first.components().collect();
    for dir in rest {
        let comps: Vec<_> = dir.components().collect();
        let keep = prefix
            .iter()
            .zip(comps.iter())
            .take_while(|(a, b)| a == b)
            .count();
        prefix.truncate(keep);
    }
    prefix.iter().collect()
}

/// Best-effort open of the generated index in the default browser (`sys`-gated;
/// a no-op with a hint otherwise).
#[cfg(all(feature = "doc", feature = "sys"))]
fn open_in_browser(path: &std::path::Path) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener).arg(path).spawn();
}

/// `--open` fallback when `sys` is not compiled in.
#[cfg(all(feature = "doc", not(feature = "sys")))]
fn open_in_browser(path: &std::path::Path) {
    eprintln!(
        "note: --open requires the 'sys' feature; the docs are at {}",
        path.display()
    );
}

/// Map a package-command `Result<(), String>` to an exit code (clear error to
/// stderr on failure).
#[cfg(feature = "pkg")]
fn pkg_command_exit(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
