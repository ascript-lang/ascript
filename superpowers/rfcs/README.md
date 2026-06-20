# RFCs — changing the AScript language

This directory holds **RFC-lite** proposals: the deliberately light front door for
**language-surface** changes. It is not a second process — the existing campaign
cadence (**spec → independent review → lock → plan → implement → review → merge**)
*is* the change-management process. RFC-lite only formalizes the **proposal and
verdict** step for changes that touch the spec'd surface.

## When you need an RFC

A change is **RFC-bearing** iff it:

1. changes **STABLE** spec'd behaviour, **or**
2. adds **language surface** (grammar / AST / a new value kind), **or**
3. **promotes or demotes** a stability tier.

Everything else is **not** RFC-bearing: stdlib additions inside existing rules, bug
fixes that move the implementation *toward* already-spec'd behaviour, and INTERNAL
changes (`.aso`, opcodes, shape/IC machinery, performance work). The authoritative
rule lives in the [stability chapter](../../docs/content/spec/stability.md). When in
doubt, open an RFC — a one-pager is cheap.

## The process (one screen)

1. **Open an RFC.** Copy [`0000-template.md`](0000-template.md) to
   `NNNN-slug.md` (the next free number) and open a PR. Keep it to one page: Problem,
   Proposal + one example, the Impact checklist, Alternatives, and an empty Verdict
   block.
2. **Owner review → verdict.** The owner fills the **Verdict** block —
   **Accepted / Rejected / Deferred** + date + rationale — and merges the RFC file so
   the decision is recorded regardless of outcome.
3. **On Accept, graduate to a design spec.** An accepted RFC becomes a full design
   document under `superpowers/specs/` and the normal campaign cadence takes over
   (independent review → lock → plan → implement → review → merge).
4. **The implementing PR updates the spec.** When the change lands, the same PR
   updates the affected `docs/content/spec/` chapters. The grammar half is enforced
   mechanically by `tests/spec_drift.rs` (an unmentioned `grammar.js` rule fails CI);
   the semantics half is a reviewer duty backed by the `CLAUDE.md` "Touching syntax"
   checklist.
5. **The RFC is the permanent record.** It stays in this directory forever.
   **Rejected** and **Deferred** RFCs stay filed too — they are the record of *why*
   the language is shaped the way it is, and what was considered and parked.

## Files

- [`0000-template.md`](0000-template.md) — the one-page template. Do not edit in
  place; copy it to a numbered file.
- `NNNN-slug.md` — individual RFCs (none yet).
