# CST Front-End Migration — Design Spec

**Date:** 2026-06-02
**Status:** Approved (brainstorming complete; Plan 1 written)
**Sub-project:** #1 of a larger "powerful tooling + runtime" effort (see Decomposition)

> **REVISION 2026-06-02 — runtime pivot.** After this spec was first approved, we revisited the
> interpreter representation. The runtime will **not** execute the CST directly. Instead the CST is
> the *tooling* source-of-truth (formatter/checker/LSP), and the runtime is rebuilt as a **bytecode
> compiler + virtual machine (model "2a": an async dispatch loop that keeps tokio / `spawn_local` /
> cancel-on-drop)**. The bytecode VM is **pulled forward** to be the next major phase after the CST
> foundation + resolver. The VM itself (and its async/generator integration) is large enough that it
> gets its **own dedicated design spec**; this document records the decision, the revised
> decomposition, and the unchanged front-end foundation. **Plan 1 (the lossless lexer) is unaffected
> by the pivot.** Durable/serializable continuations + deterministic scheduling (model "2b") remain
> explicit non-goals.

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
- A **shared name resolver** built as reusable infrastructure (consumed by the **bytecode compiler**
  for local-slot allocation, and by the checker later).
- Foundations that make both the **bytecode VM** and the later checker sub-projects straightforward
  to build (a clean typed AST + resolved bindings is exactly what the compiler consumes).

> The runtime goal — *no observable behavior change, plus a measurable speed-up* — now belongs to the
> bytecode-VM phase (its own spec), not to this front-end sub-project.

## Non-Goals (this sub-project)

- The `ascript check` CLI command and lint rules (sub-projects #2–#5).
- Static contract checking (sub-project #5).
- Collapsing the tree-sitter grammar into the new pipeline (it stays as a test-only oracle; any
  future unification is a separate decision).
- Incremental/streaming reparse (cstree supports the shape; not needed yet).
- **The bytecode compiler + VM itself** — pulled forward as the next phase, but designed in its own
  dedicated spec; this document only records the decision + revised ordering.
- **Model "2b" durable execution** (serializable continuations, deterministic/replayable scheduling).
  The VM is built as model "2a" (async loop borrowing tokio's suspension). 2b stays a non-goal,
  consistent with the existing async non-goals in CLAUDE.md §7.

## Decomposition (full effort)

This spec covers **sub-project #1 only**. The whole effort decomposes into independent
spec → plan → build cycles. **Revised ordering (runtime pulled forward):**

1. **Lossless CST + typed-AST front-end** (this spec) — lexer (trivia) → event parser → ungrammar
   codegen → **shared name resolver** → **comment-preserving formatter** (the user-visible deliverable
   + CST acceptance gate). Foundation for everything below.
2. **Bytecode compiler + VM (model 2a)** — *own dedicated spec.* Compiler lowers the typed AST
   (using the resolver's slot allocation) to bytecode; an async dispatch-loop VM executes it,
   replacing the tree-walker. Keeps tokio / `spawn_local` / cancel-on-drop / generators / `http.serve`.
   The async + generator integration is the high-risk part and may be split into its own sub-spec.
   *Pulled forward ahead of the checker per project-owner decision.*
3. **`ascript check` CLI + shared analysis core** — feature-independent diagnostic type, ariadne
   CLI renderer with exit codes, all syntax errors (not just first), LSP rewired onto the core.
4. **Scope & control-flow lints** — *uses* the resolver from #1 (undefined/unused/shadowed/
   unused-import, unreachable code, non-exhaustive match, missing return).
5. **AScript-specific lints** — unawaited `future`, ignored `Result`/`?`/`!`, dead `recover`.
6. **Static contract checking** — best-effort verification of gradual type contracts / `T?` /
   schema-class shapes, with an explicit false-positive philosophy.

Ordering is dependency-driven; #1 is the load-bearing foundation (the compiler in #2 needs its typed
AST + resolved bindings). The resolver was moved from the lint phase into #1 (see Decisions) and now
serves the compiler first.

## Architecture

Layered front-end; each layer has one job and a typed interface to the next:

```
source text
  → LEXER        flat token stream INCLUDING trivia (comments, whitespace, newlines, `;`)
  → PARSER       recursive descent, emits Start(kind)/Token/Finish events; error-recovering
  → CST          cstree green/red tree (lossless); built by cstree's builder from the events
  → TYPED AST    thin typed wrappers over CST nodes, GENERATED from an ungrammar grammar file
        │
        ├──► TOOLING:   fmt (walks CST, prints trivia), checker, lsp        [reads CST/AST]
        │
        └──► RUNTIME:   RESOLVER (binding → slot) → COMPILER → bytecode → VM (model 2a)
```

The CST is the **tooling** source-of-truth (fidelity, positions). The **runtime does not execute the
CST**; it executes bytecode produced by lowering the typed AST. This separation is the industry norm
(rust-analyzer: CST → HIR; CPython: AST → bytecode; Roslyn: syntax tree → IL) and lets each side
optimize without compromising the other.

### Library & codegen choices

- **Tree library: `cstree`** (a fork of rowan by the rust-analyzer authors). Chosen over rowan
  specifically because:
  - cstree **persists red nodes and returns references**; rowan re-creates the red layer (cloning
    nodes) on every traversal. For a tree-walking interpreter that visits nodes on the hot path,
    this directly mitigates the primary performance risk.
  - cstree **interns token strings** — deduplicates repeated identifiers (memory + comparison win;
    pairs with the runtime's existing `Rc<str>` interning).
  - cstree nodes can carry **custom data** — useful for tooling annotations.
  - Cost: no mutable-tree API. Not a real loss — autofix (later) uses edit-based rewriting like
    ruff/biome, not in-place mutation.
  - `!Send`/`!Sync` of the chosen config is irrelevant; the runtime is single-threaded by design.

  > **Post-pivot note:** the "persistent red nodes mitigate interpreter hot-path cost" rationale no
  > longer applies (the runtime executes bytecode, not the CST). cstree remains the right choice for
  > the **tooling** layer (losslessness + string interning + mature green/red API); rowan would also
  > be acceptable now, but there's no reason to switch.
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
- `src/fmt.rs`: rewritten to walk the CST and re-emit trivia.
- `src/repl.rs`, `src/lsp/*`: rewired onto the new pipeline (parse path).
- **Runtime (sub-project #2, separate spec):** a new compiler + VM replaces `src/interp.rs`'s
  tree-walker. Reuses `src/value.rs` and the entire `src/stdlib/*` unchanged. Out of scope for this
  document beyond the decision record below.
- Old `src/lexer.rs`, `src/token.rs`, `src/parser.rs`, `src/ast.rs`: **coexist with the new path
  during branch development** (enables live old-vs-new differential testing) and are **deleted in
  the merge commit** — `main` never carries two front-ends.
- Vendored tree-sitter grammar: **retained** as an independent differential oracle in tests.

## Lexer & Parser

**Lexer.** Emits *every* lexeme as a token tagged with a `SyntaxKind`, including new trivia kinds
`LINE_COMMENT`, `BLOCK_COMMENT`, `WHITESPACE`, and `NEWLINE`. (`;` is **not** trivia — it is a normal
structural `SEMICOLON` token that the *formatter* canonicalizes to a newline; it is not attached as
trivia.) Nothing is discarded.
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

## Name resolver (built in this sub-project)

The runtime no longer "caches values on CST nodes." Instead, the front-end builds a **name resolver**
— a scope-analysis pass over the typed AST that maps every identifier use to its declaration and
assigns each local a **slot index** within its function frame. This is the artifact the bytecode
compiler needs to emit fast slot-based loads/stores instead of name lookups.

- Built as **shared infrastructure with a clean API**: the **compiler** (sub-project #2) consumes the
  slot allocation; the **checker** (sub-project #4) consumes the same resolver for
  undefined/unused/shadowed diagnostics. Build once, two consumers.
- Parsed-literal values (numbers, string/template escapes) are no longer "node caches" — they become
  **compile-time constants in the bytecode constant pool**, parsed exactly once during compilation.
  This subsumes the old "tier-1 value caching" idea more cleanly than node-attached caches.

> The previous "Interpreter execution view" (CST-native typed accessors + node-attached value/binding
> caches) is **superseded by the bytecode-VM pivot**. The runtime semantics (`Flow`/`Control`,
> resource handling, the `Rc`/`RefCell`/`!Send` async model, take-out-across-await discipline) are
> preserved by the VM and specified in the dedicated bytecode-VM spec, not here.

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
3. **Resolver cross-check.** The new resolver's binding decisions must agree with the legacy
   interpreter's scoping on the corpus (no use-before-def divergence, same shadowing). Plus resolver
   unit tests. *(Execution-behavioral equivalence belongs to sub-project #2 — see below.)*

> **Note on behavioral equivalence:** because this sub-project does **not** change execution (the
> legacy interpreter keeps running the binary; only `fmt`/LSP/checker move onto the new pipeline),
> *runtime* behavioral equivalence + the perf benchmark are owned by the **bytecode-VM spec (#2)**,
> where the VM replaces the tree-walker. There, the gate is byte-identical stdout/exit-code vs
> recorded `main` goldens **and a measurable speed-up** (a bytecode VM is expected to be *faster*,
> not within 5% — the 5%-regression ceiling was an artifact of the superseded CST-native-interp plan).

## Rollout (this sub-project = front-end #1)

- **Capture goldens on `main` first** (snapshot fixtures of current behavior); commit. These serve
  the VM spec (#2) later.
- **One feature branch.** The new front-end is added; the **legacy lexer/parser/AST + the
  tree-walking interpreter stay in place and keep running the binary** — only `fmt`, the LSP parse
  path, and (later) the checker move onto the new CST pipeline.
- **Internal checkpoints on the branch** (each with its gates green):
  - **A — New parse pipeline:** trivia lexer, event parser, cstree CST, ungrammar codegen. Gate:
    losslessness (oracle 2) + differential parse oracle (oracle 1) green over the corpus.
  - **B — Name resolver:** scope analysis + per-frame slot allocation, as shared infrastructure.
    Gate: resolver unit tests + resolver cross-check (oracle 3) green.
  - **C — Comment-preserving formatter:** rewrite `fmt` on the CST; point `ascript fmt` at it. Gate:
    comment-preservation + idempotence across corpus.
  - **D — Wire LSP/REPL parse path** onto the new pipeline. Gate: clippy clean in **both** feature
    configs, full suite green.
- **No drift management needed:** the project owner is holding `main` (no new features land) until
  the whole effort merges, so the branch cannot diverge.

> **OPEN DECISION (carried to the #2 spec) — "no two front-ends on `main`".** The earlier rule was
> "delete the legacy front-end in the merge commit." Pulling the VM forward splits the work: #1
> (front-end) leaves the legacy *parser+interp* in place to keep execution working; only #2 (the VM)
> can finally delete the legacy lexer/parser/AST/interp once the VM consumes the new typed AST. Two
> options, to confirm when speccing #2: **(i)** merge #1 and #2 together on one long branch so `main`
> never carries two parsers (honors the original preference; one big merge — fine since `main` is
> frozen), or **(ii)** merge #1 first (transient dual-parser window on `main`), then #2 deletes the
> legacy path. Default assumption: **(i)**.

## Risks

- **Trivia attachment subtleties** (esp. around the formatter's reordering): addressed by the
  documented attachment rule + the enumerated formatter edge-case tests + lossless round-trip.
- **Two parsers transiently** (legacy for execution, new for tooling) until the VM lands: bounded by
  the OPEN DECISION above; differential parse oracle keeps them in agreement meanwhile.
- **Resolver/legacy-scope divergence:** caught by the resolver cross-check oracle.
- **The dominant *effort* risk has moved to sub-project #2** (compiler + VM + async/generator
  integration) — tracked in its own spec, not here.

## Docs to update (within this sub-project)

- `CLAUDE.md`: rewrite the front-end architecture section and the "two parsers" note → "CST pipeline
  (cstree + ungrammar codegen) + tree-sitter test oracle"; note the runtime invariant and the
  trivia model.
- New ADR recording the cstree-over-rowan and ungrammar-codegen decisions and their rationale
  (this document's Decisions).

## Decisions (log)

- **Checker scope target:** up to and including static contract checking (informs the decomposition;
  contract checking is the last sub-project).
- **Trivia model:** full lossless CST (chosen over attach-to-AST-node and position-keyed side-table).
- **Runtime representation (REVISED):** **bytecode compiler + VM** consuming a *lowered* IR — the
  runtime does **not** execute the CST. *Supersedes the earlier "CST-native typed accessors"
  decision*, which was identified as executing the tooling tree (against the grain; the whole industry
  lowers CST → execution IR). The CST remains the tooling source-of-truth only.
- **VM suspension model:** **2a** — async dispatch loop that borrows tokio's suspension (keeps
  `spawn_local`/`Value::Future`/`await`/cancel-on-drop/generators/`http.serve`). Chosen over **2b**
  (explicit-stack VM with serializable continuations + deterministic scheduling), which stays a
  non-goal. Bytecode/VM is **pulled forward** to be sub-project #2, ahead of the checker.
- **Tree library:** `cstree` (interning + lossless green/red API + maturity; over rowan and eventree).
  *Post-pivot:* the interpreter-hot-path rationale is moot, but cstree remains fine for tooling.
- **Typed AST:** ungrammar grammar-driven codegen (Approach C; over hand-written wrappers and no
  codegen).
- **tree-sitter:** retained as a test-only differential oracle (Option X; tree-sitter-as-CST rejected
  due to runtime C-FFI, untyped/coarser trees, loss of hand-tuned control).
- **Name resolver:** built in #1 as shared infrastructure; now serves the **compiler** (slot
  allocation) first and the checker later. Parsed-literal values become **constant-pool entries** at
  compile time (supersedes node-attached "tier-1 value caching").
- **Rollout (REVISED):** legacy parser+interp stay in #1 to keep execution working; only the VM (#2)
  can delete the legacy path. "Never two front-ends on `main`" is now an OPEN DECISION carried to the
  #2 spec (default: develop #1+#2 on one branch, single merge, delete legacy then).
