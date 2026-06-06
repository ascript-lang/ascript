# SP7 — Docs & cleanup — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resync docs/comments with the post-VM/post-GC/post-CST reality, reword stale "not yet"/"deferred" markers, correct the stale #147 opt-out comment and add the (already-green) held-future differential assertion, normalize the tree with `cargo fmt` (isolated commit), and record the accepted SP1 trade-offs. **No behavior change** except one new passing test + the `cargo fmt` reflow.

**Architecture:** Six commit groups (A docs, B comments, C dead-arm rewords, D #147, E isolated `cargo fmt`, F known-items). Each group is a single focused commit; the full gate set stays green after each. This is cleanup — there is no TDD red phase except group D (where the new test is written, then shown to already pass because the engines already agree).

**Tech Stack:** Rust. Markdown docs; Rust doc/inline comments; `tests/vm_differential.rs`; `cargo fmt`. No `Value`/opcode/grammar/`.aso` change.

**Spec:** `docs/superpowers/specs/2026-06-04-sp7-docs-cleanup-design.md`.

**Branch:** `feat/sp1-engine-parity` (SP7 cleanup rides the same branch).

---

## Conventions for every task

- **Gate set (run + paste tails after each commit):**
  - `cargo build 2>&1 | tail -3`
  - `cargo test 2>&1 | tail` (0 failures, all binaries)
  - `cargo test --no-default-features 2>&1 | tail` (0 failures)
  - `cargo clippy --all-targets 2>&1 | tail` (clean) AND `cargo clippy --no-default-features --all-targets 2>&1 | tail` (clean)
  - `grep await_holding_refcell_ref Cargo.toml` (still `deny`)
  - For the documentation-only commits (A, F): a build + targeted `cargo test --doc` is sufficient if no `src/` compiled file changed; for any commit touching `src/` (B, C) run the FULL gate set.
- **Behavior-neutrality check:** for B and C, after the edit run `cargo test --test vm_differential 2>&1 | tail` and confirm it is byte-identical to before (no diff in pass count). No engine output may change.
- **Commit trailer:** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Do NOT** touch the SP1-owned rejections (`src/compile/mod.rs:2046/2054` `fn*` methods, `:3605` `a?.m()`), the accurate in-flight compile comments (`:56`, `:800`, `:2548`), or the `Op::InstanceOf` reservation (SP2 owns it).
- **Isolation:** the `cargo fmt` reflow (group E) is its OWN commit, run LAST among code commits so it cleanly reflows whatever B/C/D added (or first — see Task E note); never fold it into a content commit.

---

## Group A — Stale docs (§1) — documentation only

**Files:** `docs/content/stdlib/net.md`, `docs/superpowers/roadmap.md`, `docs/superpowers/specs/2026-05-29-ascript-design.md`, `docs/superpowers/specs/2026-06-03-ranges-step-analyzer-design.md`.

### Task A1: correct the net.md HTTP-server concurrency paragraph

- [ ] **Step 1 — Read** `docs/content/stdlib/net.md:416` and `src/stdlib/http_server.rs:17–27,706,764–771,805`.
- [ ] **Step 2 — Rewrite** the "## std/http/server" intro paragraph (`:416`) to describe current behavior: each accepted connection is handled on its **own `spawn_local` task** so a slow handler can't block other clients; a `tokio::sync::Semaphore` caps in-flight handler concurrency. Drop "strictly sequentially" and "Concurrent connections are a documented v1 limitation." Match the doc's existing tone (concise, code-snippet-led). This is a reference doc → correct outright (no dated note).

### Task A2: dated notes on the roadmap REPL + fmt lines

- [ ] **Step 3 — Read** `docs/superpowers/roadmap.md:611` and `:613`.
- [ ] **Step 4 — Append a dated note** after each line (do NOT delete the historical line). For `:611`: note that REPL multi-line accumulation shipped (the `is_incomplete` token-depth buffer; CLAUDE.md "REPL multi-line input"). For `:613`: note that the lossless CST formatter (`src/syntax/format`) superseded the AST pretty-printer and preserves comments. Use the form `**Update 2026-06-04:** …` matching any existing dated-note style in the file (grep for "Update" first; if none, this is the introduced convention).

### Task A3: dated note on the design-spec "No bytecode VM" non-goal

- [ ] **Step 5 — Read** `docs/superpowers/specs/2026-05-29-ascript-design.md:33` (the §1 v1 non-goals list).
- [ ] **Step 6 — Append** a dated parenthetical to that bullet: a bytecode VM is now the **default** engine and the tree-walker is the byte-identical reference oracle; JIT remains a non-goal. Do NOT delete the historical bullet (this spec is the historical design record).

### Task A4: dated note on the ranges current-state table

- [ ] **Step 7 — Read** `docs/superpowers/specs/2026-06-03-ranges-step-analyzer-design.md:60–68` (the "Status today" table) and `:271`.
- [ ] **Step 8 — Append** a dated note immediately after the table (and at `:271`) stating that the rows marking `..=` in for-range / `..=` as a value / "rejected outside patterns" were closed by the ranges/step work: `print(1..=5)` → `[1,2,3,4,5]` and descending `for (i in 10..=1 step -2)` now run on both engines. Leave the historical snapshot rows intact.

- [ ] **Step 9 — Gate** (docs-only: `cargo build` + a docs-site sanity skim is enough; no `src/` changed). **Commit:** `docs(sp7): resync stale docs — http server concurrency, REPL/fmt/VM/ranges dated notes`.

---

## Group B — Stale code comments (§2) — comment only

**Files:** `src/syntax/mod.rs`, `src/syntax/kind.rs`, `src/gc.rs`, `src/vm/value_ext.rs`, `src/compile/mod.rs`.

### Task B1: reword each stale comment

- [ ] **Step 1 — `src/syntax/mod.rs:4`:** replace "does not yet drive the binary." with a line stating the CST front-end is the production front-end (drives `compile` → default VM engine); the legacy `lexer`/`parser`/`ast` front-end now backs only the `--tree-walker` reference oracle.
- [ ] **Step 2 — `src/syntax/kind.rs:11`:** change "// --- nodes (only Root for now; the parser plan adds the rest) ---" to "// --- core node kinds ---" (or similar), dropping "only Root for now".
- [ ] **Step 3 — `src/gc.rs:16` (the `### Phasing` block):** rewrite to past tense — V13-T1 (Trace impls), V13-T2 (Rc→Cc migration of the cycle-capable variants), V13-T3 (collection runs) are all complete; the `Trace` impls are load-bearing (the collector calls them; see `src/value.rs` `Cc<…>` variants and `gc::collect`). Keep the "what is traced vs acyclic / deterministic-Drop invariant" text below (still accurate).
- [ ] **Step 4 — `src/vm/value_ext.rs:5`:** reword "V4/V5 fold `Closure` into `Value` …" — `Closure` is already a `Value` variant (`src/value.rs:370`); state that `value_ext` holds `Closure` plus the VM-only run-loop status enums (`RunOutcome`/`FiberState`). Also re-check `:46` ("not yet driven this task") and reword if stale.
- [ ] **Step 5 — `src/compile/mod.rs:1499–1500`:** reword so `extends`/`super` are described as implemented; the only remaining reservation is `instanceof`, **owned by SP2** (do NOT claim `instanceof` shipped; do NOT remove the reservation).
- [ ] **Step 6 — `src/compile/mod.rs:1820–1825`:** reword the import docstring so file/relative (`./…`) imports + `export <decl>` are described as compiling (drop "deferral to V12-T4 — a CompileError here"); cross-reference `:5221` where the V12-T1 deferral note already says it was lifted.

- [ ] **Step 7 — Verify scope:** confirm you did NOT touch `:56`, `:800`, `:2548` (accurate in-flight compile comments) or the SP1 rejections (`:2046/2054`, `:3605`).
- [ ] **Step 8 — Full gate set** + confirm `cargo test --test vm_differential` pass count is unchanged. **Commit:** `chore(sp7): reword stale code comments to match shipped CST/VM/GC reality`.

---

## Group C — Dead defensive match arms (§3) — reword messages, keep arms

**Files:** `src/compile/mod.rs` (`:1118`, `:1209`, `:3258`, `:3838`, `:4124`, `:4396`).

> **Discipline:** every one of these matches is over a wide enum (`SyntaxKind`) or `Option<Resolution>` and the catch-all is REQUIRED for exhaustiveness. **Do NOT delete any arm** (deletion breaks compilation). **Only** reword the `CompileError` message from "not yet supported in V1/V2/V4" to a clear internal-invariant message. Behavior on valid input is unchanged (these arms are unreachable from the parser).

### Task C1: reword each catch-all to an internal-invariant message

- [ ] **Step 1 — `:1118`** ("assignment to a non-local target not yet supported (V4)"): confirm the `Some(Local)`/`Some(Upvalue)`/`Some(Global)` arms above cover every assignable NameRef (an undeclared target resolves to `Global`; verified `undeclaredX = 5` → runtime "cannot assign to undefined variable" on both engines). Reword to e.g. `"internal: assignment target resolved to an unexpected binding (compiler invariant)"`. Keep the `_` arm.
- [ ] **Step 2 — `:1209`** ("statement kind not yet supported in V2"): confirm every `Stmt` variant except `ExprStmt` is handled and all callers (`compile_source:730`, `compile_block:3554`, `compile_export:1924`) intercept `ExprStmt`/pass only declarations. Reword to internal-invariant (e.g. `"internal: unexpected statement kind in compile_stmt (compiler invariant)"`). Keep the arm.
- [ ] **Step 3 — `:3258`** ("unsupported match pattern kind {other:?}"): already invariant-style; normalize to an `"internal: …"` prefix consistent with the others. Keep the arm.
- [ ] **Step 4 — `:3838`** ("binary operator {other:?} not yet supported in V2"): confirm all arithmetic/comparison ops handled and `&&`/`||`/`??` route through the short-circuit path above. Reword to `"internal: unexpected binary operator {other:?} (compiler invariant)"`. Keep the arm.
- [ ] **Step 5 — `:4124`** ("unary operator {other:?} not yet supported in V1"): confirm `Minus`/`Bang` are the only unary ops. Reword to internal-invariant. Keep the arm.
- [ ] **Step 6 — `:4396`** ("literal token {other:?} not yet supported in V1"): confirm `Number`/`Str`/`TrueKw`/`FalseKw`/`NilKw` are all literal tokens. Reword to internal-invariant. Keep the arm.

- [ ] **Step 7 — Build + full gate set** + confirm `cargo test --test vm_differential` pass count is unchanged (these arms are never hit by valid corpus, so nothing should move). **Commit:** `chore(sp7): reword unreachable compile catch-alls to internal-invariant messages`.

---

## Group D — Close #147 (§4)

**Files:** `tests/vm_differential.rs`, `docs/superpowers/roadmap.md` (gap register / #147 status).

### Task D1: add the held-future drain differential assertion + correct the stale comment

- [ ] **Step 1 — Read** the opt-out comment at `tests/vm_differential.rs:~2955` (inside `vm_unawaited_async_call_is_cancelled_like_treewalker`) and the drain semantics in `src/lib.rs` (`run_source` `:122–124`, `vm_run_source_with` `:391–393` — both `run_until(...).await; local.await;`).
- [ ] **Step 2 — Add a new differential test** (match the file's existing `#[tokio::test]` + three-engine pattern; read a neighbor like `vm_unawaited_async_loop_stays_bounded_and_matches_treewalker` for the exact entry-point helpers). It must assert byte-identical output across tree-walker / specialized-VM / generic-VM for a HELD un-awaited future whose body `await`s then prints:

```rust
#[tokio::test]
async fn vm_held_future_drains_identically_to_treewalker() {
    // #147: a future HELD in a local until program end (not the bare cancel-on-drop
    // case) whose body awaits then prints. Both engines drain spawned tasks at
    // end-of-program (`local.run_until(..).await; local.await;` in src/lib.rs), so the
    // body runs on BOTH — byte-identical. (The neighboring test covers the bare
    // un-awaited cancel-on-drop case.)
    let src = "async fn work() { await 0\n print(\"worked\") }\nlet f = work()\nprint(\"main\")\n";
    let tw = ascript::run_source(src).await.expect("tree-walker ok");
    let (vm, _) = ascript::vm_run_source(src).await.expect("vm ok");
    let (gen, _) = ascript::vm_run_source_generic(src).await.expect("generic vm ok");
    assert_eq!(tw, vm, "specialized VM diverged from tree-walker");
    assert_eq!(tw, gen, "generic VM diverged from tree-walker");
    assert_eq!(tw, "main\nworked\n");
}
```

- [ ] **Step 3 — Run** `cargo test --test vm_differential vm_held_future_drains_identically_to_treewalker 2>&1 | tail` → it PASSES immediately (the engines already agree — this is a guard, not a fix; verified during planning: all three emit `"main\nworked\n"`).
- [ ] **Step 4 — Correct the stale opt-out comment** in `vm_unawaited_async_call_is_cancelled_like_treewalker`: remove/replace the sentences claiming a held future "interacts with end-of-program task draining and the two engines legitimately differ there (the tree-walker's end-of-program drain runs the still-held task; the VM does not)." Replace with a one-liner noting the held-future case is now covered by `vm_held_future_drains_identically_to_treewalker` (engines agree); keep the bare-un-awaited cancel-on-drop assertions intact.
- [ ] **Step 5 — Mark #147 resolved** in `docs/superpowers/roadmap.md` (gap register / wherever #147 is tracked) with a dated note pointing at the new test.
- [ ] **Step 6 — Full gate set** (the differential binary must be green with the new test). **Commit:** `test(sp7): assert held un-awaited future drains identically on both engines (closes #147)`.

---

## Group E — `cargo fmt` the Rust tree (§5) — ISOLATED commit

**Files:** whole Rust tree (formatting only).

### Task E1: run cargo fmt, verify green, isolated commit

- [ ] **Step 1 — Order note:** run this AFTER Groups B/C/D so it cleanly reflows any lines they touched (so B/C/D commits stay free of fmt churn, and E is pure whitespace). If a worker prefers fmt-first, it must re-run `cargo fmt` once more at the end and confirm no further drift — but the simpler path is fmt-last.
- [ ] **Step 2 — Confirm pre-existing drift exists** (sanity): `cargo fmt --check 2>&1 | head` (expect non-empty, e.g. `build.rs:23,52`).
- [ ] **Step 3 — Run** `cargo fmt`.
- [ ] **Step 4 — Verify behavior-neutral:** full gate set — `cargo build`, `cargo test` (default), `cargo test --no-default-features`, `cargo clippy --all-targets`, `cargo clippy --no-default-features --all-targets`. All green (formatting cannot change behavior). `cargo fmt --check` now clean.
- [ ] **Step 5 — Confirm the diff is formatting-only** (`git diff --stat`; spot-check a few hunks are whitespace/line-wrap only). **Commit (isolated):** `style(sp7): cargo fmt the Rust tree (formatting only, no behavior change)`.

---

## Group F — Document known/accepted SP1 items (§6) — documentation only

**Files:** `CLAUDE.md`, `docs/superpowers/specs/2026-06-04-sp1-engine-parity-class-model-design.md`.

### Task F1: record the accepted trade-offs + the InstanceOf coordination

- [ ] **Step 1 — `CLAUDE.md` "Current deferrals" (`:422–423`):** append three accepted items, matching the section's terse owner-noted style:
  - the **1-column caret-span offset** between the CST and legacy front-ends in error diagnostics (cosmetic, message always correct, accepted);
  - the **perf regression** (~2.9× → ~2.5× geomean) from routing top-level vars through `GET_GLOBAL` for tree-walker-parity late-binding (accepted trade, still ≥2× gate; SP8 may recover it);
  - that **`Op::InstanceOf` is reserved for SP2** (declared at `src/vm/opcode.rs:290`, not yet emitted) — do NOT remove it as "dead code".
- [ ] **Step 2 — SP1 design spec:** add a short "Known/accepted after implementation" note recording the caret-offset + the perf trade (cross-reference SP8). Keep it brief.
- [ ] **Step 3 — Verify wording** does NOT instruct anyone to "remove" the `InstanceOf` reservation (coordinate with SP2 ownership).
- [ ] **Step 4 — Gate** (docs-only: `cargo build`). **Commit:** `docs(sp7): record accepted SP1 trade-offs (caret offset, perf, InstanceOf reservation)`.

---

## Self-review (author)

**Spec coverage:** §1→Group A; §2→Group B; §3→Group C; §4→Group D; §5→Group E; §6→Group F. All covered.

**Behavior-neutrality:** A/F docs-only; B comments-only; C reword unreachable-arm messages (no valid program reaches them); D adds a passing test + comment fix (engines already agree, verified during planning); E is pure `cargo fmt`. The whole-corpus three-way differential, both feature configs, and clippy (both) must stay green after each commit; the only test-count change is +1 in Group D.

**Do-not-touch list (explicit):** SP1 rejections (`compile/mod.rs:2046/2054` `fn*` methods, `:3605` `a?.m()`); accurate in-flight comments (`:56`, `:800`, `:2548`); the `Op::InstanceOf` reservation (SP2 owns it — reword its mention, never remove). No match arm is deleted (all are exhaustiveness-required).

**Verified during planning (not stale → handled correctly):** the `:1118`/`:1209`/`:3838`/`:4124`/`:4396` arms ARE unreachable on valid input (so safe to reword), but are NOT safe to delete (exhaustiveness); `:3258` is already invariant-style (only normalize the prefix). The held-future #147 case was probed and all three engines emit `"main\nworked\n"`. The `..=`/descending-`step` ranges were run and work. The `cargo fmt` drift is real (`build.rs:23,52`).

**Isolation:** Group E is its own commit, run last among code-touching groups. No placeholders; every path is concrete; every command is runnable.
