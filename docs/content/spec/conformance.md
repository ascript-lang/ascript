# The conformance suite

This chapter formally adopts AScript's **conformance suite** and states the
**criterion** an implementation must meet to conform. The preceding chapters describe
*what* the language is; this chapter says *how an implementation proves it got it
right*. The criterion is **four-mode byte-identity over a fixed corpus** — the
executable definition of "the same language."

## The conformance suite v1

The conformance suite is the union of four components, all checked in by the
reference implementation:

1. **The `examples/` corpus, run four-mode.** Every program under `examples/` and
   `examples/advanced/` is executed on each of the four engine modes and their
   stdout/exit behaviour compared **byte for byte** by `tests/vm_differential.rs`.
   The four modes are:
   - the **tree-walking interpreter** (the differential oracle),
   - the **specialized bytecode VM** (the default production engine),
   - the **generic bytecode VM** (`--no-specialize`, the semantic floor with every
     fast path disabled), and
   - the **`.aso`-compiled** path (`build` → run the serialized bytecode).

   A small, **per-file-documented** skip list (`EXAMPLE_SKIPS`) excludes only programs
   that cannot be compared by a stdout-equality oracle — inherently non-deterministic
   output (ephemeral ports, live clocks, random bytes, network event streams),
   long-running servers that block, and daemon-dependent programs. **Each skip is
   itself test-guarded** with a recorded reason; a program may not silently leave the
   gate.

2. **The recorded goldens.** `tests/vm_goldens/` holds the expected stdout for the
   corpus. A change in observable output is a golden diff — visible, reviewed, never
   silent.

3. **The two front-end catalogs.** `tests/treesitter_conformance.rs` and
   `tests/frontend_conformance.rs` assert that **both** parsers — the tree-sitter
   grammar and the legacy precedence-climbing front-end — accept the corpus. This is
   the empirical pin of **grammar equivalence**: the [grammar chapter](grammar)'s
   drift test proves every tree-sitter rule is *covered* by an EBNF production, but it
   is these catalogs that prove the two grammars accept the **same language**.

4. **The drift tripwires.** `tests/docs_drift.rs` and `tests/spec_drift.rs` keep the
   documentation honest: module→page bijection, NAV reachability, spec chapter
   existence + `## Conformance` sections, spec citation resolution, and grammar-rule
   coverage. A documentation claim that rots fails CI.

## The criterion

> An implementation of AScript **CONFORMS** iff, over the entire conformance suite,
> it produces **byte-identical observable behaviour** — stdout, exit status, and
> panic/diagnostic messages (caret columns may differ by the recorded ±1 column) — to
> the suite's goldens and the reference implementation, in **both** feature
> configurations (default and `--no-default-features`).

The in-tree engines meet this bar **continuously**: tree-walker == specialized VM ==
generic VM == `.aso`-compiled, on every corpus program, in both feature configs.
Behaviour — including Tier-2 panic message strings — is raised from code **both
engines reach**, so the strings are identical by construction, not by coincidence
(see the [errors chapter](errors)).

**Where this specification's prose and the suite disagree, the suite is presumed
correct and the prose is a defect.** The differential oracle is the executable
semantics; the chapters are its human-readable projection. A contradiction is
triaged, not papered over.

## Chapter → suite map

Each chapter's `## Conformance` section names the suite components that pin its
claims; the table below is the union, kept honest by `tests/spec_drift.rs` (which
verifies every cited path resolves on disk). The four cross-cutting pins —
`tests/vm_differential.rs`, `tests/vm_goldens/`, the two front-end catalogs, and the
drift tripwires — underwrite **every** chapter; per-chapter sections add the specific
examples and focused tests for that chapter's constructs.

## What the suite is not

The conformance suite is **not a feature-coverage promise**. It does not assert that
every library function or syntactic corner is exercised — it asserts that everything
it *does* cover behaves identically across modes. The suite **grows with the
language**: each new construct or stdlib surface lands with corpus examples and, where
relevant, focused tests, extending the gate. The language version records the suite
**snapshot** it was verified against; a later version's suite is a superset, never a
silent narrowing.

## Running it

The suite is run with the standard test commands, in **both** feature
configurations:

```bash
cargo test --test vm_differential                       # four-mode corpus identity (default features)
cargo test --no-default-features --test vm_differential  # …and the bare-language config
cargo test --test treesitter_conformance                # tree-sitter accepts the corpus
cargo test --test frontend_conformance                  # the legacy front-end accepts the corpus
cargo test --test spec_drift --test docs_drift          # the drift tripwires
```

## Conformance

The conformance suite is defined by, and self-exercises, these components:

- `tests/vm_differential.rs` — the four-mode byte-identity battery over the
  `examples/` corpus, with the documented `EXAMPLE_SKIPS` exclusions; **446 passing**
  in the default configuration.
- `tests/treesitter_conformance.rs` — the tree-sitter front-end accepts the corpus
  (16 passing).
- `tests/frontend_conformance.rs` — the legacy front-end accepts the corpus (36
  passing).

Verified directly:

- `cargo test --test vm_differential` → `446 passed; 0 failed`.
- `cargo test --test treesitter_conformance` → `16 passed; 0 failed`.
- `cargo test --test frontend_conformance` → `36 passed; 0 failed`.
