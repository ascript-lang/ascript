//! The AScript bytecode runtime (VM plan V1+).
//!
//! Built alongside the existing async tree-walking interpreter; nothing here is
//! wired into the binary yet. Exec arms, the compiler, and the run loop land in
//! later VM plan slices (V2–V10).

pub mod chunk;
pub mod disasm;
pub mod fiber;
pub mod ic;
pub mod opcode;
pub mod run;
pub mod shape;
pub mod value_ext;

pub use chunk::{Chunk, FnProto};
pub use disasm::{disasm, disasm_at};
pub use fiber::{CallFrame, Fiber};
pub use opcode::Op;
pub use run::Vm;
pub use value_ext::{Closure, FiberState, RunOutcome};
