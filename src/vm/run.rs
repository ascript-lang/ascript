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
                    // Pop everything into owned locals BEFORE the await so no
                    // borrow of `fiber` is held across the suspension point
                    // (`await_holding_refcell_ref` stays clean).
                    let mut args = vec![Value::Nil; argc];
                    for slot in args.iter_mut().rev() {
                        *slot = fiber.pop();
                    }
                    let callee = fiber.pop();
                    let span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
                    // `call_value` dispatches Builtin/Function/Class/etc. The only
                    // callee it cannot handle is `Value::Closure`, which the
                    // compiler does not emit a call for until V4.
                    let result = self.interp.call_value(callee, args, span).await?;
                    fiber.push(result);
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

                Op::Return => {
                    let result = fiber.pop();
                    return Ok(RunOutcome::Done(result));
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
        // JUMP is not implemented in the V1 subset.
        let mut c = Chunk::new();
        let jump_span = Span::new(2, 4);
        let site = c.emit_jump(Op::Jump, jump_span);
        c.patch_jump(site);
        c.emit(Op::Nil, s());
        c.emit(Op::Return, s());
        match run_chunk(c) {
            Err(Control::Panic(e)) => {
                assert!(
                    e.message.contains("not yet implemented"),
                    "message was: {}",
                    e.message
                );
                assert_eq!(e.span, Some(jump_span));
            }
            other => panic!("expected Panic, got {other:?}"),
        }
    }
}
