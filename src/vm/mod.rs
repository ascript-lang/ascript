//! The AScript bytecode runtime (VM plan V1+).
//!
//! Built alongside the existing async tree-walking interpreter; nothing here is
//! wired into the binary yet. Exec arms, the compiler, and the run loop land in
//! later VM plan slices (V2–V10).

pub mod chunk;
pub mod opcode;

pub use chunk::{Chunk, FnProto};
pub use opcode::Op;
