# AScript DX — Doc-gen, Test Framework, LSP Completion & Diagnostics — Design (DX)

- **Status:** Draft for review
- **Date:** 2026-06-08
- **Code:** DX (continuous-infrastructure pillar of the Serious Language campaign — see `goal.md`)
- **Kind:** **Umbrella / continuous infrastructure.** This is NOT a single feature on a single branch.
  It defines the developer-experience surface (`ascript doc`, the test framework, the LSP, diagnostics)
  and the *standard every other spec is held to*: any spec that adds a construct (NUM, ADT, IFACE, TYPE,
  FFI, SRV, BIN) updates the DX surface for that construct **in the same PR** (Gate 8). The sub-deliverables
  below may land incrementally on their own branches; this spec is the shared design they conform to.
- **Depends on:** nothing hard (stands up alongside NUM). Each sub-deliverable is *fed* by other specs:
  TYPE supplies inlay/hover types; ADT/IFACE/NUM/FFI/SRV each add doc/hover/completion surface for their
  constructs. The test-parallelism deliverable builds on the **shipped** `src/worker/` subsystem.
- **Depended on by:** nothing downstream blocks on DX; DX is the quality bar, not a capability gate.
- **Engines:** **none changed.** `ascript doc`, the LSP, and diagnostics are *static* (CST + checker, never
  the interpreter). The test runner is a *harness* over the existing engines — it adds no opcode, no `Value`
  kind, no `.aso` change, and therefore is outside the four-mode byte-identity gate **except** that test
  *programs* still run on the production engine and their assertions observe identical behavior.
- **Breaking:** no. Additive CLI flags, a new `ascript doc` subcommand, an additive `///` doc-comment
  convention (a `///` line is a `LineComment` today — strictly reinterpreted, never re-tokenized).

---

## 1. Summary & motivation

Pillar 2 of `goal.md` — **developer experience** — is declared *continuous infrastructure*: "First-class
docs (`ascript doc`), a deep test framework, a complete LSP, and excellent diagnostics ... updated by every
spec that adds surface." Today three of those four are partial, and the fourth (the LSP) is more complete
than the campaign brief assumed but carries a real architectural seam. Concretely, grounded in the tree:

- **No doc-gen at all.** There is no `ascript doc` subcommand (`src/main.rs` `Command` enum has
  `run/build/repl/fmt/check/test/lsp` + the `pkg` commands — no `doc`). The user-facing site under `docs/`
  is *hand-written* Markdown; nothing extracts API reference from source. A serious language generates its
  own API docs (rustdoc, godoc, swift-doc).
- **A basic test runner.** `test(name, fn)` registers into `Interp.tests` (`src/interp.rs:445`,
  `:4705`); `run_registered_tests` (`src/interp.rs:2269`) runs them **serially on one `Interp`** and returns
  a `TestSummary { passed, failed, failures }` (`src/interp.rs:780`). `ascript test` (`src/main.rs`) loads
  files then prints the summary. `std/assert` already has deep-equality assertions **and a working
  `assert.snapshot(name, value, update?)`** with a `__snapshots__/` store (`src/stdlib/assert_mod.rs:36`).
  What is missing: **parallel** test-file execution (we now have shared-nothing isolates — `src/worker/` —
  whose stateless pool *enables* parallel test-file dispatch, even though the workers spec does not itself
  claim this workload), **coverage**, a **watch** mode, **test
  filtering/focus**, richer **assertion diffs**, and a `--update-snapshots` CLI flag (the capability exists
  per-call but is not wired to a CLI switch or `test`-level grouping).
- **The LSP is already large** — `src/lsp/` advertises ~24 capabilities (`server.rs` `ServerCapabilities`:
  hover, completion+resolve, definition/declaration/typeDefinition/implementation, references, rename+prepare,
  documentHighlight, document/workspace symbols, callHierarchy, foldingRange, selectionRange, documentLink,
  formatting (full+range), codeAction+resolve, codeLens+resolve, color, linkedEditingRange, pull
  diagnostics, signatureHelp, **inlayHint** (already fed by SP10 `infer::hover_type_at`,
  `src/lsp/providers/inlay.rs`), semanticTokens). Crucially, the campaign brief's premise — "navigation is on
  the legacy AST" — is **stale**: `src/lsp/providers/mod.rs` states every provider is "over the cached model
  — no provider re-parses or touches the legacy `crate::{ast,lexer,parser}`," and both `navigation.rs` and
  `workspace.rs` consume `crate::syntax::parser::parse` + `crate::syntax::resolve::resolve`. **The whole LSP
  is already off the legacy front-end.** The real seam is described in §4 (two CST-based resolution systems,
  not CST-vs-legacy) — we restate the deliverable accurately rather than fix a problem that no longer exists.
- **Diagnostics are single-error, single-caret.** `src/diagnostics.rs::report` renders **one** `AsError`
  via ariadne with one red label. There is no multi-error batching at the CLI, no "did you mean" typo
  suggestion anywhere (`grep` finds only the `unawaited` rule's fixed hint), and the documented **SP1
  1-column caret offset** between the CST and legacy front-ends is still open (`CLAUDE.md` "Accepted SP1
  trade-offs").

This spec turns the DX pillar into a concrete, conformant surface and writes down the convention every
future construct plugs into.

### Sub-deliverables (may land incrementally)

| # | Deliverable | Branch-able independently? | Fed by |
|---|---|---|---|
| D1 | `ascript doc` + the `///` doc-comment convention | yes | every construct spec (signatures, types) |
| D2 | Test framework depth (parallel/snapshot/coverage/watch/diffs/filter) | yes (per capability) | `src/worker/` (parallel); TYPE (typed diffs) |
| D3 | LSP semantic-resolver unification + cross-file completeness + inlay | yes | TYPE (inlay/hover types); ADT/IFACE/FFI (completion items) |
| D4 | Diagnostics quality (multi-error, did-you-mean, caret-offset resolution) | yes | NUM/TYPE (new diagnostic codes ride the same renderer) |

## 2. The doc-comment convention (LOCKED — the cross-cutting decision)

**`///` is the line doc-comment; `//!` is the inner/module doc-comment.** Both are parsed from CST trivia,
not introduced as new tokens. Decision and rationale:

- A `///`-prefixed line is, lexically, a `LineComment` (`src/syntax/kind.rs:97`) — the CST already preserves
  it losslessly as leading trivia on the following node. **No grammar change, no new `SyntaxKind`, no
  tree-sitter regen for the convention itself.** Doc-gen and the LSP reinterpret a `LineComment` whose text
  starts with `///` (and not `////`) as a doc-comment; `//!` at the top of a file / block is the module/inner
  doc. This mirrors **Rust** (`///` outer, `//!` inner) — the most recognizable convention for our audience —
  while reusing `//` line-comment lexing we already have.
- **Attachment rule (LOCKED, precise):** a run of consecutive `///` lines immediately preceding a
  declaration — with **no blank line** between the last `///` and the decl — is that declaration's doc. This
  is the CST leading-trivia of the decl node; the extractor walks trivia backward from the decl's first
  token, collecting the contiguous `///` run, and **stops at the first blank line**.
  - **"Blank line" is defined on the trivia stream, not re-tokenized:** a blank line is **≥ 2 consecutive
    `Newline` trivia tokens** (the CST preserves each newline as its own trivia; one `Newline` merely ends
    the previous line, two in a row means an empty line between content). Intervening **`Whitespace`**
    (indentation) trivia is ignored — a `///` line indented under the decl still attaches. So:
    `/// a`⏎`/// b`⏎`fn f` attaches both (single `Newline` between each); `/// a`⏎⏎`fn f` (a blank line, two
    `Newline`s) attaches nothing. The run is also broken by any **non-`///` non-trivia token** or a `////`
    line (four-or-more slashes is an ordinary comment, never doc — mirrors rustdoc).
  - A `///` not attached to a decl (e.g. before a statement, or separated by a blank line) is an ordinary
    comment for doc purposes (still highlighted, never extracted).
- **Markdown body.** The text after `/// ` (one leading space stripped if present) is **Markdown**, rendered
  by the same renderer the `docs/` site uses, so code fences, links, and emphasis work. The first paragraph
  is the **summary** (used in symbol lists / hover one-liners); the rest is detail.
- **Optional structured tags (additive, recognized but never required):** `@param name — text`,
  `@returns — text`, `@example` (a following fenced block), `@deprecated — text`, `@see slug`. These are a
  *convenience overlay* on the Markdown, not a separate syntax; an undocumented `@foo` renders literally.
  (Rejected: a javadoc-style mandatory tag grammar — too heavy; Markdown-first matches godoc/rustdoc.)
- **Why not a new `DocComment` `SyntaxKind`?** Considered and **deferred**: the tree-sitter grammar + both
  parsers + highlights would all need the regen/publish dance for zero behavioral gain (a `///` already
  highlights as a comment). If editors later want a *distinct* doc-comment highlight color, that is a purely
  additive `highlights.scm` predicate (`(line_comment) @comment.documentation (#match? @... "^///")`) — a
  small follow-up, not a blocker. The convention ships on trivia reinterpretation first.

This convention is the contract **every construct spec extends**: a documented `worker fn`, an ADT `enum`
variant, an `interface` method, a generic `fn<T>`, a typed field — each gets its doc from the `///` run above
it, and each new construct's doc-gen rendering (§3.3) and LSP hover (§4) MUST render that doc.

## 3. `ascript doc` — API documentation from the CST

### 3.1 CLI surface

A new `src/main.rs` subcommand:

```
ascript doc [PATHS...] [--out DIR] [--format html|md] [--private] [--open] [--check]
```

- `PATHS` — files or directories (default: the entry/project root, discovered like `check`/the workspace
  index, reusing `workspace::discover_as_files`). Resolves imports to document a whole project.
- `--out DIR` — output directory (default `target/doc/`).
- `--format html|md` — HTML (default; mirrors the `docs/` site, §3.4) or Markdown (for embedding / the
  in-repo reader).
- `--private` — include non-exported declarations (default: **public API only** — see §3.2).
- `--open` — open the generated `index.html` (best-effort, `sys`-gated).
- `--check` — **doc lint / no write**: exit non-zero if a public declaration lacks a doc-comment (a CI gate
  for "the stdlib is documented"); reports the undocumented symbols. Pairs with the future self-hosted
  stdlib.

`ascript doc` is **static**: it runs the CST front-end + the SP10 inferencer, **never the interpreter** —
same `Send`-able, runtime-free posture as the LSP (`src/lsp/mod.rs` note). It lives in a new `src/doc/`
module (CLI-side, feature-gated like `lsp`/`pkg` so `--no-default-features` need not build it).

### 3.2 What is documented (public API)

"Public" = a top-level declaration that is **exported** (the `export` marker the workspace index already
tracks via `FileIndex.exports`, `src/lsp/workspace.rs:80`) OR a `pub`-equivalent surface the module exposes
on `import`. Covered declaration kinds (each reuses the CST `decl_kind` walk in `workspace.rs:823`):

- **functions** (incl. `async fn`, `worker fn`, `static worker fn`, generators `fn*`, and — post-TYPE —
  `fn<T>` generics): rendered with their **signature** (params + annotated types; **post-TYPE, the
  inferred return type** via `infer`), the `worker`/`async`/`static` modifiers, and the doc body.
- **classes** (fields with types/defaults, `init`, methods, `static` members, inheritance) — fields use the
  `FieldDecl`/`FieldSchema` info; each method gets its own doc.
- **enums** — variants; **post-ADT**, variant payload types (`Circle(r: float)`) are part of the rendered
  signature, each variant individually documentable.
- **interfaces** (post-IFACE) — the structural method set, each method's signature + doc.
- **type aliases / declared types** (post-TYPE) — the aliased type, generic params.
- **constants** — value-or-type and doc.

The extractor is a pure CST walk; **it reuses the existing `symbols`/`docs` providers' machinery**
(`src/lsp/providers/symbols.rs`, `docs.rs`) rather than a second parse — see §6 rejected alternatives.

### 3.3 Signature & type rendering

Signatures come straight from the CST node text (annotations as written). **Where a type is unannotated and
TYPE has landed,** the rendered signature appends the *inferred* type in a visually distinct style (e.g.
`fn add(a, b) ⟶ inferred (a: number, b: number) -> number`), reusing `infer::hover_type_at` — the **same
source the LSP inlay/hover already uses** (`src/lsp/providers/inlay.rs`). Before TYPE, only declared
annotations render (no inference column). This is the concrete way "post-TYPE, inferred/declared types"
flows into docs without a second type engine.

### 3.4 Output & site integration

- **HTML output** mirrors the existing static site's look: it reuses `docs/assets/styles.css` and a
  cmd-K-style index, so generated API docs are visually consistent with `docs/content/`. The generator emits
  one page per module + an index, with **cross-links** (a use of a documented symbol links to its def page;
  resolution reuses the workspace index's cross-file targets, `ResolvedTarget`).
- **Markdown output** is plain `.md` per module, readable straight from the repo (matching the `docs/`
  philosophy that Markdown is readable without serving).
- **The `NAV`-array gotcha (called out per the brief + `CLAUDE.md`/memory):** if `ascript doc` output is
  ever merged *into* the hosted `docs/` site (e.g. an auto-generated "stdlib API" section), **every new page
  slug MUST be added to the `NAV` array in `docs/assets/app.js`** — the sidebar AND cmd-K search both derive
  from `NAV`; a page with no `NAV` entry is unreachable (no link, no search hit). The **default**, however,
  is to emit to a **separate** `target/doc/` tree with its **own** self-contained index (no `NAV`
  dependency) — so generated docs never silently orphan and never need a manual `NAV` edit unless we
  deliberately fold them into the hand-written site. This is the safe default; folding-in is an explicit opt.

## 4. LSP completion — the semantic-resolver story (restated accurately)

### 4.1 What the brief calls "split-brain" — the real seam

The campaign brief says "navigation is still on the legacy AST while diagnostics use the new resolver." That
is **no longer true** and we correct it here: per `src/lsp/providers/mod.rs` and `src/lsp/mod.rs`, **no LSP
provider touches `crate::{ast,lexer,parser}`** — every provider runs on the cached `SemanticModel` (CST +
`syntax::resolve`) and the `WorkspaceIndex` (also CST + resolver). The legacy AST migration is **done**.

The **actual** seam (the thing worth fixing) is that there are **two CST-based resolution systems** that do
not share a model:

1. **Per-file `SemanticModel.resolved`** — the frame-precise, scope-aware resolver result
   (`src/lsp/providers/navigation.rs`): `Resolution::{Local,Upvalue,Global,Unresolved}`, upvalue chains,
   slot identity. This is what in-file go-to-def / highlight / rename use, and it is **scope-correct**
   (shadowing, sibling same-name locals).
2. **`WorkspaceIndex`** (`src/lsp/workspace.rs`) — a *coarser* cross-file index built by its **own**
   name-walk (`collect_uses`, `workspace.rs:904`, tagging each use with `ResolvedTarget::{LocalDef,
   Imported, Other}`, `:44` + `:936-947`) + import edges. It is **name-and-export based**, not frame-precise:
   cross-file references and rename match on **name + import edge**, not on the per-frame binding identity the
   in-file resolver computes.

The seam: a symbol's identity is computed **twice, differently** — frame-precise in-file, name-coarse
cross-file. Edge cases (a cross-file rename that should NOT touch a same-named local in an importer; a
find-references that should include a frame-precise local *and* its cross-file uses uniformly) require both
halves to agree.

**Why the existing `BindingId` cannot be reused verbatim (the load-bearing correction).** The in-file
`BindingId` (`navigation.rs:71-75`) is `Local(TextRange)` | `Global(String)`. Neither variant carries a
*file*: a `TextRange` is a byte interval **into one file's text** (two unrelated files trivially produce the
same `0..5` range), and `Global(String)` is **name-only** — which is exactly the cross-file coarseness this
deliverable sets out to remove (it would rename every module-global named `x` in the project, and a
same-named local in an importer collides with it). So `navigation.rs`'s `BindingId` is correct *within one
`SemanticModel`* but is **not** a project-wide identity; using it cross-file would re-import the bug §9's
shadowed-local cross-file rename test targets.

**The deliverable is to unify identity on ONE semantic model via a NEW, file-qualified identity** that
*extends* (does not literally reuse) `navigation.rs`'s. Define a `GlobalBindingId` carried by the workspace
index:

- a **local/upvalue** is `(FileId, TextRange)` — the file plus the decl range, so identical ranges in
  different files are distinct and shadowed siblings stay distinct (the per-file `BindingId::Local(TextRange)`
  is the in-file projection of this, paired with the model's `FileId`);
- a **module-global / export** is `(definer-FileId, exported-name)` — keyed on the file that *defines* the
  binding plus its name, NOT the bare name. An importer's use resolves through its import edge to the
  **definer's** `FileId`, so `import {x} from "a"` and a local `let x` in the importer are different
  identities, and renaming the export touches only uses that resolve to *that* definer.

`FileId` is a stable per-file handle the index already has the material for (`WorkspaceIndex.files` is keyed
by canonical `PathBuf`, `workspace.rs:97`; a `PathBuf`→`FileId` interner gives the cheap copyable id). The
in-file resolver is unchanged — its `Resolution`/`BindingId` stay file-local; the workspace index *lifts*
each per-file binding into the file-qualified id when it joins files.

**LOCKED decision:** the unified identity is a **new `GlobalBindingId`** = `(FileId, TextRange)` for
locals/upvalues and `(definer-FileId, exported-name)` for globals/exports, built by lifting the per-file
`syntax::resolve` `Resolution` into file-qualified form. The workspace index becomes a cross-file *join* over
per-file resolver outputs (def/export table + use sites tagged by `GlobalBindingId` resolved through the
import edge), replacing the independent name-walk (`collect_uses`, `workspace.rs:904`, which today tags uses
by `ResolvedTarget::{LocalDef, Imported, …}` on name alone). This removes the divergence class without
re-introducing the legacy AST, without a third resolver, and — crucially — without the name-only / per-file
collisions the bare `navigation.rs` `BindingId` would carry across files.

### 4.2 Completion specifically

Completion (`src/lsp/providers/completion.rs`, already 23 KB) gains, on the unified model:
- **Scope-correct identifier completion** — offer exactly the bindings live at the cursor's frame (from the
  resolver's frame/upvalue chain), not a flat name list. Today's completion is already CST-based; this makes
  its candidate set frame-precise like navigation.
- **Member completion** — `.` after a value whose class/shape is known (from `infer`) offers that class's
  fields/methods; after a `std/*` module import, offers the module's exports.
- **New-construct items (fed by other specs):** `worker`/`static` modifiers (Workers — already wired per the
  workers spec), `int`/`float`/`number` type names (NUM), enum variants in `match` (ADT — exhaustiveness
  completion: offer the *missing* variants), `interface` names (IFACE), generic param names (TYPE). **Each
  construct spec adds its completion items here in its own PR** (Gate 8).

### 4.3 Inlay hints (from TYPE)

`src/lsp/providers/inlay.rs` already emits inferred-type hints at unannotated `let`/`const` and param-name
hints, fed by SP10 `infer::hover_type_at`. **Post-TYPE** this gets *better inputs* (sound generics, ADT/
IFACE types) automatically — no new wiring, the same provider renders richer types. The DX deliverable is to
keep inlay hints in lockstep with the inferencer: every new `CheckTy` kind a construct spec adds must render
a sensible inlay label (a `Display` for the new `CheckTy`), enforced by a provider test per construct.

### 4.4 The ~20-feature LSP surface (what ships)

Status grounded in `server.rs` `ServerCapabilities` today (✅ already advertised) vs DX work (🔧 unify on the
semantic model / complete cross-file / improve):

| # | LSP feature | Status |
|---|---|---|
| 1 | `textDocument/hover` (types via `infer`) | ✅ |
| 2 | `completion` + `completionItem/resolve` | ✅ → 🔧 frame-precise + member + construct items (§4.2) |
| 3 | `signatureHelp` | ✅ |
| 4 | `definition` | ✅ (in-file resolver; 🔧 unify identity with cross-file) |
| 5 | `declaration` | ✅ |
| 6 | `typeDefinition` | ✅ |
| 7 | `implementation` | ✅ (🔧 post-IFACE: interface→impls) |
| 8 | `references` (cross-file) | ✅ → 🔧 unify on the file-qualified `GlobalBindingId` (§4.1) |
| 9 | `rename` + `prepareRename` (cross-file) | ✅ → 🔧 unify on `GlobalBindingId` |
| 10 | `documentHighlight` | ✅ |
| 11 | `documentSymbol` | ✅ → 🔧 completeness for every new construct |
| 12 | `workspaceSymbol` + resolve | ✅ → 🔧 completeness |
| 13 | `callHierarchy` (prepare/incoming/outgoing) | ✅ |
| 14 | `foldingRange` | ✅ |
| 15 | `selectionRange` | ✅ |
| 16 | `documentLink` (import paths) | ✅ |
| 17 | `formatting` (full + range) | ✅ |
| 18 | `codeAction` + resolve (incl. `--fix`) | ✅ → 🔧 add did-you-mean quick-fixes (§5) |
| 19 | `codeLens` + resolve (run-test lenses) | ✅ → 🔧 "run test" / "run all tests" lens wired to the runner |
| 20 | `documentColor` | ✅ |
| 21 | `linkedEditingRange` | ✅ |
| 22 | pull `diagnostic` (doc + workspace) | ✅ |
| 23 | `inlayHint` + resolve (types from TYPE) | ✅ → 🔧 richer post-TYPE (§4.3) |
| 24 | `semanticTokens` | ✅ → 🔧 new-construct token types per spec |

So the LSP **ships nearly the whole surface today**; DX's job is **(a)** the file-qualified identity
unification (4, 8, 9),
**(b)** completion depth (2), **(c)** keeping 11/12/23/24 complete as constructs land, and **(d)** the
codeLens "run test" integration (19). Nothing is *added* to the capability list except as constructs require.

## 5. Diagnostics quality

### 5.1 Multi-error reporting

Today `diagnostics::report` renders **one** `AsError`. The checker (`src/check`) already produces *many*
diagnostics (it's the LSP's source) — the CLI just doesn't batch them for `run`/parse errors. Deliverable:

- **`ascript check`** already emits all diagnostics; keep that.
- **Parse/`run`-path errors:** where the front-end can recover (the CST parser is error-tolerant — it builds
  a tree with error nodes), collect **all** parse diagnostics and render them together via a new
  `report_all(&[AsError])` (ariadne supports multiple labels/reports). A single fatal runtime panic stays
  single-report (it's a Tier-2 abort, not a batchable static error). This makes "fix one error, recompile,
  find the next" into "see them all at once."

### 5.2 "Did you mean" suggestions

No typo suggestions exist today. Add an **edit-distance suggestion** (Levenshtein ≤ 2, or ≤ ⌈len/3⌉) on the
two highest-value error classes:
- **Unresolved name** (`Resolution::Unresolved`): suggest the closest in-scope binding / builtin / import.
  The resolver already knows the candidate set (scope chain + builtins list `interp.rs:133`).
- **Unknown member / unknown `std/*` export**: suggest the closest field/method/export.

Rendered as an ariadne `help`/note line (`unknown name 'lenght' — did you mean 'length'?`) and, in the LSP,
as a `codeAction` quick-fix (feature 18). This is a small, shared `suggest::closest(name, &candidates)`
helper used by both the CLI renderer and the checker rules.

### 5.3 The SP1 1-column caret offset — RESOLVE, don't accept

`CLAUDE.md` records a "1-column caret-span offset between the CST and legacy front-ends in diagnostics
(message always correct, only the caret column can be off by one)." Now that **the entire LSP and all static
tooling are on the CST front-end** (§4.1) and the legacy front-end is only the runtime oracle, the offset is
a CST-vs-legacy *span-origin* inconsistency that a serious-language diagnostics pass should close, not
enshrine. Deliverable:

- **Audit** where the off-by-one originates (CST byte-range → char-offset `Span` conversion vs the legacy
  lexer's char spans; `src/lsp/convert.rs` + `src/diagnostics.rs::char_to_byte`). The likely culprit is an
  inclusive/exclusive or 0/1-based column boundary in one front-end's span construction.
- **Fix it** so the caret lands on the exact column in **both** front-ends, with a golden test that pins the
  caret column for a representative error in each front-end and asserts they match.
- If, after the audit, the offset proves to be an irreducible artifact of the two independent lexers (e.g. a
  deliberate span convention the runtime depends on), **formally accept it** with a one-line owner note and a
  test that pins the *current* behavior — but the default posture is **fix**, per the no-bugs/DX pillars.

### 5.4 Message clarity pass

A sweep over the highest-traffic Tier-2 panic messages and checker diagnostics for consistency: include the
offending value's `type_name`, the expected shape, and (where applicable) a `help` line. This is ongoing —
each construct spec writes clear messages for its new errors (NUM's overflow/`int`-index panics, ADT's
non-exhaustive-match, IFACE's conformance, TYPE's `type-*`), and DX owns the *style guide* they follow.

## 6. Test framework depth

The runner stays the existing model — `test(name, fn)` registration (`interp.rs:4705`) +
`run_registered_tests` (`interp.rs:2269`) + `TestSummary` — extended as follows. CLI surface (`src/main.rs`
`Test`) grows flags:

```
ascript test [FILES...] [--parallel[=N]] [--update-snapshots] [--coverage[=text|lcov|html]]
             [--watch] [--filter PATTERN] [--locked]
```

### 6.1 Parallel test FILES across worker isolates (the natural fit)

The shipped `src/worker/` isolate pool *enables* this (the workers spec defines the stateless pool and its
cost model but makes no claim about test execution — that workload is **ours** to layer on): **distribute
test *files* across `src/worker/` isolates**. Each test file is a shared-nothing unit (its own module load +
its own registrations), so running file A's tests on isolate 1 and file B's on isolate 2 is exactly the
stateless isolate model — **no new concurrency primitive**, reuse `src/worker/pool.rs` + the
dispatch/code-slice machinery.

- **Granularity = per file**, not per `test()`. A file is the isolation boundary that already matches the
  structured-clone airlock: we ship the file path to an isolate, it loads + runs that file's registered
  tests in its own `Interp`, and returns the result across the airlock. **Note: `TestSummary` is NOT itself
  Sendable** — it is a plain Rust struct (`interp.rs:779-785`: `passed`/`failed`/`failures`), not `Clone`,
  not `Serialize`, and **not a `Value`**, and the worker airlock crosses **`Value` only** (`encode(v:
  &Value)`, `serialize.rs:360`; `check_sendable` rejects everything that is not a sendable `Value`). So the
  isolate **encodes its `TestSummary` as a `Value::Object`** — `{passed: number, failed: number, failures:
  array<{name, message}>}` (an insertion-ordered `Object`, all leaves being sendable `Value` kinds: numbers,
  strings, arrays, objects) — ships *that* `Value` back, and the parent **decodes the `Object` back into a
  `TestSummary`** for aggregation. This is the one airlock-shaping detail the parallel runner owns; it adds
  no new sendable kind (it is an ordinary `Object`).
- **`--parallel[=N]`** — N defaults to `num_cpus` (cap via `ASCRIPT_WORKERS`, the existing env var). Serial
  remains the default for a single file / when isolates would cost more than they save (the cost model from
  the workers spec: ~0.5–2 ms isolate birth — parallelize *files*, the coarse unit, not individual tests).
- **Deterministic aggregation (REQUIRED, §7):** files dispatch in parallel but **results aggregate in a
  stable order** (input file order, then registration order within a file) before printing, so the summary
  and exit code are byte-identical regardless of completion order. This is the determinism contract for the
  parallel runner — timing is nondeterministic, *output* is not (same discipline the workers differential
  tests use: gather preserves order).
- A test file that contains a `worker fn` and itself dispatches to the pool runs its workers **inline** in
  its test isolate (the workers-spec nested-inline rule) — no deadlock, no pool reservation.

### 6.2 Snapshot testing

`assert.snapshot(name, value, update?)` already exists (`assert_mod.rs:36`, `__snapshots__/` store, mismatch
diff). DX completes it into a *framework feature*:
- **`--update-snapshots`** CLI flag sets the per-call `update` for the whole run (an `Interp`-level "update
  mode" the `assert.snapshot` handler reads), so `jest -u`-style bulk updates work without editing source.
- **Obsolete-snapshot detection** (`--check`-style): a snapshot file with no corresponding assertion in the
  run is reported (and removable with `--update-snapshots`). Tracked by recording which snapshot names were
  touched during a run.
- **Richer serialization:** snapshots serialize via the existing JSON path; the diff on mismatch reuses the
  §6.5 structural diff for readability.

### 6.3 Coverage (line/branch via VM instrumentation — behind a flag)

**Cost-analyzed and gated.** Coverage needs per-line (and ideally per-branch) hit counts, which means
instrumenting execution. Options and the LOCKED choice:

- **VM line-counting via a debug hook (CHOSEN, behind `--coverage`):** the VM can carry a *coverage table*
  keyed by `(chunk, line)` incremented as instructions retire, gated exactly like the existing recursion/
  debug seams — **zero cost when off** (the `--coverage` flag flips an `Option`; the `None` branch is the
  untouched hot path, the same pattern as `Vm.specialize`, `run.rs:104`, and the SP9 determinism cell).
  - **The source of "line" is a Span (byte-offset) table, not a line table** — *correcting the loose
    wording*. `Chunk` carries `spans: Vec<(usize, Span)>` (`chunk.rs:247`): a sorted **code-offset → `Span`**
    map, one entry per instruction that emits, where `Span { start, end }` is a **byte interval** into the
    module source (`span.rs:4-6`), NOT a `(line, col)` pair. There is no line table today. So coverage
    **derives the line per retired instruction** by mapping the retiring op's offset → its `Span` (via the
    existing `Chunk::span_at` binary-search, `chunk.rs:635`) → the start byte → a line number through the
    module's line index (the same byte→line machinery diagnostics already use). Coverage counts *lines*, but
    its raw input is the per-instruction byte `Span`. (DBG independently needs a byte-offset↔line derivation
    too — cross-cutting #6 below; they share this derivation.)
  - **Branch coverage** is a follow-up (it needs per-jump-target counts — a richer table); ship **line
    coverage first**, branch behind the same flag later.
- **Output formats:** `text` (a per-file summary table, default), `lcov` (`lcov.info` for CI/codecov), `html`
  (annotated source, reusing the `docs/`/`ascript doc` HTML style).
- **Engine note:** coverage runs on the **VM** (the production engine; the tree-walker is the oracle and is
  not instrumented for coverage). This is an explicit, documented asymmetry like the VM-only bytecode caps
  (SP3) — coverage is a VM debug-hook feature, not an engine-parity feature, so it does **not** enter the
  four-mode differential.
- **Rejected:** source-rewriting instrumentation (a second front-end transform — fragile, breaks span
  fidelity) and sampling coverage (nondeterministic — violates the test-determinism contract).

#### 6.3.1 The unified VM instrumentation seam (coordination with DBG — cross-cutting #6)

DX's coverage hook and DBG (`2026-06-08-debugger-profiler-design.md`) **both** add `Option`-gated state to
the same `Vm` (DBG locks `Vm.debugger: Option<…>` and `Vm.profiler: Option<…>`, both beside `Vm.specialize`,
`run.rs:104`). Naively each becomes its own field with its own per-loop check; the review's cross-cutting #6
forbids that — the not-attached hot loop must keep **ONE** predictably-not-taken check, not two (one per
feature), to hold Gate 12.

**LOCKED coordination (DX + DBG agree on a single seam):**

- **A single instrumentation gate.** Instead of independent `Vm.coverage` / `Vm.debugger` / `Vm.profiler`
  fields each polled in the loop, the VM carries **one** optional instrumentation handle —
  `Vm.instrument: Option<Instrument>` — and the **hot loop performs exactly one `is_some()`-style check**
  (the same `None`-gated, predictably-not-taken pattern as `Vm.specialize`). When `None` (the default,
  not-attached case) the loop is byte-identical to today; there is **no per-feature branch**.
- **The enum lives behind the gate, never in the per-op fast path.** `Instrument` is the *coverage |
  breakpoint | sample* union (`enum Instrument { Coverage(CoverageTable), Debug(DebuggerHook),
  Profile(ProfilerHook) }`, or a small struct of opt-in sub-hooks behind the one outer `Option`). The
  feature dispatch (which kind is active) happens **only after** the single gate has already been taken —
  i.e. on the cold, attached path — so adding DX coverage to a build that also has DBG does **not** add a
  second hot-loop branch.
- **DBG's breakpoint mechanism is orthogonal and adds no branch regardless.** DBG's *primary* mechanism is
  bytecode-patching (`Op::Break` overwrites the op byte, original in a side table — `2026-06-08-debugger-…`
  §3.2), so a not-attached run is byte-identical with **no** instrumentation check at all for breakpoints.
  The shared `Vm.instrument` gate is what coverage (per-instruction-retire counting) and the profiler
  (sample/frame push-pop) ride; breakpoints ride the patched opcode. The unified seam therefore guarantees
  **at most one** predictably-not-taken check covers *all* always-present instrumentation, satisfying #6.
- **Ownership.** Whichever of DX/DBG merges first introduces `Vm.instrument` + the single gate; the second
  *adds its variant* to the existing `Instrument` (it does NOT add a second field). Merge order is sequential
  (the `.aso` debug-section bump is owned by DBG, cross-cutting #5); DX's coverage table is **runtime-only**
  and is never serialized, so it needs no `.aso`/`ASO_FORMAT_VERSION` change.
- **Both derive line↔byte-offset the same way.** Coverage and DBG both need the per-instruction byte `Span`
  → line mapping (§6.3, `chunk.rs:247`/`span_at:635`); they share that derivation rather than each building a
  table.

#### 6.3.2 The coverage-OFF benchmark (Gate 12, REQUIRED)

To prove the `None`-gated `Vm.instrument` hook is genuinely zero-cost when coverage is off, ship a
benchmark — the same posture the workers/`--no-specialize`/DBG specs use:

- **Three configs over an IC/arithmetic-heavy corpus:** (1) today's `main` (pre-DX, no instrumentation
  field), (2) post-DX with **coverage off** (`Vm.instrument == None`), (3) post-DX with `--coverage` on.
  **Acceptance:** config (2) shows **no measurable steady-state regression** vs (1) (the single predictably-
  not-taken check is in the noise), and config (3)'s overhead is reported (coverage is expected to cost; it
  is the attached path). This is the Gate-12 proof that adding the seam does not tax the not-attached loop,
  and it is run in **both** feature configs. A `tests/`/bench harness asserts the (1)≈(2) parity bound.

### 6.4 Watch mode

**`--watch`** re-runs the affected tests on file change. CLI-side (`sys`-gated file watching); on a change it
re-loads + re-runs, reusing the parallel runner. Scoping by import graph (only re-run files whose import
closure touched the changed file) reuses the **workspace index's import edges** (`workspace.rs` `ImportEdge`)
— the same dependency graph the LSP already maintains. Falls back to "run all" if the graph is unavailable.

### 6.5 Richer assertions & diffs

- A shared **structural diff** for `assert.eq` / snapshot mismatches: instead of `expected X got Y`, render a
  per-field/per-element diff (Object key added/removed/changed, Array index changed) using `deep_equal`'s
  traversal. This is the single most-used quality-of-life win and is reused by §6.2.
- **New assertions:** `assert.matches(value, regex)`, `assert.deepEq` (alias making deep-equality explicit),
  `assert.throwsWith(fn, substr)` (message assertion on the existing async `assert.throws`). Each registers
  in `std_arity.rs` and is documented in `docs/content/stdlib/assert.md`.

### 6.6 Test filtering / focus

**`--filter PATTERN`** runs only tests whose name matches (substring or `/regex/`). Combined with parallel
files, a filter prunes both which files to load and which registered tests to run. Optional in-source
`test.only(name, fn)` / `test.skip(name, fn)` markers are an additive follow-up (a flag on the
registration), not in the first cut.

## 7. Determinism

DX is almost entirely outside the engine-determinism envelope, with one contract:

- **`ascript doc`, the LSP, diagnostics** are **static** (CST + checker, no interpreter) — no clock, no RNG,
  no scheduling, so deterministic by construction. `ascript doc` output is a pure function of source +
  inferencer (a golden-comparable artifact, §8).
- **The test harness** runs programs on the production engine; each test isolate replays deterministically
  on its own (SP9 per-`Interp` determinism context, unchanged). The **only** new nondeterminism is
  *parallel-file completion order*, which is contained by the **deterministic aggregation** contract (§6.1):
  the printed summary, the failure list order, and the exit code are a stable function of input order, not
  completion order. A required test asserts identical summary output across `--parallel=1` and
  `--parallel=N` over the same corpus.
- **Coverage** is deterministic line counts (the same program over the same inputs hits the same lines);
  parallel coverage is **merged** by summing per-isolate tables in a stable key order — the merged report is
  order-independent.
- **No four-mode byte-identity impact:** DX adds no opcode, no `Value` kind, no `.aso` field, so
  `vm_differential` is unchanged. (Coverage's VM hook is `None`-gated and never observed by program output,
  so it cannot perturb the differential — a test asserts a `--coverage` run and a normal run produce
  identical *program* output.)

## 8. Implementation surface & cross-cutting checklist

Per `CLAUDE.md` "Touching syntax" (mostly N/A — no grammar change) plus the DX-specific surfaces. Each is a
required deliverable of the sub-deliverable it belongs to.

**Doc-gen (D1):**
- **New `src/doc/` module** (feature-gated like `lsp`/`pkg`): CST walk → doc model → HTML/Markdown emitter.
  Reuses `src/lsp/providers/{symbols,docs}.rs` extraction and `syntax::resolve`/`infer` — **no second
  parse** (§6 rejected).
- **`src/main.rs`:** the `Doc { paths, out, format, private, open, check }` subcommand + dispatch.
- **`src/syntax`:** a small **doc-comment extractor** over leading trivia (the contiguous `///` run before a
  decl) — shared by `src/doc/` AND the LSP hover/docs provider (so hover shows the *user's* `///` doc, not
  just the kind label `docs.rs` shows today). This is the one shared `syntax`-level addition; it adds **no
  `SyntaxKind`** (trivia reinterpretation, §2).
- **Docs + `NAV`:** if generated docs fold into `docs/`, add slugs to `NAV` in `docs/assets/app.js`
  (default: separate `target/doc/` tree, no `NAV` edit). A `docs/content/language/` mention of the `///`
  convention + an `ascript doc` reference page.
- **Campaign-wide README / landing "scripting → general-purpose" repositioning — OWNERSHIP (LOCKED):**
  the campaign's goal is to reposition AScript from "scripting language" to "general-purpose / serious
  language," which touches `README.md`'s front-door prose and `docs/index.html`'s landing copy. **DX owns the
  cross-cutting *prose repositioning*** — the README/landing narrative rewrite is a DX deliverable (DX is the
  developer-experience pillar; the umbrella nature already makes it the place the campaign-wide DX surface
  converges). **Each feature spec owns its OWN reference page(s)** — NUM documents the numeric tower, ADT the
  enums page, IFACE the interfaces page, TYPE the generics page, etc., each in its own PR (Gate 8). So: DX
  rewrites the top-level *positioning* (README intro + `docs/index.html` hero + the stdlib/feature *table*
  that lists the new pillars), and every construct spec adds/updates its per-construct content page and
  wires its `NAV` slug. This split is stated here so the repositioning is not orphaned ("everyone assumed
  someone else") and not duplicated.
- **Examples (Gate 9 — a runnable `examples/advanced/` DX artifact):** a fully-documented, production-shaped
  module under `examples/advanced/` (e.g. `examples/advanced/documented_library.as`) carrying `///` docs on
  its fns/classes/enums **and** an accompanying `test(...)` suite, so the one file simultaneously
  (a) is the `ascript doc` golden source, (b) exercises the parallel test runner + snapshot/coverage path,
  and (c) stays runnable via `target/release/ascript run` like every other `examples/advanced/*.as`. This is
  the Gate-9 "DX dogfoods its own surface" artifact — it must `run` clean, `doc` to the pinned golden, and
  `test --coverage` to a known line-hit set.

**Test framework (D2):**
- **`src/main.rs`:** `Test` grows `--parallel/--update-snapshots/--coverage/--watch/--filter`.
- **`src/lib.rs`:** `run_tests*` gains a parallel path dispatching files over `src/worker/` (reuse
  `pool`/`dispatch`); deterministic aggregation of `TestSummary`s.
- **`src/worker/`:** a test-file dispatch entry (load file + run its tests + return the result across the
  airlock) — reuses the existing isolate bootstrap + structured-clone airlock. **`TestSummary` is a Rust
  struct, not a `Value`** (`interp.rs:779-785`), and the airlock crosses `Value` only (`encode(v: &Value)`,
  `serialize.rs:360`), so the isolate encodes it as a `Value::Object` (`{passed, failed, failures}`) and the
  parent decodes back into a `TestSummary` (§6.1) — no new sendable kind.
- **VM coverage hook (on the UNIFIED `Vm.instrument` seam, §6.3.1):** coverage is the `Instrument::Coverage`
  variant behind the single `Vm.instrument: Option<…>` gate (`None`-gated, zero-cost off; coordinated with
  DBG so the not-attached loop keeps ONE predictably-not-taken check — cross-cutting #6), with a
  `CoverageTable` keyed by `(chunk, line)` where line is **derived from the per-instruction byte `Span`**
  (`chunk.rs:247` / `span_at:635`, NOT a line table); `src/vm/run.rs` (beside `specialize:104`) + a
  `--coverage` plumb. Tree-walker not instrumented (documented asymmetry). The coverage table is
  runtime-only — no `.aso`/`ASO_FORMAT_VERSION` change.
- **`std/assert` (`src/stdlib/assert_mod.rs`):** `--update-snapshots` mode, obsolete-snapshot tracking,
  structural diff, the new assertions; `std_arity.rs` entries; `docs/content/stdlib/assert.md`.
- **Watch:** `sys`-gated file watcher driving re-runs, scoped by the workspace import graph.
- **Examples/tests:** a multi-file test corpus exercised serially AND in parallel (asserting identical
  aggregation).

**LSP (D3):**
- **`src/lsp/workspace.rs`:** add a `FileId` interner (over the existing canonical-`PathBuf` keys,
  `workspace.rs:97`) and a new file-qualified `GlobalBindingId` = `(FileId, TextRange)` /
  `(definer-FileId, exported-name)`; make the cross-file index a *join over per-file `syntax::resolve`
  results* lifted into `GlobalBindingId`, replacing the name-only `collect_uses` walk (`workspace.rs:904`,
  which tags uses by `ResolvedTarget` on bare name) — the identity unification (§4.1). The in-file
  `navigation.rs` `BindingId` (`:71-75`) stays file-local and becomes the per-file projection.
  References/rename then share one identity model with in-file navigation.
- **`src/lsp/providers/completion.rs`:** frame-precise candidates + member completion via `infer`.
- **`src/lsp/providers/{symbols,semantic_tokens,inlay}.rs`:** completeness hooks for new constructs (each
  construct spec adds its arm; DX defines the pattern + a per-provider test requirement).
- **`src/lsp/providers/lens.rs`:** "run test" / "run all" code lenses wired to `ascript test`.
- **`src/lsp/providers/docs.rs` / `hover.rs`:** render the user's `///` doc (shared extractor above).
- **Tests:** `tests/lsp.rs` — references/rename cross-file identity correctness (the shadowing edge), member
  completion, inlay per construct.

**Diagnostics (D4):**
- **`src/diagnostics.rs`:** `report_all(&[AsError])` multi-report; `help`/note lines for did-you-mean.
- **`src/check` + a shared `suggest::closest`:** edit-distance suggestions for unresolved-name / unknown-
  member; surfaced both at the CLI and as LSP quick-fixes (`code_action.rs`).
- **Caret-offset audit + fix** (§5.3): `src/lsp/convert.rs` / span construction in both front-ends; a golden
  pinning the caret column in each front-end and asserting equality (or a formally-accepted pin + note).

**Unchanged:** the GC, the `Interp` async model, all opcodes/`Value`/`.aso` (no `ASO_FORMAT_VERSION` bump —
DX touches neither), the tree-sitter grammar (the `///` convention is trivia reinterpretation; a distinct
doc-comment *highlight* is a deferred additive `highlights.scm` predicate), all non-test stdlib semantics.

## 9. Testing

- **Doc-gen golden:** `ascript doc --format md` over the documented example module produces a pinned
  Markdown tree (signatures, `///` bodies, cross-links); a second golden for the HTML index structure.
  `ascript doc --check` exits non-zero on a deliberately-undocumented public symbol.
- **Parallel-test correctness:** the multi-file corpus produces an **identical `TestSummary` + identical
  printed output** at `--parallel=1` and `--parallel=N` (the determinism contract, §7); a failing test in
  one file is reported with the right file/name regardless of completion order; a no-op (no isolates) path
  for a single file.
- **Snapshot self-test:** first run writes; second run passes; a mutated value fails with a structural diff;
  `--update-snapshots` re-baselines; an orphaned snapshot is reported.
- **Coverage:** a known program yields the expected line-hit set (line derived from the per-instruction byte
  `Span`, §6.3); `--coverage` output (program stdout) is byte-identical to a non-coverage run (hook is
  observation-only); parallel coverage merge is order-stable.
- **Coverage-OFF zero-cost benchmark (Gate 12, REQUIRED, §6.3.2):** the three-config bench (pre-DX `main` vs
  post-DX coverage-off `Vm.instrument == None` vs `--coverage` on) asserts **no measurable steady-state
  regression** for the not-attached config and reports the on-cost — proving the unified `Vm.instrument` gate
  adds no hot-loop tax; run in both feature configs.
- **`examples/advanced/` DX artifact (Gate 9):** the documented production-shaped module (§8) `run`s clean on
  the release binary, `doc`s to the pinned golden, and `test --coverage` produces the known line-hit set.
- **LSP provider tests** (`tests/lsp.rs`): cross-file references/rename on the unified file-qualified
  `GlobalBindingId` model (§4.1) — including the **shadowed-local cross-file rename edge** that the bare
  `navigation.rs` `BindingId` would get wrong (rename an export `x` defined in file A → its uses in importers
  rename, but a same-named local `let x` in an importer is NOT touched; and two locals named `x` in different
  files at the same byte range stay distinct); member + frame-precise completion; inlay/hover render the
  `///` doc; semantic tokens for at least one new construct.
- **Diagnostics:** multi-error batching renders all parse errors; did-you-mean fires within edit distance
  and not beyond it; the caret-column golden matches across front-ends (or pins the accepted offset).
- **Gates:** clippy clean in both feature configs; `cargo test` + `--no-default-features` green;
  `examples/**` still emits **zero** `type-*` diagnostics (DX adds no inference); `vm_differential` unchanged
  (DX adds no engine surface).

## 10. Scope & rejected alternatives

**In scope:** `ascript doc` + the `///`/`//!` doc-comment convention (trivia-based) with HTML/Markdown output
and site-consistent styling; test-framework depth (parallel-via-isolates, snapshot completion + bulk update,
line coverage behind a flag, watch, structural diffs, filtering, new assertions); the LSP semantic-resolver
*identity unification* (cross-file on a new file-qualified `GlobalBindingId` model) + completion depth +
inlay/symbol/token
completeness hooks for new constructs + run-test lenses; diagnostics quality (multi-error, did-you-mean, the
caret-offset audit/fix, a message style guide); coverage on the **unified `Vm.instrument` seam** shared with
DBG (§6.3.1, one hot-loop gate); and the campaign-wide README/landing **prose repositioning** (DX owns the
narrative; each construct spec owns its own page — §8). DX is the standing surface every other spec updates.

**Out of scope / deferred:**
- A distinct `DocComment` `SyntaxKind` + tree-sitter highlight (deferred; additive `highlights.scm`
  predicate later — §2).
- **Branch** coverage (ship line coverage first; branch behind the same `--coverage` flag later).
- `test.only`/`test.skip` source markers (additive follow-up to `--filter`).
- A doc *hosting* pipeline / versioned docs site (generation is in scope; hosting is not).
- Per-test (rather than per-file) parallelism (rejected: the isolate cost model favors the file unit;
  per-test would thrash isolate birth — §6.1).

**Rejected:**
- **Doc-gen from a separate parse / a doc-specific front-end.** Wasteful and divergence-prone; reuse the
  **lossless CST** (`src/syntax/`) + the existing `symbols`/`docs` providers + `infer` — one source of truth
  for signatures, types, and trivia (the whole point of a lossless CST is doc-gen + tooling, per the brief).
- **Serial-only tests.** Leaves the shipped `src/worker/` isolate model on the table for the one workload
  (independent test files) it fits perfectly; a serious language runs its test suite across cores.
- **Leaving navigation on a second, coarser cross-file resolver.** Two identity models (frame-precise
  in-file vs name-coarse cross-file) is a divergence farm for references/rename; unify on a file-qualified
  `GlobalBindingId` that *lifts* the per-file resolver result (NOT the bare `navigation.rs` `BindingId`,
  whose `Local(TextRange)` collides across files and whose `Global(String)` is name-only — §4.1). (Note: the
  *legacy-AST* split the brief feared is already gone — §4.1.)
- **Source-rewriting coverage instrumentation.** Breaks span fidelity and adds a second transform; the
  `None`-gated VM hook (the unified `Vm.instrument` seam, §6.3.1, deriving line from the per-instruction byte
  `Span` table) is zero-cost-off and span-faithful.
- **A separate `Vm.coverage` field alongside DBG's `Vm.debugger`/`Vm.profiler`.** Two independent
  `Option` fields = two hot-loop checks, violating Gate 12 / cross-cutting #6. Rejected in favour of the
  single `Vm.instrument` gate DX and DBG share (§6.3.1).
- **Accepting the SP1 caret offset by default.** With all static tooling now on the CST, the off-by-one is a
  closable inconsistency, not an inherent trade-off; default posture is fix-with-golden (§5.3).

## 11. Grounding (verified sources)

- **Doc-gen conventions:** Rust `rustdoc` (`///` outer / `//!` inner doc-comments, Markdown bodies); Go
  `godoc` (comment-immediately-preceding-decl attachment, Markdown-ish plain text); Swift `swift-doc` /
  DocC (Markdown doc-comments, symbol-graph extraction). The `///`/`//!` + contiguous-run-attaches-to-decl
  rule is the rustdoc model, chosen for audience familiarity and zero-grammar-cost on our existing `//`
  lexing.
- **Snapshot testing:** Jest snapshots (`toMatchSnapshot`, `--ci`, `-u`/`--updateSnapshot`, obsolete-snapshot
  reporting) — the model `assert.snapshot` + `--update-snapshots` + orphan detection mirrors.
- **Coverage instrumentation cost:** Rust `-C instrument-coverage` / LLVM source-based coverage and lcov
  output formats; the "behind a flag, zero-cost when off" posture matches the existing `Vm.specialize` /
  SP9-determinism `None`-gated seams in this codebase.
- **Parallel test execution across isolates:** Node `worker_threads`-based test runners; the workers
  foundation spec (`2026-06-07-workers-foundation-stateless-design.md` — §7 pool, the `ASCRIPT_WORKERS` cap
  (:149), the ~0.5–2 ms isolate-birth cost model (:160), the nested-inline rule (:313), and the
  gather-preserves-order determinism discipline (:181, :186)). The workers spec does **not** itself claim a
  test-execution workload; DX *layers* file-granularity dispatch over that shared-nothing pool — file
  granularity is the unit the isolate cost model favours.
- **LSP completeness & a single semantic model:** rust-analyzer (one salsa-backed semantic model powering
  navigation, completion, and diagnostics uniformly — the anti-split-brain reference) and the LSP
  specification (the capability surface enumerated in §4.4).
- **Diagnostics quality:** rustc's multi-error batching + "did you mean" suggestions (Levenshtein-based name
  suggestion) and ariadne (already the renderer, `src/diagnostics.rs`) for multi-label reports.
