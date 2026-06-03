//! The VM's async run loop (`Vm::run`).
//!
//! V2 implements the **synchronous core**: constants, literal pushes, stack
//! `Pop`/`Dup`, locals/globals, calls, templates, the full binary/unary operators
//! (string concat / decimal / range / cross-type equality / numeric) and `Return`.
//! Every other opcode is a documented `not yet implemented` Tier-2 panic that
//! later VM slices fill in. Panics carry the faulting instruction's [`Span`] so
//! ariadne points at the source exactly like the tree-walker.
//!
//! The binary/unary arms call the SAME `apply_binop`/`apply_unop` free functions
//! the tree-walker uses (`src/interp.rs`), so the two engines cannot drift on
//! arithmetic semantics or panic messages — there is one implementation.

use crate::ast::{BinOp, UnOp};
use crate::error::AsError;
use crate::interp::{error_message, Control, Interp};
use crate::span::Span;
use crate::value::Value;
use crate::vm::fiber::Fiber;
use crate::vm::opcode::Op;
use crate::vm::value_ext::{Closure, RunOutcome};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// The bytecode virtual machine.
///
/// Holds the shared [`Interp`] (the runtime state the VM and tree-walker share)
/// and a self-`Weak` mirroring [`Interp`]'s pattern, so a `&self` method can
/// recover an owned `Rc<Vm>` to hand to a spawned task in V7.
pub struct Vm {
    interp: Rc<Interp>,
    self_weak: RefCell<Weak<Vm>>,
    /// Per-class compiled-method table (V9). `value.rs`'s `Class`/`Method` is
    /// frozen and holds a TREE-WALKER body the VM cannot run, so the VM compiles
    /// each method to a `Value::Closure` and stores it HERE instead — keyed by the
    /// class's `Rc` IDENTITY (`Rc::as_ptr` address) → method name → compiled
    /// closure. A class's `Value::Class.methods` map is left empty; method dispatch
    /// goes through this table (`compiled_method`). The key is stable because the
    /// `Rc<Class>` is created once at compile time and shared by every instance.
    class_methods: RefCell<HashMap<usize, HashMap<String, Rc<Closure>>>>,
    /// Per-class field-default thunk table (V9): class `Rc` identity → field name →
    /// a zero-arg closure that produces the field's default value. Run once per
    /// constructed instance (so a mutable default yields a fresh value each time,
    /// matching the tree-walker's per-construct default eval).
    class_defaults: RefCell<HashMap<usize, HashMap<String, Rc<Closure>>>>,
}

impl Vm {
    /// Build a VM over `interp` and install its self-`Weak` (mirroring
    /// [`Interp::install_self`]).
    pub fn new(interp: Rc<Interp>) -> Rc<Self> {
        let vm = Rc::new(Vm {
            interp,
            self_weak: RefCell::new(Weak::new()),
            class_methods: RefCell::new(HashMap::new()),
            class_defaults: RefCell::new(HashMap::new()),
        });
        *vm.self_weak.borrow_mut() = Rc::downgrade(&vm);
        // Register the VM on the shared interpreter so a native higher-order
        // stdlib function (e.g. `array.map`, `recover`) can re-enter the VM to
        // run a `Value::Closure` callback (the `native → VM` half of the bridge;
        // see `Interp::call_value`'s `Closure` arm and `Vm::call_value`).
        vm.interp.set_vm(Rc::downgrade(&vm));
        vm
    }

    /// Recover an owned `Rc<Vm>` from `&self`. Used by the async-fn eager-spawn in
    /// the `Op::Call` arm (V7) to hand an owned VM into the `'static` spawned task.
    pub fn rc(&self) -> Rc<Vm> {
        self.self_weak
            .borrow()
            .upgrade()
            .expect("Vm self-ref not installed")
    }

    /// The shared interpreter state.
    // used by V2+ (globals/native dispatch): no caller yet in V1.
    #[allow(dead_code)]
    pub fn interp(&self) -> &Rc<Interp> {
        &self.interp
    }

    /// Drive `fiber` until it returns (or panics). V1 runs the synchronous
    /// arithmetic subset only.
    ///
    /// The faulting `ip` is captured *before* advancing past the opcode and its
    /// operands so diagnostics point at the instruction that faulted. The current
    /// chunk is re-borrowed per access (`&fiber.frame().closure.proto.chunk`) and
    /// never held across a suspension point, keeping
    /// `clippy::await_holding_refcell_ref` clean once V7 introduces awaits.
    pub async fn run(&self, fiber: &mut Fiber) -> Result<RunOutcome, Control> {
        loop {
            // Capture the faulting ip (the opcode byte's offset) before advancing.
            let fault_ip = fiber.frame().ip;
            let byte = fiber.frame().closure.proto.chunk.code[fault_ip];
            let op = Op::from_u8(byte)
                .unwrap_or_else(|| panic!("invalid opcode byte {byte:#x} at ip {fault_ip}"));

            // Advance ip past the opcode byte and its inline operands.
            let operand_at = fault_ip + 1;
            fiber.frame_mut().ip = operand_at + op.operand_width();

            match op {
                Op::Const => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.frame().closure.proto.chunk.consts[idx].clone();
                    fiber.push(v);
                }
                Op::Nil => fiber.push(Value::Nil),
                Op::True => fiber.push(Value::Bool(true)),
                Op::False => fiber.push(Value::Bool(false)),
                Op::Pop => {
                    fiber.pop();
                }
                Op::Dup => {
                    let top = fiber.peek(0).clone();
                    fiber.push(top);
                }
                Op::Swap => {
                    // `a b -- b a`. Both operands are compiler-produced, so the
                    // stack always has the two values (a non-empty stack is a
                    // compiler invariant, not user-reachable).
                    let b = fiber.pop();
                    let a = fiber.pop();
                    fiber.push(b);
                    fiber.push(a);
                }
                Op::Rot3 => {
                    // `a b c -- b c a` (the value 3rd from the top rotates to the
                    // top). Compiler-produced three-value group; never user-reachable
                    // with fewer than three on the stack.
                    let c = fiber.pop();
                    let b = fiber.pop();
                    let a = fiber.pop();
                    fiber.push(b);
                    fiber.push(c);
                    fiber.push(a);
                }

                Op::Add
                | Op::Sub
                | Op::Mul
                | Op::Div
                | Op::Mod
                | Op::Pow
                | Op::Lt
                | Op::Le
                | Op::Gt
                | Op::Ge
                | Op::Eq
                | Op::Ne
                | Op::Range => {
                    // The two operands were pushed lhs-then-rhs, so pop rhs first.
                    // The op's span anchors any Tier-2 panic so the VM's
                    // diagnostics are byte-identical to the tree-walker.
                    let b = fiber.pop();
                    let a = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    // ONE shared dispatch with the tree-walker (`apply_binop`):
                    // string concat / decimal / range / cross-type equality /
                    // numeric, plus every exact panic message. And/Or/Coalesce are
                    // never lowered to these ops (they short-circuit via jumps), so
                    // `binop_of` never maps to one of them.
                    let v = crate::interp::apply_binop(binop_of(op), a, b, span)?;
                    fiber.push(v);
                }

                Op::Neg | Op::Not => {
                    let a = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let v = crate::interp::apply_unop(unop_of(op), a, span)?;
                    fiber.push(v);
                }

                Op::GetLocal => {
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.local(slot).clone();
                    fiber.push(v);
                }
                Op::SetLocal => {
                    // Clean stack discipline: SET_LOCAL POPS the value and stores
                    // it. Assignment-as-expression `DUP`s beforehand so a copy
                    // remains as the expression's result (see `compile_assign`).
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.pop();
                    fiber.set_local(slot, v);
                }

                Op::GetGlobal => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match &fiber.frame().closure.proto.chunk.consts[idx] {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("GET_GLOBAL operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    // The VM's globals are the bare builtins. The resolver classes
                    // every free identifier as `Global`, and the compiler only
                    // emits `GET_GLOBAL` for names in `BUILTIN_NAMES` (a bare-builtin
                    // call or a first-class builtin reference like `let p = print`);
                    // a non-builtin global is a compile-time deferral, so it never
                    // reaches here in a validly-compiled program. The guard below is
                    // defence-in-depth: should one ever arrive, we surface the
                    // tree-walker's exact runtime message (`undefined variable '<n>'`,
                    // see `Interp::eval_expr`'s `ExprKind::Ident` arm) so the two
                    // engines stay byte-identical.
                    if crate::interp::BUILTIN_NAMES.contains(&name.as_ref()) {
                        fiber.push(Value::Builtin(name));
                    } else {
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            format!("undefined variable '{name}'"),
                        ));
                    }
                }

                Op::Call | Op::CallSpread => {
                    // `Op::Call` carries a STATIC `u8` argc; `Op::CallSpread` carries
                    // none — its arguments arrived as a single runtime `Value::Array`
                    // (built by the array/spread builder ops) sitting on top of the
                    // callee `[..., callee, argsArray]`. For `CallSpread` we POP the
                    // args array and re-push its elements as individual stack slots,
                    // so the stack becomes `[..., callee, arg0, .., arg{n-1}]` — the
                    // EXACT shape `Op::Call` expects — and dispatch is shared below
                    // (arity/contracts then apply to the flattened list, byte-
                    // identical to the tree-walker's `eval_call_args` → call).
                    let argc = if matches!(op, Op::CallSpread) {
                        let args = match fiber.pop() {
                            Value::Array(a) => a,
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "CALL_SPREAD args are not an array: {}",
                                        crate::interp::type_name(&other)
                                    ),
                                ))
                            }
                        };
                        let items: Vec<Value> = args.borrow().iter().cloned().collect();
                        let n = items.len();
                        for v in items {
                            fiber.push(v);
                        }
                        n
                    } else {
                        fiber.frame().closure.proto.chunk.read_u8(operand_at) as usize
                    };
                    // The callee sits just below its `argc` arguments on the stack:
                    // `[..., callee, arg0, .., arg{argc-1}]`. Its stack index is the
                    // base where, for a Closure callee, the args become the callee
                    // frame's first local slots (the CALL convention).
                    let callee_idx = fiber.stack.len() - argc - 1;
                    match fiber.stack[callee_idx].clone() {
                        // A generator closure (`fn*` / `async fn*`) is NOT run and
                        // NOT spawned: calling it builds a NOT-STARTED Fiber for the
                        // closure (args bound into its slots, ip 0) and wraps it in a
                        // VM-backed `GeneratorHandle`, pushing a `Value::Generator`
                        // immediately. The body runs only when the consumer calls
                        // `gen.next()` (→ `GeneratorHandle::resume`), exactly like the
                        // tree-walker's `is_generator` branch of `call_function`.
                        // Both sync and async generators take this path (the async-
                        // generator yield+await fusion is V8-T5; for now we build the
                        // generator the same way). Arg binding reuses the SAME
                        // `check_call_args` the tree-walker / plain-call path uses, so
                        // arity/contract panics are byte-identical and surface eagerly
                        // at the call (the tree-walker also binds args eagerly when
                        // building the generator). AWAIT DISCIPLINE: no await here;
                        // the fiber is built synchronously and handed to the handle.
                        Value::Closure(callee) if callee.proto.is_generator => {
                            let call_span =
                                fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            let what = callee
                                .proto
                                .chunk
                                .name
                                .as_deref()
                                .unwrap_or("function");
                            // Pop the args, then drop the callee value beneath them.
                            let mut args = vec![Value::Nil; argc];
                            for slot in args.iter_mut().rev() {
                                *slot = fiber.pop();
                            }
                            fiber.pop(); // the callee value at callee_idx
                            // Bind args (arity + per-param contracts + rest) — shared
                            // with every other call path. A mismatch is a Tier-2
                            // panic at the call site, eager (like the tree-walker).
                            let bound = crate::interp::check_call_args(
                                &callee.proto.params,
                                args,
                                call_span,
                                what,
                            )?;
                            // Build a NOT-STARTED one-frame Fiber for the closure and
                            // place the bound params into its slots (cell slot → cell,
                            // plain slot → stack). `Fiber::new` reserved the locals
                            // and the cell vector. We do NOT run it.
                            let mut gfiber = Fiber::new(callee);
                            gfiber.frame_mut().ret_span = call_span;
                            let cells = gfiber.frame().cells.clone();
                            for (slot, v) in bound.into_iter().enumerate() {
                                if let Some(cell) = &cells[slot] {
                                    *cell.borrow_mut() = v;
                                } else {
                                    gfiber.stack[slot] = v;
                                }
                            }
                            let handle = crate::coro::GeneratorHandle::new_vm(
                                gfiber,
                                Rc::downgrade(&self.rc()),
                            );
                            fiber.push(Value::Generator(Rc::new(handle)));
                        }
                        // An `async fn` closure is NOT run inline: it is scheduled
                        // eagerly (M17 model 2a), exactly like the tree-walker's
                        // `is_async` branch of `call_function`. We build a body future
                        // that re-enters the VM via `Vm::call_value` (which sets up a
                        // fresh one-frame fiber, binds args via `check_call_args`, and
                        // runs to Done), `spawn_local` it onto the current-thread
                        // LocalSet, and hand back a `Value::Future` IMMEDIATELY; the
                        // caller `await`s it later. Because `call_value` runs the arity
                        // /contract check INSIDE the spawned task, an async arity or
                        // contract violation surfaces LAZILY — it resolves into the
                        // SharedFuture and re-emerges at the `await` site — byte-
                        // identical to the tree-walker. AWAIT DISCIPLINE: the closure
                        // and its args move into the `'static` spawned task; `vm` is an
                        // owned `Rc<Vm>`; no `fiber` RefCell borrow is held across the
                        // spawn/await below.
                        Value::Closure(callee) if callee.proto.is_async => {
                            let call_span =
                                fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            // Pop the `argc` args into an owned vec (top of stack is
                            // the LAST arg), then drop the callee value beneath them.
                            let mut args = vec![Value::Nil; argc];
                            for slot in args.iter_mut().rev() {
                                *slot = fiber.pop();
                            }
                            fiber.pop(); // the callee value at callee_idx
                            // Reuse the shared M17 dance (mirrors `call_function`'s
                            // async branch and `BoundMethod`'s): an owned `Rc<Vm>`
                            // (Vm self-weak, installed at `Vm::new`) drives the body;
                            // the task resolves the CELL (never a `SharedFuture` clone)
                            // so cancel-on-drop works; the inflight guard provides
                            // backpressure (reused from the shared interp).
                            let vm = self.rc();
                            let fut = crate::task::SharedFuture::new();
                            let cell = fut.cell();
                            let guard = self.interp.inflight_guard();
                            let handle = tokio::task::spawn_local(async move {
                                let _g = guard;
                                let r = vm
                                    .call_value(Value::Closure(callee), args, call_span)
                                    .await;
                                cell.resolve(r);
                            });
                            fut.set_abort(handle.abort_handle());
                            self.interp.maybe_yield_for_inflight().await;
                            fiber.push(Value::Future(fut));
                        }
                        Value::Closure(callee) => {
                            // The call-site span anchors arity/contract/return
                            // panics exactly where the tree-walker's do.
                            let call_span =
                                fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            // `what` mirrors the tree-walker's `func.name.as_deref()
                            // .unwrap_or("function")` so the wording matches.
                            let what = callee
                                .proto
                                .chunk
                                .name
                                .as_deref()
                                .unwrap_or("function");
                            // Pop the `argc` args into an owned vec (top of stack is
                            // the LAST arg), then drop the callee value beneath them.
                            let mut args = vec![Value::Nil; argc];
                            for slot in args.iter_mut().rev() {
                                *slot = fiber.pop();
                            }
                            fiber.pop(); // the callee value at callee_idx
                            // Arity + per-param contracts + rest collection, shared
                            // verbatim with the tree-walker via `check_call_args`. On
                            // a mismatch this returns a `Control::Panic` carrying the
                            // identical message anchored at `call_span`.
                            let bound = crate::interp::check_call_args(
                                &callee.proto.params,
                                args,
                                call_span,
                                what,
                            )?;
                            // The args/rest array are gone from the stack; the new
                            // frame's window starts where the callee value was.
                            let slot_base = callee_idx;
                            let slot_count = callee.proto.chunk.slot_count as usize;
                            // Allocate cells, then place each bound param into its
                            // slot (cell slot → cell; plain slot → stack). Reserve
                            // the remaining locals as Nil so the window is full.
                            let cells = super::fiber::alloc_cells(
                                slot_count,
                                &callee.proto.chunk.cell_slots,
                            );
                            fiber.stack.resize(slot_base + slot_count, Value::Nil);
                            for (slot, v) in bound.into_iter().enumerate() {
                                if let Some(cell) = &cells[slot] {
                                    *cell.borrow_mut() = v;
                                } else {
                                    fiber.stack[slot_base + slot] = v;
                                }
                            }
                            fiber.frames.push(super::fiber::CallFrame {
                                closure: callee,
                                ip: 0,
                                slot_base,
                                cells,
                                ret_span: call_span,
                                // A plain in-VM function/closure call is never a
                                // method frame; only `invoke_compiled_method` sets a
                                // `def_class` (so `super` is unavailable here, which
                                // is correct — `super` only appears in method bodies).
                                def_class: None,
                            });
                            // Continue the loop in the new frame (the run loop reads
                            // `fiber.frame()` at the top of each iteration). RETURN
                            // pops this frame and restores the caller.
                        }
                        other => {
                            // Native callee (Builtin/Function/Class/BoundMethod/...):
                            // delegate to the VM-aware `call_value`, which routes a
                            // VM class constructor / VM bound method to COMPILED code
                            // (V9) and everything else to the shared `Interp`
                            // dispatch. Pop the args and the callee into owned locals
                            // BEFORE the await so no borrow of `fiber` is held across
                            // the suspension point (`await_holding_refcell_ref` stays
                            // clean).
                            let mut args = vec![Value::Nil; argc];
                            for slot in args.iter_mut().rev() {
                                *slot = fiber.pop();
                            }
                            let _callee = fiber.pop(); // the Value at callee_idx
                            let span =
                                fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            let result = self.call_value(other, args, span).await?;
                            fiber.push(result);
                        }
                    }
                }

                Op::CallMethod => {
                    // A method call `recv.<name>(args)`. Mirrors the tree-walker's
                    // `eval_chain` Call arm for a `Member` callee: the schema
                    // fluent-method hook, else `read_member(recv, name)` →
                    // `call_value`. The receiver sits below its args on the stack.
                    //
                    // ORDERING NOTE: the tree-walker reads the member BEFORE
                    // evaluating the call args (so a member-read error preempts arg
                    // side effects). Here the compiler already evaluated the args
                    // (they are on the stack), so a member-read error does NOT
                    // preempt arg side effects. This sub-case (a side-effecting arg
                    // AND an erroring member read) is the documented deviation
                    // deferred to the full V9 method-call slice; the generator
                    // consumer API (`gen.next(v)`/`gen.close()`) and the rest of the
                    // gated corpus do not hit it. Everything else is byte-identical.
                    let name = match &fiber.frame().closure.proto.chunk.consts
                        [fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize]
                    {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("CALL_METHOD name is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let argc = fiber.frame().closure.proto.chunk.read_u8(operand_at + 2) as usize;
                    // Pop the args (top is the LAST arg), then the receiver beneath.
                    let mut args = vec![Value::Nil; argc];
                    for slot in args.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    let recv = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    // Schema fluent-method hook (same predicate the tree-walker uses).
                    if crate::stdlib::schema::is_schema_value(&recv)
                        && crate::stdlib::schema::is_schema_method(&name)
                    {
                        let mut sargs = Vec::with_capacity(args.len() + 1);
                        sargs.push(recv);
                        sargs.extend(args);
                        let v = self.interp.call_schema(&name, &sargs, span).await?;
                        fiber.push(v);
                    } else {
                        // Fallback: read the member, then call it. `vm_read_member`
                        // yields a VM `BoundMethod` for an Instance method on a VM
                        // class (dispatched to COMPILED code by `call_value`), else
                        // the SAME dispatch the tree-walker runs (a BoundMethod /
                        // GeneratorMethod / NativeMethod / Builtin / … bound to
                        // `recv`); `call_value` invokes it (VM-aware for V9 classes,
                        // shared with the tree-walker otherwise).
                        let callee_v = self.vm_read_member(&recv, &name, span)?;
                        let v = self.call_value(callee_v, args, span).await?;
                        fiber.push(v);
                    }
                }

                Op::Template => {
                    // Pop `n` parts (pushed left-to-right) and concatenate their
                    // string coercions in source order. The coercion is exactly
                    // the tree-walker's `Value::to_string()` (the `Display` impl
                    // shared with `print`), so a template interpolating any value
                    // renders byte-identically to `ExprKind::Template`.
                    let n = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let mut parts = vec![Value::Nil; n];
                    for slot in parts.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    let mut out = String::new();
                    for v in &parts {
                        out.push_str(&v.to_string());
                    }
                    fiber.push(Value::Str(out.into()));
                }

                Op::Jump => {
                    // Unconditional relative jump. The displacement is measured
                    // from the byte AFTER the operand to the target (see
                    // `Chunk::patch_jump`/`emit_loop`). At this point we have
                    // already advanced `ip` past the opcode and its 2-byte
                    // operand, so `fiber.frame().ip == operand_at + 2` is exactly
                    // that base; add the signed displacement to land on target.
                    let disp = fiber.frame().closure.proto.chunk.read_i16(operand_at);
                    let base = fiber.frame().ip as isize;
                    fiber.frame_mut().ip = (base + disp as isize) as usize;
                }
                Op::Loop => {
                    // Unconditional backward (relative) jump used for loop
                    // back-edges. Identical mechanics to `Op::Jump` — the
                    // displacement (negative for a real backward jump) is measured
                    // from the byte AFTER the operand to the target (see
                    // `Chunk::emit_loop`).
                    let disp = fiber.frame().closure.proto.chunk.read_i16(operand_at);
                    let base = fiber.frame().ip as isize;
                    fiber.frame_mut().ip = (base + disp as isize) as usize;
                }
                Op::JumpIfFalse => {
                    // Pop the tested value; jump iff it is falsy. Short-circuit
                    // lowering `DUP`s the operand beforehand so the un-tested copy
                    // survives as the expression's result when we jump.
                    let v = fiber.pop();
                    if !v.is_truthy() {
                        let disp = fiber.frame().closure.proto.chunk.read_i16(operand_at);
                        let base = fiber.frame().ip as isize;
                        fiber.frame_mut().ip = (base + disp as isize) as usize;
                    }
                }
                Op::JumpIfTrue => {
                    // Pop the tested value; jump iff it is truthy.
                    let v = fiber.pop();
                    if v.is_truthy() {
                        let disp = fiber.frame().closure.proto.chunk.read_i16(operand_at);
                        let base = fiber.frame().ip as isize;
                        fiber.frame_mut().ip = (base + disp as isize) as usize;
                    }
                }
                Op::JumpIfNotNil => {
                    // Pop the tested value; jump iff it is NOT `nil`. Mirrors the
                    // tree-walker's `??` test (`l == Value::Nil` selects the RHS;
                    // anything else keeps the left), so the jump fires on "keep
                    // the non-nil left operand".
                    let v = fiber.pop();
                    if v != Value::Nil {
                        let disp = fiber.frame().closure.proto.chunk.read_i16(operand_at);
                        let base = fiber.frame().ip as isize;
                        fiber.frame_mut().ip = (base + disp as isize) as usize;
                    }
                }

                Op::NewArray => {
                    // Pop `n` elements (pushed in source order, so the last
                    // pushed is on top) into a Vec preserving source order, then
                    // push `Value::Array`. Matches the tree-walker's
                    // `ExprKind::Array` construction (`Rc<RefCell<Vec>>`).
                    let n = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let mut values = vec![Value::Nil; n];
                    for slot in values.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    fiber.push(Value::Array(Rc::new(RefCell::new(values))));
                }

                Op::NewObject => {
                    // Pop `n` (key, value) pairs. Each pair was pushed key-first
                    // then value, and the pairs were pushed in source order, so
                    // the stack top-down is: vN, kN, …, v1, k1. Pop into a
                    // source-order list, then insert into an `IndexMap` in source
                    // order — a later duplicate key overwrites the value but keeps
                    // the first-seen position (IndexMap semantics), byte-identical
                    // to the tree-walker's `ExprKind::Object`.
                    let n = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let mut pairs: Vec<(Rc<str>, Value)> = vec![(Rc::from(""), Value::Nil); n];
                    for slot in pairs.iter_mut().rev() {
                        let value = fiber.pop();
                        let key = match fiber.pop() {
                            Value::Str(s) => s,
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("NEW_OBJECT key is not a string constant: {other:?}"),
                                ))
                            }
                        };
                        *slot = (key, value);
                    }
                    let mut map = indexmap::IndexMap::with_capacity(n);
                    for (k, v) in pairs {
                        map.insert(k.to_string(), v);
                    }
                    fiber.push(Value::Object(Rc::new(RefCell::new(map))));
                }

                Op::Spread | Op::SpreadArgs => {
                    // `[arr, operand] -- [arr]` — flatten the spread `operand` (an
                    // Array) into the under-construction array `arr` below it.
                    // Mirrors the tree-walker's `ExprKind::Array` / `eval_call_args`
                    // spread arm: a non-array is the SAME Tier-2 panic, anchored at
                    // this op's span (the operand's trivia-trimmed code span). The
                    // ONLY difference between SPREAD and SPREAD_ARGS is the message
                    // ("into an array" vs "as call arguments").
                    let operand = fiber.pop();
                    match operand {
                        Value::Array(src) => {
                            // Clone elements out FIRST so a self-spread (`[...a]`
                            // where `arr` aliased `a`) cannot observe a borrow
                            // conflict, then extend the builder array.
                            let items: Vec<Value> = src.borrow().iter().cloned().collect();
                            match fiber.peek(0) {
                                Value::Array(arr) => arr.borrow_mut().extend(items),
                                other => {
                                    return Err(self.panic_at(
                                        fiber,
                                        fault_ip,
                                        format!(
                                            "SPREAD target is not an array: {}",
                                            crate::interp::type_name(other)
                                        ),
                                    ))
                                }
                            }
                        }
                        other => {
                            let msg = if matches!(op, Op::SpreadArgs) {
                                format!(
                                    "can only spread an array as call arguments, got {}",
                                    crate::interp::type_name(&other)
                                )
                            } else {
                                format!(
                                    "can only spread an array into an array, got {}",
                                    crate::interp::type_name(&other)
                                )
                            };
                            return Err(self.panic_at(fiber, fault_ip, msg));
                        }
                    }
                }

                Op::AppendArray => {
                    // `[arr, item] -- [arr]` — push one `item` onto the builder
                    // array `arr` below it.
                    let item = fiber.pop();
                    match fiber.peek(0) {
                        Value::Array(arr) => arr.borrow_mut().push(item),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "APPEND_ARRAY target is not an array: {}",
                                    crate::interp::type_name(other)
                                ),
                            ))
                        }
                    }
                }

                Op::AppendObject => {
                    // `[obj, key, val] -- [obj]` — insert `key -> val` into the
                    // builder object `obj`. Later-wins + first-position (IndexMap
                    // insert), byte-identical to the tree-walker's `ExprKind::Object`.
                    let val = fiber.pop();
                    let key = match fiber.pop() {
                        Value::Str(s) => s,
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("APPEND_OBJECT key is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    match fiber.peek(0) {
                        Value::Object(obj) => {
                            obj.borrow_mut().insert(key.to_string(), val);
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "APPEND_OBJECT target is not an object: {}",
                                    crate::interp::type_name(other)
                                ),
                            ))
                        }
                    }
                }

                Op::SpreadObject => {
                    // `[obj, operand] -- [obj]` — merge the operand object's entries
                    // into the builder object `obj`. Mirrors the tree-walker's
                    // `ExprKind::Object` spread arm: a non-object is the SAME Tier-2
                    // panic at this op's span; entries insert later-wins/first-pos.
                    let operand = fiber.pop();
                    match operand {
                        Value::Object(src) => {
                            // Snapshot the source entries FIRST (avoids a borrow
                            // conflict if `obj` aliases `src` via a self-spread).
                            let entries: Vec<(String, Value)> = src
                                .borrow()
                                .iter()
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect();
                            match fiber.peek(0) {
                                Value::Object(obj) => {
                                    let mut m = obj.borrow_mut();
                                    for (k, v) in entries {
                                        m.insert(k, v);
                                    }
                                }
                                other => {
                                    return Err(self.panic_at(
                                        fiber,
                                        fault_ip,
                                        format!(
                                            "SPREAD_OBJECT target is not an object: {}",
                                            crate::interp::type_name(other)
                                        ),
                                    ))
                                }
                            }
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "can only spread an object into an object, got {}",
                                    crate::interp::type_name(&other)
                                ),
                            ))
                        }
                    }
                }

                Op::GetIndex => {
                    // `obj idx -- obj[idx]`. The two operands were pushed
                    // obj-then-idx, so pop idx first. The shared `index_get`
                    // dispatch (with the tree-walker) anchors every panic at the
                    // op's span; the VM has a single instruction span, so it is
                    // passed for both the receiver-span and index-span parameters.
                    let idx = fiber.pop();
                    let obj = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let v = crate::interp::index_get(&obj, &idx, span, span)?;
                    fiber.push(v);
                }

                Op::SetIndex => {
                    // `obj idx val -- val` — store `obj[idx] = val`. The operands
                    // were pushed obj-then-idx-then-val, so pop val, idx, obj. The
                    // shared `index_set` dispatch (with the tree-walker) anchors
                    // every panic at the op's span; the VM has a single instruction
                    // span, so it is passed for both the receiver-span and
                    // index-span parameters. Leaves the assigned value on the stack
                    // (assignment is an expression).
                    let val = fiber.pop();
                    let idx = fiber.pop();
                    let obj = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let v = crate::interp::index_set(&obj, &idx, val, span, span)?;
                    fiber.push(v);
                }

                Op::GetProp | Op::GetPropOpt => {
                    // `obj -- obj.<name>` (the optional form short-circuits to
                    // `nil` when the receiver is `nil`). `read_member` is the SAME
                    // member-access dispatch the tree-walker runs (fields, methods
                    // → BoundMethod, enum variants, native handles, nil-receiver
                    // errors), so the two engines cannot drift.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match &fiber.frame().closure.proto.chunk.consts[idx] {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("GET_PROP operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let obj = fiber.pop();
                    if op == Op::GetPropOpt && obj == Value::Nil {
                        fiber.push(Value::Nil);
                    } else {
                        let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                        // VM-aware member read: an Instance method on a VM class
                        // becomes a `BoundMethod` (compiled-closure-backed); fields
                        // and every non-VM receiver go through the shared
                        // `Interp::read_member`.
                        let v = self.vm_read_member(&obj, &name, span)?;
                        fiber.push(v);
                    }
                }

                Op::CheckNumbers => {
                    // Peek-only bounds guard for for-range: the top two stack
                    // values (start below, end on top) must both be numbers.
                    // Leaves them in place so the surrounding lowering can store
                    // them into slots. The op's span is the START bound's span, so
                    // the panic is byte-identical to the tree-walker's
                    // `Stmt::ForRange` ("for-range bounds must be numbers" at
                    // `start.span`).
                    let end_ok = matches!(fiber.peek(0), Value::Number(_));
                    let start_ok = matches!(fiber.peek(1), Value::Number(_));
                    if !(end_ok && start_ok) {
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            "for-range bounds must be numbers".to_string(),
                        ));
                    }
                }

                Op::IterSnapshot => {
                    // Materialize the SYNC for-of snapshot from the iterable on
                    // TOS. Byte-identical to the tree-walker's `Stmt::ForOf` (sync,
                    // `for_await == false`) `items` build: an `Array` snapshots a
                    // CLONE of its current elements (so the iteration is fixed even
                    // if the body mutates the source array), a `Str` snapshots its
                    // chars each as a 1-char string, and ANYTHING ELSE — including
                    // object/map/set, which are NOT iterable in sync for-of —
                    // raises the Tier-2 panic at this op's span (the iterable
                    // expression's trivia-trimmed code span), exactly like
                    // `AsError::at(format!("value of type {} is not iterable", ...))`.
                    let iterable = fiber.pop();
                    let items: Vec<Value> = match iterable {
                        Value::Array(arr) => arr.borrow().clone(),
                        Value::Str(s) => {
                            s.chars().map(|c| Value::Str(c.to_string().into())).collect()
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "value of type {} is not iterable",
                                    crate::interp::type_name(&other)
                                ),
                            ))
                        }
                    };
                    fiber.push(Value::Array(Rc::new(RefCell::new(items))));
                }

                Op::ArrayLen => {
                    // Pop a (compiler-produced) snapshot array and push its element
                    // count as a `Number`. The operand is never user input — the
                    // compiler emits this only over an `IterSnapshot` result — so a
                    // non-array is a compiler bug surfaced as a Tier-2 panic.
                    let v = fiber.pop();
                    match v {
                        Value::Array(arr) => {
                            let len = arr.borrow().len();
                            fiber.push(Value::Number(len as f64));
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("ARRAY_LEN operand is not an array: {other:?}"),
                            ))
                        }
                    }
                }

                Op::Closure => {
                    // Build a closure over a nested proto, capturing its upvalues
                    // BY REFERENCE per the proto's capture plan
                    // (`proto.chunk.upvalues`, indexed by upvalue number):
                    //   - ParentLocal(slot): clone the CURRENT frame's cell for that
                    //     slot. The resolver guarantees a captured local is a cell
                    //     slot, so `cells[slot]` is `Some`; a `None` is a
                    //     compiler/resolver bug (clear panic).
                    //   - ParentUpvalue(idx): clone the CURRENT closure's upvalue
                    //     cell (a transitive capture from an outer frame).
                    // Capturing the cell `Rc` (not its value) is what makes capture
                    // by-reference: the closure sees later mutation of the cell.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let proto = fiber.frame().closure.proto.chunk.protos[idx].clone();
                    let mut upvalues = Vec::with_capacity(proto.chunk.upvalues.len());
                    for desc in &proto.chunk.upvalues {
                        let cell = match *desc {
                            crate::syntax::resolve::types::UpvalueDescriptor::ParentLocal(slot) => {
                                fiber
                                    .frame()
                                    .cells
                                    .get(slot as usize)
                                    .and_then(|c| c.as_ref())
                                    .unwrap_or_else(|| {
                                        panic!(
                                            "CLOSURE captures parent local slot {slot} that is not a cell (compiler/resolver bug)"
                                        )
                                    })
                                    .clone()
                            }
                            crate::syntax::resolve::types::UpvalueDescriptor::ParentUpvalue(up) => {
                                fiber.frame().closure.upvalues[up as usize].clone()
                            }
                        };
                        upvalues.push(cell);
                    }
                    let closure =
                        crate::vm::value_ext::Closure::with_upvalues(proto, upvalues);
                    fiber.push(Value::Closure(closure));
                }

                Op::GetLocalCell => {
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.get_local_cell(slot);
                    fiber.push(v);
                }
                Op::SetLocalCell => {
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.pop();
                    fiber.set_local_cell(slot, v);
                }
                Op::FreshCell => {
                    // Install a brand-new heap cell into this slot, dropping the
                    // frame's ref to the previous cell (any closure that captured
                    // it keeps its own `Rc`, so it retains that iteration's value).
                    // Emitted at the top of each loop iteration for per-iteration
                    // capture freshness.
                    let slot = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    fiber.fresh_cell(slot);
                }

                Op::CheckArrayDestructure => {
                    // Peek the RHS on TOS and validate it is an Array, exactly like
                    // the tree-walker's `Stmt::LetDestructure` type check (which runs
                    // ONCE before binding any name). Leaves the source in place so the
                    // surrounding lowering can stash it in a temp slot.
                    if !matches!(fiber.peek(0), Value::Array(_)) {
                        let t = crate::interp::type_name(fiber.peek(0));
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            format!("cannot destructure a non-array value of type {t}"),
                        ));
                    }
                }

                Op::CheckObjectDestructure => {
                    // Peek the RHS on TOS and validate it is an Object or Instance,
                    // exactly like the tree-walker's `Stmt::LetDestructureObject` type
                    // check. Leaves the source in place.
                    if !matches!(fiber.peek(0), Value::Object(_) | Value::Instance(_)) {
                        let t = crate::interp::type_name(fiber.peek(0));
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            format!("cannot destructure a non-object value of type {t}"),
                        ));
                    }
                }

                Op::ArrayElem => {
                    // `src -- src[index]`. Pop the (already-validated) array and push
                    // the element at `index`, or `nil` for an out-of-bounds position
                    // (positions past the length bind nil — `items.get(i).cloned()
                    // .unwrap_or(Value::Nil)`).
                    let index = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let src = fiber.pop();
                    match src {
                        Value::Array(arr) => {
                            let v = arr.borrow().get(index).cloned().unwrap_or(Value::Nil);
                            fiber.push(v);
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("ARRAY_ELEM operand is not an array: {other:?}"),
                            ))
                        }
                    }
                }

                Op::ObjectKey => {
                    // `src -- src[key]` where `key = consts[idx]`. Pop the
                    // (already-validated) Object/Instance and push the value under
                    // `key`, or `nil` if absent. Mirrors the tree-walker's destructure
                    // `get` closure EXACTLY: an Instance reads only its `fields` (it
                    // does NOT fall back to methods like `read_member` would).
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let key = match &fiber.frame().closure.proto.chunk.consts[idx] {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("OBJECT_KEY operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let src = fiber.pop();
                    let v = match src {
                        Value::Object(o) => {
                            o.borrow().get(key.as_ref()).cloned().unwrap_or(Value::Nil)
                        }
                        Value::Instance(i) => {
                            i.borrow().fields.get(key.as_ref()).cloned().unwrap_or(Value::Nil)
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("OBJECT_KEY operand is not an object: {other:?}"),
                            ))
                        }
                    };
                    fiber.push(v);
                }

                Op::ArrayRest => {
                    // `src -- src[start..]`. Pop the (already-validated) array and push
                    // a NEW array of its elements from `start` to the end — the `...rest`
                    // collector (`items.iter().skip(names.len())`).
                    let start = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let src = fiber.pop();
                    match src {
                        Value::Array(arr) => {
                            let tail: Vec<Value> =
                                arr.borrow().iter().skip(start).cloned().collect();
                            fiber.push(Value::Array(Rc::new(RefCell::new(tail))));
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("ARRAY_REST operand is not an array: {other:?}"),
                            ))
                        }
                    }
                }

                Op::ObjectRest => {
                    // `src -- leftover` where `consts[idx]` is an Array of the bound
                    // key strings. Pop the (already-validated) Object/Instance and push
                    // a NEW object of its entries whose key is NOT bound, in source
                    // order — the object-rest collector (excludes already-bound SOURCE
                    // keys).
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let bound: std::collections::HashSet<Rc<str>> =
                        match &fiber.frame().closure.proto.chunk.consts[idx] {
                            Value::Array(keys) => keys
                                .borrow()
                                .iter()
                                .filter_map(|v| match v {
                                    Value::Str(s) => Some(s.clone()),
                                    _ => None,
                                })
                                .collect(),
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "OBJECT_REST operand is not a key array: {other:?}"
                                    ),
                                ))
                            }
                        };
                    let src = fiber.pop();
                    let mut remaining: indexmap::IndexMap<String, Value> =
                        indexmap::IndexMap::new();
                    match src {
                        Value::Object(o) => {
                            for (k, v) in o.borrow().iter() {
                                if !bound.contains(k.as_str()) {
                                    remaining.insert(k.clone(), v.clone());
                                }
                            }
                        }
                        Value::Instance(i) => {
                            for (k, v) in i.borrow().fields.iter() {
                                if !bound.contains(k.as_str()) {
                                    remaining.insert(k.clone(), v.clone());
                                }
                            }
                        }
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("OBJECT_REST operand is not an object: {other:?}"),
                            ))
                        }
                    }
                    fiber.push(Value::Object(Rc::new(RefCell::new(remaining))));
                }

                Op::GetUpvalue => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.frame().closure.upvalues[idx].borrow().clone();
                    fiber.push(v);
                }
                Op::SetUpvalue => {
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let v = fiber.pop();
                    *fiber.frame().closure.upvalues[idx].borrow_mut() = v;
                }

                Op::Return => {
                    // Pop the result and unwind one frame, returning that value to
                    // the caller (or ending the program if this was the root frame).
                    // The shared `return_from_frame` helper applies the return-type
                    // contract, drops the frame (releasing its cell `Rc`s — captured
                    // cells stay alive via the closures' own refs), truncates the
                    // stack to `slot_base`, and pushes the result into the caller.
                    // `PROPAGATE` reuses this SAME unwind on a propagated error.
                    let result = fiber.pop();
                    if let Some(outcome) = self.return_from_frame(fiber, result)? {
                        return Ok(outcome);
                    }
                }

                Op::Propagate => {
                    // The `?` operator. Mirrors the tree-walker's `ExprKind::Try`
                    // exactly: the operand must be a 2-element `[value, err]` Result
                    // pair (else a Tier-2 panic with the identical message, anchored
                    // at this op's span = the `TryExpr`'s code span). If `err == nil`
                    // the `value` is left on the stack (the `?` expression's result);
                    // otherwise it does a FUNCTION-LEVEL early return of `[nil, err]`
                    // — the SAME unwind-one-frame logic as `Op::Return` — so the
                    // enclosing function returns the propagated pair (and at the top
                    // level the program ends with that pair, treated as `Ok` by the
                    // driver, just like `Control::Propagate` in `run_file`).
                    let v = fiber.pop();
                    let (value, err) = match &v {
                        Value::Array(a) if a.borrow().len() == 2 => {
                            let b = a.borrow();
                            (b[0].clone(), b[1].clone())
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "the ? operator requires a Result pair [value, err]".to_string(),
                            ))
                        }
                    };
                    if err == Value::Nil {
                        fiber.push(value);
                    } else {
                        let pair = crate::interp::make_pair(Value::Nil, err);
                        if let Some(outcome) = self.return_from_frame(fiber, pair)? {
                            return Ok(outcome);
                        }
                    }
                }

                Op::Unwrap => {
                    // The `!` force-unwrap operator. Mirrors the tree-walker's
                    // `ExprKind::Unwrap` exactly: the operand must be a 2-element
                    // `[value, err]` Result pair (else a Tier-2 panic with the
                    // identical message, anchored at this op's span = the
                    // `UnwrapExpr`'s code span). If `err == nil` the `value` is
                    // left on the stack (the `!` expression's result); otherwise
                    // it raises a RECOVERABLE `Control::Panic` carrying the
                    // original error's message (`error_message`), so `recover`
                    // round-trips it into `[nil, err]` IDENTICALLY to the
                    // tree-walker's `AsError::at(error_message(&err), span)`.
                    let v = fiber.pop();
                    let (value, err) = match &v {
                        Value::Array(a) if a.borrow().len() == 2 => {
                            let b = a.borrow();
                            (b[0].clone(), b[1].clone())
                        }
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "the ! operator requires a Result pair [value, err]".to_string(),
                            ))
                        }
                    };
                    if err == Value::Nil {
                        fiber.push(value);
                    } else {
                        return Err(self.panic_at(fiber, fault_ip, error_message(&err)));
                    }
                }

                Op::Await => {
                    // `await expr`. Mirrors the tree-walker's `ExprKind::Await`
                    // EXACTLY: if the operand is a `Value::Future`, drive it to
                    // completion (`f.get().await`) — a panic/propagation raised in
                    // the spawned task re-surfaces HERE (cross-task propagation),
                    // byte-identical to the tree-walker; otherwise `await` on a
                    // non-future is identity (`await 5 == 5`). Pop the operand into
                    // an owned local BEFORE the await so no `fiber` RefCell borrow is
                    // held across the suspension point (`await_holding_refcell_ref`
                    // stays clean).
                    let v = fiber.pop();
                    match v {
                        Value::Future(f) => {
                            let r = f.get().await?;
                            fiber.push(r);
                        }
                        other => fiber.push(other),
                    }
                }

                Op::Yield => {
                    // `yield expr`. The Fiber model makes this trivial: the yielded
                    // value is on TOS; pop it and return `RunOutcome::Yielded(v)`
                    // WITHOUT unwinding any frames — the frame stack stays live in
                    // the Fiber and `ip` is already past this op, so the next
                    // `resume` continues exactly here. The consumer's `next(v)`
                    // (driven via `GeneratorHandle::resume_vm`) pushes its `v` back
                    // onto the Fiber's stack, where the bytecode after `Op::Yield`
                    // expects the yield expression's value — that is the value-
                    // injection mechanism. `yield` with no operand pushed a `Nil`
                    // (the compiler emits NIL), so the popped value is `nil`.
                    let v = fiber.pop();
                    fiber.state = crate::vm::FiberState::Suspended;
                    return Ok(RunOutcome::Yielded(v));
                }

                Op::GetIter => {
                    // `for await` async-iterable validation: TOS must be a
                    // `Value::Generator` (driven by `resume`) or a native stream
                    // handle (WebSocket `recv` / SSE `next`). ANYTHING ELSE is the
                    // Tier-2 panic `value of type {t} is not async-iterable`,
                    // byte-identical to the tree-walker's `exec_for_await` (the
                    // `other =>` and the Native-with-no-stream-method arms both
                    // produce this message). We PEEK (leave the value in place): the
                    // compiler immediately stores it into a scratch slot to drive
                    // lazily across iterations.
                    let ok = match fiber.peek(0) {
                        Value::Generator(_) => true,
                        Value::Native(n) => {
                            crate::interp::native_stream_method(n.kind).is_some()
                        }
                        _ => false,
                    };
                    if !ok {
                        let t = crate::interp::type_name(fiber.peek(0));
                        return Err(self.panic_at(
                            fiber,
                            fault_ip,
                            format!("value of type {t} is not async-iterable"),
                        ));
                    }
                }

                Op::IterNext => {
                    // Drive one lazy `for await` step over the async-iterable on TOS.
                    // Pop it into an owned local BEFORE any `.await` so no `fiber`
                    // RefCell borrow is held across the suspension point
                    // (`await_holding_refcell_ref` stays clean), then push back the
                    // produced `value` and a `done` boolean. Byte-identical to
                    // `exec_for_await` (`src/interp.rs`).
                    // The op's span (the iterable expression's code span), captured
                    // before any borrow/await so a native-stream call has a site.
                    let op_span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let iterable = fiber.pop();
                    match iterable {
                        Value::Generator(g) => {
                            // `resume(nil)` drives the backing Fiber to its next
                            // `Op::Yield` (awaiting any inner futures along the way —
                            // this is how an async generator's await+yield fuse).
                            // `Some(v)` -> a value; `None` -> done.
                            match g.resume(Value::Nil).await? {
                                Some(v) => {
                                    fiber.push(v);
                                    fiber.push(Value::Bool(false));
                                }
                                None => {
                                    fiber.push(Value::Nil);
                                    fiber.push(Value::Bool(true));
                                }
                            }
                        }
                        Value::Native(n) => {
                            // A native stream: call its `recv`/`next` method for a
                            // `[value, err]` pair (a non-nil `err` is a Tier-2 panic,
                            // a nil `value` ends the stream), mirroring
                            // `exec_for_await`'s `Value::Native` arm exactly.
                            // `GetIter` already validated the handle, so a missing
                            // stream method here is a wiring bug — surface it as a
                            // defensive Tier-2 panic rather than an `unwrap`.
                            let method = match crate::interp::native_stream_method(n.kind) {
                                Some(m) => m,
                                None => {
                                    return Err(self.panic_at(
                                        fiber,
                                        fault_ip,
                                        format!(
                                            "value of type {} is not async-iterable",
                                            crate::interp::type_name(&Value::Native(n))
                                        ),
                                    ))
                                }
                            };
                            let bound =
                                Value::NativeMethod(Rc::new(crate::value::NativeMethod {
                                    receiver: n,
                                    method: method.to_string(),
                                }));
                            // Box this edge: `call_value` may re-enter `run`, so
                            // the recursive future needs a finite size.
                            let pair =
                                Box::pin(self.call_value(bound, Vec::new(), op_span)).await?;
                            let (value, err) = match &pair {
                                Value::Array(a) if a.borrow().len() == 2 => {
                                    let b = a.borrow();
                                    (b[0].clone(), b[1].clone())
                                }
                                // Defensive: a non-pair return ends iteration.
                                _ => {
                                    fiber.push(Value::Nil);
                                    fiber.push(Value::Bool(true));
                                    continue;
                                }
                            };
                            if err != Value::Nil {
                                let msg = crate::interp::error_message(&err);
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("for await stream error: {msg}"),
                                ));
                            }
                            if value == Value::Nil {
                                fiber.push(Value::Nil);
                                fiber.push(Value::Bool(true));
                            } else {
                                fiber.push(value);
                                fiber.push(Value::Bool(false));
                            }
                        }
                        other => {
                            // GetIter validated the iterable, so this is unreachable
                            // in practice; surface defensively rather than panic the
                            // host.
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!(
                                    "value of type {} is not async-iterable",
                                    crate::interp::type_name(&other)
                                ),
                            ));
                        }
                    }
                }

                Op::IterClose => {
                    // Close the async-iterable on TOS on a `break`/early-`return` out
                    // of a `for await` over a generator — `g.close()` drops the
                    // backing Fiber so it is reclaimed promptly, byte-identical to
                    // the tree-walker. A native stream is reclaimed at scope end, so
                    // closing it is a no-op here.
                    let iterable = fiber.pop();
                    if let Value::Generator(g) = iterable {
                        g.close();
                    }
                }

                Op::SetProp => {
                    // `obj value -- value` — store `obj.<name> = value`, applying a
                    // declared field-type contract on an Instance field. The SAME
                    // `set_member` the tree-walker's `assign_to` Member arm uses, so
                    // the field contract panic (message + span) is byte-identical.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match &fiber.frame().closure.proto.chunk.consts[idx] {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("SET_PROP operand is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let value = fiber.pop();
                    let obj = fiber.pop();
                    // The op's span is the VALUE's span (see the compiler), matching
                    // the tree-walker's `value_span` for the contract panic; reuse it
                    // for the "cannot set property" error too (single VM span).
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    let v = self.interp.set_member(&obj, &name, value, span, span)?;
                    fiber.push(v);
                }

                Op::Class => {
                    // Build a class value (V9). The compiler emitted, just below this
                    // op, one closure per defaulted field (declaration order) then
                    // one closure per method (declaration order); the class proto
                    // carries the prebuilt `Rc<Class>` and the parallel name lists.
                    // Register the default thunks and method closures in the VM side
                    // tables keyed by the class's `Rc` identity, then push the class.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let cp = fiber.frame().closure.proto.chunk.class_protos[idx].clone();
                    let n_methods = cp.method_names.len();
                    let n_defaults = cp.default_fields.len();
                    // Pop method closures (top = LAST method), then default thunks.
                    let mut methods = vec![Value::Nil; n_methods];
                    for slot in methods.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    let mut defaults = vec![Value::Nil; n_defaults];
                    for slot in defaults.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    // For an `extends` clause, the superclass class-value was pushed
                    // FIRST (it is the bottom of the group), so it pops LAST. Build a
                    // FRESH `Rc<Class>` with `superclass` set (the prebuilt template
                    // had `superclass: None`); the method/default tables are then
                    // registered under the NEW class's identity key. Mirrors the
                    // tree-walker's `Stmt::Class`, which sets `superclass` to the
                    // resolved parent `Value::Class`.
                    let class: Rc<crate::value::Class> = if cp.has_super {
                        let sup = fiber.pop();
                        let superclass = match sup {
                            Value::Class(c) => c,
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("'{other}' is not a class"),
                                ))
                            }
                        };
                        Rc::new(crate::value::Class {
                            name: cp.class.name.clone(),
                            superclass: Some(superclass),
                            fields: cp.class.fields.clone(),
                            methods: cp.class.methods.clone(),
                            def_env: cp.class.def_env.clone(),
                        })
                    } else {
                        cp.class.clone()
                    };
                    let key = Rc::as_ptr(&class) as usize;
                    let mut method_map: HashMap<String, Rc<Closure>> = HashMap::new();
                    for (name, mv) in cp.method_names.iter().zip(methods) {
                        match mv {
                            Value::Closure(c) => {
                                method_map.insert(name.clone(), c);
                            }
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!("class method '{name}' is not a closure: {other:?}"),
                                ))
                            }
                        }
                    }
                    let mut default_map: HashMap<String, Rc<Closure>> = HashMap::new();
                    for (name, dv) in cp.default_fields.iter().zip(defaults) {
                        match dv {
                            Value::Closure(c) => {
                                default_map.insert(name.clone(), c);
                            }
                            other => {
                                return Err(self.panic_at(
                                    fiber,
                                    fault_ip,
                                    format!(
                                        "field default '{name}' thunk is not a closure: {other:?}"
                                    ),
                                ))
                            }
                        }
                    }
                    self.class_methods.borrow_mut().insert(key, method_map);
                    self.class_defaults.borrow_mut().insert(key, default_map);
                    fiber.push(Value::Class(class));
                }

                Op::GetSuper => {
                    // `super.<name>` (V9-T2): resolve `name` starting at the CURRENT
                    // method's DEFINING class's superclass, bound to `self` (slot 0).
                    // Mirrors the tree-walker: `super` is a `Value::Super` whose
                    // `start` is `defining_class.superclass`, and `read_member` on it
                    // finds the method up that chain and produces a BoundMethod on
                    // `self` (which the subsequent CALL invokes). The `defining_class`
                    // we stamp onto the BoundMethod is the ANCESTOR that actually
                    // declared the method, so a NESTED `super` resolves from the right
                    // link too.
                    let idx = fiber.frame().closure.proto.chunk.read_u16(operand_at) as usize;
                    let name = match &fiber.frame().closure.proto.chunk.consts[idx] {
                        Value::Str(s) => s.clone(),
                        other => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                format!("GET_SUPER name is not a string constant: {other:?}"),
                            ))
                        }
                    };
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    // The defining class of the running method (set by
                    // `invoke_compiled_method`). Absent only if `super` somehow
                    // appears outside a method frame — a compiler invariant violation.
                    let def_class = match &fiber.frame().def_class {
                        Some(c) => c.clone(),
                        None => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "'super' used outside of a method".to_string(),
                            ))
                        }
                    };
                    // self = slot 0, read cell-aware (it is a cell slot whenever a
                    // nested closure captured it).
                    let receiver = match &fiber.frame().cells[0] {
                        Some(cell) => cell.borrow().clone(),
                        None => fiber.local(0).clone(),
                    };
                    // Resolve up from the DEFINING class's superclass (NOT the
                    // instance's class), matching `SuperRef { start: superclass }`.
                    let start = def_class.superclass.clone();
                    let bound = match start
                        .as_ref()
                        .and_then(|s| self.find_compiled_method(s, &name))
                    {
                        Some((_closure, found_class)) => Value::BoundMethod(Rc::new(
                            crate::value::BoundMethod {
                                receiver,
                                method: Rc::new(crate::value::Method {
                                    params: Vec::new(),
                                    ret: None,
                                    body: Vec::new(),
                                    is_async: false,
                                }),
                                defining_class: found_class,
                                name: name.to_string(),
                            },
                        )),
                        None => {
                            // Mirror the tree-walker's `Value::Super` member-read
                            // error wording (with/without a superclass).
                            let msg = if start.is_some() {
                                format!("no superclass method '{name}'")
                            } else {
                                format!("no superclass method '{name}' (no superclass)")
                            };
                            return Err(Control::Panic(AsError::at(msg, span)));
                        }
                    };
                    fiber.push(bound);
                }

                other => {
                    return Err(self.panic_at(
                        fiber,
                        fault_ip,
                        format!("opcode {other:?} not yet implemented"),
                    ))
                }
            }
        }
    }

    /// Call ANY value, the single primitive both engines re-enter through.
    ///
    /// This is the bridge in BOTH directions:
    /// - A `Value::Closure` (`native → VM`): a native higher-order stdlib function
    ///   (`array.map`, a sort comparator, `recover`, …) invokes a user callback
    ///   the VM produced. We build a fresh one-frame [`Fiber`] whose sole frame is
    ///   the closure called with `args`, then drive it to completion. Each closure
    ///   invocation gets its OWN Fiber, so the reentrant nesting (VM run → native
    ///   HOF → `call_value` → `Vm::call_value` → `run(new fiber)`) is naturally
    ///   recursive and self-contained.
    /// - Anything else (`VM → native`): delegate to the shared
    ///   [`Interp::call_value`] — identical to the `Op::Call` non-Closure arm.
    ///
    /// Arity / per-param contracts / rest collection use the SAME
    /// [`check_call_args`](crate::interp::check_call_args) the tree-walker and the
    /// `Op::Call` arm use, so a closure called from native code binds its args and
    /// surfaces arity/contract panics byte-identically. The return-type contract is
    /// enforced by `Op::Return` against the frame's `ret_span` (the call span),
    /// exactly as for an in-VM call.
    #[async_recursion::async_recursion(?Send)]
    pub async fn call_value(
        &self,
        callee: Value,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        match callee {
            Value::Closure(closure) => {
                // `what` mirrors the tree-walker's
                // `func.name.as_deref().unwrap_or("function")` so an arity/contract
                // panic message matches.
                let what = closure.proto.chunk.name.as_deref().unwrap_or("function");
                // Arity + per-param contracts + rest collection, shared verbatim
                // with the tree-walker and the `Op::Call` arm.
                let bound = crate::interp::check_call_args(&closure.proto.params, args, span, what)?;
                // Build a one-frame Fiber whose sole frame is the closure, then
                // place the bound params into its slots (cell slot → cell, plain
                // slot → stack). `Fiber::new` already reserved `slot_count` Nil
                // locals and allocated the cell vector, so we only overwrite the
                // param slots; the rest stay Nil.
                let mut fiber = Fiber::new(closure);
                fiber.frame_mut().ret_span = span;
                // Snapshot the cell `Rc`s for the param slots so we don't hold a
                // frame borrow while also writing `fiber.stack` (plain slots).
                let cells = fiber.frame().cells.clone();
                for (slot, v) in bound.into_iter().enumerate() {
                    if let Some(cell) = &cells[slot] {
                        *cell.borrow_mut() = v;
                    } else {
                        fiber.stack[slot] = v;
                    }
                }
                // Drive the fresh fiber to completion. A top-level closure body
                // cannot `yield` (yield is only valid inside a generator, which is
                // driven differently), so `Done(v)` is the only outcome; a `yield`
                // here would be a compiler bug.
                match self.run(&mut fiber).await? {
                    RunOutcome::Done(v) => Ok(v),
                    RunOutcome::Yielded(_) => {
                        unreachable!("a closure called via Vm::call_value cannot yield")
                    }
                }
            }
            // A class constructor (V9): build an instance VM-side (defaults via
            // thunks + compiled `init`) so the init method runs as COMPILED code.
            Value::Class(class) if self.is_vm_class(&class) => {
                self.vm_construct(class, args, span).await
            }
            // A bound method (V9) on a VM-registered class: run the COMPILED method
            // closure with `self` bound to the receiver (slot 0).
            Value::BoundMethod(bm) if self.bound_method_is_vm(&bm).is_some() => {
                let closure = self.bound_method_is_vm(&bm).expect("checked above");
                // The BoundMethod's `defining_class` is the class that actually
                // declared the method (set by `vm_read_member` / `Op::GetSuper` via
                // the chain walk), so a `super.<name>` inside it resolves correctly.
                self.invoke_compiled_method(
                    closure,
                    bm.receiver.clone(),
                    args,
                    span,
                    Some(bm.defining_class.clone()),
                )
                .await
            }
            // Native callee: delegate to the shared dispatch (same as the
            // `Op::Call` non-Closure arm).
            other => self.interp.call_value(other, args, span).await,
        }
    }

    /// Whether `class` is a VM-registered class (it has a compiled-method table).
    /// A class minted by the tree-walker (e.g. via a native module) is NOT here, so
    /// it falls through to the shared `Interp` dispatch.
    fn is_vm_class(&self, class: &Rc<crate::value::Class>) -> bool {
        let key = Rc::as_ptr(class) as usize;
        self.class_methods.borrow().contains_key(&key)
    }

    /// The compiled method closure for `(class identity, name)` looked up ON the
    /// given class ONLY (no chain walk), if registered.
    fn compiled_method_own(
        &self,
        class: &Rc<crate::value::Class>,
        name: &str,
    ) -> Option<Rc<Closure>> {
        let key = Rc::as_ptr(class) as usize;
        self.class_methods
            .borrow()
            .get(&key)
            .and_then(|m| m.get(name))
            .cloned()
    }

    /// Walk the superclass chain from `class` upward, returning the first compiled
    /// method named `name` plus the ANCESTOR class that DEFINED it. The VM method
    /// side-table is keyed by `Rc::as_ptr(class)`, so walking the chain means
    /// probing each ancestor's table in turn. Mirrors the tree-walker's
    /// `value::find_method` (own class first, then up `superclass`), so an
    /// inherited method runs the ancestor's COMPILED closure and a `super` lookup
    /// gets the correct defining class.
    fn find_compiled_method(
        &self,
        class: &Rc<crate::value::Class>,
        name: &str,
    ) -> Option<(Rc<Closure>, Rc<crate::value::Class>)> {
        let mut cur = Some(class.clone());
        while let Some(c) = cur {
            if let Some(closure) = self.compiled_method_own(&c, name) {
                return Some((closure, c));
            }
            cur = c.superclass.clone();
        }
        None
    }

    /// If `bm` is a bound method on a VM-registered class, return its compiled
    /// method closure (resolved up the chain); else `None` (so a tree-walker
    /// BoundMethod delegates).
    fn bound_method_is_vm(
        &self,
        bm: &crate::value::BoundMethod,
    ) -> Option<Rc<Closure>> {
        if let Value::Instance(inst) = &bm.receiver {
            let class = inst.borrow().class.clone();
            // Resolve from the method's DEFINING class (set by `vm_read_member` /
            // `Op::GetSuper`) so an inherited or super-dispatched method runs the
            // right ancestor's closure; fall back to the instance's class chain for
            // a BoundMethod minted elsewhere.
            return self
                .find_compiled_method(&bm.defining_class, &bm.name)
                .or_else(|| self.find_compiled_method(&class, &bm.name))
                .map(|(closure, _)| closure);
        }
        None
    }

    /// VM member read (V9). For an `Instance` of a VM-registered class, a method
    /// name resolves to a `Value::BoundMethod` carrying the receiver + class +
    /// method name (the compiled closure is looked up at CALL time via
    /// `bound_method_is_vm`); a field name reads the stored field; anything else
    /// (and any non-VM receiver) delegates to the shared `Interp::read_member` so
    /// the two engines share field/enum/native member-access semantics. The dummy
    /// `Method` carried by the `BoundMethod` is never executed by the VM — its body
    /// is empty — it exists only to satisfy the frozen `value.rs` `BoundMethod`
    /// shape; method dispatch always runs the COMPILED closure.
    fn vm_read_member(&self, obj: &Value, name: &str, span: Span) -> Result<Value, Control> {
        if let Value::Instance(inst) = obj {
            let (class, has_field) = {
                let b = inst.borrow();
                (b.class.clone(), b.fields.contains_key(name))
            };
            if !has_field {
                // Walk the chain so an INHERITED method binds with the ANCESTOR
                // class as `defining_class` (so a `super` inside it resolves from
                // the right link), mirroring `value::find_method`.
                if let Some((_closure, def_class)) = self.find_compiled_method(&class, name) {
                    let bm = crate::value::BoundMethod {
                        receiver: obj.clone(),
                        method: Rc::new(crate::value::Method {
                            params: Vec::new(),
                            ret: None,
                            body: Vec::new(),
                            is_async: false,
                        }),
                        defining_class: def_class,
                        name: name.to_string(),
                    };
                    return Ok(Value::BoundMethod(Rc::new(bm)));
                }
            }
        }
        // Field / non-VM receiver: shared dispatch (also yields the correct
        // nil-field / nil-receiver behavior, byte-identical to the tree-walker).
        self.interp.read_member(obj, name, span).map_err(Control::from)
    }

    /// Construct an instance of a VM-registered class (V9). Mirrors the
    /// tree-walker's `construct`: create the instance, apply field DEFAULTS (each
    /// via its compiled thunk closure, so a mutable default is fresh per instance),
    /// checking each default against its field-type contract, then run the compiled
    /// `init` method (if present) with the args; a class with no `init` rejects any
    /// args, byte-identically.
    #[async_recursion::async_recursion(?Send)]
    async fn vm_construct(
        &self,
        class: Rc<crate::value::Class>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let instance = Rc::new(RefCell::new(crate::value::Instance {
            class: class.clone(),
            fields: indexmap::IndexMap::new(),
        }));
        let inst_val = Value::Instance(instance.clone());

        // Apply field defaults BASE-CLASS FIRST so a subclass default overrides a
        // base one with the same name (mirrors the tree-walker's `construct`, which
        // iterates `merged_field_schema` — base-first). For each class in the chain
        // (deepest ancestor first), run its defaulted fields' compiled thunks (each
        // thunk is registered under THAT class's identity key) to get a fresh value,
        // check the contract, then store it. The contract panic span is the
        // construct call site (`span`), matching `construct`.
        let mut chain: Vec<Rc<crate::value::Class>> = Vec::new();
        {
            let mut cur = Some(class.clone());
            while let Some(c) = cur {
                cur = c.superclass.clone();
                chain.push(c);
            }
        }
        for c in chain.iter().rev() {
            let key = Rc::as_ptr(c) as usize;
            // Defaulted field names for THIS class, in declared (schema) order.
            let default_names: Vec<String> = self
                .class_defaults
                .borrow()
                .get(&key)
                .map(|m| c.fields.keys().filter(|k| m.contains_key(*k)).cloned().collect())
                .unwrap_or_default();
            for fname in default_names {
                let thunk = self
                    .class_defaults
                    .borrow()
                    .get(&key)
                    .and_then(|m| m.get(&fname))
                    .cloned();
                let Some(thunk) = thunk else { continue };
                let dv = self
                    .call_value(Value::Closure(thunk), Vec::new(), span)
                    .await?;
                if let Some(schema) = c.fields.get(&fname) {
                    if !crate::interp::check_type(&dv, &schema.ty) {
                        return Err(crate::interp::contract_panic(&schema.ty, &dv, span));
                    }
                }
                instance.borrow_mut().fields.insert(fname, dv);
            }
        }

        // Run the compiled `init`, if any — resolved up the chain (a subclass may
        // inherit the base init). `def_class` is the class that DEFINED init, so a
        // `super.init(...)` inside it resolves from the correct link.
        if let Some((init, def_class)) = self.find_compiled_method(&class, "init") {
            self.invoke_compiled_method(init, inst_val.clone(), args, span, Some(def_class))
                .await?;
        } else if !args.is_empty() {
            return Err(AsError::at(
                format!(
                    "{} has no init but was given {} argument(s)",
                    class.name,
                    args.len()
                ),
                span,
            )
            .into());
        }
        Ok(inst_val)
    }

    /// Invoke a COMPILED method closure with `self`=`receiver` bound to slot 0 and
    /// the arguments bound to slots `1..n+1`. The method proto's `arity`/`params`
    /// EXCLUDE `self` (the resolver declares `self` as the method frame's slot 0,
    /// the compiler builds the params from the user params), so arity + per-param
    /// contracts use the SAME `check_call_args` every other call path uses — the
    /// arg contract panic is byte-identical. Drives a fresh one-frame Fiber to
    /// completion (a non-generator/non-async method body cannot `yield`). Async
    /// methods are out of scope for V9-T1 (deferred — a sync `init`/method is the
    /// T1 surface).
    #[async_recursion::async_recursion(?Send)]
    async fn invoke_compiled_method(
        &self,
        closure: Rc<Closure>,
        receiver: Value,
        args: Vec<Value>,
        span: Span,
        def_class: Option<Rc<crate::value::Class>>,
    ) -> Result<Value, Control> {
        let what = closure.proto.chunk.name.as_deref().unwrap_or("method");
        // Bind the user args (arity + per-param contracts + rest) against the
        // method's declared params (which EXCLUDE self) — shared with every call
        // path. The bound values land in slots 1.. (self is slot 0).
        let bound = crate::interp::check_call_args(&closure.proto.params, args, span, what)?;
        let mut fiber = Fiber::new(closure);
        fiber.frame_mut().ret_span = span;
        // Record the DEFINING class so a `super.<name>` in this method body
        // (Op::GetSuper) resolves up from `def_class.superclass`, exactly like the
        // tree-walker's `invoke_method` super binding.
        fiber.frame_mut().def_class = def_class;
        let cells = fiber.frame().cells.clone();
        // self -> slot 0 (cell-aware, in case a nested closure captured self).
        if let Some(cell) = &cells[0] {
            *cell.borrow_mut() = receiver;
        } else {
            fiber.stack[0] = receiver;
        }
        // bound args -> slots 1..n+1.
        for (i, v) in bound.into_iter().enumerate() {
            let slot = i + 1;
            if let Some(cell) = &cells[slot] {
                *cell.borrow_mut() = v;
            } else {
                fiber.stack[slot] = v;
            }
        }
        match self.run(&mut fiber).await? {
            RunOutcome::Done(v) => Ok(v),
            RunOutcome::Yielded(_) => {
                unreachable!("a non-generator method cannot yield")
            }
        }
    }

    /// Build a Tier-2 [`Control::Panic`] whose [`AsError`] is anchored at the span
    /// of the instruction at `ip`, so ariadne points at the source exactly like
    /// the tree-walker.
    fn panic_at(&self, fiber: &Fiber, ip: usize, msg: String) -> Control {
        let span = fiber.frame().closure.proto.chunk.span_at(ip);
        Control::Panic(AsError::at(msg, span))
    }

    /// Unwind ONE call frame, returning `value` from it.
    ///
    /// Shared by `Op::Return` (a normal `return v`) and `Op::Propagate` (a `?`
    /// early-return of a `[nil, err]` pair) — the two have the same mechanics:
    /// pop the current frame; if it declared a `: T` return contract, check the
    /// returned value against it (panicking exactly as the tree-walker's
    /// `run_body` does — anchored at the CALL-site span `frame.ret_span`, with the
    /// identical message — and note the tree-walker applies this same contract to a
    /// `Control::Propagate`-derived value too); truncate the stack back to the
    /// frame's `slot_base` (discarding the callee's locals/operands). Dropping the
    /// frame releases ITS cell `Rc`s — closures that captured them keep their own
    /// strong refs, so by-reference captures stay alive. Recursion is heap-bounded:
    /// each CALL pushed a heap frame and this pops one, so the Rust stack stays flat.
    ///
    /// Returns `Ok(Some(outcome))` when the ROOT frame was popped — the program is
    /// done and `outcome` is its result (the driver treats a top-level propagated
    /// pair as `Ok`, exactly like `run_file`'s `Control::Propagate => Ok`). Returns
    /// `Ok(None)` when a caller frame remains — `value` was pushed onto its stack
    /// and execution continues there.
    fn return_from_frame(
        &self,
        fiber: &mut Fiber,
        value: Value,
    ) -> Result<Option<RunOutcome>, Control> {
        let frame = fiber
            .frames
            .pop()
            .expect("return/propagate with no active frame (VM bug)");
        if let Some(ret_ty) = &frame.closure.proto.ret {
            if !crate::interp::check_type(&value, ret_ty) {
                return Err(crate::interp::contract_panic(ret_ty, &value, frame.ret_span));
            }
        }
        fiber.stack.truncate(frame.slot_base);
        if fiber.frames.is_empty() {
            return Ok(Some(RunOutcome::Done(value)));
        }
        fiber.push(value);
        Ok(None)
    }
}

/// Map a binary-operator opcode to the shared [`BinOp`] the tree-walker uses, so
/// both engines run the SAME `apply_binop` dispatch. Short-circuit operators
/// (`&&`/`||`/`??`) are never lowered to a single binary opcode — the compiler
/// emits jumps for them (V2-T6) — so they have no opcode and never reach here.
fn binop_of(op: Op) -> BinOp {
    match op {
        Op::Add => BinOp::Add,
        Op::Sub => BinOp::Sub,
        Op::Mul => BinOp::Mul,
        Op::Div => BinOp::Div,
        Op::Mod => BinOp::Mod,
        Op::Pow => BinOp::Pow,
        Op::Lt => BinOp::Lt,
        Op::Le => BinOp::Le,
        Op::Gt => BinOp::Gt,
        Op::Ge => BinOp::Ge,
        Op::Eq => BinOp::Eq,
        Op::Ne => BinOp::Ne,
        Op::Range => BinOp::Range,
        _ => unreachable!("binop_of called with non-binary opcode {op:?}"),
    }
}

/// Map a unary-operator opcode to the shared [`UnOp`].
fn unop_of(op: Op) -> UnOp {
    match op {
        Op::Neg => UnOp::Neg,
        Op::Not => UnOp::Not,
        _ => unreachable!("unop_of called with non-unary opcode {op:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::Span;
    use crate::vm::chunk::{Chunk, FnProto};
    use crate::vm::value_ext::Closure;
    use tokio::task::LocalSet;

    /// Wrap a chunk in a closure + fiber and run it to completion on a
    /// current-thread runtime inside a `LocalSet` (the runtime is `!Send`).
    fn run_chunk(chunk: Chunk) -> Result<RunOutcome, Control> {
        let proto = Rc::new(FnProto {
            chunk,
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            params: Vec::new(),
            ret: None,
        });
        let closure = Closure::new(proto);
        let mut fiber = Fiber::new(closure);

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let local = LocalSet::new();
        local.block_on(&rt, async move {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            let vm = Vm::new(interp);
            vm.run(&mut fiber).await
        })
    }

    fn expect_number(chunk: Chunk) -> f64 {
        match run_chunk(chunk).expect("run ok") {
            RunOutcome::Done(Value::Number(n)) => n,
            other => panic!("expected Done(Number), got {other:?}"),
            #[allow(unreachable_patterns)]
            _ => unreachable!(),
        }
    }

    // `RunOutcome` has no Debug; small helper for assert messages.
    impl std::fmt::Debug for RunOutcome {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                RunOutcome::Done(v) => write!(f, "Done({v:?})"),
                RunOutcome::Yielded(v) => write!(f, "Yielded({v:?})"),
            }
        }
    }

    fn s() -> Span {
        Span::new(0, 1)
    }

    #[test]
    fn arithmetic_one_plus_two_times_four() {
        // (1 + 2) * 4 == 12
        let mut c = Chunk::new();
        let k1 = c.add_const(Value::Number(1.0));
        let k2 = c.add_const(Value::Number(2.0));
        let k4 = c.add_const(Value::Number(4.0));
        c.emit_u16(Op::Const, k1, s());
        c.emit_u16(Op::Const, k2, s());
        c.emit(Op::Add, s());
        c.emit_u16(Op::Const, k4, s());
        c.emit(Op::Mul, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 12.0);
    }

    #[test]
    fn negate() {
        let mut c = Chunk::new();
        let k = c.add_const(Value::Number(5.0));
        c.emit_u16(Op::Const, k, s());
        c.emit(Op::Neg, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), -5.0);
    }

    #[test]
    fn modulo() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::Number(7.0));
        let b = c.add_const(Value::Number(3.0));
        c.emit_u16(Op::Const, a, s());
        c.emit_u16(Op::Const, b, s());
        c.emit(Op::Mod, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 1.0);
    }

    #[test]
    fn power() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::Number(2.0));
        let b = c.add_const(Value::Number(10.0));
        c.emit_u16(Op::Const, a, s());
        c.emit_u16(Op::Const, b, s());
        c.emit(Op::Pow, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 1024.0);
    }

    #[test]
    fn less_than_true() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::Number(1.0));
        let b = c.add_const(Value::Number(2.0));
        c.emit_u16(Op::Const, a, s());
        c.emit_u16(Op::Const, b, s());
        c.emit(Op::Lt, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("run ok") {
            RunOutcome::Done(Value::Bool(b)) => assert!(b),
            other => panic!("expected Done(Bool), got {other:?}"),
        }
    }

    #[test]
    fn not_on_truthy() {
        let mut c = Chunk::new();
        c.emit(Op::True, s());
        c.emit(Op::Not, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("run ok") {
            RunOutcome::Done(Value::Bool(b)) => assert!(!b),
            other => panic!("expected Done(Bool), got {other:?}"),
        }
    }

    #[test]
    fn eq_numbers() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::Number(3.0));
        let b = c.add_const(Value::Number(3.0));
        c.emit_u16(Op::Const, a, s());
        c.emit_u16(Op::Const, b, s());
        c.emit(Op::Eq, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("run ok") {
            RunOutcome::Done(Value::Bool(b)) => assert!(b),
            other => panic!("expected Done(Bool), got {other:?}"),
        }
    }

    #[test]
    fn neg_non_number_panics_with_span() {
        // Push a Str const, then NEG -> "cannot negate" panic with a real span.
        let mut c = Chunk::new();
        let k = c.add_const(Value::Str(Rc::from("nope")));
        c.emit_u16(Op::Const, k, s());
        // give NEG a distinct, non-empty span so we can assert it is carried.
        let neg_span = Span::new(5, 9);
        c.emit(Op::Neg, neg_span);
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                assert!(
                    e.message.contains("cannot negate"),
                    "message was: {}",
                    e.message
                );
                let span = e.span.expect("panic carries a span");
                assert_eq!(span, neg_span, "panic carries the faulting op's span");
                assert!(span.end > span.start, "span is non-empty");
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn add_non_numbers_panics() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::Str(Rc::from("a")));
        let b = c.add_const(Value::Number(1.0));
        c.emit_u16(Op::Const, a, s());
        c.emit_u16(Op::Const, b, s());
        c.emit(Op::Add, s());
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => assert!(
                e.message.contains("operator requires two numbers"),
                "message was: {}",
                e.message
            ),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    /// A `Value::Decimal` from a decimal string literal (test helper). The VM
    /// compiler cannot yet *produce* a decimal (that needs `import`/member-access
    /// for `std/decimal`), so the decimal arithmetic path is exercised by pushing
    /// decimal consts directly. The semantics themselves are the SAME shared
    /// `apply_binop` the tree-walker runs, so these tests pin the VM's dispatch to
    /// it.
    fn dec(s: &str) -> Value {
        use std::str::FromStr;
        Value::Decimal(rust_decimal::Decimal::from_str(s).expect("valid decimal literal"))
    }

    /// Push two decimal consts and apply `op`, returning the run outcome.
    fn run_decimal_binop(a: &str, op: Op, b: &str) -> Result<RunOutcome, Control> {
        let mut c = Chunk::new();
        let ka = c.add_const(dec(a));
        let kb = c.add_const(dec(b));
        c.emit_u16(Op::Const, ka, s());
        c.emit_u16(Op::Const, kb, s());
        c.emit(op, s());
        c.emit(Op::Return, s());
        run_chunk(c)
    }

    #[test]
    fn decimal_arithmetic_through_shared_dispatch() {
        // Add / Sub / Mul / Div over two decimals → Decimal, formatted exactly.
        // Expected renderings preserve rust_decimal's scale exactly (the same
        // `Value::Display` the tree-walker uses), so e.g. `3 / 2` is `1.50`.
        for (a, op, b, want) in [
            ("1.5", Op::Add, "2.5", "4.0"),
            ("2.5", Op::Sub, "0.5", "2.0"),
            ("1.5", Op::Mul, "2", "3.0"),
            ("3", Op::Div, "2", "1.50"),
        ] {
            match run_decimal_binop(a, op, b).expect("decimal arith ok") {
                RunOutcome::Done(v) => assert_eq!(
                    v.to_string(),
                    want,
                    "{a} {op:?} {b} rendered wrong"
                ),
                other => panic!("expected Done, got {other:?}"),
            }
        }
    }

    #[test]
    fn decimal_division_by_zero_panics() {
        match run_decimal_binop("1", Op::Div, "0") {
            Err(Control::Panic(e)) => {
                assert_eq!(e.message, "decimal division by zero", "msg: {}", e.message)
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn decimal_remainder_by_zero_panics() {
        match run_decimal_binop("1", Op::Mod, "0") {
            Err(Control::Panic(e)) => {
                assert_eq!(e.message, "decimal remainder by zero", "msg: {}", e.message)
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn decimal_pow_is_unsupported() {
        match run_decimal_binop("2", Op::Pow, "3") {
            Err(Control::Panic(e)) => assert_eq!(
                e.message,
                "exponentiation (**) is not supported for decimal; use math.pow or convert to number",
                "msg: {}",
                e.message
            ),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn decimal_ordering_through_shared_dispatch() {
        match run_decimal_binop("1.5", Op::Lt, "2.5").expect("ok") {
            RunOutcome::Done(Value::Bool(b)) => assert!(b),
            other => panic!("expected Done(Bool), got {other:?}"),
        }
        match run_decimal_binop("3", Op::Ge, "3").expect("ok") {
            RunOutcome::Done(Value::Bool(b)) => assert!(b),
            other => panic!("expected Done(Bool), got {other:?}"),
        }
    }

    #[test]
    fn decimal_vs_number_cross_equality() {
        // decimal("1") == 1 → true (cross-type Decimal↔Number equality), exactly
        // as the tree-walker's `decimal_cross_eq`.
        let mut c = Chunk::new();
        let kd = c.add_const(dec("1"));
        let kn = c.add_const(Value::Number(1.0));
        c.emit_u16(Op::Const, kd, s());
        c.emit_u16(Op::Const, kn, s());
        c.emit(Op::Eq, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Bool(b)) => assert!(b, "decimal(1) == 1 should be true"),
            other => panic!("expected Done(Bool), got {other:?}"),
        }
    }

    #[test]
    fn range_op_builds_half_open_array() {
        // 0 .. 5 → [0, 1, 2, 3, 4].
        let mut c = Chunk::new();
        let k0 = c.add_const(Value::Number(0.0));
        let k5 = c.add_const(Value::Number(5.0));
        c.emit_u16(Op::Const, k0, s());
        c.emit_u16(Op::Const, k5, s());
        c.emit(Op::Range, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Array(a)) => {
                let got: Vec<f64> = a
                    .borrow()
                    .iter()
                    .map(|v| match v {
                        Value::Number(n) => *n,
                        other => panic!("non-number in range array: {other:?}"),
                    })
                    .collect();
                assert_eq!(got, vec![0.0, 1.0, 2.0, 3.0, 4.0]);
            }
            other => panic!("expected Done(Array), got {other:?}"),
        }
    }

    #[test]
    fn range_op_non_number_bounds_panics() {
        let mut c = Chunk::new();
        let ks = c.add_const(Value::Str(Rc::from("x")));
        let k5 = c.add_const(Value::Number(5.0));
        c.emit_u16(Op::Const, ks, s());
        c.emit_u16(Op::Const, k5, s());
        c.emit(Op::Range, s());
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                assert_eq!(e.message, "range bounds must be numbers", "msg: {}", e.message)
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn string_concat_through_add() {
        let mut c = Chunk::new();
        let ka = c.add_const(Value::Str(Rc::from("foo")));
        let kb = c.add_const(Value::Str(Rc::from("bar")));
        c.emit_u16(Op::Const, ka, s());
        c.emit_u16(Op::Const, kb, s());
        c.emit(Op::Add, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Str(st)) => assert_eq!(&*st, "foobar"),
            other => panic!("expected Done(Str), got {other:?}"),
        }
    }

    /// Run a chunk and return the shared interp's captured output alongside the
    /// outcome — for exercising the `print` builtin via `CALL`.
    fn run_chunk_with_output(chunk: Chunk) -> (Result<RunOutcome, Control>, String) {
        let proto = Rc::new(FnProto {
            chunk,
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            params: Vec::new(),
            ret: None,
        });
        let closure = Closure::new(proto);
        let mut fiber = Fiber::new(closure);

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let local = LocalSet::new();
        local.block_on(&rt, async move {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            let vm = Vm::new(interp.clone());
            let outcome = vm.run(&mut fiber).await;
            (outcome, interp.output())
        })
    }

    #[test]
    fn call_print_writes_to_shared_sink() {
        // GET_GLOBAL print; CONST 42; CALL 1; RETURN (CALL leaves print's nil
        // result, which RETURN pops).
        let mut c = Chunk::new();
        let name = c.add_const(Value::Str(Rc::from("print")));
        c.emit_u16(Op::GetGlobal, name, s());
        let k = c.add_const(Value::Number(42.0));
        c.emit_u16(Op::Const, k, s());
        c.emit_u8(Op::Call, 1, s());
        c.emit(Op::Return, s());
        let (outcome, out) = run_chunk_with_output(c);
        assert!(matches!(outcome, Ok(RunOutcome::Done(_))), "ran ok");
        assert_eq!(out, "42\n", "print wrote to the shared capture sink");
    }

    #[test]
    fn get_global_undefined_panics() {
        let mut c = Chunk::new();
        let name = c.add_const(Value::Str(Rc::from("not_a_builtin")));
        let gg_span = Span::new(3, 16);
        c.emit_u16(Op::GetGlobal, name, gg_span);
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                // The message matches the tree-walker's runtime undefined-name
                // error exactly (`undefined variable '<name>'`), so the two
                // engines stay byte-identical even on this defence-in-depth path.
                assert!(
                    e.message.contains("undefined variable"),
                    "message was: {}",
                    e.message
                );
                assert_eq!(e.span, Some(gg_span));
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn unimplemented_op_panics() {
        // An opcode with no exec arm must surface a span-carrying "not yet
        // implemented" Tier-2 panic. `MAKE_GENERATOR` is never emitted by the
        // compiler (a `fn*` CALL builds the generator directly in the CALL arm,
        // mirroring the tree-walker), so it remains unimplemented — a good probe
        // for the catch-all guard. (JUMP/JUMP_IF_* land in V2-T6, AWAIT in V7,
        // YIELD in V8.)
        let mut c = Chunk::new();
        let op_span = Span::new(2, 4);
        c.emit(Op::Nil, s());
        c.emit(Op::MakeGenerator, op_span);
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                assert!(
                    e.message.contains("not yet implemented"),
                    "message was: {}",
                    e.message
                );
                assert_eq!(e.span, Some(op_span));
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    // ---- await exec arm (V7) ---------------------------------------------

    #[test]
    fn await_non_future_is_identity() {
        // `await 5` is identity on a non-future, exactly like the tree-walker's
        // `ExprKind::Await` (`other => Ok(other)`).
        let mut c = Chunk::new();
        let k = c.add_const(Value::Number(5.0));
        c.emit_u16(Op::Const, k, s());
        c.emit(Op::Await, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 5.0);
    }

    // ---- jump exec arms (V2-T6) -------------------------------------------

    #[test]
    fn jump_skips_intervening_code() {
        // NIL is pushed, then an unconditional JUMP hops over a CONST 999, so the
        // result is `nil` (proving the jump landed past the skipped push).
        let mut c = Chunk::new();
        c.emit(Op::Nil, s());
        let site = c.emit_jump(Op::Jump, s());
        let k = c.add_const(Value::Number(999.0));
        c.emit_u16(Op::Const, k, s()); // skipped
        c.patch_jump(site); // land here, leaving only NIL
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Nil) => {}
            other => panic!("expected Done(Nil), got {other:?}"),
        }
    }

    #[test]
    fn jump_if_false_pops_and_branches_on_falsy() {
        // FALSE on the stack -> JUMP_IF_FALSE pops it and jumps; the CONST 1 in
        // between is skipped, so RETURN sees the trailing CONST 2.
        let mut c = Chunk::new();
        c.emit(Op::False, s());
        let site = c.emit_jump(Op::JumpIfFalse, s());
        let k1 = c.add_const(Value::Number(1.0));
        c.emit_u16(Op::Const, k1, s()); // skipped (would otherwise be the result)
        c.patch_jump(site);
        let k2 = c.add_const(Value::Number(2.0));
        c.emit_u16(Op::Const, k2, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 2.0);
    }

    #[test]
    fn jump_if_true_pops_and_falls_through_on_falsy() {
        // FALSE -> JUMP_IF_TRUE pops, does NOT jump, falls through to CONST 7.
        let mut c = Chunk::new();
        c.emit(Op::False, s());
        let site = c.emit_jump(Op::JumpIfTrue, s());
        let k7 = c.add_const(Value::Number(7.0));
        c.emit_u16(Op::Const, k7, s()); // executed (no jump)
        c.emit(Op::Return, s());
        c.patch_jump(site); // target is past RETURN; never reached
        assert_eq!(expect_number(c), 7.0);
    }

    #[test]
    fn jump_if_not_nil_pops_and_branches_on_non_nil() {
        // CONST 5 (non-nil) -> JUMP_IF_NOT_NIL pops & jumps over CONST 1; RETURN
        // sees the trailing CONST 2.
        let mut c = Chunk::new();
        let k5 = c.add_const(Value::Number(5.0));
        c.emit_u16(Op::Const, k5, s());
        let site = c.emit_jump(Op::JumpIfNotNil, s());
        let k1 = c.add_const(Value::Number(1.0));
        c.emit_u16(Op::Const, k1, s()); // skipped
        c.patch_jump(site);
        let k2 = c.add_const(Value::Number(2.0));
        c.emit_u16(Op::Const, k2, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 2.0);
    }

    // ---- collections: literals + index/member read (V2-T4b) ---------------

    #[test]
    fn new_array_preserves_source_order() {
        // CONST 1; CONST 2; CONST 3; NEW_ARRAY 3 → [1, 2, 3].
        let mut c = Chunk::new();
        for n in [1.0, 2.0, 3.0] {
            let k = c.add_const(Value::Number(n));
            c.emit_u16(Op::Const, k, s());
        }
        c.emit_u16(Op::NewArray, 3, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Array(a)) => {
                let got: Vec<f64> = a
                    .borrow()
                    .iter()
                    .map(|v| match v {
                        Value::Number(n) => *n,
                        other => panic!("non-number: {other:?}"),
                    })
                    .collect();
                assert_eq!(got, vec![1.0, 2.0, 3.0]);
            }
            other => panic!("expected Done(Array), got {other:?}"),
        }
    }

    #[test]
    fn new_object_builds_indexmap_in_order() {
        // CONST "a"; CONST 1; CONST "b"; CONST 2; NEW_OBJECT 2 → {a:1, b:2}.
        let mut c = Chunk::new();
        for (k, v) in [("a", 1.0), ("b", 2.0)] {
            let ki = c.add_const(Value::Str(Rc::from(k)));
            c.emit_u16(Op::Const, ki, s());
            let vi = c.add_const(Value::Number(v));
            c.emit_u16(Op::Const, vi, s());
        }
        c.emit_u16(Op::NewObject, 2, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Object(o)) => {
                let b = o.borrow();
                let keys: Vec<&str> = b.keys().map(|k| k.as_str()).collect();
                assert_eq!(keys, vec!["a", "b"], "keys in insertion order");
                assert_eq!(b.get("a"), Some(&Value::Number(1.0)));
                assert_eq!(b.get("b"), Some(&Value::Number(2.0)));
            }
            other => panic!("expected Done(Object), got {other:?}"),
        }
    }

    #[test]
    fn get_index_array() {
        // [10, 20, 30]; CONST 1; GET_INDEX → 20.
        let mut c = Chunk::new();
        for n in [10.0, 20.0, 30.0] {
            let k = c.add_const(Value::Number(n));
            c.emit_u16(Op::Const, k, s());
        }
        c.emit_u16(Op::NewArray, 3, s());
        let i = c.add_const(Value::Number(1.0));
        c.emit_u16(Op::Const, i, s());
        c.emit(Op::GetIndex, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 20.0);
    }

    #[test]
    fn get_index_out_of_bounds_panics() {
        let mut c = Chunk::new();
        let k = c.add_const(Value::Number(10.0));
        c.emit_u16(Op::Const, k, s());
        c.emit_u16(Op::NewArray, 1, s());
        let i = c.add_const(Value::Number(5.0));
        c.emit_u16(Op::Const, i, s());
        c.emit(Op::GetIndex, s());
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => assert!(
                e.message.contains("out of bounds"),
                "msg: {}",
                e.message
            ),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn get_index_object_missing_key_is_nil() {
        // {a:1}["b"] → nil (missing object key is nil, not a panic).
        let mut c = Chunk::new();
        let ka = c.add_const(Value::Str(Rc::from("a")));
        c.emit_u16(Op::Const, ka, s());
        let v1 = c.add_const(Value::Number(1.0));
        c.emit_u16(Op::Const, v1, s());
        c.emit_u16(Op::NewObject, 1, s());
        let kb = c.add_const(Value::Str(Rc::from("b")));
        c.emit_u16(Op::Const, kb, s());
        c.emit(Op::GetIndex, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Nil) => {}
            other => panic!("expected Done(Nil), got {other:?}"),
        }
    }

    #[test]
    fn get_prop_object_field() {
        // {a:1}.a → 1 via GET_PROP "a".
        let mut c = Chunk::new();
        let ka = c.add_const(Value::Str(Rc::from("a")));
        c.emit_u16(Op::Const, ka, s());
        let v1 = c.add_const(Value::Number(1.0));
        c.emit_u16(Op::Const, v1, s());
        c.emit_u16(Op::NewObject, 1, s());
        let name = c.add_const(Value::Str(Rc::from("a")));
        c.emit_u16(Op::GetProp, name, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 1.0);
    }

    #[test]
    fn get_prop_opt_nil_receiver_is_nil() {
        // nil?.a → nil (short-circuit, no read_member call).
        let mut c = Chunk::new();
        c.emit(Op::Nil, s());
        let name = c.add_const(Value::Str(Rc::from("a")));
        c.emit_u16(Op::GetPropOpt, name, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Nil) => {}
            other => panic!("expected Done(Nil), got {other:?}"),
        }
    }

    #[test]
    fn get_prop_nil_receiver_panics() {
        // nil.a → "cannot read property 'a' of nil" (NOT short-circuited).
        let mut c = Chunk::new();
        c.emit(Op::Nil, s());
        let name = c.add_const(Value::Str(Rc::from("a")));
        c.emit_u16(Op::GetProp, name, s());
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => assert!(
                e.message.contains("cannot read property 'a' of nil"),
                "msg: {}",
                e.message
            ),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    // ---- Vm::call_value bridge (native → VM closures), V4-T5 ---------------

    /// Compile a program whose trailing expression evaluates to a closure, run it
    /// on the VM, and return that `Value::Closure`. This is how a native
    /// higher-order function would *receive* a user callback (e.g. the `f` arg of
    /// `array.map`). The closure is self-contained (proto + captured upvalue
    /// cells), so a fresh VM can later drive it via `Vm::call_value`.
    fn compile_closure(src: &str) -> Value {
        let chunk = crate::compile::compile_source(src).expect("compile ok");
        match run_chunk(chunk).expect("run ok") {
            RunOutcome::Done(v @ Value::Closure(_)) => v,
            other => panic!("expected the program to yield a closure, got {other:?}"),
        }
    }

    /// Run `body(vm)` on a current-thread runtime inside a `LocalSet` with a fresh
    /// `Vm` over a fresh `Interp`, mirroring the production entry points. Returns
    /// whatever the async body returns.
    fn with_vm<F, Fut, T>(body: F) -> T
    where
        F: FnOnce(Rc<Vm>) -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let local = LocalSet::new();
        local.block_on(&rt, async move {
            let interp = Rc::new(Interp::new());
            interp.install_self();
            let vm = Vm::new(interp);
            body(vm).await
        })
    }

    #[test]
    fn call_value_runs_a_vm_closure_with_native_supplied_args() {
        // The exact `array.map` shape: a native caller hands the closure ONE arg
        // per element. `(x) => x * 2` called with 21 → 42.
        let f = compile_closure("(x) => x * 2");
        let got = with_vm(|vm| async move {
            vm.call_value(f, vec![Value::Number(21.0)], s())
                .await
                .expect("call ok")
        });
        assert!(matches!(got, Value::Number(n) if n == 42.0), "got {got:?}");
    }

    #[test]
    fn call_value_invokes_a_closure_repeatedly_each_on_its_own_fiber() {
        // A native HOF calls the SAME closure once per element; each invocation is
        // an independent Fiber, so there is no cross-call state leakage.
        let f = compile_closure("(x) => x + 1");
        let got = with_vm(|vm| async move {
            let mut out = Vec::new();
            for n in [10.0, 20.0, 30.0] {
                let v = vm
                    .call_value(f.clone(), vec![Value::Number(n)], s())
                    .await
                    .expect("call ok");
                out.push(v);
            }
            out
        });
        let nums: Vec<f64> = got
            .iter()
            .map(|v| match v {
                Value::Number(n) => *n,
                other => panic!("non-number: {other:?}"),
            })
            .collect();
        assert_eq!(nums, vec![11.0, 21.0, 31.0]);
    }

    #[test]
    fn call_value_closure_observes_its_captured_upvalue() {
        // A closure capturing an outer `k` and applied to a native-supplied arg —
        // exactly `let k = 10; array.map([..], (x) => x + k)`. The captured cell
        // travels with the closure value, so a fresh VM driving it still sees k.
        let f = compile_closure("let k = 10\nlet f = (x) => x + k\nf");
        let got = with_vm(|vm| async move {
            vm.call_value(f, vec![Value::Number(5.0)], s())
                .await
                .expect("call ok")
        });
        assert!(matches!(got, Value::Number(n) if n == 15.0), "got {got:?}");
    }

    // ---- V7-T4: structured-concurrency over VM-produced futures -----------
    //
    // The std/task ops (`gather`/`race`/`timeout`/`spawn`) are native fns on the
    // shared `Interp` that await/select over `Value::Future`s. The VM produces
    // ordinary `Value::Future`s (the SAME `SharedFuture` the tree-walker uses;
    // see the `Op::Call` async-fn arm). These tests de-risk the V12 end-to-end
    // structured-concurrency differential (`concurrency.as` /
    // `structured_concurrency.as`, which need `import` — not compiled until V12)
    // by exercising a task op DIRECTLY over a VM-produced future, with no
    // `import`. They prove the bridge is sound today: `task.gather` over two VM
    // async-fn futures awaits both and preserves order.

    /// Spawn a VM async-fn call exactly the way the `Op::Call` async arm does:
    /// `spawn_local` a task that drives `Vm::call_value(closure, args)` and
    /// resolves a `SharedFuture` cell, returning the `Value::Future` handle
    /// immediately. This is the canonical "VM-produced future".
    fn spawn_vm_future(vm: &Rc<Vm>, closure: Value, args: Vec<Value>) -> Value {
        let vm2 = vm.rc();
        let fut = crate::task::SharedFuture::new();
        let cell = fut.cell();
        let handle = tokio::task::spawn_local(async move {
            let r = vm2.call_value(closure, args, s()).await;
            cell.resolve(r);
        });
        fut.set_abort(handle.abort_handle());
        Value::Future(fut)
    }

    /// Compile + run a whole `.as` program `src` on a fresh Vm (mirroring the
    /// `vm_run_source` entry point) and return the shared `Interp`'s in-flight
    /// high-water mark — used to prove un-awaited async tasks are reaped (bounded),
    /// not leaked (the M17 memory-leak guard, on the VM).
    fn run_program_max_inflight(src: &str) -> u64 {
        let chunk = crate::compile::compile_source(src).expect("compile ok");
        let proto = Rc::new(FnProto {
            chunk,
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            params: Vec::new(),
            ret: None,
        });
        let closure = Closure::new(proto);
        let mut fiber = Fiber::new(closure);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let local = LocalSet::new();
        let interp = Rc::new(Interp::new());
        interp.install_self();
        let vm = Vm::new(interp.clone());
        local.block_on(&rt, async {
            local.run_until(vm.run(&mut fiber)).await.expect("run ok");
        });
        interp.max_inflight()
    }

    #[test]
    fn unawaited_async_loop_keeps_inflight_bounded_on_the_vm() {
        // M17 leak guard, on the VM: a tight loop spawning async calls WITHOUT
        // awaiting them must stay bounded. Each un-awaited future is dropped → its
        // task is cancelled; the cooperative yield above `INFLIGHT_YIELD_CAP`
        // (256) reaps finished/cancelled tasks so the in-flight high-water mark
        // stays well below the iteration count. Without reaping a 5000-iteration
        // loop would peak near 5000. Mirrors the interp's
        // `unawaited_async_loop_keeps_inflight_bounded`.
        let src = "async fn work(n) { return n }\n\
                   let i = 0\n\
                   while (i < 5000) {\n  work(i)\n  i = i + 1\n}\n\
                   print(\"done\")\n";
        let peak = run_program_max_inflight(src);
        assert!(
            peak < 1000,
            "in-flight high-water mark should stay bounded (got {peak})"
        );
    }

    #[test]
    fn task_gather_awaits_vm_produced_futures_in_order() {
        // `(n) => n + 1` invoked as two independent VM futures, gathered. The
        // native `task.gather` op awaits each `Value::Future` and returns the
        // values in input order — proving the VM's futures interoperate with the
        // structured-concurrency machinery (Part C de-risk; full e2e is V12).
        let f = compile_closure("(n) => n + 1");
        let out = with_vm(|vm| async move {
            let a = spawn_vm_future(&vm, f.clone(), vec![Value::Number(10.0)]);
            let b = spawn_vm_future(&vm, f, vec![Value::Number(20.0)]);
            let arr = Value::Array(Rc::new(RefCell::new(vec![a, b])));
            vm.interp()
                .call_task("gather", &[arr], s())
                .await
                .expect("gather ok")
        });
        match out {
            Value::Array(a) => {
                let got: Vec<f64> = a
                    .borrow()
                    .iter()
                    .map(|v| match v {
                        Value::Number(n) => *n,
                        other => panic!("non-number in gather result: {other:?}"),
                    })
                    .collect();
                assert_eq!(got, vec![11.0, 21.0], "gather preserves order over VM futures");
            }
            other => panic!("gather should return an array, got {other:?}"),
        }
    }

    #[test]
    fn task_race_resolves_a_vm_produced_future() {
        // A single VM-produced future raced resolves to its value — `task.race`
        // selects over `Value::Future`s and the VM's future drives to completion.
        let f = compile_closure("(n) => n * 2");
        let out = with_vm(|vm| async move {
            let a = spawn_vm_future(&vm, f, vec![Value::Number(21.0)]);
            let arr = Value::Array(Rc::new(RefCell::new(vec![a])));
            vm.interp()
                .call_task("race", &[arr], s())
                .await
                .expect("race ok")
        });
        assert!(matches!(out, Value::Number(n) if n == 42.0), "got {out:?}");
    }

    #[test]
    fn call_value_propagates_a_closure_panic() {
        // A native HOF whose callback panics must see the SAME `Control::Panic`
        // surface out of `call_value` (so e.g. `array.map` aborts identically).
        // `(x) => x[9]` indexes a 1-element array out of bounds at runtime.
        let f = compile_closure("(x) => x[9]");
        let err = with_vm(|vm| async move {
            let arr = Value::Array(Rc::new(RefCell::new(vec![Value::Number(0.0)])));
            vm.call_value(f, vec![arr], s())
                .await
                .expect_err("expected a panic")
        });
        match err {
            Control::Panic(e) => assert!(
                e.message.contains("out of bounds"),
                "msg: {}",
                e.message
            ),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn call_value_arity_mismatch_panics_like_the_tree_walker() {
        // Calling a 1-param closure with 0 args from native code surfaces the
        // shared `check_call_args` arity panic (same wording as the tree-walker).
        let f = compile_closure("(x) => x");
        let err = with_vm(|vm| async move {
            vm.call_value(f, Vec::new(), s())
                .await
                .expect_err("expected an arity panic")
        });
        match err {
            Control::Panic(e) => assert!(
                e.message.contains("expected 1 argument(s), got 0"),
                "msg: {}",
                e.message
            ),
            other => panic!("expected Panic, got {other:?}"),
        }
    }

    #[test]
    fn call_value_delegates_native_callees_to_the_interp() {
        // A non-closure callee (here the `print` builtin) routes to the shared
        // `Interp::call_value`, exactly like the `Op::Call` non-Closure arm.
        let out = with_vm(|vm| async move {
            let r = vm
                .call_value(Value::Builtin(Rc::from("print")), vec![Value::Number(7.0)], s())
                .await
                .expect("call ok");
            // print returns nil and writes to the shared sink.
            assert!(matches!(r, Value::Nil), "print returns nil");
            vm.interp().output()
        });
        assert_eq!(out, "7\n", "print wrote through the delegated path");
    }

    #[test]
    fn jump_if_not_nil_falls_through_on_nil() {
        // NIL -> JUMP_IF_NOT_NIL pops, does NOT jump, falls through to CONST 9.
        let mut c = Chunk::new();
        c.emit(Op::Nil, s());
        let site = c.emit_jump(Op::JumpIfNotNil, s());
        let k9 = c.add_const(Value::Number(9.0));
        c.emit_u16(Op::Const, k9, s()); // executed (no jump)
        c.emit(Op::Return, s());
        c.patch_jump(site); // never reached
        assert_eq!(expect_number(c), 9.0);
    }

    // ---- PROPAGATE (? operator) at the bytecode level (V6-T1) -------------

    /// A success pair `[7, nil]` through PROPAGATE leaves `7` on the stack
    /// (the `?` expression's result), so the surrounding RETURN yields `7`.
    #[test]
    fn propagate_success_yields_value() {
        let mut c = Chunk::new();
        let pair = c.add_const(crate::interp::make_pair(Value::Number(7.0), Value::Nil));
        c.emit_u16(Op::Const, pair, s());
        c.emit(Op::Propagate, s());
        c.emit(Op::Return, s());
        assert_eq!(expect_number(c), 7.0);
    }

    /// A failure pair `[nil, "boom"]` through PROPAGATE early-returns the
    /// `[nil, err]` pair from the (root) frame — the trailing CONST 999 / RETURN
    /// never run, so the program result is the propagated pair.
    #[test]
    fn propagate_failure_early_returns_pair_from_frame() {
        let mut c = Chunk::new();
        let pair = c.add_const(crate::interp::make_pair(
            Value::Nil,
            Value::Str(Rc::from("boom")),
        ));
        c.emit_u16(Op::Const, pair, s());
        c.emit(Op::Propagate, s());
        // Never reached: PROPAGATE early-returned from the root frame.
        let k999 = c.add_const(Value::Number(999.0));
        c.emit_u16(Op::Const, k999, s());
        c.emit(Op::Return, s());
        match run_chunk(c).expect("ok") {
            RunOutcome::Done(Value::Array(a)) => {
                let b = a.borrow();
                assert_eq!(b.len(), 2);
                assert_eq!(b[0], Value::Nil);
                assert_eq!(b[1], Value::Str(Rc::from("boom")));
            }
            other => panic!("expected Done([nil, \"boom\"]), got {other:?}"),
        }
    }

    /// `expr?` where `expr` is not a 2-element array is a Tier-2 panic carrying
    /// the exact message and the PROPAGATE op's span (the `TryExpr`'s code span).
    #[test]
    fn propagate_non_pair_panics_with_span() {
        let mut c = Chunk::new();
        let k = c.add_const(Value::Number(5.0));
        c.emit_u16(Op::Const, k, s());
        let prop_span = Span::new(8, 10);
        c.emit(Op::Propagate, prop_span);
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                assert_eq!(
                    e.message, "the ? operator requires a Result pair [value, err]",
                    "msg: {}",
                    e.message
                );
                assert_eq!(e.span, Some(prop_span), "panic carries the op's span");
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }
}
