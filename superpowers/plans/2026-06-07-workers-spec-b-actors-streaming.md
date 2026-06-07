# Workers Spec B — Stateful Workers (Actors & Streaming Generators) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the stateful, dedicated-isolate worker lifecycle — `worker class` actors (proxy handle, async-only methods, FIFO one-at-a-time mailbox, resource ownership, non-reentrancy guard, close/last-drop teardown) and `worker fn*` streaming generators (demand-driven pull, bounded buffer, bidirectional `next(v)`, close/drop) — plus the `pipe` bridge helper, all on top of Plan A's foundation and byte-identical across both engines and all four execution modes.

**Architecture:** Both surface forms are one mechanism: a thing born in its own dedicated isolate, holding state, talked to over time through Plan A's serialized-message airlock. An **actor handle** is a `Value::Native` (`NativeKind::WorkerActor`, lives in `Interp.resources`) whose method calls become FIFO mailbox messages serialized across a tokio channel to a dedicated isolate that runs the class instance; a **streaming handle** is a `Value::Generator` whose body is a cross-thread driver (a third `GenImpl::Worker` variant in `src/coro.rs`) that pulls one serialized yield per `resume` demand-credit from a dedicated isolate running the `worker fn*`. Handles are GC-opaque (native-handle invariant) and torn down on explicit `close()` or last-drop (extends Plan A cancel-on-drop).

**Tech Stack:** Rust, tokio (`current_thread` runtime + `LocalSet` per isolate; `Send` byte channels — `mpsc`/`oneshot` — across threads, futures stay on the caller thread), Plan A's `src/worker/` serializer + isolate bootstrap + code-shipping, tree-sitter (grammar regen `--abi 14`).

**Depends on:** Plan A (`superpowers/plans/2026-06-07-workers-spec-a-foundation-stateless.md`) — reuses its serializer (`encode`/`decode`/`check_sendable`), isolate bootstrap (`src/worker/isolate.rs`), code-shipping + dependency-closure (`src/worker/dispatch.rs`), cancel/error model, the `worker` contextual keyword + `is_worker` fn-decl flag + `WorkerKw` token/remap in both front-ends, and Plan A's `.aso` bump. Plan A was written in parallel; its public symbols have since been **reconciled and confirmed** in the "Plan-A integration points" section below (`IsolateHandle`, `spawn_isolate`, `WorkerCodeSlice`, `build_code_slice`, `dispatch_worker`, `WorkerKw`, `is_worker`). Both plans use the real CST path `src/syntax/` (not the `src/cst/` named in CLAUDE.md — a stale doc reference to fix in the §8.2 sweep).

---

## Plan-A integration points (resolve these names FIRST, before Task 1)

Before starting, read the merged Plan A and pin these exact symbols (this plan assumes them):

- `src/worker/serialize.rs` (CONFIRMED against merged Plan A): `pub fn encode(&Value) -> Result<Vec<u8>, SendError>`, `pub fn decode(&[u8], &Interp) -> Result<Value, SendError>`, `pub fn check_sendable(&Value) -> Result<(), SendError>`. `SendError` carries a field path and converts into a recoverable Tier-2 `Control::Panic`.
- `src/worker/isolate.rs` (CONFIRMED): the isolate bootstrap (OS thread + `current_thread` runtime + `LocalSet` + fresh `Interp`/`Vm` with `global_env`, on the `WORKER_STACK_SIZE` 512 MB stack). Plan A exposes the per-isolate handle type **`IsolateHandle`** (thread + inbound `Send` byte channel) and spawns isolates from inside `src/worker/pool.rs`. Spec B needs the SAME bootstrap for a *dedicated, non-pooled* isolate, so **Task 4 refactors Plan A's bootstrap into a public `isolate::spawn_isolate(...) -> IsolateHandle`** and has the pool reuse it (no behavior change to Plan A's pool).
- `src/worker/dispatch.rs` (CONFIRMED): Plan A's real symbols are the struct **`WorkerCodeSlice { fn_id, entry_aso, class_name }`** and the builder **`build_code_slice(interp, entry, class_name) -> Result<WorkerCodeSlice, Control>`**, plus the *pooled* dispatch entry **`dispatch_worker(interp, slice, args, span) -> Result<Value, Control>`** (returns a `Value::Future`). Spec B REUSES `WorkerCodeSlice` + `build_code_slice` to ship a **class** code-slice (superclass chain + method table) and a **`worker fn*`** code-slice via the SAME machinery; it does NOT use `dispatch_worker` (that is the pooled request/response path) — actors/streams get their own dedicated-isolate dispatch in `src/worker/actor.rs` / `src/worker/stream.rs` built over `spawn_isolate` + the `IsolateHandle` channel.
- `Value`/`.aso`: Plan A added `is_worker` to the fn proto. Spec B adds `is_worker` to the **class** proto. **If both specs land together, share Plan A's single `ASO_FORMAT_VERSION` bump** (do NOT double-bump); Task 11 bumps only if Plan A has already landed and merged separately.
- Front-end: Plan A added the `worker` contextual keyword (`WorkerKw`), `is_worker` on the fn-decl in both `src/parser.rs` and `src/syntax/parser.rs` (CST), and the tree-sitter `worker` modifier on functions. Spec B extends it to **classes** and to the **`worker fn*`** combination.

---

## File Structure

**New files:**
- `src/worker/actor.rs` — the dedicated-isolate **actor** runtime: spawn an isolate running a class instance; the FIFO mailbox (one inbound message processed to completion before the next); inbound-message dispatch (method name + serialized args → run method in isolate → serialized result back); resource ownership (the instance + its native resources stay in the isolate, never cross); non-reentrancy guard; close/teardown. **Responsibility:** everything actor-side that runs on the dedicated thread.
- `src/worker/actor_handle.rs` — the **caller-side** proxy: the `WorkerActorHandle` `ResourceState` payload (the outbound `Send` channel + abort handle + declared class id/name for `instanceof`/completion), the `ClassName.spawn(args)` host logic, async method-call dispatch (`await handle.method(args)`), and last-drop teardown. **Responsibility:** the `Value::Native` proxy and its method routing. *(May be merged into `actor.rs` if the agent prefers a single file; keep the caller/isolate split conceptually.)*
- `src/worker/stream.rs` — the **streaming generator** runtime: the dedicated-isolate driver behind `GenImpl::Worker`; demand-driven pull (one `.next()` = one demand credit), the bounded prefetch buffer (default 1 = strict pull), the bidirectional `next(v)` injection across the boundary, close/drop teardown. **Responsibility:** the cross-thread generator driver.
- `examples/advanced/workers_actor_counter.as`, `workers_actor_service.as`, `workers_stream_records.as`, `workers_stream_bidirectional.as`, `workers_event_bridge.as`, `workers_actor_subscribe.as` — the runnable corpus (§7.3), doubling as docs + all-modes differential tests.
- `docs/content/language/workers.md` — the workers language-guide page (shared by both specs: model + two lifecycles + actors + streaming + event-bus bridge). *(If Plan A already created this page, EXTEND it instead of creating; Task 13.1 handles either case.)*

**Modified (by responsibility):**
- Front-end syntax: `src/ast.rs` (`is_worker` on `Stmt::Class` + `MethodDecl` Display), `src/parser.rs` (`class_decl` accepts leading `worker`; `fn*` already parsed — combine with `is_worker`), `src/syntax/parser.rs` (CST `class_decl` accepts `worker`), `src/syntax/ast/ascript.ungram` + generated accessors (ClassDecl `worker` token), `tree-sitter-ascript/grammar.js` + regen `parser.c`, `tree-sitter-ascript/queries/{highlights.scm,tags.scm}`.
- Editor surfaces: `editors/vscode/syntaxes/ascript.tmLanguage.json`, `editors/zed/languages/ascript/highlights.scm`, `editors/nvim/queries/ascript/highlights.scm`, `editors/zed/extension.toml` (commit pin), `editors/nvim/lua/ascript/treesitter.lua` (revision pin), `editors/nvim/tests/treesitter_spec.lua`.
- Formatter: `src/syntax/format/` (render `worker class` + `worker fn*`), `src/ast.rs` `Display`.
- Resolver/compiler: `src/syntax/resolve/mod.rs` (an `is_worker_class(node)` helper alongside `is_static_method`), `src/compile/mod.rs` (set `is_worker` on the `ClassProto`; compile a `worker class`/`worker fn*` so the dedicated-isolate machinery is reachable), `src/parser.rs` (legacy `Stmt::Class` field).
- Runtime: `src/value.rs` (`NativeKind::WorkerActor` + `type_name`; the `GenImpl::Worker` is in `coro.rs`), `src/coro.rs` (`GenImpl::Worker` variant + `resume_worker`/`close`), `src/interp.rs` (`ResourceState::WorkerActor`; `ClassName.spawn` routing on `Value::ClassMethod`/class value; actor-handle member dispatch returning `future<T>`; the non-reentrancy guard; `worker fn*` call → `Value::Generator` over `GenImpl::Worker`), `src/vm/run.rs` (mirror the `spawn`/handle-call/`worker fn*`-call routing so the VM matches), `src/worker/mod.rs` (declare `actor`/`actor_handle`/`stream` mods).
- Stdlib: `src/stdlib/task_mod.rs` (the `pipe` bridge helper) + `src/stdlib/mod.rs` (register), `src/check/std_arity.rs` (`pipe` arity).
- Checker/types: `src/check/infer/pass.rs` + `ty.rs` (`spawn` → `future<handle>`; actor method → `future<T>`; `worker fn*` → generator type), optional `src/check/rules/worker_reentrancy.rs` + `rules::ALL`.
- LSP: `src/lsp/providers/{semantic_tokens.rs,symbols.rs,navigation.rs,hover.rs,completion.rs}` + `src/lsp/workspace.rs`.
- `.aso`: `src/vm/aso.rs` (class proto `is_worker` flag), `src/vm/verify.rs`.
- Determinism: `src/det.rs` (`DetEvent` variants for actor-message / generator-yield boundary events), `src/stdlib/workflow.rs` (`workflow-determinism` lint extension).
- Tests: `tests/frontend_conformance.rs`, `tests/treesitter_conformance.rs`, `tests/vm_differential.rs`, `tests/check.rs`, `tests/lsp.rs`, plus a new `tests/workers_stateful.rs` integration file.
- Docs: `docs/content/language/workers.md`, `docs/assets/app.js` (`NAV`), `docs/index.html`, `docs/content/language/modules-async.md`, `README.md`, `CLAUDE.md`, `superpowers/specs/2026-05-29-ascript-design.md`, `superpowers/roadmap.md`.
- Perf: `bench/` (extend Plan A's harness + report).

---

## Task 1: `worker class` + `worker fn*` in both parsers + AST flags

**Files:** `src/ast.rs`, `src/parser.rs`, `src/syntax/parser.rs`, `src/syntax/ast/ascript.ungram` (+ generated accessor), `src/syntax/resolve/mod.rs`, `src/compile/mod.rs`, `tests/frontend_conformance.rs`.

- [x] **Step 1: Failing parse test (both front-ends).** Add to `tests/frontend_conformance.rs` cases that BOTH the legacy parser and the CST parser accept:
  ```
  worker class Db { field conn; init(url) { self.conn = url } fn query(sql) { return sql } }
  worker fn* records(path) { yield path }
  ```
  Assert the legacy `Stmt::Class { is_worker: true, .. }` and the legacy `Stmt::Fn { is_worker: true, is_generator: true, .. }` (the fn-decl `is_worker` is Plan A's; this asserts it combines with `is_generator`). Assert the CST `ClassDecl` has a `WorkerKw` child and the `Stmt::Fn` parses without error. Run `cargo test --test frontend_conformance worker_class` → **expect FAIL** (legacy parser does not accept `worker class`; `Stmt::Class` has no `is_worker` field).

- [x] **Step 2: AST field.** In `src/ast.rs`, add `is_worker: bool` to `Stmt::Class { .. }` (next to `name`/`superclass`). Update every constructor/match of `Stmt::Class` across the codebase (compile errors will list them — `interp.rs`, `compile/mod.rs`, `parser.rs`, `fmt.rs` if any, `lsp`).

- [x] **Step 3: Legacy parser.** In `src/parser.rs`, before `class_decl` is dispatched, accept an optional leading `worker` (Plan A's `WorkerKw`/contextual `Tok::Ident("worker")` check — reuse the exact predicate Plan A added for `worker fn`). Set `is_worker` on the produced `Stmt::Class`. (The `worker fn*` case is already covered — Plan A's `worker fn` path + the existing `*` generator flag combine; verify `fn_decl` reads `*` after `worker fn`.)

- [x] **Step 4: CST parser.** In `src/syntax/parser.rs` `class_decl` (line ~1264), if the cursor is at the `worker` contextual keyword, `p.bump_remap(WorkerKw)` before `p.bump(); // class` (mirror `at_static_method`/`bump_remap(StaticKw)`). Add `worker` to the ungrammar `ClassDecl` node (`src/syntax/ast/ascript.ungram`) so the generated typed accessor exposes the token. Add a resolver helper in `src/syntax/resolve/mod.rs` next to `is_static_method`:
  ```rust
  pub fn is_worker_class(node: &ResolvedNode) -> bool {
      node.children_with_tokens()
          .filter_map(|e| e.into_token())
          .any(|t| t.kind() == SyntaxKind::WorkerKw)
          && node.kind() == SyntaxKind::ClassDecl
  }
  ```

- [x] **Step 5: Compiler reads the flag.** In `src/compile/mod.rs` `compile_class`, set `ClassProto.is_worker = crate::syntax::resolve::is_worker_class(class_decl.syntax())` (the proto field is added in Task 11; for now compile to `false`-default and wire after Task 11, OR add the bool field now and default it). Keep compilation otherwise identical (a `worker class` still compiles its method table — it must, so the code-slice can ship).
  > **Note (Task 1 impl):** `ClassProto.is_worker` does not exist yet — that's Task 11. Per the plan's "for now compile to `false`-default", no change to `compile/mod.rs` was made; `is_worker_class()` is wired here and ready for Task 11 to call it.

- [x] **Step 6:** Run `cargo test --test frontend_conformance worker_class && cargo test --no-default-features --test frontend_conformance worker_class` → **expect PASS**.

- [x] **Step 7: Commit**
  ```bash
  git add src/ast.rs src/parser.rs src/syntax/ tests/frontend_conformance.rs src/compile/mod.rs
  git commit -m "feat(workers): parse 'worker class' + 'worker fn*' in both front-ends (is_worker on class AST)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

## Task 2: Tree-sitter grammar + queries + regen + grammar sync + editor pins + highlights

**Files:** `tree-sitter-ascript/grammar.js`, `tree-sitter-ascript/src/parser.c` (regen), `tree-sitter-ascript/queries/{highlights.scm,tags.scm}`, `editors/vscode/syntaxes/ascript.tmLanguage.json`, `editors/zed/languages/ascript/highlights.scm`, `editors/nvim/queries/ascript/highlights.scm`, `editors/nvim/tests/treesitter_spec.lua`, `editors/zed/extension.toml`, `editors/nvim/lua/ascript/treesitter.lua`, `tests/treesitter_conformance.rs`.

> **If Plan A and Spec B land together, a SINGLE `./scripts/sync-grammar.sh` run + ONE pin bump covers both** — do the grammar work for both, then one sync. If Plan A already synced, this task re-syncs after adding the class `worker`.

- [x] **Step 1: Failing conformance test.** In `tests/treesitter_conformance.rs`, add cases asserting the vendored tree-sitter parser produces an error-free tree for `worker class C { fn f() {} }` and `worker fn* g() { yield 1 }`, and that `worker` appears as a keyword/modifier node. Run `cargo test --test treesitter_conformance worker` → **expect FAIL**.

- [x] **Step 2: Grammar.** In `tree-sitter-ascript/grammar.js` `class_declaration` (line ~234), added `optional($.worker_keyword)` immediately before `'class'`. Confirmed `function_declaration` already has Plan A's `optional($.worker_keyword)` before `optional('async')`, and that `optional('*')` after `'fn'` covers `worker fn*` (the combination needs no new rule — verified). `worker_keyword` is the existing distinct token from Plan A; `worker` remains contextual (the new test confirms `let worker = 5` / `fn worker() {}` / `f(worker)` still parse without ERROR). No new GLR conflicts introduced.

- [x] **Step 3: Regen.** `cd tree-sitter-ascript && tree-sitter generate --abi 14` regenerated `src/parser.c`. Verified via `cargo build` (clean build, no warnings).

- [x] **Step 4: Queries.** `tree-sitter-ascript/queries/highlights.scm` already has `(worker_keyword) @keyword` (Plan A), which captures `worker` on both function and class declarations — no change needed. `tags.scm` uses `(class_declaration name: (identifier) @name) @definition.class` which matches `worker class` (the leading optional modifier does not affect the structural match) — verified; added a comment in tags.scm confirming this.

- [x] **Step 5: Editor highlight copies.** Verified all three editor surfaces already cover `worker`:
  - `editors/vscode/syntaxes/ascript.tmLanguage.json`: `\b(static|worker|async)\b` storage-modifier pattern (Plan A) — colors `worker` before `class` correctly.
  - `editors/zed/languages/ascript/highlights.scm` and `editors/nvim/queries/ascript/highlights.scm`: both already have `(worker_keyword) @keyword` — no changes needed.
  - `editors/nvim/tests/treesitter_spec.lua`: does not assert on keyword tokens; no change needed.

- [x] **Step 6:** `cargo test --test treesitter_conformance` → 8/8 PASS (both new cases + existing). Full `cargo test` → all green. `cargo test --no-default-features` → all green. Both clippy configs → zero warnings.

- [x] **Step 7: Sync + pin bump (DONE-ON-RELEASE fallback).** `./scripts/sync-grammar.sh` requires push credentials to `git@github.com:ascript-lang/tree-sitter-ascript.git` (not available in this environment). Grammar sync + SHA pin bump in `editors/zed/extension.toml` (`rev`) and `editors/nvim/lua/ascript/treesitter.lua` (`GRAMMAR_REV`) are deferred to release time, consistent with Plan A Task 3's handling. The vendored `tree-sitter-ascript/src/parser.c` is updated in the monorepo and builds correctly; editors will receive the update when the grammar mirror is synced.

- [x] **Step 8: Commit**
  ```bash
  git add tree-sitter-ascript/ editors/ tests/treesitter_conformance.rs
  git commit -m "feat(grammar): 'worker class' + 'worker fn*' in tree-sitter + queries + editor highlights; sync + pin bump

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

## Task 3: Formatter + Display

**Files:** `src/syntax/format/` (the lossless CST formatter), `src/ast.rs` `Display`, formatter golden tests.

- [x] **Step 1: Failing formatter golden.** Add a golden asserting idempotent formatting:
  - Input `worker  class  Db{fn f(){return 1}}` → `worker class Db {\n  fn f() {\n    return 1\n  }\n}`.
  - Input `worker fn*  g(){yield 1}` → `worker fn* g() {\n  yield 1\n}`.
  - Canonical modifier order: `static? worker? fn` for methods (a `static worker fn` is NOT a valid combination per Spec A — `worker` instance methods are Spec B's `worker class` methods, and `static worker fn` is Plan A's pooled form; the formatter just preserves whatever modifiers exist in canonical order). Tests were added and confirmed failing before the fix.

- [x] **Step 2: Formatter.** Fixed `src/syntax/format/mod.rs` (the CST formatter used by `ascript fmt` CLI) in three places: `fn_decl` (emits `worker ` before `async`), `class_decl` (emits `worker ` before `class`), and `member`/MethodDecl (emits `worker ` in canonical `static? worker? async? fn` order). The `WorkerKw` token is correctly remapped by the CST parser and detected by `toks.contains(&WorkerKw)`. The legacy AST formatter (`src/fmt.rs`) was already correct from Task 1.

- [x] **Step 3: `ast.rs` Display.** Confirmed there is no `Display for Stmt` — `src/fmt.rs` is the canonical printer (as established in Plan A). The `Stmt::Class` arm in `src/fmt.rs` already renders `worker ` when `is_worker` (Task 1). `Stmt::Fn` and `write_method` also already handle `worker fn*` correctly. No ast.rs changes needed.

- [x] **Step 4:** Both test configs green (35 fmt.rs tests, 25 syntax::format tests, all 1914+ lib tests). Both clippy configs → zero warnings. Worker example files: `worker fn`/`worker class` keywords preserved after formatting (pre-existing style divergences in examples are unrelated to worker keyword handling).

- [x] **Step 5: Commit** — `f6ca48d feat(fmt): render 'worker class' and 'worker fn*' (CST formatter + ast Display, idempotent)`

## Task 4: Dedicated-isolate lifecycle/manager (reuse Plan A bootstrap)

**Files:** `src/worker/mod.rs`, `src/worker/isolate.rs` (extend), `src/worker/actor.rs` (new, skeleton), `src/worker/stream.rs` (new, skeleton), unit tests inline.

- [x] **Step 1: Failing unit test.** Added `worker::isolate::tests::spawn_isolate_runs_loop_and_drops_cleanly`: spawns a dedicated isolate, ships a value as bytes over the inbound channel, the run-loop decodes it against the isolate's OWN `Interp` (proving shared-nothing), doubles it, replies over a `Send` back-channel; then dropping the `IsolateHandle` closes the channel, ends the run-loop, and joins the thread (a `loop_ended` flag confirms no zombie). Confirmed FAIL before the spawner existed.

- [x] **Step 2: Public dedicated spawner.** `src/worker/isolate.rs` now exposes `pub(crate) fn spawn_isolate<F, Fut>(make_loop) -> io::Result<IsolateHandle>`. Refactored the Plan A bootstrap into a shared private `bootstrap(make_loop)` (OS thread + `ISOLATE_STACK_SIZE` stack) + `run_isolate_thread` (own `current_thread` runtime + `LocalSet` + fresh `Interp`/`Vm` via `install_self`/`Vm::new`). The pooled `Isolate::spawn` now calls the SAME `bootstrap` (its run-loop is `isolate_loop(vm, rx)`); `isolate_loop` takes the bootstrap-built `Vm` instead of constructing its own — behavior-identical (verified: all 11 Plan A `cli worker` tests pass 3×, vm_differential green). NOTE: matched the existing pooled isolate's `ISOLATE_STACK_SIZE` (8 MiB + `stacker::maybe_grow`), NOT the 512 MiB `WORKER_STACK_SIZE` the plan text mentions — that 512 MiB was deliberately replaced in Plan A (its comment explains the address-space-pressure reason); the dedicated isolate reuses the exact same proven bootstrap.

- [x] **Step 3: `IsolateHandle`.** Defined: `tx: mpsc::UnboundedSender<Vec<u8>>` (inbound `Send` byte channel) + an `Option<JoinHandle<()>>`, with a `Drop` impl that drops the live sender (closes the channel → the run-loop's `recv().await` returns `None` → body ends) and joins the thread (no zombie). Cancel-on-drop / clean teardown — no deadlock because the channel close is what ends the loop before the join. Skeleton `actor.rs`/`stream.rs` re-export `spawn_isolate`/`IsolateHandle`.

- [x] **Step 4: Declare mods.** `src/worker/mod.rs` now declares `pub mod actor;` and `pub mod stream;` (folded `actor_handle` into `actor` per the plan's "may be merged" note; the caller/isolate split stays conceptual for Task 5). Matched the existing `pub mod` visibility of the other worker submodules.

- [x] **Step 5:** Isolate unit test passes in BOTH feature configs (3× each, stable). `cargo clippy --all-targets` AND `--no-default-features --all-targets` both ZERO warnings. No `await_holding_refcell_ref` risk: the substrate holds no `RefCell` borrow across `.await` (the dedicated isolate builds its own runtime types; only `Send` bytes cross).

- [x] **Step 6: Commit**
  ```bash
  git add src/worker/
  git commit -m "feat(workers): dedicated (non-pooled) isolate lifecycle reusing Plan A bootstrap

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

## Task 5: Actor handle — `ClassName.spawn(args)` → `future<handle>`, FIFO mailbox, async method dispatch, non-reentrancy, teardown

**Files:** `src/value.rs` (`NativeKind::WorkerActor`), `src/interp.rs` (`ResourceState::WorkerActor`, `spawn` routing, handle member dispatch, non-reentrancy guard), `src/worker/actor.rs`, `src/worker/actor_handle.rs`, `src/vm/run.rs` (mirror), `tests/workers_stateful.rs`.

- [x] **Step 1: Failing behavioral test (actor state persists).** New `tests/workers_stateful.rs`, spawning the built binary on a `.as` program:
  ```
  worker class Counter {
    field n = 0
    fn inc() { self.n = self.n + 1; return self.n }
    fn get() { return self.n }
  }
  let c = await Counter.spawn()
  print(await c.inc())   // 1
  print(await c.inc())   // 2
  print(await c.get())   // 2
  c.close()
  ```
  Assert stdout `1\n2\n2\n`. Run `cargo test --test workers_stateful actor_counter` → **expect FAIL** (`spawn` unknown).

- [x] **Step 2: `NativeKind::WorkerActor` + `ResourceState::WorkerActor`.** Add `WorkerActor` to `NativeKind` (`src/value.rs`) with `type_name() => "workerActor"` and a fields entry (e.g. the declared class name, readable but not the actor's state). Add `ResourceState::WorkerActor(Box<WorkerActorHandle>)` in `src/interp.rs` (boxed for `large_enum_variant`). `WorkerActorHandle` (in `actor_handle.rs`) holds: the outbound `Send` mpsc sender of `ActorMsg`, the `IsolateHandle` (its `Drop` tears down — last-handle-drop teardown is automatic), the declared class id/name (for `instanceof` + completion), and a `Cell<bool> in_call` re-entrancy flag.

- [x] **Step 3: `ClassName.spawn(args)` routing.** In `src/interp.rs`, intercept a call whose callee is `Member { object: <class value>, name: "spawn" }` (the same call-site-hook style `std/schema` uses) when the class value's decl `is_worker`. The handler:
  1. `check_sendable` each arg (Plan A serializer; field-path panic on failure).
  2. `dispatch::ensure_loaded` the class code-slice (superclass chain + method table) to a freshly `spawn_isolate`'d dedicated isolate.
  3. Send an `ActorMsg::Init { class_id, args: encode(args) }`; the isolate decodes args, constructs the instance via `init` IN the isolate (so `init`'s resource opens stay in the isolate), and acks.
  4. Register a `ResourceState::WorkerActor` in `Interp.resources` (`next_resource_id`), return `future<Value::Native(WorkerActor)>` (spawning is async). A bare `ClassName(args)` is UNCHANGED (still a local instance — no overloading of construction).
  Mirror the routing in `src/vm/run.rs` so the VM matches the tree-walker.

- [x] **Step 4: FIFO mailbox, one-at-a-time.** In `src/worker/actor.rs`, the isolate's main loop awaits inbound `ActorMsg::Call { method, args, reply }` from the mpsc receiver and processes **each to completion before receiving the next** (a plain `while let Some(msg) = rx.recv().await` loop — single-consumer = serialized = no internal locks, the GenServer guarantee). Each call: `decode` args → look up + run the method on the in-isolate instance (the method body may `await` its own I/O) → `encode` the return value (or the panic message) → send back over `reply` (`oneshot`). A `[value, err]` Result crosses as ordinary data (Plan A). An uncaught Tier-2 panic re-raises as a recoverable panic on the caller (Plan A error model).

- [x] **Step 5: Async method dispatch on the handle.** In `src/interp.rs`, member access on a `Value::Native(WorkerActor)` returns a bound `Value::NativeMethod`-style callable (mirror the events/task native-method dispatch pattern at `src/interp.rs:3254`/`3346`). Calling it: `check_sendable(args)` → send `ActorMsg::Call` → return a `future<T>` that awaits the `oneshot` reply and `decode`s it. **Take the channel sender out across the `.await` (clone the `Sender`, which is cheap), never hold a `resources` borrow across the await** (take-out-across-await pattern). Mirror in `src/vm/run.rs`.

- [x] **Step 6: Non-reentrancy guard.** When dispatching a method call, set `in_call=true` for the duration on the handle and detect a same-handle call delivered while the mailbox is already processing a message *from this same caller-side handle re-entering itself*. Spec: "an actor method that calls back into *its own* handle would deadlock its own mailbox" → recoverable Tier-2 panic with a clear message (e.g. `actor method re-entered its own handle (actors are non-reentrant); call a different actor or restructure`). Implement by tagging each in-flight message with the originating handle id and, on the isolate side, if a call arrives whose origin handle is the one currently being serviced, reply with a recoverable panic instead of enqueueing (which would deadlock the one-at-a-time mailbox). Add a failing test first:
  ```
  worker class A { fn ping(self_handle) { return self_handle.ping(self_handle) } }  // illustrative; the real test reproduces a self-call via a stored handle
  ```
  (Design the test to the actual mechanism: the cleanest reproduction is an actor method that `await`s a method on *its own* proxy passed back in — but proxies aren't sendable, so the realistic reproduction is the runtime detecting a queued message whose origin == the currently-serviced origin. Write the test to whatever the implementation surfaces, asserting `recover` catches a panic whose message mentions non-reentrancy.)

- [x] **Step 7: close() + last-drop teardown + closed-actor calls.** Add a `close` method on the handle (`handle.close()`) that drops the `IsolateHandle` (teardown). Dropping the last `Value::Native(WorkerActor)` (GC/`Rc` last-drop) reclaims the `ResourceState` and thus the isolate (cancel-on-drop). An in-flight or new call on a closed actor resolves to a **recoverable** panic (the `oneshot` reply channel is dropped → map the recv error to a recoverable `Control::Panic` "actor is closed"). Add tests for `close()`, last-drop, and closed-call-panic.

- [x] **Step 8: GC invariant.** `ResourceState::WorkerActor` is a native handle — the GC must NOT trace into it (it holds `Send` channels + a thread handle, not script `Value`s reachable for cycles). Confirm no `Value::trace` arm reaches into it (native handles are `Rc` with no-op `Trace` per CLAUDE.md). Add a one-line code comment asserting this.

- [x] **Step 9: Resource-ownership test.** `tests/workers_stateful.rs`: an actor whose `init` opens a MOCK native resource (use a real but local one, e.g. an in-memory sqlite via `sql.open(":memory:")` under the `sql` feature, gated `#[cfg]`) and a method that queries it; assert the resource works and that attempting to RETURN the raw resource handle from a method is a sendability panic with a field path (it can't cross — methods must return data). This is the canonical "resource lives in the actor" pattern.

- [x] **Step 10:** Run `cargo test --test workers_stateful` (full) in both feature configs → **expect PASS**. `cargo clippy --all-targets` clean in both configs.

- [x] **Step 11: Commit**
  ```bash
  git add src/value.rs src/interp.rs src/worker/ src/vm/run.rs tests/workers_stateful.rs
  git commit -m "feat(workers): worker class actors — spawn, proxy handle, FIFO mailbox, async methods, non-reentrancy, teardown

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

## Task 6: `worker fn*` streaming generator — dedicated isolate, demand-driven pull, bounded buffer, bidirectional `next(v)`, close/drop

**Files:** `src/coro.rs` (`GenImpl::Worker` variant + `resume_worker`/`close`), `src/worker/stream.rs`, `src/interp.rs` (`worker fn*` call → `Value::Generator`), `src/vm/run.rs` (mirror), `tests/workers_stateful.rs`.

- [x] **Step 1: Failing behavioral test (ordered yield).** Add:
  ```
  worker fn* records(n) { for i in 1..=n { yield i * 10 } }
  for await r in records(3) { print(r) }   // 10 20 30
  ```
  Assert stdout `10\n20\n30\n`. Run `cargo test --test workers_stateful stream_records` → **expect FAIL** (calling a `worker fn*` runs locally / errors).

- [x] **Step 2: `GenImpl::Worker` variant in `src/coro.rs`.** Add a third variant alongside `Body` (tree-walker) and `Vm`:
  ```rust
  /// Cross-thread streaming generator (`worker fn*`, Spec B). The producer body runs
  /// in a DEDICATED isolate; this side is a demand-driven driver. `resume(input)`
  /// sends one demand credit (carrying `input` as the value the producer's `yield`
  /// expression returns — bidirectional) over the channel and awaits the next
  /// serialized yield, or done/error. A bounded prefetch buffer lets the producer
  /// run ahead at most `prefetch` steps (default 1 = strict pull = backpressure).
  Worker {
      driver: RefCell<Option<Box<crate::worker::stream::StreamDriver>>>,
      done: Cell<bool>,
  }
  ```
  Add `GeneratorHandle::new_worker(driver) -> Self`. Extend `resume` dispatch (line ~150) with a `GenImpl::Worker { .. } => self.resume_worker(input).await` arm and `close()` with a `Worker` arm (drop the driver = teardown). Follow the existing **take-out-across-await** discipline used by `resume_vm` (move the driver out of the `RefCell<Option<..>>` before the `.await`, put it back if still pending) so `await_holding_refcell_ref` stays clean.

- [x] **Step 3: `StreamDriver` in `src/worker/stream.rs`.** Holds an `IsolateHandle` (dedicated isolate running the `worker fn*` body), a demand channel (`mpsc` of `Resume{ input: encode(v) }`), a yield channel (`mpsc`/bounded of `encode(yielded)` / `Done` / `Err(msg)`), and the prefetch window (default 1). `resume_worker(input)`:
  1. If `done`, return `Ok(None)`.
  2. Send a demand credit with `encode(input)` (first resume ignores `input`, matching the tree-walker first-`next` semantics — mirror `resume_vm`'s `started` handling).
  3. Await the next yield-channel message: `Yielded(bytes)` → `decode` → `Ok(Some(v))`; `Done` → set `done`, `Ok(None)`; `Err(msg)` → recoverable `Control::Panic`.
  The producer-side isolate runs the `worker fn*` body driving its *local* generator, blocking (parking) when the bounded buffer is full = backpressure (prefetch=1 means it produces exactly one ahead of demand). A yielded value is `check_sendable` + `encode`d before crossing; non-sendable yield → field-path panic.

- [x] **Step 4: `worker fn*` call → `Value::Generator`.** In `src/interp.rs`, when calling a function whose decl is `is_worker && is_generator`, instead of building a local `GeneratorHandle` (the `Value::Generator(Rc::new(GeneratorHandle::new(..)))` path at ~3710), `check_sendable` the args, `spawn_isolate` + `dispatch::ensure_loaded` the `worker fn*` code-slice, build a `StreamDriver`, and return `Value::Generator(Rc::new(GeneratorHandle::new_worker(driver)))`. `for await` / `.next(v)` / `.close()` already dispatch through `GeneratorHandle::resume`/`close` (Task 2 added the `Worker` arm) — **transparent to user code**. Mirror in `src/vm/run.rs` (the VM's `worker fn*` call constructs the same `GenImpl::Worker` generator rather than a `GenImpl::Vm` fiber).

- [x] **Step 5: Backpressure test.** Add a test with an INSTRUMENTED producer (a `worker fn*` that, e.g., prints/records each production) consumed one element at a time with an artificial delay between `.next()` calls; assert the producer does not run more than `prefetch` (=1) ahead of demand. Because output ordering across threads is the assertion target, drive it deterministically (sequential `.next()` with awaits) and assert the *count* of productions matches the count of resumes (+1 for prefetch), not wall-clock.

- [x] **Step 6: Bidirectional `next(v)` test.** Add:
  ```
  worker fn* echo() { let a = yield 1; let b = yield a + 100; yield b + 1000 }
  let g = echo()
  print(await g.next())     // {value:1,...} → print just the value per the existing next() shape
  print(await g.next(5))    // 105
  print(await g.next(7))    // 1007
  ```
  Match the EXISTING `gen.next(v)` return shape (see `src/interp.rs:3346` `dispatch_generator_method`). Assert the round-trip. (The injected `v` is `encode`d across the boundary and becomes the producer's `yield` expression result.)

- [x] **Step 7: close/drop teardown test.** Consume one value, then `g.close()` (or drop the generator); assert a subsequent `resume` returns done and the isolate is reclaimed (no zombie thread — the test process exits cleanly). Mirror `coro.rs`'s `abandoning_after_one_value_drops_cleanly` style.

- [x] **Step 8:** Run `cargo test --test workers_stateful stream` in both feature configs → **expect PASS**. Clippy clean both configs.

- [x] **Step 9: Commit**
  ```bash
  git add src/coro.rs src/worker/stream.rs src/interp.rs src/vm/run.rs tests/workers_stateful.rs
  git commit -m "feat(workers): worker fn* streaming generators — dedicated isolate, demand-driven pull, bounded buffer, bidirectional next(v)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

## Task 7: The `pipe(gen, bus)` bridge helper + std_arity

**Files:** `src/stdlib/task_mod.rs`, `src/stdlib/mod.rs`, `src/check/std_arity.rs`, `tests/workers_stateful.rs`.

- [x] **Step 1: Failing test (bridge fans out in order).** Add a `.as` test:
  ```
  worker fn* source() { yield {kind:"a", n:1}; yield {kind:"a", n:2} }
  let bus = events.new()
  let seen = []
  bus.on("a", fn(e) { seen = [...seen, e.n] })
  await task.pipe(source(), bus)
  print(seen)   // [1, 2]
  ```
  Assert `[1, 2]`. Run `cargo test --test workers_stateful bridge` → **expect FAIL** (`task.pipe` unknown).

- [x] **Step 2: `pipe`.** In `src/stdlib/task_mod.rs`, add an async `pipe(gen, bus)` to `exports()` (`("pipe", bi("task.pipe"))`) and the dispatch match. Implementation (exactly the spec's idiom):
  ```rust
  async fn task_pipe(&self, args: &[Value], span: Span) -> Result<Value, Control> {
      let gen = want_generator(&arg(args, 0), span, "task.pipe")?;
      let bus = arg(args, 1); // an events emitter (Native)
      loop {
          match gen.resume(Value::Nil).await? {
              Some(e) => {
                  // e.kind drives the channel; emit on the LOCAL bus (intra-isolate fan-out)
                  let kind = /* read e.kind as a string */;
                  self.call_native_method(&bus, "emit", &[Value::Str(kind), e], span).await?;
              }
              None => break,
          }
      }
      Ok(Value::Nil)
  }
  ```
  Backpressure threads end-to-end for free: a slow local `on` listener slows `emit`, which slows the loop, which slows `resume`, which slows the producer. Keep it engine-agnostic (`Value` layer) — both engines reuse it.

- [x] **Step 3: std_arity.** In `src/check/std_arity.rs`, register `("task", "pipe") => required 2` so `call-arity` checks it (`max=None` per native-fn convention). Confirm the `every_entry_is_a_real_export` test still passes (it asserts every arity entry is a real export).

- [x] **Step 4: Multi-listener + slow-listener backpressure test.** Extend the test: two `on("a", ...)` listeners both observe both events in order; a deliberately slow listener (awaits a short sleep) still receives all events and the final `seen` is complete and ordered.

- [x] **Step 5:** Run `cargo test --test workers_stateful bridge` + `cargo test --test check std_arity` in both configs → **expect PASS**.

- [x] **Step 6: Commit**
  ```bash
  git commit -am "feat(workers): task.pipe bridge (worker stream -> local event bus) + std_arity

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

## Task 8: Checker type inference + optional `worker-reentrancy` lint

**Files:** `src/check/infer/pass.rs`, `src/check/infer/ty.rs`, optionally `src/check/rules/worker_reentrancy.rs` + `src/check/rules/mod.rs` (`rules::ALL`), `tests/check.rs`.

- [x] **Step 1: Failing inference tests.** In `tests/check.rs` (or infer unit tests), assert via hover/synth:
  - `ClassName.spawn(args)` on a `worker class` synthesizes `future<handle>` (a class-handle type — reuse the constructor-call → instance-type path at `pass.rs:1047`, wrapped in `CheckTy::Future`). Concretely the synthesized type displays as `future<Db handle>` (or `future<Db>` if a distinct handle type is too costly — match the LSP hover requirement in Task 9).
  - An actor-method call on a handle synthesizes `future<T>` where `T` is the method's declared/inferred return type.
  - A `worker fn*` call synthesizes the SAME generator type as a local generator (the streaming handle is surface-identical).
  - **Invariant:** `examples/**` (incl. the new Task-13 examples) emit ZERO `type-*` diagnostics in BOTH feature configs (default to `Unknown` for anything uncertain — never relax the gate). Run `cargo test --test check infer_worker` → **expect FAIL**.

- [x] **Step 2: Inference.** In `src/check/infer/pass.rs` `synth_call`: detect `Member{object, name:"spawn"}` where `object` resolves to a `worker class` → `CheckTy::Future(Box::new(<class instance/handle type>))`. Detect a method call on a value whose type is that handle → `CheckTy::Future(Box::new(<method ret>))` (mirror the async-fn-call → `future<R>` logic at `pass.rs:1070`). Detect a call to a `worker fn*` → the generator type (reuse the existing generator-call synthesis; the `is_worker` flag changes nothing in the TYPE, only the runtime). Where the class/method is not statically known, return `Unknown` (gradual escape).

- [x] **Step 3: Re-verify the zero-diagnostic invariant.** Run `cargo run -- check` over every `examples/**` worker file in both configs; assert zero `type-*`. If any appears, fix `assignable`/`synth` to default to `Unknown` — DO NOT relax the gate (CLAUDE.md SP10 invariant 1).

- [x] **Step 4: Optional `worker-reentrancy` lint (additive, default Warning).** DOCUMENTED SKIP: the lint would require tracking whether a binding in a worker-class method body is statically known to be a proxy handle for the currently-executing actor instance — information not available in the static checker without a full alias-analysis pass. Any heuristic approximation would produce false positives on valid code that stores a handle in a field and retrieves it in a method body. The runtime Task-5 non-reentrancy guard provides the real safety guarantee; a noisy static lint would violate the zero-FP SP10 invariant. Deferred per the plan's documented-skip option.

- [x] **Step 5:** Run `cargo test --test check` in both configs → **expect PASS**.

- [x] **Step 6: Commit**
  ```bash
  git commit -am "feat(check): infer future<handle> for spawn, future<T> for actor methods, generator for worker fn*; optional worker-reentrancy lint

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

## Task 9: LSP — tokens, symbols, hover, navigation, completion

**Files:** `src/lsp/providers/{semantic_tokens.rs,symbols.rs,navigation.rs,hover.rs,completion.rs}`, `src/lsp/workspace.rs`, `tests/lsp.rs`.

- [x] **Step 1: Failing LSP tests.** In `tests/lsp.rs`, assert:
  - Semantic tokens: `worker` on a class and on a `fn*` is emitted as a keyword/modifier token.
  - Document/workspace symbols: a `worker class` appears in the outline as a class, and its methods are listed/navigable (reuse the existing class-symbol path — a `worker class` is still a `ClassDecl`).
  - Hover: hovering `Db.spawn` shows it returns `future<Db handle>`; hovering an actor-handle method shows `future<T>`; hovering a `worker fn*` shows the streaming/generator type.
  - Navigation: go-to-def / find-references / rename across a `worker class`, its methods, and a `worker fn*` decl.
  - Completion: `worker` is offered before `class`/`fn`; an actor handle's methods are offered after `.` (resolved from the handle's declared class).
  Run `cargo test --test lsp worker` → **expect FAIL**.

- [x] **Step 2: Semantic tokens.** In `semantic_tokens.rs`, ensure the `WorkerKw` token (on `ClassDecl` and on the fn/method decl) maps to the keyword/modifier token type (Plan A added the fn case; add the class case — likely just confirming the token walk covers `ClassDecl`'s leading token). Implemented via `contextual_keyword_spans`: cross-references the CST for `WorkerKw`/`StaticKw` byte positions and intercepts them as `TYPE_KEYWORD` in `classify_one` (raw lexer emits `Ident` for `worker`; CST remaps it).

- [x] **Step 3: Symbols + navigation.** Confirm `src/lsp/providers/symbols.rs` and `navigation.rs` + `src/lsp/workspace.rs` index a `worker class` as a class with its methods (a `worker class` is a `ClassDecl` with an extra token — the existing index should cover it once the parser sets the token; add the `worker fn*` decl to the fn index the same way). Added unit tests confirming both `worker class Counter` → `DefKind::Class` and `worker fn* stream` → `DefKind::Fn` in the workspace index (fixed test fixture to use valid CST-parser syntax).

- [x] **Step 4: Hover.** In `docs.rs` (`decl_doc`), surface `worker class` annotation for `ClassName` hover (mentions "worker class — stateful actor; call `ClassName.spawn()` to get a handle."). `future<handle>` / `future<T>` hover types are driven by Task 8's inference already wired in `hover.rs`. Added `class_decl_is_worker` helper and unit tests.

- [x] **Step 5: Completion.** `worker` completion was already wired in Plan A (Task 12). The existing `lsp_offers_worker_completion` test confirms `worker` appears as a keyword snippet in completion items.

- [x] **Step 6:** Run `cargo test --test lsp` in both configs → **expect PASS**. 15/15 LSP integration tests green; 1932 lib unit tests green; 0 clippy warnings in both feature configs.

- [x] **Step 7: Commit**
  ```bash
  git commit -am "feat(lsp): worker class/fn* tokens, symbols, hover (future<handle>/future<T>), nav, handle-method completion

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

## Task 10: REPL regression

**Files:** `tests/cli.rs` (or `tests/workers_stateful.rs`), no `src/repl.rs` change expected.

- [x] **Step 1: Failing/confirming REPL test.** Add a REPL integration test piping a multi-line `worker class` and a `worker fn*` definition into `ascript repl` and then using them across lines (e.g. define `worker class Counter {...}` on lines 1-4, then `let c = await Counter.spawn()` then `print(await c.inc())` on later lines). Assert the brace-delimited definitions buffer correctly via the existing `is_incomplete` token-depth logic and that the session `Vm`/`Interp` persists the `worker class` definition across lines. Run → **expect PASS** (if it fails, the `worker` token confuses depth counting — fix `is_incomplete` to treat `worker` as a non-delimiter, but brace counting should already handle it).

- [x] **Step 2: Commit**
  ```bash
  git commit -am "test(repl): worker class / worker fn* multi-line entry + cross-line persistence regression

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

## Task 11: `.aso` bump + verify

**Files:** `src/vm/aso.rs`, `src/vm/verify.rs`, `src/vm/chunk.rs` (`ClassProto.is_worker`).

> **If Plan A landed in the same campaign and already bumped `ASO_FORMAT_VERSION` for the fn-proto `is_worker`, SHARE that bump** — add the class flag under the same version. Only bump again if Plan A merged separately and is already at a released version.

- [x] **Step 1: Failing round-trip test.** In `src/vm/aso.rs` tests, add a `worker class` to the round-trip corpus and assert `ClassProto.is_worker` survives `write` → `read`. Assert `ASO_FORMAT_VERSION` is bumped (or shared with Plan A's). Run `cargo test --test ... aso worker` (or the inline aso tests) → **expect FAIL**.

- [x] **Step 2: Class proto flag.** Add `pub is_worker: bool` to `ClassProto` (`src/vm/chunk.rs`). In `src/vm/aso.rs`, serialize it (a flag byte/bit in the class-proto layout, mirroring how `write_proto`/`read_proto` pack `is_async`/`is_generator` at line ~728). Bump `ASO_FORMAT_VERSION` from 15 → 16 (or share Plan A's bump). Update `src/vm/verify.rs` if it validates class-proto layout.

- [x] **Step 3:** Run the aso tests + `cargo test --test vm_limits` (verify trips) in both configs → **expect PASS**.

- [x] **Step 4: Commit**
  ```bash
  git commit -am "feat(aso): is_worker on ClassProto; bump ASO_FORMAT_VERSION; verify update

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

## Task 12: Determinism event-sourcing + `workflow-determinism` lint extension

**Files:** `src/det.rs`, `src/stdlib/workflow.rs`, `src/check/rules/` (the `workflow-determinism` lint), tests inline.

- [x] **Step 1: Failing det test.** Assert that, under a `DeterminismContext` (Record then Replay), an actor's inbound message sequence + results and a generator's yield/resume sequence are recorded as boundary `DetEvent`s and replayed deterministically (the replay returns the recorded result without re-crossing the isolate boundary, matching the SP9 model where the `None` branch is inert and `Some` routes through recorded events). Run → **expect FAIL**.

- [x] **Step 2: Boundary events.** In `src/det.rs`, add `DetEvent` variants (e.g. `ActorCall { method, result }`, `GeneratorYield { value }`) recording each cross-isolate interaction. In the actor/stream dispatch (Tasks 5/6), when `self.determinism` is `Some`, record on Record and return the recorded result on Replay (clone the cell out, never hold across `.await` — SP9 invariant). When `None` (default), the path is the exact pre-Spec-B behavior (byte-identical).

- [x] **Step 3: Lint extension.** Extend the `workflow-determinism` lint (`src/stdlib/workflow.rs` / the rule) to flag UNRECORDED cross-isolate interaction (an actor call / `worker fn*` consumption) inside a `workflow.run` body that isn't event-sourced. Add a `tests/check.rs` case.

- [x] **Step 4:** Run `cargo test --test check workflow_determinism` + the det tests in both configs → **expect PASS**.

- [x] **Step 5: Commit**
  ```bash
  git commit -am "feat(det): event-source actor messages + generator yields; extend workflow-determinism lint

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```
: Example corpus (§7.3) — runnable, doubles as docs & all-modes tests

**Files:** `examples/advanced/workers_actor_counter.as`, `workers_actor_service.as`, `workers_stream_records.as`, `workers_stream_bidirectional.as`, `workers_event_bridge.as`, `workers_actor_subscribe.as`.

> Each example MUST be order-deterministic (drive actors with sequenced awaits; consume generators in order) so the Task-14 byte-identical comparison is meaningful. Verify each with `target/release/ascript run <file>` AND `cargo run -- fmt --check <file>` (idempotent formatting).

- [x] **Step 1: `workers_actor_counter.as`** — a stateful counter/cache actor; state persists across calls. Full content:
  ```
  worker class Counter {
    field n = 0
    field cache = {}
    fn inc() { self.n = self.n + 1; return self.n }
    fn remember(k, v) { self.cache = {...self.cache, [k]: v}; return self.cache[k] }
    fn lookup(k) { return self.cache[k] }
  }

  async fn main() {
    let c = await Counter.spawn()
    print(await c.inc())            // 1
    print(await c.inc())            // 2
    print(await c.remember("x", 42)) // 42
    print(await c.lookup("x"))       // 42
    print(await c.inc())            // 3 (state persisted)
    c.close()
  }
  await main()
  ```
  Expected stdout: `1\n2\n42\n42\n3\n`.

- [x] **Step 2: `workers_actor_service.as`** — a service actor owning a MOCK connection opened INSIDE the isolate, fully error-handled (the "resource lives in the actor" pattern). Use a self-contained mock (no external service): the `init` builds an in-isolate state object standing in for a connection; methods query it; demonstrate `[value, err]` Result crossing as data and `recover` on a method panic. Keep output deterministic.

- [x] **Step 3: `workers_stream_records.as`** — `worker fn*` streaming parsed records with demand-driven backpressure, consumed via `for await`:
  ```
  worker fn* records(n) {
    for i in 1..=n { yield { id: i, label: "rec-" + string(i) } }
  }
  async fn main() {
    for await r in records(4) { print(r.id + ":" + r.label) }
  }
  await main()
  ```
  Expected: `1:rec-1\n2:rec-2\n3:rec-3\n4:rec-4\n`.

- [x] **Step 4: `workers_stream_bidirectional.as`** — `gen.next(v)` injecting values back into the producer (round-trip across the boundary). Deterministic.

- [x] **Step 5: `workers_event_bridge.as`** — the bridge: a `worker fn*` event source piped onto a LOCAL `events` bus that fans out to multiple listeners, via `task.pipe`. Deterministic ordered output.

- [x] **Step 6: `workers_actor_subscribe.as`** — an actor exposing a `fn*` `subscribe` method (a producer actor); consume it in order. *(DEFERRED: actor method returning a generator cannot cross the isolate boundary — a generator handle is non-sendable. Workaround: actor.snapshot() returns a plain array; a separate `worker fn* subscribe(entries)` streams from it — gives identical observable semantics. Documented in the example file header.)*

- [x] **Step 7:** Build release (`cargo build --release`) and run each: `for f in examples/advanced/workers_actor_counter.as examples/advanced/workers_*.as; do target/release/ascript run "$f"; done` → **expect each runs with the documented output**. Also run each under `--no-default-features` build (skip `sql`-gated bits behind `#[cfg]` if used).

- [x] **Step 8: Commit**
  ```bash
  git add examples/advanced/workers_*.as
  git commit -m "docs(examples): stateful-worker corpus — actors, streaming, bidirectional, event bridge, subscribe

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

## Task 14: All-modes execution in `vm_differential.rs` (tree-walker / specialized / generic / `.aso`)

**Files:** `tests/vm_differential.rs`.

- [x] **Step 1: Failing all-modes test.** Extend `tests/vm_differential.rs` so every Task-13 worker example runs in all FOUR modes with byte-identical, order-deterministic output:
  1. Tree-walker (`--tree-walker` / `ASCRIPT_ENGINE=tree-walker`).
  2. Specialized VM (default).
  3. Generic VM (`--no-specialize` / `Vm::new_generic`).
  4. `.aso`-compiled (`ascript build file.as -o file.aso` → `ascript run file.aso`) — proves worker class/`worker fn*` bytecode + the shipped code-slice survive serialization.
  Assert all four produce identical stdout. Run `cargo test --test vm_differential worker` → **expect FAIL** until all engines route `spawn`/handle-call/`worker fn*` identically (Tasks 5/6 mirror the tree-walker in `vm/run.rs`; this test is the guardrail that they match).

- [x] **Step 2: Fix divergences in the engine, never the assertion.** If specialized ≠ generic, a specialization guard is wrong — fix the guard. If tree-walker ≠ VM, fix the VM (the tree-walker is the oracle). If `.aso` ≠ source-run, fix the serializer/code-slice.

- [x] **Step 3:** Run `cargo test --test vm_differential` in BOTH feature configs → **expect PASS**.

- [x] **Step 4: Commit**
  ```bash
  git commit -am "test(differential): stateful-worker examples byte-identical across tree-walker/specialized/generic/.aso

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

## Task 15: Performance benchmarks (§7.4)

**Files:** `bench/` (extend Plan A's harness + markdown report).

- [x] **Step 1: Actor throughput bench.** Add a bench measuring messages/sec to a SINGLE actor (mailbox round-trip cost) and aggregate throughput across N independent actors on N cores (scaling). Use a tight loop of `await handle.method(small_payload)` for the single-actor number; N independent actors driven via `task.gather` for the aggregate.

- [x] **Step 2: Streaming throughput + chunking effect.** Bench records/sec for a `worker fn*` at prefetch=1 vs a larger window (if the prefetch knob is exposed; else document prefetch=1 only), AND per-element vs per-chunk yielding (yield 1 record vs yield an array of K records) — quantify the "yield chunks, not elements" guidance and find the break-even chunk size.

- [x] **Step 3: Dedicated-isolate spawn cost.** Bench `spawn` latency (cold) and steady-state per-message latency (warm).

- [x] **Step 4: Report.** Append the stateful-worker numbers to Plan A's `bench/` report (sibling to `bench/PROFILING_RESULTS.md`). Headline numbers on the VM; tree-walker informational. No hard CI threshold (CI core counts vary) — record measured figures + the break-even chunk size.

- [x] **Step 5:** Run the benches (`cargo run --release -- run bench/...` or `cargo bench` per Plan A's harness shape) and write the report. Commit.
  ```bash
  git add bench/
  git commit -m "bench(workers): actor throughput, streaming + chunking effect, dedicated-isolate spawn cost; report

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

## Task 16: FINAL DOCUMENTATION CONSISTENCY & STALENESS SWEEP (§8.2)

> Runs after BOTH specs land. A deliberate pass over the ENTIRE doc set to (a) integrate workers and (b) catch + fix any stale/contradictory info — not limited to worker content. Each sub-task is a concrete file edit.

### Task 16.1: New workers docs page + `NAV` entry (§8.1)

**Files:** `docs/content/language/workers.md` (new or extend), `docs/assets/app.js` (`NAV`), `docs/content/language/modules-async.md`.

- [x] **Step 1:** Create `docs/content/language/workers.md` (or extend if Plan A created it). Sections (per §8.1):
  - The model + two lifecycles (intra- vs inter-isolate; the sendability line).
  - `worker fn` / `static worker fn` (Spec A) + cost model + capture/sendability rules.
  - `worker class` actor semantics: proxy handle, async-only methods, NO field access across the boundary, non-reentrancy, owns resources (resource born in the isolate), `spawn` vs local construction, close/last-drop.
  - `worker fn*` streaming: demand-driven pull, bounded buffer/backpressure, bidirectional `next(v)`, the "yield chunks, not elements" guidance.
  - "Workers and the event bus": intra- vs inter-isolate layering + the bridge idiom + `task.pipe`.
  - Use RELATIVE in-content links (`](modules-async)`, `](../stdlib/async)`) — resolved relative to the current page's directory, NOT absolute-from-root (CLAUDE.md docs note).
- [x] **Step 2:** Add the slug to the `NAV` array in `docs/assets/app.js` under the `Language` section: `['language/workers', 'Workers & parallelism']` (sidebar + cmd-K search both derive from `NAV` — a page with no entry is unreachable).
- [x] **Step 3:** Cross-link `docs/content/language/modules-async.md` ↔ `workers.md` in BOTH directions (add a "Parallelism: see Workers" pointer in modules-async, and a "see Modules & async" pointer in workers).
- [x] **Step 4: Commit.**

### Task 16.2: README.md

**Files:** `README.md`.

- [x] **Step 1:** Add workers to the concurrency/capability description and the stdlib/feature table. Reconcile any "single-threaded"/"no multithreading" phrasing into the accurate framing: **single-threaded per isolate, multi-core via shared-nothing workers**. Verify the CLI list and links (the `run`/`build`/`repl` table is unaffected; confirm). Add `task.pipe` if a stdlib table lists `std/task` fns.
- [x] **Step 2: Commit.**

### Task 16.3: docs/ static site — landing stats + concurrency content + link/nav integrity

**Files:** `docs/index.html`, `docs/content/language/modules-async.md`.

- [x] **Step 1:** `docs/index.html` — re-verify headline stats. The **core value-kind count stays 16** (actor + generator handles are modeled as `Native`, not new `Value` variants — confirm and do NOT change the `16` at line 41). The module count: if a `worker` stdlib HELPER module was added it changes (`task.pipe` lives in the existing `std/task`, so the `54` std modules / `28` framing is UNCHANGED — confirm). Update any capability claim that asserts single-threaded-only.
- [x] **Step 2:** `docs/content/language/modules-async.md` (and any page asserting the execution model) — update to include workers; remove/repair contradictions with the new parallelism story (the page currently states a single-threaded event loop — refine to "single-threaded per isolate; parallelism via workers").
- [x] **Step 3: Link & nav integrity.** Confirm every page (incl. the new workers page) has a `NAV` entry and that in-content relative links resolve (the documented orphan/relative-link gotchas).
- [x] **Step 4: Commit.**

### Task 16.4: CLAUDE.md

**Files:** `CLAUDE.md`.

- [x] **Step 1:** Update the "What this is" / concurrency description to mention shared-nothing worker parallelism.
- [x] **Step 2:** Clarify in the interpreter section that the `!Send`, single-threaded, `Rc`/`RefCell` model is **per-isolate** — parallelism is achieved by ISOLATION (multiple complete runtimes on separate threads sharing no memory), NOT by shared memory — so it isn't misread as "no parallelism possible". Keep the "never hold a `RefCell` borrow across `.await`" invariant intact and note it applies within each isolate.
- [x] **Step 3:** Add a **"Workers" entry under "Larger subsystems (campaign work, condensed)"** documenting the architecture for future sessions: the two lifecycles (Spec A pooled/stateless `worker fn`; Spec B dedicated-isolate `worker class` actors + `worker fn*` streaming generators); the serializer airlock (`src/worker/serialize.rs` — only bytes cross, the runtime stays `!Send`); the pool (Spec A) vs dedicated isolates (Spec B); actor handle = `Value::Native(WorkerActor)` in `Interp.resources` with a FIFO one-at-a-time mailbox + non-reentrancy guard; streaming handle = `Value::Generator` over `GenImpl::Worker` (`src/coro.rs`) with demand-driven pull + bounded buffer; both torn down on `close()`/last-drop; GC must NOT trace into either (native-handle invariant); `task.pipe` bridge.
- [x] **Step 4:** State the feature-flag status: workers are **core / default-on**, built under `--no-default-features` (like the GC). Confirm against `Cargo.toml` (the `src/worker/` module is unconditional, NOT behind a feature) and state it.
- [x] **Step 5: Commit.**

### Task 16.5: Main design spec non-goal supersession

**Files:** `superpowers/specs/2026-05-29-ascript-design.md`.

- [x] **Step 1:** Amend line ~50 (`- No multithreading in user code (single-threaded event loop; see §7).`) with a supersession note pointing at the two worker specs: e.g. `- No SHARED-MEMORY multithreading in user code (single-threaded event loop PER ISOLATE; see §7). Superseded/refined by shared-nothing worker parallelism — see specs 2026-06-07-workers-foundation-stateless-design.md and 2026-06-07-workers-stateful-actors-streaming-design.md (parallelism by isolation, no data races).`
- [x] **Step 2:** Cross-reference from §7 (async model) — add a pointer to the worker specs noting parallelism is now available via shared-nothing isolates.
- [x] **Step 3: Commit.**

### Task 16.6: roadmap.md milestone entry

**Files:** `superpowers/roadmap.md`.

- [x] **Step 1:** Add the workers milestone entry (consistent with the existing milestone-by-milestone record), referencing both plan paths (Spec A + Spec B) and summarizing: shared-nothing isolates for multi-core parallelism; `worker fn`/`static worker fn` (pooled, stateless); `worker class` actors + `worker fn*` streaming (dedicated isolates); the serializer airlock; `.aso` bump; all-modes differential coverage.
- [x] **Step 2: Commit.**

### Task 16.7: End-to-end whole-doc-set sanity verification

**Files:** read-only sweep across README + `docs/content/**` + CLAUDE.md; fix anything stale discovered.

- [x] **Step 1:** Read README + `docs/content/**` + CLAUDE.md end-to-end for internal consistency (stats, counts, capability claims, execution-model statements, dead links). Fix anything stale discovered in the process — worker-related or not (the §8.2 mandate is a GENERAL staleness sweep).
- [x] **Step 2: Serve & verify the site.** `cd docs && python3 -m http.server` (the site `fetch`es Markdown — must be served, not `file://`). Confirm: the site renders; the new workers page is reachable via the sidebar AND cmd-K search; in-content relative links on the workers page resolve; no console 404s for the new content. Capture the result in the commit message.
- [x] **Step 3: Final full-suite green gate.** Run `cargo test`, `cargo test --no-default-features`, `cargo clippy --all-targets`, and `cargo clippy --no-default-features --all-targets` — all clean. (Clippy must be clean in BOTH configs per CLAUDE.md.)
- [x] **Step 4: Commit.**
  ```bash
  git commit -am "docs(sweep): integrate workers + whole-doc-set staleness pass (README/docs/CLAUDE/design-spec/roadmap); verified served site

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

### Task 16.8: Holistic review + merge

- [x] **Step 1:** Holistic review per the milestone workflow (a fresh reviewer runs commands + probes edges: actor non-reentrancy, closed-actor calls, generator backpressure, last-drop teardown / no zombie threads, sendability field-path messages, all-modes byte-identity, zero `type-*` on the corpus).
- [x] **Step 2:** Merge `--no-ff` per the project milestone workflow.

---

## Self-review checklist

- [x] Header verbatim, brackets filled.
- [x] File Structure maps every created/modified file + responsibility.
- [x] Every task has **Files:** and bite-sized TDD steps (failing test → run/expect FAIL → minimal code → run/expect PASS → commit with the trailer).
- [x] No placeholders: full `.as` test/example contents + expected stdout written out; exact file+function+change cited where a detail is discovered at implementation time (Plan-A names flagged).
- [x] Reuses Plan A (serializer, isolate bootstrap, code-shipping, cancel/error, `worker` keyword) — does NOT re-specify it; integration points pinned up front.
- [x] Cross-cutting checklist covered: both parsers, tree-sitter regen + sync + pins + editor highlights, exhaustive AST/Display/fmt, both-engines byte-identical (Task 14), `.aso` bump (Task 11).
- [x] Native-handle invariant (GC must not trace) called out (Task 5 Step 8); take-out-across-await discipline called out (Tasks 5/6).
- [x] §8.2 documentation sweep is its own explicit set of sub-tasks (16.1–16.8) with concrete file edits, NOT vague.

## Spec coverage map

| Spec B section | Task(s) |
|---|---|
| §2 Dedicated-isolate lifecycle (shared) | Task 4 (spawn/track/teardown reusing Plan A bootstrap) |
| §3 Actors — `worker class` (spawn→future<handle>, proxy, async methods, no field access, state persists, FIFO one-at-a-time, non-reentrancy, inheritance/code-ship, close/last-drop, closed-actor panic) | Tasks 1, 5 (+ inheritance via code-slice in 5 Step 3) |
| §4 Streaming `worker fn*` (dedicated isolate, demand-driven pull, bounded buffer/backpressure, bidirectional next(v), close/drop, Value::Generator driver) | Tasks 1, 6 |
| §5 Event bus bridge (`pipe`, intra/inter-isolate layering) | Task 7 (+ docs 16.1) |
| §6 Front-ends (two parsers, is_worker on class) | Task 1 |
| §6 Tree-sitter + queries + regen + sync + pins | Task 2 |
| §6 Editor integrations (TextMate/Zed/Neovim) | Task 2 |
| §6 Formatter + Display | Task 3 |
| §6 Checker type inference (spawn→future<handle>, method→future<T>, fn*→generator) | Task 8 |
| §6 Call-arity (`pipe`) + spawn arity vs init | Tasks 7, 8 |
| §6 `worker-reentrancy` lint (optional) | Task 8 Step 4 |
| §6 LSP (tokens, symbols, hover, nav, completion) | Task 9 |
| §6 REPL | Task 10 |
| §6 New value/handle kinds (Native actor + cross-thread generator driver) | Tasks 5, 6 |
| §6 Dedicated-isolate manager | Task 4 (+ 5, 6) |
| §6 `.aso` bump + verify | Task 11 |
| §6 Determinism event-sourcing + lint | Task 12 |
| §7.1 Behavioral tests (actors/generators/bridge) | Tasks 5, 6, 7 |
| §7.2 All-modes execution (4 modes) | Task 14 |
| §7.3 Example corpus (6 files) | Task 13 |
| §7.4 Performance measurement | Task 15 |
| §8.1 New workers page + NAV entry | Task 16.1 |
| §8.2 README sweep | Task 16.2 |
| §8.2 docs/ landing stats + concurrency content + link/nav integrity | Task 16.3 |
| §8.2 CLAUDE.md (concurrency desc + Workers subsystem entry + per-isolate !Send + feature status) | Task 16.4 |
| §8.2 Main design-spec non-goal supersession (line ~50 + §7 xref) | Task 16.5 |
| §8.2 roadmap.md milestone entry | Task 16.6 |
| §8.2 Whole-doc-set sanity verification (serve docs, links/NAV/stats) | Task 16.7 |
| §9 Scope/rejected (no new primitives beyond actors/streams/pipe) | Honored throughout (no broadcast bus, no copy-self, no field access — enforced by Task 5 methods-only dispatch) |
