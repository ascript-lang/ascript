# wasm32 Target + Browser Playground (WASM) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. At the end of each
> phase, a **holistic per-phase review subagent** reviews the phase's combined changes before
> the next phase starts. A task/phase is closed only when every box under it is ticked.

**Goal:** Phase 0 — a **feasibility spike with a recorded GO/NO-GO** (the gate IS the
deliverable until it says otherwise): a fixed build-matrix over `wasm32-unknown-unknown`
(dependency inventory, the tokio-on-wasm executor probe, the tree-sitter linkage skip,
gcmodule runtime smoke, size/load), every cell's raw output committed verbatim into the
spec's §10 evidence appendix and `goal-perf.md` updated **either way** — a NO-GO is an
honored outcome. On GO only: ship v1 — an in-browser playground on the existing docs site
that compiles (CST → resolver → bytecode compiler) and runs (async VM) AScript entirely
client-side via a thin `wasm-bindgen` wrapper crate `ascript-wasm/` exporting
`run_program(source) → Promise<RunResult>`, output captured (`OutputSink::Capture`), caps
denied-all, the curated pure-compute stdlib subset (`data,binary,log,shared` + spike-gated
`crypto`/`datetime` candidates), workers/intervals as clean Tier-2 platform errors, a
mini-differential CI smoke byte-comparing wasm output vs native over the example corpus —
with the native build's dependency graph, tests, and four-mode behavior **byte-identical**
(Gate W-1). No grammar change, no opcode, no `.aso` bump.

**Spec:** `superpowers/specs/2026-06-12-wasm-target-design.md` (WASM). **Read it first and
in full** — §0 (the spike posture), §2 (verified ground truth — re-grep every cited symbol
before editing), §4 (the spike matrix + GO/NO-GO rules), §5 (every v1 design decision this
plan implements), §6 (the failure-mode table — every row becomes a test), §7.3 (gates
W-1…W-4). Section references (§) below are into it.

**Before writing any code, read these files end to end** (line numbers verified 2026-06-12;
names are the anchors — re-grep before editing):
- `build.rs` (lines 1–11: the unconditional `cc` step to be TARGET-gated)
- `Cargo.toml` (the dep tables to split; the feature graph that does the stdlib pruning)
- `src/lib.rs` (module cfg's; `run_source_exit` ~`:637` — the entry the wrapper wraps)
- `src/interp.rs` (`OutputSink` `:375`, capture default `:975`; `MAX_CALL_DEPTH` `:714`;
  `ResourceState::Interval` `:272`)
- `src/vm/stack.rs` (the whole file — `grow`/`grow_future` + the RED_ZONE doc)
- `src/stdlib/time.rs` (`:8,29,44`), `src/stdlib/math.rs` (`:501-536`),
  `src/stdlib/task_mod.rs` (`:186,308`), `src/stdlib/time_timers.rs`
- `src/det.rs` (`VirtualClock`/`SeededRng` — the seams that sit ABOVE the new platform shim)
- `src/stdlib/caps.rs` + `src/stdlib/mod.rs:325` (`required_cap`)
- `src/worker/isolate.rs` (`spawn_isolate` `:211`, the `std::thread::Builder` at `:273`),
  `src/worker/mod.rs` (`dispatch_worker` `:87`, `dispatch_worker_dedicated` `:342`)
- `docs/assets/app.js` (NAV `:11`, renderMarkdown conventions), `docs/reader.html`
  (topbar `:13-24`), `docs/index.html` (topnav `:15-18`), `.github/workflows/ci.yml`

**Architecture:** Phase 0 (the spike — on a kept-but-never-merged `spike/wasm-target`
branch; probe patches allowed there ONLY): cells (a)–(e) → spec §10.1–10.5, decision →
§10.6 + `goal-perf.md`. **Everything after Phase 0 executes only on a recorded GO.**
Phase 1 (main-crate portability, TDD, native-byte-identical): build.rs skip, target-dep
tables, `src/platform.rs` clock/entropy/sleep seam, recursion ceiling, the worker/interval
Tier-2 guards, the `wasm_run_source` lib entry, `tests/wasm_negative_space.rs`. Phase 2
(the wrapper crate): `ascript-wasm/` + `run_program` + wasm-bindgen-tests under Node.
Phase 3 (playground UI + docs): `docs/playground.html`, the Web-Worker driver JS,
`tooling/playground.md` + NAV, links, stretch Run▶ buttons. Phase 4 (CI + finish):
the `wasm` CI job, the mini-differential smoke, size report, CLAUDE/roadmap/goal-perf,
holistic review + gates checklist.

**Tech stack:** Rust (stable) + `rustup target add wasm32-unknown-unknown`; `wasm-pack`
(build + `--node` test runner), `wasm-opt` (binaryen), Node ≥ 18 for the smoke; the `!Send`
current-thread runtime model unchanged (tokio `LocalSet::run_until` driven by
`wasm-bindgen-futures` — spec §2.3/§5.3.6; never `block_on` on wasm); vanilla JS in
`docs/assets/` (no framework, no bundler — `app.js` conventions).

**Hard rules carried from the spec:**
- **Gate W-1 (native invariance):** every main-crate edit keeps native `cargo tree` (both
  configs), `cargo test` (both configs), clippy (both configs), and the differential corpus
  **unchanged**. Each Phase-1 task diffs `cargo tree` before/after. The non-wasm rows of
  every target-dep table are textually today's deps.
- **No silent gaps (W-3):** every wasm platform gap is the existing unknown-module error,
  the existing cap-denied error, or a Tier-2
  `"<thing> is not available on this platform (wasm)"` — each asserted by a test. Never a
  hang, never a stub.
- **No `.aso` bump, no grammar change, no new `Op`** — pinned by
  `tests/wasm_negative_space.rs` (read the current `ASO_FORMAT_VERSION` from
  `src/vm/aso.rs` when writing the pin; do not hardcode from this plan).
- The det seams (`src/det.rs`) are NOT modified — the platform shim slots BELOW them
  (§5.3.3); det unit tests must pass untouched.
- Probe patches live on the spike branch only; the GO implementation **re-does them
  failing-test-first** on `feat/wasm-target`.

**Binding execution standards (production-grade mandate):** any bug found while working —
ours or pre-existing, direct or incidental — is fixed in-branch with a failing-test-first
regression guard, never stepped around (goal.md Gate 14). No placeholders, no silent
deferrals — the ONLY sanctioned fill-in slots are spec §10.1–10.6 (the spike evidence).
Branches: `spike/wasm-target` off `main` (Phase 0, kept unmerged), then
`feat/wasm-target` off `main` (Phases 1–4). Commit per task with the house trailer:
`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

## File structure

**New files (GO path):**
- `src/platform.rs` — clock/entropy/sleep seam (native arms byte-identical to today's
  inline code; wasm arms via `js-sys`/`getrandom`).
- `tests/wasm_negative_space.rs` — ASO pin, no-`Op` pin, no-wasm-cfg-in-serializer pin,
  `wasm_run_source` cap-deny pins (native-runnable).
- `ascript-wasm/` — `Cargo.toml` (own empty `[workspace]`), `.cargo/config.toml`
  (`-zstack-size`), `src/lib.rs`, `tests/node_smoke.rs` (wasm-bindgen-test).
- `scripts/build-wasm.sh`, `scripts/wasm_smoke.mjs` (+ its committed `EXAMPLES_WASM` list).
- `docs/playground.html`, `docs/assets/playground.js`, `docs/assets/playground-worker.js`,
  `docs/assets/playground/pkg/` (committed wasm-pack output, CI-freshness-checked).
- `docs/content/tooling/playground.md`, `bench/WASM_SIZE.md`.

**Modified files (GO path):**
- `build.rs` (TARGET skip), `Cargo.toml` (target-dep tables), `src/lib.rs` (repl cfg +
  `wasm_run_source`), `src/interp.rs` (wasm `MAX_CALL_DEPTH` + interval guard),
  `src/vm/stack.rs` (wasm pass-throughs, iff spike says so), `src/stdlib/{time,math}.rs` +
  `src/stdlib/{task_mod,time_timers}.rs` (route through `platform`),
  `src/worker/isolate.rs` (`spawn_isolate` guard), `.github/workflows/ci.yml` (wasm job),
  `docs/assets/app.js` (NAV + stretch Run▶), `docs/index.html`, `docs/reader.html`,
  `docs/content/examples.md`, `README.md`, `CLAUDE.md`, `superpowers/roadmap.md`,
  `goal-perf.md`, spec §10 (evidence).

---

## Phase 0 — THE SPIKE (the gate; spec §4)

> Branch: `git checkout -b spike/wasm-target main`. Throwaway-quality probe patches are
> sanctioned HERE ONLY. Every task ends by pasting raw command output into the matching
> spec §10 slot and committing. If any GO-critical cell fails, jump to Task 0.7 and record
> the NO-GO — do not grind on unblocking beyond the sketch-and-cost rule (§4.6).

### Task 0.1: toolchain + branch

- [ ] **Step 1:** `git checkout -b spike/wasm-target main`.
- [ ] **Step 2:** `rustup target add wasm32-unknown-unknown` ·
  `cargo install wasm-pack --locked || true` · `wasm-pack --version` ·
  `node --version` (≥18) · `wasm-opt --version` (install binaryen via
  `brew install binaryen` if absent). Record versions in the branch notes.
- [ ] **Step 3:** Baseline sanity: `cargo build --release` and
  `cargo test --no-default-features` green on native before any probe patch.
- [ ] Commit — `chore(wasm): spike branch + toolchain baseline`.

### Task 0.2: spike (a) — dependency build inventory → spec §10.1

- [ ] **Step 1:** `cargo check --target wasm32-unknown-unknown --no-default-features 2>&1 | tee /tmp/wasm-a1.log`.
  Expected first failure: the `cc` step on `tree-sitter-ascript/src/parser.c` (no wasm
  sysroot). Record verbatim.
- [ ] **Step 2 (probe patch 1):** TARGET-gate the cc step in `build.rs`:

```rust
fn main() {
    let dir = "tree-sitter-ascript/src";
    // Keep rerun-if-changed unconditional so dep-tracking is target-stable.
    println!("cargo:rerun-if-changed={}/parser.c", dir);
    // WASM §5.3.1: the compiled C parser's ONLY consumer is the native dev test
    // tests/treesitter_conformance.rs (zero src/ references — spec §2.1), and
    // wasm32-unknown-unknown has no C sysroot. Skip the cc step for wasm targets.
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.starts_with("wasm32") {
        cc::Build::new()
            .include(dir)
            .file(format!("{}/parser.c", dir))
            .warnings(false)
            .compile("tree_sitter_ascript");
    }
    generate_ast_nodes();
}
```

- [ ] **Step 3:** Re-run the Step-1 command; iterate, recording EVERY failing dep in order
  with its error head (expected: tokio `rt-multi-thread`; `rustyline`; possibly `stacker`/
  `psm`; possibly `std::time` misuse surfaces only at runtime — note compile-time cells
  only here). For each, apply the minimal probe patch from spec §5.3.2 (target-dep table
  split; `#[cfg(not(target_family = "wasm"))] pub mod repl;`) and re-run until
  `--no-default-features` checks clean OR a cell is genuinely unbuildable.
- [ ] **Step 4 (curated set):**
  `cargo check --target wasm32-unknown-unknown --no-default-features --features data,binary,log,shared 2>&1 | tee /tmp/wasm-a2.log`.
  Expected blocker: `getrandom` wanting the `js` backend (via `uuid`). Probe patch:
  the wasm target-dep rows `getrandom = { version = "0.2", features = ["js"] }` and the
  wasm `uuid` row with `"js"` (spec §5.3.2). Re-run to clean or record the blocker.
- [ ] **Step 5 (candidate cells, individually):**
  `cargo check --target wasm32-unknown-unknown --no-default-features --features data,binary,log,shared,crypto 2>&1 | tee /tmp/wasm-a3.log`
  and the same with `,datetime` (`/tmp/wasm-a4.log`). Pass/fail each — these decide whether
  `crypto`/`datetime` join the §5.2 subset; **they are not GO criteria**. For `datetime`,
  if chrono fails wanting a clock, retry with a probe wasm-row
  `chrono = { ..., features = ["wasmbind"] }` and record both results.
- [ ] **Step 6:** Paste the ordered inventory (dep → error → probe patch → result) plus the
  final clean-check transcript into **spec §10.1**. Commit —
  `spike(wasm): (a) dependency inventory + probe patches — spec §10.1`.
- [ ] Independent reviewer: re-runs Steps 1, 4, 5 from the committed branch; confirms
  native `cargo test` AND `cargo test --no-default-features` still green WITH the probe
  patches applied (the target tables must not perturb native resolution — diff
  `cargo tree > /tmp/tree-after.txt` against a fresh `main` checkout's `cargo tree`;
  byte-equal required); confirms §10.1 matches the logs.

### Task 0.3: spike (b) — tokio-on-wasm executor probe → spec §10.2

**Files:** `ascript-wasm/` skeleton (Cargo.toml from spec §5.1 + the test below). The
skeleton here is the spike's vehicle; Phase 2 rebuilds it properly.

- [ ] **Step 1:** Create `ascript-wasm/` with the spec §5.1 `Cargo.toml` (own empty
  `[workspace]`; add `console_error_panic_hook = "0.1"` to deps and
  `wasm-bindgen-test = "0.3"` to dev-deps) and `.cargo/config.toml`:

```toml
[target.wasm32-unknown-unknown]
rustflags = ["-C", "link-arg=-zstack-size=8388608"]
```

- [ ] **Step 2:** Write the executor probe as `#[wasm_bindgen_test]`s in
  `ascript-wasm/tests/node_smoke.rs` — NO ascript yet, pure tokio-machinery cells:

```rust
//! WASM spike (b) — spec §4.2: tokio's LocalSet machinery driven by the
//! wasm-bindgen-futures executor (no Runtime::block_on ever).
use wasm_bindgen_test::*;

#[wasm_bindgen_test]
async fn spawn_local_joinhandle_resolves_under_run_until() {
    let local = tokio::task::LocalSet::new();
    let out = local
        .run_until(async {
            let jh = tokio::task::spawn_local(async { 21 * 2 });
            jh.await.unwrap()
        })
        .await;
    assert_eq!(out, 42);
}

#[wasm_bindgen_test]
async fn abort_cancels_spawned_task() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
            let jh = tokio::task::spawn_local(async move {
                let _tx = tx; // dropped only if the task is dropped/aborted
                std::future::pending::<()>().await;
            });
            jh.abort();
            // The abort must drop the task body, closing the channel.
            assert!(rx.try_recv().is_err());
            tokio::task::yield_now().await;
            assert!(matches!(
                rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
            ));
        })
        .await;
}

#[wasm_bindgen_test]
async fn mpsc_and_yield_work() {
    let local = tokio::task::LocalSet::new();
    let got = local
        .run_until(async {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::task::spawn_local(async move {
                for i in 0..3 {
                    tx.send(i).unwrap();
                    tokio::task::yield_now().await;
                }
            });
            let mut v = vec![];
            while let Some(i) = rx.recv().await {
                v.push(i);
                if v.len() == 3 { break; }
            }
            v
        })
        .await;
    assert_eq!(got, vec![0, 1, 2]);
}
```

- [ ] **Step 3:** `cd ascript-wasm && wasm-pack test --node 2>&1 | tee /tmp/wasm-b1.log`.
  **If a test panics wanting a runtime context** ("must be called from the context of a
  Tokio runtime"): retry each test body wrapped as

```rust
let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
let _guard = rt.enter(); // never block_on — single-threaded wasm makes holding this benign
let local = tokio::task::LocalSet::new();
/* ... as before ... */
```

  and record WHICH configuration passes (`/tmp/wasm-b2.log`). The passing configuration is
  the **normative wrapper shape** for Phase 2 (spec §5.3.6).
- [ ] **Step 4 — outcomes:** **GO cell** = all three tests pass in one of the two
  configurations → paste transcripts + the chosen configuration into **spec §10.2**.
  **NO-GO cell** = neither configuration runs `spawn_local` to completion → paste the
  failure, write the costed fallback sketch required by §4.2 (a cfg'd `crate::rt` facade
  with hand-rolled abortable local spawn; enumerate the 20+ `tokio::task::spawn_local`
  call sites from `grep -rn "spawn_local" src/` as the migration surface) into §10.2,
  and proceed to Task 0.7 — this cell alone may flip the overall decision.
- [ ] Commit — `spike(wasm): (b) executor probe — spec §10.2`.
- [ ] Independent reviewer: re-runs `wasm-pack test --node`; verifies the abort test
  genuinely exercises cancel-on-drop (temporarily remove `jh.abort()` → the test must
  fail — anti-false-green); confirms §10.2 records the configuration choice.

### Task 0.4: spike (c) — tree-sitter linkage proof → spec §10.3

- [ ] **Step 1:** Commit the static evidence:
  `grep -rn "tree_sitter_ascript" src/ tests/ | tee /tmp/wasm-c1.log` — expected: ONLY
  `tests/treesitter_conformance.rs` hits. If a `src/` hit appears, STOP — spec §2.1's
  ground truth moved; re-design the skip before proceeding.
- [ ] **Step 2:** Prove the native suite is untouched by probe patch 1:
  `cargo test --test treesitter_conformance 2>&1 | tee /tmp/wasm-c2.log` (green) and
  `cargo test --test frontend_conformance` (green).
- [ ] **Step 3:** Paste both into **spec §10.3**. Commit —
  `spike(wasm): (c) tree-sitter skip evidence — spec §10.3`.
- [ ] Independent reviewer: confirms the grep is over the full `src/` tree (not a subdir)
  and the conformance tests ran against the patched build.rs (check the build hash/log).

### Task 0.5: spike (d) — gcmodule runtime smoke + stacker/getrandom cells → spec §10.4

- [ ] **Step 1:** stacker cell: from Task 0.2's inventory, record whether `stacker`/`psm`
  **compiled** for wasm32. If it failed, the probe patch moves `stacker` to the
  `cfg(not(target_family = "wasm"))` table and cfg's `src/vm/stack.rs`:

```rust
#[cfg(target_family = "wasm")]
#[inline]
pub fn grow<R>(f: impl FnOnce() -> R) -> R {
    // WASM §5.3.5: no segment growth on wasm — the linker-set shadow stack plus the
    // wasm-calibrated MAX_CALL_DEPTH (interp.rs) are the recursion bound.
    f()
}

#[cfg(target_family = "wasm")]
pub fn grow_future<'a, F, O>(fut: F) -> std::pin::Pin<Box<dyn std::future::Future<Output = O> + 'a>>
where
    F: std::future::Future<Output = O> + 'a,
{
    Box::pin(fut)
}
```

  (native bodies gain the mirroring `#[cfg(not(target_family = "wasm"))]`). Record the
  outcome either way.
- [ ] **Step 2:** gcmodule **runtime** smoke (compiling ≠ collecting — spec §4.4). Add to
  the spike wrapper a temporary export calling `ascript::run_source(...)` on a cyclic-graph
  program, plus a `#[wasm_bindgen_test]`:

```rust
#[wasm_bindgen_test]
async fn gc_collects_cycles_on_wasm() {
    // A cycle-building loop: without the cycle collector this is unbounded growth;
    // the assertion is completion + correct output under Node.
    let src = r#"
let keep = []
for i in 0..2000 {
    let a = {n: i}
    let b = {peer: a}
    a.peer = b            // cycle
    if i % 500 == 0 { keep.push(a.n) }
}
print(keep)
"#;
    let out = ascript::run_source(src).await.unwrap();
    assert_eq!(out, "[0, 500, 1000, 1500]\n");
}
```

  (Requires Task 0.2's curated check to be green so `ascript` builds as a wasm dep. Adjust
  the program if `push` spelling differs — verify against `docs/content/stdlib/collections.md`.)
- [ ] **Step 3:** `wasm-pack test --node 2>&1 | tee /tmp/wasm-d1.log`. Also run a
  first **hello + async** end-to-end here (GO criterion 3): a test asserting
  `run_source` output for `print("hello")` and for an `async fn`/`await`/`task.gather`
  snippet, byte-compared against `target/release/ascript run` of the same source on native
  (hardcode the expected strings after capturing them natively; note the native command in
  the test comment).
- [ ] **Step 4:** Paste stacker outcome + gc/hello/async transcripts into **spec §10.4**.
  Commit — `spike(wasm): (d) gcmodule runtime smoke + stacker cell — spec §10.4`.
- [ ] Independent reviewer: re-runs the Node tests; verifies the native-vs-wasm expected
  strings by running the native binary themself; probes one edge (e.g. a Tier-2 panic
  program — does the error surface, not trap?) and records the observation in §10.4.

### Task 0.6: spike (e) — size & load → spec §10.5

- [ ] **Step 1:** `cd ascript-wasm && wasm-pack build --release --target web 2>&1 | tail -5`.
- [ ] **Step 2:**

```bash
wasm-opt -Oz -o pkg/ascript_wasm_bg.opt.wasm pkg/ascript_wasm_bg.wasm
gzip -9 -k pkg/ascript_wasm_bg.opt.wasm
ls -l pkg/ascript_wasm_bg.wasm pkg/ascript_wasm_bg.opt.wasm pkg/ascript_wasm_bg.opt.wasm.gz
```

- [ ] **Step 3:** Node cold-instantiate timing (5 runs, report min/median):

```bash
node -e 'const t0=performance.now();
const m=await import("./pkg/ascript_wasm.js"); await m.default();
console.log("instantiate ms:", (performance.now()-t0).toFixed(1));' --input-type=module
```

- [ ] **Step 4:** Record raw / `-Oz` / gz bytes + instantiate ms into **spec §10.5**. If
  gz > 5 MB, ALSO record the §5.7 pruning plan candidates with per-feature size deltas
  (re-measure with `--features data,log,shared` i.e. minus `binary`, etc.) — measurement,
  not threshold; size alone does not flip GO.
- [ ] Commit — `spike(wasm): (e) size/load numbers — spec §10.5`.
- [ ] Independent reviewer: re-measures; checks the gz file measured is the `-Oz` one (an
  easy mislabel); confirms §10.5 numbers match.

### Task 0.7: GO / NO-GO — spec §10.6 + goal-perf.md (the gate)

- [ ] **Step 1:** Evaluate spec §4.6 criteria 1–5 against §10.1–10.5. Write the decision
  record into **spec §10.6**: decision, date, the five criteria each with a one-line
  verdict + evidence pointer, and (on NO-GO) the per-blocker table (crate + version +
  error + unblock sketch + cost note).
- [ ] **Step 2:** Update `goal-perf.md`'s WASM entry status:
  **GO** → `🟡 in progress — Phase-0 spike GO recorded (spec §10.6, <date>)`;
  **NO-GO** → `⬜ NO-GO recorded <date> — blockers: <list> (spec §10.6); spike branch
  spike/wasm-target kept`.
- [ ] **Step 3 (NO-GO only):** push the spike branch, open no PR, STOP — the plan ends
  here as an honored outcome. **(GO only):** continue to Phase 1.
- [ ] Commit — `spike(wasm): GO/NO-GO recorded — spec §10.6 + goal-perf` (this commit
  carries the spec-§10 + goal-perf edits; Phase 1 cherry-picks it onto the feature branch).
- [ ] Independent reviewer: audits every §10 slot for verbatim raw output (no summarized
  evidence); confirms the decision follows §4.6 mechanically; confirms goal-perf wording.

---

## Phase 1 — main-crate portability (GO only; TDD; native byte-identical)

> `git checkout -b feat/wasm-target main && git cherry-pick <Task-0.7 commit>` (spec §10 +
> goal-perf evidence only). Re-implement every probe patch failing-test-first. After EVERY
> task in this phase: `cargo test` + `cargo test --no-default-features` + both clippy
> configs green, AND `cargo tree | diff - /tmp/tree-main.txt` (captured from `main` in
> Task 1.1) byte-equal — Gate W-1.

### Task 1.1: negative-space pins + build.rs skip + target-dep tables

**Files:** create `tests/wasm_negative_space.rs`; modify `build.rs`, `Cargo.toml`,
`src/lib.rs`.

- [ ] **Step 1 (pins first):** `cargo tree > /tmp/tree-main.txt` and
  `cargo tree --no-default-features > /tmp/tree-main-ndf.txt` from `main`. Then on the
  branch write `tests/wasm_negative_space.rs`:

```rust
//! WASM §7.3 — negative space: the wasm port changes NOTHING native-observable.
use std::fs;

#[test]
fn aso_format_version_unchanged() {
    // Read the constant from source so the pin tracks intent, not a copy.
    let src = fs::read_to_string("src/vm/aso.rs").unwrap();
    let line = src.lines().find(|l| l.contains("ASO_FORMAT_VERSION")).unwrap();
    // Record the value seen at branch time; bumping it in this branch is a spec violation.
    let at_branch: u32 = /* read src/vm/aso.rs NOW and inline the literal */;
    assert!(line.contains(&at_branch.to_string()),
        "WASM must not bump ASO_FORMAT_VERSION (spec §7.3)");
}

#[test]
fn no_wasm_cfg_in_chunk_or_serializer() {
    // The compiler/serializer paths stay platform-independent (spec §7.3).
    for f in ["src/vm/chunk.rs", "src/vm/aso.rs", "src/vm/opcode.rs", "src/compile/mod.rs"] {
        let src = fs::read_to_string(f).unwrap();
        assert!(!src.contains("target_family = \"wasm\""), "wasm cfg leaked into {f}");
    }
}
```

  (Resolve the `/* inline */` by reading `src/vm/aso.rs` at implementation time — the plan
  deliberately does not hardcode it.) Both tests green (they pin status quo).
- [ ] **Step 2:** Re-apply the build.rs skip (Task 0.2 Step 2 code verbatim, with the spec
  §2.1 comment). `cargo test --test treesitter_conformance` green.
- [ ] **Step 3:** Re-apply the Cargo target tables exactly per spec §5.3.2: move `tokio`,
  `rustyline` (and `stacker` iff §10.4 says so) into
  `[target.'cfg(not(target_family = "wasm"))'.dependencies]` with **today's exact feature
  lists**; add the wasm rows (`tokio` `rt,macros,sync` — plus `time` iff §10.1 showed it
  compiling; `wasm-bindgen`, `js-sys`, `getrandom(js)`, the `uuid(js)` optional row).
  `#[cfg(not(target_family = "wasm"))] pub mod repl;` in `src/lib.rs` with a doc-comment
  citing §2.2. Note: with `uuid` declared in target tables, keep the `data` feature's
  `dep:uuid` reference working — verify `cargo check --features data` resolves on native.
- [ ] **Step 4 (W-1 proof):** `cargo tree | diff - /tmp/tree-main.txt` AND the
  `--no-default-features` diff — **byte-equal**. Full native suite + both clippy configs
  green. `cargo check --target wasm32-unknown-unknown --no-default-features --features data,binary,log,shared` clean.
- [ ] Commit — `feat(wasm): build.rs wasm skip + target-dep tables + negative-space pins (W-1 proven)`.
- [ ] Independent reviewer: runs the tree diffs themself; greps `Cargo.toml` for any
  non-wasm-row feature drift; runs the wasm check; confirms the repl cfg doesn't break
  `cargo doc`/`--all-targets` builds.

### Task 1.2: `src/platform.rs` — clock/entropy/sleep seam (det seams above, untouched)

**Files:** create `src/platform.rs`; modify `src/lib.rs` (`pub mod platform;`),
`src/stdlib/time.rs`, `src/stdlib/math.rs`, `src/stdlib/task_mod.rs`,
`src/stdlib/time_timers.rs`.

- [ ] **Step 1 (failing-test-first, native):** unit tests in `src/platform.rs` pinning that
  the native arms are behavior-identical: `now_unix_ms()` within ±5s of
  `SystemTime::now()` computed inline; `monotonic_ms()` monotone across two calls;
  `entropy_seed()` nonzero and different across two processes is untestable — assert
  nonzero + that two calls in-process differ per today's `seed()` contract (read
  `math.rs:501-505` first and mirror its exact derivation as the native arm);
  `sleep_ms(10).await` sleeps ≥10ms (`#[tokio::test]`).
- [ ] **Step 2:** Implement per spec §5.3.3:

```rust
//! WASM §5.3.3 — the ONE home for raw ambient platform sources (clock, entropy,
//! sleep). Sits BELOW the det.rs seams: when a determinism context is armed, these
//! are never consulted. Native arms are byte-for-byte the previous inline code.

#[cfg(not(target_family = "wasm"))]
pub fn now_unix_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}
#[cfg(target_family = "wasm")]
pub fn now_unix_ms() -> f64 { js_sys::Date::now() }

#[cfg(not(target_family = "wasm"))]
pub fn monotonic_ms() -> f64 { /* lift time.rs's LazyLock<Instant> START here verbatim */ }
#[cfg(target_family = "wasm")]
pub fn monotonic_ms() -> f64 {
    js_sys::Reflect::get(&js_sys::global(), &"performance".into())
        .ok()
        .and_then(|p| js_sys::Reflect::get(&p, &"now".into()).ok()
            .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
            .and_then(|f| f.call0(&p).ok())
            .and_then(|v| v.as_f64()))
        .unwrap_or_else(js_sys::Date::now)
}

#[cfg(not(target_family = "wasm"))]
pub fn entropy_seed() -> u64 { /* today's math.rs seed() body, moved verbatim */ }
#[cfg(target_family = "wasm")]
pub fn entropy_seed() -> u64 {
    let mut b = [0u8; 8];
    getrandom::getrandom(&mut b).expect("getrandom(js)");
    u64::from_le_bytes(b) | 1
}

#[cfg(not(target_family = "wasm"))]
pub async fn sleep_ms(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}
#[cfg(target_family = "wasm")]
pub async fn sleep_ms(ms: u64) {
    // A hand-rolled setTimeout future (no gloo dep): the JS callback wakes the task;
    // works because the whole program future is polled by the browser microtask loop
    // (spec §5.3.6).
    use wasm_bindgen::{closure::Closure, JsCast};
    let p = js_sys::Promise::new(&mut |resolve, _| {
        let g = js_sys::global();
        let set_timeout = js_sys::Reflect::get(&g, &"setTimeout".into()).unwrap();
        let f = set_timeout.dyn_into::<js_sys::Function>().unwrap();
        let _ = f.call2(&g, &resolve, &(ms as f64).into());
    });
    let _ = wasm_bindgen_futures::JsFuture::from(p).await;
}
```

  (Exact JS-interop shape may be simplified with `web_sys`-free `js_sys` as above; the
  implementer verifies it compiles for wasm and adjusts mechanically — behavior contract:
  resolve after ≥ms.)
- [ ] **Step 3:** Re-route consumers (native-identical, one at a time, suite green after
  each): `time.rs` `now`/monotonic → `platform::{now_unix_ms,monotonic_ms}`; `math.rs`
  `seed()` → `platform::entropy_seed()`; `task_mod.rs:186,308` and `time_timers.rs:249`
  sleeps → `platform::sleep_ms`. **Do NOT touch `det.rs`** — run `cargo test det` to prove
  the seams unmoved; run the corpus differential
  (`cargo test --test vm_differential`) to prove byte-identity.
- [ ] Commit — `feat(wasm): src/platform.rs clock/entropy/sleep seam; consumers re-routed (det untouched)`.
- [ ] Independent reviewer: diffs the moved native bodies against `main`'s inline versions
  (must be verbatim-equivalent); runs `vm_differential` both feature configs; greps for
  remaining raw `SystemTime::now`/`Instant::now` in `src/stdlib/` and demands a recorded
  justification for each survivor (e.g. profiler/bench internals are native-only paths).

### Task 1.3: recursion ceiling on wasm + interval guard

**Files:** modify `src/interp.rs`, `src/vm/stack.rs` (iff §10.4 stacker cell failed),
`src/stdlib/time_timers.rs`; wrapper test added in Phase 2 exercises both.

- [ ] **Step 1:** `src/interp.rs` — split the constant with the spec §5.3.5 rationale:

```rust
/// WASM §5.3.5: wasm cannot grow native-stack segments (no stacker), so the logical
/// cap must fit the linker-set 8 MiB shadow stack with ≥2× margin. Calibrated by the
/// Phase-2 Node probe (binary-search the real frame budget; set to ≤50% of it) —
/// adjust the literal there if the probe says so, with the measurement in the commit.
#[cfg(target_family = "wasm")]
pub const MAX_CALL_DEPTH: u32 = 1000;
#[cfg(not(target_family = "wasm"))]
pub const MAX_CALL_DEPTH: u32 = 3000;
```

  The panic message is already shared — grep `maximum recursion depth exceeded` to confirm
  no second copy needs the constant.
- [ ] **Step 2:** `src/vm/stack.rs` wasm pass-throughs (Task 0.5 Step 1 code, with the
  module-doc note) **iff** §10.4 recorded stacker non-buildable; otherwise leave the dep in
  place and add only a doc-comment recording that `maybe_grow` degrades to pass-through on
  wasm (cite the §10.4 evidence).
- [ ] **Step 3:** Interval guard (spec §5.3.4): at the `tokio::time::interval` construction
  funnel in `time_timers.rs` (and any second construction site — grep
  `tokio::time::interval`), add:

```rust
#[cfg(target_family = "wasm")]
return Err(AsError::new(
    "time.every is not available on this platform (wasm)", span,
).into());
```

  shaped to the surrounding error plumbing (Tier-2 — mirror the nearest existing
  platform/feature refusal's construction; `worker/mod.rs:225` is the wording model).
  Native arm untouched. If §10.1 recorded tokio-`time` NOT compiling on wasm, additionally
  cfg the `ResourceState::Interval` variant + its match arms
  (`#[cfg(not(target_family = "wasm"))]`) — the spike output decides which shape, per spec.
- [ ] **Step 4:** Native suite + clippy + tree-diff green (W-1). wasm check clean.
- [ ] Commit — `feat(wasm): wasm recursion ceiling + interval platform guard`.
- [ ] Independent reviewer: confirms the native constant is untouched at 3000 and
  `examples/deep_recursion.as` still passes natively; confirms the guard message wording
  matches the spec §6 table exactly.

### Task 1.4: workers guard + the `wasm_run_source` lib entry

**Files:** modify `src/worker/isolate.rs`, `src/lib.rs`; extend
`tests/wasm_negative_space.rs`.

- [ ] **Step 1 (guard):** in `spawn_isolate` (`src/worker/isolate.rs:211`), before the
  `std::thread::Builder` spawn:

```rust
#[cfg(target_family = "wasm")]
{
    // WASM §5.3.7: one chokepoint covers worker fn / worker class / worker fn* /
    // run_in_worker / task.pmap / task.preduce — they all funnel here.
    return Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "workers are not available on this platform (wasm)",
    ));
}
```

  Verify the error PROPAGATES as a Tier-2 panic with that message through
  `dispatch_worker`'s existing io-error path (read `worker/mod.rs:87+`; if io-errors are
  `.expect`ed anywhere on that path, fix it to propagate — that is a Gate-14 bug fix with
  its own failing test, native-simulable by temporarily injecting the error).
- [ ] **Step 2 (entry):** `src/lib.rs` — the cfg-free, native-tested entry the wrapper
  calls (spec §5.4):

```rust
/// WASM §5.4: `run_source_exit` with an explicit `CapSet` applied at `Interp`
/// construction — the playground passes all-five-denied. cfg-free so native tests
/// exercise the exact path the wasm wrapper ships.
pub async fn wasm_run_source(
    src: &str,
    caps: crate::stdlib::caps::CapSet,
) -> Result<(String, Option<i32>), AsError> { /* run_source_exit body + caps threading */ }
```

  Implement by threading `caps` through the same construction `run_source_exit` uses (read
  how `--deny`/`--sandbox` reach the `Interp`/`Vm` from `main.rs` and reuse that setter —
  do not invent a second caps path).
- [ ] **Step 3 (failing tests first, native):** in `tests/wasm_negative_space.rs`:

```rust
#[tokio::test]
async fn wasm_entry_denies_all_caps() {
    let mut caps = ascript::stdlib::caps::CapSet::all_granted();
    /* deny all five — use the existing deny API; grep caps.rs for the method names */
    let (out, _) = ascript::wasm_run_source(
        r#"import * as caps from "std/caps"
let [v, err] = recover(() => /* an fs/env touch available in default build */)
print(err != nil)"#, caps).await.unwrap();
    assert_eq!(out, "true\n");
}
```

  plus a pin that `wasm_run_source` with `all_granted` byte-matches `run_source_exit` on a
  pure-compute program (the entry adds nothing).
- [ ] **Step 4:** Suite + clippy + tree-diff (W-1) green; wasm check clean.
- [ ] Commit — `feat(wasm): spawn_isolate platform guard + wasm_run_source caps entry`.
- [ ] Independent reviewer: traces the guard's propagation path by reading
  `dispatch_worker` → the Tier-2 surface and records the chain; verifies the cap-denied
  test asserts the SHIPPED denial wording; probes `worker fn` on native (still works).

### Task 1.5: Phase 1 holistic review

- [ ] Holistic reviewer: full native suite both configs; both clippy configs; the
  `cargo tree` diffs; `cargo test --test vm_differential` both configs;
  `cargo run -- run examples/async.as` + `examples/deep_recursion.as` spot-runs; the wasm
  curated check; confirms ZERO `target_family = "wasm"` cfg outside the files this phase
  names (grep) and the negative-space pins still pass.

---

## Phase 2 — the wrapper crate `ascript-wasm/` (GO only)

### Task 2.1: crate + `run_program` export (TDD under Node)

**Files:** rebuild `ascript-wasm/{Cargo.toml,.cargo/config.toml,src/lib.rs,tests/node_smoke.rs}`
on the feature branch (spec §5.1 shape; the spike skeleton is the reference, not the code).

- [ ] **Step 1 (failing tests first):** `tests/node_smoke.rs` — the spec §6 table as tests:
  hello (`print("hello")` → output `"hello\n"`, ok:true); async
  (`task.gather`/`await` program, output pinned against native); Tier-1 error program
  (ok:false, error non-null, plain text — **no ANSI escapes**: assert `!error.contains('\u{1b}')`);
  compile-error program (diagnostics non-empty, no ANSI); cap-denied (the Task 1.4 program);
  `worker fn` → error contains `workers are not available on this platform (wasm)`;
  `time.every` → its §6 message; deep recursion → `maximum recursion depth exceeded`
  (NOT a wasm trap); `exit(3)` → exitCode 3; the gc cycle smoke (Task 0.5's, kept
  permanently); `time.sleep(20)` completes (the sleep_ms shim end-to-end).
- [ ] **Step 2:** Implement `src/lib.rs`:

```rust
//! ascript-wasm — the browser playground entry (WASM spec §5.4).
use wasm_bindgen::prelude::*;

#[derive(serde::Serialize)]
struct RunResult {
    ok: bool,
    output: String,
    error: Option<String>,
    diagnostics: Vec<String>,
    #[serde(rename = "exitCode")]
    exit_code: Option<i32>,
    #[serde(rename = "durationMs")]
    duration_ms: f64,
}

#[wasm_bindgen(start)]
pub fn start() { console_error_panic_hook::set_once(); }

#[wasm_bindgen]
pub async fn run_program(source: String) -> JsValue {
    let t0 = ascript::platform::monotonic_ms();
    // §4.2's recorded configuration: LocalSet::run_until on the microtask loop
    // (+ the enter-guard arm iff spec §10.2 recorded it as required).
    let local = tokio::task::LocalSet::new();
    let caps = deny_all_caps(); // CapSet with fs/net/process/ffi/env denied (§5.4)
    let res = local
        .run_until(async { ascript::wasm_run_source(&source, caps).await })
        .await;
    let out = match res {
        Ok((output, exit_code)) => RunResult {
            ok: true, output, error: None, diagnostics: vec![],
            exit_code, duration_ms: ascript::platform::monotonic_ms() - t0,
        },
        Err(e) => RunResult {
            ok: false,
            output: e.captured_output().unwrap_or_default(), // expose if available; else ""
            error: Some(render_plain(&e)),       // ariadne with color OFF (§5.4)
            diagnostics: render_diagnostics(&e), // compile-phase: per-diagnostic plain strings
            exit_code: None, duration_ms: ascript::platform::monotonic_ms() - t0,
        },
    };
    serde_wasm_bindgen::to_value(&res_or(out)).unwrap()
}
```

  `render_plain`/`render_diagnostics`: reuse `src/diagnostics.rs` rendering with ariadne
  `Config` color disabled — if `diagnostics.rs` only exposes the eprint-to-terminal
  `report()`, add a `pub fn render_to_string(err: &AsError, color: bool) -> String` there
  (native-tested; the CLI `report()` becomes a one-line wrapper over it — a reuse
  refactor, not a fork). Whether partial output is recoverable from an `Err` depends on
  `AsError`'s shape — read it; if not carried, return output `""` on error v1 and record
  that in `tooling/playground.md` (panics lose prior prints in v1 — honest doc), OR
  thread the capture buffer out; implementer decides by reading `run_source_exit`'s error
  path and records the choice in the commit message.
- [ ] **Step 3 (recursion calibration — spec §5.3.5):** a Node probe test that runs a
  self-recursive program at depth `MAX_CALL_DEPTH - 10` (must COMPLETE) and one that
  exceeds it (must yield the clean panic). If the near-limit run traps the wasm stack,
  binary-search the real budget, set the wasm `MAX_CALL_DEPTH` literal to ≤50% of it
  (main-crate edit, with the measurement in the commit message), re-run.
- [ ] **Step 4:** `wasm-pack test --node` green. Commit —
  `feat(wasm): ascript-wasm wrapper — run_program + RunResult + Node test suite`.
- [ ] Independent reviewer: runs the Node suite; anti-false-green probe (break the deny-all
  → cap test must fail; restore); verifies no ANSI in error/diagnostics; verifies the
  recursion near-limit test genuinely runs deep (print the depth).

### Task 2.2: the mini-differential smoke — `scripts/wasm_smoke.mjs`

**Files:** create `scripts/wasm_smoke.mjs`, `scripts/build-wasm.sh`.

- [ ] **Step 1:** `scripts/build-wasm.sh` — the ONE artifact pipeline (used by CI and by
  the docs-artifact commit):

```bash
#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../ascript-wasm"
wasm-pack build --release --target web
wasm-opt -Oz -o pkg/ascript_wasm_bg.wasm.opt pkg/ascript_wasm_bg.wasm
mv pkg/ascript_wasm_bg.wasm.opt pkg/ascript_wasm_bg.wasm
mkdir -p ../docs/assets/playground/pkg
cp pkg/ascript_wasm.js pkg/ascript_wasm_bg.wasm ../docs/assets/playground/pkg/
```

- [ ] **Step 2:** `scripts/wasm_smoke.mjs`: builds nothing (takes the built pkg path);
  contains the committed `EXAMPLES_WASM` list (spec §5.6 — derive it by sweeping
  `examples/*.as`: include iff (imports ∩ {fs,env,process,net,http,sqlite,…excluded
  modules} = ∅) AND no `worker`/`time.every` use; start from the spec's expected ≳25 and
  record per-file include/exclude reasons as comments — shrinking later requires a
  recorded reason in-file). For each example: `run_program(source)` under Node, compare
  `result.output` **byte-for-byte** against `execFileSync(nativeBin, ["run", file])`
  stdout; on `ok:false` or any diff, print a unified diff and exit 1. Also assert
  `examples/deep_recursion.as` is EXCLUDED with the recorded reason (different ceiling).
- [ ] **Step 3:** `cargo build --release && ./scripts/build-wasm.sh && node scripts/wasm_smoke.mjs target/release/ascript` —
  green. Fix any divergence as a real bug (most likely suspects: float printing must
  already match since it's the same VM; clock/RNG examples must be excluded — they're
  nondeterministic on BOTH hosts; record exclusions).
- [ ] Commit — `feat(wasm): build-wasm.sh + wasm_smoke.mjs mini-differential (N examples byte-equal)`.
- [ ] Independent reviewer: runs the smoke; corrupts one expected output to prove the diff
  trips; audits 5 random exclusions against their stated reasons; counts the list (≥20 or
  a recorded justification).

### Task 2.3: Phase 2 holistic review

- [ ] Holistic reviewer: full Node suite + smoke from a clean checkout (`rm -rf
  ascript-wasm/target ascript-wasm/pkg`); native suite still green (the wrapper is
  workspace-excluded — verify root `cargo build` does NOT build it); spike-vs-final size
  re-measured and appended to spec §10.5 (post-implementation row).

---

## Phase 3 — playground UI + docs (GO only)

### Task 3.1: `playground.html` + the Web-Worker driver

**Files:** create `docs/playground.html`, `docs/assets/playground.js`,
`docs/assets/playground-worker.js`; commit `docs/assets/playground/pkg/` (built by
`build-wasm.sh`).

- [ ] **Step 1:** `docs/playground.html` — same topbar/brand markup as `reader.html:13-24`
  (copy the header block, add `Playground` as the active topnav item), `styles.css`
  reused, layout: editor `<textarea id="src" spellcheck="false">`, toolbar (Run, Stop,
  examples `<select id="examples">`, Share), output `<pre id="out">`. No new CSS file
  unless styles.css genuinely lacks the pieces — prefer extending styles.css with a small
  `/* playground */` section (app.js-era conventions: no framework, no build step).
- [ ] **Step 2:** `docs/assets/playground-worker.js` (a **browser** Web Worker — JS-side
  only; NOT the AScript worker subsystem):

```js
// Runs the wasm engine off the UI thread; Stop = terminate() from the page side.
let ready = (async () => {
  const mod = await import('./playground/pkg/ascript_wasm.js');
  await mod.default();
  return mod;
})();
self.onmessage = async (e) => {
  const { id, source } = e.data;
  try {
    const mod = await ready;
    const result = await mod.run_program(source);
    self.postMessage({ id, result });
  } catch (err) {
    self.postMessage({ id, result: { ok: false, output: '', error: String(err),
      diagnostics: [], exitCode: null, durationMs: 0 } });
  }
};
```

- [ ] **Step 3:** `docs/assets/playground.js`: spawn the worker
  (`new Worker('assets/playground-worker.js', { type: 'module' })`), `run()` posts
  `{id, source}` and renders the `RunResult` (output verbatim; error/diagnostics in a
  styled error block; duration + exitCode in a status line); **Stop =
  `worker.terminate()` + lazy respawn** (spec §5.5); examples `<select>` populated from a
  small inline manifest (8–12 entries; each `{title, source}` — source strings inlined at
  authoring time from `EXAMPLES_WASM` members, kept short); Share button writes
  `location.hash = '#code=' + base64url(src)`; on load, a `#code=` hash populates the
  editor (the read-side that the Task 3.3 stretch builds on). Match `app.js` style
  (plain functions, `el()`-style helpers if present — read app.js first and mirror).
- [ ] **Step 4:** Manual verification (recorded): `cd docs && python3 -m http.server` —
  run hello + an async example; Stop kills `while true {}` and the next Run works
  (respawn); a `#code=` URL round-trips; the page works after `build-wasm.sh` regenerates
  the pkg. Record a checklist in the commit message.
- [ ] Commit — `feat(wasm): docs playground page + Web-Worker driver + committed pkg artifact`.
- [ ] Independent reviewer: serves the site, replays the manual checklist, probes: Run
  while a run is in flight (must queue or disable — no interleaved output), a
  compile-error program (diagnostics render, no ANSI), `worker fn` program (clean message
  in the output pane), browser refresh mid-run (no console errors).

### Task 3.2: docs content + NAV + links

**Files:** create `docs/content/tooling/playground.md`; modify `docs/assets/app.js`
(NAV), `docs/index.html`, `docs/reader.html`, `docs/content/examples.md`, `README.md`.

- [ ] **Step 1:** `tooling/playground.md`: what it is (client-side compile+run), the §5.2
  subset table reproduced (in/out/candidates AS SHIPPED — reflect the spike's
  crypto/datetime verdicts), the platform-asymmetry list (spec §7.2: recursion ceiling,
  no workers/intervals, captured output, deny-all caps), the share-link format, and a
  "report a playground bug" pointer.
- [ ] **Step 2 (the NAV-orphan rule):** add `['tooling/playground', 'Playground']` to the
  `NAV` Tooling section in `docs/assets/app.js:11+` — sidebar AND cmd-K search derive from
  it. Verify the page is reachable + searchable on the served site.
- [ ] **Step 3:** Links: `docs/index.html` topnav (`:15-18`) gains
  `<a href="playground.html">Playground</a>` + a hero button; `docs/reader.html` topbar
  (`:16`) gains the same topnav link; `docs/content/examples.md` gains a one-line "run
  these in the playground" pointer; `README.md` gains a Playground bullet (link + the
  one-line "compiles and runs entirely in your browser").
- [ ] **Step 4:** Served-site click-through of every new link (the in-content link rule:
  relative to the page's directory — `](playground)` from `tooling/`, and the examples.md
  pointer must be a plain `playground.html` href since it leaves the reader app — verify
  both resolve).
- [ ] Commit — `docs(wasm): playground content page + NAV + topbar/index/README links`.
- [ ] Independent reviewer: cmd-K search finds "Playground"; clicks every link from the
  served site; diffs the subset table against the shipped feature list in
  `ascript-wasm/Cargo.toml` (they must agree — this table is the documentation contract).

### Task 3.3 (STRETCH — optional-in-v1, spec §5.5; skipping requires only a roadmap note): Run▶ on docs code blocks

- [ ] **Step 1:** in `app.js` `renderMarkdown`'s fenced-code path, when the fence language
  is `ascript`/`as` AND the block contains no excluded-module import (reuse the
  EXAMPLES_WASM exclusion regex, inlined), append a small `Run ▶` anchor:
  `playground.html#code=<base64url(block)>`. Pure read-side reuse — the hash loader
  shipped in Task 3.1.
- [ ] **Step 2:** Manual spot-check on 3 language pages + 1 stdlib page; blocks with `fs`/
  `net` imports must NOT get the button.
- [ ] Commit — `docs(wasm): Run-in-playground buttons on eligible code blocks (stretch)`.
- [ ] Independent reviewer: probes a template-literal-heavy block (base64url survives
  `${}`), and a >2KB block (URL length acceptable; if not, cap with a recorded threshold).

### Task 3.4: Phase 3 holistic review

- [ ] Holistic reviewer: full served-site pass (landing, reader, playground, search, every
  new link), one full playground session per excluded-capability error class (§6 table),
  and a docs-staleness sweep: no page still claims wasm/playground is unavailable.

---

## Phase 4 — CI + finish (GO only)

### Task 4.1: the `wasm` CI job + artifact freshness

**Files:** modify `.github/workflows/ci.yml`; create `bench/WASM_SIZE.md`.

- [ ] **Step 1:** add job `wasm` (alongside `test`/`fuzz-smoke`), per spec §5.6:

```yaml
  wasm:
    name: wasm · build + clippy + smoke
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install Rust + wasm target
        run: rustup toolchain install stable --profile minimal --component clippy && rustup target add wasm32-unknown-unknown
      - uses: actions/setup-node@v4
        with: { node-version: 20 }
      - name: Install wasm-pack + binaryen
        run: cargo install wasm-pack --locked && sudo apt-get update && sudo apt-get install -y binaryen
      - name: Check (main crate, curated features)
        run: cargo check --target wasm32-unknown-unknown --no-default-features --features data,binary,log,shared
      - name: Clippy (wasm target)
        run: cargo clippy --target wasm32-unknown-unknown --no-default-features --features data,binary,log,shared -- -D warnings
      - name: Wrapper tests (Node)
        run: cd ascript-wasm && wasm-pack test --node
      - name: Build native oracle
        run: cargo build --release
      - name: Build wasm artifact
        run: ./scripts/build-wasm.sh
      - name: Committed-artifact freshness
        run: git diff --exit-code docs/assets/playground/pkg/ || (echo "::error::docs pkg is stale — run scripts/build-wasm.sh and commit" && exit 1)
      - name: Mini-differential smoke (wasm output == native output)
        run: node scripts/wasm_smoke.mjs target/release/ascript
      - name: Size report
        run: ls -l docs/assets/playground/pkg/ascript_wasm_bg.wasm && gzip -9 -c docs/assets/playground/pkg/ascript_wasm_bg.wasm | wc -c | xargs -I{} echo "gzip bytes: {}" >> "$GITHUB_STEP_SUMMARY"
```

  (Adjust the curated-features strings IF the spike admitted `crypto`/`datetime` — keep
  CI, wrapper `Cargo.toml`, and the docs table in lockstep; the reviewer cross-checks all
  three.) Note the freshness check requires `build-wasm.sh` to be reproducible — if
  wasm-pack output proves nondeterministic in CI, replace the byte-diff with a
  size+exports check and record why in the workflow comment.
- [ ] **Step 2:** `bench/WASM_SIZE.md` — the spec §5.7 record: raw/-Oz/gz/brotli bytes,
  Node instantiate ms, one manually-recorded browser cold-load, the spike row vs the
  merge row, and the pruning-candidates note iff gz > 5 MB.
- [ ] **Step 3:** Push; CI green including the new job.
- [ ] Commit — `ci(wasm): wasm job — build, clippy, Node tests, artifact freshness, mini-differential, size`.
- [ ] Independent reviewer: forces a smoke failure in a scratch commit (corrupt one
  expected output) → CI must fail; reverts; confirms the freshness step trips on a stale
  pkg.

### Task 4.2: CLAUDE.md / roadmap / goal-perf / final gates checklist

- [ ] **Step 1:** `CLAUDE.md`: a WASM subsection under the larger-subsystems list — the
  wrapper crate + build command, the curated-feature lockstep rule (wrapper Cargo.toml ==
  CI == docs table), the platform-asymmetry list (§7.2), the target-dep-table rule for
  future dep edits ("a new non-optional dep must be wasm-clean or target-gated; CI's wasm
  check is the tripwire"), and the `src/platform.rs` seam ("raw clock/entropy/sleep go
  here, below det.rs — never inline `SystemTime::now` in stdlib again").
- [ ] **Step 2:** `superpowers/roadmap.md` — the WASM milestone entry (spike verdict +
  what shipped); `goal-perf.md` — flip the WASM item to ✅ with the spec/§10 pointer.
- [ ] **Step 3 — final gates checklist (all boxes required):**
  - [ ] Gates 1–14: native four-mode differential + goldens green BOTH configs
    (`cargo test --test vm_differential`, full `cargo test`, `--no-default-features`);
    clippy clean both native configs AND the wasm target (W-2); no borrow-across-await
    introduced (clippy deny active); zero `type-*` corpus regressions
    (`cargo test --test check corpus` both configs); no placeholders outside spec §10;
    examples corpus untouched-or-extended, never deleted.
  - [ ] W-1: `cargo tree` diffs vs `main` byte-equal (both configs) — re-run at merge.
  - [ ] W-2: the wasm CI job green (clippy, Node tests, freshness, mini-differential, size).
  - [ ] W-3: every §6 failure-mode row has a passing test (enumerate: unknown-module,
    cap-denied, workers, interval, recursion, panic-rendering, gc-cycle, sleep).
  - [ ] W-4: `tooling/playground.md` + NAV entry live; index/reader/README links;
    examples.md pointer; subset table == shipped features.
  - [ ] Negative space: `tests/wasm_negative_space.rs` green (`ASO_FORMAT_VERSION`
    unchanged; no wasm cfg in chunk/serializer/opcode/compile paths).
  - [ ] `bench/WASM_SIZE.md` committed with merge-time numbers.
  - [ ] Spec §10 fully populated (10.1–10.6), no empty slot.
- [ ] **Step 4:** Final holistic review subagent over the whole branch diff; then merge
  `--no-ff` per house cadence.
- [ ] Commit — `docs(wasm): CLAUDE.md/roadmap/goal-perf updates + gates checklist`.
