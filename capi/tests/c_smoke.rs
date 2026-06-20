//! EMBED §8.3 — the compiled C smoke test.
//!
//! Compiles `tests/smoke.c` with the system compiler (`cc::Build`) at test time, links it
//! against the freshly built `ascript-capi` cdylib, runs it, and asserts exit 0 + the
//! expected stdout. `#[cfg(unix)]` per the §8.3 owner-noted Windows deferral (the cdylib
//! builds on Windows; the linked smoke test waits for a Windows runner).

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

    // Candidate target dirs: the env-provided one (set under `cargo test`), then the
    // conventional `<manifest>/target/{debug,release}`. Each crate-type lands in the
    // profile root.
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

    // Not built yet — build the cdylib explicitly, then re-probe.
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
fn c_smoke_compiles_links_and_runs() {
    let cdylib = locate_cdylib();
    let lib_dir = cdylib.parent().expect("cdylib has a parent dir").to_path_buf();
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let include_dir = manifest_dir.join("include");
    let smoke_c = manifest_dir.join("tests").join("smoke.c");

    // A scratch dir for the compiled object + the linked binary.
    let out_dir = tempfile::tempdir().expect("tempdir");

    // `cc::Build::get_compiler()` reads build-script env vars (OPT_LEVEL/HOST/TARGET) that
    // are NOT set during a `cargo test` run — provide sane defaults so it resolves the
    // system compiler. SAFETY: single-threaded test setup, before any cc call.
    ensure_cc_env();

    // Compile smoke.c → an object file (cc::Build, the system compiler).
    let obj = out_dir.path().join("smoke.o");
    let compiler = cc::Build::new().include(&include_dir).get_compiler();
    let mut cc_cmd = compiler.to_command();
    cc_cmd
        .arg("-c")
        .arg(&smoke_c)
        .arg("-I")
        .arg(&include_dir)
        .arg("-o")
        .arg(&obj);
    let status = cc_cmd.status().expect("invoke C compiler");
    assert!(status.success(), "compiling smoke.c failed: {cc_cmd:?}");

    // Link the object against the cdylib → an executable.
    let bin = out_dir.path().join("smoke_bin");
    let mut link = compiler.to_command();
    link.arg(&obj)
        .arg("-L")
        .arg(&lib_dir)
        .arg("-lascript_capi")
        // Embed an rpath so the binary finds the cdylib at run time without env vars.
        .arg(format!("-Wl,-rpath,{}", lib_dir.display()))
        .arg("-o")
        .arg(&bin);
    let status = link.status().expect("invoke linker");
    assert!(status.success(), "linking smoke binary failed: {link:?}");

    // Run it (belt-and-suspenders: also set the dynamic-loader path env var).
    let mut run = Command::new(&bin);
    set_loader_path(&mut run, &lib_dir);
    let output = run.output().expect("run smoke binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "smoke binary exited non-zero: {:?}\nstdout: {stdout}\nstderr: {stderr}",
        output.status
    );
    assert_eq!(stdout.trim(), "OK", "stderr: {stderr}");
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
