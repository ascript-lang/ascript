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

    // 3. REVERSE direction (the dangerous drift): FEATURE_DEPS must be COMPLETE over
    //    the features `required_features` actually traverses. Compute the set of
    //    "relevant" features = the closure of the STD_MODULE_FEATURES value-set under
    //    Cargo's REAL feature→feature edges (the ground truth of what the stub's feature
    //    resolution must reach). For every relevant feature, every bare-feature edge it
    //    has in Cargo.toml MUST appear in FEATURE_DEPS — else a NEW Cargo edge (e.g.
    //    `binary` gaining a dep on a new runtime feature) would silently make
    //    `required_features` under-report the closure while this gate stayed green.
    let mut relevant: BTreeSet<&str> = STD_MODULE_FEATURES
        .iter()
        .filter_map(|(_m, f)| *f)
        .collect();
    loop {
        let mut grew = false;
        let snapshot: Vec<&str> = relevant.iter().copied().collect();
        for feat in snapshot {
            if let Some(deps) = manifest.get(feat) {
                for dep in deps {
                    // Only follow into features that themselves appear in the manifest
                    // (bare feature→feature edges; `dep:`/`pkg/feat` already filtered out
                    // by parse_cargo_feature_deps).
                    if manifest.contains_key(dep) && relevant.insert(dep.as_str()) {
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }
    for feat in &relevant {
        if let Some(deps) = manifest.get(*feat) {
            for dep in deps {
                if manifest.contains_key(dep)
                    && !FEATURE_DEPS.iter().any(|(f, d)| f == feat && d == dep)
                {
                    errors.push(format!(
                        "  Cargo.toml [features.{feat}] lists '{dep}' (a feature→feature \
                         edge among closure-relevant features) but FEATURE_DEPS is MISSING \
                         the ({feat} → {dep}) edge — required_features would under-report \
                         the closure. Add it to FEATURE_DEPS."
                    ));
                }
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

// ════════════════════════════════════════════════════════════════════════════════
// RT Task 5 — tiers, nearest-superset selection, build report.
// ════════════════════════════════════════════════════════════════════════════════

use ascript::rtstub::report::{BuildReport, PayloadInfo, StubInfo};
use ascript::rtstub::select::{select, Selection, TierSource};
use ascript::rtstub::tiers::{select_tier, validate_forced_tier, Tier};

// ─── Test 6: the tier chain is a STRICT superset chain ───────────────────────

/// rt-core ⊊ rt-local ⊊ rt-net ⊊ rt-full — each is a PROPER superset of the
/// previous (set containment over the FEATURES consts, both `⊇` and `≠`).
#[test]
fn tier_chain_is_strict_superset() {
    let pairs = [
        (Tier::RtCore, Tier::RtLocal),
        (Tier::RtLocal, Tier::RtNet),
        (Tier::RtNet, Tier::RtFull),
    ];
    for (lower, higher) in pairs {
        let lo = lower.feature_set();
        let hi = higher.feature_set();
        // superset: every feature of the lower tier is in the higher tier
        assert!(
            lo.iter().all(|f| hi.contains(f)),
            "{} is not a superset of {}: missing {:?}",
            higher.name(),
            lower.name(),
            lo.difference(&hi).collect::<Vec<_>>(),
        );
        // proper: the higher tier has strictly more
        assert!(
            hi.len() > lo.len(),
            "{} must be a PROPER superset of {} (strictly larger), got {} vs {}",
            higher.name(),
            lower.name(),
            hi.len(),
            lo.len(),
        );
    }
    // bundle-zstd present in every tier (the stub-side decompressor, §3.1)
    for t in Tier::CHAIN {
        assert!(
            t.feature_set().contains("bundle-zstd"),
            "{} must include bundle-zstd",
            t.name()
        );
    }
}

// ─── Test 7: nearest-superset selection ──────────────────────────────────────

fn set(items: &[&str]) -> BTreeSet<&'static str> {
    // SAFETY of 'static: the literals live for the program lifetime.
    items
        .iter()
        .map(|s| -> &'static str {
            match *s {
                "data" => "data",
                "net" => "net",
                "ffi" => "ffi",
                "binary" => "binary",
                "shared" => "shared",
                other => Box::leak(other.to_string().into_boxed_str()),
            }
        })
        .collect()
}

#[test]
fn select_tier_nearest_superset() {
    assert_eq!(select_tier(&set(&[])), Tier::RtCore, "empty → rt-core");
    assert_eq!(select_tier(&set(&["data"])), Tier::RtLocal, "data → rt-local");
    assert_eq!(select_tier(&set(&["net"])), Tier::RtNet, "net → rt-net");
    assert_eq!(select_tier(&set(&["ffi"])), Tier::RtFull, "ffi → rt-full");
    // binary alone (a local-tier feature) → rt-local
    assert_eq!(select_tier(&set(&["binary"])), Tier::RtLocal);
}

// ─── Test 8: --tier downgrade error names missing features + modules ──────────

#[test]
fn tier_downgrade_below_requirement_errors() {
    // A program needing `data` forced to rt-core (which lacks `data`).
    let required = set(&["data"]);
    let demanding = vec![("std/json".to_string(), "data")];
    let err = validate_forced_tier(Tier::RtCore, &required, &demanding)
        .expect_err("rt-core cannot satisfy a `data` requirement");
    assert!(err.contains("data"), "error must name the missing feature: {err}");
    assert!(
        err.contains("std/json"),
        "error must name the demanding module: {err}"
    );
    assert!(err.contains("rt-core"), "error must name the forced tier: {err}");

    // A satisfying downgrade passes (rt-full forced for a `data`-only program).
    assert!(validate_forced_tier(Tier::RtFull, &required, &demanding).is_ok());
    // Forcing the exact tier passes.
    assert!(validate_forced_tier(Tier::RtLocal, &required, &demanding).is_ok());
}

/// The high-level `select()` entry surfaces the same downgrade error through `--tier`.
#[test]
fn select_with_forced_insufficient_tier_errors() {
    let imports: BTreeSet<String> = ["std/json"].iter().map(|s| s.to_string()).collect();
    let err = select(&imports, Some(Tier::RtCore))
        .expect_err("forcing rt-core for a std/json program must fail");
    assert!(err.contains("data") && err.contains("std/json"), "{err}");

    // Automatic selection picks rt-local for std/json.
    let sel = select(&imports, None).expect("auto-select must succeed");
    assert_eq!(sel.tier, Tier::RtLocal);
    assert_eq!(sel.source, TierSource::Selected);
    assert!(sel.required.contains(&"data".to_string()));
    // rt-local carries many features std/json does not need.
    assert!(sel.unused.contains(&"sql".to_string()));
    assert!(!sel.unused.contains(&"data".to_string()), "required ∉ unused");
}

// ─── Test 9: tier drift — tiers.rs vs scripts/build-rt.sh ─────────────────────

/// Parse `scripts/build-rt.sh`'s `case` arms (`rt-X) FEATURES="a,b,c" ;;`) and assert
/// each tier's feature list equals `tiers.rs`'s FEATURES const — BOTH directions (the
/// script and the code are one source of truth tested against the other).
#[test]
fn tier_drift_against_build_script() {
    let script = read("scripts/build-rt.sh");

    // Extract `FEATURES="..."` for each `rt-X)` case arm.
    let mut from_script: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in script.lines() {
        let t = line.trim();
        // e.g.  rt-core)  FEATURES="shared,bundle-zstd" ;;
        for tier_name in ["rt-core", "rt-local", "rt-net", "rt-full"] {
            let prefix = format!("{tier_name})");
            if t.starts_with(&prefix) {
                let feats = t
                    .split_once("FEATURES=\"")
                    .and_then(|(_, rest)| rest.split_once('"'))
                    .map(|(f, _)| f)
                    .unwrap_or_else(|| panic!("no FEATURES=\"...\" on line: {t}"));
                let list: Vec<String> = feats.split(',').map(|s| s.trim().to_string()).collect();
                from_script.insert(tier_name.to_string(), list);
            }
        }
    }

    assert_eq!(
        from_script.len(),
        4,
        "expected 4 tier case arms in build-rt.sh, found {}: {:?}",
        from_script.len(),
        from_script.keys().collect::<Vec<_>>()
    );

    for tier in Tier::CHAIN {
        let script_feats: BTreeSet<&str> = from_script
            .get(tier.name())
            .unwrap_or_else(|| panic!("build-rt.sh missing tier {}", tier.name()))
            .iter()
            .map(|s| s.as_str())
            .collect();
        let code_feats: BTreeSet<&str> = tier.feature_set();
        assert_eq!(
            script_feats, code_feats,
            "tier '{}' DRIFT between scripts/build-rt.sh and src/rtstub/tiers.rs:\n\
             script: {:?}\n  code: {:?}",
            tier.name(),
            script_feats,
            code_feats,
        );
    }
}

// ─── Test 10: build report JSON schema + determinism + sha pinning ────────────

fn sample_report() -> BuildReport {
    let imports: BTreeSet<String> = ["std/math"].iter().map(|s| s.to_string()).collect();
    let selection: Selection = select(&imports, None).expect("select std/math");
    BuildReport {
        source: "hello.as".to_string(),
        output: "hello".to_string(),
        output_sha256: "a".repeat(64),
        target: None,
        tier: selection.tier,
        tier_source: selection.source,
        selection,
        payload: PayloadInfo {
            format: "aso",
            compressed: false,
            size: 1234,
            uncompressed_size: 1234,
            sha256: "b".repeat(64),
        },
        stub: StubInfo {
            origin: "current_exe",
            sha256: "c".repeat(64),
            size: 42_000_000,
        },
        module_count: 1,
        shake_digest: None,
        caps_all_granted: true,
    }
}

#[test]
fn report_json_schema_and_fields() {
    let r = sample_report();
    let json = r.to_json();

    // Schema 1 + the §9.2 field set present.
    assert!(json.contains("\"schema\":1"), "schema 1 marker: {json}");
    for key in [
        "\"source\":",
        "\"output\":",
        "\"output_sha256\":",
        "\"target\":",
        "\"tier\":",
        "\"tier_source\":",
        "\"required\":",
        "\"stub_features\":",
        "\"unused\":",
        "\"payload\":",
        "\"module_count\":",
        "\"caps_all_granted\":",
    ] {
        assert!(json.contains(key), "missing JSON key {key} in {json}");
    }

    // §9.1 determinism: NO timestamp field of any kind.
    for forbidden in ["created", "timestamp", "time", "date", "built_at", "now"] {
        assert!(
            !json.contains(&format!("\"{forbidden}\"")),
            "report JSON must contain NO time field, found '{forbidden}': {json}"
        );
    }

    // rt-core for a std/math-only program, tier_source = selected.
    assert!(json.contains("\"tier\":\"rt-core\""), "{json}");
    assert!(json.contains("\"tier_source\":\"selected\""), "{json}");
    // null target (host).
    assert!(json.contains("\"target\":null"), "{json}");
}

#[test]
fn report_json_is_deterministic() {
    let a = sample_report().to_json();
    let b = sample_report().to_json();
    assert_eq!(a, b, "the build report JSON must be byte-identical across builds");
}

/// The §4.6 stderr report carries the tier, the stub origin, and the sizes.
#[test]
fn report_stderr_carries_tier_origin_sizes() {
    let r = sample_report();
    let s = r.render_stderr();
    assert!(s.contains("rt-core"), "stderr report must name the tier: {s}");
    assert!(s.contains("current_exe"), "stderr report must name the stub origin: {s}");
    assert!(s.contains("42000000"), "stderr report must carry the stub size: {s}");
    assert!(s.contains("1234"), "stderr report must carry the payload size: {s}");
}

/// END-TO-END: `--report-json -` on a real hello build emits schema-1 JSON whose
/// `output_sha256` matches an INDEPENDENTLY computed sha256 of the artifact bytes, and a
/// double-build yields an identical digest (§9.1 determinism).
#[test]
fn report_json_end_to_end_sha_stable() {
    use std::process::Command;

    fn bin() -> &'static str {
        env!("CARGO_BIN_EXE_ascript")
    }
    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(bytes);
        let d = h.finalize();
        d.iter().map(|b| format!("{b:02x}")).collect()
    }
    fn extract<'a>(json: &'a str, key: &str) -> &'a str {
        let needle = format!("\"{key}\":\"");
        let start = json.find(&needle).unwrap_or_else(|| panic!("no {key} in {json}")) + needle.len();
        let rest = &json[start..];
        let end = rest.find('"').unwrap();
        &rest[..end]
    }

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("prog.as");
    std::fs::write(&src, "import { sqrt } from \"std/math\"\nprint(sqrt(16.0))\n").unwrap();

    let build = |out_name: &str| -> String {
        let out = dir.path().join(out_name);
        let o = Command::new(bin())
            .args(["build", "--native", "--report-json", "-"])
            .arg(&src)
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "build failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        // The JSON line is on stdout (the only stdout line besides `bundled … -> …`).
        let stdout = String::from_utf8(o.stdout).unwrap();
        let json_line = stdout
            .lines()
            .find(|l| l.trim_start().starts_with('{'))
            .unwrap_or_else(|| panic!("no JSON line on stdout: {stdout}"))
            .to_string();

        // The report's output_sha256 must equal the real artifact's sha256.
        let artifact = std::fs::read(&out).unwrap();
        assert_eq!(
            extract(&json_line, "output_sha256"),
            sha256_hex(&artifact),
            "report output_sha256 must equal the real artifact digest"
        );
        // std/math is core → rt-core selected.
        assert!(json_line.contains("\"tier\":\"rt-core\""), "{json_line}");
        // unused-feature delta is present (rt-core has bundle-zstd/shared unused here).
        assert!(json_line.contains("\"unused\":["), "{json_line}");
        json_line
    };

    let j1 = build("prog1");
    let j2 = build("prog2");
    // The two builds differ only by output PATH; their output_sha256 (a function of the
    // payload+stub bytes, not the name) must be identical, and the rest of the report too.
    assert_eq!(
        extract(&j1, "output_sha256"),
        extract(&j2, "output_sha256"),
        "two builds of the same source must have a stable output_sha256"
    );
}

// ─── RT §9.2 — report-json schema lock ──────────────────────────────────────
//
// This is the VERSIONED CI CONTRACT for `--report-json` consumers.
// Bumping the schema (adding/removing/retyping a top-level field) REQUIRES:
//   1. Setting `"schema": 2` in `BuildReport::to_json` (src/rtstub/report.rs).
//   2. Updating BOTH the `EXPECTED_TOP_LEVEL_KEYS` set AND the type assertions
//      in this test to match the new schema.
//
// The test MUST fail if any field is added, removed, or retyped without a schema bump.

/// Parse the top-level keys of a hand-rolled JSON object (no serde dep).
/// Returns a sorted list of `(key, type_tag)` where type_tag is one of:
///   "number", "string", "bool", "null", "array", "object"
fn parse_top_level_schema(json: &str) -> Vec<(String, &'static str)> {
    // Strip outer braces.
    let inner = json
        .trim()
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .expect("JSON must be an object");

    let mut result = Vec::new();
    // Walk the inner string extracting `"key":<value>` pairs.
    let mut pos = 0;
    let bytes = inner.as_bytes();
    while pos < bytes.len() {
        // Skip whitespace/comma.
        while pos < bytes.len() && (bytes[pos] == b' ' || bytes[pos] == b',' || bytes[pos] == b'\n') {
            pos += 1;
        }
        if pos >= bytes.len() { break; }
        // Expect a `"key":` entry.
        if bytes[pos] != b'"' { pos += 1; continue; }
        pos += 1; // skip opening quote
        let key_start = pos;
        while pos < bytes.len() && bytes[pos] != b'"' {
            if bytes[pos] == b'\\' { pos += 1; }
            pos += 1;
        }
        let key = &inner[key_start..pos];
        pos += 1; // skip closing quote
        // Skip `:`.
        while pos < bytes.len() && (bytes[pos] == b':' || bytes[pos] == b' ') { pos += 1; }
        // Peek at value type.
        if pos >= bytes.len() { break; }
        let type_tag: &'static str = match bytes[pos] {
            b'"' => "string",
            b'{' => "object",
            b'[' => "array",
            b't' | b'f' => "bool",
            b'n' => "null",
            b'0'..=b'9' | b'-' => "number",
            _ => "unknown",
        };
        result.push((key.to_string(), type_tag));
        // Skip past the value (depth-counting to handle nested structures).
        let open = bytes[pos];
        if open == b'{' || open == b'[' {
            let close = if open == b'{' { b'}' } else { b']' };
            let mut depth = 0i32;
            let mut in_str = false;
            while pos < bytes.len() {
                let b = bytes[pos];
                if in_str {
                    if b == b'\\' { pos += 1; }
                    else if b == b'"' { in_str = false; }
                } else {
                    if b == b'"' { in_str = true; }
                    else if b == open { depth += 1; }
                    else if b == close {
                        depth -= 1;
                        if depth == 0 { pos += 1; break; }
                    }
                }
                pos += 1;
            }
        } else if bytes[pos] == b'"' {
            // string: skip past closing quote
            pos += 1;
            while pos < bytes.len() {
                if bytes[pos] == b'\\' { pos += 1; }
                else if bytes[pos] == b'"' { pos += 1; break; }
                pos += 1;
            }
        } else {
            // scalar: skip to next comma/} outside string
            while pos < bytes.len() && bytes[pos] != b',' && bytes[pos] != b'}' { pos += 1; }
        }
    }
    result
}

/// RT §9.2 — the STRICT schema lock for `BuildReport::to_json`.
///
/// **CI CONTRACT**: This test encodes the EXACT top-level key set + value types of the
/// `schema: 1` JSON document. A consumer of `--report-json` can depend on these fields
/// being stable. Any change to the key set (add/remove/retype) MUST bump `"schema"` to 2
/// AND update this test. This test MUST fail on any un-versioned field change.
///
/// # Schema 1 field manifest (§9.2):
/// ```
///   "schema"          : number   — always 1 (the schema version)
///   "source"          : string   — source file path
///   "output"          : string   — artifact output path
///   "output_sha256"   : string   — sha256(artifact bytes)
///   "target"          : string|null — cross target or null (host)
///   "tier"            : string   — "rt-core" | "rt-local" | "rt-net" | "rt-full"
///   "tier_source"     : string   — "selected" | "--tier"
///   "required"        : array    — features the program requires (closure-expanded)
///   "stub_features"   : array    — full feature set shipped by the chosen tier
///   "unused"          : array    — stub_features \ required (potential --exact savings)
///   "stub"            : object   — {origin: string, sha256: string, size: number}
///   "payload"         : object   — {format, compressed, size, uncompressed_size, sha256}
///   "module_count"    : number   — number of embedded modules
///   "shake_digest"    : string|null — archive shake digest or null
///   "caps_all_granted": bool     — whether caps are all-granted
/// ```
#[test]
fn report_json_schema_lock() {
    // Build the sample report (same as used by the other report tests).
    let r = sample_report();
    let json = r.to_json();

    // ── 1. Schema version marker ────────────────────────────────────────────
    assert!(
        json.contains("\"schema\":1"),
        "schema must be 1 (the current schema version); update to schema 2 if you are \
         adding/removing/retyping fields: {json}"
    );

    // ── 2. Parse the top-level key set ──────────────────────────────────────
    let schema = parse_top_level_schema(&json);
    let got_keys: Vec<&str> = schema.iter().map(|(k, _)| k.as_str()).collect();

    // The EXACT set of top-level keys in schema 1, in §9.2 canonical order.
    // If you need to add/remove/rename a key: bump to schema 2 and update this list.
    const EXPECTED_KEYS_IN_ORDER: &[&str] = &[
        "schema",
        "source",
        "output",
        "output_sha256",
        "target",
        "tier",
        "tier_source",
        "required",
        "stub_features",
        "unused",
        "stub",
        "payload",
        "module_count",
        "shake_digest",
        "caps_all_granted",
    ];

    assert_eq!(
        got_keys,
        EXPECTED_KEYS_IN_ORDER,
        "§9.2 SCHEMA LOCK VIOLATED: the top-level key set or order has changed.\n\
         To add/remove/retype a field: bump to '\"schema\":2' in BuildReport::to_json \
         AND update EXPECTED_KEYS_IN_ORDER and the type assertions in this test.\n\
         Got keys (in order): {got_keys:?}\n\
         Expected:            {EXPECTED_KEYS_IN_ORDER:?}"
    );

    // ── 3. Assert exact types for every top-level field ──────────────────────
    // Any type change is a schema change and requires schema 2.
    let type_of = |key: &str| -> &'static str {
        schema
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, t)| *t)
            .unwrap_or("MISSING")
    };

    assert_eq!(type_of("schema"), "number", "schema must be a number");
    assert_eq!(type_of("source"), "string", "source must be a string");
    assert_eq!(type_of("output"), "string", "output must be a string");
    assert_eq!(type_of("output_sha256"), "string", "output_sha256 must be a string");
    // target is null when no cross-target (sample_report has target=None).
    assert_eq!(type_of("target"), "null", "target must be null (string when cross-target)");
    assert_eq!(type_of("tier"), "string", "tier must be a string");
    assert_eq!(type_of("tier_source"), "string", "tier_source must be a string");
    assert_eq!(type_of("required"), "array", "required must be an array");
    assert_eq!(type_of("stub_features"), "array", "stub_features must be an array");
    assert_eq!(type_of("unused"), "array", "unused must be an array");
    assert_eq!(type_of("stub"), "object", "stub must be an object");
    assert_eq!(type_of("payload"), "object", "payload must be an object");
    assert_eq!(type_of("module_count"), "number", "module_count must be a number");
    // shake_digest is null for a single-module build (sample_report has shake_digest=None).
    assert_eq!(type_of("shake_digest"), "null", "shake_digest must be null (string for multi-module)");
    assert_eq!(type_of("caps_all_granted"), "bool", "caps_all_granted must be a bool");

    // ── 4. Verify the stub sub-object fields ────────────────────────────────
    // Extract the stub sub-object and verify its structure.
    let stub_start = json.find("\"stub\":{").expect("stub object missing") + "\"stub\":".len();
    // Find the matching closing brace.
    let stub_json = {
        let tail = &json[stub_start..];
        let mut depth = 0i32;
        let mut end = 0;
        let mut in_str = false;
        for (i, b) in tail.bytes().enumerate() {
            if in_str {
                if b == b'\\' { continue; }
                if b == b'"' { in_str = false; }
            } else {
                match b {
                    b'"' => { in_str = true; }
                    b'{' => { depth += 1; }
                    b'}' => {
                        depth -= 1;
                        if depth == 0 { end = i + 1; break; }
                    }
                    _ => {}
                }
            }
        }
        &tail[..end]
    };
    let stub_schema = parse_top_level_schema(stub_json);
    let stub_keys: Vec<&str> = stub_schema.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        stub_keys,
        &["origin", "sha256", "size"],
        "stub sub-object must have exactly {{origin, sha256, size}}: {stub_json}"
    );
    assert_eq!(stub_schema[0].1, "string", "stub.origin must be string");
    assert_eq!(stub_schema[1].1, "string", "stub.sha256 must be string");
    assert_eq!(stub_schema[2].1, "number", "stub.size must be number");

    // ── 5. Verify the payload sub-object fields ──────────────────────────────
    let payload_start = json.find("\"payload\":{").expect("payload object missing") + "\"payload\":".len();
    let payload_json = {
        let tail = &json[payload_start..];
        let mut depth = 0i32;
        let mut end = 0;
        let mut in_str = false;
        for (i, b) in tail.bytes().enumerate() {
            if in_str {
                if b == b'\\' { continue; }
                if b == b'"' { in_str = false; }
            } else {
                match b {
                    b'"' => { in_str = true; }
                    b'{' => { depth += 1; }
                    b'}' => {
                        depth -= 1;
                        if depth == 0 { end = i + 1; break; }
                    }
                    _ => {}
                }
            }
        }
        &tail[..end]
    };
    let payload_schema = parse_top_level_schema(payload_json);
    let payload_keys: Vec<&str> = payload_schema.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        payload_keys,
        &["format", "compressed", "size", "uncompressed_size", "sha256"],
        "payload sub-object must have exactly \
         {{format, compressed, size, uncompressed_size, sha256}}: {payload_json}"
    );
    assert_eq!(payload_schema[0].1, "string", "payload.format must be string");
    assert_eq!(payload_schema[1].1, "bool", "payload.compressed must be bool");
    assert_eq!(payload_schema[2].1, "number", "payload.size must be number");
    assert_eq!(payload_schema[3].1, "number", "payload.uncompressed_size must be number");
    assert_eq!(payload_schema[4].1, "string", "payload.sha256 must be string");

    // ── 6. No time/date fields anywhere (§9.1 determinism contract) ──────────
    for forbidden in [
        "\"created\"", "\"timestamp\"", "\"time\"", "\"date\"",
        "\"built_at\"", "\"now\"", "\"built\"",
    ] {
        assert!(
            !json.contains(forbidden),
            "§9.1 violation: report JSON must contain NO time field; \
             found {forbidden} in:\n{json}"
        );
    }
}

// ─── RT §4.5 exact_build_plan pure-unit tests ───────────────────────────────

use ascript::rtstub::exact::{check_darwin_cross, exact_build_plan};
use std::collections::BTreeSet as BS;

/// `exact_build_plan` always includes `bundle-zstd` and `shared`.
#[test]
fn exact_build_plan_minimal_empty_required() {
    let req: BS<&str> = BS::new();
    let (argv, env) = exact_build_plan(&req, None);
    // Verify the fixed argv prefix.
    assert_eq!(&argv[..6], &["build", "--release", "--bin", "ascript-rt",
                              "--no-default-features", "--features"]);
    // The features arg must contain bundle-zstd and shared (sorted).
    let feat_arg = &argv[6];
    let mut parts: Vec<&str> = feat_arg.split(',').collect();
    parts.sort_unstable();
    assert!(parts.contains(&"bundle-zstd"), "bundle-zstd missing: {feat_arg}");
    assert!(parts.contains(&"shared"), "shared missing: {feat_arg}");
    // No --target when target is None.
    assert!(!argv.contains(&"--target".to_string()), "--target present but not requested");
    // Required env vars.
    assert!(env.contains(&("ASCRIPT_RT".to_string(), "1".to_string())));
    assert!(env.contains(&("ASCRIPT_RT_TIER".to_string(), "custom".to_string())));
}

/// With `{"data"}` required, the features arg must be `"bundle-zstd,data,shared"` (sorted).
#[test]
fn exact_build_plan_with_data_feature() {
    let mut req: BS<&str> = BS::new();
    req.insert("data");
    let (argv, _env) = exact_build_plan(&req, None);
    let feat_arg = &argv[6];
    // Sorted order: bundle-zstd < data < shared
    assert_eq!(feat_arg, "bundle-zstd,data,shared", "feature set: {feat_arg}");
}

/// With an empty required set the features should be exactly `"bundle-zstd,shared"` (sorted).
#[test]
fn exact_build_plan_empty_required_features_string() {
    let req: BS<&str> = BS::new();
    let (argv, _env) = exact_build_plan(&req, None);
    let feat_arg = &argv[6];
    assert_eq!(feat_arg, "bundle-zstd,shared");
}

/// With `{"ffi"}` required and a target, `--target` appears in the argv.
#[test]
fn exact_build_plan_with_target() {
    let mut req: BS<&str> = BS::new();
    req.insert("ffi");
    let target = "x86_64-unknown-linux-gnu";
    let (argv, _env) = exact_build_plan(&req, Some(target));
    let target_idx = argv
        .iter()
        .position(|a| a == "--target")
        .expect("--target not in argv");
    assert_eq!(argv[target_idx + 1], target);
}

/// Detection: cargo missing → specific error mentioning "install cargo" / "rustup.rs".
#[test]
fn exact_detect_no_cargo() {
    use ascript::rtstub::exact::DetectContext;
    let ctx = DetectContext {
        cargo_available: false,
        ascript_src: Some(std::path::PathBuf::from("/some/path")),
        version_override: Some(env!("CARGO_PKG_VERSION").to_string()),
    };
    let err = ascript::rtstub::exact::detect(&ctx).unwrap_err();
    assert!(
        err.contains("install") || err.contains("cargo"),
        "expected 'install cargo' hint, got: {err}"
    );
    assert!(
        err.contains("rustup.rs") || err.contains("PATH"),
        "expected PATH/rustup hint, got: {err}"
    );
}

/// Detection: `$ASCRIPT_SRC` unset → specific error mentioning ASCRIPT_SRC.
#[test]
fn exact_detect_no_ascript_src() {
    use ascript::rtstub::exact::DetectContext;
    let ctx = DetectContext {
        cargo_available: true,
        ascript_src: None,
        version_override: Some(env!("CARGO_PKG_VERSION").to_string()),
    };
    let err = ascript::rtstub::exact::detect(&ctx).unwrap_err();
    assert!(
        err.contains("ASCRIPT_SRC"),
        "expected ASCRIPT_SRC in error, got: {err}"
    );
}

/// Detection: version mismatch → specific error naming both versions.
#[test]
fn exact_detect_version_mismatch() {
    use ascript::rtstub::exact::DetectContext;
    let ctx = DetectContext {
        cargo_available: true,
        ascript_src: Some(std::path::PathBuf::from("/some/path")),
        version_override: Some("0.0.0-wrong".to_string()),
    };
    let err = ascript::rtstub::exact::detect(&ctx).unwrap_err();
    assert!(
        err.contains("0.0.0-wrong"),
        "expected mismatch version in error, got: {err}"
    );
    assert!(
        err.contains(env!("CARGO_PKG_VERSION")),
        "expected current version in error, got: {err}"
    );
    assert!(
        err.contains("mismatch") || err.contains("version"),
        "expected version mismatch message, got: {err}"
    );
}

/// Detection: matching version → Ok.
#[test]
fn exact_detect_version_match_ok() {
    use ascript::rtstub::exact::DetectContext;
    let ctx = DetectContext {
        cargo_available: true,
        ascript_src: Some(std::path::PathBuf::from("/tmp/fake-src")),
        version_override: Some(env!("CARGO_PKG_VERSION").to_string()),
    };
    // Should succeed (the src_path is not validated for existence in detect itself —
    // only the version is checked via the override).
    let result = ascript::rtstub::exact::detect(&ctx);
    assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
}

/// `--exact --target *-apple-darwin` on a non-macOS host → rejection.
#[cfg(not(target_os = "macos"))]
#[test]
fn exact_darwin_cross_rejected_on_non_macos() {
    let err = check_darwin_cross(Some("aarch64-apple-darwin")).unwrap_err();
    assert!(
        err.contains("non-macOS") || err.contains("macOS"),
        "expected macOS-host error, got: {err}"
    );
    assert!(
        err.contains("apple-darwin"),
        "expected target name in error, got: {err}"
    );
}

/// `--exact --target x86_64-unknown-linux-gnu` is always fine (non-darwin target).
#[test]
fn exact_non_darwin_target_ok() {
    assert!(
        check_darwin_cross(Some("x86_64-unknown-linux-gnu")).is_ok(),
        "non-darwin target must not be rejected"
    );
}

/// `--exact` with no target (`None`) is fine (host, never darwin unless we're on macOS).
#[cfg(not(target_os = "macos"))]
#[test]
fn exact_no_target_ok_on_non_macos() {
    assert!(
        check_darwin_cross(None).is_ok(),
        "no target on non-macOS must not be rejected"
    );
}
