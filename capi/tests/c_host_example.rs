//! EMBED Unit E (§12, Gate 9) — the c-host EXAMPLE, CI-executed.
//!
//! Compiles `examples/embed/c-host/main.c` (the SAME source the Makefile builds for
//! humans) with the system compiler (`cc::Build`), links it against the freshly built
//! `ascript-capi` cdylib, runs it, and asserts exit 0 + the `EMBED-C-HOST-OK` sentinel.
//! The Makefile is documentation; THIS test is the CI truth — both share one `main.c`.
//!
//! `#[cfg(unix)]` per the §8.3 owner-noted Windows deferral (the cdylib builds on
//! Windows; the linked C test waits for a Windows runner).

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// The cdylib file name for this platform.
fn cdylib_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "libascript_capi.dylib"
    } else {
        "libascript_capi.so"
    }
}

/// Locate the built cdylib, building it first if necessary. `cargo test` builds the test
/// binaries + the rlib but NOT necessarily the `cdylib` crate-type, so we ensure it via an
/// explicit `cargo build` on this crate's manifest, then probe the standard target dirs.
fn locate_cdylib() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let name = cdylib_name();

    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(t) = std::env::var("CARGO_TARGET_DIR") {
        roots.push(PathBuf::from(t));
    }
    roots.push(manifest_dir.join("target"));

    let profiles = ["debug", "release"];
    let probe = |roots: &[PathBuf]| -> Option<PathBuf> {
        for r in roots {
            for p in profiles {
                let cand = r.join(p).join(name);
                if cand.exists() {
                    return Some(cand);
                }
            }
        }
        None
    };

    if let Some(found) = probe(&roots) {
        return found;
    }

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = Command::new(cargo)
        .arg("build")
        .arg("--manifest-path")
        .arg(manifest_dir.join("Cargo.toml"))
        .status()
        .expect("failed to invoke `cargo build` for the cdylib");
    assert!(status.success(), "`cargo build` of the cdylib failed");

    probe(&roots).unwrap_or_else(|| {
        panic!(
            "could not locate the built {name} cdylib after `cargo build`. \
             Searched: {roots:?} under profiles {profiles:?}. \
             Ensure the crate's [lib] crate-type includes \"cdylib\"."
        )
    })
}

#[test]
fn c_host_example_compiles_links_and_runs() {
    let cdylib = locate_cdylib();
    let lib_dir = cdylib.parent().expect("cdylib has a parent dir").to_path_buf();
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let include_dir = manifest_dir.join("include");
    // The repo root is the capi crate's parent; the c-host example lives under it.
    let repo_root = manifest_dir.parent().expect("capi has a parent (repo root)");
    let main_c = repo_root
        .join("examples")
        .join("embed")
        .join("c-host")
        .join("main.c");
    assert!(main_c.exists(), "c-host main.c missing: {main_c:?}");

    let out_dir = tempfile::tempdir().expect("tempdir");

    ensure_cc_env();

    // Compile main.c → an object file (cc::Build, the system compiler).
    let obj = out_dir.path().join("c_host.o");
    let compiler = cc::Build::new().include(&include_dir).get_compiler();
    let mut cc_cmd = compiler.to_command();
    cc_cmd
        .arg("-c")
        .arg(&main_c)
        .arg("-I")
        .arg(&include_dir)
        .arg("-o")
        .arg(&obj);
    let status = cc_cmd.status().expect("invoke C compiler");
    assert!(status.success(), "compiling c-host main.c failed: {cc_cmd:?}");

    // Link the object against the cdylib → an executable.
    let bin = out_dir.path().join("c_host_bin");
    let mut link = compiler.to_command();
    link.arg(&obj)
        .arg("-L")
        .arg(&lib_dir)
        .arg("-lascript_capi")
        .arg(format!("-Wl,-rpath,{}", lib_dir.display()))
        .arg("-o")
        .arg(&bin);
    let status = link.status().expect("invoke linker");
    assert!(status.success(), "linking c-host binary failed: {link:?}");

    // Run it.
    let mut run = Command::new(&bin);
    set_loader_path(&mut run, &lib_dir);
    let output = run.output().expect("run c-host binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "c-host binary exited non-zero: {:?}\nstdout: {stdout}\nstderr: {stderr}",
        output.status
    );
    assert_eq!(stdout.trim(), "EMBED-C-HOST-OK", "stderr: {stderr}");
}

/// Provide the build-script env vars `cc::Build` expects (it normally runs inside a
/// build script). Defaults are conservative: no optimization, host == target.
fn ensure_cc_env() {
    fn set_if_absent(key: &str, val: &str) {
        if std::env::var_os(key).is_none() {
            // SAFETY: set in single-threaded test setup before spawning anything.
            std::env::set_var(key, val);
        }
    }
    set_if_absent("OPT_LEVEL", "0");
    let target = env!("CAPI_TARGET");
    set_if_absent("TARGET", target);
    set_if_absent("HOST", target);
}

/// Set the platform's dynamic-loader search-path env var to include `lib_dir`.
fn set_loader_path(cmd: &mut Command, lib_dir: &Path) {
    let var = if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
    let existing = std::env::var(var).unwrap_or_default();
    let joined = if existing.is_empty() {
        lib_dir.display().to_string()
    } else {
        format!("{}:{}", lib_dir.display(), existing)
    };
    cmd.env(var, joined);
}
