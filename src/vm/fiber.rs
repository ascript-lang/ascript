//! [`Fiber`] — the VM's unit of execution: a stack of [`CallFrame`]s over a
//! single shared operand/local [`Value`] stack.
//!
//! Each frame owns a window `stack[slot_base .. slot_base + slot_count]` for its
//! locals; operands push *above* that window. For the single top frame created
//! by [`Fiber::new`], `slot_base == 0`, so operands push above the top frame's
//! locals. When V4 adds CALL/RETURN, a callee frame's `slot_base` is set so the
//! caller's pushed args land in the callee's first local slots.

use crate::span::Span;
use crate::value::Value;
use crate::vm::value_ext::{Closure, FiberState};
use gcmodule::Cc;
use std::cell::RefCell;
use std::rc::Rc;

/// One activation record: the closure being run, the instruction pointer into
/// its chunk, and the base index of this frame's local window on the shared
/// stack.
pub struct CallFrame {
    pub closure: Cc<Closure>,
    pub ip: usize,
    pub slot_base: usize,
    /// Heap cells for this frame's *cell slots* (captured locals), indexed by
    /// local slot. `Some(cell)` for a cell slot (`Cc<RefCell<Value>>`, allocated
    /// nil at frame entry), `None` for a plain stack slot. A closure created in
    /// this frame captures these cells by reference (cloning the `Cc`), so it
    /// observes later mutation. Dropping the frame releases the frame's own
    /// strong refs; any capturing closures keep theirs — correct by-reference
    /// semantics. `Cc` (not `Rc`) so a frame-cell ↔ closure cycle is collectable.
    pub cells: Vec<Option<Cc<RefCell<Value>>>>,
    /// The CALL-site span this frame was invoked at. Used to anchor the
    /// return-type contract panic at RETURN exactly where the tree-walker does
    /// (`run_body` checks the return value against the CALL span, not the
    /// `return` statement's span). For the bottom (script) frame this is unused
    /// (the script body declares no return contract).
    pub ret_span: Span,
    /// The class that DEFINED the method running in this frame (V9-T2), if any.
    /// Set only for method frames (built by `invoke_compiled_method`); `None` for
    /// plain function/script/closure frames. `Op::GetSuper` reads it to resolve a
    /// `super.<name>` lookup starting at `def_class.superclass`, mirroring the
    /// tree-walker's `bm.defining_class.superclass` super binding.
    pub def_class: Option<Rc<crate::value::Class>>,
}

/// Build the per-slot cell vector for a frame from its proto's `cell_slots`
/// (every captured local). `slot_count` sizes the vector; each cell slot gets a
/// fresh `Cc<RefCell<Value::Nil>>`, every other slot is `None`.
pub(crate) fn alloc_cells(
    slot_count: usize,
    cell_slots: &[u32],
) -> Vec<Option<Cc<RefCell<Value>>>> {
    let mut cells = vec![None; slot_count];
    for &slot in cell_slots {
        let idx = slot as usize;
        // The resolver allocated this slot within the frame's window, so it is in
        // range; a stale index would be a resolver/compiler bug.
        cells[idx] = Some(Cc::new(RefCell::new(Value::Nil)));
    }
    cells
}

/// A cooperative execution context: a frame stack over a single value stack.
pub struct Fiber {
    pub frames: Vec<CallFrame>,
    pub stack: Vec<Value>,
    pub state: FiberState,
}

impl Fiber {
    /// Create a fiber running `top` as its sole (bottom) frame. Reserves the
    /// frame's local slots as `Value::Nil` so locals occupy `stack[0 ..
    /// slot_count]`; operands push above. Starts in [`FiberState::Running`].
    pub fn new(top: Cc<Closure>) -> Self {
        let slot_count = top.proto.chunk.slot_count as usize;
        let stack = vec![Value::Nil; slot_count];
        let cells = alloc_cells(slot_count, &top.proto.chunk.cell_slots);
        let frame = CallFrame {
            closure: top,
            ip: 0,
            slot_base: 0,
            cells,
            ret_span: Span::new(0, 0),
            def_class: None,
        };
        Fiber {
            frames: vec![frame],
            stack,
            state: FiberState::Running,
        }
    }

    /// The top (current) frame.
    ///
    /// # Panics
    /// If the frame stack is empty (a VM bug — the VM never calls this with no
    /// frames).
    pub fn frame(&self) -> &CallFrame {
        self.frames
            .last()
            .expect("Fiber::frame called with no active frames (VM bug)")
    }

    /// The top (current) frame, mutably.
    ///
    /// # Panics
    /// If the frame stack is empty (a VM bug).
    pub fn frame_mut(&mut self) -> &mut CallFrame {
        self.frames
            .last_mut()
            .expect("Fiber::frame_mut called with no active frames (VM bug)")
    }

    /// Push an operand onto the stack.
    pub fn push(&mut self, v: Value) {
        self.stack.push(v);
    }

    /// Pop the top operand off the stack.
    ///
    /// # Panics
    /// If the operand stack is empty (a VM bug — the compiler keeps the stack
    /// balanced).
    pub fn pop(&mut self) -> Value {
        self.stack
            .pop()
            .expect("Fiber::pop on empty operand stack (VM bug)")
    }

    /// Peek `back` entries from the top; `peek(0)` is the top of stack.
    ///
    /// # Panics
    /// If `back` is out of bounds (a VM bug).
    pub fn peek(&self, back: usize) -> &Value {
        let len = self.stack.len();
        let idx = len
            .checked_sub(1 + back)
            .expect("Fiber::peek out of bounds (VM bug)");
        &self.stack[idx]
    }

    /// Read local slot `slot` of the current frame
    /// (`stack[frame.slot_base + slot]`).
    ///
    /// # Panics
    /// If the resulting index is out of bounds (a VM bug, not user error).
    pub fn local(&self, slot: usize) -> &Value {
        let base = self.frame().slot_base;
        let idx = base + slot;
        self.stack
            .get(idx)
            .expect("Fiber::local slot out of bounds (compiler bug)")
    }

    /// Write local slot `slot` of the current frame.
    ///
    /// # Panics
    /// If the resulting index is out of bounds (a VM bug, not user error).
    pub fn set_local(&mut self, slot: usize, v: Value) {
        let base = self.frame().slot_base;
        let idx = base + slot;
        let cell = self
            .stack
            .get_mut(idx)
            .expect("Fiber::set_local slot out of bounds (compiler bug)");
        *cell = v;
    }

    /// Read the value held in the current frame's heap cell for `slot`.
    ///
    /// # Panics
    /// If `slot` is not a cell slot (the compiler only emits `GET_LOCAL_CELL` for
    /// resolver cell slots, so a `None` here is a compiler/resolver bug).
    pub fn get_local_cell(&self, slot: usize) -> Value {
        let cell = self.frame().cells.get(slot).and_then(|c| c.as_ref()).expect(
            "Fiber::get_local_cell on a non-cell slot (compiler/resolver bug)",
        );
        cell.borrow().clone()
    }

    /// Store `v` into the current frame's heap cell for `slot`.
    ///
    /// # Panics
    /// If `slot` is not a cell slot (a compiler/resolver bug, as above).
    pub fn set_local_cell(&self, slot: usize, v: Value) {
        let cell = self.frame().cells.get(slot).and_then(|c| c.as_ref()).expect(
            "Fiber::set_local_cell on a non-cell slot (compiler/resolver bug)",
        );
        *cell.borrow_mut() = v;
    }

    /// Install a BRAND-NEW heap cell (`Cc<RefCell<Value::Nil>>`) into the current
    /// frame's `slot`, dropping the frame's strong ref to the previous cell. Any
    /// closure that captured the previous cell keeps it alive with its own value.
    /// Used by `Op::FreshCell` to give each loop iteration a fresh cell for the
    /// loop variable / loop-body captured `let`s (per-iteration capture freshness).
    ///
    /// # Panics
    /// If `slot` is not a cell slot (the compiler only emits `FRESH_CELL` for
    /// resolver cell slots, so a `None` here is a compiler/resolver bug).
    pub fn fresh_cell(&mut self, slot: usize) {
        let slot_cell = self
            .frame_mut()
            .cells
            .get_mut(slot)
            .and_then(|c| c.as_mut())
            .expect("Fiber::fresh_cell on a non-cell slot (compiler/resolver bug)");
        *slot_cell = Cc::new(RefCell::new(Value::Nil));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::chunk::{Chunk, FnProto};

    fn closure_with_slots(slots: u16) -> Cc<Closure> {
        let mut chunk = Chunk::new();
        chunk.slot_count = slots;
        let proto = Rc::new(FnProto {
            chunk,
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            params: Vec::new(),
            ret: None,
        });
        Closure::new(proto)
    }

    fn closure_with_cell_slots(slots: u16, cell_slots: Vec<u32>) -> Cc<Closure> {
        let mut chunk = Chunk::new();
        chunk.slot_count = slots;
        chunk.cell_slots = cell_slots;
        let proto = Rc::new(FnProto {
            chunk,
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            params: Vec::new(),
            ret: None,
        });
        Closure::new(proto)
    }

    #[test]
    fn cell_slots_get_a_cell_others_are_none() {
        // Slot 1 is a cell; slot 0 is a plain stack slot.
        let fiber = Fiber::new(closure_with_cell_slots(2, vec![1]));
        assert!(fiber.frame().cells[0].is_none(), "slot 0 is plain");
        assert!(fiber.frame().cells[1].is_some(), "slot 1 is a cell");
    }

    #[test]
    fn cell_get_set_roundtrip_through_the_cell() {
        let fiber = Fiber::new(closure_with_cell_slots(2, vec![1]));
        fiber.set_local_cell(1, Value::Number(7.0));
        assert!(matches!(fiber.get_local_cell(1), Value::Number(n) if n == 7.0));
        // The cell access does NOT touch the plain stack slot.
        assert!(matches!(fiber.local(1), Value::Nil));
    }

    #[test]
    fn new_reserves_locals_and_one_frame() {
        let fiber = Fiber::new(closure_with_slots(2));
        assert_eq!(fiber.frames.len(), 1);
        assert_eq!(fiber.stack.len(), 2);
        assert!(matches!(fiber.stack[0], Value::Nil));
        assert!(matches!(fiber.stack[1], Value::Nil));
        assert_eq!(fiber.state, FiberState::Running);
        assert_eq!(fiber.frame().ip, 0);
        assert_eq!(fiber.frame().slot_base, 0);
    }

    #[test]
    fn push_pop_peek_lifo() {
        let mut fiber = Fiber::new(closure_with_slots(0));
        fiber.push(Value::Number(1.0));
        fiber.push(Value::Number(2.0));
        fiber.push(Value::Number(3.0));

        assert!(matches!(fiber.peek(0), Value::Number(n) if *n == 3.0));
        assert!(matches!(fiber.peek(1), Value::Number(n) if *n == 2.0));
        assert!(matches!(fiber.peek(2), Value::Number(n) if *n == 1.0));

        assert!(matches!(fiber.pop(), Value::Number(n) if n == 3.0));
        assert!(matches!(fiber.pop(), Value::Number(n) if n == 2.0));
        assert!(matches!(fiber.pop(), Value::Number(n) if n == 1.0));
    }

    #[test]
    fn set_local_and_local_roundtrip() {
        let mut fiber = Fiber::new(closure_with_slots(2));
        fiber.set_local(1, Value::Number(42.0));
        assert!(matches!(fiber.local(1), Value::Number(n) if *n == 42.0));
        assert!(matches!(fiber.local(0), Value::Nil));
    }
}
