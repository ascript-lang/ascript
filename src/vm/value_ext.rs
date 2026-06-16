//! VM-only runtime types that are *not* (yet) part of [`Value`].
//!
//! [`Closure`] is now a [`Value`] variant (`Value::Closure`, see `src/value.rs`)
//! with CALL/RETURN and upvalue capture wired; this module holds its definition
//! plus the VM-only run-loop status enums ([`RunOutcome`], [`FiberState`]) that
//! the tree-walker never used.

use crate::value::Value;
use crate::vm::chunk::FnProto;
use gcmodule::Cc;
use std::cell::RefCell;
use std::rc::Rc;

/// A runtime closure: a function prototype plus its captured upvalue cells.
///
/// Each cell is shared (`Cc<RefCell<Value>>`) so closures over the same variable
/// observe mutation (by-reference capture). `Cc` (not `Rc`) so a closure-cycle
/// (a closure captured by reference into a value it also reaches) is
/// cycle-collectable (V13). The `upvalues` vector is indexed by upvalue number,
/// matching the resolver's `Resolution::Upvalue(idx)` and the proto's
/// `chunk.upvalues` capture plan.
pub struct Closure {
    pub proto: Rc<FnProto>,
    /// Captured upvalue cells, in upvalue-index order.
    pub upvalues: Vec<Cc<RefCell<Value>>>,
}

impl Closure {
    /// Build a closure with no captured upvalues (the top-level script frame and
    /// any function that captures nothing).
    pub fn new(proto: Rc<FnProto>) -> Cc<Self> {
        Cc::new(Closure {
            proto,
            upvalues: Vec::new(),
        })
    }

    /// Build a closure over `proto` capturing the given upvalue cells (in
    /// upvalue-index order). Used by `Op::Closure` once the capture plan is known.
    pub fn with_upvalues(proto: Rc<FnProto>, upvalues: Vec<Cc<RefCell<Value>>>) -> Cc<Self> {
        Cc::new(Closure { proto, upvalues })
    }
}

/// The result of driving a fiber one step / to completion.
// The run loop (`Vm::run`) returns this; `Done`/`Yielded` are produced by
// RETURN and YIELD respectively.
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
            is_worker: false,
            owning_class: None,
            params: Vec::new(),
            ret: None,
            local_names: Vec::new(),
            debug_name: None,
            name_span: None,
            region_kills: RefCell::new(None),
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
