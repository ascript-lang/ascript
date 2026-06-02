# `ascript check` — Checker & Linter Design Spec

**Date:** 2026-06-02
**Status:** Draft for review (authored under the "I draft, you approve" mode)
**Sub-projects covered:** #3 (`ascript check` CLI + shared analysis core), #4 (scope & control-flow lints), #5 (AScript-specific lints), #6 (static contract checking). Depends on the **CST front-end + name resolver** (front-end spec + Plans 1–3).

> A handful of decisions are flagged **[CONFIRM]** inline — those are the ones that genuinely need your call. Everything else is a recommendation made under the draft mode.

## Problem

Today AScript has no way to check a script outside an LSP-capable editor: `src/lsp/analysis.rs::diagnostics` is lex+parse-only, reports just the *first* error, returns LSP-typed values, and is gated behind the `lsp` Cargo feature. CLI users, CI, and editors without LSP get nothing — there is no `ascript check`. The original goal is a **fully powerful checker**, up to and including best-effort static contract checking, usable from the terminal and CI as well as the editor.

## Goals

- A **feature-independent analysis core** producing a neutral `AsDiagnostic` list (span + severity + rule code + message + optional fix), available in *every* build config (not behind `lsp`).
- An **`ascript check` CLI** that reports **all** diagnostics (not just the first), renders them with ariadne, and sets a CI-meaningful exit code.
- The **LSP rewired onto the same core** (map `AsDiagnostic` → `lsp_types::Diagnostic`), so editor and CLI never diverge.
- A **tiered lint engine** consuming the shared name resolver: scope/control-flow lints, AScript-specific lints, and best-effort static contract checking — with an explicit **no-false-positives** philosophy at the uncertain edges.
- **Suppression** + **per-rule configuration** so teams can tune severity and silence intentional cases.

## Non-Goals

- A full static type system / type inference. Contract checking is **best-effort and partial** by design (AScript is gradually typed); it only flags the *provably* wrong.
- Reformatting (that's the formatter, Plan 4).
- Cross-file/whole-program analysis beyond import-name resolution (single-file analysis; imports resolved to known module exports). Deeper cross-file flow is a future extension.

## Decomposition (build order)

This spec spans four sub-projects, built in this order (each its own plan(s)):

1. **#3 — Analysis core + `ascript check` CLI + LSP rewiring.** The `AsDiagnostic` model, the analysis driver (lex → parse → resolve → run lint passes), the ariadne CLI renderer + exit codes, all-syntax-errors, and the LSP adapter. Ships value immediately: full syntax checking from the terminal/CI.
2. **#4 — Scope & control-flow lints** (consume the resolver): undefined variable, unused binding, unused import, shadowing, unreachable code, missing return.
3. **#5 — AScript-specific lints:** unawaited `future`, ignored `Result`/`?`/`!`, dead `recover`, and the language's other footguns.
4. **#6 — Static contract checking:** verify gradual type contracts / `T?` / schema-class shapes where decidable, conservatively.

## Architecture

```
source → lexer (Plan 1) → parser (Plan 2/2b, error-recovering) → CST + parse errors
                                              │
                                              ▼
                                    resolver (Plan 3) → ResolveResult
                                              │
                            ┌─────────────────┴─────────────────┐
                            ▼                                     ▼
                    LINT PASSES (per rule)                 (compiler — separate)
                            │  each: &CST + &ResolveResult → Vec<AsDiagnostic>
                            ▼
                    Vec<AsDiagnostic>  (deduped, sorted by span)
                            │
              ┌─────────────┴──────────────┐
              ▼                             ▼
        ascript check (ariadne + exit)   LSP (→ lsp_types::Diagnostic)
```

**New module `src/check/`** (feature-independent — *not* behind `lsp`):
- `diagnostic.rs` — `AsDiagnostic`, `Severity`, `RuleCode`, `Fix` (a text edit).
- `analyze.rs` — `analyze(src) -> Analysis { diagnostics, .. }`: the driver that runs parse + resolve + all enabled rules.
- `rules/` — one module per rule (or per tier): `rules/scope.rs`, `rules/control_flow.rs`, `rules/ascript.rs`, `rules/contracts.rs`.
- `config.rs` — rule enable/severity config + suppression handling.
- `render.rs` — ariadne rendering for the CLI; a `--json` machine format.

**Touched elsewhere:**
- `src/main.rs` — new `Command::Check { files, format, … }`.
- `src/lsp/analysis.rs` / `server.rs` — `diagnostics()` becomes a thin adapter: call `check::analyze`, map `AsDiagnostic` → `lsp_types::Diagnostic`. The `lsp` feature keeps only the *mapping*, not the analysis.

### The diagnostic model

```rust
pub struct AsDiagnostic {
    pub range: Span,            // byte/char range into source (ariadne + LSP both consume)
    pub severity: Severity,     // Error | Warning | Info | Hint
    pub code: RuleCode,         // stable machine code, e.g. "unused-binding"
    pub message: String,
    pub fix: Option<Fix>,       // optional autofix (text edit); see Autofix
}
pub enum Severity { Error, Warning, Info, Hint }
pub struct Fix { pub edits: Vec<TextEdit>, pub title: String }
pub struct TextEdit { pub range: Span, pub replacement: String }
```

Neutral on purpose: ariadne, the LSP, and `--json` all derive from this. `RuleCode` is a stable string so config/suppression/CI can reference rules.

### Severity & exit codes (DECIDED)

- **Syntax errors** → `Error`; `ascript check` exits **non-zero**.
- **Lints** default severities: correctness-ish lints (undefined var, unreachable, unawaited future, ignored Result) → `Warning`; style lints (unused binding/import, shadowing) → `Warning`; contract mismatches → `Warning`.
- **CI exit policy (DECIDED):** non-zero on `Error` **always**; non-zero on `Warning` **only** under `--deny-warnings` (or `--deny <rule>`) — ruff/clippy convention.

### Suppression (DECIDED)

Inline comments — `// ascript-ignore[rule-code]` on the line above (or the same line as) the offending node suppresses that rule there; `// ascript-ignore-file[rule-code]` at the top suppresses file-wide. The checker reads these from the **CST trivia** (comments are in the tree — Plan 1/2). Multiple codes comma-separated; bare `// ascript-ignore` suppresses all rules at that site.

### Configuration

A `[lint]` table in an optional `ascript.toml` at the project root: `deny`/`warn`/`allow` lists by rule code, mirroring CLI flags (`--deny`, `--warn`, `--allow`). CLI flags override the file. No config file → sensible defaults (above). Keep it minimal (YAGNI): no per-directory configs in v1.

### Autofix (DECIDED)

`AsDiagnostic.fix` carries an optional edit-based fix (e.g. remove an unused import, add `await`). The model carries `fix` **from the start**, populated for trivially-safe rules (unused import/binding). `ascript check --fix` (and the LSP code-action) **ships once a couple of rules have fixes** — cheap once the model carries them. Fixes are edit-based (re-parse after applying), à la ruff/biome.

## Rule catalog

### Tier 0 — Syntax (sub-project #3)

- **`syntax-error`** (Error): every `ERROR` node / collected parse error from the recovering parser, each with its span. Reporting *all* (not just the first) is the headline improvement over today's analysis.

### Tier 1 — Scope & control flow (sub-project #4; consumes `ResolveResult`)

- **`undefined-variable`** (Warning): a `NameRef` the resolver marks `Global` whose name is **not** a builtin (`global_env` set: `print`/`len`/`range`/…), **not** an imported name, and not a known stdlib module alias. The resolver deliberately classifies frees as `Global`; the checker has the builtin list + import analysis to decide "genuinely undefined." Resolves the `Global`/`Unresolved` boundary left open in Plan 3.
- **`unused-binding`** (Warning, fixable): a `Binding` with `use_count == 0` that isn't a parameter of an exported/public fn (params are often intentionally unused; recommendation: skip params, or require a leading-underscore opt-out — **mirrors common linters**). Fix: remove the binding (when safe).
- **`unused-import`** (Warning, fixable): an imported name (named or namespace alias) never referenced. Fix: remove the import / the name from the list.
- **`shadowing`** (Hint by default): a binding whose name shadows an outer binding in an enclosing scope. Off-by-default-ish (Hint) since shadowing is legal and sometimes intentional.
- **`unreachable-code`** (Warning): statements following a `return`/`break`/`continue` in the same block (control-flow analysis over the CST block structure).
- **`missing-return`** (Warning): a function with a declared non-`nil` return type (`RetType`) whose body has a control path that falls off the end without returning. Conservative CFG over the function body (if/else/match completeness drives it).

### Tier 2 — AScript-specific (sub-project #5)

- **`unawaited-future`** (Warning): a `future<T>`-producing call (a script `async fn` call, or a stdlib API known to return a future) whose result is dropped (an `ExprStmt` that is a bare call to an async fn, not `await`ed, not assigned/returned). This is the *flagship* lint — it's the exact class of bug M17 fought (the 130 MB un-awaited-async leak). Detection uses syntactic signals (callee is a known `async fn` via the resolver / a known future-returning builtin) — best-effort, conservative.
- **`ignored-result`** (Warning): a call known to return a Tier-1 `[value, err]` Result used as a bare `ExprStmt` without `?`, `!`, or destructuring/inspection — the error is silently dropped. Conservative: only for calls whose Result-ness is statically known (stdlib signatures / `?`-typed returns).
- **`dead-recover`** (Hint): a `recover(fn)` whose `fn` body cannot panic (no fallible calls / no `!`) — the recover is inert. Best-effort.

### Tier 3 — Static contract checking (sub-project #6)

Best-effort verification of the gradual type contracts where the answer is **decidable from syntax + resolver data**, with a strict **no-false-positives** rule:

- **`contract-mismatch`** (Warning): flag only **provably-wrong** cases — e.g. a literal of the wrong primitive passed where a `number` param is annotated (`f("x")` for `fn f(n: number)`), a `nil` passed to a non-`T?` annotated param, a field assignment of a literal of the wrong type inside `init`, an obviously-wrong `.from`/`json.parse(_, Class)` shape with literal inputs.
- **Philosophy [CONFIRM]:** **conservative — never flag when uncertain.** If a value's type isn't statically known (most non-literal expressions in a dynamic language), say nothing. This keeps `contract-mismatch` trustworthy (zero false positives) at the cost of coverage — the right trade for a gradually-typed language where false positives would train users to ignore the checker. **[CONFIRM]** you want this conservative stance (vs. a more aggressive flow-typing pass that risks false positives).

## CLI

```
ascript check [FILES]...        # default: human (ariadne) output
  --json                        # machine-readable diagnostics (editors/CI)
  --deny <rule|warnings>        # treat as error (non-zero exit)
  --warn <rule>  --allow <rule> # adjust severity
  --fix                         # apply safe autofixes to the working tree
```
- No files → check all `*.as` under the cwd (or per config), like `fmt`.
- Exit: non-zero on any `Error` (always) and on `Warning` only under `--deny-warnings`/`--deny <rule>`.
- Reuses ariadne (already a dependency, already used for runtime diagnostics) so terminal output matches the look of runtime panics.

## Runtime invariant interaction

The checker is the **tooling** consumer of the error-recovering parser (front-end spec's invariant): it reports *all* syntax errors and analyzes the good parts. This never affects the **run path** — `run_file`/`import`/REPL still refuse to execute a program with any parse error. The checker and the runtime share the parser, not the policy.

## Testing

- **Per-rule unit tests**: each rule module has `(source → expected diagnostics)` table tests (code + span + message), including **negative** cases (no false positives) — especially dense for `contract-mismatch`.
- **Suppression tests**: `// ascript-ignore[code]` silences exactly that rule at that site and nowhere else.
- **CLI integration tests** (`tests/check.rs`): exit codes, `--json` shape, `--deny-warnings`, multi-error output, `--fix` round-trip.
- **LSP parity test**: `check::analyze` and the LSP adapter agree (same diagnostics, mapped).
- **Corpus smoke**: `ascript check examples/**/*.as` reports **zero** diagnostics on the (clean) example corpus — a regression guard that the checker doesn't false-positive on idiomatic code. (Any example that legitimately should warn gets a suppression or is fixed.)
- **Clippy clean in both feature configs**; the core builds with `--no-default-features` (the whole point — checking without the `lsp`/stdlib features).

## Decisions (log)

- **Analysis core is feature-independent** (neutral `AsDiagnostic`; not behind `lsp`), so CLI + CI + bare-language builds can check. LSP becomes a thin mapping adapter.
- **All syntax errors reported** (error-recovering parser), not just the first.
- **Resolver reused** (Plan 3) for scope lints; the checker owns the `Global`-vs-genuinely-undefined decision via the builtin list + import analysis.
- **Tiered rules**, built #3→#6; contract checking last and **conservative (no false positives)**.
- **Suppression via CST-trivia comments**; **config via optional `ascript.toml` `[lint]`** + CLI flags.
- **Autofix** carried in the diagnostic model from the start; `--fix` shipped once safe rules have fixes.
- **`unawaited-future`** is the flagship AScript-specific lint (directly targets the M17 leak class).

## Open items flagged for your approval

1. **[CONFIRM] CI exit policy** — non-zero on warnings only under `--deny-warnings` (recommended), or non-zero on any warning by default?
2. **[CONFIRM] Suppression syntax** — `// ascript-ignore[rule]` (recommended) or another form?
3. **[CONFIRM] Autofix scope** — populate `fix` now + ship `--fix` after a couple safe rules (recommended), or defer the fix model entirely?
4. **[CONFIRM] Contract-checking stance** — conservative / zero-false-positives (recommended) vs. a more aggressive flow-typing pass?
