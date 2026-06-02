# CST Front-End Migration — Design Spec

**Date:** 2026-06-02
**Status:** Approved (brainstorming complete; pending implementation plan)
**Sub-project:** #1 of a larger "powerful tooling" effort (see Decomposition)

## Problem

Two concrete gaps motivated this work:

1. **The formatter silently drops every comment.** Confirmed empirically on the current
   binary: `fmt` on `let x = 1 // hello\n/* block */\nlet y = 2` returns `let x = 1\nlet y = 2`.
   Root cause: the lexer (`src/lexer.rs:212`) advances past comments emitting *no token*, and
   whitespace is skipped at `src/lexer.rs:108`. Comments never enter the token stream or AST, so
   no downstream consumer can preserve them. A formatter that deletes comments is a data-loss bug.
2. **There is no way to check scripts outside an LSP-capable editor.** The only static analysis
   (`src/lsp/analysis.rs::diagnostics`) is lex+parse-only, returns LSP-typed `Diagnostic`s, is
   gated behind the `lsp` Cargo feature, and reports only the *first* error. CLI users, CI, and
   editors without LSP get nothing. There is no `ascript check` command.

The user's intent is a **fully powerful checker** (up to and including best-effort static contract
checking) plus a **comment-preserving formatter**. Both ultimately require a faithful,
loss-less representation of source — which the current discard-trivia front-end cannot provide.

## Goals

- A **lossless concrete syntax tree (CST)** front-end: every byte of source (including comments,
  whitespace, `;`) is represented in the tree.
- A **comment-preserving, idempotent formatter** built on that tree (this sub-project's
  user-visible deliverable and acceptance gate).
- A **CST-native interpreter** with no observable behavior change and no unacceptable performance
  regression.
- A **shared name resolver** built as reusable infrastructure (consumed by the interpreter now,
  the checker later).
- Foundations that make the later checker sub-projects straightforward to build.

## Non-Goals (this sub-project)

- The `ascript check` CLI command and lint rules (sub-projects #2–#5).
- Static contract checking (sub-project #5).
- Collapsing the tree-sitter grammar into the new pipeline (it stays as a test-only oracle; any
  future unification is a separate decision).
- Incremental/streaming reparse (cstree supports the shape; not needed yet).

## Decomposition (full effort)

This spec covers **sub-project #1 only**. The whole effort decomposes into independent
spec → plan → build cycles:

1. **Lossless CST + typed-AST front-end** (this spec) — delivers the comment-preserving formatter;
   also builds the shared name resolver.
2. **`ascript check` CLI + shared analysis core** — feature-independent diagnostic type, ariadne
   CLI renderer with exit codes, all syntax errors (not just first), LSP rewired onto the core.
3. **Scope & control-flow lints** — *uses* the resolver from #1 (undefined/unused/shadowed/
   unused-import, unreachable code, non-exhaustive match, missing return).
4. **AScript-specific lints** — unawaited `future`, ignored `Result`/`?`/`!`, dead `recover`.
5. **Static contract checking** — best-effort verification of gradual type contracts / `T?` /
   schema-class shapes, with an explicit false-positive philosophy.

Ordering is dependency-driven; #1 is the load-bearing foundation. Note the resolver was moved
from #3 into #1 (see Decisions).

## Architecture

Layered front-end; each layer has one job and a typed interface to the next:

```
source text
  → LEXER        flat token stream INCLUDING trivia (comments, whitespace, newlines, `;`)
  → PARSER       recursive descent, emits Start(kind)/Token/Finish events; error-recovering
  → CST          cstree green/red tree (lossless); built by cstree's builder from the events
  → TYPED AST    thin typed wrappers over CST nodes, GENERATED from an ungrammar grammar file
  → CONSUMERS    interp (execution view), fmt (walks CST, prints trivia), lsp, future checker
```

### Library & codegen choices

- **Tree library: `cstree`** (a fork of rowan by the rust-analyzer authors). Chosen over rowan
  specifically because:
  - cstree **persists red nodes and returns references**; rowan re-creates the red layer (cloning
    nodes) on every traversal. For a tree-walking interpreter that visits nodes on the hot path,
    this directly mitigates the primary performance risk.
  - cstree **interns token strings** — deduplicates repeated identifiers (memory + comparison win;
    pairs with the runtime's existing `Rc<str>` interning).
  - cstree nodes can carry **custom data** — a natural home for the resolution cache.
  - Cost: no mutable-tree API. Not a real loss — autofix (later) uses edit-based rewriting like
    ruff/biome, not in-place mutation.
  - `!Send`/`!Sync` of the chosen config is irrelevant; the runtime is single-threaded by design.
- **Typed AST: generated from an `ungrammar` grammar file** (`ascript.ungram`) via a `build.rs`
  codegen step. ungrammar describes only AST *shape* (it is not a parser and encodes no tokens/
  precedence). Validated by Biome, which autogenerates its typed API from a grammar over a
  rowan-style fork. Chosen over hand-written wrappers because the language is evolving quickly
  (match patterns, spread, schema all landed recently) and codegen keeps the typed layer from
  drifting.

### State-of-the-art validation

- **Biome:** internal rowan fork, green/red trees, typed API autogenerated from grammar — i.e.
  exactly this architecture.
- **Ruff:** hand-written recursive-descent parser → CST → AST — exactly this parser strategy.

Sources: biomejs.dev/internals/architecture, deepwiki.com/astral-sh/ruff, github.com/domenicquirl/cstree.

### Modules

- `src/syntax/` (new): `kind.rs` (the flat `SyntaxKind` enum — all token + node kinds, the contract
  between lexer/parser/cstree/codegen), `lexer.rs` (emits trivia), `parser.rs` (event-emitter +
  recovery), `cst.rs` (cstree `Language` impl, `SyntaxNode`/`SyntaxToken` aliases),
  `ast/` (generated typed wrappers + `ascript.ungram`).
- `build.rs`: gains the ungrammar codegen step (alongside the existing tree-sitter `cc` compile).
- `src/interp.rs`: consumes the typed AST via an execution view (below).
- `src/fmt.rs`: rewritten to walk the CST and re-emit trivia.
- `src/repl.rs`, `src/lsp/*`: rewired onto the new pipeline.
- Old `src/lexer.rs`, `src/token.rs`, `src/parser.rs`, `src/ast.rs`: **coexist with the new path
  during branch development** (enables live old-vs-new differential testing) and are **deleted in
  the merge commit** — `main` never carries two front-ends.
- Vendored tree-sitter grammar: **retained** as an independent differential oracle in tests.

## Lexer & Parser

**Lexer.** Emits *every* lexeme as a token tagged with a `SyntaxKind`, including new trivia kinds
`LINE_COMMENT`, `BLOCK_COMMENT`, `WHITESPACE`, `NEWLINE`, and `SEMICOLON`. Nothing is discarded.
All existing tricky logic is preserved: template interpolation (`Full`/`Start`/`End`), hex/binary/
float scanning, unterminated-block-comment error. The `?`/ternary ambiguity remains a *parser*
concern.

**Parser.** Keeps the current precedence-climbing structure and the hand-tuned disambiguation
(`unwrap_tier`, `ternary`, `is_ternary_question`), but emits a `Start(kind)/Token/Finish` event
stream consumed by cstree's builder. Three deliberate changes:

1. **Error recovery, not first-error-abort.** On error, emit an `ERROR` node, skip to a recovery
   point (statement boundary / closing delimiter), and continue. Errors are collected in a side-
   list alongside the tree.
2. **Trivia attachment policy** (documented, standard): trailing trivia on the same line attaches
   to the preceding token; leading trivia (including blank lines) attaches to the next token's
   node. This makes comments survive the formatter's reorderings.
3. **The parser always produces a tree**, even for broken input (needed by LSP/check).

### Runtime invariant (non-negotiable)

Error recovery is a **tooling** capability, not a runtime one:

- **Run path** (`run_file`/`run_source`/`import`/REPL eval): if the error list is non-empty, report
  via ariadne and **refuse to execute**. A tree containing `ERROR` nodes is **never** handed to the
  interpreter. This preserves today's all-or-nothing fail-fast behavior exactly — a malformed
  program aborts before any side effect.
- **Tooling path** (`check`/LSP): consumes the same tree *plus* all errors to report everything and
  still analyze the good parts.

Regression gate: every existing "this is a syntax/parse error" test still aborts the run with the
same exit behavior.

## Interpreter execution view

The interpreter consumes the generated typed AST (CST-native). Hot-path cost is controlled in two
tiers:

- **Tier-1 value caching (folded into the migration).** Parsed numeric literals, resolved string/
  template escapes — pure functions of a node's own text, cached on the node (cstree custom-data).
  Restores the "parse-once" property the old AST had for free. Trivial to keep correct (node text
  is immutable for a given parse).
- **Tier-2 binding resolution (explicit second step — NOT deferred).** A real **name resolver**
  (scope analysis) computes "this identifier binds to that declaration"; results cached on nodes.
  The interpreter switches from env-chain walks to resolved bindings. The resolver is built as
  **shared infrastructure with a clean API**, explicitly so sub-project #3's checker consumes it
  instead of reimplementing it. (Decision: the resolver moved from #3 into #1 — building it now and
  sharing it is better than building it twice. "The cache" is just where the resolver's output
  lives; a binding cache without a resolver holds nothing, so "don't defer the cache" means "build
  the resolver now.")

Other invariants preserved: `SyntaxKind`-dispatch replaces `match ExprKind` arm-by-arm but each
arm's runtime semantics (`Flow`/`Control`, resource handling, async model) are unchanged. The
`Rc`/`RefCell`/`!Send` model, the take-out-across-await discipline, and the
`await_holding_refcell_ref` lint are unaffected (the CST is immutable, shared via handles; no new
borrow-across-await hazard).

## Formatter (acceptance gate)

The formatter walks the **CST** so trivia is present to re-emit. It remains opinionated (normalizes
whitespace, `;`→newline, fields-before-methods, `name?: T`→`name: T?`, quote escaping). New
responsibility: **thread comments through canonicalization by node attachment, not source
position** — so a comment on a method travels with that method when fields are sorted ahead of it.
(This is precisely why a position-keyed side-table was rejected.)

**Blank-line rule:** one blank line between items is significant and preserved; 2+ collapse to 1.
This is the only whitespace trivia the formatter reads; all other whitespace is normalized.

**Comment edge cases (each gets a test):**
- end-of-line `// c` after a statement → stays trailing on that line
- standalone `// c` above an item → leading, with a preserved blank line above
- comment inside an expression (`f(/* x */ a)`) → attached to inner token, re-emitted inline
- comment between a node and a reordered sibling → travels with its owner
- comment at EOF without trailing newline; leading header/license block at SOF → preserved verbatim
- multi-line block comment → interior preserved as-is (no reflow)

**Idempotence:** `fmt(fmt(x)) == fmt(x)` as a property test over the whole corpus *including
comments*.

**Acceptance gate:** for every `examples/**/*.as` and test fixture, `fmt` preserves every comment
and is idempotent. The migration is not done until this is green.

## Testing strategy — three equivalence oracles

1. **Differential parse oracle.** New parser vs the existing **tree-sitter grammar** must agree on
   accept/reject for every `examples/**/*.as` + fixtures (extends `treesitter_conformance.rs`,
   `frontend_conformance.rs`).
2. **Lossless round-trip.** Concatenating every token's text in the new CST must reproduce the
   source **byte-for-byte** — the property that *guarantees* no trivia is dropped (stronger than
   "comments look preserved").
3. **Behavioral equivalence.** Two complementary forms:
   - **Live old-vs-new** while both front-ends coexist on the branch: identical stdout/exit-code on
     the whole corpus.
   - **Recorded goldens** captured from `main` *before* changes (via Phase 9 `assert.snapshot`):
     the new impl must match byte-for-byte (survives the eventual deletion of the old path).
   - Full `cargo test` (~540+ tests) green with identical output in both feature configs.

## Rollout

- **Capture goldens on `main` first** (snapshot fixtures of current behavior); commit.
- **One feature branch**; old and new front-ends **coexist during development** (enables live
  differential testing). Old front-end is **deleted in the merge commit**.
- **Internal checkpoints on the branch** (each with its gates green, not separate merges):
  - **A — Front-end + interp to parity:** new lexer/parser/CST, ungrammar codegen, interp rewritten
    arm-by-arm with tier-1 value caching. Gate: oracles 1–3 green.
  - **B — Comment-preserving formatter:** rewrite `fmt` on the CST. Gate: comment-preservation +
    idempotence across corpus.
  - **C — Name resolver + binding cache:** shared resolver; interp uses resolved bindings. Gate:
    behavioral equivalence holds; resolver unit-tested.
  - **D — Cut over & delete:** point `main.rs`/`lib.rs`/`repl`/`lsp` at the new path, delete the old
    front-end. Gate: clippy clean in **both** feature configs, full suite green.
- **Performance gate:** after A's vertical slice runs, benchmark new vs old interpreter with
  `std/bench`. **A regression of more than 5% is unacceptable** and triggers the binding-cache work
  (and any further optimization) before proceeding — no perf wall discovered after the expensive
  rewrite. 5% is a hard ceiling, not a target.
- **Single merge to `main`** once all gates pass.
- **No drift management needed:** the project owner is holding `main` (no new features land) until
  this work merges, so the branch cannot diverge. No freeze window or periodic rebase is required.

## Risks

- **Effort, not design, is the dominant risk:** rewriting every `interp.rs` arm (6,507 lines) is the
  bulk of the work. Mitigated by the three-oracle net and arm-by-arm equivalence.
- **Performance:** addressed by cstree's persistent red nodes + tier-1 caching, with the resolver/
  binding-cache and an early benchmark as the backstop; **>5% regression is a hard fail.**
- **Branch drift:** not a risk — `main` is frozen by the project owner until this lands.
- **Trivia attachment subtleties** (esp. around reordering): addressed by the documented attachment
  rule + the enumerated formatter edge-case tests + lossless round-trip.

## Docs to update (within this sub-project)

- `CLAUDE.md`: rewrite the front-end architecture section and the "two parsers" note → "CST pipeline
  (cstree + ungrammar codegen) + tree-sitter test oracle"; note the runtime invariant and the
  trivia model.
- New ADR recording the cstree-over-rowan and ungrammar-codegen decisions and their rationale
  (this document's Decisions).

## Decisions (log)

- **Checker scope target:** up to and including static contract checking (informs the decomposition;
  contract checking is sub-project #5).
- **Trivia model:** full lossless CST (chosen over attach-to-AST-node and position-keyed side-table).
- **Interpreter:** CST-native typed accessors (chosen over lower-CST-to-existing-AST).
- **Tree library:** `cstree` (chosen over rowan for persistent red nodes + interning + custom data;
  over eventree for maturity).
- **Typed AST:** ungrammar grammar-driven codegen (Approach C; chosen over hand-written wrappers and
  over a no-codegen approach).
- **tree-sitter:** retained as a test-only differential oracle (Option X; tree-sitter-as-CST
  rejected due to runtime C-FFI on the hot path, untyped/coarser trees, loss of hand-tuned control).
- **Name resolver:** built in #1 as shared infrastructure (moved out of #3); tier-1 value caching
  folded into the migration, resolver + binding cache as an explicit second step — neither deferred.
- **Rollout:** coexist during branch development for live differential testing; delete old front-end
  in the merge commit; single merge to `main`; never two front-ends on `main`.
