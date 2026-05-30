# Async Concurrency + Generators/Coroutines (Architecture A) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn AScript's async model from "sequential inline, `await` is identity" into real cooperative concurrency on the single-threaded tokio runtime, then expose the interpreter's *existing* stackless-coroutine nature as script-level generators and bidirectional coroutines — all on one engine, no `unsafe` coroutine crate, no CPS rewrite.

**Architecture:** The interpreter is already an `async` tree-walker (`#[async_recursion]`). We (1) move interpreter state behind interior mutability so multiple eval futures can be live at once, (2) make calling an `async fn` return an eagerly-scheduled `Value::Future` and make `await` actually drive it, adding `spawn`/`gather`/`race`/`timeout`, then (3) implement `yield` as a real `.await` on a single-consumer rendezvous, giving generators/coroutines and `for await` for free. Concurrency rides `tokio::task::LocalSet` + `spawn_local` (accepts `!Send` futures — our `Rc`/`RefCell` model is preserved).

**Tech Stack:** Rust, tokio (current-thread runtime + `LocalSet`, `sync::Notify`), `async-recursion`, tree-sitter (grammar regen). No new external crate is required (rendezvous is hand-rolled on `tokio::sync`).

**Non-goals (documented deferrals — see Phase 0):** durable/serializable continuations (needs an explicit-stack VM, "B2"); robust unbounded script recursion over deep data (needs stackful coroutines, "B1"); deterministic/replayable scheduling. None are required by the target use cases (multi-client serving, composable AI/SSE streaming, coroutine handlers).

---

## Blast Radius Assessment

Quantified from the current tree (commit `3620821`). Severity = risk if done carelessly; most of the surface is mechanical and guarded by the existing ~540-test suite + differential parser conformance.

| Area | Files | Scale | Severity | Notes |
|---|---|---|---|---|
| **Interior-mutability refactor** | `src/interp.rs` (**33** `&mut self`), 9 stdlib modules (`net_tcp`, `http_server`, `net_ws`, `process`, `sqlite`, …) | large, mechanical | **High** | `&mut self` → `&self` + `RefCell` cells. The dominant diff. Hazard: a `RefCell` borrow held across `.await` panics at runtime (not caught by the borrow checker). Mitigated by a clippy lint (Task 1.5) + the scoped-borrow helper. |
| **Async core semantics** | `src/interp.rs` (`Await` arm @883, async-fn call path, **15** `#[async_recursion]`), `src/lib.rs` (3 entry points), `src/main.rs`, `src/repl.rs` | medium | **High** | `await` identity → drive-future; async-call → schedule future; top-level wraps a `LocalSet` and drains tasks (structured join). |
| **New runtime values** | `src/value.rs` (`Value` enum @207, `type_name` @156, `Display` @309), `src/interp.rs::type_name` @1450, `src/fmt.rs` (value printing) | small | Med | `+Value::Future`, `+Value::Generator` (identity equality, like `Array`/`Map`). |
| **New module: rendezvous/scheduler** | `src/coro.rs` (new), `src/task.rs` (new) | new code you own | **High** | The novel kernel: a single-consumer bidirectional rendezvous future + the future/task registry. Heavily unit-tested. |
| **Grammar / front-end** | `src/token.rs` (`+Yield`), `src/ast.rs` (`ExprKind::Yield`, `+is_generator` on `Fn`/`Arrow`/`MethodDecl`, `+for_await` on `ForOf`, `Type::Future`), `src/parser.rs` (fn_decl @181, arrow, yield, for-await, type parser), `src/value.rs::Function` (`+is_generator` @197) | medium | Med | Mirror in `fmt.rs` (`write_expr_inner`) + `ast.rs` `Display`. |
| **Tree-sitter** | `docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js` + regen `src/parser.c` (`tree-sitter generate --abi 14`) | medium | Med | Watch for new GLR conflicts (grammar already declares one for `?`). |
| **Conformance & examples** | `tests/frontend_conformance.rs`, `tests/treesitter_conformance.rs`, `tests/cli.rs`, `examples/*.as`, `examples/advanced/*.as` | medium | Med | Both parsers must accept new syntax; examples double as living docs. |
| **Behavior-change tests** | `src/interp.rs` (`std_time_sleep_without_await_also_works` @2840), net/async integration tests | small | Med | The sleep-without-await test encodes the OLD semantic and **changes by design**. |
| **Docs** | `docs/superpowers/specs/2026-05-29-ascript-design.md` (§7), `docs/content/**`, `README.md`, `CLAUDE.md`, `docs/superpowers/roadmap.md` | medium | Low | Doc-only; do alongside, finalize in Phase 6. |
| **Feature flags** | `Cargo.toml` | tiny | Low | `LocalSet`/`spawn_local` live in `tokio/rt` (already on); `sync::Notify` already on. No new crate. |

**What is explicitly NOT touched:** `lexer` token scanning beyond one keyword, the LSP analysis core (only keyword lists), the value model's `Rc`/`RefCell` discipline (preserved — we lean into it).

---

## File Structure

**New files:**
- `src/coro.rs` — the single-consumer bidirectional rendezvous (`YieldChannel`) + `GeneratorState`. One responsibility: suspend/resume a script generator body across `yield`.
- `src/task.rs` — `FutureState` (shared completion cell) + the `spawn`/`gather`/`race`/`timeout` host logic and the `LocalSet` driver helper. One responsibility: scheduling & joining futures.
- `docs/content/stdlib/async.md` — user docs for `future<T>`, `spawn`/`gather`/`race`/`timeout`, generators, coroutines, `for await`.

**Modified (by responsibility):**
- Front-end (syntax): `token.rs`, `ast.rs`, `parser.rs`, `fmt.rs`, tree-sitter grammar.
- Runtime: `value.rs` (variants), `interp.rs` (eval semantics, dispatch, interior mutability).
- Entry/driver: `lib.rs`, `main.rs`, `repl.rs`.
- Stdlib: new `stdlib/task_mod.rs` registration in `stdlib/mod.rs`; `&mut self`→`&self` in the 9 native modules.

---

## Phases Overview (tracking)

- [ ] **Phase 0 — Design lock-in:** spec §7 rewrite + ADR; CLAUDE.md interim note. *(docs only, no code)*
- [ ] **Phase 1 — Interior-mutability foundation:** `&mut self`→`&self`, borrow lint. *No behavior change; all ~540 tests stay green.*
- [ ] **Phase 2 — Futures & real async:** `Value::Future`, real `await`, `spawn`/`gather`/`race`/`timeout`, structured top-level.
- [ ] **Phase 3 — `future<T>` type contract.**
- [ ] **Phase 4 — Generators & coroutines:** `src/coro.rs`, `yield`, `fn*`, `async fn*`, `for await`, `Value::Generator`.
- [ ] **Phase 5 — Callback server form** (`http.serve(handler)`, net-gated) *(optional, high-value ergonomics)*.
- [ ] **Phase 6 — Docs, CLAUDE.md, examples, conformance + holistic review + merge.**

Each phase ends green (both `cargo test` and `cargo test --no-default-features`, plus `cargo clippy --all-targets` in both configs) and is committed. Merge `--no-ff` at Phase 6 per the project milestone workflow.

---

## Phase 0 — Design Lock-In (docs only)

### Task 0.1: Rewrite spec §7 and record the architecture decision

**Files:**
- Modify: `docs/superpowers/specs/2026-05-29-ascript-design.md` (§7 Concurrency, currently ~line 278)
- Create: `docs/superpowers/specs/adr/2026-05-30-async-generators.md`
- Modify: `docs/superpowers/roadmap.md` (append a new milestone entry)

- [ ] **Step 1:** In the ADR, document the decision verbatim: *Architecture A + generators on the existing async engine.* Include the three considered options (A / B1 stackful / B2 CPS), why A-with-async-generators wins (single engine, no `unsafe`, `!Send`-preserved), and the **three documented deferrals** (durable serialization, robust unbounded recursion, deterministic scheduling) each tagged as future work, not silent drops. Cite the research: genawaiter (bidirectional coroutines on stable async), async-stream, RFC 3513.

- [ ] **Step 2:** Rewrite spec §7 to state the new semantics precisely:
  - Calling an `async fn` returns a `future<T>` that is **eagerly scheduled** (begins running, progresses while the current task is parked at an `await`).
  - `await e` drives `e`'s future to completion and yields its value; `await x` for a non-future is identity (back-compat).
  - The top-level program runs as the root task; the runtime **joins all spawned tasks before exit** (structured concurrency; no detached fire-and-forget that outlives `main`).
  - Generators (`fn*`) and async generators (`async fn*`) produce values via `yield`; `yield` is a suspension point implemented as an `await` on an internal rendezvous; bidirectional resume is supported (`gen.next(v)`).
  - `for await (x in e)` consumes any async-iterable (native stream handle **or** script generator).
  - Single-threaded invariant retained: `Rc`/`RefCell`, `!Send`, no data races; **the one new rule for script authors is none — but for the interpreter, "never hold a borrow across `await`".**

- [ ] **Step 3:** Add the roadmap milestone "M17: Async Concurrency + Generators/Coroutines" referencing this plan path.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-05-29-ascript-design.md docs/superpowers/specs/adr/2026-05-30-async-generators.md docs/superpowers/roadmap.md
git commit -m "docs(spec): M17 — async concurrency + generators design (Architecture A)"
```

### Task 0.2: Interim CLAUDE.md note

**Files:**
- Modify: `CLAUDE.md` (the interpreter section)

- [ ] **Step 1:** Add a short note under "The interpreter" that M17 is in progress and that the new invariant is **"never hold a `RefCell` borrow across an `.await`"**, with a pointer to this plan. (The full CLAUDE.md rewrite lands in Phase 6 once the code is real.)

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: note M17 async invariant in CLAUDE.md (interim)"
```

---

## Phase 1 — Interior-Mutability Foundation (no behavior change)

**Why first:** decouples interpreter state from `&mut self` so concurrent eval futures can coexist, WITHOUT changing any observable behavior. This is the biggest, riskiest mechanical change; isolating it means the entire ~540-test suite proves correctness with zero semantic drift before any async work.

### Task 1.1: Wrap mutable `Interp` state in interior-mutability cells

**Files:**
- Modify: `src/interp.rs` (`struct Interp` @170; `Interp::new` @196; every reader/writer of `output`, `resources`, `next_resource_id`, `tests`, `modules`)

- [ ] **Step 1: Write the failing test** (proves shared-ref eval works)

```rust
// src/interp.rs (tests module)
#[tokio::test]
async fn interp_evaluates_via_shared_ref() {
    let vm = std::rc::Rc::new(Interp::new());
    let env = global_env().child();
    let stmts = parse_program("let x = 41\nprint(x + 1)");
    vm.exec(&stmts, &env).await.unwrap();        // &self, not &mut self
    assert_eq!(vm.output(), "42\n");             // output() reads the RefCell
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test interp_evaluates_via_shared_ref` → FAIL (compile error: `exec` takes `&mut self`, no `output()`).

- [ ] **Step 3: Change the struct to interior mutability**

```rust
pub struct Interp {
    output: RefCell<String>,
    modules: RefCell<HashMap<PathBuf, ModuleEntry>>,
    module_dir: RefCell<PathBuf>,
    current_exports: Rc<RefCell<HashSet<String>>>,
    tests: RefCell<Vec<(String, Value)>>,
    resources: RefCell<HashMap<u64, ResourceState>>,
    next_resource_id: Cell<u64>,
}
impl Interp {
    pub fn output(&self) -> String { self.output.borrow().clone() }
    pub(crate) fn push_output(&self, s: &str) { self.output.borrow_mut().push_str(s); }
    pub(crate) fn next_id(&self) -> u64 {
        let id = self.next_resource_id.get(); self.next_resource_id.set(id + 1); id
    }
}
```

- [ ] **Step 4: Convert `exec`/`eval_expr`/`exec_stmt`/`call_*` signatures** from `&mut self` to `&self`. The bodies stop taking `&mut`; mutation goes through the cell accessors. **Apply this exact pattern to all 33 `&mut self` sites in `interp.rs`** (enumerate with `grep -n "&mut self" src/interp.rs`). Representative conversion:

```rust
// before
pub async fn exec(&mut self, program: &[Stmt], env: &Environment) -> Result<Flow, Control> { ... }
// after
pub async fn exec(&self, program: &[Stmt], env: &Environment) -> Result<Flow, Control> { ... }
```

For resource access, replace `self.resources.get_mut(&id)` with a scoped borrow helper (Task 1.3).

- [ ] **Step 5: Update direct field reads** — every `self.output.push_str(..)` → `self.push_output(..)`; `interp.output` (in `lib.rs`) → `interp.output()`.

- [ ] **Step 6: Run** — `cargo test interp_evaluates_via_shared_ref` → PASS.

- [ ] **Step 7: Commit**

```bash
git add src/interp.rs src/lib.rs
git commit -m "refactor(interp): move state to interior mutability; eval takes &self"
```

### Task 1.2: Convert stdlib dispatch to `&self`

**Files:**
- Modify: `src/stdlib/mod.rs` (`call_stdlib` @126, `call_time` @189), and the 9 native modules using `&mut self`/`&mut Interp` (enumerate: `grep -rl "&mut self\|&mut Interp\|interp: &mut" src/stdlib/`)

- [ ] **Step 1:** Change `call_stdlib(&mut self, …)` → `call_stdlib(&self, …)` and propagate to each module's `call(...)`/method dispatch.
- [ ] **Step 2:** Inside each native module, replace any held `&mut self.resources` with the scoped take-out pattern (Task 1.3).
- [ ] **Step 3: Run both configs** — `cargo test` and `cargo test --no-default-features` → all green (no behavior change).
- [ ] **Step 4: Commit**

```bash
git add src/stdlib/
git commit -m "refactor(stdlib): dispatch over &self"
```

### Task 1.3: Universal "take the resource out across await" helper

**Files:**
- Modify: `src/interp.rs` (add `with_resource_taken` helper near the resource table)

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn resource_take_out_roundtrips() {
    let vm = Rc::new(Interp::new());
    let id = vm.register_resource(ResourceState::Counter(0)); // test-only variant
    let taken = vm.take_resource(id).unwrap();
    // simulate awaiting while owning it
    tokio::task::yield_now().await;
    vm.return_resource(id, taken);
    assert!(vm.has_resource(id));
}
```

- [ ] **Step 2: Run** → FAIL (helpers missing).

- [ ] **Step 3: Implement** `take_resource(&self, id) -> Option<ResourceState>` (removes from the `RefCell<HashMap>` so no borrow is held across the subsequent `.await`), `return_resource(&self, id, state)`, `has_resource`. This generalizes the existing pattern at `http_server.rs:574`.

- [ ] **Step 4: Run** → PASS. **Then audit** every native I/O method that currently `.await`s while touching `self.resources` and convert it to take-out → await → return. Enumerate: `grep -rn "\.await" src/stdlib/{net_tcp,net_ws,http_server,process,sqlite}.rs` and confirm none hold a `resources` borrow across the await.

- [ ] **Step 5: Commit**

```bash
git add src/interp.rs src/stdlib/
git commit -m "refactor: take resources out of the table across awaits (no borrow held)"
```

### Task 1.4: Drive entry points through a `LocalSet` (still sequential)

**Files:**
- Modify: `src/lib.rs` (`run_file` @24, `run_tests` @35, `run_source` @47); `src/main.rs` (@28); `src/repl.rs`

- [ ] **Step 1: Write the failing test**

```rust
// tests/cli.rs — program still runs identically, now under a LocalSet
#[tokio::test]
async fn runs_under_localset() {
    let out = ascript::run_source("print(1+1)").await.unwrap();
    assert_eq!(out, "2\n");
}
```

- [ ] **Step 2: Run** → PASS already if signature unchanged; the point is to refactor entry points to own a `LocalSet` so Phase 2 can `spawn_local`. Wrap the root future:

```rust
pub async fn run_source(src: &str) -> Result<String, AsError> {
    // ...lex/parse...
    let vm = Rc::new(Interp::new());
    let env = global_env().child();
    let local = tokio::task::LocalSet::new();
    let result = local.run_until(async { vm.exec(&program, &env).await }).await;
    local.await; // drain spawned tasks (structured join) — no-op until Phase 2
    // ...map Flow/Control to Ok(vm.output())...
}
```

- [ ] **Step 3:** Apply the same `LocalSet` wrapping to `run_file`, `run_tests`, and the REPL eval loop. `main.rs` stays `#[tokio::main(flavor = "current_thread")]`.

- [ ] **Step 4: Run both configs** → all green.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/main.rs src/repl.rs tests/cli.rs
git commit -m "refactor: run programs under a tokio LocalSet (sequential; enables spawn_local)"
```

### Task 1.5: Borrow-across-await clippy lint / CI guard

**Files:**
- Modify: `Cargo.toml` or `clippy.toml`; add a CI note in `CLAUDE.md` Phase 6 list

- [ ] **Step 1:** Enable `clippy::await_holding_refcell_ref` as `deny` (this is a real clippy lint that catches exactly the hazard).

```toml
# Cargo.toml [lints.clippy] or crate-level inner attribute
await_holding_refcell_ref = "deny"
```

- [ ] **Step 2: Run** `cargo clippy --all-targets` and `cargo clippy --no-default-features --all-targets` → must be clean (fix any flagged site by take-out or scoped borrow).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "ci: deny await_holding_refcell_ref (the M17 borrow hazard)"
```

**Phase 1 exit criteria:** both test configs green, both clippy configs clean, **zero observable behavior change**.

---

## Phase 2 — Futures & Real Async

### Task 2.1: `FutureState` + `Value::Future`

**Files:**
- Create: `src/task.rs`
- Modify: `src/value.rs` (`Value` @207, `type_name` @156, `Display` @309), `src/interp.rs::type_name` @1450, `src/lib.rs` (`pub mod task;`)

- [ ] **Step 1: Write the failing test**

```rust
// src/task.rs (tests)
#[tokio::test]
async fn future_resolves_and_caches() {
    let f = SharedFuture::new();
    let f2 = f.clone();
    tokio::task::LocalSet::new().run_until(async move {
        tokio::task::spawn_local(async move { f2.resolve(Value::Number(7.0)); });
        assert_eq!(f.get().await, Value::Number(7.0));
        assert_eq!(f.get().await, Value::Number(7.0)); // cached
    }).await;
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** the shared completion cell (no new crate):

```rust
// src/task.rs
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;
use tokio::sync::Notify;

#[derive(Clone)]
pub struct SharedFuture(Rc<Inner>);
struct Inner { slot: RefCell<Option<Value>>, ready: Notify }

impl SharedFuture {
    pub fn new() -> Self { SharedFuture(Rc::new(Inner { slot: RefCell::new(None), ready: Notify::new() })) }
    pub fn resolve(&self, v: Value) { *self.0.slot.borrow_mut() = Some(v); self.0.ready.notify_waiters(); }
    pub async fn get(&self) -> Value {
        loop {
            if let Some(v) = self.0.slot.borrow().clone() { return v; }
            self.0.ready.notified().await;
        }
    }
    pub fn ptr_eq(&self, other: &SharedFuture) -> bool { Rc::ptr_eq(&self.0, &other.0) }
}
```

- [ ] **Step 4:** Add `Value::Future(SharedFuture)` to `value.rs`; `type_name` → `"future"`; `Display` → `"<future>"`; equality = `ptr_eq` (identity, like `Array`/`Map`); add the arm to `interp::type_name`.

- [ ] **Step 5: Run** → PASS; `cargo build` clean (exhaustive matches updated).

- [ ] **Step 6: Commit**

```bash
git add src/task.rs src/value.rs src/interp.rs src/lib.rs
git commit -m "feat(runtime): Value::Future backed by a shared completion cell"
```

### Task 2.2: Async-fn call returns an eagerly-scheduled future; `await` drives it

**Files:**
- Modify: `src/interp.rs` (`call_function` @1176 / the call path; `Await` arm @883)

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn async_call_returns_future_awaited_later() {
    let out = run("async fn f(x){ return x*2 }\nlet a = f(20)\nlet b = f(1)\nprint(await a + await b)").await;
    assert_eq!(out, "42\n");
}
```

- [ ] **Step 2: Run** → FAIL (today `await` is identity but async-call runs inline returning a number, not a future; this test still passes by luck — change it to assert concurrency):

```rust
#[tokio::test]
async fn unawaited_async_call_progresses_concurrently() {
    // two sleeps started without await; total wall-time ~ max, not sum
    let out = run("import * as time from \"std/time\"\n\
                   async fn slow(ms,v){ await time.sleep(ms); return v }\n\
                   let a = slow(30,\"a\")\nlet b = slow(30,\"b\")\n\
                   print(await a)\nprint(await b)").await;
    assert_eq!(out, "a\nb\n");
}
```

- [ ] **Step 3: Implement.** When calling a `Function` with `is_async == true`, build the body future, wrap a `SharedFuture`, `spawn_local` a task that runs the body and `resolve`s it, and return `Value::Future(shared)` immediately. The spawned future captures `Rc<Interp>` (clone) + the call env — `'static`, `!Send`, fine on `spawn_local`.

```rust
// inside the call path, when func.is_async:
let vm = self.rc_clone();            // Rc<Interp> (add this helper)
let fut = SharedFuture::new();
let fut2 = fut.clone();
let body = func.clone(); let call_env = /* built as today */;
tokio::task::spawn_local(async move {
    let v = match vm.run_body(&body, args, &call_env, span, &name).await {
        Ok(Flow::Return(v)) => v,
        Ok(_) => Value::Nil,
        Err(c) => { /* record panic to surface on await; see Step 5 */ Value::Nil }
    };
    fut2.resolve(v);
});
return Ok(Value::Future(fut));
```

- [ ] **Step 4: Change the `Await` arm** to drive a future:

```rust
ExprKind::Await(inner) => {
    let v = self.eval_expr(inner, env).await?;
    match v {
        Value::Future(f) => Ok(f.get().await),
        other => Ok(other), // `await 5` stays identity
    }
}
```

- [ ] **Step 5: Panic/error propagation across the task boundary.** Store an optional `Control` alongside the value in `SharedFuture` (extend the slot to `Option<Result<Value, Control>>`); `get()` returns the `Result`, and the `Await` arm `?`-propagates it. Add a test: `async fn boom(){ panic("x") }` then `await boom()` surfaces the panic at the await site.

- [ ] **Step 6: Run** both configs → green; the concurrency test passes.

- [ ] **Step 7: Commit**

```bash
git add src/interp.rs
git commit -m "feat(interp): async calls schedule eagerly; await drives futures"
```

### Task 2.3: `spawn` / `gather` / `race` / `timeout` builtins

**Files:**
- Create: `src/stdlib/task_mod.rs` (exports + `call`)
- Modify: `src/stdlib/mod.rs` (register in both `std_module_exports` and `call` arms), `src/task.rs` (helpers)

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn gather_runs_concurrently() {
    let out = run("import * as task from \"std/task\"\n\
                   import * as time from \"std/time\"\n\
                   async fn slow(ms,v){ await time.sleep(ms); return v }\n\
                   let r = await task.gather([slow(20,1), slow(20,2), slow(20,3)])\n\
                   print(r)").await;
    assert_eq!(out, "[1, 2, 3]\n");
}
#[tokio::test]
async fn race_first_wins() {
    let out = run("import * as task from \"std/task\"\n\
                   import * as time from \"std/time\"\n\
                   async fn d(ms,v){ await time.sleep(ms); return v }\n\
                   print(await task.race([d(50,\"slow\"), d(5,\"fast\")]))").await;
    assert_eq!(out, "fast\n");
}
#[tokio::test]
async fn timeout_fires() {
    // slow future misses the 5ms deadline -> [nil, err] Result pair
    let out = run("import * as task from \"std/task\"\n\
                   import * as time from \"std/time\"\n\
                   async fn slow(ms,v){ await time.sleep(ms); return v }\n\
                   let [v, err] = await task.timeout(5, slow(50, \"x\"))\n\
                   print(v == nil)\n print(err != nil)").await;
    assert_eq!(out, "true\ntrue\n");
}
```

- [ ] **Step 2: Run** → FAIL (module missing).

- [ ] **Step 3: Implement** `std/task`:
  - `spawn(futureOr0ArgFn)` → if given a `Value::Future`, return it as a tracked handle; if given a 0-arg function, call it (which schedules it) and return the resulting future. (Calling an async fn already schedules; `spawn` is the explicit detach/track entry point.)
  - `gather(array_of_futures)` → `await` each `SharedFuture::get()` in order, collect into an array; first `Control` short-circuits.
  - `race(array)` → poll all via `futures`-free manual select: park on each `Notify`; return the first resolved. (Implement with `tokio::select!` over the `get()` futures, or a small loop.)
  - `timeout(ms, future)` → returns a **Result pair**: `[value, nil]` if `future` resolves before `ms`, else `[nil, err]` (a Tier-1 timeout error) when `ms` elapses first. Implement by racing `future`'s `SharedFuture::get()` against a `tokio::time::sleep(ms)` (e.g. `tokio::select!`); the sleeper branch yields the timeout `err`. Never panics on a missed deadline.

- [ ] **Step 4:** Register `"std/task"` in both match arms of `src/stdlib/mod.rs`. (No feature gate — core async.)

- [ ] **Step 5: Run** both configs → green.

- [ ] **Step 6: Commit**

```bash
git add src/stdlib/task_mod.rs src/stdlib/mod.rs src/task.rs
git commit -m "feat(stdlib): std/task — spawn, gather, race, timeout"
```

### Task 2.4: Update the behavior-change test + structured top-level drain

**Files:**
- Modify: `src/interp.rs` (`std_time_sleep_without_await_also_works` @2840), `src/lib.rs`

- [ ] **Step 1:** Rewrite the old test to the new semantic: an *unawaited* `time.sleep(5)` now returns a future; assert that the program still completes and that the top-level `local.await` drains it before exit (so the sleep effect is observed). Rename to `unawaited_sleep_is_a_future_drained_at_exit`.

- [ ] **Step 2:** Confirm `run_file`/`run_source`/`run_tests` call `local.await` AFTER the root future so detached tasks complete (structured join). Add a test: a `spawn`'d task that `push`es to a shared array completes before `run_source` returns.

- [ ] **Step 3: Run** both configs → green.

- [ ] **Step 4: Commit**

```bash
git add src/interp.rs src/lib.rs
git commit -m "test+feat: structured top-level drain; update unawaited-sleep semantics"
```

**Phase 2 exit:** real concurrency works (`gather`/`race`), both configs green, both clippy clean.

---

## Phase 3 — `future<T>` Type Contract

### Task 3.1: Parse and check `future<T>`

**Files:**
- Modify: `src/ast.rs` (`Type` @46 `+Future(Box<Type>)`; `Display` @72), `src/parser.rs` (type parser), `src/interp.rs` (contract check site), `src/fmt.rs` (type printing)

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn future_type_annotation_checks() {
    let ok = run("async fn f(): future<number> { return 1 }\nlet x: future<number> = f()\nprint(await x)").await;
    assert_eq!(ok, "1\n");
    let err = run_err("let y: future<number> = 5").await;
    assert!(err.message.contains("future"));
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement.** Add `Type::Future(Box<Type>)`, parse `future<T>` (mirror `array<T>` @ existing array-type parse), add `Display` arm `write!(f, "future<{}>", t)`, and a contract check: a value satisfies `future<T>` iff it is `Value::Future` (the inner `T` is advisory/erased at runtime, like `array<T>` element checks today — match the existing depth of element checking).

- [ ] **Step 4: Run** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/ast.rs src/parser.rs src/interp.rs src/fmt.rs
git commit -m "feat(types): future<T> annotation + contract"
```

### Task 3.2: tree-sitter `future<T>` + conformance

**Files:**
- Modify: `docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js`; regen `src/parser.c`
- Modify: `tests/frontend_conformance.rs`, `tests/treesitter_conformance.rs`; add `examples/typed.as` line

- [ ] **Step 1:** Add `future` to the generic-type rule in `grammar.js`. Regenerate:

```bash
cd docs/superpowers/specs/grammar/tree-sitter-ascript && tree-sitter generate --abi 14
```

- [ ] **Step 2:** Add a `future<number>` annotation to `examples/typed.as`. Run `cargo test --test treesitter_conformance --test frontend_conformance` → both parsers accept it.
- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/grammar examples/typed.as tests/
git commit -m "feat(grammar): future<T> in tree-sitter + conformance"
```

---

## Phase 4 — Generators & Coroutines

### Task 4.1: The rendezvous kernel (`src/coro.rs`)

**Files:**
- Create: `src/coro.rs`
- Modify: `src/lib.rs` (`pub mod coro;`)

- [ ] **Step 1: Write the failing test** (bidirectional resume)

```rust
// src/coro.rs (tests)
#[tokio::test]
async fn rendezvous_yields_and_resumes_with_value() {
    let ch = YieldChannel::new();
    let producer = ch.clone();
    tokio::task::LocalSet::new().run_until(async move {
        let body = tokio::task::spawn_local(async move {
            let r1 = producer.yield_(Value::Number(1.0)).await; // yields 1, waits
            let r2 = producer.yield_(Value::Number(2.0)).await; // yields 2, waits
            assert_eq!(r1, Value::Str("a".into()));
            assert_eq!(r2, Value::Str("b".into()));
        });
        assert_eq!(ch.resume(Value::Nil).await, Some(Value::Number(1.0)));
        assert_eq!(ch.resume(Value::Str("a".into())).await, Some(Value::Number(2.0)));
        assert_eq!(ch.resume(Value::Str("b".into())).await, None); // body finished
        body.await.unwrap();
    }).await;
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** the single-consumer bidirectional rendezvous on `tokio::sync::Notify` (no new crate). `yield_(v)` parks the producer after placing `v` in `out`; `resume(input)` places `input`, wakes the producer, then waits for the next `out` (or completion). One value in flight at a time.

```rust
// src/coro.rs
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;
use tokio::sync::Notify;

#[derive(Clone)]
pub struct YieldChannel(Rc<Chan>);
struct Chan {
    out: RefCell<Option<Value>>,   // producer -> consumer (yielded value)
    inp: RefCell<Option<Value>>,   // consumer -> producer (resume value)
    done: RefCell<bool>,
    to_consumer: Notify,
    to_producer: Notify,
}
impl YieldChannel {
    pub fn new() -> Self { YieldChannel(Rc::new(Chan {
        out: RefCell::new(None), inp: RefCell::new(None), done: RefCell::new(false),
        to_consumer: Notify::new(), to_producer: Notify::new(),
    })) }
    // producer side
    pub async fn yield_(&self, v: Value) -> Value {
        *self.0.out.borrow_mut() = Some(v);
        self.0.to_consumer.notify_waiters();
        loop {
            if let Some(r) = self.0.inp.borrow_mut().take() { return r; }
            self.0.to_producer.notified().await;
        }
    }
    pub fn finish(&self) { *self.0.done.borrow_mut() = true; self.0.to_consumer.notify_waiters(); }
    // consumer side: returns Some(yielded) or None if the generator is done
    pub async fn resume(&self, input: Value) -> Option<Value> {
        *self.0.inp.borrow_mut() = Some(input);
        self.0.to_producer.notify_waiters();
        loop {
            if let Some(v) = self.0.out.borrow_mut().take() { return Some(v); }
            if *self.0.done.borrow() { return None; }
            self.0.to_consumer.notified().await;
        }
    }
}
```

> NOTE: the very first `resume` must NOT pre-load `inp` for an unstarted body. Add a `started: Cell<bool>` and on the first `resume`, just await the first `out`/`done` rather than feeding an input. The reference test above passes `Value::Nil` as the ignored first input — encode that.

- [ ] **Step 4: Run** → PASS (fix the first-resume nuance until green).

- [ ] **Step 5: Commit**

```bash
git add src/coro.rs src/lib.rs
git commit -m "feat(coro): single-consumer bidirectional yield rendezvous"
```

### Task 4.2: Grammar — `yield`, `fn*`, `async fn*`, `for await`

**Files:**
- Modify: `src/token.rs` (`+Yield`), `src/ast.rs` (`ExprKind::Yield(Option<Box<Expr>>)`; `+is_generator: bool` on `Stmt::Fn` @144, `Arrow` @24, `MethodDecl` @172; `+for_await: bool` on `Stmt::ForOf` @140; `Display` arms), `src/value.rs::Function` (`+is_generator` @197), `src/parser.rs` (fn_decl @181, arrow, yield expr, for-await), `src/fmt.rs` (`write_expr_inner` Yield arm; print `fn*`; `for await`)

- [ ] **Step 1: Write the failing parse tests**

```rust
// tests/frontend_conformance.rs or parser tests
#[test] fn parses_generator_and_yield() {
    assert!(parse_ok("fn* g(){ yield 1\n yield 2 }"));
    assert!(parse_ok("async fn* s(){ yield await fetch() }"));
    assert!(parse_ok("for await (x in g()) { print(x) }"));
    assert!(parse_ok("let v = yield"));         // bare yield
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement parsing.**
  - `token.rs`: add `Tok::Yield` (keyword `yield`); lexer maps it.
  - `parser.rs::fn_decl` (@181): after consuming `fn`, if next is `*`, set `is_generator = true`. Same for the `async fn` path (@262 area) and methods.
  - Add `yield` as a very-low-precedence prefix expression (just around assignment, like `await`): `yield`, optionally followed by an expression → `ExprKind::Yield(Option<Box<Expr>>)`.
  - `for await (x in e)`: after `for`, optional `await` keyword → `for_await = true` on `ForOf`.
  - Arrow generators (`async fn*` arrow) are out of scope for v1 (only named/`fn*`); document in spec.

- [ ] **Step 4: Mirror in `fmt.rs` and `ast.rs` Display** — `Yield` arm (`(yield {})`), print `fn*`/`async fn*`, and `for await (...)`. (CLAUDE.md invariant: ExprKind matches in interp/fmt/ast-Display all exhaustive.)

- [ ] **Step 5: Run** parser tests → PASS; `cargo build` clean.

- [ ] **Step 6: tree-sitter:** add `yield_expression`, generator marker (`*` after `fn`), `for_await` to `grammar.js`; regen `parser.c` (`--abi 14`); resolve any GLR conflict. Run conformance.

- [ ] **Step 7: Commit**

```bash
git add src/token.rs src/ast.rs src/value.rs src/parser.rs src/fmt.rs docs/superpowers/specs/grammar tests/
git commit -m "feat(grammar): yield, fn*/async fn*, for await (both parsers)"
```

### Task 4.3: `Value::Generator` + generator call + `yield` eval

**Files:**
- Modify: `src/value.rs` (`+Value::Generator`, `type_name`, `Display`, equality), `src/interp.rs` (`type_name`; call path for `is_generator`; `ExprKind::Yield` eval; a per-task "current yield channel" stack)

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn generator_yields_values() {
    let out = run("fn* count(){ yield 1\n yield 2\n yield 3 }\n\
                   let g = count()\n\
                   for await (x in g) { print(x) }").await;
    assert_eq!(out, "1\n2\n3\n");
}
#[tokio::test]
async fn coroutine_receives_resume_values() {
    let out = run("fn* echo(){ let a = yield \"q1\"\n print(a)\n let b = yield \"q2\"\n print(b) }\n\
                   let g = echo()\n print(g.next(nil))\n print(g.next(\"A\"))\n g.next(\"B\")").await;
    // prints: q1, A, q2, B  (order per protocol)
    assert_eq!(out, "q1\nA\nq2\nB\n");
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement.**
  - `Value::Generator(Rc<GeneratorHandle>)` where `GeneratorHandle { chan: YieldChannel, started: Cell<bool> }` and the spawned body future. Identity equality; `type_name` → `"generator"`.
  - Calling a `Function` with `is_generator`: create a `YieldChannel`, `spawn_local` the body future with that channel installed as the task's "current yield sink", and return `Value::Generator`. Do NOT run the body yet beyond the first park (the channel's first `resume` triggers the first step).
  - `ExprKind::Yield(e)`: evaluate `e` (or `nil`), look up the current task's yield channel (a `RefCell<Vec<YieldChannel>>` on `Interp`, pushed when a generator body starts), call `chan.yield_(v).await`, return the resume value. Error (`Control::Panic`) if `yield` is used outside a generator body.
  - Generator methods: `g.next(v)` → `chan.resume(v).await` returning `{value, done}` or the value / `nil`-at-done (pick the `recv()`-style nil sentinel for consistency with native streams). `g.return()`/`g.close()` finishes the channel and drops the body.

- [ ] **Step 4:** Implement `for await (x in e)` in `interp.rs` as desugaring over the async-iterable protocol: if `e` is a `Value::Generator`, loop `g.next(nil)` until done; if `e` is a native stream handle, loop its async `recv()`/`next()` until `nil`. Bind `x`, run body, honor `Break`/`Continue`.

- [ ] **Step 5: Run** both configs → green.

- [ ] **Step 6: Commit**

```bash
git add src/value.rs src/interp.rs
git commit -m "feat(interp): generators + coroutines via yield rendezvous; for await"
```

### Task 4.4: `async fn*` (async generators) + native-stream `for await`

**Files:**
- Modify: `src/interp.rs` (generator body may itself `await`); a net-gated test if `net` is on

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn async_generator_composes() {
    let out = run("import * as time from \"std/time\"\n\
                   async fn* nums(){ yield 1\n await time.sleep(1)\n yield 2 }\n\
                   async fn* dbl(src){ for await (x in src) { yield x*2 } }\n\
                   for await (y in dbl(nums())) { print(y) }").await;
    assert_eq!(out, "2\n4\n");
}
```

- [ ] **Step 2: Run** → FAIL (if composition not wired).

- [ ] **Step 3:** Ensure a generator body that contains `await` works — since the body is already an async future, `await` inside it parks on the I/O future while the generator is between yields. Verify nested `for await` over another generator composes (the `dbl(nums())` pipeline). Fix any channel/borrow issue surfaced.

- [ ] **Step 4: Run** both configs → green; clippy clean (no borrow held across the yield/await).

- [ ] **Step 5: Commit**

```bash
git add src/interp.rs
git commit -m "feat(interp): async generators compose (for await over generators)"
```

---

## Phase 5 — Callback Server Form (net-gated, optional but recommended)

### Task 5.1: `http.serve(handler)` concurrent per-connection tasks

**Files:**
- Modify: `src/stdlib/http_server.rs` (the serve loop @543), docs

- [ ] **Step 1: Write the failing test** — two concurrent clients are served without head-of-line blocking (one slow handler doesn't stall the other). Use the existing test harness pattern (`http_server.rs:1032`) but with a callback handler and concurrent client tasks.

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3:** Add a `serve(handler)` variant that, per accepted connection, `spawn_local`s a task running the script handler. Keep the existing sequential `serve({maxRequests})` for tests. Enforce a concurrency cap (semaphore) and per-request timeout (already present).

- [ ] **Step 4: Run** `cargo test --test ... ` (net feature) → green; `--no-default-features` unaffected.

- [ ] **Step 5: Commit**

```bash
git add src/stdlib/http_server.rs
git commit -m "feat(net): concurrent callback http.serve(handler)"
```

---

## Phase 6 — Docs, CLAUDE.md, Examples, Conformance + Holistic Review

### Task 6.1: User-facing docs

**Files:**
- Create: `docs/content/stdlib/async.md`
- Modify: `docs/content/` language guide (generators/coroutines/await section), `README.md` (stdlib table + feature blurb)

- [ ] **Step 1:** Write `docs/content/stdlib/async.md` covering `future<T>`, `spawn`/`gather`/`race`/`timeout`, `async fn`/`await` semantics (eager scheduling), generators (`fn*`/`yield`), coroutines (resume), `async fn*`, and `for await`. Include the AI-SSE-restream pipeline example (`tokens|sentences|throttle`).
- [ ] **Step 2:** Update the language guide + README stdlib table. Verify the docs site still serves (`cd docs && python3 -m http.server`) and links resolve.
- [ ] **Step 3: Commit**

```bash
git add docs/content README.md
git commit -m "docs: async/generators/coroutines user guide + stdlib/async reference"
```

### Task 6.2: Runnable examples

**Files:**
- Create: `examples/generators.as`, `examples/concurrency.as`, `examples/advanced/sse_stream_pipeline.as`

- [ ] **Step 1:** `examples/concurrency.as` — `gather`/`race`/`timeout`, fully error-handled. `examples/generators.as` — `fn*`, `for await`, a bidirectional coroutine. `examples/advanced/sse_stream_pipeline.as` — composable AI/SSE restream (net-gated; guard with a comment if it needs a server).
- [ ] **Step 2:** Verify each: `target/release/ascript run <file>` (build release first). Ensure conformance tests (which exercise `examples/*.as`) pass for both parsers.
- [ ] **Step 3: Commit**

```bash
git add examples/
git commit -m "docs(examples): concurrency, generators, SSE stream pipeline"
```

### Task 6.3: Finalize CLAUDE.md

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1:** Update the interpreter section: async model is now real cooperative concurrency on a `LocalSet`; document `Value::Future`/`Value::Generator`, the `src/task.rs`/`src/coro.rs` modules, the **"never hold a `RefCell` borrow across `.await`"** invariant + the `await_holding_refcell_ref` deny-lint, the eager-scheduling semantics, and the structured top-level drain. Add `std/task` to the stdlib routing notes. Update the "when adding a stateful native API" note to mention the take-out-across-await rule. Update the Values list (`+future`, `+generator`).
- [ ] **Step 2:** Add the three documented deferrals (durable serialization, unbounded recursion, deterministic scheduling) to the "Current deferrals" line.
- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: CLAUDE.md — M17 async/generators architecture + invariants"
```

### Task 6.4: Holistic review + roadmap close-out + merge

**Files:**
- Modify: `docs/superpowers/roadmap.md`

- [ ] **Step 1:** Run the full gate in BOTH configs:

```bash
cargo test && cargo test --no-default-features
cargo clippy --all-targets && cargo clippy --no-default-features --all-targets
```
Expected: all green, clippy clean (both).

- [ ] **Step 2:** Independent review pass (per project workflow: a reviewer that runs commands and probes edges) — focus on: borrow-across-await audit, generator cancellation/drop (early `break` out of `for await` must drop the body cleanly), panic propagation across task/generator boundaries, and that `--no-default-features` (core language) still builds the bare async core.
- [ ] **Step 3:** Mark M17 complete in `docs/superpowers/roadmap.md`.
- [ ] **Step 4:** Merge the branch `--no-ff` into `main` with a milestone commit message.

```bash
git add docs/superpowers/roadmap.md
git commit -m "docs: mark M17 complete (async concurrency + generators/coroutines)"
# then: git checkout main && git merge --no-ff <branch>
```

---

## Self-Review

**Spec coverage:** Phase 0 rewrites §7; every spec semantic (eager scheduling, real await, structured drain, generators/coroutines, for-await, single-thread invariant) maps to a Phase 1–4 task. `future<T>` (Phase 3) and callback serve (Phase 5) covered. Docs/CLAUDE.md/examples covered in Phase 6. ✅

**Type/name consistency:** `Value::Future(SharedFuture)`, `Value::Generator(Rc<GeneratorHandle>)`, `SharedFuture` (Task 2.1), `YieldChannel` (Task 4.1), builtins `spawn`/`gather`/`race`/`timeout` under `std/task`, AST `ExprKind::Yield(Option<Box<Expr>>)`, `is_generator` on `Fn`/`Arrow`/`MethodDecl`/`Function`, `for_await` on `ForOf`, `Type::Future(Box<Type>)` — used consistently across tasks. ✅

**Placeholder scan:** Kernel code (interior-mutability cells, `SharedFuture`, `YieldChannel`, async-call scheduling, `Await` arm, `Yield` eval) is given in full. Large mechanical sweeps (33 `&mut self` sites; 9 stdlib modules) are specified as an exact pattern + enumeration command + worked example + verification — actionable, not "TBD". ✅

**Known sharp edges flagged inline:** first-`resume` nuance (Task 4.1 NOTE), panic propagation across task boundary (Task 2.2 Step 5), generator drop on early `break` (Task 6.4 Step 2), the behavior-change test (Task 2.4).
