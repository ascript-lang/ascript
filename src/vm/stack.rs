//! SP9 §1 — robust unbounded recursion via a probe-and-grow native stack.
//!
//! The runtime is **model 2a**: straight script recursion pushes heap-backed
//! `Fiber.frames`, not native Rust frames, so it is bounded by heap (and the SP3
//! logical [`crate::interp::MAX_CALL_DEPTH`] cap), never the native stack. The
//! residual native recursion is the narrow set of **re-entry points** where a
//! native funnel re-enters the engine on a fresh Rust frame:
//!
//! - higher-order stdlib callbacks (`array.map`/`reduce`/… via `Vm::call_value`),
//! - non-IC method dispatch / construction (`invoke_compiled_method`/`vm_construct`),
//! - nested generator composition (`coro::resume_vm`),
//! - the synchronous compiler `compile_expr` and the tree-walker `eval_expr`/`run_body`.
//!
//! At each of those, [`grow`] checks the remaining native stack and, if it is below
//! [`RED_ZONE`], allocates a fresh [`STACK_SIZE`] segment before running the closure
//! (`stacker::maybe_grow`). The guard is a no-op when the stack is healthy, so a
//! program already within limits is byte-identical; a program that previously
//! `SIGABRT`ed (exit 134) below the cap now reaches SP3's clean
//! `maximum recursion depth exceeded` panic (or completes) — on BOTH engines.
//!
//! Coordination with SP3 (`MAX_CALL_DEPTH`): the logical cap stays the ceiling and
//! the safety backstop (it also bounds heap-`frames` growth, which `stacker` does
//! NOT bound). `grow` only ensures the native re-entry paths *reach* that cap
//! instead of overflowing the native stack first.

/// The minimum remaining native stack (bytes) below which [`grow`] allocates a
/// fresh segment. 128 KiB is deliberately generous — it must comfortably exceed
/// the largest single VM / tree-walker frame (the `#[async_recursion]` frames in
/// `interp.rs` are large), so the boxed re-entry future is never entered with less
/// than this much headroom.
pub const RED_ZONE: usize = 128 * 1024;

/// The size (bytes) of each freshly-allocated stack segment. 2 MiB matches a
/// typical thread stack and amortizes the allocation across many re-entries.
pub const STACK_SIZE: usize = 2 * 1024 * 1024;

/// Run `f` on a native stack guaranteed to have at least [`RED_ZONE`] bytes
/// remaining, growing onto a fresh [`STACK_SIZE`] heap segment first if needed.
///
/// Inert (a cheap remaining-stack probe, no allocation) when the stack is healthy,
/// which is the overwhelmingly common case — so the hot path pays only the probe.
/// Place this at a native re-entry funnel so the synchronous, native-stack-consuming
/// portion of each re-entry runs inside a grown segment.
#[inline]
pub fn grow<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(RED_ZONE, STACK_SIZE, f)
}
