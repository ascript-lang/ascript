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
//!
//! **WASM §5.3.5 — platform note (no code change here).** On `wasm32-unknown-unknown`
//! `stacker`/`psm` compile but cannot switch stacks (spike §10.4), so `maybe_grow`
//! degrades to "run the closure on the current stack" — i.e. [`grow`]/[`grow_future`]
//! become the identity pass-throughs they already are when the stack is healthy. wasm
//! has no growable native stack: recursion is bounded by the linker-set shadow stack
//! (raised to 8 MiB by the wrapper) and the LOWERED wasm `MAX_CALL_DEPTH` (1000,
//! `src/interp.rs`) trips the SAME `maximum recursion depth exceeded` panic before the
//! shadow stack overflows. No `cfg` is needed here — `maybe_grow`'s own wasm arm is
//! the pass-through. Native behavior is unchanged (Gate W-1).

/// The minimum remaining native stack (bytes) below which [`grow`] / [`grow_future`]
/// allocate a fresh segment. It must comfortably exceed the **largest native-stack
/// consumption between two consecutive guard checks** — i.e. one logical re-entry
/// step (a `run_body`/`run`/`compile_expr` re-entry plus the dispatch frames around
/// it). In debug builds the `#[async_recursion]` frames in `interp.rs`/`run.rs` are
/// very large (a single HOF re-entry step measured at ~200 KiB), so 1 MiB is a
/// deliberately generous margin that guarantees the guard re-grows BEFORE the next
/// step can overshoot the segment. (128 KiB — the spec's first cut — proved too
/// small for the measured debug frame size; see SP9 §1.3 "measure".)
pub const RED_ZONE: usize = 1024 * 1024;

/// The size (bytes) of each freshly-allocated stack segment. 8 MiB amortizes the
/// allocation across many re-entries (≈30+ logical steps per segment at the
/// measured ~200 KiB/step) so deep recursion does not allocate a segment per call.
pub const STACK_SIZE: usize = 8 * 1024 * 1024;

/// Run `f` on a native stack guaranteed to have at least [`RED_ZONE`] bytes
/// remaining, growing onto a fresh [`STACK_SIZE`] heap segment first if needed.
///
/// Inert (a cheap remaining-stack probe, no allocation) when the stack is healthy,
/// which is the overwhelmingly common case — so the hot path pays only the probe.
/// Place this at a synchronous native re-entry funnel (e.g. the compiler's
/// `compile_expr`) so the native-stack-consuming portion of each re-entry runs
/// inside a grown segment.
#[inline]
pub fn grow<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(RED_ZONE, STACK_SIZE, f)
}

/// Drive `fut` to completion with every `poll` run on a stack guaranteed to have
/// at least [`RED_ZONE`] bytes remaining (growing a fresh [`STACK_SIZE`] segment
/// when low), then return its output. The async-boundary analogue of [`grow`].
///
/// `#[async_recursion]` boxes each recursive future; the *synchronous* portion of a
/// re-entry — the run from one suspension point to the next, which is where the
/// native stack is actually consumed by nested recursion — happens entirely inside
/// a single `poll`. Wrapping every `poll` in `stacker::maybe_grow` therefore runs
/// that native-stack-consuming work inside a grown segment, with no real
/// suspension required (CPU-bound deep recursion polls straight through). Inert
/// (a probe per poll) until the stack runs low.
///
/// No `unsafe`: the future is kept boxed-and-pinned for its whole life and only
/// `poll`ed, never moved, so a plain `&mut Pin<Box<F>>` re-pin via `as_mut()` is
/// sound. Returns a type-erased `Pin<Box<dyn Future + 'a>>` so it has a fixed size
/// at every call site — this breaks the `#[async_recursion]` cycle (`run →
/// call_value → grow_future → run`) that would otherwise be unsizeable. The `'a`
/// lifetime lets the wrapped future borrow (e.g. `&mut Fiber`, `&self`), as the VM
/// re-entry futures do.
pub fn grow_future<'a, F, O>(fut: F) -> std::pin::Pin<Box<dyn std::future::Future<Output = O> + 'a>>
where
    F: std::future::Future<Output = O> + 'a,
{
    let mut fut = Box::pin(fut);
    Box::pin(std::future::poll_fn(move |cx| {
        stacker::maybe_grow(RED_ZONE, STACK_SIZE, || fut.as_mut().poll(cx))
    }))
}
