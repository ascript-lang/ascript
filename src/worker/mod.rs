//! Workers Spec A: shared-nothing isolates. `serialize` is the value airlock;
//! `dispatch` builds the shippable code slice (entry fn + its transitive top-level
//! dependency closure, materialized as a `.aso` module fragment); `pool`/`isolate`
//! (later tasks) host the isolate pool + the `Send` byte-channel transport.

pub mod dispatch;
pub mod serialize;

use std::rc::Rc;

pub use dispatch::build_code_slice;

/// The shippable bytecode payload for one worker fn: its compiled chunk plus its
/// transitive top-level dependency closure (other top-level `fn`s and literal
/// `const`s it references), serialized via the `.aso` writer as a self-contained
/// "module fragment", keyed by a stable function identity for per-isolate caching.
///
/// Running `entry_aso` on a FRESH isolate's `Vm` defines exactly the closure's
/// globals (and the entry) and nothing else from the original module — so the
/// isolate can fetch and call the entry with zero access to the original heap.
pub struct WorkerCodeSlice {
    /// Identity for the per-isolate code cache (a stable hash of the entry's
    /// `class_name` + name). A repeatedly-dispatched worker ships its bytecode at
    /// most once per isolate, keyed by this id (Task 8).
    pub fn_id: u64,
    /// The `.aso` bytes: the module fragment carrying the transitive deps + the
    /// entry fn define.
    pub entry_aso: Rc<[u8]>,
    /// `Some(class)` for a `static worker fn` on a class; `None` for a free
    /// `worker fn`. Task 8 binds the class on the far isolate.
    pub class_name: Option<Rc<str>>,
}
