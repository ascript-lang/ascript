# Checker Plan CFG — wire LintConfig to `--deny`/`--warn`/`--allow` + `ascript.toml`

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development.
> **Execution order:** AFTER the VM/GC sub-project (per the owner). Independent of the VM — pure checker work against APIs that already exist.

**Goal:** Make the staged-but-unwired `LintConfig` (`deny`/`warn`/`allow`/`effective`, already implemented + unit-tested in `src/check/config.rs`) actually configure the checker: repeatable CLI flags `--deny <rule>` / `--warn <rule>` / `--allow <rule>`, and an `ascript.toml` `[lint]` section (per-rule severity overrides + `deny_warnings`). Resolved severities are applied in `analyze`/the `check` CLI exit logic, with clear precedence. Inline `// ascript-ignore[code]` suppression still wins for suppression.

**Architecture:** `analyze` currently returns diagnostics at each rule's DEFAULT severity. Thread a `&LintConfig` into the analysis (or apply as a post-pass): for each diagnostic, `config.effective(code, default) → Some(sev)` (remap severity) or `None` (drop — `allow`/suppressed). The CLI builds the `LintConfig` from `ascript.toml` (if found) then overlays CLI flags, and uses the resolved severities for output + exit codes. **Depends on the checker (C1–C4), all shipped.**

---

## Ground truth
- `LintConfig { overrides: HashMap<String,Option<Severity>>, deny_warnings: bool }` with `deny(code)`/`warn(code)`/`allow(code)` (allow = `Some(None)` = suppress) and `effective(code, default) -> Option<Severity>` — `src/check/config.rs` (implemented + tested; currently no callers outside the file).
- `analyze(src) -> Analysis { diagnostics: Vec<AsDiagnostic> }` (`src/check/analyze.rs`) — diagnostics carry `code` + default `severity`. Inline suppression already applied inside `analyze`.
- CLI `Command::Check { files, json, deny_warnings }` (`src/main.rs`) — only `deny_warnings: bool` is consulted today.
- The 10 rule codes: `syntax-error`, `undefined-variable`, `unused-binding`, `unused-import`, `shadowing`, `unreachable-code`, `missing-return`, `unawaited-future`, `ignored-result`, `dead-recover`, `contract-mismatch`. (`syntax-error` is always Error — `--allow syntax-error` should be rejected or ignored; decide + document.)
- `toml` crate availability: behind the `data` feature? The checker core is feature-INDEPENDENT — so `ascript.toml` parsing must either use a feature-independent toml parser OR live in the CLI layer (`main.rs`, which has full features) rather than `src/check/`. PREFER: config parsing in `main.rs`/a CLI-side module (full features), `src/check/` stays feature-independent and only consumes a `LintConfig`.

---

## Tasks
- [ ] **T1 — apply LintConfig in analyze (or post-pass).** Add `analyze_with_config(src, &LintConfig) -> Analysis` (keep `analyze(src)` = `analyze_with_config(src, &LintConfig::default())`). After collecting diagnostics (and after inline-suppression), remap each: `match config.effective(&d.code, d.severity) { Some(sev) => d.severity = sev, None => drop }`. `syntax-error` is never downgraded/dropped (guard it). Inline `ascript-ignore` still removes a diagnostic regardless of config. Unit tests: deny promotes a warning→error; allow drops; warn demotes; syntax-error immune; inline-ignore still wins. Commit `feat(check): apply LintConfig severities in analyze`.
- [ ] **T2 — CLI flags `--deny/--warn/--allow` (repeatable).** Extend `Command::Check` with `#[arg(long="deny")] deny: Vec<String>`, `--warn`, `--allow` (repeatable). Build a `LintConfig` from them (validate rule codes against the known set; unknown code → a clear error, not silent). Pass to `analyze_with_config`. Exit-code logic uses the RESOLVED severities (an `--deny`'d rule now Error → non-zero exit; `--deny-warnings` still promotes all warnings for exit). Integration tests (`tests/check.rs`): `--deny unused-binding` makes a warning-only file exit non-zero; `--allow unused-binding` silences it; `--warn` demotes. Commit `feat(cli): --deny/--warn/--allow flags`.
- [ ] **T3 — `ascript.toml` `[lint]` section.** In the CLI layer (full features → use the `toml` crate), discover `ascript.toml` (walk up from the file's dir / cwd, like a project root marker), parse a `[lint]` table: `deny = ["rule", ...]`, `warn = [...]`, `allow = [...]`, `deny_warnings = true`. Build a `LintConfig`; CLI flags OVERLAY it (precedence: CLI > toml > rule default). Inline `ascript-ignore` always wins for suppression. Malformed toml / unknown rule → a clear diagnostic. Integration tests: a temp project with `ascript.toml` that denies a rule → non-zero exit; CLI `--allow` overrides the toml deny; precedence verified. Commit `feat(cli): ascript.toml [lint] config`.
- [ ] **T4 — docs + spec.** Update the checker spec (`docs/superpowers/specs/2026-06-02-checker-design.md`) CLI section: `--deny/--warn/--allow` + `ascript.toml` are now SHIPPED (move them out of "future work"). Document precedence (inline-ignore > CLI > toml > default; syntax-error immune), the rule-code list, and an `ascript.toml` example. Update `README.md`/`docs/content` if they describe `ascript check`. Update the staged-scaffolding NOTE in `config.rs` (it's now wired). Commit `docs(check): document --deny/--warn/--allow + ascript.toml`.
- [ ] **T5 — full suite + clippy both configs + feature-independence.** `src/check/` stays feature-independent (toml parsing is CLI-side). `cargo test`, `cargo build --no-default-features`, clippy both configs. Commit.

## Done criteria (CFG)
- [ ] `--deny/--warn/--allow` (repeatable) + `ascript.toml [lint]` configure rule severities; precedence inline-ignore > CLI > toml > default; `syntax-error` immune.
- [ ] Exit codes reflect resolved severities; `LintConfig` is now wired (no longer staged-only).
- [ ] `src/check/` stays feature-independent; spec/docs updated; `cargo test` green; clippy clean both configs.

**This closes the deferred checker-config follow-up.**
