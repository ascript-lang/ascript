//! RT §4.2 — the checked-in module→feature table + feature-dependency closure.
//!
//! **DRIFT-TESTED:** three tests in `tests/rt_select.rs` keep this file byte-identical
//! to the ground truth:
//!
//! 1. **Completeness/bijection** — every entry in `stdlib::STD_MODULES` appears
//!    exactly once in [`STD_MODULE_FEATURES`], and vice versa.
//! 2. **Gate drift** — the `#[cfg(feature = "X")]` + `"std/Y" =>` pairs extracted
//!    from `src/stdlib/mod.rs` at test runtime match this table.
//! 3. **Closure drift** — `Cargo.toml` [features] actual edges match [`FEATURE_DEPS`],
//!    and every feature named in the table exists in the manifest.
//!
//! **Do NOT edit one without the others** — a drift test will catch it immediately.

use std::collections::BTreeSet;

/// (canonical `std/*` specifier, required Cargo feature OR `None` for core modules).
///
/// "Core" means the module is always present (no `#[cfg(feature = …)]` gate on its
/// `std_module_exports` arm). The ordering mirrors `stdlib::STD_MODULES` for
/// readability; the drift tests are order-independent.
pub const STD_MODULE_FEATURES: &[(&str, Option<&str>)] = &[
    // ── Core / unconditional (always built under --no-default-features) ──────
    ("std/ai",          Some("ai")),
    ("std/assert",      None),
    ("std/bench",       None),
    ("std/cli",         None),
    ("std/color",       None),
    ("std/decimal",     None),
    ("std/math",        None),
    ("std/string",      None),
    ("std/array",       None),
    ("std/object",      None),
    ("std/map",         None),
    ("std/schema",      None),
    ("std/shared",      Some("shared")),
    ("std/set",         None),
    ("std/lru",         None),
    ("std/events",      None),
    ("std/template",    None),
    ("std/bytes",       None),
    ("std/caps",        None),
    ("std/convert",     None),
    ("std/task",        None),
    ("std/time",        None),
    ("std/sync",        None),
    ("std/stream",      None),
    // ── Optional feature-gated modules ──────────────────────────────────────
    ("std/date",        Some("datetime")),
    ("std/intl",        Some("intl")),
    ("std/json",        Some("data")),
    ("std/log",         Some("log")),
    ("std/workflow",    Some("workflow")),
    ("std/telemetry",   Some("telemetry")),
    ("std/encoding",    Some("data")),
    ("std/crypto",      Some("crypto")),
    ("std/compress",    Some("compress")),
    ("std/env",         Some("sys")),
    ("std/fs",          Some("sys")),
    ("std/os",          Some("sys")),
    ("std/io",          Some("sys")),
    ("std/process",     Some("sys")),
    ("std/net",         Some("net")),
    ("std/net/tcp",     Some("net")),
    ("std/net/http",    Some("net")),
    ("std/http/server", Some("net")),
    ("std/net/udp",     Some("net")),
    ("std/net/unix",    Some("net")),
    ("std/net/ws",      Some("net")),
    ("std/regex",       Some("data")),
    ("std/sqlite",      Some("sql")),
    ("std/postgres",    Some("postgres")),
    ("std/redis",       Some("redis")),
    ("std/url",         Some("data")),
    ("std/uuid",        Some("data")),
    ("std/csv",         Some("data")),
    ("std/toml",        Some("data")),
    ("std/yaml",        Some("data")),
    ("std/msgpack",     Some("binary")),
    ("std/cbor",        Some("binary")),
    ("std/tui",         Some("tui")),
    ("std/ffi",         Some("ffi")),
    ("std/resilience",  Some("resilience")),
    ("std/docker",      Some("docker")),
];

/// Cargo feature-dependency edges relevant for the runtime feature closure.
/// Each entry is `(feature, depends_on)`: to enable `feature`, `depends_on` is
/// also required.
///
/// Only runtime features are included (toolchain-only features like `lsp`, `dap`,
/// `doc`, `pkg`, `profile`, `fuzzgen`, `decode-census`, `http3` are excluded).
///
/// Source: `Cargo.toml` [features] section (verified 2026-06-12).
/// **DRIFT-TESTED** by `closure_drift` in `tests/rt_select.rs`.
pub const FEATURE_DEPS: &[(&str, &str)] = &[
    // binary = ["dep:rmpv", "dep:ciborium", "data"]
    ("binary", "data"),
    // log = ["data"]
    ("log", "data"),
    // workflow = ["data"]
    ("workflow", "data"),
    // telemetry = ["data", "net"]
    ("telemetry", "data"),
    ("telemetry", "net"),
    // ai = ["data", "net", "dep:genai"]
    ("ai", "data"),
    ("ai", "net"),
    // docker = ["net", "data"]
    ("docker", "net"),
    ("docker", "data"),
    // pkg = ["net", "compress", "dep:base64"]
    // (pkg is toolchain-only but included so required_features closure is complete
    //  if someone ever maps a pkg-requiring import — currently no std module needs it)
    ("pkg", "net"),
    ("pkg", "compress"),
];

/// Collect all `std/` module specifiers imported anywhere in `archive`.
///
/// Decodes each embedded chunk and reads its `imports` side table; specifiers that
/// start with `std/` are collected, others (relative `./foo`, package `pkg/x`)
/// are skipped. Returns a `BTreeSet` for deterministic iteration.
///
/// This is the §4.1 "chunk-level truth" scanner: it sees every import the runtime
/// will execute, across all modules including worker slices (which derive from the
/// same chunks).
pub fn collect_std_imports(
    archive: &crate::vm::archive::ModuleArchive,
) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    for (_key, bytes) in &archive.modules {
        // Decode the chunk — on a well-formed archive this always succeeds
        // (the builder verified them); on a corrupt byte string we skip silently
        // rather than panicking (the runtime verifier is the trust boundary).
        let Ok(chunk) = crate::vm::chunk::Chunk::from_bytes_verified(bytes) else {
            continue;
        };
        for import in &chunk.imports {
            let src = import.source();
            if src.starts_with("std/") {
                result.insert(src.to_owned());
            }
        }
    }
    result
}

/// Map a set of `std/` import specifiers to the minimal set of Cargo features
/// required to support them, with the transitive closure of feature dependencies
/// from [`FEATURE_DEPS`].
///
/// Returns `Ok(BTreeSet<feature name>)`, or `Err(unknown module specifier)` for
/// any specifier not in [`STD_MODULE_FEATURES`].
///
/// Modules that map to `None` (core/unconditional) contribute no feature.
pub fn required_features<'a>(
    std_imports: &BTreeSet<String>,
) -> Result<BTreeSet<&'a str>, String> {
    // Collect the direct features first.
    let mut features: BTreeSet<&'static str> = BTreeSet::new();
    for spec in std_imports {
        let entry = STD_MODULE_FEATURES
            .iter()
            .find(|(m, _)| *m == spec.as_str())
            .ok_or_else(|| spec.clone())?;
        if let Some(feat) = entry.1 {
            features.insert(feat);
        }
    }

    // Transitive closure: keep expanding until stable.
    let mut changed = true;
    while changed {
        changed = false;
        for (feat, dep) in FEATURE_DEPS {
            if features.contains(feat) && !features.contains(dep) {
                features.insert(dep);
                changed = true;
            }
        }
    }

    Ok(features)
}
