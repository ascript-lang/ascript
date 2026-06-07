//! `worker fn*` streaming-generator runtime (Spec B, Task 6) — the dedicated-isolate
//! driver behind `GenImpl::Worker`.
//!
//! A streaming generator runs its producer body in its OWN dedicated isolate (via
//! [`crate::worker::isolate::spawn_isolate`]); the caller side is a demand-driven
//! driver: each `resume`/`.next(v)` sends one demand credit over the [`IsolateHandle`]'s
//! inbound `Send` byte channel and awaits the next serialized yield (or done/error). A
//! bounded prefetch buffer (default 1 = strict pull) gives backpressure. Teardown on
//! `close`/last-drop reclaims the isolate.
//!
//! ## Task 4 status (substrate only)
//!
//! Task 4 ships the SHARED dedicated-isolate lifecycle: [`spawn_isolate`] + the
//! [`IsolateHandle`] ownership / teardown model (cancel-on-drop, no zombie thread). The
//! `StreamDriver`, the `GenImpl::Worker` wiring in `src/coro.rs`, the demand/yield
//! channel protocol, and bidirectional `next(v)` are Task 6 and land here. Until then
//! this module only re-exports the substrate so the seam is explicit.

#[allow(unused_imports)] // wired up by Task 6 (the StreamDriver is built over this).
pub(crate) use crate::worker::isolate::{spawn_isolate, IsolateHandle};
