//! Bytecode verifier for a [`Chunk`] (a loaded `.aso`, or any chunk before it is
//! run).
//!
//! Loading an `.aso` runs its bytecode. We treat `.aso` as *trusted-but-verified*
//! (like CPython's `.pyc`): a malformed or corrupt chunk must be REJECTED at load
//! with a clear error, never cause undefined behavior or a deep VM panic. This pass
//! validates the structural invariants the VM's run loop assumes but does NOT
//! re-check at execution time:
//!
//! 1. **Decode integrity** — every byte is reached as exactly one opcode byte or
//!    one of its inline-operand bytes; each opcode decodes via [`Op::from_u8`]; an
//!    opcode's inline operands do not run past the end of `code`; no opcode byte is
//!    decoded mid-operand.
//! 2. **Operand ranges** — every const/proto/class-proto/import index is in range;
//!    a name-const index additionally names a `Str`; every local-slot index is
//!    `< slot_count`; every upvalue index is `< upvalues.len()`.
//! 3. **Jump targets** — every relative jump lands ON an instruction boundary and
//!    within `[0, code.len()]`. A jump into the middle of an instruction, or out of
//!    bounds, is rejected.
//! 4. **Stack-depth balance** — an abstract interpretation over the control-flow
//!    graph proves the operand stack never underflows and that every join point is
//!    reached at a single, consistent depth. This is the core safety property: the
//!    VM's `fiber.pop()`/`fiber.peek()` assume a non-empty stack, so an underflow
//!    in untrusted bytecode would otherwise be UB.
//! 5. **Recursion** — every nested [`FnProto`]'s chunk is verified the same way.
//!
//! The stack-effect model ([`stack_effect`]) is the SINGLE source of truth for each
//! op's net push/pop. Data-dependent ops (`CALL`, `NEW_ARRAY`, `TEMPLATE`, …) read
//! their `argc`/`n` operand to compute an exact effect; the few genuinely
//! dynamic-arity ops (`CALL_SPREAD`, `CALL_METHOD_SPREAD`) consume a single runtime
//! args *array* the builder ops already balanced, so their effect IS static. See
//! the per-op notes in [`stack_effect`] for how each was derived from the run loop.

use crate::vm::chunk::Chunk;
use crate::vm::opcode::Op;

/// A structural defect found while verifying a [`Chunk`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// A code byte at `offset` was decoded as an opcode but is not a valid
    /// [`Op`] discriminant.
    BadOpcode { offset: usize, byte: u8 },
    /// An opcode at `offset` declares inline operands that run past the end of the
    /// code stream.
    OperandTruncated { offset: usize },
    /// An operand index is out of range for the table it indexes. `kind` names the
    /// table (`"const"`, `"proto"`, `"class_proto"`, `"import"`, `"name-const"`,
    /// `"slot"`, `"upvalue"`); `index` is the offending value; `len` is the bound.
    OperandOutOfRange {
        offset: usize,
        kind: &'static str,
        index: usize,
        len: usize,
    },
    /// A name-const operand (e.g. `GET_PROP`, `GET_GLOBAL`) indexes a constant that
    /// is not a `Value::Str`.
    NameConstNotString { offset: usize, index: usize },
    /// A jump's computed absolute target is outside `[0, code.len()]`.
    JumpOutOfBounds {
        offset: usize,
        target: isize,
        code_len: usize,
    },
    /// A jump's target lands inside an instruction (not on an opcode boundary).
    JumpIntoInstruction { offset: usize, target: usize },
    /// An op at `offset` would pop more values than the abstract stack holds.
    StackUnderflow { offset: usize },
    /// A join point (`offset`) is reachable at two different abstract stack depths.
    StackJoinMismatch {
        offset: usize,
        expected: isize,
        found: isize,
    },
    /// Control flow can fall off the end of the code stream (no terminating
    /// `RETURN`/`JUMP`/… at the tail of a reachable path).
    FallsOffEnd { offset: usize },
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::BadOpcode { offset, byte } => {
                write!(f, "invalid opcode byte {byte} at offset {offset}")
            }
            VerifyError::OperandTruncated { offset } => {
                write!(
                    f,
                    "operand of opcode at offset {offset} runs past end of code"
                )
            }
            VerifyError::OperandOutOfRange {
                offset,
                kind,
                index,
                len,
            } => write!(
                f,
                "{kind} index {index} out of range (len {len}) at offset {offset}"
            ),
            VerifyError::NameConstNotString { offset, index } => write!(
                f,
                "name-const index {index} at offset {offset} is not a string constant"
            ),
            VerifyError::JumpOutOfBounds {
                offset,
                target,
                code_len,
            } => write!(
                f,
                "jump at offset {offset} targets {target}, outside [0, {code_len}]"
            ),
            VerifyError::JumpIntoInstruction { offset, target } => write!(
                f,
                "jump at offset {offset} targets {target}, not an instruction boundary"
            ),
            VerifyError::StackUnderflow { offset } => {
                write!(f, "stack underflow at offset {offset}")
            }
            VerifyError::StackJoinMismatch {
                offset,
                expected,
                found,
            } => write!(
                f,
                "stack depth mismatch at join offset {offset}: {expected} vs {found}"
            ),
            VerifyError::FallsOffEnd { offset } => {
                write!(
                    f,
                    "control flow falls off the end of code (last op at {offset})"
                )
            }
        }
    }
}

impl std::error::Error for VerifyError {}

/// The net change an op makes to the operand-stack depth (`pushes - pops`), and
/// the minimum depth it requires BEFORE executing (the number of values it pops).
///
/// Both are computed from the run loop's actual behavior (`src/vm/run.rs`). For a
/// data-dependent op the caller passes its decoded operand(s) so the effect is
/// exact. The two SPREAD-call ops are dynamic-arity at runtime but consume a single
/// runtime args *array* (the array/spread builder ops already balanced the stack to
/// `[..callee/recv, argsArray]`), so their static effect is well-defined here.
struct Effect {
    /// Values popped before the op runs (the minimum required stack depth).
    pops: usize,
    /// Values pushed after popping.
    pushes: usize,
}

impl Effect {
    fn new(pops: usize, pushes: usize) -> Self {
        Effect { pops, pushes }
    }
    /// Net stack delta `pushes - pops`.
    fn net(&self) -> isize {
        self.pushes as isize - self.pops as isize
    }
}

/// Whether an op terminates its straight-line path (the run loop never falls
/// through to the next instruction after it).
fn is_unconditional_terminator(op: Op) -> bool {
    matches!(
        op,
        Op::Return | Op::Jump | Op::Loop | Op::Yield | Op::MatchNoArm | Op::ImmutableError
    )
}

/// The stack effect of `op` given its already-decoded operands.
///
/// `argc_or_n` carries the relevant inline count operand for the data-dependent ops
/// (`Call` argc, `CallMethod` argc, `NewArray`/`NewObject`/`Template` n); it is
/// ignored for ops whose effect is static.
///
/// Derivation notes for the non-obvious / data-dependent ops:
/// - `Call(argc)`: pops the callee + `argc` args, pushes 1 result → net `-argc`.
/// - `CallMethod(_, argc)`: pops recv + `argc` args, pushes 1 → net `-(argc+1)`.
/// - `CallSpread` / `CallMethodSpread`: the args arrived as ONE runtime array
///   (`[..callee/recv, argsArray]`); the op pops that array + the callee/recv and
///   pushes 1 result → net `-1` (static, NOT dynamic — the builder ops balanced the
///   element pushes into the single array).
/// - `NewArray(n)`: pops `n` elems, pushes 1. `NewObject(n)`: pops `2n` (k/v pairs),
///   pushes 1. `Template(n)`: pops `n` parts, pushes 1.
/// - `Class`: pops `n_defaults + n_methods` closures (+1 superclass if `has_super`),
///   pushes the class — handled specially by the caller (it needs the class-proto),
///   so it is NOT decoded here.
/// - Builder mutators (`Spread`/`AppendArray`/`AppendObject`/`SpreadObject`): pop the
///   element/entry, leave the builder container in place → net `-1` (or `-2` for the
///   key+value `AppendObject`).
/// - Peek-only guards (`CheckNumbers`/`CheckArrayDestructure`/`CheckObjectDestructure`
///   /`GetIter`): require depth but pop nothing → net 0.
/// - `IterNext`: pops the iterable, pushes `value` + `done` → net `+1`.
/// - `Propagate`: pops the pair; on the no-error path pushes the value (net 0); the
///   error path returns from the frame (handled as a possible terminator by the
///   caller). Its straight-line net is 0.
fn stack_effect(op: Op, argc_or_n: usize) -> Effect {
    use Op::*;
    match op {
        // ---- pushes 1, pops 0 ----
        Const | Nil | True | False | GetLocal | GetLocalCell | GetUpvalue | GetGlobal | Closure => {
            Effect::new(0, 1)
        }

        // GET_SUPER reads slot 0 (self) but does not POP it; pushes the bound method.
        GetSuper => Effect::new(0, 1),

        // ---- pure pops ----
        Pop => Effect::new(1, 0),
        // SET_LOCAL/SET_LOCAL_CELL/SET_UPVALUE pop the value (clean discipline).
        SetLocal | SetLocalCell | SetUpvalue => Effect::new(1, 0),

        // ---- peek/dup ----
        Dup => Effect::new(1, 2),
        Swap => Effect::new(2, 2),
        Rot3 => Effect::new(3, 3),

        // ---- binary arithmetic / comparison / range / bitwise / shift / wrapping ----
        Add | Sub | Mul | Div | Mod | Pow | Lt | Le | Gt | Ge | Eq | Ne | Range
        | RangeInclusive | BitAnd | BitOr | BitXor | Shl | Shr | WrapAdd | WrapSub
        | WrapMul => Effect::new(2, 1),
        // ---- unary ----
        Neg | Not | BitNot => Effect::new(1, 1),

        // ---- jumps ----
        // JUMP/LOOP are unconditional, no stack effect. JUMP_IF_* pop the tested value.
        Jump | Loop => Effect::new(0, 0),
        JumpIfFalse | JumpIfTrue | JumpIfNotNil => Effect::new(1, 0),
        // JUMP_IF_ARG_SUPPLIED consults the frame's argc only; no stack effect.
        // CHECK_PARAM peeks TOS (the default value) and validates it in place.
        JumpIfArgSupplied => Effect::new(0, 0),
        CheckParam => Effect::new(1, 1),

        // ---- calls ----
        Call => Effect::new(argc_or_n + 1, 1),
        CallMethod => Effect::new(argc_or_n + 1, 1),
        // dynamic arity collapsed to a single args array on the stack (see note).
        CallSpread | CallMethodSpread => Effect::new(2, 1),
        Return => Effect::new(1, 0),

        // ---- collections / builders ----
        NewArray => Effect::new(argc_or_n, 1),
        NewObject => Effect::new(2 * argc_or_n, 1),
        // `#{…}` builder: NEW_MAP pushes an empty map (pops 0); MAP_ENTRY pops the
        // map+key+value and pushes the map back (like APPEND_OBJECT).
        NewMap => Effect::new(0, 1),
        MapEntry => Effect::new(3, 1),
        Spread | SpreadArgs | AppendArray | SpreadObject => Effect::new(2, 1),
        AppendObject => Effect::new(3, 1),
        GetIndex => Effect::new(2, 1),
        SetIndex => Effect::new(3, 1),
        GetProp | GetPropOpt => Effect::new(1, 1),
        SetProp => Effect::new(2, 1),

        // ---- strings ----
        Template => Effect::new(argc_or_n, 1),

        // ---- AScript-specific ----
        Await => Effect::new(1, 1),
        Yield => Effect::new(1, 0),
        Propagate => Effect::new(1, 1),
        Unwrap => Effect::new(1, 1),
        Import => Effect::new(0, 0),
        // DEFINE_EXPORT pops the exported value and records it in the module map.
        DefineExport => Effect::new(1, 0),

        // ---- iteration ----
        // GET_ITER peeks (validates) the iterable, leaves it in place.
        GetIter => Effect::new(1, 1),
        IterNext => Effect::new(1, 2),
        IterClose => Effect::new(1, 0),
        IterSnapshot => Effect::new(1, 1),
        ArrayLen => Effect::new(1, 1),

        // ---- cells ----
        FreshCell => Effect::new(0, 0),
        // CLOSE_UPVALUE closes a cell by slot — no stack effect.
        CloseUpvalue => Effect::new(0, 0),

        // ---- for-range bounds guard (peek-only) ----
        CheckNumbers => Effect::new(2, 2),
        // ---- stepped ranges ----
        // RANGE_STEP_VALUE pops lo/hi/step, pushes the materialized array.
        RangeStepValue => Effect::new(3, 1),
        // RANGE_RESOLVE_STEP pops lo/hi/step, pushes lo/hi/resolved_step (validates).
        RangeResolveStep => Effect::new(3, 3),
        // RANGE_HAS_NEXT pops i/hi/step, pushes the continue-bool.
        RangeHasNext => Effect::new(3, 1),

        // ---- destructuring (peek-only guards leave src in place) ----
        CheckArrayDestructure | CheckObjectDestructure => Effect::new(1, 1),
        ArrayElem | ObjectKey | ArrayRest | ObjectRest => Effect::new(1, 1),

        // ---- match tests (subject -- bool) ----
        MatchArray | MatchObject | MatchHasKey => Effect::new(1, 1),
        // `subject lo hi step -- ok` — pops 4 (step always present as a value,
        // a `nil` placeholder when the pattern has no `step` clause).
        MatchRange => Effect::new(4, 1),
        MatchNoArm => Effect::new(0, 0),
        // IMMUTABLE_ERROR always diverges (raises a Tier-2 panic); like MATCH_NO_ARM
        // it never produces a value, so it has no net stack effect.
        ImmutableError => Effect::new(0, 0),

        // DEFINE_GLOBAL pops the value and binds it as a module-scope user-global.
        DefineGlobal => Effect::new(1, 0),
        // SET_GLOBAL stores TOS into an existing user-global but LEAVES it on the
        // stack (an assignment is an expression yielding the assigned value), exactly
        // like SET_LOCAL.
        SetGlobal => Effect::new(0, 0),
        // METHOD attaches TOS closure onto the class below it, leaving the class.
        Method => Effect::new(2, 1),
        // INSTANCE_OF: inst cls -- bool.
        InstanceOf => Effect::new(2, 1),
        // MAKE_GENERATOR wraps the current frame; no operand-stack change.
        MakeGenerator => Effect::new(0, 0),

        // ---- handled specially by the caller (operand-dependent on a side table) ----
        // CLASS pops a variable number of closures (defaults + methods + super);
        // the depth walk computes its exact effect from the class-proto, so this
        // placeholder is never consulted for CLASS.
        Class => Effect::new(0, 1),
    }
}

/// Verify a [`Chunk`] (and, recursively, every nested [`FnProto`]'s chunk).
///
/// Returns `Ok(())` iff the chunk is structurally safe to run. Every chunk the
/// compiler emits passes; only malformed/corrupt bytecode is rejected.
pub fn verify(chunk: &Chunk) -> Result<(), VerifyError> {
    verify_chunk(chunk)?;
    for proto in &chunk.protos {
        verify(&proto.chunk)?;
    }
    Ok(())
}

/// Verify a single chunk's own code stream (does NOT recurse into protos — see
/// [`verify`]).
fn verify_chunk(chunk: &Chunk) -> Result<(), VerifyError> {
    let code = &chunk.code;
    // An empty body is trivially valid (e.g. a class template chunk).
    if code.is_empty() {
        return Ok(());
    }

    // ---- Pass 1: decode every instruction, record boundaries + operand ranges +
    //              jump targets. `boundaries[off]` is true iff an opcode begins at
    //              `off`. We also collect the decoded ops for pass 2.
    let mut boundaries = vec![false; code.len() + 1];
    boundaries[code.len()] = true; // one-past-the-end is a valid jump target.

    // (offset, op, operand_at) for each decoded instruction, in offset order.
    let mut instrs: Vec<(usize, Op, usize)> = Vec::new();

    let mut off = 0;
    while off < code.len() {
        let byte = code[off];
        let op = Op::from_u8(byte).ok_or(VerifyError::BadOpcode { offset: off, byte })?;
        let width = op.operand_width();
        let operand_at = off + 1;
        if operand_at + width > code.len() {
            return Err(VerifyError::OperandTruncated { offset: off });
        }
        boundaries[off] = true;
        instrs.push((off, op, operand_at));
        off += 1 + width;
    }

    // ---- Pass 2: per-instruction operand-range checks + jump-target validation.
    for &(off, op, operand_at) in &instrs {
        check_operands(chunk, off, op, operand_at, &boundaries)?;
    }

    // ---- Pass 3: abstract stack-depth interpretation over the CFG. ----
    verify_stack_balance(chunk, &instrs, &boundaries)
}

/// Validate the operand(s) of one decoded instruction: const/proto/import/slot/
/// upvalue index ranges, name-const string-ness, and jump-target landing.
fn check_operands(
    chunk: &Chunk,
    off: usize,
    op: Op,
    operand_at: usize,
    boundaries: &[bool],
) -> Result<(), VerifyError> {
    use Op::*;

    // Helper closures over the chunk's tables.
    let check_const = |idx: usize| -> Result<(), VerifyError> {
        if idx >= chunk.consts.len() {
            return Err(VerifyError::OperandOutOfRange {
                offset: off,
                kind: "const",
                index: idx,
                len: chunk.consts.len(),
            });
        }
        Ok(())
    };
    let check_name_const = |idx: usize| -> Result<(), VerifyError> {
        check_const(idx)?;
        if !matches!(chunk.consts[idx], crate::value::Value::Str(_)) {
            return Err(VerifyError::NameConstNotString {
                offset: off,
                index: idx,
            });
        }
        Ok(())
    };
    let check_slot = |idx: usize| -> Result<(), VerifyError> {
        if idx >= chunk.slot_count as usize {
            return Err(VerifyError::OperandOutOfRange {
                offset: off,
                kind: "slot",
                index: idx,
                len: chunk.slot_count as usize,
            });
        }
        Ok(())
    };
    let check_upvalue = |idx: usize| -> Result<(), VerifyError> {
        if idx >= chunk.upvalues.len() {
            return Err(VerifyError::OperandOutOfRange {
                offset: off,
                kind: "upvalue",
                index: idx,
                len: chunk.upvalues.len(),
            });
        }
        Ok(())
    };

    match op {
        // ---- plain const-pool index ----
        Const => check_const(chunk.read_u16(operand_at) as usize)?,

        // ---- name-const (must be a Str) ----
        GetGlobal | DefineGlobal | SetGlobal | ImmutableError | GetProp | SetProp | GetPropOpt
        | Method | GetSuper | ObjectKey | MatchHasKey | CallMethodSpread | DefineExport => {
            check_name_const(chunk.read_u16(operand_at) as usize)?
        }

        // ---- CLASS: u16 class-proto-table index ----
        Class => {
            let idx = chunk.read_u16(operand_at) as usize;
            if idx >= chunk.class_protos.len() {
                return Err(VerifyError::OperandOutOfRange {
                    offset: off,
                    kind: "class_proto",
                    index: idx,
                    len: chunk.class_protos.len(),
                });
            }
        }

        // ---- TEMPLATE: u16 count operand, no table ----
        Template => { /* count operand; no table */ }

        // ---- OBJECT_REST: u16 const index naming the bound-keys ARRAY (not a Str) ----
        ObjectRest => check_const(chunk.read_u16(operand_at) as usize)?,

        // ---- local slot index ----
        GetLocal | SetLocal | GetLocalCell | SetLocalCell | FreshCell | CloseUpvalue => {
            check_slot(chunk.read_u16(operand_at) as usize)?
        }

        // ---- upvalue index ----
        GetUpvalue | SetUpvalue => check_upvalue(chunk.read_u16(operand_at) as usize)?,

        // ---- proto-table index ----
        Closure => {
            let idx = chunk.read_u16(operand_at) as usize;
            if idx >= chunk.protos.len() {
                return Err(VerifyError::OperandOutOfRange {
                    offset: off,
                    kind: "proto",
                    index: idx,
                    len: chunk.protos.len(),
                });
            }
        }

        // ---- import-table index ----
        Import => {
            let idx = chunk.read_u16(operand_at) as usize;
            if idx >= chunk.imports.len() {
                return Err(VerifyError::OperandOutOfRange {
                    offset: off,
                    kind: "import",
                    index: idx,
                    len: chunk.imports.len(),
                });
            }
        }

        // ---- CALL_METHOD: u16 name-const + u8 argc ----
        CallMethod => check_name_const(chunk.read_u16(operand_at) as usize)?,

        // ---- destructuring element/rest ops: u16 plain const index (ARRAY_*) or
        //      a key array (OBJECT_REST handled above as name-const? no — it is an
        //      Array const). ARRAY_ELEM/ARRAY_REST carry a numeric index/start, not
        //      a const index, so nothing to range-check. ----
        ArrayElem | ArrayRest => { /* numeric operand; no table */ }

        // ---- jump ops: validate the computed target ----
        Jump | JumpIfFalse | JumpIfTrue | JumpIfNotNil | Loop => {
            let disp = chunk.read_i16(operand_at) as isize;
            // The run loop computes the target as (ip-after-operand) + disp, where
            // ip-after-operand == operand_at + 2 (jump operands are 2 bytes).
            let base = (operand_at + 2) as isize;
            let target = base + disp;
            if target < 0 || target > chunk.code.len() as isize {
                return Err(VerifyError::JumpOutOfBounds {
                    offset: off,
                    target,
                    code_len: chunk.code.len(),
                });
            }
            let t = target as usize;
            if !boundaries[t] {
                return Err(VerifyError::JumpIntoInstruction {
                    offset: off,
                    target: t,
                });
            }
        }

        // ---- MATCH_ARRAY: u16 len + u8 exact (counts, no table) ----
        MatchArray => { /* count operands */ }

        // ---- JUMP_IF_ARG_SUPPLIED: u16 param-index + i16 forward jump ----
        JumpIfArgSupplied => {
            // The param index is a runtime guard against `frame.argc`; its bound
            // is the callee's own param list, checked at run time. Validate the
            // forward jump target lands on a boundary (the disp is the 2nd operand).
            let disp = chunk.read_i16(operand_at + 2) as isize;
            let base = (operand_at + 4) as isize;
            let target = base + disp;
            if target < 0 || target > chunk.code.len() as isize {
                return Err(VerifyError::JumpOutOfBounds {
                    offset: off,
                    target,
                    code_len: chunk.code.len(),
                });
            }
            let t = target as usize;
            if !boundaries[t] {
                return Err(VerifyError::JumpIntoInstruction {
                    offset: off,
                    target: t,
                });
            }
        }

        // ---- ops with no operand or a count operand needing no table check ----
        _ => {}
    }

    Ok(())
}

/// Abstract-interpret the operand-stack depth across the chunk's control-flow
/// graph: prove no op underflows and every reachable offset has a single,
/// consistent entry depth. A worklist over instruction offsets propagates the
/// post-op depth to each successor (fall-through + jump target).
fn verify_stack_balance(
    chunk: &Chunk,
    instrs: &[(usize, Op, usize)],
    boundaries: &[bool],
) -> Result<(), VerifyError> {
    use std::collections::HashMap;

    // Map each opcode offset to its index in `instrs` for O(1) successor lookup.
    let mut idx_of: HashMap<usize, usize> = HashMap::with_capacity(instrs.len());
    for (i, &(off, _, _)) in instrs.iter().enumerate() {
        idx_of.insert(off, i);
    }

    // The proven entry depth at each instruction offset (None = not yet reached).
    let mut entry: Vec<Option<isize>> = vec![None; instrs.len()];

    // Worklist of (instr index, entry depth). Entry point: offset 0, depth 0.
    let mut work: Vec<(usize, isize)> = vec![(0, 0)];

    // Resolve a code offset to an instruction index. A successor offset is always a
    // boundary (pass 1/2 guarantee it for jumps; fall-through advances by op width).
    let resolve = |target: usize| -> Option<usize> { idx_of.get(&target).copied() };

    while let Some((i, depth_in)) = work.pop() {
        // Join: if we have a recorded entry depth, it must match.
        if let Some(prev) = entry[i] {
            if prev != depth_in {
                return Err(VerifyError::StackJoinMismatch {
                    offset: instrs[i].0,
                    expected: prev,
                    found: depth_in,
                });
            }
            continue; // already processed at this depth.
        }
        entry[i] = Some(depth_in);

        let (off, op, operand_at) = instrs[i];

        // Decode the count operand for data-dependent ops + the Class special case.
        let depth_out = if op == Op::Class {
            // CLASS pops n_defaults + n_methods (+1 superclass), pushes the class.
            let cp_idx = chunk.read_u16(operand_at) as usize;
            // Pass 2 already range-checked the class-proto index.
            let cp = &chunk.class_protos[cp_idx];
            let pops = cp.default_fields.len() + cp.method_names.len() + usize::from(cp.has_super);
            let after = depth_in - pops as isize;
            if after < 0 {
                return Err(VerifyError::StackUnderflow { offset: off });
            }
            after + 1
        } else {
            let n = count_operand(chunk, op, operand_at);
            let eff = stack_effect(op, n);
            if depth_in < eff.pops as isize {
                return Err(VerifyError::StackUnderflow { offset: off });
            }
            depth_in + eff.net()
        };

        // Propagate to successors.
        let next_off = next_offset(chunk, off, op);

        match op {
            // Unconditional jumps: only the target is a successor.
            Op::Jump | Op::Loop => {
                let target = jump_target(chunk, operand_at);
                push_succ(&mut work, &resolve, target, depth_out, boundaries, off)?;
            }
            // Conditional jumps: both the target and the fall-through.
            Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNotNil => {
                let target = jump_target(chunk, operand_at);
                push_succ(&mut work, &resolve, target, depth_out, boundaries, off)?;
                push_fallthrough(&mut work, &resolve, next_off, depth_out, off)?;
            }
            // JUMP_IF_ARG_SUPPLIED is a conditional FORWARD jump whose i16 offset
            // is the SECOND operand (after the u16 param-index).
            Op::JumpIfArgSupplied => {
                let target = jump_target(chunk, operand_at + 2);
                push_succ(&mut work, &resolve, target, depth_out, boundaries, off)?;
                push_fallthrough(&mut work, &resolve, next_off, depth_out, off)?;
            }
            // Frame terminators: no in-chunk successor.
            Op::Return | Op::Yield | Op::MatchNoArm | Op::ImmutableError => {}
            // PROPAGATE may return from the frame OR fall through; the fall-through
            // path is the one with a stack effect we track (net 0). The return path
            // leaves the chunk, so only the fall-through is an in-chunk successor.
            Op::Propagate => {
                push_fallthrough(&mut work, &resolve, next_off, depth_out, off)?;
            }
            // Everything else falls through.
            _ => {
                push_fallthrough(&mut work, &resolve, next_off, depth_out, off)?;
            }
        }
    }

    // A reachable op that is NOT an unconditional terminator and whose fall-through
    // is one-past-the-end means control can run off the end of code. The compiler
    // always ends a chunk with RETURN, so this guards corrupt bytecode.
    if let Some(&(last_off, last_op, _)) = instrs.last() {
        let last_idx = instrs.len() - 1;
        if entry[last_idx].is_some()
            && !is_unconditional_terminator(last_op)
            && next_offset(chunk, last_off, last_op) == chunk.code.len()
        {
            return Err(VerifyError::FallsOffEnd { offset: last_off });
        }
    }

    Ok(())
}

/// The byte offset of the instruction immediately following the op at `off`.
fn next_offset(_chunk: &Chunk, off: usize, op: Op) -> usize {
    off + 1 + op.operand_width()
}

/// The absolute jump target for a jump op whose 2-byte operand starts at
/// `operand_at` (mirrors the run loop's `ip-after-operand + disp`).
fn jump_target(chunk: &Chunk, operand_at: usize) -> usize {
    let disp = chunk.read_i16(operand_at) as isize;
    ((operand_at + 2) as isize + disp) as usize
}

/// Decode the count/argc operand for a data-dependent op, or 0 if the op's effect
/// is static.
fn count_operand(chunk: &Chunk, op: Op, operand_at: usize) -> usize {
    match op {
        Op::Call => chunk.read_u8(operand_at) as usize,
        // CALL_METHOD: u16 name then u8 argc.
        Op::CallMethod => chunk.read_u8(operand_at + 2) as usize,
        Op::NewArray | Op::NewObject | Op::Template => chunk.read_u16(operand_at) as usize,
        _ => 0,
    }
}

/// The NET operand-stack delta (`pushes - pops`) of the instruction `op` at
/// `operand_at` in `chunk`, decoding its inline count operand and handling the
/// `Op::Class` special case (whose pop count comes from its class-proto). This is the
/// SAME authoritative table [`verify_stack_balance`] uses; it is exposed so other
/// passes (e.g. the worker code-slice builder, which tracks top-level stack depth to
/// bound a computed-`const` initializer to its own statement) reuse one source of
/// truth rather than re-deriving stack effects.
pub(crate) fn op_stack_delta(chunk: &Chunk, op: Op, operand_at: usize) -> isize {
    if op == Op::Class {
        // CLASS pops n_defaults + n_methods (+1 superclass), pushes the class.
        if let Some(cp) = chunk.class_protos.get(chunk.read_u16(operand_at) as usize) {
            let pops = cp.default_fields.len() + cp.method_names.len() + usize::from(cp.has_super);
            return 1 - pops as isize;
        }
        return 0;
    }
    stack_effect(op, count_operand(chunk, op, operand_at)).net()
}

/// Push a jump-target successor onto the worklist, validating it lands on a
/// boundary (defence-in-depth; pass 2 already checked jumps).
fn push_succ(
    work: &mut Vec<(usize, isize)>,
    resolve: &dyn Fn(usize) -> Option<usize>,
    target: usize,
    depth: isize,
    boundaries: &[bool],
    off: usize,
) -> Result<(), VerifyError> {
    if target >= boundaries.len() || !boundaries[target] {
        return Err(VerifyError::JumpIntoInstruction {
            offset: off,
            target,
        });
    }
    // One-past-the-end is a valid boundary but has no instruction; a jump there is
    // an implicit fall-off, rejected as out-of-bounds-into-nothing.
    match resolve(target) {
        Some(i) => work.push((i, depth)),
        None => {
            // target == code.len(): jumping to the end with no terminator there.
            return Err(VerifyError::JumpOutOfBounds {
                offset: off,
                target: target as isize,
                code_len: target,
            });
        }
    }
    Ok(())
}

/// Push a fall-through successor onto the worklist.
fn push_fallthrough(
    work: &mut Vec<(usize, isize)>,
    resolve: &dyn Fn(usize) -> Option<usize>,
    next_off: usize,
    depth: isize,
    off: usize,
) -> Result<(), VerifyError> {
    match resolve(next_off) {
        Some(i) => work.push((i, depth)),
        None => {
            // Falling through to one-past-the-end → run-off-end (caught by the tail
            // check too, but be explicit if it is mid-stream).
            return Err(VerifyError::FallsOffEnd { offset: off });
        }
    }
    Ok(())
}

/// A standalone helper for V12-T4: deserialize and then verify in one step. A valid
/// `.aso` decodes AND verifies; a corrupt one fails at whichever check trips first.
impl Chunk {
    /// Deserialize an `.aso` byte stream ([`Chunk::from_bytes`]) and then run the
    /// [bytecode verifier](verify) over it. Returns the chunk only if BOTH succeed.
    /// This is the load path V12-T4 will use so corrupt bytecode is never run.
    pub fn from_bytes_verified(bytes: &[u8]) -> Result<Chunk, FromBytesVerifiedError> {
        let chunk = Chunk::from_bytes(bytes).map_err(FromBytesVerifiedError::Decode)?;
        verify(&chunk).map_err(FromBytesVerifiedError::Verify)?;
        Ok(chunk)
    }
}

/// The error from [`Chunk::from_bytes_verified`]: either deserialization or
/// verification failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromBytesVerifiedError {
    /// The byte stream failed to deserialize.
    Decode(crate::vm::aso::AsoError),
    /// The chunk deserialized but failed verification.
    Verify(VerifyError),
}

impl std::fmt::Display for FromBytesVerifiedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FromBytesVerifiedError::Decode(e) => write!(f, "{e}"),
            FromBytesVerifiedError::Verify(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FromBytesVerifiedError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::Span;
    use crate::value::Value;
    use crate::vm::chunk::Chunk;
    use crate::vm::opcode::Op;

    fn compile(src: &str) -> Chunk {
        crate::compile::compile_source(src)
            .unwrap_or_else(|e| panic!("compile failed: {} @ {:?}", e.message, e.span))
    }

    fn s() -> Span {
        Span::new(0, 0)
    }

    /// A spread of programs exercising every compiler feature; every one must
    /// VERIFY OK (the compiler emits valid bytecode).
    const PROGRAMS: &[&str] = &[
        "print(1 + 2 * 3)",
        "let a = 1\nlet b = \"hi\"\nprint(a)\nprint(b)",
        "let x = [1, 2, 3]\nprint(x[0])",
        "let o = {a: 1, b: 2}\nprint(o.a)",
        "if (1 < 2) { print(\"y\") } else { print(\"n\") }",
        "let t = 0\nwhile (t < 5) { t = t + 1 }\nprint(t)",
        "for (i in 0..3) { print(i) }",
        "for (x of [10, 20]) { print(x) }",
        "fn add(a, b) { return a + b }\nprint(add(2, 3))",
        "let mk = (n) => (v) => v + n\nlet f = mk(10)\nprint(f(5))",
        "fn outer() { let c = 0\nlet inc = () => { c = c + 1\nreturn c }\nreturn inc() }\nprint(outer())",
        "import { max } from \"std/math\"\nprint(max(1, 2))",
        "import * as math from \"std/math\"\nprint(math.abs(-3))",
        "enum Color { Red = 1, Green = 2 }\nprint(Color.Red.value)",
        "class P { x: number = 0\nfn get(): number { return self.x } }\nlet p = P()\nprint(p.get())",
        "let [a, b, ...rest] = [1, 2, 3, 4]\nprint(a)\nprint(rest)",
        "let {x, y as z} = {x: 1, y: 2}\nprint(x)\nprint(z)",
        "let xs = [1, 2]\nlet ys = [0, ...xs, 3]\nprint(ys)",
        "fn sum(...ns: array<number>) { let t = 0\nfor (n of ns) { t = t + n }\nreturn t }\nprint(sum(1, 2, 3))",
        "let v = 5\nlet r = match v { 1 => \"one\", 1..10 => \"small\", _ => \"big\" }\nprint(r)",
        "let p = {kind: \"a\", n: 1}\nlet r = match p { {kind, n} => n, _ => 0 }\nprint(r)",
        "let c = 1 == 1 ? \"eq\" : \"ne\"\nprint(c)",
        "let a = nil\nlet b = a ?? 7\nprint(b)",
        "let s = `value is ${1 + 2}`\nprint(s)",
    ];

    #[test]
    fn all_compiler_output_verifies_ok() {
        for &src in PROGRAMS {
            let chunk = compile(src);
            verify(&chunk).unwrap_or_else(|e| panic!("verify failed for {src:?}: {e}"));
        }
    }

    #[test]
    fn roundtrip_aso_verifies_ok() {
        for &src in PROGRAMS {
            let chunk = compile(src);
            let bytes = chunk.to_bytes().expect("serialize");
            Chunk::from_bytes_verified(&bytes)
                .unwrap_or_else(|e| panic!("from_bytes_verified failed for {src:?}: {e}"));
        }
    }

    #[test]
    fn empty_chunk_verifies() {
        let c = Chunk::new();
        assert_eq!(verify(&c), Ok(()));
    }

    // ---- malformed-chunk rejection ----

    #[test]
    fn bad_opcode_byte_rejected() {
        let mut c = Chunk::new();
        c.code.push(0xFE); // not a valid opcode
        c.code.push(Op::Return as u8);
        match verify(&c) {
            Err(VerifyError::BadOpcode { offset, byte }) => {
                assert_eq!((offset, byte), (0, 0xFE));
            }
            other => panic!("expected BadOpcode, got {other:?}"),
        }
    }

    #[test]
    fn operand_truncated_rejected() {
        let mut c = Chunk::new();
        // CONST needs a 2-byte operand but the stream ends right after the opcode.
        c.code.push(Op::Const as u8);
        c.code.push(0x00); // only 1 of 2 operand bytes
        assert!(matches!(
            verify(&c),
            Err(VerifyError::OperandTruncated { offset: 0 })
        ));
    }

    #[test]
    fn const_index_out_of_range_rejected() {
        let mut c = Chunk::new();
        c.add_const(Value::Float(1.0)); // index 0 is the only valid one
        c.emit_u16(Op::Const, 5, s()); // index 5 is out of range
        c.emit(Op::Return, s());
        match verify(&c) {
            Err(VerifyError::OperandOutOfRange {
                kind, index, len, ..
            }) => {
                assert_eq!((kind, index, len), ("const", 5, 1));
            }
            other => panic!("expected OperandOutOfRange, got {other:?}"),
        }
    }

    #[test]
    fn name_const_not_string_rejected() {
        let mut c = Chunk::new();
        let n = c.add_const(Value::Float(1.0)); // a number, not a Str
        c.emit_u16(Op::GetGlobal, n, s());
        c.emit(Op::Return, s());
        assert!(matches!(
            verify(&c),
            Err(VerifyError::NameConstNotString { .. })
        ));
    }

    #[test]
    fn slot_out_of_range_rejected() {
        let mut c = Chunk::new();
        c.slot_count = 1;
        c.emit_u16(Op::GetLocal, 5, s()); // slot 5 >= slot_count 1
        c.emit(Op::Return, s());
        match verify(&c) {
            Err(VerifyError::OperandOutOfRange { kind, .. }) => assert_eq!(kind, "slot"),
            other => panic!("expected slot OperandOutOfRange, got {other:?}"),
        }
    }

    #[test]
    fn jump_out_of_bounds_rejected() {
        let mut c = Chunk::new();
        c.emit_jump(Op::Jump, s()); // op at 0, operand at 1
        c.emit(Op::Return, s());
        // Patch the displacement to point way past code.len().
        let huge: i16 = 1000;
        c.code[1..3].copy_from_slice(&huge.to_le_bytes());
        match verify(&c) {
            Err(VerifyError::JumpOutOfBounds { .. }) => {}
            other => panic!("expected JumpOutOfBounds, got {other:?}"),
        }
    }

    #[test]
    fn jump_into_instruction_rejected() {
        let mut c = Chunk::new();
        // CONST (3 bytes) then JUMP whose target lands in the middle of the CONST.
        let k = c.add_const(Value::Float(1.0));
        c.emit_u16(Op::Const, k, s()); // bytes 0,1,2
        let site = c.emit_jump(Op::Jump, s()); // op at 3, operand at 4
        c.emit(Op::Return, s());
        // Make the jump land at offset 1 (mid-CONST). after-operand = site+2 = 6.
        let disp = 1i64 - (site as i64 + 2);
        c.code[site..site + 2].copy_from_slice(&(disp as i16).to_le_bytes());
        match verify(&c) {
            Err(VerifyError::JumpIntoInstruction { target, .. }) => assert_eq!(target, 1),
            other => panic!("expected JumpIntoInstruction, got {other:?}"),
        }
    }

    #[test]
    fn stack_underflow_rejected() {
        let mut c = Chunk::new();
        c.emit(Op::Pop, s()); // POP on an empty stack
        c.emit(Op::Return, s());
        match verify(&c) {
            Err(VerifyError::StackUnderflow { offset }) => assert_eq!(offset, 0),
            other => panic!("expected StackUnderflow, got {other:?}"),
        }
    }

    #[test]
    fn stack_join_mismatch_rejected() {
        // Build: NIL ; JUMP_IF_FALSE +k (pops the NIL) ; NIL ; <join> RETURN
        // The two edges into the join have different depths.
        let mut c = Chunk::new();
        c.emit(Op::Nil, s()); // off 0  depth 0 -> 1
        let site = c.emit_jump(Op::JumpIfFalse, s()); // off 1, pops -> depth 0; operand at 2
        c.emit(Op::Nil, s()); // off 4  depth 0 -> 1
        c.patch_jump(site); // target = current len (5) = the RETURN below
        c.emit(Op::Return, s()); // off 5: fall-through arrives depth 1; jump arrives 0
        match verify(&c) {
            Err(VerifyError::StackJoinMismatch { .. }) => {}
            other => panic!("expected StackJoinMismatch, got {other:?}"),
        }
    }

    #[test]
    fn falls_off_end_rejected() {
        let mut c = Chunk::new();
        c.emit(Op::Nil, s()); // a non-terminator at the tail
        match verify(&c) {
            Err(VerifyError::FallsOffEnd { offset }) => assert_eq!(offset, 0),
            other => panic!("expected FallsOffEnd, got {other:?}"),
        }
    }

    #[test]
    fn corrupted_aso_rejected_after_roundtrip() {
        // Round-trip a real program, corrupt a byte in the code region, and confirm
        // from_bytes_verified rejects it (either decode or verify fails).
        let chunk = compile("fn f(a) { return a + 1 }\nprint(f(41))");
        let mut bytes = chunk.to_bytes().expect("serialize");
        // The code length prefix is right after magic(4) + version(4) = offset 8.
        let code_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        assert!(code_len > 0);
        // Corrupt the FIRST code byte to an invalid opcode (0xFE). Code starts at 12.
        bytes[12] = 0xFE;
        match Chunk::from_bytes_verified(&bytes) {
            Err(FromBytesVerifiedError::Verify(VerifyError::BadOpcode { .. })) => {}
            // Depending on layout the corruption may surface as a decode error of a
            // nested table; either way it must NOT succeed.
            Err(_) => {}
            Ok(_) => panic!("corrupted .aso must not verify"),
        }
    }

    #[test]
    fn nested_proto_verified_recursively() {
        // A program with a nested fn; corrupt the nested proto's code post-hoc and
        // confirm verify recurses and rejects it.
        let mut chunk = compile("fn f(a) { return a + 1 }\nprint(f(1))");
        // Valid as-is.
        assert_eq!(verify(&chunk), Ok(()));
        // Reach into the first proto's chunk and inject a bad opcode.
        let proto = std::rc::Rc::get_mut(&mut chunk.protos[0]).expect("unique proto ref");
        proto.chunk.code[0] = 0xFD;
        assert!(matches!(verify(&chunk), Err(VerifyError::BadOpcode { .. })));
    }
}
