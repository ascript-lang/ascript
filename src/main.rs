// RT §2.2 — under the runtime-only build cfg, the toolchain `ascript` bin (clap CLI,
// the checker/fmt/doc/pkg/repl/lsp/dap subcommands) is compiled OUT. This file's whole
// body is gated `cfg(not(ascript_rt))`; the cfg build gets a tiny loud-error stub `main`
// so `cargo build --bins` under the cfg still fails LOUDLY rather than confusingly. The
// runtime entry point is the separate `ascript-rt` bin (`src/bin/ascript-rt.rs`).
#[cfg(ascript_rt)]
fn main() -> std::process::ExitCode {
    eprintln!(
        "error: this is a toolchain build misconfiguration — the `ascript` bin cannot be \
         built with ASCRIPT_RT=1 (the front-end is compiled out under that cfg). \
         ascript-rt is the runtime bin; build it with `scripts/build-rt.sh <tier>`."
    );
    std::process::ExitCode::from(2)
}

#[cfg(not(ascript_rt))]
use std::process::ExitCode;

#[cfg(all(not(ascript_rt), feature = "pkg"))]
mod pkg;

#[cfg(not(ascript_rt))]
use ascript::cli_surface::{CacheAction, CapFlags, Cli, Command};
#[cfg(not(ascript_rt))]
use clap::Parser;

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
#[cfg(not(ascript_rt))]
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
#[cfg(all(not(ascript_rt), feature = "profile"))]
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

#[cfg(not(ascript_rt))]
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
#[cfg(not(ascript_rt))]
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
#[cfg(not(ascript_rt))]
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

/// BIN §2.3 — the pre-clap startup shim. If THIS executable is a native bundle (a trailing
/// `ASCRIPTB` footer over a valid payload region), read the payload and run it through the
/// embedded path, returning its exit code. A plain `ascript` launch (no footer) returns
/// `None` and the caller falls through to `Cli::parse()`, byte-identical to before.
///
/// Cost on the NON-bundle path (every normal launch): a `current_exe()` resolve + open +
/// stat + a single `FOOTER_SIZE`-byte tail read — it never loads the whole image (Task 7).
/// Any I/O failure BEFORE footer confirmation (open / stat / footer read / `validate_footer`)
/// is treated as "not a bundle" — it may be a plain `ascript` launch, so it falls through to
/// `Cli::parse()`. Once the `ASCRIPTB` magic is confirmed, a payload-read failure is a
/// REPORTED error (exit 1), NOT a silent fall-through — the binary IS a bundle, so a confusing
/// clap "missing subcommand" error would be wrong.
#[cfg(not(ascript_rt))]
async fn try_run_embedded() -> Option<ExitCode> {
    // RT §2.4: the shim body is shared with the `ascript-rt` bin — ONE implementation
    // in `ascript::run_embedded_if_bundled`. Map its `Option<i32>` to an `ExitCode`.
    ascript::run_embedded_if_bundled()
        .await
        .map(|code| ExitCode::from(code as u8))
}

#[cfg(not(ascript_rt))]
async fn real_main() -> ExitCode {
    // BIN §2.3: a bundled binary runs its embedded program BEFORE clap ever sees argv — so
    // `./app a b --c` forwards `[a, b, --c]` to the program, never as ascript subcommands.
    if let Some(code) = try_run_embedded().await {
        return code;
    }
    let cli = Cli::parse();
    match cli.command {
        Command::Run {
            tree_walker,
            elide: elide_flag,
            no_elide,
            locked,
            caps: CapFlags { deny, sandbox, deny_net, deny_fs },
            inspect,
            profile,
            out,
            profile_hz,
            profile_format,
            no_cache,
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
            // DX D4 §5.1 multi-error reporting: a `.as` source file with several
            // PARSE errors renders them ALL at once (the error-tolerant CST parser
            // records every one) instead of the compiler bailing on the first. This
            // runs only for `.as` files (a `.aso` is already-compiled bytecode); a
            // clean parse falls through to the normal run, and a later FATAL runtime
            // panic still goes through the single-report path below. The pre-check
            // shares no state with the runner, so the program runs identically when
            // the parse is clean.
            if !is_aso {
                let parse_errors = ascript::collect_parse_errors(path);
                if !parse_errors.is_empty() {
                    ascript::diagnostics::report_all(&parse_errors);
                    return ExitCode::from(1);
                }
                // Shared BLOCKING semantic gate (both engines): reject statically-
                // invalid programs the same way before EITHER engine runs — currently
                // an or-pattern whose alternatives bind different name sets. Runs only
                // on a clean parse; shares no state with the runner, so a valid program
                // runs identically.
                let blocking = ascript::collect_blocking_diagnostics(path);
                if !blocking.is_empty() {
                    ascript::diagnostics::report_all(&blocking);
                    return ExitCode::from(1);
                }
            }
            // ELIDE §5.2: resolve the elision decision (`--elide`/`ASCRIPT_ELIDE`
            // opt-in vs `--no-elide`/`ASCRIPT_NO_ELIDE` force-off over the measured
            // default-OFF). `.aso` runs skip this — a compiled module already baked in
            // whatever the `build` step elided (the opcode is durable).
            let elide = ascript::elide_enabled(elide_flag, no_elide);
            // WARM A: the plain `.as`-on-the-VM path routes through the cached front
            // door (fail-open; a cache error falls back to the uncached compile-and-run).
            // `--no-cache` OR `ASCRIPT_NO_COMPILE_CACHE=1` bypasses it. `.aso` (already
            // compiled) and `--tree-walker` (the oracle path) are NEVER cached. The
            // `--inspect`/`--profile` paths returned earlier above, so they never reach
            // here — also uncached, as the spec requires (§2.8).
            //
            // The cache artifact is always the unshaken, debug-carrying, non-elided
            // archive (§2.6). `--elide` therefore bypasses the cache too: an elided run
            // would compile DIFFERENT bytes, but `elide` is a v1-constant cache flag, so
            // routing an elided run through the cache would either stale-hit a non-elided
            // artifact or never hit. Cleanest: an explicit-elide run is uncached.
            let no_compile_cache = no_cache
                || std::env::var("ASCRIPT_NO_COMPILE_CACHE").as_deref() == Ok("1")
                || elide;
            let result = if is_aso {
                ascript::run_aso_file(path, &args, caps).await
            } else if use_tree_walker {
                ascript::run_file_with_packages(path, &args, packages, caps, elide).await
            } else if no_compile_cache {
                ascript::run_file_on_vm_with_packages(path, &args, packages, caps, elide).await
            } else {
                ascript::run_file_on_vm_cached(path, &args, packages, caps, false).await
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
        Command::Build {
            file,
            out,
            elide: elide_flag,
            no_elide,
            strip,
            native,
            target,
            stub,
            no_fetch,
            exact,
            compress,
            tier,
            report_json,
            oci,
            oci_tag,
            pgo,
            caps: CapFlags { deny, sandbox, deny_net, deny_fs },
        } => {
            let out_path = out.as_deref().map(std::path::Path::new);
            let src = std::path::Path::new(&file);
            // ELIDE §4.2/§5: `build --elide` (or `ASCRIPT_ELIDE=1`) bakes the proven
            // elisions into the artifact — the `CallElided` opcode is durable, so the
            // win rides every later `run` of the `.aso`/native bundle. Default-OFF
            // (same as `run`); `--no-elide` is the explicit force-off.
            let elide = ascript::elide_enabled(elide_flag, no_elide);
            // SELF-CONTAINED-BUNDLES (Task 3.1): compose the initial CapSet from the
            // CLI flags + the nearest `ascript.toml` `[capabilities]` table (the SAME
            // `compose_caps` the `run`/`test` commands use — single source of truth).
            // `None` → all granted (the byte-identical default); `Some` → restricted.
            // The composed set is EMBEDDED into the produced archive manifest (Task 3.2
            // enforces it at runtime).
            let caps = match compose_caps(src, &deny, sandbox, deny_net.as_deref(), deny_fs.as_deref()) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(1);
                }
            };
            // RT §8: `--oci` implies `--native` (an OCI image contains a native bundle).
            if native || oci {
                // RT §4.4: parse the optional `--tier` override into a `Tier` (a clear
                // error for an unknown name, never a silent ignore).
                let parsed_tier = match tier.as_deref() {
                    None => None,
                    Some(name) => match ascript::rtstub::tiers::Tier::parse(name) {
                        Some(t) => Some(t),
                        None => {
                            eprintln!(
                                "error: unknown --tier '{name}' (expected one of: \
                                 rt-core, rt-local, rt-net, rt-full)"
                            );
                            return ExitCode::from(1);
                        }
                    },
                };
                // BIN/RT: bundle a self-contained native executable, resolving the stub
                // via the §5.4 ladder (--stub → cache → fetch → sibling → current_exe).
                let opts = ascript::NativeBuildOpts {
                    target: target.clone(),
                    tier: parsed_tier,
                    compress,
                    report_json: report_json.clone(),
                    stub: stub.as_deref().map(std::path::PathBuf::from),
                    no_fetch,
                    strip,
                    exact,
                    oci,
                    oci_tag,
                };
                match ascript::build_native(src, out_path, caps, elide, &opts).await {
                    Ok(_) => ExitCode::SUCCESS, // build_native prints `bundled … -> …`
                    Err(e) => {
                        ascript::diagnostics::report(&e);
                        ExitCode::from(1)
                    }
                }
            } else if pgo {
                // WARM B §3.1: PGO harvest — run the program as a training workload,
                // harvest warmed ICs + adaptive caches, embed a PGO section.
                match ascript::build_file_with_pgo(src, out_path, !strip, caps, elide).await {
                    Ok(written) => {
                        println!("compiled {} -> {} (with PGO section)", file, written.display());
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        ascript::diagnostics::report(&e);
                        ExitCode::from(1)
                    }
                }
            } else {
                match ascript::build_file(src, out_path, !strip, caps, elide) {
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
            elide: elide_flag,
            no_elide,
            locked,
            deny,
            sandbox,
            parallel,
            update_snapshots,
            filter,
            watch,
            coverage,
        } => {
            // ELIDE §4.6/§5: resolve the elision decision for the (serial) test path.
            // The PARALLEL path runs each file in a worker isolate, which never elides.
            let elide = ascript::elide_enabled(elide_flag, no_elide);
            // DX D2 Task 10: validate the `--filter` ONCE up front so a malformed regex is a
            // clean error before any test runs (the raw string is re-parsed downstream — in
            // each isolate / the serial path — but it is already known-good here).
            if let Some(raw) = &filter {
                if let Err(e) = ascript::test_filter::TestFilter::parse(raw) {
                    eprintln!("error: {e}");
                    return ExitCode::from(1);
                }
            }
            // DX D2 Task 6: validate the `--coverage[=fmt]` value up front.
            let coverage_fmt = match &coverage {
                Some(raw) => {
                    match ascript::vm::coverage_report::CoverageFormat::parse(raw) {
                        Some(fmt) => Some(fmt),
                        None => {
                            eprintln!(
                                "error: unknown coverage format '{raw}' \
                                 (expected text, lcov, or html)"
                            );
                            return ExitCode::from(1);
                        }
                    }
                }
                None => None,
            };
            let filter_raw: Option<&str> = filter.as_deref();
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
            #[cfg(not(feature = "pkg"))]
            let packages = {
                let _ = locked;
                None
            };

            // DX D2 Task 10: `--watch` re-runs the affected tests on file change (sys-gated,
            // import-graph-scoped). The loop never terminates, so it owns its own printing +
            // exit. `--no-default-features` (no `sys`) reports a clean rebuild hint.
            if watch {
                #[cfg(feature = "sys")]
                {
                    match ascript::watch::run_watch(
                        &files, packages, caps, parallel, filter_raw,
                    )
                    .await
                    {
                        Ok(()) => return ExitCode::SUCCESS,
                        Err(e) => {
                            ascript::diagnostics::report(&e);
                            return ExitCode::from(1);
                        }
                    }
                }
                #[cfg(not(feature = "sys"))]
                {
                    let _ = (&packages, &caps);
                    eprintln!(
                        "error: `--watch` requires the 'sys' feature (file watching); \
                         rebuild with default features"
                    );
                    return ExitCode::from(1);
                }
            }

            // DX D2 Task 6: a `--coverage` run uses the VM-based coverage runner (the
            // normal `ascript test` path runs on the tree-walker, which is NOT instrumented
            // — coverage is a VM-only feature, a documented asymmetry). It records line
            // coverage on the `Vm.instrument` seam, prints the same FAIL/tally lines, then
            // emits the coverage report in the requested format. Program output is
            // byte-identical to a non-coverage run (the trap re-dispatches the same op).
            if let Some(fmt) = coverage_fmt {
                let cov_result =
                    ascript::run_tests_with_coverage(&files, packages, caps, filter_raw, fmt)
                        .await;
                return match cov_result {
                    Ok((summary, report)) => {
                        for (name, message) in &summary.failures {
                            println!("FAIL {}: {}", name, message);
                        }
                        summary.print_tally();
                        // The report goes to stdout (text/lcov) or is written to
                        // target/coverage/ (html) by the runner — `report` is the
                        // stdout-bound text (a path hint for html).
                        print!("{report}");
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
                };
            }

            let test_result = ascript::run_tests_with_options(
                &files,
                packages,
                caps,
                parallel,
                update_snapshots,
                filter_raw,
                elide,
            )
            .await;
            match test_result {
            Ok(summary) => {
                for (name, message) in &summary.failures {
                    println!("FAIL {}: {}", name, message);
                }
                summary.print_tally();
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
        Command::Init { template, force, dir } => run_init(&template, force, &dir),
        Command::Cache { action } => match action {
            CacheAction::Clean => {
                let compiled = ascript::cache::compile_cache::compiled_dir();
                if compiled.exists() {
                    let count = std::fs::read_dir(&compiled)
                        .map(|rd| rd.count())
                        .unwrap_or(0);
                    std::fs::remove_dir_all(&compiled)
                        .unwrap_or_else(|e| eprintln!("warning: {e}"));
                    println!("removed {count} cached compilation(s)");
                } else {
                    println!("compiled cache is already empty");
                }
                ExitCode::SUCCESS
            }
            CacheAction::Dir => {
                println!("{}", ascript::cache::compile_cache::cache_root().display());
                ExitCode::SUCCESS
            }
        },
        #[cfg(feature = "rt-release")]
        Command::RtManifestGen { genkey, version, created, entries_file, key, out_dir } => {
            run_rt_manifest_gen(genkey, version, created, entries_file, key, out_dir)
        }
    }
}

/// RT §5.1 / Task 11 — generate + sign the release stub manifest (hidden subcommand,
/// `rt-release`-gated). Two modes:
///
/// - `--genkey`: mint a fresh keypair, print `(private seed hex, public key hex)`, exit.
/// - default: read the entries file, build the CANONICAL manifest, sign it with the
///   key-file seed, and write `rt-manifest.json` + `rt-manifest.json.sig` into `--out-dir`.
///
/// The PRIVATE key is read from a file path (the CI-secret path) — never echoed.
#[cfg(feature = "rt-release")]
fn run_rt_manifest_gen(
    genkey: bool,
    version: Option<String>,
    created: Option<String>,
    entries_file: Option<String>,
    key: Option<String>,
    out_dir: Option<String>,
) -> ExitCode {
    use ascript::rtstub::manifest;

    if genkey {
        // Mint a fresh keypair for the maintainer (out-of-band key rotation).
        let (seed_hex, pub_hex) = manifest::generate_keypair();
        // The Rust array literal for the PRODUCTION_PUBKEY const, for convenience.
        let arr: Vec<String> = (0..32)
            .map(|i| format!("0x{}", &pub_hex[i * 2..i * 2 + 2]))
            .collect();
        println!("private_seed_hex={seed_hex}");
        println!("public_key_hex={pub_hex}");
        println!("public_key_rust=[{}]", arr.join(", "));
        eprintln!(
            "note: store the private seed in the CI secret ASCRIPT_RT_SIGNING_KEY and \
             compile the public key into PRODUCTION_PUBKEY (a toolchain release)."
        );
        return ExitCode::SUCCESS;
    }

    let version = version.unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let created = created.unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

    // Read + parse the entries file.
    let entries_path = match entries_file {
        Some(p) => p,
        None => {
            eprintln!("error: --entries-file is required (or use --genkey)");
            return ExitCode::from(2);
        }
    };
    let entries_bytes = match std::fs::read(&entries_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: cannot read entries file '{entries_path}': {e}");
            return ExitCode::from(1);
        }
    };
    let entries = match manifest::parse_entries(&entries_bytes) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };

    // Read the private signing seed from the key-file path (never echoed).
    let key_path = match key {
        Some(p) => p,
        None => {
            eprintln!("error: --key <path-to-seed-file> is required to sign the manifest");
            return ExitCode::from(2);
        }
    };
    let seed_hex = match std::fs::read_to_string(&key_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read signing key file '{key_path}': {e}");
            return ExitCode::from(1);
        }
    };
    let signing_key = match manifest::load_signing_key_hex(&seed_hex) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };

    // Generate the canonical manifest + detached signature.
    let manifest_bytes = manifest::generate_manifest(&version, &created, &entries);
    let sig = manifest::sign_manifest(&manifest_bytes, &signing_key);

    let dir = out_dir.unwrap_or_else(|| ".".to_string());
    let dir = std::path::Path::new(&dir);
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("error: cannot create out-dir '{}': {e}", dir.display());
        return ExitCode::from(1);
    }
    let manifest_out = dir.join("rt-manifest.json");
    let sig_out = dir.join("rt-manifest.json.sig");
    if let Err(e) = std::fs::write(&manifest_out, &manifest_bytes) {
        eprintln!("error: cannot write '{}': {e}", manifest_out.display());
        return ExitCode::from(1);
    }
    if let Err(e) = std::fs::write(&sig_out, sig) {
        eprintln!("error: cannot write '{}': {e}", sig_out.display());
        return ExitCode::from(1);
    }
    eprintln!(
        "wrote {} ({} stub entries) + {}",
        manifest_out.display(),
        entries.len(),
        sig_out.display()
    );
    ExitCode::SUCCESS
}

/// DX D1: `ascript doc` — discover `.as` files, build the doc model (static CST
/// walk, never the interpreter), and emit Markdown / HTML, or `--check` for
/// undocumented public symbols.
#[cfg(all(not(ascript_rt), feature = "doc"))]
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
#[cfg(all(not(ascript_rt), feature = "doc"))]
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
#[cfg(all(not(ascript_rt), feature = "doc"))]
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
#[cfg(all(not(ascript_rt), feature = "doc", feature = "sys"))]
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
#[cfg(all(not(ascript_rt), feature = "doc", not(feature = "sys")))]
fn open_in_browser(path: &std::path::Path) {
    eprintln!(
        "note: --open requires the 'sys' feature; the docs are at {}",
        path.display()
    );
}

/// Map a package-command `Result<(), String>` to an exit code (clear error to
/// stderr on failure).
#[cfg(all(not(ascript_rt), feature = "pkg"))]
fn pkg_command_exit(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

/// The embedded `server` template (CNTR §9.3). Each tuple is
/// `(scaffolded-filename, file-contents)`. Files are `include_str!`-embedded at
/// compile time from `templates/server/` — no network, no runtime file reads.
#[cfg(not(ascript_rt))]
const SERVER_TEMPLATE: &[(&str, &str)] = &[
    ("main.as", include_str!("../templates/server/main.as")),
    ("Dockerfile", include_str!("../templates/server/Dockerfile")),
    (".dockerignore", include_str!("../templates/server/.dockerignore")),
    ("ascript.toml", include_str!("../templates/server/ascript.toml")),
    ("README.md", include_str!("../templates/server/README.md")),
];

/// CNTR §9.3 — `ascript init [--template server] [--force] [dir]`. Scaffolds a
/// container-ready project from an embedded template. Refuses to overwrite existing
/// files (lists the conflicts, exits nonzero) unless `--force`.
#[cfg(not(ascript_rt))]
fn run_init(template: &str, force: bool, dir: &str) -> ExitCode {
    let files = match template {
        "server" => SERVER_TEMPLATE,
        other => {
            eprintln!("error: unknown template '{other}' (available: server)");
            return ExitCode::from(2);
        }
    };

    let target = std::path::Path::new(dir);
    if let Err(e) = std::fs::create_dir_all(target) {
        eprintln!("error: could not create {}: {e}", target.display());
        return ExitCode::from(1);
    }

    // Conflict detection: collect every existing destination before writing anything.
    if !force {
        let conflicts: Vec<&str> = files
            .iter()
            .filter(|(name, _)| target.join(name).exists())
            .map(|(name, _)| *name)
            .collect();
        if !conflicts.is_empty() {
            eprintln!(
                "error: refusing to overwrite existing file(s) in {}:",
                target.display()
            );
            for name in &conflicts {
                eprintln!("  {name}");
            }
            eprintln!("rerun with --force to overwrite");
            return ExitCode::from(1);
        }
    }

    for (name, contents) in files {
        let dest = target.join(name);
        if let Err(e) = std::fs::write(&dest, contents) {
            eprintln!("error: could not write {}: {e}", dest.display());
            return ExitCode::from(1);
        }
    }

    println!("scaffolded the '{template}' template into {}", target.display());
    println!("  next: cd {} && ascript run main.as", target.display());
    ExitCode::SUCCESS
}
