//! LSPEC — permanent spec-drift guardrails (spec: 2026-06-12-language-spec-stability-design.md §7).
//!
//! Keep the normative AScript specification (the 16 chapters under
//! `docs/content/spec/`) in lockstep with the implementation. Two mechanical
//! check groups: (7.1) grammar-rule COVERAGE — every named `grammar.js` rule
//! appears verbatim in `docs/content/spec/grammar.md`; (7.2) chapter manifest +
//! citation existence — all 16 chapter files exist, each has a `## Conformance`
//! section, and every `examples/…`/`tests/…` path cited there exists on disk.
//!
//! House pattern: tests-as-gates (`tests/docs_drift.rs`, `tests/srv_negative_space.rs`),
//! std-only, repo-rooted via `env!("CARGO_MANIFEST_DIR")`, no new deps. Each pure
//! helper is exercised by a deliberate-mutation self-test (anti-false-green). Fix
//! the SPEC, never the assertion.
//!
//! RED-BRANCH NOTE (LSPEC Phase 1, Task 1.1): this file is committed deliberately
//! RED. `docs/content/spec/` does not exist yet, so `grammar_rules_are_covered_by_spec`
//! and `spec_chapters_exist_with_conformance_sections` FAIL by design; the mutation
//! self-test `spec_drift_helpers_catch_mutations` PASSES (it proves the helpers work).
//! Phase 2 writes the chapters and turns the real tests green. The branch may be red
//! ONLY on this file until then.

use std::path::{Path, PathBuf};

/// The normative spec's 16 chapter slugs (LSPEC §2 / §7.2). Checked-in manifest:
/// every slug must resolve to `docs/content/spec/<slug>.md`.
const SPEC_CHAPTERS: [&str; 16] = [
    "intro",
    "lexical",
    "grammar",
    "values",
    "expressions",
    "statements",
    "classes",
    "patterns",
    "errors",
    "modules",
    "concurrency",
    "capabilities",
    "types",
    "stdlib",
    "conformance",
    "stability",
];

/// Minimum byte length a real chapter must have (a stub can't false-green §7.2).
const MIN_CHAPTER_BYTES: usize = 1500;

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

// ─── Pure helpers (std-only) ─────────────────────────────────────────────────

/// Extract every named rule from `grammar.js`: lines indented by EXACTLY four
/// spaces whose 5th char begins an identifier `[a-z_]`, taking `[a-z0-9_]+` up to
/// a terminating `:` (the rules-object entries, INCLUDING `_`-prefixed hidden
/// rules). Lines whose identifier is empty or is not immediately followed by `:`
/// are rejected — this is rule-name COVERAGE extraction, not a JS parser.
fn grammar_rule_names(grammar_js: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in grammar_js.lines() {
        // Exactly four leading spaces (rules-object entries sit at this depth;
        // PREC entries are two-space, choice/seq contents are six-plus).
        let Some(rest) = line.strip_prefix("    ") else {
            continue;
        };
        if rest.starts_with(' ') {
            continue; // five-plus spaces — deeper than the rules object
        }
        let mut chars = rest.char_indices();
        // 5th char of the line == first char of `rest` must open an identifier.
        let Some((_, first)) = chars.next() else {
            continue;
        };
        if !(first.is_ascii_lowercase() || first == '_') {
            continue;
        }
        // Consume the rest of the identifier `[a-z0-9_]*`.
        let mut end = first.len_utf8();
        for (i, c) in chars {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
                end = i + c.len_utf8();
            } else {
                break;
            }
        }
        let name = &rest[..end];
        // The identifier must be terminated IMMEDIATELY by ':' (a rules entry).
        let after = &rest[end..];
        if name.is_empty() || !after.starts_with(':') {
            continue;
        }
        out.push(name.to_string());
    }
    out
}

/// Every rule name must appear verbatim (a backticked mention or any occurrence)
/// in the spec grammar chapter text; return those that DON'T.
fn uncovered_rules(rules: &[String], grammar_md: &str) -> Vec<String> {
    rules
        .iter()
        .filter(|r| !grammar_md.contains(r.as_str()))
        .cloned()
        .collect()
}

/// Chapter manifest checks: for each manifest slug the file exists under
/// `spec_dir`, is at least `MIN_CHAPTER_BYTES` long, and contains a
/// `## Conformance` section. Returns one `slug: reason` string per violation.
fn chapter_violations(spec_dir: &Path, manifest: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for slug in manifest {
        let path = spec_dir.join(format!("{slug}.md"));
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(_) => {
                out.push(format!("{slug}: missing file {}", path.display()));
                continue;
            }
        };
        if body.len() < MIN_CHAPTER_BYTES {
            out.push(format!(
                "{slug}: too short ({} bytes < {MIN_CHAPTER_BYTES})",
                body.len()
            ));
        }
        if !body.contains("## Conformance") {
            out.push(format!("{slug}: missing '## Conformance' section"));
        }
    }
    out
}

/// Scan a chapter's `## Conformance` section for backticked repo-relative
/// `examples/...` / `tests/...` tokens; return those that don't exist on disk.
fn dead_citations(chapter_md: &str, repo_root: &Path) -> Vec<String> {
    // Isolate the Conformance section: from the `## Conformance` heading to the
    // next `## ` heading (or EOF).
    let Some(start) = chapter_md.find("## Conformance") else {
        return Vec::new();
    };
    let section = &chapter_md[start..];
    let body_after = &section["## Conformance".len()..];
    let end = body_after.find("\n## ").map(|i| i + "## Conformance".len());
    let section = match end {
        Some(e) => &section[..e],
        None => section,
    };

    let mut out = Vec::new();
    // Extract backticked tokens; keep those that look like repo-relative
    // `examples/…` or `tests/…` paths.
    for raw in section.split('`').skip(1).step_by(2) {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        // A citation may be just a path, or a path with trailing prose stripped
        // by the backticks; take the first whitespace-delimited word.
        let word = token.split_whitespace().next().unwrap_or(token);
        if !(word.starts_with("examples/") || word.starts_with("tests/")) {
            continue;
        }
        if !repo_root.join(word).exists() {
            out.push(word.to_string());
        }
    }
    out
}

// ─── Group 7.1: grammar-rule coverage ────────────────────────────────────────

/// LSPEC §7.1: every named `grammar.js` rule appears verbatim in
/// `docs/content/spec/grammar.md`. A new grammar rule MUST gain a production in
/// the spec grammar chapter (and a semantics chapter update if behavior changed).
///
/// RED until Phase 2 writes `docs/content/spec/grammar.md`.
#[test]
fn grammar_rules_are_covered_by_spec() {
    let grammar_js =
        std::fs::read_to_string(repo_root().join("tree-sitter-ascript/grammar.js")).unwrap();
    let rules = grammar_rule_names(&grammar_js);

    // Sanity floor: a broken extractor can't false-green by returning [].
    assert!(
        rules.len() >= 100,
        "grammar_rule_names extracted only {} rules (expected >= 100) — the extractor is broken; \
         a false-empty rule set would vacuously 'cover' the spec",
        rules.len()
    );

    let grammar_md = std::fs::read_to_string(repo_root().join("docs/content/spec/grammar.md"))
        .unwrap_or_default();
    let missing = uncovered_rules(&rules, &grammar_md);

    assert!(
        missing.is_empty(),
        "grammar.js rules absent from docs/content/spec/grammar.md ({} of {}):\n  {}\n\
         a new grammar rule needs a spec/grammar.md production — and a semantics chapter \
         update if behavior changed (CLAUDE.md 'Touching syntax')",
        missing.len(),
        rules.len(),
        missing.join("\n  ")
    );
}

// ─── Group 7.2: chapter manifest + citation existence ────────────────────────

/// LSPEC §7.2: all 16 spec chapters exist, are non-stub, and carry a
/// `## Conformance` section.
///
/// RED until Phase 2 writes the chapters under `docs/content/spec/`.
#[test]
fn spec_chapters_exist_with_conformance_sections() {
    let spec_dir = repo_root().join("docs/content/spec");
    let violations = chapter_violations(&spec_dir, &SPEC_CHAPTERS);
    assert!(
        violations.is_empty(),
        "spec chapter manifest violations ({}):\n  {}",
        violations.len(),
        violations.join("\n  ")
    );
}

/// LSPEC §7.2: every `examples/…`/`tests/…` path cited in an EXISTING chapter's
/// `## Conformance` section resolves on disk. Vacuously green until Phase 2
/// writes the chapters (it iterates only chapters that exist), then load-bearing.
#[test]
fn spec_citations_resolve() {
    let spec_dir = repo_root().join("docs/content/spec");
    let mut dead: Vec<String> = Vec::new();
    for slug in SPEC_CHAPTERS {
        let path = spec_dir.join(format!("{slug}.md"));
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue; // chapter not written yet — covered by the manifest test
        };
        for cite in dead_citations(&body, repo_root()) {
            dead.push(format!("{slug}.md cites missing `{cite}`"));
        }
    }
    assert!(
        dead.is_empty(),
        "dead spec citations ({}):\n  {}",
        dead.len(),
        dead.join("\n  ")
    );
}

// ─── Mutation self-test (anti-false-green) ───────────────────────────────────

/// Proves all three pure helpers actually detect drift. Must PASS even while the
/// real tests above are RED (it uses synthetic inputs, never the real spec dir).
#[test]
fn spec_drift_helpers_catch_mutations() {
    // (a) uncovered_rules: a synthetic grammar with one extra rule vs a spec text
    //     lacking it → reports EXACTLY that rule.
    let synthetic_grammar = "\
module.exports = grammar({
  rules: {
    source_file: $ => repeat($._item),
    widget_decl: $ => seq('widget', $.identifier),
    identifier: $ => /[a-z]+/,
  }
});
";
    let rules = grammar_rule_names(synthetic_grammar);
    assert_eq!(
        rules,
        vec![
            "source_file".to_string(),
            "widget_decl".to_string(),
            "identifier".to_string()
        ],
        "grammar_rule_names should extract exactly the three four-space rules"
    );
    // NOTE: the spec text must NOT mention the omitted rule anywhere (even in
    // prose) — `uncovered_rules` is a verbatim `contains` check.
    let spec_text = "Productions: `source_file`, `identifier`. (one rule deliberately omitted.)";
    let uncovered = uncovered_rules(&rules, spec_text);
    assert_eq!(
        uncovered,
        vec!["widget_decl".to_string()],
        "uncovered_rules must report exactly the one rule missing from the spec text"
    );

    // (b) dead_citations: a synthetic chapter citing a missing example → reports it.
    let chapter = "\
# Some chapter

Body text.

## Conformance

Verified against `examples/does_not_exist.as` and `examples/`. The README is not a citation.
";
    let dead = dead_citations(chapter, repo_root());
    assert_eq!(
        dead,
        vec!["examples/does_not_exist.as".to_string()],
        "dead_citations must report the missing example path (and only it)"
    );

    // (c) chapter_violations: a manifest naming a missing chapter → reports it.
    let empty_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("spec_drift_selftest_nonexistent_dir");
    let violations = chapter_violations(&empty_dir, &["ghost"]);
    assert_eq!(
        violations.len(),
        1,
        "chapter_violations must report the one missing chapter, got: {violations:?}"
    );
    assert!(
        violations[0].starts_with("ghost: missing file"),
        "violation must name the missing chapter: {}",
        violations[0]
    );
}
