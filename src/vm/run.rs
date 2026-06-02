//! The VM's async run loop (`Vm::run`).
//!
//! V1 implements the **synchronous arithmetic subset** only: constants, the
//! literal pushes, stack `Pop`/`Dup`, the numeric binary/unary operators, the
//! numeric comparisons, numeric `Eq`/`Ne`, and `Return`. Every other opcode is a
//! documented `not yet implemented` Tier-2 panic that later VM slices (V2–V10)
//! fill in. Panics carry the faulting instruction's [`Span`] so ariadne points at
//! the source exactly like the tree-walker.
//!
//! The loop mirrors the tree-walker's Number-path semantics
//! (`src/interp.rs`): the full String-concat / Decimal / Range / container paths
//! are deferred to V2, which replaces the subset's "two numbers" panic with the
//! complete dispatch.

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

                Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Mod | Op::Pow => {
                    let b = fiber.pop();
                    let a = fiber.pop();
                    let v = self.num_arith(op, a, b, fiber, fault_ip)?;
                    fiber.push(v);
                }
                Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                    let b = fiber.pop();
                    let a = fiber.pop();
                    let v = self.num_compare(op, a, b, fiber, fault_ip)?;
                    fiber.push(v);
                }
                Op::Eq | Op::Ne => {
                    let b = fiber.pop();
                    let a = fiber.pop();
                    let v = self.num_eq(op, a, b, fiber, fault_ip)?;
                    fiber.push(v);
                }

                Op::Neg => {
                    let a = fiber.pop();
                    match a {
                        Value::Number(n) => fiber.push(Value::Number(-n)),
                        _ => {
                            return Err(self.panic_at(
                                fiber,
                                fault_ip,
                                "cannot negate a non-number".to_string(),
                            ))
                        }
                    }
                }
                Op::Not => {
                    let a = fiber.pop();
                    fiber.push(Value::Bool(!a.is_truthy()));
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

    /// Numeric binary arithmetic (`Add/Sub/Mul/Div/Mod/Pow`) over two
    /// [`Value::Number`]s. Any non-Number operand is the subset's deferred path →
    /// Tier-2 panic at the faulting instruction's span (V2 replaces this with the
    /// full dispatch).
    fn num_arith(
        &self,
        op: Op,
        a: Value,
        b: Value,
        fiber: &Fiber,
        fault_ip: usize,
    ) -> Result<Value, Control> {
        match (a, b) {
            (Value::Number(a), Value::Number(b)) => Ok(Value::Number(match op {
                Op::Add => a + b,
                Op::Sub => a - b,
                Op::Mul => a * b,
                Op::Div => a / b,
                Op::Mod => a % b,
                Op::Pow => a.powf(b),
                _ => unreachable!("num_arith called with non-arith op {op:?}"),
            })),
            _ => Err(self.operator_requires_numbers(fiber, fault_ip)),
        }
    }

    /// Numeric ordering comparison (`Lt/Le/Gt/Ge`) → [`Value::Bool`].
    fn num_compare(
        &self,
        op: Op,
        a: Value,
        b: Value,
        fiber: &Fiber,
        fault_ip: usize,
    ) -> Result<Value, Control> {
        match (a, b) {
            (Value::Number(a), Value::Number(b)) => Ok(Value::Bool(match op {
                Op::Lt => a < b,
                Op::Le => a <= b,
                Op::Gt => a > b,
                Op::Ge => a >= b,
                _ => unreachable!("num_compare called with non-compare op {op:?}"),
            })),
            _ => Err(self.operator_requires_numbers(fiber, fault_ip)),
        }
    }

    /// Numeric equality (`Eq/Ne`) → [`Value::Bool`]. The full decimal-cross /
    /// container equality is V2; the subset compares two Numbers only and treats
    /// any non-Number operand as the deferred path → panic.
    fn num_eq(
        &self,
        op: Op,
        a: Value,
        b: Value,
        fiber: &Fiber,
        fault_ip: usize,
    ) -> Result<Value, Control> {
        match (a, b) {
            (Value::Number(a), Value::Number(b)) => Ok(Value::Bool(match op {
                Op::Eq => a == b,
                Op::Ne => a != b,
                _ => unreachable!("num_eq called with non-eq op {op:?}"),
            })),
            _ => Err(self.operator_requires_numbers(fiber, fault_ip)),
        }
    }

    /// The subset's "two numbers" panic, carrying the faulting instruction's span.
    fn operator_requires_numbers(&self, fiber: &Fiber, fault_ip: usize) -> Control {
        self.panic_at(
            fiber,
            fault_ip,
            "operator requires two numbers (or two decimals, or number and decimal)".to_string(),
        )
    }

    /// Build a Tier-2 [`Control::Panic`] whose [`AsError`] is anchored at the span
    /// of the instruction at `ip`, so ariadne points at the source exactly like
    /// the tree-walker.
    fn panic_at(&self, fiber: &Fiber, ip: usize, msg: String) -> Control {
        let span = fiber.frame().closure.proto.chunk.span_at(ip);
        Control::Panic(AsError::at(msg, span))
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
