# LSP Phase 7 — Performance & Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. This is the FINAL phase of the first-class-LSP campaign — it optimizes the `SemanticModel`/`DocumentStore` Phase 0 introduced, makes the server cancellation-aware and large-file-safe, and publishes the editor setup + capability-reference docs.

**Goal:** Make the LSP responsive and robust under real editing pressure, then polish the user-facing surface. Concretely: (1) bound and coalesce model rebuilds so a large file under rapid keystrokes never blocks the request queue; (2) honor `$/cancelRequest` and supersession so the server never computes obsolete results; (3) degrade expensive providers gracefully above a size threshold (range-only / skipped, logged, never a hang); (4) make initial workspace indexing report cancellable work-done progress; (5) publish a per-editor setup guide (VS Code/Zed/Neovim) and a capability-reference page under `docs/content/`, linked from `README.md`; (6) harden `tests/lsp.rs` to exercise the **full advertised capability set** end-to-end plus the consistency invariant *LSP diagnostics ≡ `ascript check`*.

**Architecture:** The parser is a **full-reparse** front-end — `crate::syntax::parser::parse(src)` re-lexes and re-parses from scratch (confirmed at `src/syntax/parser.rs:170` → `Parser::new` re-runs `lex_with_errors`; `src/syntax/tree_builder.rs:21` builds a fresh `GreenNodeBuilder` each call). cstree 0.14 (`Cargo.toml:31`) interns green nodes **within one build** but exposes **no incremental-reparse/green-node-reuse API** (no `reparse`, no reusable `NodeCache` across calls — see the `GreenNodeBuilder` usages in `src/syntax/{tree_builder,cst}.rs`). Therefore the concrete optimization is **NOT** green-node reuse; it is **edit coalescing/debounce + a measured rebuild budget + a large-file degradation bound**. We add: a generation counter on `DocumentStore` so a superseded rebuild's result is dropped; a debounce that collapses a burst of `did_change` edits into one rebuild; a `SizeClass` on `SemanticModel` that gates expensive providers; per-request cancellation tokens wired from tower-lsp's `$/cancelRequest`; and a cancellable work-done progress report around `initialized` indexing. Every change keeps the backend `Send + Sync`, holds no `Rc`/`RefCell`/`Value`, and imports no `crate::{ast,lexer,parser,token}`.

**Tech Stack:** Rust, `tower-lsp`, `cstree` 0.14, `tokio` (`current_thread` runtime), the `src/syntax/` + `src/check/` analysis core. Benchmarks use a plain `std::time::Instant`-budget test (no new dev-dependency) so the gate runs in `cargo test` under both feature configs.

**Reference (read before starting):**
- `docs/superpowers/specs/2026-06-05-lsp-first-class-design.md` §6 (Phase 7), §7 (testing strategy), §8 (risks — esp. incremental-sync correctness).
- `docs/superpowers/plans/2026-06-05-lsp-phase0-unification-foundation.md` — the `SemanticModel`/`DocumentStore`/`apply_changes` shapes this phase optimizes.
- `src/lsp/model.rs` — `SemanticModel { text, version, tree, resolved, diagnostics, tokens, line_index }`; `SemanticModel::build(text, version, &LintConfig)`; `DocumentStore { set, get, remove }`; `apply_changes`.
- `src/lsp/server.rs` — `Backend { client, documents: Mutex<DocumentStore>, index, roots }`; `analyze_and_publish`; `did_change`; `server_capabilities`; the per-request handlers.
- `src/syntax/parser.rs:170` (`parse`), `src/syntax/tree_builder.rs` (`build_tree`) — confirms full-reparse, no cstree reuse API.
- `tests/lsp.rs` — the JSON-RPC-over-stdio smoke test to extend.
- `src/check/analyze.rs` (`analyze_with_config`) + `src/check/config_toml.rs` (`config_for_file`) — for the diagnostics-≡-`ascript check` invariant.
- Docs layout: `docs/assets/app.js` `NAV` (sidebar/search source of truth), `docs/content/cli.md` (page style to mirror), `README.md:78` (CLI table) + `README.md:156` (LSP paragraph).

**Run the whole suite with:** `cargo test --lib lsp` (LSP unit + perf-budget tests), `cargo test --test lsp` (protocol smoke), and `cargo test` (full). Clippy gate: `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets` must be clean.

> **API uncertainty flagged up front (resolve in Task 1, Step 0 before coding):**
> - **No cstree incremental API.** Confirmed by reading `src/syntax/{parser,tree_builder,cst}.rs`: `parse` always re-lexes; `build_tree` always allocates a fresh `GreenNodeBuilder`. Green-node reuse would require threading a persistent `cstree` interner/cache through `parse`+`build_tree` (a front-end change out of LSP scope and risky against the VM differential). **Decision: do NOT attempt green-node reuse.** The concrete, in-scope optimization is the debounce/coalesce + budget + size-bound below. Task 1 records this in a code comment + the capability docs so it is a documented choice, not an omission.
> - **tower-lsp cancellation surface.** tower-lsp auto-replies `ContentModified`/`RequestCancelled` for an in-flight request when a `$/cancelRequest` for its id arrives, but it does NOT pre-empt a handler already running (handlers are `async` and cooperatively scheduled). Our cancellation is therefore **supersession-based**: each rebuild/expensive computation checks a per-document `generation` (bumped on every edit) and bails early when stale. Confirm `tower_lsp::lsp_types` exposes `WorkDoneProgress*` and `NumberOrString` (it does — already used in `server.rs`/`model.rs`); if a tower-lsp version mismatch surfaces, pin behavior to the supersession path (which needs no protocol cancellation token at all).

---

## File Structure

- Modify `src/lsp/model.rs` — add `generation: u64` to `SemanticModel`; add `SizeClass` + `SemanticModel::size_class()`; a `DocumentStore` generation counter (`next_gen`, `current_gen(uri)`); a `build_budgeted` timed builder used by the perf test.
- Create `src/lsp/perf.rs` — the rebuild-budget timed test + the size-threshold constants (`LARGE_FILE_BYTES`, `HUGE_FILE_BYTES`) as the single source of truth, re-exported where needed.
- Modify `src/lsp/server.rs` — debounce/coalesce `did_change`; per-request supersession via the document generation; large-file provider degradation (semantic tokens range-only, inlay skipped) with a `log_message` note; cancellable work-done progress around `initialized` indexing.
- Modify `src/lsp/mod.rs` — `pub mod perf;`.
- Modify `tests/lsp.rs` — a full-capability end-to-end exercise + a large-file-no-hang test + the diagnostics-≡-`ascript check` consistency test.
- Create `docs/content/tooling/editor-setup.md` — per-editor (VS Code/Zed/Neovim) setup guide.
- Create `docs/content/tooling/lsp-capabilities.md` — the capability reference (every supported LSP method).
- Modify `docs/assets/app.js` — add a "Tooling" NAV section pointing at the two new pages.
- Modify `README.md` — link the two new docs pages from the tooling/LSP area.

---

## Task 1: Size class + generation on the model (the degradation + supersession primitives)

**Files:**
- Create: `src/lsp/perf.rs` (constants + a placeholder the budget test fills in Task 2)
- Modify: `src/lsp/model.rs` (`SizeClass`, `SemanticModel.generation`, `size_class()`)
- Modify: `src/lsp/mod.rs` (`pub mod perf;`)
- Test: inline in `src/lsp/model.rs`

- [ ] **Step 0: Confirm the no-reuse decision (no code).** Read `src/syntax/parser.rs:170` and `src/syntax/tree_builder.rs:16-70` and verify there is no `reparse`/persistent-cache entry point. Record the finding as a `//!`-comment fact in `src/lsp/perf.rs` (Step 2). This is the single place that justifies "debounce, not green-node reuse".

- [ ] **Step 1: Declare the module + thresholds.**

In `src/lsp/mod.rs` add alongside the existing `pub mod` lines:

```rust
pub mod perf;
```

Create `src/lsp/perf.rs`:

```rust
//! LSP performance bounds and budgets.
//!
//! NOTE ON INCREMENTAL REBUILDS: the AScript front-end is a FULL-REPARSE design.
//! `crate::syntax::parser::parse` re-lexes the whole source (parser.rs ~L170 →
//! `Parser::new` runs `lex_with_errors`) and `tree_builder::build_tree` allocates a
//! FRESH `cstree` `GreenNodeBuilder` each call (tree_builder.rs ~L21) — cstree 0.14
//! exposes no cross-build green-node-reuse / `reparse` API. So Phase 7 does NOT do
//! green-node reuse (that would be an out-of-scope, differential-risky front-end
//! change). The responsiveness strategy is instead: DEBOUNCE/COALESCE rapid edits
//! into one rebuild, BOUND each rebuild with a measured budget, and DEGRADE
//! expensive providers above a size threshold. See `model::SizeClass`.

/// At or above this source size, the document is "large": semantic-tokens FULL is
/// downgraded to range-only and inlay hints are skipped (with a logged note).
pub const LARGE_FILE_BYTES: usize = 256 * 1024; // 256 KiB

/// At or above this size, the document is "huge": only diagnostics + navigation run;
/// all token/inlay/folding/color providers return empty rather than risk a stall.
pub const HUGE_FILE_BYTES: usize = 2 * 1024 * 1024; // 2 MiB

/// The per-edit model-rebuild budget asserted by the perf test (Task 2). A rebuild
/// of a `LARGE_FILE_BYTES`-class document must complete under this on CI hardware.
/// Generous (release/debug, shared CI) but still a hard ceiling that trips if a
/// provider accidentally makes `build` super-linear.
pub const REBUILD_BUDGET_MS: u128 = 750;
```

- [ ] **Step 2: Write the failing test for `SizeClass` + `generation`.**

Append to `src/lsp/model.rs`:

```rust
use crate::lsp::perf::{HUGE_FILE_BYTES, LARGE_FILE_BYTES};

/// How expensive providers should treat a document, by source size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeClass {
    /// Normal: every provider runs at full fidelity.
    Normal,
    /// Large: semantic-tokens FULL degrades to range-only; inlay hints skipped.
    Large,
    /// Huge: only diagnostics + navigation; token/inlay/folding/color return empty.
    Huge,
}

impl SemanticModel {
    /// The document's size class (drives provider degradation).
    pub fn size_class(&self) -> SizeClass {
        match self.text.len() {
            n if n >= HUGE_FILE_BYTES => SizeClass::Huge,
            n if n >= LARGE_FILE_BYTES => SizeClass::Large,
            _ => SizeClass::Normal,
        }
    }
}

#[cfg(test)]
mod size_tests {
    use super::*;

    #[test]
    fn small_file_is_normal() {
        let m = SemanticModel::build("let x = 1\n".to_string(), None, &LintConfig::default());
        assert_eq!(m.size_class(), SizeClass::Normal);
    }

    #[test]
    fn threshold_classifies_large_and_huge() {
        let large = "a".repeat(LARGE_FILE_BYTES);
        let m = SemanticModel::build(large, None, &LintConfig::default());
        assert_eq!(m.size_class(), SizeClass::Large);

        let huge = "a".repeat(HUGE_FILE_BYTES);
        let m = SemanticModel::build(huge, None, &LintConfig::default());
        assert_eq!(m.size_class(), SizeClass::Huge);
    }
}
```

Add a `generation: u64` field to `SemanticModel` (defaulted to `0` in `build`; `DocumentStore` overwrites it in Task 3). In the `SemanticModel` struct definition add `pub generation: u64,` and in `build` set `generation: 0` in the struct literal.

- [ ] **Step 3: Run to verify it fails, then passes.**

Run: `cargo test --lib lsp::model::size_tests`
Expected: compile error first if `generation` is referenced before being added anywhere — add the field; then PASS (2 tests). A `"a".repeat(...)` of `a`s is a single long token but still parses (it is one identifier expression statement) — `build` must not panic on it.

- [ ] **Step 4: Commit.**

```bash
git add src/lsp/perf.rs src/lsp/model.rs src/lsp/mod.rs
git commit -m "feat(lsp): SizeClass + model generation + perf thresholds (degradation primitives)"
```

---

## Task 2: Rebuild-budget perf test (measurement FIRST)

Write the measurement before any optimization, with an explicit budget assertion, so a regression that makes `SemanticModel::build` super-linear trips the gate.

**Files:**
- Modify: `src/lsp/perf.rs` (the timed test)
- Test: inline in `src/lsp/perf.rs`

- [ ] **Step 1: Write the budget test.**

Append to `src/lsp/perf.rs`:

```rust
#[cfg(test)]
mod budget_tests {
    use super::*;
    use crate::check::LintConfig;
    use crate::lsp::model::SemanticModel;
    use std::time::Instant;

    /// Build a realistic large AScript source: many small functions, so the parser /
    /// resolver / checker all do real work (not one giant token).
    fn large_source(target_bytes: usize) -> String {
        let unit = "fn f_NUM(a, b) {\n  let s = a + b\n  return s * 2\n}\n";
        let mut out = String::with_capacity(target_bytes + unit.len());
        let mut i = 0usize;
        while out.len() < target_bytes {
            out.push_str(&unit.replace("NUM", &i.to_string()));
            i += 1;
        }
        out
    }

    #[test]
    fn large_file_rebuild_is_under_budget() {
        let src = large_source(LARGE_FILE_BYTES);
        // Warm once (allocator / interner) so the measured run is steady-state.
        let _ = SemanticModel::build(src.clone(), Some(1), &LintConfig::default());

        let start = Instant::now();
        let model = SemanticModel::build(src, Some(2), &LintConfig::default());
        let elapsed = start.elapsed().as_millis();

        assert!(
            elapsed < REBUILD_BUDGET_MS,
            "large-file model rebuild took {elapsed}ms, budget is {REBUILD_BUDGET_MS}ms \
             (a provider likely went super-linear)"
        );
        // Sanity: the build is actually the large class.
        assert!(model.text.len() >= LARGE_FILE_BYTES);
    }
}
```

- [ ] **Step 2: Run the budget test.**

Run: `cargo test --lib lsp::perf::budget_tests`
Expected: PASS. If it FAILS, that is a real signal — profile `SemanticModel::build`; the likely culprit is a provider/analysis pass that is O(n²) in token count. Do NOT raise `REBUILD_BUDGET_MS` to make it pass; fix the cause or, if the cost is irreducibly in the shared checker, file it and proceed (the debounce in Task 4 still bounds *user-perceived* latency, but record the finding).

- [ ] **Step 3: Commit.**

```bash
git add src/lsp/perf.rs
git commit -m "test(lsp): rebuild-budget perf gate for large files (measurement first)"
```

---

## Task 3: `DocumentStore` generation tracking (supersession source of truth)

Give the store a monotone generation per `set`, so a handler can capture the generation it was scheduled at and detect that a newer edit has superseded it.

**Files:**
- Modify: `src/lsp/model.rs` (`DocumentStore` gen counter + `set_versioned` returning the gen)
- Test: inline in `src/lsp/model.rs`

- [ ] **Step 1: Write the failing test.**

Append to `src/lsp/model.rs` (`store_tests` or a new module):

```rust
#[cfg(test)]
mod gen_tests {
    use super::*;
    use tower_lsp::lsp_types::Url;

    #[test]
    fn set_bumps_generation_monotonically() {
        let mut store = DocumentStore::new();
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        let g1 = store.set_versioned(uri.clone(), "let x = 1\n".to_string(), Some(1));
        let g2 = store.set_versioned(uri.clone(), "let x = 2\n".to_string(), Some(2));
        assert!(g2 > g1, "generation must increase per edit: {g1} -> {g2}");
        assert_eq!(store.current_gen(&uri), Some(g2));
        // The stored model carries its generation.
        assert_eq!(store.get(&uri).unwrap().generation, g2);
    }

    #[test]
    fn stale_generation_is_detectable() {
        let mut store = DocumentStore::new();
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        let g1 = store.set_versioned(uri.clone(), "let x = 1\n".to_string(), Some(1));
        let _g2 = store.set_versioned(uri.clone(), "let x = 2\n".to_string(), Some(2));
        // A handler holding g1 sees it is no longer current.
        assert!(store.current_gen(&uri) != Some(g1), "g1 should be stale");
    }
}
```

- [ ] **Step 2: Implement the gen counter.**

Add to `DocumentStore`:

```rust
#[derive(Default)]
pub struct DocumentStore {
    models: HashMap<Url, SemanticModel>,
    next_gen: u64,
}
```

Add methods (keep the existing `set` delegating to `set_versioned` for back-compat with Phase 0 callers):

```rust
impl DocumentStore {
    /// Build + store the model, stamping it with a fresh monotone generation, which
    /// is returned so the caller can later detect supersession.
    pub fn set_versioned(&mut self, uri: Url, text: String, version: Option<i32>) -> u64 {
        self.next_gen += 1;
        let gen = self.next_gen;
        let config = config_for_uri(&uri);
        let mut model = SemanticModel::build(text, version, &config);
        model.generation = gen;
        self.models.insert(uri, model);
        gen
    }

    /// Back-compat shim (Phase 0 `set`): build + store, discarding the generation.
    pub fn set(&mut self, uri: Url, text: String, version: Option<i32>) {
        let _ = self.set_versioned(uri, text, version);
    }

    /// The generation currently stored for `uri` (None if not open).
    pub fn current_gen(&self, uri: &Url) -> Option<u64> {
        self.models.get(uri).map(|m| m.generation)
    }
}
```

- [ ] **Step 3: Run to verify it passes.**

Run: `cargo test --lib lsp::model`
Expected: PASS (existing + new gen tests).

- [ ] **Step 4: Commit.**

```bash
git add src/lsp/model.rs
git commit -m "feat(lsp): DocumentStore per-edit generation (supersession source of truth)"
```

---

## Task 4: Debounce/coalesce rapid `did_change` edits

Collapse a burst of keystrokes into a single rebuild + publish. A short debounce window means N rapid edits within the window cost ONE rebuild, and only the latest text is analyzed — older edits are coalesced (their `apply_changes` results are folded forward, never analyzed).

**Files:**
- Modify: `src/lsp/server.rs` (`Backend` gains a pending-edit map; `did_change` schedules a coalesced rebuild)
- Test: a unit test on the coalescing helper + the protocol test (Task 9) covers the wire path.

- [ ] **Step 1: Add a debounce-coalescing helper (pure, testable).**

In `src/lsp/server.rs`, add near the top:

```rust
use std::time::Duration;

/// The debounce window: edits arriving within this of the previous edit coalesce
/// into one rebuild. Short enough to feel instant, long enough to absorb a burst of
/// keystrokes (typing ~10 chars/sec → all fold into one rebuild).
const DEBOUNCE_MS: u64 = 40;
```

Add a per-URL pending-text map to `Backend`:

```rust
pub struct Backend {
    client: Client,
    documents: Mutex<DocumentStore>,
    index: RwLock<WorkspaceIndex>,
    roots: RwLock<Vec<PathBuf>>,
    /// Coalescing: the latest pending text + its edit sequence number per URL. A
    /// debounced rebuild only proceeds if its captured sequence is still the latest.
    pending: Mutex<HashMap<Url, (String, Option<i32>, u64)>>,
    /// Monotone edit sequence, bumped on every `did_change`.
    edit_seq: std::sync::atomic::AtomicU64,
}
```

Initialize the two new fields in `Backend::new` (`pending: Mutex::new(HashMap::new())`, `edit_seq: AtomicU64::new(0)`).

- [ ] **Step 2: Rewrite `did_change` to coalesce.**

```rust
async fn did_change(&self, params: DidChangeTextDocumentParams) {
    let uri = params.text_document.uri.clone();
    let version = Some(params.text_document.version);

    // Fold the ranged edits onto the latest known text (pending wins over the cached
    // model so consecutive bursts stack correctly).
    let base = {
        let pending = self.pending.lock().await;
        if let Some((t, _, _)) = pending.get(&uri) {
            t.clone()
        } else {
            let store = self.documents.lock().await;
            store.get(&uri).map(|m| m.text.clone()).unwrap_or_default()
        }
    };
    let new_text = crate::lsp::model::apply_changes(&base, &params.content_changes);

    // Stamp this edit and record it as the latest pending text for the URI.
    let seq = self
        .edit_seq
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;
    {
        let mut pending = self.pending.lock().await;
        pending.insert(uri.clone(), (new_text.clone(), version, seq));
    }

    // Debounce: wait the window, then rebuild ONLY if no newer edit landed.
    tokio::time::sleep(Duration::from_millis(DEBOUNCE_MS)).await;
    let still_latest = {
        let pending = self.pending.lock().await;
        pending.get(&uri).map(|(_, _, s)| *s) == Some(seq)
    };
    if !still_latest {
        return; // a newer edit superseded us; it will rebuild.
    }

    let (text, ver) = {
        let mut pending = self.pending.lock().await;
        match pending.remove(&uri) {
            Some((t, v, _)) => (t, v),
            None => return,
        }
    };
    self.reindex_uri(&uri, &text);
    self.analyze_and_publish(uri, text, ver).await;
}
```

> Note: tokio's `current_thread` runtime with a `LocalSet` still drives `tokio::time::sleep`; tower-lsp dispatches each notification as its own task, so the sleep does not block other requests — it just defers this URI's rebuild. The supersession check makes the debounce correct even though edits race.

- [ ] **Step 3: Add a coalescing unit test (logic-level).**

Because the debounce is timing-coupled, add a deterministic test of the *coalescing invariant* via `apply_changes` folding (the wire-level burst is covered by Task 9). Append to `src/lsp/model.rs` sync tests:

```rust
#[test]
fn coalesced_edits_fold_forward() {
    // Two ranged inserts applied in sequence equal one apply over the folded text —
    // proving a debounce that only analyzes the LATEST folded text is correct.
    use tower_lsp::lsp_types::{Position, Range};
    let start = "let x = 1\n";
    let e1 = TextDocumentContentChangeEvent {
        range: Some(Range::new(Position::new(0, 8), Position::new(0, 9))),
        range_length: None,
        text: "2".to_string(),
    };
    let after1 = apply_changes(start, &[e1]);
    let e2 = TextDocumentContentChangeEvent {
        range: Some(Range::new(Position::new(0, 8), Position::new(0, 9))),
        range_length: None,
        text: "3".to_string(),
    };
    let after2 = apply_changes(&after1, &[e2]);
    assert_eq!(after2, "let x = 3\n");
}
```

- [ ] **Step 4: Run + commit.**

Run: `cargo test --lib lsp && cargo test --test lsp`
Expected: PASS (the existing protocol smoke test still gets exactly one diagnostics notification per logical edit; the debounce window is well under the test's 30s deadline).

```bash
git add src/lsp/server.rs src/lsp/model.rs
git commit -m "feat(lsp): debounce/coalesce rapid did_change edits into one rebuild"
```

---

## Task 5: Request supersession (drop obsolete results)

A completion/hover request scheduled before newer keystrokes must not return results computed against stale text. Each handler captures the document generation it ran against; if a newer model exists by the time it finishes, it returns nothing (the client will re-request against the fresh document).

**Files:**
- Modify: `src/lsp/server.rs` (a `superseded` guard helper + applied in `completion` and `hover`)
- Test: inline in `src/lsp/server.rs`

- [ ] **Step 1: Add the guard helper.**

```rust
impl Backend {
    /// True if `gen` is no longer the current generation for `uri` (a newer edit has
    /// landed). A handler that captured `gen` at entry should bail when this is true.
    async fn superseded(&self, uri: &Url, gen: u64) -> bool {
        let store = self.documents.lock().await;
        store.current_gen(uri).map(|g| g != gen).unwrap_or(true)
    }
}
```

- [ ] **Step 2: Apply it in the slowest-to-stale handlers.**

In `completion` and `hover`, capture the generation under the lock, compute off the model text, then re-check before returning. For `completion`:

```rust
async fn completion(
    &self,
    params: CompletionParams,
) -> tower_lsp::jsonrpc::Result<Option<CompletionResponse>> {
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let (gen, items) = {
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let gen = model.generation;
        let offset = crate::lsp::providers::docs::byte_offset_at(model, position);
        (gen, crate::lsp::providers::completion::completions(model, offset))
    };
    // If a newer edit has landed while we computed, the result is obsolete.
    if self.superseded(&uri, gen).await {
        return Ok(None);
    }
    Ok(Some(CompletionResponse::Array(items)))
}
```

Apply the identical capture-then-recheck pattern in `hover`. (These two are the noisiest while-typing requests; navigation/symbol requests are user-initiated and rarely race, so leaving them un-guarded is fine — note this in a code comment.)

- [ ] **Step 3: Write a supersession unit test.**

```rust
#[cfg(test)]
mod supersession_tests {
    use super::*;
    use crate::lsp::model::DocumentStore;
    use tower_lsp::lsp_types::Url;

    #[test]
    fn stale_generation_is_superseded() {
        // Drive the store directly (no live Client) to prove the guard logic.
        let mut store = DocumentStore::new();
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        let g1 = store.set_versioned(uri.clone(), "let x = 1\n".to_string(), Some(1));
        let g2 = store.set_versioned(uri.clone(), "let x = 2\n".to_string(), Some(2));
        assert_ne!(g1, g2);
        assert_eq!(store.current_gen(&uri), Some(g2));
        // A handler holding g1 must treat itself as superseded.
        assert!(store.current_gen(&uri) != Some(g1));
    }
}
```

- [ ] **Step 4: Run + commit.**

Run: `cargo test --lib lsp && cargo test --test lsp`
Expected: PASS.

```bash
git add src/lsp/server.rs
git commit -m "feat(lsp): supersede stale completion/hover results via document generation"
```

---

## Task 6: Large-file provider degradation (range-only / skipped, never a hang)

Above `LARGE_FILE_BYTES`, semantic-tokens FULL degrades to range-only and inlay hints are skipped; above `HUGE_FILE_BYTES`, all token/inlay/folding/color providers return empty. Each degradation logs a one-time note so it is observable, never silent.

> This task assumes the Phase 2/4 providers (`semantic_tokens`, `inlay`, `folding`, `color`) exist. If a provider named here was not yet implemented in earlier phases, gate only the ones that exist and leave a `// TODO(phase7): gate <provider> when it lands` for the rest — do NOT invent a provider.

**Files:**
- Modify: `src/lsp/server.rs` (the relevant provider handlers consult `model.size_class()`)
- Test: inline in `src/lsp/server.rs` + a wire test in Task 9.

- [ ] **Step 1: Gate semantic tokens.**

In the `semantic_tokens_full` handler, branch on size class:

```rust
let model = /* store.get(&uri) … */;
match model.size_class() {
    crate::lsp::model::SizeClass::Normal => {
        // full tokens
    }
    crate::lsp::model::SizeClass::Large => {
        self.client
            .log_message(
                MessageType::INFO,
                format!("ascript: {uri} is large — semantic tokens served range-only"),
            )
            .await;
        return Ok(None); // client falls back to ranged requests
    }
    crate::lsp::model::SizeClass::Huge => {
        self.client
            .log_message(
                MessageType::INFO,
                format!("ascript: {uri} is huge — semantic tokens disabled"),
            )
            .await;
        return Ok(None);
    }
}
```

Returning `Ok(None)` for `semanticTokens/full` makes a capable client issue `semanticTokens/range` requests instead — those stay served (they are inherently bounded). Confirm the range handler does NOT degrade.

- [ ] **Step 2: Gate inlay + folding + color.**

In `inlay_hint`, `folding_range`, and `document_color` (each, if present), short-circuit to an empty result for `Large`/`Huge` (inlay) and `Huge` (folding/color), with a logged note for the huge case:

```rust
if matches!(model.size_class(), SizeClass::Large | SizeClass::Huge) {
    return Ok(Some(Vec::new())); // inlay: skip on large+
}
```

- [ ] **Step 3: Unit-test the classification decision.**

```rust
#[test]
fn size_class_gates_expensive_providers() {
    use crate::lsp::model::{SemanticModel, SizeClass};
    use crate::lsp::perf::LARGE_FILE_BYTES;
    let big = SemanticModel::build("a".repeat(LARGE_FILE_BYTES), None, &Default::default());
    assert!(matches!(big.size_class(), SizeClass::Large | SizeClass::Huge));
}
```

(If `LintConfig` does not implement `Default` at the call site, use `&crate::check::LintConfig::default()`.)

- [ ] **Step 4: Run + commit.**

Run: `cargo test --lib lsp && cargo test --test lsp`
Expected: PASS.

```bash
git add src/lsp/server.rs
git commit -m "feat(lsp): degrade semantic-tokens/inlay/folding/color on large files (logged, never a hang)"
```

---

## Task 7: Cancellable work-done progress for initial indexing

The `initialized` handler walks each root for `*.as` and indexes them. Wrap that in a work-done progress report (begin → report N/total → end) so a large workspace shows progress, and make it cancellable by checking a flag the client can flip via `window/workDoneProgress/cancel`.

> If Phase 4 already added work-done progress around indexing, this task only adds the **cancellation check** + the test; verify against `initialized` in `src/lsp/server.rs` before duplicating the begin/report/end.

**Files:**
- Modify: `src/lsp/server.rs` (`initialized` emits progress + honors cancel)
- Test: a wire assertion in Task 9 (progress notifications observed) + a unit on the cancel flag.

- [ ] **Step 1: Add a cancel flag to `Backend`.**

```rust
/// Set by `window/workDoneProgress/cancel` for the indexing token; checked between
/// files so a huge workspace can be aborted.
index_cancelled: std::sync::atomic::AtomicBool,
```

Init `AtomicBool::new(false)` in `Backend::new`. Implement `work_done_progress_cancel` to set it:

```rust
async fn work_done_progress_cancel(&self, params: WorkDoneProgressCancelParams) {
    if matches!(&params.token, NumberOrString::String(s) if s == "ascript-index") {
        self.index_cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
```

- [ ] **Step 2: Emit progress + check cancel in `initialized`.**

After discovering `files`, replace the index loop:

```rust
let token = NumberOrString::String("ascript-index".to_string());
// Ask the client to create the progress token (best-effort; ignore failure).
let _ = self
    .client
    .send_request::<request::WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
        token: token.clone(),
    })
    .await;
self.client
    .send_notification::<notification::Progress>(ProgressParams {
        token: token.clone(),
        value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(WorkDoneProgressBegin {
            title: "Indexing AScript workspace".to_string(),
            cancellable: Some(true),
            message: None,
            percentage: Some(0),
        })),
    })
    .await;

let total = files.len().max(1);
for (i, (path, text)) in files.iter().enumerate() {
    if self.index_cancelled.load(std::sync::atomic::Ordering::SeqCst) {
        break;
    }
    if let Ok(mut idx) = self.index.write() {
        idx.reindex_file(path, text);
    }
    self.client
        .send_notification::<notification::Progress>(ProgressParams {
            token: token.clone(),
            value: ProgressParamsValue::WorkDone(WorkDoneProgress::Report(
                WorkDoneProgressReport {
                    cancellable: Some(true),
                    message: Some(format!("{}/{}", i + 1, total)),
                    percentage: Some(((i + 1) * 100 / total) as u32),
                },
            )),
        })
        .await;
}

self.client
    .send_notification::<notification::Progress>(ProgressParams {
        token,
        value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd {
            message: None,
        })),
    })
    .await;
```

Add the needed `use tower_lsp::lsp_types::{request, notification};` (or fully-qualify). Advertise the capability: in `server_capabilities`, ensure `ServerCapabilities` is fine as-is (work-done progress needs no extra capability field; the client opts in by supporting `window/workDoneProgress`). Keep the final `log_message("ascript language server initialized")`.

- [ ] **Step 3: Unit-test the cancel flag.**

```rust
#[test]
fn index_cancel_flag_defaults_false() {
    // The flag is observable + sticky once set. (The wire path is in tests/lsp.rs.)
    let flag = std::sync::atomic::AtomicBool::new(false);
    assert!(!flag.load(std::sync::atomic::Ordering::SeqCst));
    flag.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(flag.load(std::sync::atomic::Ordering::SeqCst));
}
```

- [ ] **Step 4: Run + commit.**

Run: `cargo test --lib lsp && cargo test --test lsp`
Expected: PASS — the existing smoke test tolerates extra `$/progress` notifications (it reads by `method`/`id`, skipping others).

```bash
git add src/lsp/server.rs
git commit -m "feat(lsp): cancellable work-done progress for initial workspace indexing"
```

---

## Task 8: Docs — per-editor setup guide + capability reference

Two new Markdown pages under `docs/content/`, wired into the docs site NAV and linked from `README.md`. Match the existing page style (front-matter-less Markdown with `#` title, `##` sections, fenced code, callout `>` blocks — mirror `docs/content/cli.md`).

**Files:**
- Create: `docs/content/tooling/editor-setup.md`
- Create: `docs/content/tooling/lsp-capabilities.md`
- Modify: `docs/assets/app.js` (add a "Tooling" NAV section)
- Modify: `README.md` (link the two pages)

- [x] **Step 1: Write `docs/content/tooling/editor-setup.md`.**

```markdown
# Editor setup

AScript ships a first-class language server (`ascript lsp`, stdio JSON-RPC) plus a
tree-sitter grammar and a canonical formatter (`ascript fmt`). This page gets each
supported editor talking to them. For the full list of methods the server answers,
see [LSP capabilities](tooling/lsp-capabilities.md).

All three editors discover the server on your `PATH` as `ascript`. Build it once:

```bash
cargo build --release      # → target/release/ascript
# put target/release on PATH, or copy `ascript` into ~/.local/bin
```

## VS Code

Install the **AScript** extension (Marketplace / Open VSX). It launches `ascript lsp`
over stdio and contributes the `.as` language, a TextMate grammar for instant
coloring, and semantic tokens from the server.

Settings (`settings.json`):

```jsonc
{
  // Override the server binary if it is not on PATH:
  "ascript.server.path": "/absolute/path/to/ascript",
  // Trace LSP traffic when filing a bug:
  "ascript.trace.server": "verbose",
  // Broaden hex-string color swatches to all strings (default: off):
  "ascript.color.detectHexStringsEverywhere": false
}
```

Commands: **AScript: Restart Language Server**. Format on save uses
`textDocument/formatting` (the same output as `ascript fmt`).

## Zed

Install the **AScript** extension from the Zed extension registry. It registers the
tree-sitter grammar (highlights / injections / brackets / outline / indents) and the
`ascript lsp` server. Override the binary in your Zed settings:

```jsonc
{
  "lsp": {
    "ascript": { "binary": { "path": "/absolute/path/to/ascript", "arguments": ["lsp"] } }
  }
}
```

## Neovim

With `nvim-lspconfig` (0.10+):

```lua
vim.filetype.add({ extension = { as = "ascript" } })

local lspconfig = require("lspconfig")
local configs = require("lspconfig.configs")
if not configs.ascript then
  configs.ascript = {
    default_config = {
      cmd = { "ascript", "lsp" },
      filetypes = { "ascript" },
      root_dir = lspconfig.util.root_pattern("ascript.toml", ".git"),
      single_file_support = true,
    },
  }
end
lspconfig.ascript.setup({})
```

Tree-sitter highlighting via `nvim-treesitter` (register the `ascript` parser +
queries under `queries/ascript/`). Formatting: use the LSP
(`vim.lsp.buf.format()`), or a `conform.nvim` recipe pointing at `ascript fmt` as a
fallback:

```lua
require("conform").setup({
  formatters = { ascript_fmt = { command = "ascript", args = { "fmt", "$FILENAME" }, stdin = false } },
  formatters_by_ft = { ascript = { "ascript_fmt" } },
})
```

## Performance notes

The server coalesces rapid keystrokes into a single rebuild and supersedes stale
completion/hover results, so editing stays responsive. Very large files degrade
gracefully — above ~256 KiB semantic tokens are served range-only and inlay hints are
skipped; above ~2 MiB token/inlay/folding/color providers go quiet — with a note in
the LSP log. Diagnostics and navigation always run. (The front-end is a full-reparse
design; responsiveness comes from debouncing, not incremental green-node reuse.)
```

- [x] **Step 2: Write `docs/content/tooling/lsp-capabilities.md`.**

Enumerate every method the server advertises. Cross-check the real list against `server_capabilities()` in `src/lsp/server.rs` at implementation time and against the §4 capability matrix — adjust the table to exactly what is wired (do not list a method the server does not answer).

```markdown
# LSP capabilities

`ascript lsp` speaks LSP over stdio. Every method below is answered by the server;
each is powered by the cached per-document semantic model (CST + resolver + the SP10
advisory type inferencer) — the server never runs your code.

## Lifecycle & sync

| Method | Notes |
|---|---|
| `initialize` / `initialized` / `shutdown` / `exit` | Standard lifecycle. |
| `textDocument/didOpen` / `didChange` / `didClose` | **Incremental** sync; rapid edits are debounced/coalesced. |
| `textDocument/didSave` | Format-on-save fallback. |

## Diagnostics

| Method | Notes |
|---|---|
| `textDocument/publishDiagnostics` | Config-aware — honors the nearest `ascript.toml [lint]`, identical to `ascript check`. |
| `textDocument/diagnostic` (pull) | On-demand single-file diagnostics. |
| `workspace/diagnostic` (pull) | Project-wide. |

## Navigation

| Method | Notes |
|---|---|
| `textDocument/definition` | Cross-file, follows import edges. |
| `textDocument/declaration` | |
| `textDocument/typeDefinition` | Jumps to a value's class/enum. |
| `textDocument/implementation` | Subclasses / enum variants. |
| `textDocument/references` | Cross-file. |
| `textDocument/documentHighlight` | Read/write occurrences. |

## Symbols & structure

| Method | Notes |
|---|---|
| `textDocument/documentSymbol` | Nested (class → methods/fields, enum → variants). |
| `workspace/symbol` (+ resolve) | |
| `textDocument/foldingRange` | Blocks, functions, classes. |
| `textDocument/selectionRange` | Smart expand via CST ancestry. |
| `textDocument/documentLink` | Clickable import paths. |

## Hover, help & completion

| Method | Notes |
|---|---|
| `textDocument/hover` | Inferred/declared type + docs. |
| `textDocument/signatureHelp` | Active parameter while typing a call. |
| `textDocument/completion` (+ resolve) | Scope locals/params, members, fields/methods, enum variants, module exports, keywords; auto-import. |

## Editing power-tools

| Method | Notes |
|---|---|
| `textDocument/formatting` / `rangeFormatting` | Canonical (`ascript fmt`). |
| `textDocument/codeAction` (+ resolve) | Quick-fixes, `source.organizeImports`, `source.fixAll`. |
| `workspace/executeCommand` | Backs lenses / fixAll. |
| `textDocument/codeLens` (+ resolve) | Run `test(...)`/`main`, reference counts. |
| `textDocument/semanticTokens/full` / `range` | Types/params/props/enums. Large files: range-only. |
| `textDocument/inlayHint` (+ resolve) | Inferred `let`/param types; param-name hints. Skipped on large files. |
| `textDocument/rename` / `prepareRename` | Cross-file. |
| `textDocument/linkedEditingRange` | Local identifiers. |

## Hierarchy & workspace

| Method | Notes |
|---|---|
| `textDocument/prepareCallHierarchy` (+ incoming/outgoing) | |
| `textDocument/prepareTypeHierarchy` (+ super/sub) | Class + enum. |
| `workspace/didChangeWatchedFiles` | `.as` + `ascript.toml`. |
| `workspace/didChangeConfiguration` | Re-resolves lint config. |
| `workspace/willRenameFiles` / `didRenameFiles` | Rewrites imports on move. |
| `textDocument/documentColor` / `colorPresentation` | `color.*`, tui triples, hex/`rgba()`/`hsl()` strings in color-aware positions. |

## Performance & limits

- **Debounce/coalesce:** bursts of keystrokes fold into one rebuild.
- **Supersession:** a completion/hover computed against now-stale text is dropped.
- **Large-file bounds:** above ~256 KiB, semantic tokens go range-only and inlay
  hints are skipped; above ~2 MiB, token/inlay/folding/color providers go quiet.
  Diagnostics + navigation always run. Degradations are logged.
- **Indexing progress:** initial workspace indexing reports cancellable work-done
  progress.

> The AScript front-end is a full-reparse CST design; the LSP gets its responsiveness
> from debouncing and size bounds rather than incremental green-node reuse.
```

- [x] **Step 3: Wire the NAV section.**

In `docs/assets/app.js`, add a "Tooling" section to the `NAV` array (after "Standard library", before "Resources"):

```js
  { title: 'Tooling', items: [
    ['tooling/editor-setup', 'Editor setup'],
    ['tooling/lsp-capabilities', 'LSP capabilities'],
  ]},
```

- [x] **Step 4: Link from `README.md`.**

In the README's tooling/LSP area (the `ascript lsp` row of the CLI table ~line 78 and the LSP paragraph ~line 156), add a sentence linking the new pages, e.g.:

```markdown
See [editor setup](docs/content/tooling/editor-setup.md) for VS Code, Zed, and Neovim,
and the [LSP capability reference](docs/content/tooling/lsp-capabilities.md) for every
method the server answers.
```

- [x] **Step 5: Verify the docs render (served, not `file://`).**

```bash
cd docs && python3 -m http.server 8000 &
# open http://localhost:8000/reader.html#tooling/editor-setup and #tooling/lsp-capabilities
# confirm both pages load and appear in the sidebar + cmd-K search; then stop the server.
```

- [x] **Step 6: Commit.**

```bash
git add docs/content/tooling docs/assets/app.js README.md
git commit -m "docs(lsp): per-editor setup guide + capability reference; link from README"
```

---

## Task 9: Full-capability protocol smoke test + consistency invariant

Extend `tests/lsp.rs` to (a) assert the **full** advertised capability set on `initialize`, (b) exercise one representative request per provider end-to-end, (c) prove a large file does not hang, and (d) assert LSP diagnostics ≡ `ascript check` for the same config.

**Files:**
- Modify: `tests/lsp.rs`
- Test: the file itself.

- [x] **Step 1: Assert the full capability set.**

In `lsp_protocol_end_to_end`, after reading the `initialize` response, extend the capability assertions to cover every provider the server now advertises. Cross-check the field names against `server_capabilities()` at write time and assert each present one is non-null:

```rust
for cap in [
    "textDocumentSync", "completionProvider", "hoverProvider", "definitionProvider",
    "documentSymbolProvider", "referencesProvider", "renameProvider",
    "workspaceSymbolProvider", "documentHighlightProvider", "foldingRangeProvider",
    "selectionRangeProvider", "documentLinkProvider", "signatureHelpProvider",
    "semanticTokensProvider", "inlayHintProvider", "codeActionProvider",
    "codeLensProvider", "documentFormattingProvider", "colorProvider",
    "declarationProvider", "typeDefinitionProvider", "implementationProvider",
    "callHierarchyProvider", "typeHierarchyProvider",
] {
    assert!(!caps[cap].is_null(), "missing advertised capability `{cap}`: {resp}");
}
```

> If a capability above was NOT implemented by an earlier phase, remove it from this list to match reality — but the list MUST equal the set in `server_capabilities()`. Add a code comment pointing back to that function as the source of truth.

- [x] **Step 2: Exercise one representative request per provider.**

Add a new test `lsp_full_capability_surface` that opens a content-rich document and fires one request per provider (folding, selectionRange, documentLink, signatureHelp, semanticTokens/full, inlayHint, codeAction, documentHighlight, formatting, documentColor, callHierarchy/prepare, typeHierarchy/prepare). For each, assert the response carries a `result` member (it may be null/empty, but must be well-formed). Reuse the `LspClient` driver. Example for two of them:

```rust
client.request(10, "textDocument/foldingRange",
    json!({ "textDocument": { "uri": uri } }));
let r = client.read_response(10, overall);
assert!(r.get("result").is_some(), "foldingRange malformed: {r}");

client.request(11, "textDocument/formatting",
    json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 2, "insertSpaces": true } }));
let r = client.read_response(11, overall);
assert!(r.get("result").is_some(), "formatting malformed: {r}");
```

- [x] **Step 3: Large-file no-hang test.**

Add `lsp_large_file_does_not_hang`: open a ~300 KiB document (built from repeated `fn` units), then request semanticTokens/full and inlayHint, asserting each returns within the overall 30s deadline (the `read_response` deadline already enforces this) and that the responses are well-formed (`result` present — `null`/empty is the expected degraded answer):

```rust
let mut big = String::new();
while big.len() < 300 * 1024 {
    big.push_str("fn f(a, b) { let s = a + b\n  return s }\n");
}
client.notify("textDocument/didOpen", json!({ "textDocument":
    { "uri": "ascript-test://big.as", "languageId": "ascript", "version": 1, "text": big }}));
let _ = client.read_notification("textDocument/publishDiagnostics", overall);
client.request(20, "textDocument/semanticTokens/full",
    json!({ "textDocument": { "uri": "ascript-test://big.as" } }));
let r = client.read_response(20, overall);
assert!(r.get("result").is_some(), "large-file semantic tokens hung or malformed: {r}");
```

- [x] **Step 4: Diagnostics ≡ `ascript check` consistency invariant.**

Add `lsp_diagnostics_match_ascript_check`: in a temp dir with an `ascript.toml [lint]` and a `.as` file that trips a configurable lint, (1) capture the LSP `publishDiagnostics` codes for that file on `didOpen`, (2) run the `ascript check` binary on the same file in the same dir and parse its emitted diagnostic codes, (3) assert the two code sets are equal. Use `env!("CARGO_BIN_EXE_ascript")` for the `check` subprocess. Sketch:

```rust
let dir = std::env::temp_dir().join(format!("ascript_lsp_consistency_{}", std::process::id()));
std::fs::create_dir_all(&dir).unwrap();
std::fs::write(dir.join("ascript.toml"), "[lint]\nunused-binding = \"warn\"\n").unwrap();
let f = dir.join("m.as");
std::fs::write(&f, "fn main() {\n  let unused = 1\n}\n").unwrap();
let uri = format!("file://{}", f.display());
let root = format!("file://{}", dir.display());

// (a) LSP diagnostics for the file.
//   initialize with rootUri=root, initialized, didOpen the file, read publishDiagnostics,
//   collect the set of `code` strings.
// (b) `ascript check` on the same file:
let out = std::process::Command::new(env!("CARGO_BIN_EXE_ascript"))
    .arg("check").arg(&f).current_dir(&dir).output().unwrap();
// parse the diagnostic codes from stdout/stderr (the check output format prints the code).
// (c) assert the two code sets are equal.
```

Confirm the `ascript check` CLI output includes machine-greppable codes (it does — diagnostics carry a `code`; if the human format is hard to parse, add `--format json` IF the CLI supports it, else grep the code tokens). If exact-set equality is too brittle against ordering, assert set-equality of the codes (not the order).

- [x] **Step 5: Run the protocol suite.**

Run: `cargo test --test lsp`
Expected: PASS (all LSP wire tests, old + new).

- [x] **Step 6: Commit.**

```bash
git add tests/lsp.rs
git commit -m "test(lsp): full-capability smoke + large-file no-hang + diagnostics≡check invariant"
```

---

## Task 10: Final gate + campaign close-out

**Files:**
- Modify: `README.md` (one line: note the responsiveness/large-file behavior near the LSP paragraph, if not already added in Task 8)
- Verify: the whole suite + both clippy configs.

- [x] **Step 1: Run the full gate.**

```bash
cargo test
cargo test --no-default-features
cargo clippy --all-targets
cargo clippy --no-default-features --all-targets
```

Expected: all green/clean. Under `--no-default-features` the `lsp` feature is OFF, so `tests/lsp.rs` compiles out (it is `#![cfg(feature = "lsp")]`) and `src/lsp/*` is gated — confirm the new `perf.rs`/`model.rs` code is inside the existing `lsp` cfg boundary so the no-default build does not try to compile it.

- [x] **Step 2: Confirm the perf budget is met.**

```bash
cargo test --lib lsp::perf::budget_tests -- --nocapture
```

Expected: `large_file_rebuild_is_under_budget` PASS, well under `REBUILD_BUDGET_MS`.

- [x] **Step 3: Confirm the docs publish.**

```bash
cd docs && python3 -m http.server 8000
# verify #tooling/editor-setup and #tooling/lsp-capabilities load + appear in sidebar/search.
```

- [x] **Step 4: Commit any final doc/README tweak.**

```bash
git add README.md
git commit -m "docs: note LSP responsiveness + large-file bounds near the tooling section"
```

---

## Phase 7 Done — Gate

- [x] **Full-capability smoke green:** `cargo test --test lsp` passes, asserting the entire advertised capability set on `initialize` and exercising one representative request per provider end-to-end, with no hang on a ~300 KiB file.
- [x] **Perf budget met:** `large_file_rebuild_is_under_budget` passes under `REBUILD_BUDGET_MS`; rapid edits coalesce into one rebuild; stale completion/hover results are superseded and dropped.
- [x] **Large-file bounds live:** above `LARGE_FILE_BYTES` semantic tokens are range-only and inlay is skipped; above `HUGE_FILE_BYTES` token/inlay/folding/color go quiet — each logged, diagnostics + navigation always running.
- [x] **Indexing progress:** `initialized` emits cancellable work-done progress; `window/workDoneProgress/cancel` aborts it.
- [x] **Consistency invariant:** LSP diagnostics ≡ `ascript check` codes for the same `ascript.toml` config (`lsp_diagnostics_match_ascript_check`).
- [x] **Docs published:** `docs/content/tooling/editor-setup.md` + `docs/content/tooling/lsp-capabilities.md` exist, are in the site NAV + search, render when served, and are linked from `README.md`.
- [x] **Gates clean:** `cargo test`, `cargo test --no-default-features`, `cargo clippy --all-targets`, and `cargo clippy --no-default-features --all-targets` all pass.
- [x] **Invariants preserved:** the LSP stays `Send + Sync`, holds no `Rc`/`RefCell`/`Value`, and imports no `crate::{ast,lexer,parser,token}` (the Phase 0 guard test still passes).

**This completes the first-class-LSP campaign.** Phases 0–4 unified the LSP onto one cached `SemanticModel` and built out the full modern + advanced capability surface; Phase 5 promoted the tree-sitter grammar; Phase 6 shipped the VS Code, Zed, and Neovim integrations; and Phase 7 made the server responsive under real editing pressure, robust on large files, cancellation-aware, and fully documented. The AScript language server is now first-class end-to-end.
```