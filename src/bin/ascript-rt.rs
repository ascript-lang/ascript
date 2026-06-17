//! RT §2.4 — `ascript-rt`, the runtime-only bin.
//!
//! It carries the VM + GC + Interp kernel + stdlib + workers + caps + `.aso`/archive
//! loaders+verifiers + panic diagnostics + the embedded-payload shim, with the entire
//! FRONT-END (parsers, compiler, checker, LSP/DAP/fmt/REPL/pkg, tree-sitter) compiled
//! OUT under the build-time `cfg(ascript_rt)` (set by `ASCRIPT_RT=1`, see `build.rs`).
//!
//! This file ALWAYS compiles (so `cargo build --bins` keeps working); it is only
//! size-optimal under the cfg. There is **no clap** — the runtime parses its own
//! three-case argv (spec §2.4):
//!   1. **Bundled** (a trailing `ASCRIPTB` footer): run the embedded payload, forward
//!      argv — identical semantics to a bundled `ascript`.
//!   2. **`--rt-info`**: one JSON line describing the stub (introspection hook).
//!   3. **A single path arg**: run it as a verified `.aso`/`ASCRIPTA` artifact
//!      (`run_aso_file` semantics) — the container/dev convenience + test seam.
//!
//! Anything else → a two-line usage error.

use std::process::ExitCode;

fn main() -> ExitCode {
    // RT §2.4 / SP3 §B — the SAME worker-thread + current-thread-runtime bootstrap the
    // toolchain `main` uses, so the recursion-depth guard sits under native capacity
    // with the enlarged `WORKER_STACK_SIZE` stack.
    let worker = std::thread::Builder::new()
        .name("ascript-rt-main".to_string())
        .stack_size(ascript::interp::WORKER_STACK_SIZE)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            rt.block_on(rt_main())
        })
        .expect("failed to spawn worker thread");
    worker.join().expect("worker thread panicked")
}

async fn rt_main() -> ExitCode {
    // (1) Bundled binary: run the embedded program BEFORE any argv handling — the
    // shared shim (`ascript::run_embedded_if_bundled`) is the ONE implementation both
    // bins call. `./app a b --c` forwards `[a, b, --c]` to the program.
    if let Some(code) = ascript::run_embedded_if_bundled().await {
        return ExitCode::from(code as u8);
    }

    // Not a bundle → the bare-stub argv contract (no clap).
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        // (2) `--rt-info` — one JSON line of stub introspection (§5.4 rung-3 input).
        [flag] if flag == "--rt-info" => {
            println!("{}", rt_info_json());
            ExitCode::SUCCESS
        }
        // (3) A single path argument → run it as a verified `.aso`/`ASCRIPTA` artifact.
        // Caps default to all-granted (no CLI surface), still honoring `ASCRIPT_DENY`
        // at launch (the same as a bundled launch — applied inside `run_verified_*`).
        [path] if !path.starts_with("--") => {
            let p = std::path::Path::new(path);
            match ascript::run_aso_file(p, &[], None).await {
                Ok(code) => ExitCode::from(code as u8),
                Err(e) => {
                    ascript::diagnostics::report(&e);
                    ExitCode::from(1)
                }
            }
        }
        // Anything else → a two-line usage error.
        _ => {
            eprintln!("ascript-rt: the runtime-only stub (no toolchain; bundles embed their program).");
            eprintln!("usage: ascript-rt <program.aso>   |   ascript-rt --rt-info");
            ExitCode::from(2)
        }
    }
}

/// RT §2.4 — the `--rt-info` JSON line. Hand-rolled (serde-free) so it builds under
/// `rt-core` (no `data`/`serde_json` feature). Values are the REAL constants/cfg facts:
/// the tier is stamped at build time, the feature list is `cfg!(feature = …)`-derived,
/// and the version fields are the real `ASO_FORMAT_VERSION`/`ARCHIVE_VERSION` consts.
fn rt_info_json() -> String {
    // The runtime feature set, derived from the actual compiled cfgs (the toolchain-only
    // features are never in a tier, so they're absent from this list by construction).
    // `mut` is conditional on at least one runtime feature being enabled (each
    // `push_if!` is a `#[cfg(feature)]` push); with zero features the vec stays empty.
    #[allow(unused_mut)]
    let mut feats: Vec<&str> = Vec::new();
    macro_rules! push_if {
        ($feat:literal) => {
            #[cfg(feature = $feat)]
            feats.push($feat);
        };
    }
    push_if!("shared");
    push_if!("bundle-zstd");
    push_if!("data");
    push_if!("binary");
    push_if!("log");
    push_if!("workflow");
    push_if!("datetime");
    push_if!("crypto");
    push_if!("compress");
    push_if!("sys");
    push_if!("sysinfo");
    push_if!("sql");
    push_if!("tui");
    push_if!("net");
    push_if!("postgres");
    push_if!("redis");
    push_if!("telemetry");
    push_if!("intl");
    push_if!("ai");
    push_if!("ffi");

    fn esc(s: &str) -> String {
        s.replace('\\', "\\\\").replace('"', "\\\"")
    }
    let features = feats
        .iter()
        .map(|f| format!("\"{}\"", esc(f)))
        .collect::<Vec<_>>()
        .join(",");

    format!(
        "{{\"name\":\"ascript-rt\",\"version\":\"{ver}\",\"target\":\"{target}\",\
\"tier\":\"{tier}\",\"features\":[{features}],\
\"aso_format_version\":{aso},\"archive_version\":{arch}}}",
        ver = esc(env!("CARGO_PKG_VERSION")),
        target = esc(option_env!("TARGET").unwrap_or("unknown")),
        tier = esc(env!("ASCRIPT_RT_TIER")),
        features = features,
        aso = ascript::vm::aso::ASO_FORMAT_VERSION,
        arch = ascript::vm::archive::ARCHIVE_VERSION,
    )
}
