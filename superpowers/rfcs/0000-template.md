# RFC NNNN — <Title>

- **Date:** YYYY-MM-DD
- **Champion:** <name / handle>
- **Status:** Draft

> Copy this file to `superpowers/rfcs/NNNN-slug.md` (next free number) and open a PR.
> Keep it to **one page**. The owner fills in the **Verdict** block.

## Problem

What is broken, missing, or awkward today, and for whom? **At most three paragraphs.**
State the concrete pain — not a solution in disguise. Link any prior discussion.

## Proposal

The surface sketch: the syntax/AST/value-kind/stdlib change you propose, in enough
detail to evaluate. Include **one** worked example:

```ascript
// the proposed feature in use
```

## Impact

Check every box that applies — this is what the reviewer weighs.

- [ ] **Grammar / syntax** — changes `tree-sitter-ascript/grammar.js` (→ both parsers
      regenerated, `spec/grammar.md` EBNF updated).
- [ ] **Both parsers + regen** — touches the legacy `parser.rs` AND the CST parser,
      and regenerates `parser.c` (`tree-sitter generate --abi 14`).
- [ ] **`.aso` format** — new/changed opcode or serialization layout (→
      `ASO_FORMAT_VERSION` bump, possible `verify.rs` update).
- [ ] **Standard library** — adds/changes a `std/*` API (→ docs page + signature
      table).
- [ ] **Breaking change** — alters STABLE spec'd behaviour (→ version bump + migration
      notes + corpus migration; see `spec/stability`).
- [ ] **Spec chapters affected** — list them: <chapters>.

## Alternatives considered

What else was on the table, and why this proposal over those? Include "do nothing"
when relevant.

## Verdict

> *Filled in by the owner.*

- **Decision:** Accepted / Rejected / Deferred
- **Date:** YYYY-MM-DD
- **Rationale:** <one or two sentences>
