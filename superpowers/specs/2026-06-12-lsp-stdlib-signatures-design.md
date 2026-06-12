# Stdlib Signature Table + LSP Signature/Completion/Hover Enrichment + Audit Hardening — Design (SIG)

- **Status:** Draft for review
- **Date:** 2026-06-12
- **Code:** SIG (the single developer-experience spec of the PERF campaign — see `goal-perf.md`
  §"Developer-experience track")
- **Depends on:** the **2026-06-12 LSP reliability fixes** (currently in-tree, uncommitted on
  this working tree: the `flush_pending_for` pending-flush at the completion/hover/signature
  handlers, the single-critical-section `did_change` fold that closed the lost-edit race, the
  `STD_MODULE_PATHS` → `stdlib::STD_MODULES` derivation, and the `did_close` pending purge).
  **This spec assumes those fixes are merged**; Phase 0 of the plan re-verifies every cited
  line against the merged tree. NOTHING in the engine campaign is a dependency.
- **Sequencing:** owner-sequenced **after** the PERF engine waves (`goal-perf.md` execution
  order: "SIG (DX track) — independent of ALL engine specs; owner-sequenced after the engine
  waves"). Technically executable at any time — it is LSP/checker-static-data only.
- **Engines:** N/A — tooling only. **No engine, VM, compiler, `.aso`, or grammar surface.**
  `ASO_FORMAT_VERSION` untouched; `tests/vm_differential.rs` untouched (run once at the gate to
  prove no engine surface was touched).
- **Breaking:** no.

---

## 0. Read this first

The 2026-06-12 LSP audit (provenance: an internal read-only audit of `src/lsp/` on branch
`feat/debugger-profiler`; its findings file is **gitignored**, so every fact this spec relies
on is **inlined below with file:line against the current tree** — this document is
self-contained and normative on its own) split the owner's "the LSP doesn't work reliably"
complaint into two halves:

1. **Reliability** — trigger-character requests raced a 40ms debounced rebuild; a lost-edit
   race could corrupt the server's view of a document. **Fixed** by the 2026-06-12 reliability
   fixes this spec depends on (see Depends-on above; verified in-tree:
   `src/lsp/server.rs:112-123` `flush_pending_for`, `:540-596` the one-critical-section
   `did_change`, `:598-609` the `did_close` purge, `src/lsp/providers/completion.rs:55` the
   `STD_MODULES` reuse).
2. **Coverage & data** — even with perfect sync, signature help covers almost nothing, stdlib
   completion items are bare undifferentiated labels, and hover knows nothing about stdlib
   members — because **the crate contains no machine-readable stdlib signatures at all**.
   That is SIG's job, plus the audit's remaining (non-reliability) hardening items.

## 1. Summary & motivation — the inlined audit facts

### 1.1 Signature help resolves only a unique same-file `fn` (root cause 1)

`src/lsp/providers/signature.rs` (the whole provider is 159 lines):

- The module doc says it outright (`signature.rs:1-6`): *"In-file only … the callee must
  resolve to a unique in-file `FnDecl`. A method-call callee (`obj.m(...)`, whose callee is a
  `MemberExpr` rather than a `NameRef`) returns `None`: a documented v1 limitation."*
- `enclosing_call` (`signature.rs:31-57`) finds the innermost `CallExpr` whose `ArgList`
  contains the offset, then requires a **`NameRef` child** as the callee (`signature.rs:44`,
  `call.children().find(|c| c.kind() == SyntaxKind::NameRef)`) — a `MemberExpr` callee falls
  through the `else { continue }` and the call site is skipped entirely.
- `find_fn_decl` (`signature.rs:60-70`) then requires a **unique same-file `FnDecl`** of that
  name (two declarations → `None`, "ambiguous — skip (zero-FP)").
- `param_names` (`signature.rs:73-84`) extracts param **names only** — each `Param`'s `Ident`
  text — dropping the type annotation, the default, and the `...rest` marker;
  `make_help` (`signature.rs:103-122`) emits `ParameterLabel::Simple`, no documentation, no
  return type.

Consequence, by construction: **the ENTIRE stdlib (`math.pow(`, `array.map(` — `MemberExpr`
callees), every method call (`obj.method(`), every global builtin (`print(`, `len(` — no
`FnDecl` in file), and every cross-file imported user fn (`import { add } from "./util"` —
the `FnDecl` lives in another file) get NO signature help.** The cross-file case is doubly
galling because the workspace index **already re-parses the target module and walks its
`ParamList`** for the D-arity lint (`src/lsp/workspace.rs:419-445`, `exported_fn_arity`) — and
then keeps only the arity, discarding the names.

### 1.2 Native stdlib fns have no machine-readable signatures anywhere (root cause 2)

Native std functions are Rust closures over `Value` (`src/stdlib/*.rs`); their export surface
is `(name, Value)` pairs (`std_module_exports`, `src/stdlib/mod.rs:113-210`; fns are
`Value::Builtin("math.abs")` via `bi()`, `mod.rs:107-109`; constants are plain values, e.g.
`("pi", Value::Float(PI))` in `src/stdlib/math.rs`). Param names/types/optionality exist ONLY
as prose in the docs site (`docs/content/stdlib/*.md`, surveyed in §2.2). The single
structured datum in the crate is `src/check/std_arity.rs` — a curated **min-arity-only** table
of ~36 entries (`required_args`, `std_arity.rs:40-99`), `max = None` always because native fns
silently ignore surplus args (`std_arity.rs:6-21`, the zero-false-positive contract), consumed
by the `call-arity` lint at `src/check/rules/call_arity.rs:151-154`. There are **~56 std
modules** (`STD_MODULES`, `src/stdlib/mod.rs:221-280`) with on the order of **600 exports**
(the audit measured the auto-import flood at ~600+ items per keystroke) — coverage of the
signature surface today is therefore ≈6% of names, arity-only, zero param names.

### 1.3 Stdlib completion items are bare labels (root cause 3)

`src/lsp/providers/completion.rs`:

- The C2 namespace-member branch maps exports to `item(name, CompletionItemKind::FUNCTION)`
  (`completion.rs:249-259`): **no `detail`, no documentation, and the wrong kind for
  constants** — `math.pi` shows as FUNCTION.
- `resolve_completion` (`completion.rs:305-318`) enriches **only** the ~10 global builtins
  (`BUILTINS`, `completion.rs:34-36`) and keywords, via `docs::builtin_doc`/`keyword_doc`; a
  stdlib member item resolves to nothing. The `completion_resolve` handler builds a throwaway
  EMPTY `SemanticModel` per resolve request (`server.rs:676-690`).
- Auto-import items (`auto_import_candidates`, `completion.rs:195-224`) re-call
  `std_module_exports` for every module **on every non-member keystroke** — constructing
  hundreds of runtime `Value`s and discarding them — and carry **no `sort_text`**, so
  `abs (auto-import from std/math)` competes head-on with locals.

### 1.4 The remaining audit hardening items (Unit C)

Each item below was verified against the **current** tree (the audit's pre-fix line numbers
shifted slightly; the citations here are current). Severities are the audit's.

| # | Finding | Severity | Where (current tree) |
|---|---------|----------|----------------------|
| C1 | Member completion requires the cursor flush against the dot: `member_access_alias` demands `chars[offset-1] == '.'`, so manual invoke at `math.sq|` falls to baseline | Major | `completion.rs:420-442` |
| C2 | `workspace_diagnostic` rebuilds a full `SemanticModel` (parse+resolve+full checker incl. inference) per indexed file with **no await/yield** → blocks the single-threaded server O(workspace) | Major | `server.rs:1012-1050` (the build at `:1035`) |
| C3 | Hover re-parses + re-infers the whole file per request (`hover_type_at` takes `&str`, rebuilds everything — `src/check/infer/mod.rs:38-49`); completion C4 builds a fresh `Table` per request (`completion.rs:264`) and calls `hover_type_at` again (`:388`); nothing cached on the model; ~90ms release per 256 KiB rebuild (`src/lsp/perf.rs:30-40`); hover is NOT size-class gated (vs inlay, `server.rs:807-835`) | Minor–Major | `providers/hover.rs:12`, `completion.rs:264,373-392`, `infer/mod.rs:38-49` |
| C4 | Workspace-folder REMOVAL never unindexes: `did_change_workspace_folders` re-warms added roots but removed roots' files stay in the index for the session | Minor | `server.rs:1666-1692` |
| C5 | Index keys are lexically canonicalized only (never touches the fs) → symlink (`/tmp` → `/private/tmp` on macOS) / case-variant URIs key DIFFERENT entries than the disk walk; cross-file def/refs silently miss | Minor | `workspace.rs:977-990` |
| C6 | typeHierarchy is advertised only via the `experimental` escape hatch because lsp-types 0.94 (pinned by tower-lsp 0.20, `Cargo.toml:110`) lacks the field — invisible to most clients; AND every `self.index.read()` site degrades to silent-`None`-forever after a poisoning panic, with no log | Minor | `server.rs:283-287`; the `if let Ok(idx) = self.index.read()` pattern (e.g. `server.rs:128,180`) |
| C7 | Snippet completions never check the client's `snippetSupport`: `initialize` (`server.rs:383-414`) drops `params.capabilities`; `snippet_completions` (`completion.rs:169-188`) and ADT variant snippets (`completion.rs:70-110`) emit `InsertTextFormat::SNIPPET` unconditionally → plain-text clients see literal `${1:...}` | Minor | `server.rs:383-414`, `completion.rs:169-188,97-109` |
| C8 | Completion is not suppressed inside strings/comments (other than the import-path context) — items fire inside any string literal or comment | Minor | `completion.rs:235-296` (no token-kind check) |

### 1.5 What SIG ships

- **Unit A** — the missing data asset: a drift-tested, feature-independent
  `(module, member) → signature` table covering every export of every `STD_MODULES` module
  plus the global builtins, which **subsumes** `std_arity.rs` (one source of truth).
- **Unit B** — three consumers: signature help (member callees: stdlib namespace, typed
  receivers, cross-file imported fns, builtins — plus the kept same-file path), completion
  enrichment (real kind/detail/docs + auto-import sort_text/caching), hover on stdlib members.
- **Unit C** — the eight hardening items above.

## 2. Unit A — the stdlib signature table (the data asset)

### 2.1 Placement & shape

**New module `src/check/std_sigs.rs`, beside `std_arity.rs`.** Rationale:

- It must be usable by **both** the checker (`call-arity` derives min-arity from it, §2.5)
  and the LSP. `src/check/` is the established home for feature-independent curated std
  metadata (`std_arity.rs:1-4`: *"Feature-independent (the checker core builds under
  `--no-default-features`): this is pure DATA, not a feature-gated call into the stdlib"*).
- It is a leaf module of `&'static` data with no feature-gated deps — it compiles identically
  under `--no-default-features` (where the LSP itself is compiled out, but the checker still
  consumes it). The LSP serves source authored against the **full** stdlib regardless of the
  binary's compiled features — exactly the `STD_MODULES` philosophy (`stdlib/mod.rs:214-220`).

```rust
/// One parameter of a curated std signature.
pub struct StdParam {
    pub name: &'static str,
    /// Rendered annotation (`"number"`, `"array"`, `"fn(item)"`) — display text,
    /// NOT a CheckTy (the LSP renders it verbatim; the checker never interprets it).
    pub ty: Option<&'static str>,
    /// Optional trailing param (docs `(optional)` / `arg?`). Never followed by a
    /// required param (mirrors the language's own param rule).
    pub optional: bool,
    /// `...rest` collector — always last when present.
    pub variadic: bool,
    /// Rendered default, when documented (`"0"`, `"\" \""`).
    pub default: Option<&'static str>,
}

/// A curated std fn signature + one-line doc.
pub struct StdSig {
    pub params: &'static [StdParam],
    pub ret: Option<&'static str>,
    /// First sentence of the docs entry — shown in signature-help documentation,
    /// completion resolve, and stdlib-member hover.
    pub doc: &'static str,
}

/// The signature of `(module, name)` (`("std/math", "pow")`), or `None` for a
/// CONSTANT export (pi, e) or an unknown name.
pub fn std_sig(module: &str, name: &str) -> Option<&'static StdSig>;

/// The signature of a global builtin (`print`, `len`, `type`, `assert`, `range`,
/// `Ok`, `Err`, `recover`, `test`, `exit` — the completion `BUILTINS` set).
pub fn builtin_sig(name: &str) -> Option<&'static StdSig>;

/// All curated members of `module` with a flag distinguishing fn vs constant —
/// the completion/auto-import surface (no `Value` construction; feature-independent).
pub fn module_members(module: &str) -> Option<&'static [(&'static str, MemberKind)]>;
```

Authoring uses a declarative macro so a row reads like the docs entry it mirrors
(illustrative; final macro shape is the plan's):

```rust
sig!("std/math", "pow", (base: "number", exp: "number") -> "float",
     "Raise a base to an exponent.");
sig!("std/string", "slice", (s: "string", start: "number", end?: "number") -> "string",
     "Extract a substring between two character indices.");
sig!("std/math", "min", (...nums: "number") -> "float",
     "Return the smallest of one or more arguments.");
sig!("std/array", "map", (arr: "array", f: "fn(item)") -> "array",
     "Apply a function to every element, producing a new array.");
```

(Those four are real entries transcribed from `docs/content/stdlib/collections.md` — the
`math.pow`, `string.slice`, `math.min`, `array.map` sections.)

Constants (`pi`, `e`, …) are NOT in the sig table; they appear in `module_members` as
`MemberKind::Const("float")` so completion gets the right kind + a type detail without
constructing a `Value`.

### 2.2 The docs-notation survey (read before designing the matcher — done)

The brief's assumption of per-module pages (`array.md`, `math.md`, `task.md`) is **wrong on
disk**: `docs/content/stdlib/` is **domain-grouped** (22 pages). Verified notation census:

| Notation | Pages (modules) | Machine-checkable? |
|---|---|---|
| **Style 1 — structured entries:** `### module.fn` heading → prose line → bullet params `- name: type — desc`, `(optional)` marker, `- ...name: type` variadic, `- Returns: type` → fenced example | `collections.md` (string, array, object, map, math, convert, bytes, set — 135 entries), `data.md` (json, csv, regex, encoding, toml, yaml, url, uuid, decimal — 40), `system.md` (fs, process, env, io, crypto, compress — 50), `net.md` (net, tcp, udp, ws — 29), `db.md` (sqlite, postgres, redis + handle methods — 18), `log.md`, `schema.md`, `cli.md` (cli, color) | **Yes** — heading + bullets parse cleanly |
| **Style 2 — backticked heading with inline arg list:** `` ### `module.fn(a, b, msg?)` `` (optionality as `?` suffix), prose body, no bullets | `stream.md` (15), `assert.md` (17), `async.md` (`` `task.retry` ``, time-combinators), parts of `time.md` | **Partially** — names + `?`/`...` from the heading; no types |
| **Style 3 — prose / method tables / bare headings:** `### now` under a `## std/time` section; `\| method \| returns \| notes \|` tables (`utilities.md` lru/events/template, `db.md` conn methods); pure prose (`caps.md`, `ffi.md`, `shared.md`, `ai.md`, `workflow.md`, `telemetry.md`, `tui.md`, `bench.md`) | the rest | **No** — tolerated, not matched |

### 2.3 Authoring decision: hand-curated checked-in table + two-direction drift tests

**Decision: (i) hand-curated Rust table, validated by automated drift tests — NOT
(ii) generated from the docs.** Justification against the survey:

- Generation from docs would cover only Style-1 pages (~60% of entries), would need a
  per-style parser zoo for the rest, and would make a docs prose edit a build-input — the
  wrong coupling direction. The docs stay the **social** source of truth; the table is the
  **machine** source of truth; drift tests bind them.
- This is the established house pattern: `std_arity.rs` is a checked-in curated table with an
  `every_entry_is_a_real_export` drift `#[test]` (`std_arity.rs:124-195`); DECODE's
  superinstruction census follows the same checked-in-data + automated-verification
  discipline (`superpowers/specs/2026-06-12-decoded-dispatch-design.md`, the census mode).

**Drift test (a) — completeness, both directions** (in `std_sigs.rs` tests):

1. For every module in `STD_MODULES` whose `std_module_exports(module)` is `Some` under the
   compiled features: every exported name appears in `module_members(module)`, classified
   fn-vs-constant **consistently with the export's `Value` kind** (`Value::Builtin` ⇄
   `MemberKind::Fn`), and every `MemberKind::Fn` has a `std_sig` row. Feature-gated modules
   are exercised under whichever features the test build has (the default `cargo test` run
   covers all default features; `--no-default-features` covers the core set) — the table
   itself always carries ALL modules.
2. Reverse: every table key is a real export of its module (the existing `std_arity.rs`
   drift-guard direction, inherited).

**This is the new Gate-11-style tooling-parity tripwire:** adding a stdlib fn without
updating the docs page AND the table becomes a CI failure — (1) fails on the missing table
row; the docs-consistency test (b) fails on a table row contradicting (or, for Style-1
modules, missing from) its docs page.

**Drift test (b) — docs consistency** (a `#[test]` parsing `docs/content/stdlib/*.md` from
the repo via `env!("CARGO_MANIFEST_DIR")`):

- A tolerant matcher extracts facts per the survey: Style-1 entries yield
  `(module, fn, [param name, optional?, variadic?], ret?)` from heading + bullets; Style-2
  yields `(module, fn, [name, optional?, variadic?])` from the backticked heading's inline
  arg list; Style-3 yields nothing.
- For every extracted fact: the table entry must exist, param **names** must match in order,
  optionality/variadic flags must match, and `ret` must match where the docs state it.
  **Tolerated variance (explicit):** docs may omit types (Style 2) — type text is compared
  only when both sides state it; receiver-handle methods (db `conn.*`, lru/events handles,
  ffi `lib.symbol`/`sym.call`) match against their curated handle-method rows where present
  and are skipped otherwise; prose pages produce no facts. **Any contradiction fails the
  test** with the page, line, and the two disagreeing renderings.
- Per-module coverage assertion for Style-1 modules: every `module_members` fn of a Style-1
  module must have a parsed docs fact (a curated row with no docs entry = stale docs = fail).

### 2.4 Coverage scope

- All ~56 `STD_MODULES` modules' exports (fns get `StdSig` rows; constants get
  `MemberKind::Const` rows) + the 10 global builtins.
- **Handle/receiver methods** (db `conn.query`, `lru` handle methods, ffi `lib.symbol`,
  stream stage methods, etc.) are included as curated rows keyed under their module with the
  documented receiver-method name where the docs document them — they power hover/completion
  detail only opportunistically in v1 (signature help for handle methods needs receiver-type
  inference for natives, out of scope §7); the rows exist so v2 costs no new data work.
  `std_arity.rs` already keys ffi handle methods this way (`std_arity.rs:170-177`).

### 2.5 Subsuming `std_arity.rs` — one source of truth, no behavior change

The `call-arity` lint consumes exactly `std_fn_arity(module, name) -> Option<Arity>` at
`call_arity.rs:151-154`, flagging only below-`min` calls (`Arity { min, max: None }`,
`std_arity.rs:31-34`).

**Decision: `std_arity.rs` stays as the API (the lint call site is untouched) but becomes a
thin derivation over `std_sigs`** — `min` = the count of leading non-`optional`,
non-`variadic` params; `max = None` always (the zero-FP contract is unchanged and its module
doc moves with it). The hardcoded `required_args` list is **deleted**. Guards:

- A pinned regression test: every one of the ~36 OLD entries derives the **same** `Arity`
  from the new table (no lint behavior change for previously-covered fns).
- The derivation extends lint coverage from ~36 fns to every curated fn — deliberate, and
  guarded by Gate 5 (`examples/**` emits zero new diagnostics in both feature configs) plus
  the docs-consistency test (a wrong `optional` flag that would over-flag is a contradiction
  against the docs bullets). The existing zero-FP direction (never flag too-many) is
  preserved structurally by `max = None`.
- The `every_entry_is_a_real_export` drift test migrates into the §2.3 completeness test
  (strictly stronger).

## 3. Unit B — the three consumers

### 3.1 Signature help coverage (`providers/signature.rs`)

`signature_help` gains a resolution ladder over the **same** `enclosing_call` walk, extended
to accept a `MemberExpr` callee `(receiver NameRef, property name)` alongside the existing
`NameRef` form. Provider signature becomes
`signature_help(model, offset, index: Option<&WorkspaceIndex>, doc_path: Option<&Path>)`;
the `signature_help` handler (`server.rs:790-805`) passes `self.index.read().ok()` +
`url_to_canon(&uri)` (the same pattern `goto_definition` and `analyze_and_publish:179-185`
already use). Resolution order:

1. **(e — kept) Same-file unique `FnDecl`** for a `NameRef` callee — today's path, unchanged,
   tried first (a user fn shadowing a builtin name wins, matching the runtime's
   shadow-a-builtin semantics).
2. **(d) Global builtin** for a `NameRef` callee with no same-file decl → `builtin_sig(name)`.
3. **(c) Cross-file imported user fn** for a `NameRef` callee bound to a unique file-module
   import → a new `WorkspaceIndex::exported_fn_signature(module, name) ->
   Option<ExportedFnSig>` extending the existing `exported_fn_arity` walk
   (`workspace.rs:419-445`) to return **param names + annotation text + optional/rest flags**
   (the same `ParamList` it already re-parses; `exported_fn_arity` becomes a thin wrapper so
   the D-arity consumer is untouched). Import-target resolution reuses the
   `file_module_arity` unique-import mapping (`workspace.rs:466-499`).
4. **(a) Stdlib member call** for a `MemberExpr` callee whose receiver `NameRef` is a
   namespace import: resolve alias → module path with the **same** detection completion uses
   (`namespace_import_module`, `completion.rs:450-486` — promoted to a shared
   `pub(crate)` helper, not duplicated), then `std_sig(module, member)`.
5. **(b) Method on a typed receiver** for a `MemberExpr` callee whose receiver is NOT a
   namespace import: resolve the receiver's class exactly as completion C4 does
   (`receiver_class_info`, `completion.rs:373-392`: `hover_type_at` on the receiver span →
   leading class ident → `Table::class_id`). The `Table`'s `method_sigs:
   HashMap<String, MethodSig>` carries param **types** + return (`table.rs:28,83-87,227-238`)
   but **no param names** (a brief-vs-code correction: `ClassInfo.methods` at `table.rs:26`
   holds only return `CheckTy`s; the full sigs live in `method_sigs`, names nowhere) — so
   param names + annotations come from the same-file `ClassDecl → MethodDecl → ParamList`
   CST walk (the class is in-file by `Table::build` construction), with `method_sigs`/
   `CheckTy::display` (`ty.rs:712`) as the type renderer where the CST annotation is absent.

Rendering, for every rung:

- `SignatureInformation.label` = `name(p1: t1, p2?: t2, ...rest: t3) -> ret` (annotations and
  return omitted where unknown — same-file/cross-file user fns render exactly the
  annotations the source carries).
- **`ParameterLabel::LabelOffsets`** (UTF-16 offsets into the label — cheap, we build the
  label so the offsets are known at construction) for precise client highlighting;
  `ParameterLabel::Simple` remains only if a label is somehow non-derivable.
- `SignatureInformation.documentation` = the one-line doc (`StdSig.doc` for std/builtins;
  the `///` doc-comment line via the existing `docs` provider machinery for user fns where
  present).
- Active parameter = the existing top-level comma count (`active_param_index`,
  `signature.rs:88-101`, unchanged — its nested-arg-list exclusion is already correct), then
  **clamped to the last param index when that param is `variadic`** (typing past `...nums`
  keeps the rest param highlighted instead of overflowing).
- Unknown member / unresolvable receiver → `None` exactly as today (zero-FP discipline).

### 3.2 Completion enrichment (`providers/completion.rs`)

- **C2 namespace-member branch** (`completion.rs:249-259`): each export maps with a real
  kind — `Value::Builtin` → `FUNCTION`, else `CONSTANT` (defensive: `Value::Class` →
  `CLASS`) — `detail` = the rendered signature (`(base: number, exp: number) -> float`) from
  `std_sig`, or the constant's type name from `module_members`; and a
  `data: {"module": …, "name": …}` payload so `completionItem/resolve` can find it.
- **`resolve_completion`** (`completion.rs:305-318`): when `item.data` carries
  `{module, name}`, documentation = `StdSig.doc` (Markdown); the builtin/keyword paths stay.
  The `completion_resolve` handler's throwaway empty `SemanticModel` (`server.rs:683-688`)
  is dropped — resolve becomes a static fn needing no model.
- **Auto-import items** (`completion.rs:195-224`): (1) the candidate list is built **once
  per process** into a `OnceLock<Vec<…>>` of `(module, name, kind)` derived from
  `module_members` — no per-keystroke `std_module_exports` `Value` construction (the audit's
  flood finding); (2) every auto-import item gets `sort_text` deprioritization (a `"zz"`
  prefix class) so locals/keywords always rank first; (3) kind/detail follow the C2 rules.

### 3.3 Hover on stdlib members (`providers/hover.rs`)

`hover` (`hover.rs:10-28`) currently joins `hover_type_at` + `docs::doc_at`. Add a member
branch ahead of them: detect `alias.member` at the offset (identifier containing the offset,
preceded by `.` and a leading identifier — the same parser-free scan family as
`member_access_alias`), resolve `alias` via the shared `namespace_import_module`, and on a
`std_sig`/`module_members` hit render:

````markdown
```ascript
math.pow(base: number, exp: number) -> float
```
Raise a base to an exponent.
````

(constants render `math.pi: float`). Falls through to the existing parts on a miss; the
existing `---`-joined layout is kept.

## 4. Unit C — audit hardening (the §1.4 table, fix designs)

- **C1 — partial-identifier member completion** (`completion.rs:420-442`).
  `member_access_alias` backtracks over trailing identifier chars before requiring the `.`;
  returns `(alias, typed_prefix)`. Member items are returned with `filter_text`/the prefix
  honored (return the full member set and set the prefix as `filter_text` — clients filter;
  no server-side narrowing that could fight client fuzzy-matching).
- **C2 — `workspace_diagnostic` yielding + model reuse** (`server.rs:1012-1050`).
  `tokio::task::yield_now().await` per file; AND reuse an already-built model when the open
  `DocumentStore` holds the same text for the file's URI (open files are exactly the hot
  ones) instead of an unconditional rebuild.
- **C3 — cache the inference pass per `SemanticModel` generation + gate hover by size.**
  `SemanticModel` (`model.rs`) gains a `std::sync::OnceLock<InferCache>` (the store is
  shared across tower-lsp tasks; `OnceLock` keeps the model `Send + Sync`) holding the
  hover-span list (`pass::collect_hover_types`) and the built `Table` for the model's
  tree/resolved. New entry `crate::check::infer::hover_type_in(cache_or(model), offset)`;
  `hover.rs:12`, `completion.rs:264` (`Table::build`) and `receiver_class_info`'s
  `hover_type_at` call (`completion.rs:388`) consume the cache. The cache is built lazily on
  first use (didOpen/diagnostics latency unchanged) and dies with the model (generation
  identity is structural — a new model is a new `OnceLock`). Hover gets the same size-class
  gate inlay has (`server.rs:807-835` pattern; `SizeClass::Large`+ → type-part skipped,
  docs part kept).
- **C4 — workspace-folder removal unindexing** (`server.rs:1666-1692`). For each removed
  root: `fully_unindex` (`workspace.rs:382-385`) every indexed file under it that is not
  also under a surviving root.
- **C5 — fs-canonicalized index keys** (`workspace.rs:977-990`). `canonicalize` tries
  `std::fs::canonicalize` first and falls back to the existing lexical normalization
  (non-existent paths, tests). One choke-point change — every keying site already routes
  through this fn.
- **C6 — typeHierarchy advertising decision + poisoned-lock logging.** **Decision:
  do NOT upgrade.** tower-lsp 0.20 is the latest release and pins lsp-types 0.94
  (`Cargo.toml:110`); a newer lsp-types means replacing the server framework — a blast
  radius wildly out of proportion. The `experimental` advertisement stays
  (`server.rs:283-287`) and the client-invisibility is **documented** in
  `docs/content/tooling/lsp-capabilities.md` + the capability fn's comment. Poisoned-lock:
  a `AtomicBool`-guarded one-time `tracing`/stderr log when any `self.index.read()`/`
  .write()` returns `Err`, replacing today's silent `None`-forever.
- **C7 — snippet capability gating.** `initialize` (`server.rs:383-414`) captures
  `params.capabilities.text_document.completion.completion_item.snippet_support` into the
  backend (an `AtomicBool` beside `settings`); `snippet_completions` and
  `variant_completion_item` take the flag and emit plain-text inserts (`fn name() {}` body
  text without `${…}` tab-stops; payload variants insert `Variant(` with no placeholders)
  when unsupported.
- **C8 — string/comment suppression.** `completions` consults `model.tokens` (already on
  the model): if the cursor token is `Str`/`TemplateStr`/`TemplateStart`/`TemplateMiddle`/
  `TemplateEnd`/`LineComment`/`BlockComment` (`src/syntax/kind.rs:129-139`) AND the
  import-path context (`in_import_path_string`) does not apply → return empty. `${…}`
  template-interpolation interiors lex as ordinary tokens, so completion inside
  interpolations keeps working by construction.

## 5. Test matrix (Gates 9–11)

Unit tests live beside the code (provider `#[cfg(test)]` modules, `std_sigs.rs` tests);
integration tests in `tests/lsp.rs` use the existing spawn-the-binary harness
(initialize → didOpen → request → assert; e.g. the signatureHelp idiom at
`tests/lsp.rs:644-663`).

| Area | Test (level) |
|---|---|
| A: completeness drift | every export ⇄ table row, kind-consistent (unit, both feature configs) |
| A: docs consistency | the §2.3 matcher over all Style-1/2 pages; deliberate-contradiction self-test (mutate a parsed fact, assert it trips) (unit) |
| A: std_arity parity | all ~36 legacy entries derive identical `Arity`; `call-arity` lint behavior pinned on a std import case (unit) |
| B1a: stdlib member signature | `array.map(` shows `arr`,`f` with doc; comma advances active param (unit + integration) |
| B1b: method signature on typed receiver | `let c = C(); c.m(` shows `m`'s named params (unit + integration) |
| B1c: cross-file imported fn | two-file fixture: `import { add } from "./util"` → `add(a, b)` with names AND annotation text, not just arity (integration, temp-dir workspace) |
| B1d: builtin | `print(` shows the builtin sig (unit) |
| B1: negatives | unknown member → `None`; ambiguous receiver → `None`; variadic clamp (`math.min(1, 2, 3` stays on `...nums`) (unit) |
| B2: completion detail/kind/docs | `math.` → `pi` as CONSTANT with type detail; `pow` as FUNCTION with sig detail; resolve fills doc from `data` (unit + integration) |
| B2: auto-import | items carry `sort_text` deprioritization; candidate list identical across two calls (cached) (unit) |
| B3: hover | hover on `math.sqrt` shows sig + doc line; on `math.pi` shows the constant form (unit + integration) |
| C1 | completion at `math.sq|` (manual invoke) offers `sqrt` with `filter_text` (unit + integration) |
| C2 | workspace_diagnostic over a many-file index returns complete results; a concurrent request is answered while it runs (integration) |
| C3 | hover twice on one generation builds inference once (counter probe, unit); Large-class doc keeps docs-part hover (unit) |
| C4 | add + remove a folder → removed root's symbols gone from workspace/symbol (integration, temp dirs) |
| C5 | symlinked tmp-dir workspace: def/refs resolve across the symlink (unit on `canonicalize` + integration where the platform allows) |
| C6 | poisoned-lock log fires once (unit, force-poison via `catch_unwind` writer) |
| C7 | initialize WITHOUT snippetSupport → completion items contain no `${` (integration); with it → snippets present (existing tests keep passing) |
| C8 | completion inside a string/comment → empty; inside `${…}` interpolation → normal; import-path context still completes (unit) |

Everything green under `cargo test --test lsp`, the lib unit suites, and clippy in BOTH
feature configs. **`tests/vm_differential.rs` run once, untouched and green — the proof SIG
has no engine surface.**

## 6. Performance

No engine surface ⇒ Gates 12/17 are trivially held (no VM code touched; re-run
`vm_differential` once as the proof, §5). LSP-side, SIG is a net **reduction**: C3 removes
the per-hover full re-infer (~90ms/256 KiB release, `perf.rs:30-40`) and the per-completion
`Table::build`; §3.2 removes the per-keystroke ~600-`Value` auto-import construction; C2
removes the O(workspace) server blackout. The table is `&'static` data — zero startup cost,
a few hundred KB of rodata.

## 7. Scope & rejected

**In scope:** Units A/B/C as specified; the `std_arity` subsumption; docs page update
(`docs/content/tooling/lsp-capabilities.md` — signature help/completion/hover rows + the
typeHierarchy visibility note); `CLAUDE.md` DX note; `roadmap.md` + `goal-perf.md` status.

**Out of scope / rejected:**

- **Runtime signature introspection of native fns** — impossible: they are Rust closures
  over `Value` (`src/stdlib/mod.rs`); there is nothing to introspect. The curated table is
  the only honest source.
- **Shipping the table over the wire / into `.aso`** — No. LSP/checker-internal static data;
  `ASO_FORMAT_VERSION` untouched.
- **Signature help for the dynamic call-site hooks** (`std/schema` fluent methods, workflow
  `ctx.*`, `shared`/handle method receivers needing native-receiver inference) — out of
  scope v1, documented here per the no-silent-deferral rule. The curated handle-method rows
  (§2.4) exist so a v2 needs only receiver resolution, no new data.
- **Generating the table from docs** — rejected per the §2.2/§2.3 survey (heterogeneous
  notation; wrong coupling direction); the docs-consistency drift test captures the value
  without the fragility.
- **Upgrading tower-lsp/lsp-types** (the typeHierarchy field) — rejected: tower-lsp 0.20 is
  the final release pinning lsp-types 0.94; an upgrade is a framework replacement. Decision
  recorded in §4 C6; separately revisitable if the server framework is ever replaced.
- **Method-receiver signature help on complex receiver expressions** (`f().m(`) — v1 mirrors
  completion C4's NameRef-receiver shape; documented narrowing, not a silent drop.

## 8. Grounding (verified against the CURRENT tree, 2026-06-12, incl. the uncommitted reliability fixes)

- `src/lsp/providers/signature.rs` (159 lines): module-doc limitation `:1-6`;
  `signature_help:17-27`; `enclosing_call:31-57` (NameRef-only at `:44`);
  `find_fn_decl:60-70`; `param_names:73-84`; `active_param_index:88-101`;
  `make_help:103-122` (Simple labels, no docs).
- `src/lsp/providers/completion.rs`: `STD_MODULES` reuse `:55`; C2 bare-label branch
  `:249-259`; Context-3 table build `:264`; Context-4 `receiver_class_info:373-392`
  (`hover_type_at` at `:388`); `auto_import_candidates:195-224` (no sort_text, per-call
  exports); `resolve_completion:305-318`; `member_access_alias:420-442` (the
  `chars[offset-1] == '.'` gate at `:425`); `namespace_import_module:450-486`;
  `snippet_completions:169-188`; `variant_completion_item:70-110` (unconditional SNIPPET);
  `BUILTINS:34-36`.
- `src/lsp/server.rs`: `superseded:91-94`; `flush_pending_for:112-123`;
  `reindex_uri:126-132` (the `if let Ok` index pattern); `analyze_and_publish:169-187`;
  `server_capabilities:224+` (signatureHelp `(`/`,` triggers `:317-321`; typeHierarchy via
  `experimental` `:283-287`); `initialize:383-414` (drops `params.capabilities`);
  `did_change:540-596` (single-critical-section fold); `did_close:598-609` (pending purge);
  hover handler `:624-645`; completion handler `:647-674`; `completion_resolve:676-690`
  (throwaway empty model `:683-688`); signature_help handler `:790-805`; inlay size-class
  gate `:807-835`; `workspace_diagnostic:1012-1050` (per-file build `:1035`, no yield);
  `did_change_workspace_folders:1666-1692` (no removal unindex); `Backend` fields (documents/
  index/roots/settings/pending/edit_seq/index_cancelled).
- `src/lsp/workspace.rs`: `fully_unindex:382-385`; `exported_fn_arity:419-445` (the
  ParamList re-parse that discards names); `file_module_arity:456-530`;
  `canonicalize:977-990` (lexical-only).
- `src/lsp/providers/hover.rs`: `hover:10-28` (`hover_type_at` at `:12`); no size gating.
- `src/lsp/model.rs`: `SemanticModel` fields (text/tree/resolved/tokens/line_index/
  generation); `size_class`.
- `src/lsp/perf.rs`: `LARGE_FILE_BYTES` 256 KiB `:19`; the ~90ms-release / ~2.6s-debug
  rebuild finding `:30-40`.
- `src/check/std_arity.rs`: zero-FP contract `:6-21`; `std_fn_arity:31-34`;
  `required_args:40-99` (~36 entries); drift guard `:124-195` (ffi handle-method skip
  `:170-177`).
- `src/check/rules/call_arity.rs`: std-import consumption `:151-154`; `import_module` map
  `:76-88`. `src/check/rules/mod.rs`: `Arity:113-117`; `arity_of:125-150`;
  `decl_arity:154-162`.
- `src/stdlib/mod.rs`: `bi:107-109`; `std_module_exports:113-210`; `STD_MODULES:221-280`
  (~56 modules); `is_known_std_module:283-285`. `src/stdlib/math.rs::exports` (Builtin fns +
  `Value::Float` constants `pi`/`e`).
- `src/check/infer/mod.rs`: `hover_type_at:38-49` (full re-parse/resolve/table/pass per
  call). `src/check/infer/table.rs`: `ClassInfo:17-35` (`methods` = return-`CheckTy` map
  `:26`; `method_sigs` `:28`); `MethodSig:83-87` (param TYPES, no names); method lowering
  `:227-238`. `src/check/infer/ty.rs`: `CheckTy::FnSig:95`; `display:712`.
- `src/syntax/kind.rs:129-139`: `LineComment`/`BlockComment`/`Str`/`TemplateStr`/
  `Template{Start,Middle,End}`.
- `tests/lsp.rs` (2791+ lines): spawn-binary JSON-RPC harness; signatureHelp integration
  idiom `:644-663`; completion idioms `:1577-1631`.
- `Cargo.toml:110`: `tower-lsp = "0.20"` (pins lsp-types 0.94); `:235` the `lsp` feature.
- Docs survey (§2.2): `docs/content/stdlib/` — 22 domain-grouped pages; entry styles
  verified in `collections.md` (`### math.pow` + bullets + `Returns:`), `stream.md`/
  `assert.md`/`async.md` (`` ### `assert.eq(a, b, msg?)` `` backticked inline-args),
  `time.md` (`### now` bare), `utilities.md` (method tables). The brief's per-module
  filenames (`array.md`, `math.md`, `task.md`) do not exist — recorded as a brief-vs-tree
  correction. `docs/assets/app.js` NAV: `tooling/lsp-capabilities` page exists.
