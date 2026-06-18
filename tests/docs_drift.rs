//! DOCS — permanent docs-drift tripwires (spec: 2026-06-12-docs-reconciliation-design.md §5).
//!
//! Six mechanical assertions binding the CLI surface, env vars, stdlib module
//! coverage, NAV reachability, in-content links, and editor-pin consistency to the
//! docs. House pattern: tests-as-gates (`tests/srv_negative_space.rs`). Fix the
//! DOCS, never the assertion; allowlist additions require an owner-justified comment.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// ─── Tripwire 1 ─────────────────────────────────────────────────────────────

/// Tripwire 1 (spec §5.1): every clap subcommand and long flag is documented in
/// docs/content/cli.md. Surface comes from clap INTROSPECTION (cli_surface::cli_command),
/// never from parsing main.rs or --help text. Under --no-default-features the
/// cfg-gated subcommands vanish from the surface — the subset assertion stays valid.
///
/// Phase 2 Task 2.1 complete: cli.md brought to full parity.
#[test]
fn every_cli_subcommand_and_long_flag_is_documented() {
    /// Owner-justified exemptions (spec §5.1: starts EMPTY; an entry needs a
    /// justification comment or the reviewer rejects it).
    const FLAG_ALLOWLIST: &[(&str, &str)] = &[];

    let cli_md = read("docs/content/cli.md");
    let cmd = ascript::cli_surface::cli_command();
    let mut missing: Vec<String> = Vec::new();

    for sub in cmd.get_subcommands() {
        let name = sub.get_name();
        if !cli_md.contains(&format!("ascript {name}")) {
            missing.push(format!("subcommand `ascript {name}`"));
        }
        for arg in sub.get_arguments() {
            if arg.get_id() == "help" {
                continue; // clap auto-arg
            }
            let Some(long) = arg.get_long() else { continue }; // positionals
            if FLAG_ALLOWLIST.iter().any(|(s, f)| *s == name && *f == long) {
                continue;
            }
            let needle = format!("--{long}");
            if !cli_md.contains(&needle) {
                missing.push(format!("`ascript {name}` flag `{needle}`"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "docs/content/cli.md is missing {} CLI surface item(s) — document each \
         (cross-link depth pages per spec §4.2) rather than allowlisting:\n  {}",
        missing.len(),
        missing.join("\n  ")
    );
}

// ─── Tripwire 2 helpers ──────────────────────────────────────────────────────

/// Recursively collect .rs files under src/ (dependency-free walkdir).
fn rust_files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files_under(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Extract every ASCRIPT_[A-Z0-9_]+ token from `text` (hand-rolled — no regex dep,
/// so this runs under --no-default-features).
fn ascript_env_vars_in(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("ASCRIPT_") {
        let tail = &rest[i..];
        let len = tail
            .char_indices()
            .find(|(_, c)| !(c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_'))
            .map(|(j, _)| j)
            .unwrap_or(tail.len());
        found.push(tail[..len].trim_end_matches('_').to_string());
        rest = &tail[len..];
    }
    found.sort();
    found.dedup();
    found
}

// ─── Tripwire 2 ─────────────────────────────────────────────────────────────

/// Tripwire 2 (spec §5.2): every ASCRIPT_* env var named in src/ appears in
/// docs/content/cli.md (the Environment variables section) unless allowlisted.
///
/// Phase 2 Task 2.2 complete: env-var table added to cli.md.
#[test]
fn every_env_var_in_src_is_documented_in_cli_md() {
    // Owner-justified allowlist (spec §5.2). The three prefixes are RESERVED for
    // test fixtures — a user-facing var must never use them (it would hide here).
    const ALLOW_PREFIXES: &[(&str, &str)] = &[
        ("ASCRIPT_TEST", "opt-in integration-test fixtures (e.g. live Postgres/Redis \
          DSNs, read only inside #[cfg(test)] modules — src/stdlib/redis.rs, \
          postgres.rs); developer-facing, documented in stdlib/db.md, not user CLI surface"),
        ("ASCRIPT_DOTENV", "std/env dotenv unit-test fixture keys (src/stdlib/env.rs)"),
        ("ASCRIPT_E2E", "end-to-end test fixture keys (src/stdlib/env.rs)"),
        ("ASCRIPT_RT_SIGNING_KEY", "RT §5.1 / Task 11 — the ed25519 release-signing \
          private seed, a CI SECRET (release-rt.yml). It is NEVER read by the runtime via \
          std::env (the release script writes it to a file and passes --key <path>); it \
          appears only in doc-comments + the release runbook in CONTRIBUTING.md. Release \
          infrastructure, not user CLI surface."),
    ];

    let cli_md = read("docs/content/cli.md");
    let mut files = Vec::new();
    rust_files_under(&repo_root().join("src"), &mut files);

    let mut vars: Vec<String> = Vec::new();
    for f in &files {
        vars.extend(ascript_env_vars_in(&fs::read_to_string(f).unwrap()));
    }
    vars.sort();
    vars.dedup();

    let missing: Vec<&String> = vars
        .iter()
        .filter(|v| !ALLOW_PREFIXES.iter().any(|(p, _)| v.starts_with(p)))
        .filter(|v| !cli_md.contains(v.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/content/cli.md's Environment variables section is missing: {missing:?} \
         — add each to the env-var table (spec §4.2 item 4), or allowlist WITH an \
         owner-justified comment if genuinely internal"
    );
}

/// Unit-test the extractor: trailing `_` trimmed, duplicates deduped.
#[test]
fn ascript_env_var_extractor_trims_trailing_underscore() {
    let got = ascript_env_vars_in("x ASCRIPT_FOO_BAR=1 ASCRIPT_TEST_ENV_ y");
    assert_eq!(got, vec!["ASCRIPT_FOO_BAR", "ASCRIPT_TEST_ENV"]);
}

// ─── Tripwire 3 ─────────────────────────────────────────────────────────────

/// Spec §5.3: the authoritative module→owning-page mapping (also pointed at by
/// CLAUDE.md's stdlib-docs instruction). EVERY std module has exactly one owner;
/// cross-references from other pages are fine and unchecked.
const MODULE_PAGES: &[(&str, &str)] = &[
    ("std/ai", "ai.md"),
    ("std/assert", "assert.md"),
    ("std/bench", "bench.md"),
    ("std/cli", "cli.md"),
    ("std/color", "cli.md"),
    ("std/decimal", "data.md"),
    ("std/math", "collections.md"),
    ("std/string", "collections.md"),
    ("std/array", "collections.md"),
    ("std/object", "collections.md"),
    ("std/map", "collections.md"),
    ("std/schema", "schema.md"),
    ("std/shared", "shared.md"),
    ("std/set", "collections.md"),
    ("std/lru", "utilities.md"),
    ("std/events", "utilities.md"),
    ("std/template", "utilities.md"),
    ("std/bytes", "collections.md"),
    ("std/caps", "caps.md"),
    ("std/convert", "collections.md"),
    ("std/task", "async.md"),
    ("std/time", "time.md"),
    ("std/sync", "async.md"),
    ("std/stream", "stream.md"),
    ("std/date", "time.md"),
    ("std/intl", "time.md"),
    ("std/json", "data.md"),
    ("std/log", "log.md"),
    ("std/workflow", "workflow.md"),
    ("std/telemetry", "telemetry.md"),
    ("std/encoding", "data.md"),
    ("std/crypto", "system.md"),
    ("std/compress", "system.md"),
    ("std/env", "system.md"),
    ("std/fs", "system.md"),
    ("std/os", "system.md"),
    ("std/io", "system.md"),
    ("std/process", "system.md"),
    ("std/net", "net.md"),
    ("std/net/tcp", "net.md"),
    ("std/net/http", "net.md"),
    ("std/http/server", "net.md"),
    ("std/net/udp", "net.md"),
    ("std/net/ws", "net.md"),
    ("std/regex", "data.md"),
    ("std/sqlite", "db.md"),
    ("std/postgres", "db.md"),
    ("std/redis", "db.md"),
    ("std/url", "data.md"),
    ("std/uuid", "data.md"),
    ("std/csv", "data.md"),
    ("std/toml", "data.md"),
    ("std/yaml", "data.md"),
    ("std/msgpack", "data.md"),
    ("std/cbor", "data.md"),
    ("std/tui", "tui.md"),
    ("std/ffi", "ffi.md"),
    ("std/resilience", "resilience.md"),
];

/// Pure checker (mutation-self-testable): validates a mapping against a module
/// list and a page→content map. Returns human-readable violations.
fn check_module_pages(
    mapping: &[(&str, &str)],
    std_modules: &[&str],
    pages: &std::collections::BTreeMap<String, String>, // filename → content
) -> Vec<String> {
    let mut v = Vec::new();
    // (a) bijection on the module side: mapping keys == STD_MODULES, exactly once.
    for m in std_modules {
        match mapping.iter().filter(|(k, _)| k == m).count() {
            1 => {}
            0 => v.push(format!("{m}: in STD_MODULES but has NO owning docs page — \
                 add a stdlib reference section and a MODULE_PAGES entry")),
            n => v.push(format!("{m}: claimed by {n} mapping entries")),
        }
    }
    for (m, _) in mapping {
        if !std_modules.contains(m) {
            v.push(format!("{m}: in MODULE_PAGES but not in STD_MODULES"));
        }
    }
    // (b)+(c) the owning page exists and actually discusses the module.
    for (m, page) in mapping {
        match pages.get(*page) {
            None => v.push(format!("{m}: owning page {page} does not exist")),
            Some(text) if !text.contains(m) => {
                v.push(format!("{m}: owning page {page} never mentions `{m}`"))
            }
            _ => {}
        }
    }
    // (d) reverse: every reference page (except overview.md) owns ≥1 module.
    for page in pages.keys() {
        if page == "overview.md" {
            continue;
        }
        if !mapping.iter().any(|(_, p)| p == page) {
            v.push(format!("{page}: stdlib reference page owns no module — orphan?"));
        }
    }
    v
}

#[test]
fn every_std_module_is_claimed_by_exactly_one_stdlib_page() {
    let dir = repo_root().join("docs/content/stdlib");
    let mut pages = std::collections::BTreeMap::new();
    for entry in fs::read_dir(&dir).expect("stdlib docs dir") {
        let p = entry.expect("entry").path();
        if p.extension().is_some_and(|e| e == "md") {
            pages.insert(
                p.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read_to_string(&p).unwrap(),
            );
        }
    }
    let violations = check_module_pages(MODULE_PAGES, ascript::stdlib::STD_MODULES, &pages);
    assert!(violations.is_empty(), "module→page drift:\n  {}", violations.join("\n  "));
}

/// Anti-false-green (spec §5.7): the checker reports each violation class.
#[test]
fn module_page_checker_catches_each_violation_class() {
    let mut pages = std::collections::BTreeMap::new();
    pages.insert("a.md".to_string(), "docs for std/x".to_string());
    pages.insert("orphan.md".to_string(), "nothing".to_string());
    let mapping = &[("std/x", "a.md"), ("std/gone", "missing.md"), ("std/quiet", "a.md")];
    let v = check_module_pages(mapping, &["std/x", "std/new", "std/gone", "std/quiet"], &pages);
    assert!(v.iter().any(|s| s.contains("std/new")), "unmapped module: {v:?}");
    assert!(v.iter().any(|s| s.contains("missing.md")), "missing page: {v:?}");
    assert!(v.iter().any(|s| s.contains("std/quiet")), "page never mentions: {v:?}");
    assert!(v.iter().any(|s| s.contains("orphan.md")), "orphan page: {v:?}");
}

// ─── Tripwire 4 ─────────────────────────────────────────────────────────────

/// Tolerant NAV slug extraction (spec §5.4): the text between `const NAV = [` and
/// the next `];`, scanning for the first single-quoted string of each `['slug', …]`
/// pair. Survives reformatting; breaks loudly (panic) if NAV is renamed/moved.
fn nav_slugs(app_js: &str) -> Vec<String> {
    let start = app_js.find("const NAV = [").expect(
        "docs/assets/app.js: `const NAV = [` not found — if NAV was renamed, update \
         tests/docs_drift.rs AND CLAUDE.md's docs guidance together",
    );
    let body = &app_js[start..];
    let end = body.find("];").expect("NAV array not terminated");
    let body = &body[..end];
    let mut slugs = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("['") {
        let tail = &rest[i + 2..];
        let Some(q) = tail.find('\'') else { break };
        slugs.push(tail[..q].to_string());
        rest = &tail[q..];
    }
    slugs
}

fn content_page_slugs() -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("docs/content walk") {
            let p = entry.expect("entry").path();
            if p.is_dir() {
                walk(&p, root, out);
            } else if p.extension().is_some_and(|e| e == "md") {
                let rel = p.strip_prefix(root).unwrap().with_extension("");
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let root = repo_root().join("docs/content");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out
}

#[test]
fn nav_and_content_files_are_a_bijection() {
    let mut nav = nav_slugs(&read("docs/assets/app.js"));
    let mut files = content_page_slugs();
    nav.sort();
    files.sort();
    let orphans: Vec<_> = files.iter().filter(|f| !nav.contains(f)).collect();
    let dangling: Vec<_> = nav.iter().filter(|n| !files.contains(n)).collect();
    assert!(
        orphans.is_empty() && dangling.is_empty(),
        "NAV ⇄ docs/content drift — pages NOT in NAV (unreachable: no sidebar, no \
         cmd-K hit; add to the NAV array in docs/assets/app.js): {orphans:?}; NAV \
         entries with no file: {dangling:?}"
    );
}

#[test]
fn nav_parser_catches_a_missing_slug() {
    let fake = "const NAV = [ { items: [ ['cli', 'CLI'], ['runtime', 'R'] ] } ];";
    assert_eq!(nav_slugs(fake), vec!["cli", "runtime"]);
}

// ─── Tripwire 5 ─────────────────────────────────────────────────────────────

/// Extract `](target)` link targets from markdown (skips http/https/mailto and
/// pure-anchor links; strips trailing #anchor).
fn md_link_targets(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("](") {
        let tail = &rest[i + 2..];
        let Some(close) = tail.find(')') else { break };
        let raw = &tail[..close];
        rest = &tail[close..];
        if raw.starts_with("http://") || raw.starts_with("https://")
            || raw.starts_with("mailto:") || raw.starts_with('#') || raw.contains(' ')
        {
            continue;
        }
        let path = raw.split('#').next().unwrap();
        if !path.is_empty() {
            out.push(path.to_string());
        }
    }
    out
}

/// Resolve a link from `page_dir` (content-root-relative) per the documented rule.
/// Returns the content-root-relative target path (lexically normalized).
fn resolve_doc_link(page_dir: &str, link: &str) -> String {
    let joined = if let Some(abs) = link.strip_prefix('/') {
        abs.to_string()
    } else if page_dir.is_empty() {
        link.to_string()
    } else {
        format!("{page_dir}/{link}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for c in joined.split('/') {
        match c {
            "" | "." => {}
            ".." => { parts.pop(); }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

#[test]
fn every_in_content_relative_link_resolves() {
    let root = repo_root().join("docs/content");
    let mut broken = Vec::new();
    for slug in content_page_slugs() {
        let page = root.join(format!("{slug}.md"));
        let dir = slug.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        for link in md_link_targets(&fs::read_to_string(&page).unwrap()) {
            let target = resolve_doc_link(dir, &link);
            let as_page = root.join(format!("{target}.md"));
            let as_asset = root.join(&target); // images/files keep their extension
            if !as_page.exists() && !as_asset.exists() {
                broken.push(format!("{slug}.md → ({link}) resolves to {target} (missing)"));
            }
        }
    }
    assert!(broken.is_empty(), "broken in-content links:\n  {}", broken.join("\n  "));
}

#[test]
fn link_resolution_follows_the_documented_rule() {
    // CLAUDE.md: links resolve relative to the current page's directory.
    assert_eq!(resolve_doc_link("stdlib", "workflow"), "stdlib/workflow");
    assert_eq!(resolve_doc_link("stdlib", "../language/syntax"), "language/syntax");
    assert_eq!(resolve_doc_link("language", "errors"), "language/errors");
    assert_eq!(resolve_doc_link("", "runtime"), "runtime");
    assert_eq!(resolve_doc_link("tooling", "/cli"), "cli"); // root-absolute
}

// ─── Tripwire 6 ─────────────────────────────────────────────────────────────

/// Tripwire 6 (spec §5.6): the zed and nvim grammar pins must agree. Pin CURRENCY
/// against the ascript-lang/tree-sitter-ascript mirror is NOT in-repo testable
/// (network; other repo) — that half is a documented manual checklist item in
/// CONTRIBUTING.md / CLAUDE.md ("Publishing the grammar"), not a silent gap.
#[test]
fn editor_grammar_pins_agree() {
    fn quoted_after<'a>(text: &'a str, key: &str, file: &str) -> &'a str {
        let i = text.find(key).unwrap_or_else(|| panic!("{file}: `{key}` not found"));
        let tail = &text[i + key.len()..];
        let start = tail.find('"').unwrap_or_else(|| panic!("{file}: no quote after {key}")) + 1;
        let end = start + tail[start..].find('"').expect("closing quote");
        &tail[start..end]
    }
    let zed = read("editors/zed/extension.toml");
    let nvim = read("editors/nvim/lua/ascript/treesitter.lua");
    let zed_rev = quoted_after(&zed, "rev = ", "editors/zed/extension.toml");
    let nvim_rev = quoted_after(&nvim, "GRAMMAR_REV = ", "editors/nvim/.../treesitter.lua");
    assert_eq!(
        zed_rev, nvim_rev,
        "editor grammar pins disagree (a half-done pin bump): bump BOTH per the \
         CLAUDE.md 'Publishing the grammar' checklist"
    );
    assert!(
        zed_rev.len() == 40 && zed_rev.chars().all(|c| c.is_ascii_hexdigit()),
        "pin is not a 40-hex SHA: {zed_rev}"
    );
}
