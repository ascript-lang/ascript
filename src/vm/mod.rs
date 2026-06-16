//! The AScript bytecode runtime (VM plan V1+).
//!
//! Built alongside the existing async tree-walking interpreter; nothing here is
//! wired into the binary yet. Exec arms, the compiler, and the run loop land in
//! later VM plan slices (V2–V10).

pub mod adapt;
pub mod archive;
pub mod aso;
pub mod defer_metrics;
pub(crate) mod bcanalysis;
pub mod chunk;
pub mod coverage_report;
pub(crate) mod decode;
pub mod disasm;
pub mod fiber;
pub mod ic;
pub mod instrument;
pub mod opcode;
/// REGION Phase-0 allocation-lifetime probe (spec §5.2). Dev-only — compiled OUT
/// unless `--features region-probe`.
#[cfg(feature = "region-probe")]
pub mod region_probe;
pub mod run;
pub mod shape;
pub(crate) mod trampoline;
pub mod stack;
pub mod value_ext;
pub mod verify;

/// REGION probe (spec §5.2): reclassify the freshly-pushed top-of-stack container
/// cell's birth site as `Literal`. Called by the `Op::NewObject`/`Op::NewArray`
/// handlers (both lanes). Dev-only — compiled OUT unless `--features region-probe`.
#[cfg(feature = "region-probe")]
#[inline]
pub(crate) fn region_probe_mark_top(fiber: &fiber::Fiber) {
    use crate::value::ValueKind;
    match fiber.peek(0).kind() {
        ValueKind::Object(o) => o.region_mark_literal(),
        ValueKind::Array(a) => a.region_mark_literal(),
        _ => {}
    }
}

pub use chunk::{Chunk, FnProto};
pub use disasm::{disasm, disasm_at};
pub use fiber::{CallFrame, Fiber};
pub use opcode::Op;
pub use run::Vm;
pub use value_ext::{Closure, FiberState, RunOutcome};
