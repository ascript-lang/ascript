# Documentation Reconciliation + Permanent Drift Tripwires (DOCS) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. At the end of each
> phase, a **holistic per-phase review subagent** reviews the phase's combined changes before
> the next phase starts. A task/phase is closed only when every box under it is ticked.

**Goal:** Bring `docs/content/cli.md` to full CLI parity (every subcommand, every long flag, a
consolidated environment-variable reference), close the verified stdlib-reference member gaps,
fix the `CLAUDE.md` domain-grouping meta-drift, re-verify the tooling pages + README — and make
all of it PERMANENT with six in-tree drift tripwires (`tests/docs_drift.rs`) written FIRST,
observed failing on today's known gaps, made green by the reconciliation (TDD at the docs
level). Adds the "docs drift tripwires stay green in CI" gate to `goal-perf.md`.

**Spec:** `superpowers/specs/2026-06-12-docs-reconciliation-design.md` (DOCS). Read it first;
section references (§) below are into it. The spec is self-contained — every audit finding is
inlined there with file:line on both sides; never consult an external audit file.

**Architecture:** One behavior-identical code move — the clap derive types (`Cli`, `Command`,
`CapFlags`) relocate verbatim from `src/main.rs` into a new library module
`src/cli_surface.rs` exporting `cli_command() -> clap::Command`, so the integration test can
introspect the real CLI surface (`get_subcommands`/`get_arguments`/`get_long`) instead of
parsing source or help text (§4.1). All six tripwires live in ONE new integration-test file
`tests/docs_drift.rs` (repo-rooted via `env!("CARGO_MANIFEST_DIR")`, the
`tests/srv_negative_space.rs` idiom), dependency-free (hand-rolled scanning — no regex/walkdir
crates, so it runs under `--no-default-features` too). Each green-at-birth tripwire's checking
logic is a pure helper exercised by a deliberate-mutation self-test (§5.7 anti-false-green).
Unit A is pure Markdown/`CLAUDE.md` editing.

**Tech stack:** Rust integration tests (`std::fs` walks, string scanning, clap-4
introspection); Markdown under `docs/content/`; the docs site is static
(`docs/assets/app.js` renders the Markdown — pages must stay served-site-compatible:
`cd docs && python3 -m http.server`).

**SIG boundary (binding, §3):** DOCS fills member EXISTENCE + one-line descriptions and owns
module-level claiming, CLI/env-var/page currency, NAV, links, pins. Per-function SIGNATURE
consistency on stdlib pages is SIG's (`2026-06-12-lsp-stdlib-signatures-design.md` §2.3 drift
test b). New stdlib doc entries are written in the owning page's EXISTING style so SIG's
tolerant matcher parses them as one more fact. The two specs are order-independent; do not
add signature-validation logic here.

**Sequencing:** owner-decided; no technical dependency on any engine spec (the spec header
records this). SIG and DOCS are mutually independent.

**Red-branch discipline (explicit):** Phase 1 commits tripwires that are deliberately RED on
today's tree (tripwires 1–2). From the end of Phase 1 until the end of Phase 2 the feature
branch is allowed to be red **only** on `tests/docs_drift.rs` tripwires 1–2 — everything else
(full suite, clippy, both configs) stays green at every commit. The merge gate (Phase 3)
requires everything green. Record each observed-red output verbatim in the task log.

**Binding execution standards (production-grade mandate):** any bug found while working — ours
or pre-existing, direct or incidental — is fixed in-branch with a failing-test-first regression
guard, never stepped around (goal.md Gate 14). No placeholders, no silent deferrals. Commit per
task with the house trailer:
`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

## File structure

**New files:**
- `src/cli_surface.rs` — the clap derive types moved verbatim + `pub fn cli_command()`.
- `tests/docs_drift.rs` — the six tripwires + pure check-helpers + mutation self-tests +
  the checked-in module→page mapping.

**Modified files:**
- `src/main.rs` — derive types removed; `use ascript::cli_surface::{Cli, Command, CapFlags};`.
- `src/lib.rs` — `pub mod cli_surface;` declaration.
- `docs/content/cli.md` — full-parity rewrite + the "Environment variables" section.
- `docs/content/stdlib/async.md` — `task.pipe` entry (+ anything the re-run sweep finds).
- `CLAUDE.md` — the §4.4 wording fix + tripwire mentions in the docs guidance.
- `CONTRIBUTING.md` — the editor-pin manual-checklist line (§5.6).
- `docs/content/tooling/{lsp-capabilities,debugging-profiling,editor-setup}.md`, `README.md`,
  `docs/content/getting-started.md` — only as the §4.5 re-verification requires.
- `goal-perf.md` (gate 19 + DX-track DOCS entry + status), `superpowers/roadmap.md` — Phase 3.

---

## Phase 0 — Preflight: citation re-verification + baseline

### Task 0.1: re-verify the spec's grounding and baseline the tree

**Files:** none (verification only; findings recorded in the task log).

- [ ] **Step 1 — citation re-grep** (line numbers may have drifted since spec time; record
  ACTUAL numbers and use them throughout):
  `grep -n "enum Command" src/main.rs` (expect ~`:14`); `grep -n "struct CapFlags" src/main.rs`
  (~`:267`); `grep -n "pub const STD_MODULES" src/stdlib/mod.rs` (~`:221`, count the entries —
  expect 57; if it grew, the §5.3 mapping gains the new module in Task 1.4);
  `grep -rno 'ASCRIPT_[A-Z_0-9]*' src/ --include='*.rs' | awk -F: '{print $3}' | sort -u`
  (expect the §1.2 set); `grep -n "const NAV" docs/assets/app.js` (~`:11`);
  `grep -n 'rev = ' editors/zed/extension.toml` + `grep -n 'GRAMMAR_REV' editors/nvim/lua/ascript/treesitter.lua`;
  `grep -n "stdlib reference pages mirror" CLAUDE.md` (~`:31`);
  `grep -n "fn server_capabilities" src/lsp/server.rs` (~`:224`).
- [ ] **Step 2 — confirm the known gaps still hold** (they are the TDD-red targets): for each
  of `--strip --native --parallel --coverage --filter --update-snapshots --inspect --profile
  --locked --sandbox --deny-net --deny-fs`, confirm zero literal occurrences in
  `docs/content/cli.md`; confirm `ascript dap` absent; confirm `ASCRIPT_NO_SPECIALIZE` has
  zero hits under `docs/` and `README.md`; confirm `task.pipe` absent from
  `docs/content/stdlib/async.md`. If any was fixed since spec time, record it and drop the
  corresponding Phase-2 sub-item (never re-add prose that exists).
- [ ] **Step 3 — baseline runs:** `cargo test` green; `cargo test --no-default-features`
  green; `cargo clippy --all-targets` + `cargo clippy --no-default-features --all-targets`
  clean; `cargo test --test vm_differential` green (the BEFORE half of the
  no-engine-surface proof). Save `target/debug/ascript --help` and every
  `ascript <sub> --help` output to the task log (the §4.1 identity baseline).
- [ ] **Step 4:** create the feature branch `feat/docs-drift-tripwires` off `main`.

### Task 0.2: Phase 0 review

- [ ] **Step 1:** Independent reviewer re-runs Steps 1–3, confirms recorded numbers match the
  tree and the gap list is current. Any mismatch corrected in the task log before Phase 1.

---

## Phase 1 — Unit B: the tripwires (written first, observed red)

### Task 1.1: extract the clap surface into `src/cli_surface.rs` (behavior-identical)

**Files:** create `src/cli_surface.rs`; modify `src/main.rs`, `src/lib.rs`.

- [ ] **Step 1: Write the failing test** — in `src/cli_surface.rs` (fails to compile until
  the module exists; that IS the red step for a pure move):

```rust
#[cfg(test)]
mod tests {
    /// clap's structural self-check: catches conflicting/invalid arg definitions
    /// that only surface at parse time. Also pins the surface seam the docs-drift
    /// tripwire introspects.
    #[test]
    fn cli_command_is_structurally_valid() {
        super::cli_command().debug_assert();
    }

    /// The seam exposes the real subcommand set (feature-dependent: doc/lsp/dap/pkg
    /// variants are cfg-gated — this test asserts only the unconditional core).
    #[test]
    fn cli_command_has_the_core_subcommands() {
        let cmd = super::cli_command();
        let names: Vec<&str> = cmd.get_subcommands().map(|s| s.get_name()).collect();
        for core in ["run", "build", "repl", "fmt", "check", "test"] {
            assert!(names.contains(&core), "core subcommand '{core}' missing: {names:?}");
        }
    }
}
```

- [ ] **Step 2: Implement the move.** Cut `Cli` (`src/main.rs:7-12`), `Command`
  (`:14-261`), and `CapFlags` (`:267-289`) **verbatim** — including every doc comment and
  every `#[cfg(feature = …)]` attribute — into `src/cli_surface.rs` with a module doc
  explaining the seam (single source of truth for the CLI surface; consumed by `main.rs`
  for parsing and `tests/docs_drift.rs` for introspection — spec §4.1). Make the types
  `pub` (they cross the crate boundary now) and add:

```rust
/// The full clap command tree — the single source of truth for the CLI surface.
pub fn cli_command() -> clap::Command {
    use clap::CommandFactory;
    Cli::command()
}
```

  In `src/lib.rs` declare `pub mod cli_surface;` (unconditional — clap is an unconditional
  dependency, `Cargo.toml:52`). In `src/main.rs` replace the definitions with
  `use ascript::cli_surface::{CapFlags, Cli, Command};` — `compose_caps`, `run_profiled`,
  `try_run_embedded`, `real_main`, and all handlers stay in `main.rs` untouched
  (`src/pkg/` stays binary-side per SP6; the pkg enum variants carry only `String`s).
- [ ] **Step 3: Run — green:** `cargo test cli_surface` passes; the whole suite + clippy
  green in BOTH feature configs (the cfg-gated variants must compile under
  `--no-default-features` from the lib too — if a variant references a binary-only type,
  that is a design break: STOP and re-verify §4.1's claim that none do).
- [ ] **Step 4: Identity proof:** diff `ascript --help` and every `ascript <sub> --help`
  against the Task 0.1 Step-3 baseline — **byte-identical**. Record the diff (empty) in
  the task log.
- [ ] **Step 5:** Commit: `refactor(cli): extract clap surface into src/cli_surface.rs
  (introspection seam for docs tripwires, behavior-identical)`.
- [ ] **Step 6 — independent review:** reviewer re-runs Steps 3–4, additionally probes
  `ascript run --help` under `--no-default-features` build (the smaller tree parses), and
  confirms `git diff` shows a pure move (no logic edits inside the derive types).

### Task 1.2: tripwire 1 — CLI surface ⊆ `cli.md` (RED today)

**Files:** create `tests/docs_drift.rs`.

- [ ] **Step 1: Write the tripwire** (real code; the file's shared helpers are born here):

```rust
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

/// Tripwire 1 (spec §5.1): every clap subcommand and long flag is documented in
/// docs/content/cli.md. Surface comes from clap INTROSPECTION (cli_surface::cli_command),
/// never from parsing main.rs or --help text. Under --no-default-features the
/// cfg-gated subcommands vanish from the surface — the subset assertion stays valid.
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
```

- [ ] **Step 2: Run — expect RED.** `cargo test --test docs_drift
  every_cli_subcommand_and_long_flag_is_documented` fails. Verify the failure lists
  EXACTLY the spec §1.1 inventory (the 27 flags across run/build/test/lsp + `--stdio` on
  dap, the `ascript dap` subcommand, and the 7 pkg subcommands; `doc --open` if Step 0.1
  confirmed it). Paste the full failure output into the task log — this is the audit's
  finding reproduced mechanically.
- [ ] **Step 3:** Run the same under `--no-default-features` (`cargo test
  --no-default-features --test docs_drift …`) — still red (the core subcommands' flags
  alone are missing), proving the tripwire is feature-config-safe.
- [ ] **Step 4:** Commit (red on this test only, per the red-branch discipline):
  `test(docs): tripwire 1 — clap surface ⊆ cli.md (RED: 27 flags + dap + pkg subcommands
  undocumented)`.
- [ ] **Step 5 — independent review:** reviewer confirms the red list matches §1.1 item for
  item; probes that adding a fake flag to a scratch build would be caught (mental or local
  experiment); confirms no allowlist entries snuck in.

### Task 1.3: tripwire 2 — env vars ⊆ `cli.md` env section (RED today)

**Files:** modify `tests/docs_drift.rs`.

- [ ] **Step 1: Write the tripwire:**

```rust
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

/// Tripwire 2 (spec §5.2): every ASCRIPT_* env var named in src/ appears in
/// docs/content/cli.md (the Environment variables section) unless allowlisted.
#[test]
fn every_env_var_in_src_is_documented_in_cli_md() {
    // Owner-justified allowlist (spec §5.2). The three prefixes are RESERVED for
    // test fixtures — a user-facing var must never use them (it would hide here).
    const ALLOW_PREFIXES: &[(&str, &str)] = &[
        ("ASCRIPT_TEST", "opt-in integration-test fixtures (e.g. live Postgres/Redis \
          DSNs, read only inside #[cfg(test)] modules — src/stdlib/redis.rs:268, \
          postgres.rs:460); developer-facing, documented in stdlib/db.md, not user CLI surface"),
        ("ASCRIPT_DOTENV", "std/env dotenv unit-test fixture keys (src/stdlib/env.rs)"),
        ("ASCRIPT_E2E", "end-to-end test fixture keys (src/stdlib/env.rs)"),
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
```

- [ ] **Step 2: Run — expect RED** listing exactly `ASCRIPT_CACHE`, `ASCRIPT_DENY`,
  `ASCRIPT_ENGINE`, `ASCRIPT_LOG`, `ASCRIPT_NO_SPECIALIZE`, `ASCRIPT_UPDATE_SNAPSHOTS`,
  `ASCRIPT_WORKERS` (spec §1.2). Note: under `--no-default-features` `src/pkg/` is still
  scanned (the grep is textual, not cfg-aware — by design, §5.2), so the list is identical
  in both configs. Paste the output into the task log.
- [ ] **Step 3:** Unit-test the extractor in the same file (it is a pure helper):
  `ascript_env_vars_in("x ASCRIPT_FOO_BAR=1 ASCRIPT_TEST_ENV_ y")` →
  `["ASCRIPT_FOO_BAR", "ASCRIPT_TEST_ENV"]` (trailing `_` trimmed — fixture prefixes in
  source often end with `_`-joined suffixes). Green.
- [ ] **Step 4:** Commit: `test(docs): tripwire 2 — ASCRIPT_* env vars ⊆ cli.md (RED: 7
  user-facing vars uncentralized; ASCRIPT_NO_SPECIALIZE documented nowhere)`.
- [ ] **Step 5 — independent review:** reviewer re-runs both configs; verifies the
  allowlist justifications against the actual read sites (each cited file:line must be
  inside `mod tests`); confirms the extractor handles an `ASCRIPT_` at end-of-file.

### Task 1.4: tripwire 3 — module→page mapping (green at birth + mutation self-test)

**Files:** modify `tests/docs_drift.rs`.

- [ ] **Step 1: Write the mapping + checker + tests** (the mapping is the spec §5.3 table —
  re-derive against the Task 0.1 `STD_MODULES` count if it changed):

```rust
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
```

- [ ] **Step 2: Run.** The self-test is the red→green TDD half (write it first, watch it
  fail against a stub checker returning `vec![]`, then implement). The main test must be
  **GREEN at birth** (spec §1.5/§5.3) — if it is red, the mapping is wrong or the tree
  moved: re-derive (the §4.3 sweep methodology) and fix the MAPPING, never weaken the
  checker.
- [ ] **Step 3:** Both feature configs green (`STD_MODULES` is feature-independent,
  `src/stdlib/mod.rs:282-284` doc).
- [ ] **Step 4:** Commit: `test(docs): tripwire 3 — STD_MODULES ⇄ stdlib pages mapping
  (green baseline + mutation self-test)`.
- [ ] **Step 5 — independent review:** reviewer spot-verifies 5 random mapping rows against
  page content; temporarily appends a fake module to a local `STD_MODULES` copy…—
  impractical; instead runs the self-test and verifies all four violation classes are
  individually exercised (comment one assertion's setup out → that assertion fails).

### Task 1.5: tripwire 4 — NAV ⇄ files bijection (green at birth + self-test)

**Files:** modify `tests/docs_drift.rs`.

- [ ] **Step 1: Write it** — pure parser + checker + self-test + the real test:

```rust
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
```

  (The bijection self-test is the parser test plus the trivially-verifiable set-diff —
  exercise BOTH failure directions through `check`-style assertions on synthetic inputs
  if the reviewer asks; the parser is the fragile part.)
- [ ] **Step 2: Run — green at birth** (40 ⇄ 40, spec §1.5). Self-test green.
- [ ] **Step 3:** Commit: `test(docs): tripwire 4 — NAV ⇄ docs/content bijection (automates
  the CLAUDE.md manual rule)`.
- [ ] **Step 4 — independent review:** reviewer temporarily creates
  `docs/content/scratch.md` locally → test fails with the orphan message; deletes it;
  confirms the tolerant parse survives an `app.js` reformat (e.g. extra whitespace).

### Task 1.6: tripwire 5 — in-content link checker (green at birth + self-test)

**Files:** modify `tests/docs_drift.rs`.

- [ ] **Step 1: Write it** — resolution per the documented rule (CLAUDE.md:36-37;
  `app.js` `resolveDocHref:81-85` — relative to the current page's directory, leading `/`
  = content-root-absolute):

```rust
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
```

- [ ] **Step 2: Run — green at birth** (134 links, 0 broken — spec §1.5; if red, a link
  broke since spec time: FIX THE LINK, record it). Self-test green.
- [ ] **Step 3:** Commit: `test(docs): tripwire 5 — in-content relative link checker
  (per the documented relative-to-page rule)`.
- [ ] **Step 4 — independent review:** reviewer plants a bogus link locally → named in the
  failure; checks an anchor-bearing link (`](workflow#activities)`) and a root-absolute
  link still pass; confirms the rule test matches `resolveDocHref` semantics
  (`docs/assets/app.js:81-85`).

### Task 1.7: tripwire 6 — editor-pin consistency + the manual-checklist note

**Files:** modify `tests/docs_drift.rs` (the CONTRIBUTING/CLAUDE.md prose lands in
Phase 2 — Task 2.5).

- [ ] **Step 1: Write it:**

```rust
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
```

- [ ] **Step 2: Run — green at birth** (both pins `7227fb7f…`, spec §1.5). Probe the
  extractor against a mutated local copy (mismatched SHA → red) and revert.
- [ ] **Step 3:** Commit: `test(docs): tripwire 6 — zed/nvim grammar pin consistency
  (mirror currency stays a documented manual checklist item)`.
- [ ] **Step 4 — independent review:** reviewer verifies both quoted_after key strings
  against the actual files; confirms the test message points at the checklist.

### Task 1.8: Phase 1 holistic review

- [ ] **Step 1:** Holistic reviewer runs `cargo test --test docs_drift` in both feature
  configs: tripwires 3–6 + all self-tests green; tripwires 1–2 red with EXACTLY the spec
  §1.1/§1.2 inventories (no more, no less — an unexpected extra red line means the tree
  moved or the tripwire over-matches; resolve before Phase 2). Everything else in the
  suite green; clippy clean both configs.
- [ ] **Step 2:** Confirm `tests/docs_drift.rs` has zero feature-gated code and zero new
  dependencies (`git diff main -- Cargo.toml` shows nothing).

---

## Phase 2 — Unit A: the reconciliation sweep (turns tripwires 1–2 green)

### Task 2.1: `cli.md` to full CLI parity

**Files:** `docs/content/cli.md`.

- [ ] **Step 1:** Restructure per spec §4.2: per-command sections in `Command`-enum order
  (`run`, `build`, `repl`, `fmt`, `check`, `doc`, `test`, `lsp`, `dap`, "Package
  management"). Preserve the page's existing good prose (the repl/check/lsp sections are
  accurate — extend, don't rewrite for its own sake).
- [ ] **Step 2:** Document the §1.1 inventory, each flag with 1–3 lines of curated prose
  (NEVER paste clap help verbatim — prose quality is why generation was rejected, §2)
  and a depth cross-link instead of duplicated depth:
  - **run:** `--locked` (→ `packages`), `--deny`/`--sandbox`/`--deny-net`/`--deny-fs`
    (→ `stdlib/caps`), `--inspect` (→ `tooling/debugging-profiling`), `--profile` /
    `--out`/`-o` / `--profile-hz` / `--profile-format` (→ `tooling/debugging-profiling`),
    plus the trailing-args → `env.args()` convention (`src/main.rs:61-64`).
  - **build:** `--strip`, `--native`, `--target` (→ `language/bundles`), the 4 cap flags
    (→ `stdlib/caps`; note the composed set is EMBEDDED in the artifact,
    `src/main.rs:269-274`).
  - **test:** `--parallel[=N]`, `--coverage[=text|lcov|html]`, `--watch`, `--filter`,
    `--update-snapshots`, `--locked`, `--deny`/`--sandbox` — primary documentation HERE
    (no deeper page); mirror the clap doc comments' semantics (`src/main.rs:154-213`):
    parallel isolate semantics + `$ASCRIPT_WORKERS` clamp, coverage VM-only asymmetry,
    watch import-graph scoping + `sys` feature, filter substring-vs-`/regex/`, snapshot
    re-baseline + orphan deletion.
  - **lsp:** the `--stdio` compat no-op (one line).
  - **dap:** NEW section — the standalone DAP server, `--stdio` no-op, the
    `run --inspect` pre-set relationship, the caps posture (`ascript dap` takes no
    sandbox flags; sandboxed debugging uses `run --inspect --sandbox`,
    `src/main.rs:1191-1196`). Cross-link `tooling/debugging-profiling`.
  - **doc:** add `--open` (and `--out`/`--format` if Task 0.1 found them missing).
  - **Package management:** one sentence per subcommand
    (`add`/`remove`/`install [--locked]`/`update`/`lock`/`tree`/`verify`),
    cross-link `packages` for everything deeper.
- [ ] **Step 3: Run tripwire 1 — expect GREEN** in both feature configs. If anything is
  still listed, document it (no allowlisting to make the bar).
- [ ] **Step 4:** Served-site sanity: `cd docs && python3 -m http.server` and load the CLI
  page (the renderer fetches Markdown — check tables/callouts render).
- [ ] **Step 5:** Commit: `docs(cli): full CLI parity — every subcommand + long flag
  documented (tripwire 1 green)`.
- [ ] **Step 6 — independent review:** reviewer cross-reads each new flag description
  against the `src/main.rs` doc comment for semantic fidelity (e.g. `--parallel`'s
  `default_missing_value` auto-N behavior; `--coverage` formats; `--deny-net` modes);
  re-runs tripwire 1; spot-renders the served page.

### Task 2.2: the environment-variable reference section

**Files:** `docs/content/cli.md`.

- [ ] **Step 1:** Append the "Environment variables" section (spec §4.2 item 4): a table of
  the 7 user-facing vars — `ASCRIPT_ENGINE` (→ `runtime`), `ASCRIPT_WORKERS`
  (→ `language/workers`), `ASCRIPT_LOG` (→ `stdlib/log`), `ASCRIPT_CACHE` (→ `packages`),
  `ASCRIPT_DENY` (→ `language/bundles`), `ASCRIPT_UPDATE_SNAPSHOTS` (→ `stdlib/assert`),
  and `ASCRIPT_NO_SPECIALIZE` — its FIRST documentation anywhere: the generic-VM kill
  switch (`=1` disables every VM fast path; behavior byte-identical, speed only — a
  debugging/benchmarking knob presented like `--tree-walker`, grounded on
  `src/lib.rs:2060-2066`). Existing in-context mentions on other pages STAY.
- [ ] **Step 2: Run tripwire 2 — expect GREEN** (both configs). Tripwire 5 still green
  (the new cross-links resolve).
- [ ] **Step 3:** Commit: `docs(cli): consolidated environment-variable reference
  (tripwire 2 green; first docs for ASCRIPT_NO_SPECIALIZE)`.
- [ ] **Step 4 — independent review:** reviewer verifies each row against its read site
  (spec §1.2 table); confirms `ASCRIPT_NO_SPECIALIZE` wording matches the code (value
  `"1"` exactly, `src/lib.rs:2066`).

### Task 2.3: the stdlib member sweep (re-run + fill)

**Files:** `docs/content/stdlib/async.md` (+ any page the re-run implicates).

- [ ] **Step 1: Re-run the sweep** (spec §4.3 methodology — the tree may have moved since
  spec time). Build release, then for each of the 57 modules:

```bash
cat > /tmp/intro.as <<EOF
import * as object from "std/object"
import * as mm from "std/$MOD"
print(object.keys(mm))
EOF
target/release/ascript run /tmp/intro.as
```

  grep each key against the module's OWNING page (the Task 1.4 mapping) in all surveyed
  styles (`mod.key`, backticked heading, named-import example, table row). **Verify every
  candidate miss manually** before calling it a gap (spec §1.3: five of six audit-era
  candidates were style false positives).
- [ ] **Step 2: Fill the verified gap(s).** Known from the spec-time sweep: add
  **`task.pipe(gen, bus)`** to `async.md`'s `std/task` section, in the page's existing
  style (Style-2 backticked heading like `task.retry`), with a one-line description
  (bridges a worker generator stream onto a local `std/events` bus) and a cross-link to
  `../language/workers` for the depth. Any NEW gap the re-run finds is filled in this
  task the same way — list each in the task log with its verification evidence.
  **Do not add signature-table-grade detail** — existence + one line + link (SIG owns
  signatures, see the plan header boundary).
- [ ] **Step 3:** Tripwires 3 + 5 still green; served-site spot-render of each touched page.
- [ ] **Step 4:** Commit: `docs(stdlib): member sweep — task.pipe into the std/task
  reference (+ re-run findings)`.
- [ ] **Step 5 — independent review:** reviewer independently re-runs the sweep for 6
  randomly-chosen modules including std/task; confirms zero unverified misses remain;
  confirms the new entry's style matches its siblings (SIG-matcher compatibility).

### Task 2.4: `CLAUDE.md` meta-drift fix

**Files:** `CLAUDE.md`.

- [ ] **Step 1:** Rewrite the `CLAUDE.md:31` sentence per spec §4.4: the stdlib reference
  is **domain-grouped** (22 pages ⊃ 57 modules — e.g. `collections.md` owns
  string/array/object/map/set/math/convert/bytes); the authoritative module→page mapping
  is `MODULE_PAGES` in `tests/docs_drift.rs` (tripwire-validated both directions); a
  `std/*` API change updates the module's OWNING page; a NEW std module needs a reference
  section + a mapping entry or CI fails. Keep the adjacent NAV-orphan guidance
  (`CLAUDE.md:32-35`) and append "(now enforced by `tests/docs_drift.rs`)"; same note on
  the relative-link rule (`:36-37`).
- [ ] **Step 2:** Commit: `docs(claude): fix stdlib-docs meta-drift — domain-grouped pages
  + the tripwire-validated mapping as the lookup`.
- [ ] **Step 3 — independent review:** reviewer reads the new wording as a fresh
  contributor would ("I changed `std/math` — where do I edit?") and confirms it answers in
  one hop; checks no other `CLAUDE.md` sentence still implies per-module pages.

### Task 2.5: tooling pages + README re-verification (+ the manual checklist item)

**Files:** `docs/content/tooling/{lsp-capabilities,debugging-profiling,editor-setup}.md`,
`README.md`, `docs/content/getting-started.md`, `CONTRIBUTING.md`, `CLAUDE.md` (one line).

- [ ] **Step 1 — lsp-capabilities:** walk `server_capabilities()`
  (`src/lsp/server.rs:224` through the workspace caps `:331+`) field by field against the
  page's method tables; fix any mismatch (none expected, spec §1.5/§4.5). Record the walk
  in the task log either way.
- [ ] **Step 2 — debugging-profiling:** confirm the page states: `--profile` v1 = `cpu`
  only (`src/main.rs:423-426`); the four `--profile-format` values incl. `deterministic-*`
  (`:431-441`); the `.aso`/tree-walker refusal (`:696-700`); default out paths
  `profile.json`/`profile.txt` (`:458-464`); the `dap` no-sandbox-flags posture
  (`:1191-1196`). Fix what's missing.
- [ ] **Step 3 — editor-setup:** confirm the page describes (not embeds) the pin and the
  described install flows match `editors/zed/extension.toml` +
  `editors/nvim/lua/ascript/treesitter.lua`.
- [ ] **Step 4 — the manual checklist item (spec §5.6):** in `CONTRIBUTING.md`'s
  grammar-publishing steps (`CONTRIBUTING.md:46-49`) and `CLAUDE.md`'s "Publishing the
  grammar" bullet, add the explicit line: after a sync, verify BOTH editor pins were
  bumped to the new mirror SHA — pin **currency vs the mirror** is a manual check
  (network/another repo; not CI-testable in-repo); pin **mutual consistency** is enforced
  by `tests/docs_drift.rs`.
- [ ] **Step 5 — README + getting-started:** re-run the spec §1.5 spot-checks
  (`README.md:107-111,126,152,161`); reconcile anything the cli.md rewrite revealed
  (e.g. if README names a flag with different semantics). Tripwire 5 still green.
- [ ] **Step 6:** Commit: `docs(tooling): re-verify lsp-capabilities/debugging-profiling/
  editor-setup + README; editor-pin manual checklist item`.
- [ ] **Step 7 — independent review:** reviewer re-walks `server_capabilities()` against
  the page independently; verifies the checklist line appears in BOTH files; re-runs the
  full `docs_drift` suite — **all six tripwires green** in both feature configs.

### Task 2.6: Phase 2 holistic review

- [ ] **Step 1:** Holistic reviewer: `cargo test --test docs_drift` fully green, both
  configs — the TDD loop is closed (red Phase-1 outputs in the task logs, green now).
- [ ] **Step 2:** Full read of the new `cli.md` for prose quality (the anti-generation
  bar): no clap-help verbatim dumps, every cross-link sensible, the page still teaches.
- [ ] **Step 3:** Served-site pass over every touched page
  (`cd docs && python3 -m http.server`).

---

## Phase 3 — Gates, campaign bookkeeping, merge

### Task 3.1: the gate addition + status updates

**Files:** `goal-perf.md`, `superpowers/roadmap.md`, `CLAUDE.md`.

- [ ] **Step 1:** `goal-perf.md`: add gate **19** to §"Gates" (the spec §6 text verbatim:
  docs drift tripwires stay green in CI; same-PR docs changes; owner-justified allowlist
  additions only). Add DOCS to the §"Developer-experience track" beside SIG (status ✅ on
  merge; record that SIG/DOCS are mutually independent, boundary per the DOCS spec §3) and
  to the execution-order note ("DOCS — independent; executed as part of the docs-currency
  push, owner-decided").
- [ ] **Step 2:** `superpowers/roadmap.md`: the DOCS milestone entry (findings closed,
  tripwires live, gate added).
- [ ] **Step 3:** `CLAUDE.md` docs guidance: one sentence naming `tests/docs_drift.rs` as
  the enforcement for CLI/env-var/module/NAV/link/pin currency (the §4.4/§4.5 edits
  already reference it; this is the campaign-gate cross-link).
- [ ] **Step 4:** Commit: `docs(campaign): gate 19 — docs drift tripwires stay green in CI;
  DOCS status`.

### Task 3.2: full-gate verification + merge

- [ ] **Step 1:** `cargo test` green; `cargo test --no-default-features` green (incl.
  `--test docs_drift` explicitly in both).
- [ ] **Step 2:** `cargo clippy --all-targets` + `cargo clippy --no-default-features
  --all-targets` clean.
- [ ] **Step 3:** `cargo test --test vm_differential` green — with Task 0.1's baseline,
  the AFTER half of the no-engine-surface proof (DOCS touched `main.rs`/`cli_surface.rs`
  only as a move; the differential proves the engines untouched).
- [ ] **Step 4:** Help-output identity re-check (`ascript --help` + per-sub) against the
  Task 0.1 baseline — still byte-identical.
- [ ] **Step 5:** Served-site final sanity; tripwire suite green one last time.
- [ ] **Step 6 — independent final review:** reviewer probes edges: adds a scratch flag to
  `cli_surface.rs` locally → tripwire 1 red (revert); adds a scratch `ASCRIPT_SCRATCH`
  read → tripwire 2 red (revert); confirms allowlists carry justifications; confirms
  every plan checkbox is ticked.
- [ ] **Step 7:** Merge `feat/docs-drift-tripwires` into `main` with `--no-ff`; update the
  `goal-perf.md` status table to ✅ in the merge commit.

---

## Execution-time re-verification deltas (carry into task logs)

Recorded here so a fresh implementer doesn't re-trip the audit's stale claims (spec §1):

1. The audit's "~10 undocumented std/math members" is **STALE** — closed by the NUM docs
   commit (`37ce523`); all 44 math exports are documented in `collections.md`. Do NOT
   re-add them.
2. The full 57-module sweep found exactly **one** member gap (`task.pipe` → `async.md`);
   `workflow.run/resume/activity`, `server.create`, `sync.*`, `tui.init`, `ffi.u16/u32/
   i16/i64` are documented in named-import/heading/table styles — false positives of the
   `mod.fn` grep. Always verify a miss manually (Task 2.3 Step 1).
3. NAV ⇄ files: **40** pages (the audit said 39), bijection holds; 134 in-content links,
   0 broken; editor pins equal (`7227fb7f…`) — tripwires 3–6 are green-at-birth baselines
   whose can-it-fail proof is the mutation self-tests.
4. `server_capabilities` is at `src/lsp/server.rs:224` (audit cited `:195-325`).
5. `ascript doc` IS documented in `cli.md:250-264`; only `dap` is the wholly-missing
   subcommand section.
