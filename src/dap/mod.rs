//! DBG Task 5b — the Debug Adapter Protocol (DAP) server over stdio.
//!
//! This sits ON TOP of the already-landed VM debug core (the parked-VM command
//! protocol in [`crate::vm::instrument`] + [`crate::vm::run::Vm::debug_stop`]). It
//! adds NO new VM semantics — it only drives the existing
//! [`DebugCommand`](crate::vm::instrument::DebugCommand) /
//! [`DebugEvent`](crate::vm::instrument::DebugEvent) channels and translates them
//! to/from the wire DAP JSON protocol an editor speaks.
//!
//! # Threading (the load-bearing shape)
//!
//! The VM is `!Send` and parks by BLOCKING on a std mpsc `recv` inside `debug_stop`.
//! So three roles run on three threads, all talking plain `Send` data:
//!
//! - **The debuggee thread** ([`launch::spawn_debuggee`]) — a dedicated
//!   `WORKER_STACK_SIZE` `std::thread` with its OWN current-thread tokio runtime +
//!   `LocalSet`. It builds the instrumented `Vm`, registers the proto tree, sets
//!   break-on-entry, runs the program to completion, then ships
//!   [`DebugEvent::Output`] + [`DebugEvent::Terminated`] and drops the hook.
//! - **The event-pump thread** ([`server`]) — loops `evt_rx.recv()` and writes the
//!   corresponding DAP event to stdout through a shared `Mutex<Stdout>`.
//! - **The main (DAP server) thread** — reads Content-Length-framed DAP requests
//!   from stdin and writes responses through the SAME `Mutex<Stdout>`. Fully
//!   synchronous (no async runtime here — avoids runtime-nesting).
//!
//! `stackTrace`/`scopes`/`variables` are answered from the CACHED frames of the last
//! `Stopped` event (already crossed the airlock as plain data) — never a round-trip
//! to the VM thread.

mod launch;
mod proto;
mod server;

pub use server::run_server;
