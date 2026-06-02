//! VM-only runtime types that are *not* (yet) part of [`Value`].
//!
//! The bytecode VM needs a closure representation ([`Closure`]) and small
//! status enums ([`RunOutcome`], [`FiberState`]) that the tree-walker never
//! used. These live here for now; V4/V5 fold [`Closure`] into [`Value`] once
//! CALL/RETURN and upvalue capture are wired.

use crate::value::Value;
use crate::vm::chunk::FnProto;
use std::cell::RefCell;
use std::rc::Rc;

/// A runtime closure: a function prototype plus its captured upvalue cells.
///
/// `upvalues` stays empty until V5 wires capture; each cell is shared
/// (`Rc<RefCell<Value>>`) so closures over the same variable observe mutation.
pub struct Closure {
    pub proto: Rc<FnProto>,
    /// Captured upvalue cells; empty until V5 wires captures.
    pub upvalues: Vec<Rc<RefCell<Value>>>,
}

impl Closure {
    /// Build a closure with no captured upvalues (the common case until V5).
    pub fn new(proto: Rc<FnProto>) -> Rc<Self> {
        Rc::new(Closure {
            proto,
            upvalues: Vec::new(),
        })
    }
}

/// The result of driving a fiber one step / to completion.
// consumed by V1-T5/V4 (the run loop returns this; not yet driven this task).
#[allow(dead_code)]
pub enum RunOutcome {
    Done(Value),
    Yielded(Value),
}

/// Lifecycle state of a [`super::fiber::Fiber`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FiberState {
    Running,
    Suspended,
    Done,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::chunk::Chunk;

    fn proto_with_slots(slots: u16) -> Rc<FnProto> {
        let mut chunk = Chunk::new();
        chunk.slot_count = slots;
        Rc::new(FnProto {
            chunk,
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
        })
    }

    #[test]
    fn closure_new_has_no_upvalues() {
        let proto = proto_with_slots(0);
        let c = Closure::new(proto.clone());
        assert!(c.upvalues.is_empty());
        assert!(Rc::ptr_eq(&c.proto, &proto));
    }
}
