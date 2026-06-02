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
use crate::interp::{Control, Interp};
use crate::value::Value;
use crate::vm::fiber::Fiber;
use crate::vm::opcode::Op;
use crate::vm::value_ext::RunOutcome;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// The bytecode virtual machine.
///
/// Holds the shared [`Interp`] (the runtime state the VM and tree-walker share)
/// and a self-`Weak` mirroring [`Interp`]'s pattern, so a `&self` method can
/// recover an owned `Rc<Vm>` to hand to a spawned task in V7.
pub struct Vm {
    interp: Rc<Interp>,
    self_weak: RefCell<Weak<Vm>>,
}

impl Vm {
    /// Build a VM over `interp` and install its self-`Weak` (mirroring
    /// [`Interp::install_self`]).
    pub fn new(interp: Rc<Interp>) -> Rc<Self> {
        let vm = Rc::new(Vm {
            interp,
            self_weak: RefCell::new(Weak::new()),
        });
        *vm.self_weak.borrow_mut() = Rc::downgrade(&vm);
        vm
    }

    /// Recover an owned `Rc<Vm>` from `&self`.
    // used by V7 (spawn): no caller yet in V1.
    #[allow(dead_code)]
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

                Op::Call => {
                    let argc = fiber.frame().closure.proto.chunk.read_u8(operand_at) as usize;
                    // The callee sits just below its `argc` arguments on the stack:
                    // `[..., callee, arg0, .., arg{argc-1}]`. Its stack index is the
                    // base where, for a Closure callee, the args become the callee
                    // frame's first local slots (the CALL convention).
                    let callee_idx = fiber.stack.len() - argc - 1;
                    match fiber.stack[callee_idx].clone() {
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
                            });
                            // Continue the loop in the new frame (the run loop reads
                            // `fiber.frame()` at the top of each iteration). RETURN
                            // pops this frame and restores the caller.
                        }
                        other => {
                            // Native callee (Builtin/Function/Class/BoundMethod/...):
                            // delegate to the shared `call_value`. Pop the args and
                            // the callee into owned locals BEFORE the await so no
                            // borrow of `fiber` is held across the suspension point
                            // (`await_holding_refcell_ref` stays clean).
                            let mut args = vec![Value::Nil; argc];
                            for slot in args.iter_mut().rev() {
                                *slot = fiber.pop();
                            }
                            let _callee = fiber.pop(); // the Value at callee_idx
                            let span =
                                fiber.frame().closure.proto.chunk.span_at(fault_ip);
                            let result =
                                self.interp.call_value(other, args, span).await?;
                            fiber.push(result);
                        }
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
                        let v = self.interp.read_member(&obj, &name, span)?;
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
                    // Pop the result; pop the current frame; truncate the stack back
                    // to the frame's slot_base (discarding the callee's locals and
                    // operands); push the result. If no frames remain, the program is
                    // done; otherwise execution continues in the caller. Dropping the
                    // frame releases ITS cell `Rc`s — any closures that captured those
                    // cells keep their own strong refs, so by-reference captures stay
                    // alive. Recursion is heap-bounded: each CALL pushed a heap frame
                    // and this just pops one, so the Rust stack stays flat.
                    let result = fiber.pop();
                    let frame = fiber
                        .frames
                        .pop()
                        .expect("Op::Return with no active frame (VM bug)");
                    // Return-type contract: if the callee declared `: T`, the
                    // returned value is checked against it, panicking exactly as the
                    // tree-walker's `run_body` does — anchored at the CALL-site span
                    // (`frame.ret_span`), with the identical message.
                    if let Some(ret_ty) = &frame.closure.proto.ret {
                        if !crate::interp::check_type(&result, ret_ty) {
                            return Err(crate::interp::contract_panic(
                                ret_ty,
                                &result,
                                frame.ret_span,
                            ));
                        }
                    }
                    fiber.stack.truncate(frame.slot_base);
                    if fiber.frames.is_empty() {
                        return Ok(RunOutcome::Done(result));
                    }
                    fiber.push(result);
                }

                // V2–V10 fill these in; the V1 smoke only exercises the subset above.
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

    /// Build a Tier-2 [`Control::Panic`] whose [`AsError`] is anchored at the span
    /// of the instruction at `ip`, so ariadne points at the source exactly like
    /// the tree-walker.
    fn panic_at(&self, fiber: &Fiber, ip: usize, msg: String) -> Control {
        let span = fiber.frame().closure.proto.chunk.span_at(ip);
        Control::Panic(AsError::at(msg, span))
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
        // `AWAIT` is not implemented until a later VM slice; an unimplemented op
        // must surface a span-carrying "not yet implemented" Tier-2 panic.
        // (JUMP/JUMP_IF_*/JUMP_IF_NOT_NIL are now implemented as of V2-T6.)
        let mut c = Chunk::new();
        let await_span = Span::new(2, 4);
        c.emit(Op::Nil, s());
        c.emit(Op::Await, await_span);
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                assert!(
                    e.message.contains("not yet implemented"),
                    "message was: {}",
                    e.message
                );
                assert_eq!(e.span, Some(await_span));
            }
            other => panic!("expected Panic, got {other:?}"),
        }
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
}
