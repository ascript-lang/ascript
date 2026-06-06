# First-Class AScript Language Server — Design

- **Date:** 2026-06-05
- **Status:** Design (approved, pre-plan)
- **Topic:** Bring the AScript LSP to a full-fledged, first-class language server with
  the complete modern + advanced LSP capability surface, unify it onto a single
  CST/resolver/infer core, and ship first-class VS Code, Zed, and Neovim integrations.

## 1. Context — where the LSP stands today

The LSP (`src/lsp/`) is static-analysis-only: it never instantiates the interpreter,
holds no `Rc`/`RefCell`/`Value`, and is therefore `Send + Sync` over tower-lsp.

Post-SP-campaign it is genuinely semantic, but **split across two front-ends**:

- **Diagnostics** flow through `crate::check::analyze` (the new CST front-end →
  `syntax::resolve` → the `check::rules::ALL` lint set + the SP10 `check::infer` type
  pass). Multi-error, severity-aware, coded, suppressible — but the LSP calls plain
  `analyze` (default config), so it currently ignores `ascript.toml [lint]`.
- **Cross-file navigation** (`definition`, `references`, `rename`/`prepareRename`,
  `workspaceSymbol`, index-backed arity) flows through `src/lsp/workspace.rs`, a
  `WorkspaceIndex` built on the **same** new CST + resolver — but it re-parses per query.
- **Hover types** come from `check::infer::hover_type_at` (re-parses again).
- **Single-file `hover` / `completion` / `definition` / `documentSymbol`** still run on
  the **legacy** `crate::{ast, lexer, parser}` front-end inside `analysis.rs`.

Currently advertised capabilities (8): full text sync, `documentSymbol`, `hover`,
`completion`, `definition`, `references`, `rename` (+ prepare), `workspaceSymbol`.

`completion` is the weakest: it is the original three-branch heuristic (import-path
string → std module paths; `alias.` → namespace exports; else baseline keywords +
builtins). It does not consult the resolver, the workspace index, or the inferencer.

`ascript fmt` (a canonical formatter, both `fmt.rs` and the CST `syntax/format/`) and a
tree-sitter grammar (`docs/superpowers/specs/grammar/tree-sitter-ascript/`, with only
`highlights.scm`) exist but are **not** wired to the LSP or packaged for editors.

## 2. Goals & non-goals

### Goals

1. Implement the **complete modern + advanced LSP surface** (see the capability matrix).
2. **Unify** the LSP onto one CST/resolver/infer core — retire the legacy
   `crate::{ast, lexer, parser}` path from the LSP entirely.
3. Ship **first-class editor integrations**: VS Code, Zed, Neovim — each with LSP,
   tree-sitter syntax, and formatting.

### Non-goals (explicit, so they are not mistaken for gaps)

- **No interpreter in the LSP.** Stays static-only, `Send + Sync`, no `Rc`/`RefCell`/
  `Value`. Hard invariant; rules out any eval-based feature (no "run & show value").
- **No new type-inference engine.** The LSP *surfaces* SP10's advisory in-file checker;
  it does not deepen it. Cross-module types stay `Any` (documented SP10 non-goal), so
  hover/inlay precision tracks SP10 exactly.
- **`moniker`** (cross-repo symbol indexing), **DAP/debugging** (separate protocol),
  **notebooks**, and **LSP telemetry** are out of scope.
- **Formatter stays canonical/opinionated** — exposed as-is, no per-user style knobs
  (matches `ascript fmt`).
- **Binary auto-download is optional**, not the default (default = `PATH` + setting).
- **`linkedEditingRange` tag-pairs deferred** until HTML-template syntax exists (v1 =
  local identifiers only).

## 3. Architecture — the unified semantic core (Approach A)

The chosen approach is **unify-first, then fan out**: build one cached semantic model,
retire the legacy path, then implement every provider as a thin adapter, and package the
editors last on a finished server.

### 3.1 `SemanticModel` (one per open document)

Replaces the three-different-ways-to-parse status quo with a single structure, built once
per document version and cached:

```
SemanticModel {
  text, version,
  tree:        green/red CST          (syntax::tree_builder)
  resolved:    ResolveResult          (syntax::resolve — uses / bindings / frames)
  inferred:    InferResult            (check::infer — type map, hover types, narrowing)
  diagnostics: Vec<AsDiagnostic>      (check::analyze_with_config + infer)
  tokens:      Vec<LexToken>          (syntax::lex — semantic tokens)
}
```

Every provider becomes a pure `fn(&SemanticModel, params) -> Result`. No provider
re-parses; **no provider imports `crate::{ast, lexer, parser}`**. The legacy front-end
remains in the crate solely as the VM's differential oracle — untouched, but gone from
the LSP.

### 3.2 Workspace layer

`WorkspaceIndex` is kept but refactored to **hold/borrow cached `SemanticModel`s**
instead of re-parsing inside `definition_at` / `references_at` / etc. — one source of
truth, warm and incremental.

### 3.3 Infrastructure upgrades that fall out of the model

- **Incremental sync** — move from `TextDocumentSyncKind::FULL` to `INCREMENTAL`; apply
  ranged edits and rebuild the model (cstree enables green-node reuse as a later
  optimization).
- **Config-aware diagnostics** — load the nearest `ascript.toml [lint]` and run
  `analyze_with_config`, so the editor matches `ascript check`; re-resolve on config
  change (`didChangeWatchedFiles` / `didChangeConfiguration`).

### 3.4 Module layout

```
src/lsp/
  model.rs        SemanticModel + per-document cache
  convert.rs      byte <-> Position/Range, ByteSpan <-> Range
  workspace.rs    cross-file index over cached models
  server.rs       thin tower-lsp protocol adapter
  providers/
    navigation.rs     definition / declaration / typeDefinition / implementation /
                      references / documentHighlight
    symbols.rs        documentSymbol / workspaceSymbol (+ resolve)
    completion.rs     completion (+ resolve, auto-import)
    hover.rs          hover (types + signatures + docs)
    signature.rs      signatureHelp
    semantic_tokens.rs
    inlay.rs          inlayHint (+ resolve)
    code_action.rs    codeAction (+ resolve), executeCommand
    formatting.rs     formatting / rangeFormatting / onTypeFormatting
    hierarchy.rs      callHierarchy / typeHierarchy
    folding.rs        foldingRange / selectionRange / documentLink
    lens.rs           codeLens (+ resolve)
    color.rs          documentColor / colorPresentation (recognizer subsystem)
    rename.rs         rename / prepareRename / linkedEditingRange
```

## 4. Capability matrix

Legend: ✅ done · ◑ partial/rework · ❌ add · ⛔ excluded (with reason). "Powered by"
names the `SemanticModel` field each provider reads.

### Lifecycle & sync
| Method | Status | Target | Powered by |
|---|---|---|---|
| initialize / initialized / shutdown | ✅ | keep | — |
| didOpen / didChange / didClose | ◑ FULL | **INCREMENTAL** + ranged edits | text/cache |
| didSave / willSaveWaitUntil | ❌ | add (format-on-save fallback) | formatter |

### Diagnostics
| Method | Status | Target | Powered by |
|---|---|---|---|
| publishDiagnostics (push) | ◑ default-config | **config-aware** (`analyze_with_config` + `ascript.toml`) | diagnostics |
| textDocument/diagnostic (pull) | ❌ | add | diagnostics |
| workspace/diagnostic (pull) | ❌ | add (project-wide) | workspace |

### Navigation
| Method | Status | Target | Powered by |
|---|---|---|---|
| definition | ✅ cross-file | port to model | resolve + workspace |
| declaration | ❌ | add | resolve |
| typeDefinition | ❌ | add (jump to value's class/enum) | infer + table |
| implementation | ❌ | add (subclasses / enum variants) | table |
| references | ✅ | keep | resolve + workspace |
| documentHighlight | ❌ | add (read/write occurrences) | resolve.uses |

### Symbols & structure
| Method | Status | Target | Powered by |
|---|---|---|---|
| documentSymbol | ◑ legacy, top-level | port to CST; nest locals/methods/fields | CST |
| workspaceSymbol (+ resolve) | ✅ / ❌ | keep; add lazy resolve | workspace |
| foldingRange | ❌ | add (blocks, fns, classes, `//region`) | CST |
| selectionRange | ❌ | add (smart expand via CST ancestry) | CST |
| documentLink | ❌ | add (import paths clickable) | CST + workspace |

### Hover / help / completion
| Method | Status | Target | Powered by |
|---|---|---|---|
| hover | ◑ legacy + infer types | unify on model; add signatures + docs | infer + CST |
| signatureHelp | ❌ | add (active param while typing a call) | resolve + table |
| completion | ◑ baseline+std+namespace | **full rewrite** (scope locals/params, members, class fields/methods, enum variants, module exports, keywords, snippets) | resolve + infer + table + workspace |
| completionItem/resolve | ❌ | add (lazy detail/docs) | infer |
| auto-import | ❌ | add (unresolved name → import insert `additionalTextEdits`) | workspace |

### Editing power-tools
| Method | Status | Target | Powered by |
|---|---|---|---|
| formatting / rangeFormatting | ❌ | wire `syntax/format` | formatter |
| onTypeFormatting | ❌ | optional | formatter |
| codeAction (+ resolve) | ◑ infra only (`check::fix`, 1 code) | quickfixes (grow `FIXABLE_CODES`), `source.organizeImports`, `source.fixAll` | diagnostics + fix |
| codeLens (+ resolve) | ❌ | add (run `test(...)`/`main`, ref counts) + `executeCommand` | CST + workspace |
| semanticTokens full / range (+ delta) | ❌ | add (types/params/props/enums) | lexer + resolve |
| inlayHint (+ resolve) | ❌ | add (inferred `let`/param types; param-name hints) | infer |
| rename / prepareRename | ✅ | keep (cross-file) | resolve + workspace |
| linkedEditingRange | ❌ | add later (local idents v1; tag-pairs when HTML templates exist) | resolve.uses |

### Hierarchy & workspace
| Method | Status | Target | Powered by |
|---|---|---|---|
| callHierarchy (prepare/incoming/outgoing) | ❌ | add | workspace |
| typeHierarchy (prepare/super/sub) | ❌ | add (class + enum) | table + workspace |
| didChangeWatchedFiles | ❌ | add (`.as` + `ascript.toml`) | workspace |
| didChangeConfiguration / workspace/configuration | ❌ | add | config |
| willRenameFiles / didRenameFiles | ❌ | add (rewrite imports on move) | workspace |
| executeCommand | ❌ | add (backs lenses / fixAll) | — |
| multi-root + work-done progress | ◑ / ❌ | add (indexing progress) | workspace |

### Color (extensible recognizer subsystem)
| Method | Status | Target | Powered by |
|---|---|---|---|
| documentColor / colorPresentation | ❌ | add | CST |

Internal `Rgba { r, g, b, a }`; LSP's wire `Color` is RGBA f32 (0..1), so alpha
round-trips natively. A **recognizer registry**, each yielding `(span, Rgba)`:

1. `color.rgb(r,g,b,…)` / `color.bgRgb(…)` — truecolor calls (numeric literal channels).
2. tui `[r,g,b]` integer-triple array literals in style positions.
3. **Hex string literals** — `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa` (4 & 8 carry alpha).
4. **Functional strings** — `rgb()`, `rgba()`, `hsl()`, `hsla()` in string literals (for
   the coming CSS/HTML modules).
5. *Future hook:* named CSS colors, `color.hex(...)`, `oklch()` — add a recognizer, no
   provider change.

**Color-sink context registry** — string-based recognizers (hex/`rgba`/`hsl`) activate
**only inside argument positions of color-aware APIs** (today `color.*` / tui style;
future CSS/HTML modules register their color params the same way). This kills the
false-positive case (`p.label == "#100"` in `examples/typed_fields.as` is a label, and
`#100` is a valid 3-digit hex shape). A client setting
`ascript.color.detectHexStringsEverywhere` (default **off**) can broaden to all string
literals.

**`colorPresentation`** is **format-preserving** by default and offers cross-format
choices: a hex source gets `#rrggbb` / `#rrggbbaa`; an `rgba()` source gets functional
forms; a `color.rgb(...)` call edits its numeric args. Alpha-bearing picks emit an
alpha-capable form (hex8 / `rgba()`).

### Excluded
- ⛔ **moniker** — cross-repo symbol export indexing; irrelevant to a single-project
  server.

## 5. Editor deliverables

### 5.0 Shared foundation

- **Promote the tree-sitter grammar** to a first-class published `tree-sitter-ascript`
  (npm `package.json` + `Cargo.toml`/`bindings/`) with a **complete query set** — only
  `highlights.scm` exists today; add `injections.scm` (template `${…}`; future SQL/HTML/
  CSS in template strings), `locals.scm`, `folds.scm`, `indents.scm`, `textobjects.scm`,
  `tags.scm`, `brackets.scm`. Extend the tree-sitter conformance test to assert query
  captures resolve.
- **Binary distribution:** default discover `ascript` on `PATH`, override via
  `ascript.server.path`; optional per-platform prebuilt auto-download (checksum-verified).
  Each extension pins a **minimum server version** (`serverInfo.version`).
- **Repo home:** `editors/{vscode,zed,nvim}/` + the promoted grammar, all CI-built.

### 5.1 VS Code (`editors/vscode/`)

- Language client (TypeScript, `vscode-languageclient`) launching `ascript lsp` over
  stdio; trace + restart command.
- `language-configuration.json`: comments, brackets, auto-close/surround pairs,
  indentation rules, `wordPattern`.
- Coloring: a **TextMate grammar** (`ascript.tmLanguage.json`) for instant baseline +
  server `semanticTokens` for the semantic layer (VS Code has no public tree-sitter API
  for third-party languages — TextMate + semantic tokens is the correct path).
- Contributions: `.as` language + icon; `ascript.*` settings; commands.
- Packaging: `vsce package` → `.vsix`; publish to **Marketplace + Open VSX**; CI builds.

### 5.2 Zed (`editors/zed/`)

- Rust→WASM extension with `extension.toml`, `languages/ascript/config.toml`, the
  tree-sitter grammar reference + shared `queries/` (Zed consumes
  highlights/injections/brackets/outline/indents directly), and a WASM shim registering
  the `ascript lsp` language server + settings.
- Packaging: submit to the Zed extension registry; CI builds the WASM.

### 5.3 Neovim (`editors/nvim/`)

- LSP: an `nvim-lspconfig` server def (`cmd = {"ascript","lsp"}`, `filetypes =
  {"ascript"}`, root markers `ascript.toml`/`.git`) — ship as a snippet **and** upstream
  a PR.
- Tree-sitter: an `nvim-treesitter` parser registration (grammar + shared queries under
  `queries/ascript/`).
- Filetype + formatting: `ftdetect` for `*.as`; formatting via LSP
  `textDocument/formatting`, with a `conform.nvim` recipe pointing at `ascript fmt` as a
  fallback.
- Packaging: a minimal `ascript.nvim` plugin (or docs snippet).

## 6. Phasing & milestones

Each phase ships independently and has a gate. Phases 1–4 are server-side and largely
parallelizable after Phase 0. Phase 5 (grammar) can run in parallel but is a hard
prerequisite for Phase 6.

- **Phase 0 — Unification foundation.** `SemanticModel` + per-doc cache + `convert.rs`;
  INCREMENTAL sync; config-aware diagnostics; refactor `workspace.rs` to hold models;
  port the existing 5 providers off legacy and delete those imports from the LSP.
  *Gate:* the current 8 capabilities byte-identical; legacy path gone from the LSP.
- **Phase 1 — Editing essentials.** Formatting (+ range); completion rewrite (+ resolve,
  auto-import); code actions (+ resolve, organizeImports, fixAll) + executeCommand.
  *Gate:* format idempotence; completion-correctness suite; code-action apply tests.
- **Phase 2 — Semantic visualization.** semanticTokens (full + range); inlayHint (+
  resolve); documentHighlight; signatureHelp.
  *Gate:* token/inlay snapshots; zero-FP on corpus.
- **Phase 3 — Navigation & structure depth.** declaration / typeDefinition /
  implementation; foldingRange / selectionRange / documentLink; callHierarchy /
  typeHierarchy; workspaceSymbol/resolve.
  *Gate:* cross-file navigation suite incl. import-edge following.
- **Phase 4 — Advanced editing & workspace.** documentColor/colorPresentation;
  linkedEditingRange (local idents); codeLens; pull diagnostics; willRenameFiles/
  didRenameFiles (import rewrite); didChangeConfiguration; multi-root + work-done
  progress.
  *Gate:* color round-trip (`#100` false-positive guard); import-rewrite-on-rename;
  multi-root tests.
- **Phase 5 — Grammar promotion.** Standalone `tree-sitter-ascript` + full query set +
  conformance/drift guard. *(parallelizable; prerequisite for Phase 6.)*
- **Phase 6 — Editor extensions.** VS Code, Zed, Neovim + binary distribution; all
  CI-built; min-server-version pinned.
  *Gate:* each editor validated against the capability matrix; CI produces artifacts.
- **Phase 7 — Performance & polish.** Incremental green-node reuse; request cancellation;
  large-file bounds; per-editor setup docs + capability reference page.

## 7. Testing strategy

1. **Pure provider unit tests** — `fn(&SemanticModel, params) -> result`, tested with
   `&str` + offset, no live client (mirrors today's `analysis.rs` tests).
2. **Zero-false-positive corpus gate** — extend the SP10 pattern to noise-capable
   providers: documentColor (no bogus swatch on `#100`), semanticTokens (every token
   classifiable), inlayHint (no contradictory hints).
3. **Incremental-sync differential** — a sequence of ranged edits must yield a model
   equal to a full reparse (highest-risk new mechanism; dedicated differential).
4. **Protocol smoke test** — extend the existing JSON-RPC-over-stdio test: initialize →
   assert the full capability set → exercise representative requests end-to-end.
5. **Round-trip / idempotence** — format idempotence; rename produces compiling code;
   color picker is a no-op when the same color is chosen; code-action apply is
   overlap-safe & idempotent.
6. **Cross-file fixture tests** — multi-file temp workspaces for definition/references/
   rename/hierarchies/auto-import/rename-file import rewrite.
7. **Consistency invariant** — LSP diagnostics ≡ `ascript check` for the same config.
8. **Grammar conformance** — every `examples/*.as` parses clean AND query captures
   resolve.
9. **Editor CI** — build `.vsix` / Zed WASM / nvim plugin; lint manifests; per-editor
   manual UX-validation matrix.

## 8. Risks & mitigations

- **Incremental sync correctness** — most error-prone change; mitigated by the
  Phase-0 differential against full reparse (item 7.3).
- **Color false positives** — mitigated by context-gated string recognizers + the
  corpus zero-FP gate (the `#100` case).
- **Completion precision vs. the gradual checker** — completion/hover/inlay are only as
  precise as SP10; cross-module stays `Any`. Accepted, documented (§2 non-goals).
- **VS Code tree-sitter** — no public third-party API; TextMate + semantic tokens is the
  deliberate, robust choice (§5.1).
- **Feature-gating** — the analysis core is feature-independent, so diagnostics work
  under `--no-default-features`; the `lsp` feature gate stays.
