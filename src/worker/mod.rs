//! Workers Spec A: shared-nothing isolates. `serialize` is the value airlock;
//! `pool`/`isolate`/`dispatch` (later tasks) host the isolate pool + transport.
pub mod serialize;
