# Agent Handoff — AScript build continuity

This file carries the context that was stored in the original machine's agent
**memory** (which does not travel across machines). Read it together with
`roadmap.md` (status + "Phase 2 starting point") and `specs/2026-05-29-ascript-design.md`.

## What this project is

**AScript** — a Lua-simple, JS-flavored scripting language with a Rust tree-walking
interpreter and a batteries-included standard library. Built ground-up, milestone by
milestone: production-quality, fully unit- and example-tested, spec-compliant, nothing
deferred within a phase (deferrals must be justified + assigned an owning milestone).

## Status (as of this handoff)

**PHASE 1 COMPLETE and merged to `main`.** The language (spec §§2–9) + tooling (§10:
clap CLI, ariadne diagnostics, REPL, `ascript fmt`, `ascript test`, Tree-sitter grammar
+ conformance) + async/await surface (§7) are all implemented. ~148 tests green
(`cargo test`), `cargo clippy --all-targets` clean. The CLI has `run`/`repl`/`fmt`/`test`
subcommands. Examples in `examples/` each have an integration test.

**Phase 2 (the standard library) + the LSP are next.** Start at **M10 — Core
collections** (see roadmap "Phase 2 starting point" for the exact seams: the `std/*`
resolution hook in `resolve_import`/`load_module`, `Value::Builtin` dispatch in
`call_builtin`, and the `Map` value kind that M10 introduces).

## How to work (the approach used throughout Phase 1 — keep doing this)

Per milestone:
1. **Plan** with the `superpowers:writing-plans` skill → save to `docs/superpowers/plans/`.
2. **Execute** with `superpowers:subagent-driven-development`: a fresh subagent implements
   each task (TDD), then an **independent reviewer subagent** checks spec-compliance AND
   code quality AND runs the tests. Fix → re-review until approved.
3. **Final holistic review** of the whole branch before merge.
4. **Merge** `--no-ff` to `main`, only when `cargo test` + `cargo clippy --all-targets`
   are green and the milestone is spec-compliant. Delete the feature branch.
5. Update `roadmap.md` status markers + record any forward guidance for the next milestone.

One milestone per feature branch off `main`. Commit/push only as the human directs.
Commit trailer used: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

## Preferences (from the human, carried from memory)

- **Use Opus for spawned subagents** (implementers AND reviewers). Do not down-select to
  a cheaper model even for "mechanical" tasks — the human prioritizes output quality.
- Be rigorous: reviewers should independently verify (read code, run commands, probe edge
  cases / panics), not trust the implementer's report. This caught many real bugs in
  Phase 1 (a usize underflow panic, two spec-compliance violations, a parser collision,
  missing comment support, an fmt idempotence bug).

## Durable source of truth (all in the repo, survives compaction / machine moves)

- `docs/superpowers/roadmap.md` — milestone list, live status, per-milestone design
  guidance, and the "Phase 2 starting point" onboarding section. **Read this first.**
- `docs/superpowers/specs/2026-05-29-ascript-design.md` — the full language + stdlib spec
  (incl. §11.5 the modern HTTP client contract).
- `docs/superpowers/plans/` — one committed plan per completed milestone (M1–M9) as
  worked examples of the plan format/granularity to emulate.
- `git log` — the milestone-by-milestone history.

## Goal for the Phase-2 session

Implement the entire standard library (and finally the LSP) per the spec, milestone by
milestone (M10 → M16), same approach, until Phase 2+ is complete: nothing deferred unless
dependency-blocked + justified + assigned a milestone, production quality, fully tested.
