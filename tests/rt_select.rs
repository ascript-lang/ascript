//! RT §4.1–§4.2 drift tests — module→feature table, feature-dependency closure,
//! archive std-import scanner.
//!
//! Three drift tests keep `src/rtstub/std_features.rs` honest:
//!   1. **completeness_bijection** — bijection between STD_MODULES and
//!      STD_MODULE_FEATURES (every module covered, no extras, no duplicates).
//!   2. **gate_drift** — extracted `#[cfg(feature = "X")] "std/Y" =>` pairs from
//!      `src/stdlib/mod.rs` match the checked-in table.
//!   3. **closure_drift** — checked-in FEATURE_DEPS match Cargo.toml [features]
//!      actual edges, and every feature the table names exists in the manifest.
//!
//! Plus two functional tests:
//!   4. **scanner** — `collect_std_imports` over a temp archive with std/json +
//!      std/fs imports (including nested + relative).
//!   5. **required_features_cases** — spot checks on `required_features`.

use ascript::rtstub::std_features::{
    collect_std_imports, required_features, FEATURE_DEPS, STD_MODULE_FEATURES,
};
use ascript::stdlib::STD_MODULES;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

// ─── helpers ────────────────────────────────────────────────────────────────

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// ─── Test 1: completeness/bijection ─────────────────────────────────────────

/// Every entry of `stdlib::STD_MODULES` appears exactly once in
/// `STD_MODULE_FEATURES`, and vice versa.
#[test]
fn completeness_bijection() {
    // Check STD_MODULES ⊆ table (and no duplicates in table).
    let mut table_keys: BTreeSet<&str> = BTreeSet::new();
    let mut duplicates: Vec<&str> = Vec::new();
    for (module, _feat) in STD_MODULE_FEATURES {
        if !table_keys.insert(module) {
            duplicates.push(module);
        }
    }
    assert!(
        duplicates.is_empty(),
        "STD_MODULE_FEATURES has DUPLICATE entries: {duplicates:?}"
    );

    // Every STD_MODULE must be in the table.
    let mut missing_from_table: Vec<&str> = Vec::new();
    for &m in STD_MODULES {
        if !table_keys.contains(m) {
            missing_from_table.push(m);
        }
    }
    assert!(
        missing_from_table.is_empty(),
        "STD_MODULES entries missing from STD_MODULE_FEATURES: {missing_from_table:?}\n\
         Add them to src/rtstub/std_features.rs"
    );

    // Every table entry must be in STD_MODULES (no phantom modules).
    let std_modules_set: BTreeSet<&str> = STD_MODULES.iter().copied().collect();
    let extra_in_table: Vec<&str> = table_keys
        .iter()
        .filter(|&&m| !std_modules_set.contains(m))
        .copied()
        .collect();
    assert!(
        extra_in_table.is_empty(),
        "STD_MODULE_FEATURES has entries NOT in STD_MODULES: {extra_in_table:?}\n\
         Remove them from src/rtstub/std_features.rs"
    );
}

// ─── Test 2: gate drift ─────────────────────────────────────────────────────

/// Extract the `(module, feature | None)` mapping from `src/stdlib/mod.rs` by
/// parsing the `std_module_exports` match arms at test runtime.
///
/// Pattern we look for in the source:
///   - Optional: `#[cfg(feature = "FEAT")]` (possibly with whitespace variants)
///   - Then: `"std/MODULE" => ...` (the match arm)
///
/// We scan line by line, tracking the "pending cfg" from the preceding
/// `#[cfg(feature = "X")]` line.
fn extract_gate_map(src: &str) -> BTreeMap<String, Option<String>> {
    let mut result: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut pending_feat: Option<String> = None;
    let mut in_fn = false;

    for line in src.lines() {
        let trimmed = line.trim();

        // Detect we're inside the std_module_exports function.
        if trimmed.contains("pub fn std_module_exports") {
            in_fn = true;
            continue;
        }
        if !in_fn {
            continue;
        }
        // A closing `}` at col 0 ends the function — but to be safe we just
        // look for the end marker that the `call` fn starts.
        if trimmed.starts_with("pub(crate) fn bi(") || trimmed.starts_with("/// The complete") {
            break;
        }

        // Track pending #[cfg(feature = "X")] before the match arm.
        if trimmed.starts_with("#[cfg(feature = \"") {
            // Extract feature name between the quotes.
            if let Some(rest) = trimmed.strip_prefix("#[cfg(feature = \"") {
                if let Some(name) = rest.split('"').next() {
                    pending_feat = Some(name.to_owned());
                }
            }
            continue;
        }

        // A match arm: `"std/..." => ...`
        if trimmed.starts_with('"') {
            if let Some(rest) = trimmed.strip_prefix('"') {
                if let Some(module_end) = rest.find('"') {
                    let module = &rest[..module_end];
                    if module.starts_with("std/") {
                        result.insert(module.to_owned(), pending_feat.take());
                        continue;
                    }
                }
            }
        }

        // Any other line clears the pending cfg (the gate only covers the NEXT arm).
        // EXCEPTION: the `#[cfg(feature = "datetime")]` before a two-arm block:
        //   `#[cfg(feature = "datetime")]`
        //   `"date" if func == "now" ...` — only in the `call_stdlib` match.
        // We allow the pending to persist through blank lines but clear on any
        // non-blank, non-cfg, non-arm line that's not a continuation of the arm.
        if !trimmed.is_empty()
            && !trimmed.starts_with("//")
            && !trimmed.starts_with('"')
            && !trimmed.starts_with("#[cfg")
        {
            pending_feat = None;
        }
    }
    result
}

/// The checked-in table must match the gates actually present in stdlib/mod.rs.
#[test]
fn gate_drift() {
    let src = read("src/stdlib/mod.rs");
    let extracted = extract_gate_map(&src);

    // Build the expected map from STD_MODULE_FEATURES.
    let expected: BTreeMap<String, Option<String>> = STD_MODULE_FEATURES
        .iter()
        .map(|(m, f)| (m.to_string(), f.map(|s| s.to_owned())))
        .collect();

    // Compare.
    let mut errors: Vec<String> = Vec::new();

    for (module, got_feat) in &extracted {
        match expected.get(module.as_str()) {
            None => errors.push(format!(
                "  {module:25}: in mod.rs with feature={got_feat:?} but NOT in STD_MODULE_FEATURES"
            )),
            Some(want_feat) if want_feat != got_feat => errors.push(format!(
                "  {module:25}: feature mismatch — table says {want_feat:?}, mod.rs says {got_feat:?}"
            )),
            _ => {}
        }
    }
    for (module, want_feat) in &expected {
        if !extracted.contains_key(module.as_str()) {
            errors.push(format!(
                "  {module:25}: in STD_MODULE_FEATURES with feature={want_feat:?} but NOT found in mod.rs std_module_exports"
            ));
        }
    }

    assert!(
        errors.is_empty(),
        "Gate drift detected between src/stdlib/mod.rs and src/rtstub/std_features.rs:\n{}\n\
         Fix the table to match the actual #[cfg(feature)] gates in std_module_exports.",
        errors.join("\n")
    );
}

// ─── Test 3: closure drift ───────────────────────────────────────────────────

/// Parse the [features] section of Cargo.toml and return a map of
/// `feature_name → Vec<direct dep feature names>` (filtering out `dep:xxx` entries
/// and keeping only runtime-relevant features that FEATURE_DEPS cares about).
fn parse_cargo_feature_deps(cargo_src: &str) -> BTreeMap<String, Vec<String>> {
    // Use the `toml` crate (non-optional dep, always present).
    let doc: toml::Value = cargo_src
        .parse()
        .expect("Cargo.toml must be valid TOML");

    let features_table = doc
        .get("features")
        .and_then(|v| v.as_table())
        .expect("Cargo.toml must have a [features] section");

    let mut result: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (feat_name, value) in features_table {
        let deps: Vec<String> = value
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    // Keep only plain feature names (no `dep:xxx`, no `pkg/feature`).
                    .filter(|s| !s.starts_with("dep:") && !s.contains('/'))
                    .map(|s| s.to_owned())
                    .collect()
            })
            .unwrap_or_default();
        result.insert(feat_name.clone(), deps);
    }
    result
}

/// FEATURE_DEPS closure edges must match Cargo.toml [features] actual edges,
/// and every feature named in STD_MODULE_FEATURES must exist in the manifest.
#[test]
fn closure_drift() {
    let cargo_src = read("Cargo.toml");
    let manifest = parse_cargo_feature_deps(&cargo_src);

    let mut errors: Vec<String> = Vec::new();

    // 1. Every edge in FEATURE_DEPS must actually exist in Cargo.toml.
    for (feat, dep) in FEATURE_DEPS {
        match manifest.get(*feat) {
            None => errors.push(format!(
                "  FEATURE_DEPS has edge ({feat} → {dep}) but feature '{feat}' \
                 is not in Cargo.toml [features]"
            )),
            Some(actual_deps) => {
                if !actual_deps.iter().any(|d| d == dep) {
                    errors.push(format!(
                        "  FEATURE_DEPS has edge ({feat} → {dep}) but Cargo.toml \
                         [features.{feat}] does not list '{dep}' as a dependency \
                         (actual: {actual_deps:?})"
                    ));
                }
            }
        }
    }

    // 2. Every feature named in STD_MODULE_FEATURES must exist in the manifest.
    for (_module, maybe_feat) in STD_MODULE_FEATURES {
        if let Some(feat) = maybe_feat {
            if !manifest.contains_key(*feat) {
                errors.push(format!(
                    "  STD_MODULE_FEATURES references feature '{feat}' but it is \
                     not in Cargo.toml [features]"
                ));
            }
        }
    }

    assert!(
        errors.is_empty(),
        "Closure drift detected between src/rtstub/std_features.rs and Cargo.toml:\n{}\n\
         Update FEATURE_DEPS or STD_MODULE_FEATURES to match Cargo.toml.",
        errors.join("\n")
    );
}

// ─── Test 4: scanner ─────────────────────────────────────────────────────────

/// Build a small multi-module archive in-test via `compile_archive`, then assert
/// `collect_std_imports` returns exactly the std/ specifiers used across ALL modules.
///
/// Program layout:
///   entry.as   → imports std/json directly + ./helper (relative) + ./local (relative)
///   helper.as  → imports std/fs (this is the "nested non-entry" import the scanner must catch)
///   local.as   → no std imports (pure relative, no stdlib)
///
/// Expected: {"std/json", "std/fs"} (relative imports excluded, std/fs from nested module included).
#[test]
fn scanner_collects_std_imports_across_modules() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    // entry.as: imports std/json directly, and two relative sibling modules
    std::fs::write(
        root.join("entry.as"),
        r#"import { parse } from "std/json"
import { readFile } from "./helper"
import { noop } from "./local"
let x = parse("[1,2]")
print(x)
"#,
    )
    .expect("write entry.as");

    // helper.as: imports std/fs (the "nested" std import the scanner must catch)
    std::fs::write(
        root.join("helper.as"),
        r#"import { read } from "std/fs"
export fn readFile(p: string): string { return read(p)! }
"#,
    )
    .expect("write helper.as");

    // local.as: no std imports — a pure relative-only module
    std::fs::write(
        root.join("local.as"),
        r#"export fn noop() { }
"#,
    )
    .expect("write local.as");

    let (archive, _report) =
        ascript::compile_archive(&root.join("entry.as"), false, false)
            .expect("compile_archive must succeed for the scanner test");

    let std_imports = collect_std_imports(&archive);

    let expected: BTreeSet<String> = ["std/json", "std/fs"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    assert_eq!(
        std_imports, expected,
        "collect_std_imports returned the wrong set.\n\
         got:  {std_imports:?}\n\
         want: {expected:?}"
    );
}

// ─── Test 5: required_features ───────────────────────────────────────────────

/// Spot-check `required_features` for direct and transitive cases.
#[test]
fn required_features_cases() {
    // std/json → "data" (direct, no transitive deps from data itself in FEATURE_DEPS)
    let imports: BTreeSet<String> = ["std/json"].iter().map(|s| s.to_string()).collect();
    let feats = required_features(&imports).expect("std/json is known");
    assert!(
        feats.contains("data"),
        "std/json must require 'data', got {feats:?}"
    );
    assert!(!feats.contains("binary"), "std/json must NOT require 'binary'");

    // std/msgpack → "binary" (direct) + "data" (transitive via binary→data)
    let imports: BTreeSet<String> = ["std/msgpack"].iter().map(|s| s.to_string()).collect();
    let feats = required_features(&imports).expect("std/msgpack is known");
    assert!(
        feats.contains("binary"),
        "std/msgpack must require 'binary', got {feats:?}"
    );
    assert!(
        feats.contains("data"),
        "std/msgpack must transitively require 'data' (binary→data), got {feats:?}"
    );

    // std/math → core/None → empty feature set
    let imports: BTreeSet<String> = ["std/math"].iter().map(|s| s.to_string()).collect();
    let feats = required_features(&imports).expect("std/math is known");
    assert!(
        feats.is_empty(),
        "std/math is core and must require NO features, got {feats:?}"
    );

    // unknown specifier → Err
    let imports: BTreeSet<String> = ["std/nonexistent_module_xyz"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let result = required_features(&imports);
    assert!(
        result.is_err(),
        "an unknown module must return Err, got Ok({:?})",
        result.unwrap()
    );
}
