//! `worker class` actor runtime (Spec B, Task 5) — the dedicated-isolate side.
//!
//! An actor is a class instance born in its OWN dedicated isolate (one per actor, via
//! [`crate::worker::isolate::spawn_isolate`]), holding state across calls, talked to
//! over time through a FIFO mailbox: method calls become serialized messages sent over
//! the [`IsolateHandle`]'s inbound `Send` byte channel; each is processed to completion
//! before the next (the GenServer one-at-a-time guarantee). Resources opened in `init`
//! stay in the isolate and never cross — only `Send` bytes do.
//!
//! ## Task 4 status (substrate only)
//!
//! Task 4 ships the SHARED dedicated-isolate lifecycle: [`spawn_isolate`] + the
//! [`IsolateHandle`] ownership / teardown model (cancel-on-drop, no zombie thread). The
//! actual actor protocol — `ClassName.spawn(args)` routing, the `ActorMsg` mailbox
//! framing, async method dispatch returning `future<T>`, the non-reentrancy guard, and
//! `close`/last-drop teardown — is Task 5 and lands here. Until then this module only
//! re-exports the substrate so the seam is explicit.

#[allow(unused_imports)] // wired up by Task 5 (the actor mailbox is built over this).
pub(crate) use crate::worker::isolate::{spawn_isolate, IsolateHandle};
