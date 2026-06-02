# VM Plan V6 — Error model: PROPAGATE (`?`), UNWRAP (`!`), recover, frame-stack unwinding, diagnostics parity

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development.

**Goal:** Implement AScript's error model on the VM identically to the tree-walker: `?` (`PROPAGATE`) does a function-level early return of `[nil, err]`; `!` (`UNWRAP`) raises a recoverable panic carrying `err`'s message; `recover(fn)` (native builtin over `call_value`) catches `Control::Panic` and returns `[value, err]`, passing `Propagate`/`Exit` through; uncaught panics unwind the explicit Fiber frame stack (closing upvalue cells) to the driver, producing the SAME ariadne message + source location as the tree-walker.

**Architecture:** Reuse `Control { Panic(AsError), Propagate(Value), Exit(i32) }` unchanged. The `run` loop already returns `Result<RunOutcome,Control>`. `PROPAGATE`/`UNWRAP` are opcodes; `recover` stays a native builtin (per spec — first-class, over `call_value`). Unwinding: when an exec arm returns `Err(Control::Panic|Propagate)`, the `run` loop pops frames (closing cells) until a catch boundary or empties → returns `Err` to the driver. `recover` catches by virtue of being a native call boundary: `Vm::call_value(closure)` returns `Err(Panic)` to the `recover` builtin, which converts it. **Depends on V5.**

---

## Ground truth (from survey, mirror EXACTLY)
- `?` (tree-walker `ExprKind::Try`, `src/interp.rs:1650`): pop value; require a 2-elem `[value, err]` array (else identical panic "the ? operator requires a Result pair [value, err]"); if `err == Nil` → push value; else `Err(Control::Propagate([nil, err]))` (function-level early return).
- `!` (`ExprKind::Unwrap`, `:1674`): same pair requirement; `err==Nil` → value; else `Err(Panic(AsError::at(error_message(&err), span)))` — recoverable.
- `recover` (builtin, `:2848`): `call_value(callee, [])` → `Ok(v)` → `[v, nil]`; `Err(Panic(e))` → `[nil, {message:e}]`; `Err(Propagate(v))` → re-`Err(Propagate(v))`; `Err(Exit(c))` → re-`Err(Exit(c))`. ONLY Panic is caught.
- `make_pair`/`make_error`/`error_message` helpers in interp — reuse them (or equivalents) so the `[value,err]` shapes are byte-identical.
- Diagnostics: a top-level uncaught panic → `Err(AsError)` to the driver → ariadne via the span table. Message + location must equal the tree-walker's.

---

## Tasks
- [ ] **T1 — PROPAGATE (`?`).** `TryExpr` → compile inner; emit `PROPAGATE`. Exec: pop; validate 2-elem array (else identical panic); `err==Nil` → push value; else build `[nil, err]` and unwind the CURRENT FRAME as a return of that pair (pop frame closing cells, push the pair in the caller, resume) — i.e. `PROPAGATE` is a conditional `RETURN` of `[nil,err]`. Confirm: in the tree-walker `Propagate` unwinds to the function boundary; replicate by returning `Control::Propagate` from the arm and having the `run` loop treat `Propagate` like a `RETURN` at the nearest frame (NOT all the way out) — careful: `Propagate` in the tree-walker ends the CURRENT function and yields the pair as that function's result. Implement: on `Propagate(pair)`, pop one frame, push `pair` as its result; if at top frame → it's the program result (matches `run_file` treating top-level Propagate as Ok). Tests: `fn f(): Result<number> { let [v,e] = g()?; ... }`; propagation chains; the non-pair panic. Commit.
- [ ] **T2 — UNWRAP (`!`).** `UnwrapExpr` → `UNWRAP`. Exec: pop; validate pair; `err==Nil` → push value; else `Err(Control::Panic(AsError::at(error_message(&err), span)))`. Tests: `x!` success + failure (identical panic message + location). Commit.
- [ ] **T3 — frame-stack unwinding for Panic.** In `run`, when an exec arm returns `Err(Control::Panic(e))`: pop frames (closing each frame's cells via `CLOSE_UPVALUE` semantics) until frames empty, then return `Err(Panic(e))` to the driver. (There is no in-VM catch frame; `recover` catches at the native `call_value` boundary — T4.) Ensure the span attached is the faulting instruction's. Tests: uncaught panic from deep in a call chain → identical ariadne output (compare against tree-walker via a diagnostics-parity test). Commit.
- [ ] **T4 — recover over call_value.** `recover` is already a builtin; ensure `Vm::call_value(closure, [])` returns `Err(Control::Panic)` up to the `recover` builtin (which the VM invokes via the interp's `call_builtin`), which converts per the rules. Since `recover` runs through `interp.call_builtin("recover",..)` and the closure arg is a `Value::Closure`, the interp's `call_value` routes back to `Vm::call_value` (V4 bridge) — verify Panic propagates across that boundary intact and `Propagate`/`Exit` pass through. Tests: `recover(() => x!)` where x has an error → `[nil, err]`; `recover(() => 1)` → `[1, nil]`; `recover(() => exit(2))` → Exit passes through; a `?` inside the recovered fn → Propagate passes through. Commit.
- [ ] **T5 — diagnostics parity gate.** Add a test module asserting, for a set of panicking programs (unwrap of error, type-contract violation, bad `?`), that the VM's `AsError` message + span (line/col) equal the tree-walker's. Reuse the span table. Commit.
- [ ] **T6 — widen differential gate.** Add `result.as`, `force_unwrap.as`, `validation.as` (sync parts) to the allow-list. Byte-identical stdout AND identical exit codes (a top-level panic exits 1 with the same message). Full suite + clippy both configs. Commit.

## Done criteria (V6)
- [ ] `?`/`!`/`recover` behave identically to the tree-walker (incl. the `[value,err]` shapes, Propagate/Exit pass-through, recoverable vs unrecoverable).
- [ ] Uncaught panics unwind the Fiber frame stack and produce byte-identical ariadne diagnostics (message + location).
- [ ] Differential gate widened (stdout + exit code); `cargo test` green; clippy clean both configs.

**Next:** V7 — await & async functions (model 2a): async-fn CALL eagerly spawns a Fiber + returns `Value::Future`; `AWAIT` drives a `SharedFuture` inline; native async builtins awaited inline; structured concurrency reused.
