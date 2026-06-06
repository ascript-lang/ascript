# VM Plan V7 — Await & async functions (model 2a, highest risk)

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development.
> **Risk note:** the spec flags async as the risk concentration. This plan keeps the SAME M17 runtime primitives (`SharedFuture`, `spawn_local`, cancel-on-drop) — the VM only changes how the body is *executed* (bytecode vs tree-walk), not the concurrency model. If integration proves thornier than scoped here, split T3/T4 into a dedicated sub-spec before proceeding.

**Goal:** Calling a script `async fn` eagerly schedules a Fiber on the `LocalSet` and returns `Value::Future` immediately (M17 eager scheduling + cancel-on-drop); `AWAIT` drives a `Value::Future`'s `SharedFuture` to completion inline and pushes the result (non-future → identity); native async builtins are awaited inline at the `CALL` site. Structured concurrency (`std/task` spawn/gather/race/timeout, `http.serve`) works unchanged because it operates over `Value::Future`s.

**Architecture:** The `run` loop is already `async fn run(&self, fiber:&mut Fiber)`. An async-fn `CALL` does NOT push a frame on the current Fiber; instead it builds a fresh Fiber for the callee and `spawn_local`s a task that runs it to `Done`, resolving a `SharedFuture` — exactly mirroring the tree-walker's `call_function` async branch (`src/interp.rs:2467`). `AWAIT` pops a value; if `Future`, `.await`s its `SharedFuture::get()` inline; the Fiber is a `&mut` local so no `RefCell` borrow is held across the await (invariant preserved structurally; `clippy::await_holding_refcell_ref` stays denied). **Depends on V6.**

---

## Ground truth (mirror EXACTLY — M17)
- Async-fn call (`src/interp.rs:2467`): `let fut = SharedFuture::new(); let cell = fut.cell(); let guard = self.inflight_guard(); let handle = spawn_local(async move { let _g=guard; let r = run_body(...).await; cell.resolve(r); }); fut.set_abort(handle.abort_handle()); self.maybe_yield_for_inflight().await; Ok(Value::Future(fut))`. The VM's async-fn `CALL` does the identical dance, but `run_body` = `vm.run(&mut Fiber::new(callee_closure_with_args))`.
- `AWAIT` (`src/interp.rs:1617`): `Value::Future(f) => f.get().await`, else identity.
- Native async builtins (e.g. `time.sleep`, http, sqlite) are `Value::Builtin`/`NativeMethod` whose `call_stdlib`/`call_*` are `async` — the VM's `CALL` already `.await`s `interp.call_builtin`/`call_value`, so native async is awaited inline automatically. Verify no native async builtin needs special handling.
- `inflight_guard`/`maybe_yield_for_inflight` (backpressure, INFLIGHT_YIELD_CAP=256 reaper) — reuse the interp's, so un-awaited tasks stay bounded.
- Cancel-on-drop: dropping the last `Value::Future` clone aborts the task (SharedFuture `Drop` + AbortHandle). Unchanged — the VM produces the same `Value::Future`.

---

## Tasks
- [ ] **T1 — `vm.run` reentrancy for spawned Fibers.** Confirm `Vm::run(&self, &mut Fiber)` can be called from a `spawn_local`ed task that owns `Rc<Vm>` (mirror the interp's `self.rc()` self-Weak: add `Vm::rc()` via a self-`Weak`, installed at construction). A spawned async body owns an `Rc<Vm>` + its own `Fiber` and runs to `Done`. Unit test: spawn a trivial async closure Fiber, resolve a SharedFuture, await it. Commit.
- [ ] **T2 — AWAIT opcode.** `AwaitExpr` → compile inner; `AWAIT`. Exec: pop; if `Value::Future(f)` → `let r = f.get().await?; push(r)`; else push the value (identity). CRITICAL: do not hold any `RefCell` borrow across `.await` — the Fiber is `&mut self`-local; the stack `Vec<Value>` push happens AFTER the await returns. Clippy `await_holding_refcell_ref` must stay clean. Tests: `await <future>`, `await 5` (identity). Commit.
- [ ] **T3 — async-fn CALL eager spawn.** In the `CALL` exec arm, when the callee `Closure.proto.is_async` (or a legacy `Function.is_async`): build the callee Fiber with args bound, run the M17 spawn dance (SharedFuture + spawn_local + set_abort + inflight guard + maybe_yield), push `Value::Future`. Arity/contract errors for async fns surface LAZILY (when the future is driven) — match the tree-walker (the spawned task runs the body which checks contracts; the error resolves into the SharedFuture and re-emerges at `await`). Tests: `async fn work(){return 1}\nprint(await work())` → `1`; an async fn that panics → the panic re-emerges at `await` (identical message); a contract violation in an async fn surfaces at await. Commit.
- [ ] **T4 — structured concurrency smoke.** Verify `std/task` `spawn`/`gather`/`race`/`timeout` work over VM-produced `Value::Future`s (they call `call_value` on closures + operate on futures — the V4 bridge + V7 futures should make this transparent). `http.serve` per-connection `spawn_local` + Semaphore: confirm a serve loop's handler closure runs on the VM. Tests: `gather([work(), work()])`, `race`, `timeout`, and a bounded `http.serve({maxRequests:N})` smoke (reuse existing M17 test shapes). Cancel-on-drop: an un-awaited async call is cancelled (RSS/active-task assertion like the M17 leak test). Commit.
- [ ] **T5 — widen differential gate (async).** Add `async.as`, `concurrency.as`, `structured_concurrency.as` to the allow-list. Byte-identical stdout. NOTE: async output ordering must match — the VM uses the SAME single-threaded LocalSet + eager scheduling, so ordering is deterministic and identical; if any ordering differs, it's a real bug (do not relax the gate). Full suite + clippy both configs (incl. `await_holding_refcell_ref` denied). Commit.

## Done criteria (V7)
- [ ] async-fn calls eagerly spawn + return `Value::Future`; `await` drives inline; native async awaited inline; cancel-on-drop intact.
- [ ] Structured concurrency (spawn/gather/race/timeout/`http.serve`) works over VM futures; ordering identical to the tree-walker.
- [ ] `await_holding_refcell_ref` stays denied and clean; differential gate widened; `cargo test` green; clippy clean both configs.

**Next:** V8 — generators: `MAKE_GENERATOR` packages a Suspended Fiber; `YIELD` parks (frames intact); `resume`/`gen.next(v)`/`for await` drive to the next yield; `async fn*` can yield AND await — no `poll_fn`, no thread-local current-generator stack (the Fiber IS the context).
