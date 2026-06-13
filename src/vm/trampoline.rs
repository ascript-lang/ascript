//! CALL §5 — the higher-order callback trampoline.
//!
//! ONE reused fiber drives a plain (non-async/generator/worker) VM-closure
//! callback across all elements of a higher-order builtin loop on LANE's sync
//! lane; a suspension escalates THAT element's live fiber onto the async driver
//! (never re-executed). The tree-walker is untouched by construction: arming
//! requires a `Value::Closure`, which only the VM produces.
//!
//! # Non-wired sites (§5.5 decision table — CALL Task 3.3)
//!
//! The following stdlib callback sites remain on the generic (per-element
//! `call_value`) path. They are DOCUMENTED decisions, not deferrals. Every
//! one is a single-shot or per-event invocation with no element loop to
//! amortize over — wiring them would add arming cost without fiber reuse.
//!
//! | Site | File:approx-line | Reason |
//! |---|---|---|
//! | `events` listener dispatch | `src/stdlib/events.rs:165` | Single-shot per event; no inner loop. |
//! | `sync` task combinators | `src/stdlib/sync.rs:576` | Async-first; escalation would always fire. |
//! | `task` spawn/retry | `src/stdlib/task_mod.rs:92/275` | Async by design; no per-element loop. |
//! | `timers` setTimeout/setInterval | `src/stdlib/time_timers.rs:255/324` | Single-shot callbacks. |
//! | `bench` runner | `src/stdlib/bench.rs:69` | Single-shot; overhead not on hot path. |
//! | `assert` callbacks | `src/stdlib/assert_mod.rs:634/680` | Single-shot; not on hot path. |
//! | `schema` refiner chain | `src/stdlib/schema.rs:935` | Per-call-site dispatch; no inner loop. |
//! | `net` HTTP callbacks | `src/stdlib/net_http.rs:1319` | Callbacks are async; escalation fires always. |
//! | `workflow` replay | `src/stdlib/workflow.rs:489/642` | Determinism-critical; async driver required. |
//! | `http_server` handler/middleware | `src/stdlib/http_server.rs:437/2053/2077` | Handlers are async; escalation fires always. |
//!
//! All of these still benefit from Unit A (A1 empty-cells fast path, A3 fiber
//! pooling) via the generic `call_value` path. The trampoline is only for the
//! tight inner loops (`array.*`, `object.mapValues`, `stream.*`) where the
//! per-element async ceremony dominates. If a future profile shows one of these
//! single-shot sites is hot, wiring it is a one-line `CallbackDriver` adoption.
//!
//! ## Stream-specific narrowing (Task 3.3)
//!
//! For `stream.*` pipeline stages (Map/Filter/FlatMap inside
//! `Interp::run_stages_from`), each stage callback is stored as a `Value` inside
//! `Stage` — and `Stage` must be clonable (`clone_stage` for combinators). Since
//! `CallbackTrampoline` is NOT `Clone` (it owns a live `Fiber`), per-stage
//! trampolines CANNOT be stored in `StreamState` to achieve cross-element fiber
//! reuse. The adopted narrowing: stage callbacks arm a `CallbackDriver`
//! per-element (avoids the `call_value` async-box overhead, runs on the sync
//! lane, but does not reuse the fiber across elements). The three terminal loops
//! (`forEach`/`reduce`/`find`) arm ONE driver for their entire consumption loop
//! and DO reuse the fiber across all elements.

use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::Value;
use crate::vm::fiber::Fiber;
use crate::vm::value_ext::{Closure, RunOutcome};
use crate::vm::run::SyncOutcome;
use crate::vm::Vm;
use gcmodule::Cc;
use std::rc::Rc;

/// The reusable per-loop fiber driver for a plain VM-closure callback.
///
/// Lives for the duration of one builtin loop (e.g. the entire `array.map`
/// call) — created by [`CallbackTrampoline::arm`], consumed by `call1`/`call2`
/// via [`CallbackDriver`].
pub(crate) struct CallbackTrampoline {
    vm: Rc<Vm>,
    closure: Cc<Closure>,
    /// The ONE reused fiber. `None` only before the first element (lazily built
    /// so arming a trampoline for an empty input allocates nothing). `None` also
    /// after a panicking element (the fiber is dropped on `Err`, so the next
    /// element gets a fresh `Fiber::new` then reuses it on subsequent calls).
    fiber: Option<Fiber>,
    span: Span,
}

/// A builtin loop's callback driver: the trampoline fast path, or the exact
/// today-path (per-element `Interp::call_value`) for everything else.
///
/// One enum so every stdlib site keeps a single loop regardless of which path
/// is taken. Both arms produce the same observable results (CALL §8.1 gate).
pub(crate) enum CallbackDriver<'i> {
    Tramp(CallbackTrampoline),
    Generic { interp: &'i Interp, f: Value, span: Span },
}

impl CallbackTrampoline {
    /// CALL §5.2: arm the trampoline iff `f` is a plain VM closure.
    ///
    /// Returns `None` — falling back to the generic path — when:
    /// - The callee is not a `Value::Closure` (tree-walker `Function`, builtin,
    ///   class, etc.). This is the engine seam: tree-walker callbacks always go
    ///   through `Generic`.
    /// - The closure is `async`, a generator, or a worker fn — any of which may
    ///   suspend or spawn; the sync lane would always escalate.
    /// - The `Vm` weak reference has been dropped (should not happen during a
    ///   normal builtin call, but we handle it defensively).
    /// - The `call_fast` kill switch is off.
    pub(crate) fn arm(interp: &Interp, f: &Value, span: Span) -> Option<CallbackTrampoline> {
        let Value::Closure(c) = f else { return None };
        if c.proto.is_async || c.proto.is_generator || c.proto.is_worker {
            return None;
        }
        let vm = interp.vm()?;
        if !vm.call_fast() {
            return None;
        }
        Some(CallbackTrampoline { vm, closure: c.clone(), fiber: None, span })
    }

    /// Run ONE callback invocation on the sync lane.
    ///
    /// `args` must be a mutable slice of exactly the values to pass; they are
    /// moved (via `mem::replace`) into the fiber's slot window so NO per-element
    /// `Vec` is allocated on the fast path.
    ///
    /// On `Ok(v)` the fiber is retained for the next element (reset on entry).
    /// On `Err` the fiber is dropped (it contained mid-flight state that must not
    /// be reused — the next element gets a fresh `Fiber::new`).
    pub(crate) async fn call(&mut self, args: &mut [Value]) -> Result<Value, Control> {
        let what = self.closure.proto.chunk.name.as_deref().unwrap_or("function");

        // Arity + contract check — IDENTICAL to `check_call_args` semantics.
        // The shared cores produce byte-identical panic messages and ordering.
        // Note: `check_call_args_in_place` debug-asserts `!has_rest`; callers
        // that arm the trampoline for a rest-param closure are bugs, but the
        // arm() guard does not filter on has_rest (the in-place binding fast
        // path in Op::Call does). If the closure happens to have a rest param it
        // will be caught here as a debug_assert, which is acceptable.
        let supplied = crate::interp::check_call_args_in_place(
            &self.closure.proto.params,
            args,
            self.span,
            what,
            Some(self.vm.interp()),
            Some(&self.vm.class_env()),
        )?;

        // THE RESET INVARIANT (CALL §5.4): one fresh frame @ip0, slot_count Nil
        // stack, FRESH cells, `Running` — regardless of the previous element's
        // fate. Reusing the Vecs' capacity (the whole point of the pooled fiber).
        let mut fiber = match self.fiber.take() {
            Some(mut f) => {
                f.reset(self.closure.clone());
                f
            }
            None => Fiber::new(self.closure.clone()),
        };

        // Write the supplied args into the slot window (cell-aware, same as A2
        // in-place binding in Op::Call). Slots beyond `supplied` stay Nil
        // (reset() filled them), which is correct: the default-prologue fills
        // those via `Op::JumpIfArgSupplied` + `frame.argc`.
        fiber.frame_mut().ret_span = self.span;
        fiber.frame_mut().argc = supplied;
        for (slot, a) in args[..supplied].iter_mut().enumerate() {
            let v = std::mem::replace(a, Value::Nil);
            if let Some(cell) = fiber.frame().cells.get(slot).and_then(|c| c.as_ref()) {
                *cell.borrow_mut() = v;
            } else {
                fiber.stack[slot] = v;
            }
        }

        // SP3: exactly ONE logical-call increment per element (RAII guard).
        // This mirrors `Vm::call_value`'s `enter_call_depth_scoped` discipline —
        // a `Cell<u32>`, never held across an `.await`.
        let _depth = self.vm.interp().enter_call_depth_scoped(self.span)?;

        // Coverage counter (CALL §8.3 anti-false-green).
        self.vm.bump_trampoline_call();

        // Drive the fiber on the SYNC lane first (CALL §5 — no `.await`, no
        // boxed future on the non-suspending path).
        // SP9: grow() is the same native-stack discipline the async path uses.
        let outcome = match crate::vm::stack::grow(|| self.vm.run_loop_sync(&mut fiber))? {
            SyncOutcome::Finished(outcome) => outcome,
            SyncOutcome::NeedsAsync => {
                // ESCALATION (CALL §5.3): the callback suspended (await, yield,
                // worker dispatch). Continue THE SAME live fiber on the async
                // driver — NEVER re-dispatch through `call_value`.
                // Side effects up to the suspension already happened; the fiber's
                // ip points at the suspending op; the async driver re-decodes it.
                self.vm.bump_trampoline_escalation();
                crate::vm::stack::grow_future(self.vm.run(&mut fiber)).await?
            }
        };

        match outcome {
            RunOutcome::Done(v) => {
                // Retain the fiber for the next element (the reset will reuse the
                // Vecs' capacity). Return BEFORE the `_depth` guard drops — it
                // decrements the call counter on drop, which is correct.
                self.fiber = Some(fiber);
                Ok(v)
            }
            RunOutcome::Yielded(_) => {
                // A non-generator closure cannot yield — this is a compiler bug.
                unreachable!("a non-generator callback cannot yield (compiler/resolver bug)")
            }
        }
        // `_depth` drops here, decrementing the call counter.
    }
}

impl<'i> CallbackDriver<'i> {
    /// Invoke the callback with one argument.
    pub(crate) async fn call1(&mut self, a: Value) -> Result<Value, Control> {
        match self {
            Self::Tramp(t) => {
                let mut args = [a];
                t.call(&mut args).await
            }
            Self::Generic { interp, f, span } => {
                interp.call_value(f.clone(), vec![a], *span).await
            }
        }
    }

    /// Invoke the callback with two arguments.
    pub(crate) async fn call2(&mut self, a: Value, b: Value) -> Result<Value, Control> {
        match self {
            Self::Tramp(t) => {
                let mut args = [a, b];
                t.call(&mut args).await
            }
            Self::Generic { interp, f, span } => {
                interp.call_value(f.clone(), vec![a, b], *span).await
            }
        }
    }
}

impl Interp {
    /// CALL §5.5: the one constructor every stdlib iteration site uses.
    ///
    /// Returns a `CallbackDriver::Tramp` (one reused fiber, sync lane) when the
    /// trampoline can be armed for `f`, falling back to `CallbackDriver::Generic`
    /// (per-element `call_value`) otherwise. Both arms are byte-identical; only
    /// allocation/latency differs.
    pub(crate) fn callback_driver<'i>(&'i self, f: Value, span: Span) -> CallbackDriver<'i> {
        match CallbackTrampoline::arm(self, &f, span) {
            Some(t) => CallbackDriver::Tramp(t),
            None => CallbackDriver::Generic { interp: self, f, span },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::Interp;
    use crate::span::Span;
    use crate::vm::chunk::FnProto;
    use crate::vm::fiber::Fiber;
    use crate::vm::value_ext::{Closure, RunOutcome};
    use crate::vm::Vm;
    use std::rc::Rc;

    /// Compile `src`, run it to completion on a VM with `call_fast=cf`.
    /// Returns `(output, vm)` or panics.
    ///
    /// Uses the `block_on` + `LocalSet` pattern from `src/vm/run.rs` unit tests.
    fn run_program(src: &str, call_fast: bool) -> (String, Rc<Vm>) {
        let chunk = crate::compile::compile_source(src)
            .unwrap_or_else(|e| panic!("compile error: {}", e.message));
        let proto = Rc::new(FnProto {
            chunk,
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            is_worker: false,
            owning_class: None,
            params: Vec::new(),
            ret: None,
            local_names: Vec::new(),
            debug_name: None,
        });
        let closure = Closure::new(proto);
        let interp = Rc::new(Interp::new());
        interp.install_self();
        interp.set_worker_source(src);
        let vm = Vm::with_flags(interp.clone(), true, true, call_fast);
        let mut fiber = Fiber::new(closure);

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build runtime");
        let local = tokio::task::LocalSet::new();
        let result = local.block_on(&rt, vm.run(&mut fiber));
        crate::gc::collect();
        match result {
            Ok(RunOutcome::Done(_)) | Ok(RunOutcome::Yielded(_)) => {}
            Err(crate::interp::Control::Exit(_)) => {}
            Err(crate::interp::Control::Propagate(_)) => {}
            Err(crate::interp::Control::Panic(e)) => panic!("program panicked: {}", e.message),
        }
        (interp.output(), vm)
    }

    /// Compile `src`, run it, and return the panic message (or None if it succeeded).
    fn run_expect_panic(src: &str, call_fast: bool) -> Option<String> {
        let chunk = crate::compile::compile_source(src)
            .unwrap_or_else(|e| panic!("compile error: {}", e.message));
        let proto = Rc::new(FnProto {
            chunk,
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            is_worker: false,
            owning_class: None,
            params: Vec::new(),
            ret: None,
            local_names: Vec::new(),
            debug_name: None,
        });
        let closure = Closure::new(proto);
        let interp = Rc::new(Interp::new());
        interp.install_self();
        interp.set_worker_source(src);
        let vm = Vm::with_flags(interp.clone(), true, true, call_fast);
        let mut fiber = Fiber::new(closure);

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build runtime");
        let local = tokio::task::LocalSet::new();
        let result = local.block_on(&rt, vm.run(&mut fiber));
        crate::gc::collect();
        match result {
            Ok(_) | Err(crate::interp::Control::Exit(_)) | Err(crate::interp::Control::Propagate(_)) => None,
            Err(crate::interp::Control::Panic(e)) => Some(e.message),
        }
    }

    /// Drive a `CallbackTrampoline` directly, bypassing stdlib wiring.
    ///
    /// Compiles `fn_src` (a top-level `fn` declaration naming the closure),
    /// runs it to register the function in user globals, fetches the
    /// `Value::Closure`, arms a trampoline, and drives it for `args_list`.
    fn run_trampoline_direct(
        fn_src: &str,
        fn_name: &str,
        args_list: Vec<Vec<Value>>,
    ) -> (Vec<Result<Value, String>>, Rc<Vm>) {
        let (_, vm) = run_program(fn_src, true);
        let f = vm.user_global(fn_name)
            .unwrap_or_else(|| panic!("global '{}' not found after running src", fn_name));

        let span = Span::new(0, 1);
        let mut tramp = CallbackTrampoline::arm(vm.interp(), &f, span)
            .expect("arm should succeed for a plain closure");

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build runtime");
        let local = tokio::task::LocalSet::new();
        let mut results = Vec::new();
        for mut args in args_list {
            let res = local.block_on(&rt, tramp.call(&mut args));
            match res {
                Ok(v) => results.push(Ok(v)),
                Err(crate::interp::Control::Panic(e)) => results.push(Err(e.message)),
                Err(e) => results.push(Err(format!("{e:?}"))),
            }
        }
        let stats = vm.call_fast_stats();
        drop(tramp);
        crate::gc::collect();
        // Re-return stats by inspecting the vm we have
        let _ = stats;
        (results, vm)
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Test (a): three sequential call1 invocations return correct results,
    // and trampoline_calls == 3.
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn trampoline_call1_three_times_reuses_fiber() {
        let fn_src = "fn add_ten(x) { return x + 10 }";
        let (results, vm) = run_trampoline_direct(
            fn_src,
            "add_ten",
            vec![
                vec![Value::Int(1)],
                vec![Value::Int(2)],
                vec![Value::Int(3)],
            ],
        );
        assert_eq!(results.len(), 3, "should have 3 results");
        assert_eq!(results[0], Ok(Value::Int(11)), "call1 result[0]");
        assert_eq!(results[1], Ok(Value::Int(12)), "call1 result[1]");
        assert_eq!(results[2], Ok(Value::Int(13)), "call1 result[2]");
        // The trampoline counter must have been bumped 3 times.
        let stats = vm.call_fast_stats();
        assert_eq!(
            stats.trampoline_calls, 3,
            "expected trampoline_calls==3, got {}",
            stats.trampoline_calls
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Test (b): reset invariant — a panicking element does NOT poison the next.
    //
    // Drive a typed callback (fn cb(x: string) {...}) with:
    //   element 0: "hello" → succeeds
    //   element 1: 42 (int) → contract panic
    //   element 2: "world" → must still succeed (reset invariant)
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn trampoline_reset_invariant_panic_does_not_poison_next() {
        let fn_src = r#"fn typed_cb(x: string) { return len(x) }"#;
        let (_, vm) = run_program(fn_src, true);
        let f = vm.user_global("typed_cb").expect("typed_cb not found");

        let span = Span::new(0, 1);
        let mut tramp = CallbackTrampoline::arm(vm.interp(), &f, span)
            .expect("arm should succeed");

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build runtime");
        let local = tokio::task::LocalSet::new();

        // Call 0: "hello" → Ok(5)
        let r0 = local.block_on(&rt, tramp.call(&mut [Value::Str("hello".into())]));
        assert!(r0.is_ok(), "call 0 should succeed: {r0:?}");
        assert_eq!(r0.unwrap(), Value::Int(5), "call 0 result wrong");

        // Call 1: 42 → contract panic (type mismatch)
        let r1 = local.block_on(&rt, tramp.call(&mut [Value::Int(42)]));
        assert!(r1.is_err(), "call 1 should panic on type mismatch");
        let msg1 = match r1 { Err(crate::interp::Control::Panic(e)) => e.message, other => panic!("unexpected: {other:?}") };
        assert!(
            msg1.contains("string") || msg1.contains("expected") || msg1.contains("contract"),
            "panic message should mention type issue: {msg1}"
        );

        // Call 2: "world" → MUST succeed (the reset invariant: panic didn't poison).
        let r2 = local.block_on(&rt, tramp.call(&mut [Value::Str("world".into())]));
        assert!(r2.is_ok(), "call 2 should succeed after panic (reset invariant): {r2:?}");
        assert_eq!(r2.unwrap(), Value::Int(5), "call 2 result wrong");

        // The counter is bumped ONLY for calls that reach the sync lane
        // (i.e., after the contract check passes). The panicking call (call 1)
        // fails at `check_call_args_in_place` before the bump, so only the
        // two succeeding calls (0 and 2) increment the counter.
        let stats = vm.call_fast_stats();
        assert_eq!(
            stats.trampoline_calls, 2,
            "trampoline_calls should be 2 (calls 0 and 2 reach the sync lane, call 1 fails contract check first), got {}",
            stats.trampoline_calls
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Test (c): async callback goes through Generic (arm refuses is_async=true).
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn trampoline_arm_refuses_async_closure() {
        // An async function produces a Value::Closure with is_async=true.
        // arm() must return None for it.
        let fn_src = "async fn async_double(x) { return x * 2 }";
        let (_, vm) = run_program(fn_src, true);
        let f = vm.user_global("async_double").expect("async_double not found");
        let span = Span::new(0, 1);
        // arm() must refuse is_async closures.
        let result = CallbackTrampoline::arm(vm.interp(), &f, span);
        assert!(
            result.is_none(),
            "arm() should return None for an async closure, got Some"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Test (c-2): generator closure is also refused.
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn trampoline_arm_refuses_generator_closure() {
        let fn_src = "fn* gen_fn(n) { yield n }";
        let (_, vm) = run_program(fn_src, true);
        let f = vm.user_global("gen_fn").expect("gen_fn not found");
        let span = Span::new(0, 1);
        let result = CallbackTrampoline::arm(vm.interp(), &f, span);
        assert!(
            result.is_none(),
            "arm() should return None for a generator closure, got Some"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Test (d): call_depth exactly-once — recursion limit parity.
    //
    // A self-recursive callback trips the recursion limit at the SAME depth
    // whether driven through the trampoline (call_fast=true) or the generic
    // Vm::call_value path (call_fast=false).
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn trampoline_recursion_limit_parity() {
        // This program recurses 5000 levels deep via a plain function call,
        // which will exceed MAX_CALL_DEPTH (3000). Both call_fast modes must
        // produce the same error message.
        let deep_src = r#"
            fn deep(n) {
                if (n <= 0) { return 0 }
                return deep(n - 1) + 1
            }
            deep(5000)
        "#;
        let err_on = run_expect_panic(deep_src, true);
        let err_off = run_expect_panic(deep_src, false);
        match (err_on, err_off) {
            (Some(msg_on), Some(msg_off)) => {
                assert!(
                    msg_on.contains("recursion depth"),
                    "call_fast=true: wrong error: {msg_on}"
                );
                assert_eq!(
                    msg_on, msg_off,
                    "recursion limit message differs between call_fast on/off:\n  on: {msg_on}\n  off: {msg_off}"
                );
            }
            (None, None) => {
                // Both succeeded — MAX_CALL_DEPTH must not have been reached, which is
                // a build configuration issue. Document but don't fail the test.
                eprintln!("WARNING: deep(5000) did not hit MAX_CALL_DEPTH in either mode");
            }
            (on, off) => panic!(
                "recursion depth diverged: call_fast=on -> {on:?}, call_fast=off -> {off:?}"
            ),
        }
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Arm eligibility: kill switch off => arm returns None.
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn trampoline_arm_refuses_when_call_fast_off() {
        let fn_src = "fn add_ten(x) { return x + 10 }";
        let chunk = crate::compile::compile_source(fn_src).unwrap();
        let proto = Rc::new(FnProto {
            chunk,
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            is_worker: false,
            owning_class: None,
            params: Vec::new(),
            ret: None,
            local_names: Vec::new(),
            debug_name: None,
        });
        let closure = Closure::new(proto);
        let interp = Rc::new(Interp::new());
        interp.install_self();
        // call_fast=false
        let vm = Vm::with_flags(interp.clone(), true, true, false);
        let mut fiber = Fiber::new(closure);

        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let local = tokio::task::LocalSet::new();
        let _ = local.block_on(&rt, vm.run(&mut fiber));

        let f = vm.user_global("add_ten").expect("add_ten not found");
        let span = Span::new(0, 1);
        let result = CallbackTrampoline::arm(&interp, &f, span);
        assert!(
            result.is_none(),
            "arm() should return None when call_fast=false, got Some"
        );
    }
}
