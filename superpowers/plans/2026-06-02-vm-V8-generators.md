# VM Plan V8 — Generators: MAKE_GENERATOR, YIELD, resume, for-await, async generators

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development.

**Goal:** Implement `fn*`/`async fn*` on the VM as **consumer-driven Suspended Fibers**: `MAKE_GENERATOR` packages a not-yet-started Fiber as a `Value::Generator`; `gen.next(v)`/`gen.resume(v)`/`for await x of gen`/`for x of gen` drive the Fiber to its next `YIELD` (→ `Some(value)`, frames intact) or completion (→ `None`); `gen.close()` drops the Fiber. An `async fn*` can both `yield` and `await` internally. This DELETES `coro.rs`'s `poll_fn` parking + thread-local current-generator stack — the Fiber's explicit frame stack IS the suspension context.

**Architecture:** This is where the Fiber model pays off vs M17's tree-walker generators. A generator is a `Fiber` in `state: Suspended`. `YIELD` returns `RunOutcome::Yielded(value)` from `run` WITHOUT unwinding frames (they stay in the Fiber). `resume(gen, input)` pushes `input` where the yield expression's result goes, sets the Fiber Running, and calls `run` again → it continues from the saved `ip`. Replace `Value::Generator(Rc<GeneratorHandle>)`'s internals: introduce a VM-backed generator holding `RefCell<Fiber>` + the `Rc<Vm>` to drive it. Keep the `Value::Generator` variant + its method surface (`next`/`resume`/`close`/`for await`) so the stdlib/`for-of` integration is unchanged. **Depends on V7.**

---

## Ground truth
- M17 generators (`src/coro.rs`): `GeneratorHandle` stores a pinned body future, driven by `resume` via `poll_fn`; `yield` looks up the current generator from a thread-local stack. `Value::Generator(Rc<GeneratorHandle>)`, `Value::GeneratorMethod(handle, "next"|"resume"|"close")`. `for await` drives via `resume`.
- The VM replaces the *mechanism* (Fiber, not poll_fn) but MUST preserve observable behavior: yielded sequence, return value handling, `gen.next(v)` injecting `v` as the yield expression's value, early `close()`, and `for await`/`for` iteration. Differential gate enforces identical output.
- An `async fn*`: `yield` parks the Fiber; `await` inside drives a sub-future inline (the Fiber's `run` is async, so `AWAIT` works inside a generator Fiber too). Confirm the tree-walker's async-generator semantics and mirror.

---

## Tasks
- [ ] **T1 — Generator-backed Fiber + Value::Generator internals.** Define a VM generator: `struct VmGenerator { fiber: RefCell<Fiber>, vm: Rc<Vm>, started: Cell<bool>, done: Cell<bool> }`. Decide how `Value::Generator` holds it: either reuse the existing `Rc<GeneratorHandle>` variant by making `GeneratorHandle` wrap a VM generator (preferred — no value.rs change), or add a thin internal type behind the same variant. Keep the variant + `GeneratorMethod` surface. Commit `feat(vm): generator-backed Fiber scaffold`.
- [ ] **T2 — MAKE_GENERATOR + YIELD.** `fn*`/`async fn*` CALL: instead of running, build the callee Fiber (args bound, NOT started) and `MAKE_GENERATOR` → push `Value::Generator`. `YieldExpr` → compile operand (or nil); `YIELD`. Exec `YIELD`: return `Ok(RunOutcome::Yielded(value))` from `run` WITHOUT popping frames; the driver records the value and leaves the Fiber Suspended with `ip` past the YIELD. On resume, the injected input becomes the YIELD expression's value (push it). Tests via a manual drive loop: a `fn* g(){ yield 1; yield 2 }` yields 1 then 2 then done. Commit.
- [ ] **T3 — resume / gen.next(v) / close.** Implement `Vm::resume(gen, input) -> Result<Option<Value>, Control>`: if done → None; if not started → start (ignore input or per tree-walker's first-next semantics — verify); push input as the pending yield result; set Running; `run` → `Yielded(v)` → `Some(v)` (stay Suspended); `Done(ret)` → mark done → None (and the return value handling per tree-walker — does a generator's `return x` surface? verify). `gen.next(v)`/`gen.resume(v)` builtins route here. `gen.close()` drops the Fiber (Done). Tests: `gen.next()` sequence, value injection (`let x = yield` receiving the next input), early close. Commit.
- [ ] **T4 — for-of / for-await over generators.** Wire V3's `for-of` iteration protocol to drive a `Value::Generator` via `resume` (sync `for x of gen` and `for await x of gen`). For `for await`, each `resume` may itself await internally (async generator). Confirm iteration matches the tree-walker for both forms. Tests: `for x of gen {...}`, `for await x of asyncGen {...}`. Commit.
- [ ] **T5 — async generators (yield + await).** Verify an `async fn*` Fiber can `AWAIT` inside (drive a sub-future inline) AND `YIELD`. Since the generator Fiber's `run` is async, `AWAIT` works; ensure `resume` is async and awaits the inner work before returning the yielded value. Tests: `async fn* g(){ let a = await fetch(); yield a; ... }`. Commit.
- [ ] **T6 — widen differential gate + retire coro.rs path (alongside).** Add `generators.as`, `generators_test.as` to the allow-list. Byte-identical. Do NOT delete `coro.rs` yet (the tree-walker still uses it until cutover) — but the VM must not depend on its `poll_fn`/thread-local. Confirm no thread-local current-generator usage in the VM path. Full suite + clippy both configs. Commit.

## Done criteria (V8)
- [ ] `fn*`/`async fn*` run as Suspended Fibers; yield/resume/next(v)/close/for-of/for-await all identical to the tree-walker; async generators yield+await.
- [ ] VM generator path uses NO `poll_fn`/thread-local current-generator stack.
- [ ] Differential gate widened; `cargo test` green; clippy clean both configs.

**Next:** V9 — classes/enums/super: `CLASS`/`METHOD`/`GET_SUPER`/`INSTANCE_OF`, typed fields + defaults, `init`, `.from` validation, enums + variants, bound methods — reusing the existing class/contract/schema semantics on the VM call path.
