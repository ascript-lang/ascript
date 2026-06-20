//! Forward the build target triple to the C smoke test (`tests/c_smoke.rs`) as a compiled
//! env var. `cc::Build` (used at test time to compile `smoke.c`) expects the `TARGET`
//! build-script env var, which is absent during a plain `cargo test`; the smoke test reads
//! `CAPI_TARGET` to repopulate it.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=CAPI_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
