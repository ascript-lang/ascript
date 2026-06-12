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
    /// An `Op::DefineInterface` references an `InterfaceProto` whose method-requirement
    /// set is malformed (an empty or duplicate requirement name). `what` describes it.
    BadInterface { offset: usize, what: &'static str },
    /// DBG: an `Op::Break` byte appears in a chunk at verification (load) time.
    /// `Break` is NEVER compiler-emitted and NEVER serialized — it exists only as a
    /// transient runtime byte patch installed by an attached debugger. Its presence in
    /// a loaded/`.aso` chunk is corruption (or a hostile hand-crafted file), so reject
    /// it rather than treat it as a silent no-op safepoint.
    BreakInSerializedCode { offset: usize },
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
            VerifyError::BadInterface { offset, what } => {
                write!(f, "{what} at offset {offset}")
            }
            VerifyError::BreakInSerializedCode { offset } => write!(
                f,
                "Op::Break at offset {offset} is not valid in stored bytecode \
                 (it is a runtime-only debugger patch)"
            ),
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
        // CHECK_LOCAL peeks TOS (the bound initializer) and validates it in place.
        CheckLocal => Effect::new(1, 1),

        // ---- calls ----
        Call => Effect::new(argc_or_n + 1, 1),
        CallMethod => Effect::new(argc_or_n + 1, 1),
        // CALL_NAMED: pops the callee + `argc` arg values, pushes 1 result.
        CallNamed => Effect::new(argc_or_n + 1, 1),
        // dynamic arity collapsed to a single args array on the stack (see note).
        CallSpread | CallMethodSpread => Effect::new(2, 1),
        // ADT spread+named lockstep builder: each APPEND_*_ARG pops one value/operand,
        // leaving the two builder arrays it just appended to (net -1). CALL_NAMED_SPREAD
        // pops the callee + args array + names array, pushes 1 result (net -2).
        // Each pops the trailing `value`/`operand`, then `peek(1)`+`peek(0)`s the two
        // builder arrays (`argsArray`, `namesArray`) it mutates in place — so the op
        // REQUIRES depth >= 3 at runtime (`fiber.peek(1)` would panic below that),
        // even though it only consumes one value (net -1, leaving the two arrays).
        // `pops` doubles as the minimum-depth check, so it must be 3 (not 1); push the
        // two arrays back → `pushes` 2 keeps the net at -1.
        AppendNamedArg | AppendPosArg | AppendSpreadArg => Effect::new(3, 2),
        CallNamedSpread => Effect::new(3, 1),
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
        // IFACE: DEFINE_INTERFACE builds the descriptor and PUSHES it (the compiler
        // emits the matching DEFINE_GLOBAL/SET_LOCAL bind op after) — net +1, like CLASS.
        DefineInterface => Effect::new(0, 1),
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
        // ADT: variant destructure (subject -- value) and tag/field tests
        // (subject -- bool) — all pop the subject, push one result.
        VariantElem | VariantField | MatchVariant | MatchVariantArity
        | MatchVariantHasField => Effect::new(1, 1),

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
        // SET_GLOBAL `peek(0)`s TOS, stores it into an existing user-global, and
        // LEAVES it on the stack (an assignment is an expression yielding the assigned
        // value — UNLIKE SET_LOCAL, which pops). It is a "peek-and-keep" op: it
        // REQUIRES depth >= 1 (the runtime `fiber.peek(0)` would otherwise panic) but
        // is net-zero, so `pops` and `pushes` are both 1 (the `pops` field doubles as
        // the minimum-depth requirement; see the `Effect` doc).
        SetGlobal => Effect::new(1, 1),
        // METHOD attaches TOS closure onto the class below it, leaving the class.
        Method => Effect::new(2, 1),
        // INSTANCE_OF: inst cls -- bool.
        InstanceOf => Effect::new(2, 1),
        // INSTANCE_OF_TYPE: inst -- bool (the type name rides as a const operand).
        InstanceOfType => Effect::new(1, 1),
        // MAKE_GENERATOR wraps the current frame; no operand-stack change.
        MakeGenerator => Effect::new(0, 0),

        // ---- handled specially by the caller (operand-dependent on a side table) ----
        // CLASS pops a variable number of closures (defaults + methods + super);
        // the depth walk computes its exact effect from the class-proto, so this
        // placeholder is never consulted for CLASS.
        Class => Effect::new(0, 1),

        // DBG: the breakpoint trap is NEVER compiler-emitted and NEVER serialized — it
        // exists only as a runtime byte patch. Pass 1 of `verify_chunk` rejects a
        // `Break` byte outright (`BreakInSerializedCode`), so this arm is unreachable in
        // practice; it exists only to keep the `Op` match exhaustive (0 stack effect).
        Break => Effect::new(0, 0),
    }
}

/// Verify a [`Chunk`] (and, recursively, every nested [`FnProto`]'s chunk).
///
/// Returns `Ok(())` iff the chunk is structurally safe to run. Every chunk the
/// compiler emits passes; only malformed/corrupt bytecode is rejected.
pub fn verify(chunk: &Chunk) -> Result<(), VerifyError> {
    // The top-level (entry) chunk has no params: it runs in a synthetic 0-arity frame,
    // so any `CHECK_PARAM` here is already out of range (and the compiler never emits one
    // at top level). `verify_proto_chunk` then recurses into each nested proto with its
    // OWN param count, bounding that proto's `CHECK_PARAM` operands (they index
    // `proto.params`, which lives on the FnProto, not the chunk — so the count must be
    // threaded in explicitly at every depth).
    verify_proto_chunk(chunk, 0)
}

/// Verify a chunk (bounding `CHECK_PARAM` operands by `params_len`), then recurse into
/// every nested proto's chunk with that proto's own param count.
fn verify_proto_chunk(chunk: &Chunk, params_len: usize) -> Result<(), VerifyError> {
    verify_chunk(chunk, params_len)?;
    for proto in &chunk.protos {
        verify_proto_chunk(&proto.chunk, proto.params.len())?;
    }
    Ok(())
}

/// Verify a single chunk's own code stream (does NOT recurse into protos — see
/// [`verify`]). `params_len` bounds `CHECK_PARAM` operands (the param list lives on the
/// owning [`FnProto`], so the count is threaded in by the caller).
fn verify_chunk(chunk: &Chunk, params_len: usize) -> Result<(), VerifyError> {
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
        // DBG: `Op::Break` is a runtime-only byte patch — never compiler-emitted, never
        // serialized. Encountering it at load/verify time means corrupt or hand-crafted
        // bytecode; reject rather than admit it as a silent safepoint.
        if op == Op::Break {
            return Err(VerifyError::BreakInSerializedCode { offset: off });
        }
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
        check_operands(chunk, off, op, operand_at, &boundaries, params_len)?;
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
    params_len: usize,
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
    let check_type_const = |idx: usize| -> Result<(), VerifyError> {
        if idx >= chunk.type_consts.len() {
            return Err(VerifyError::OperandOutOfRange {
                offset: off,
                kind: "type-const",
                index: idx,
                len: chunk.type_consts.len(),
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
        | Method | GetSuper | ObjectKey | MatchHasKey | CallMethodSpread | DefineExport
        | InstanceOfType | VariantField | MatchVariantHasField | AppendNamedArg => {
            check_name_const(chunk.read_u16(operand_at) as usize)?
        }

        // ADT: MATCH_VARIANT references a 2-element `[variant, enumOrNil]` Array const.
        MatchVariant => check_const(chunk.read_u16(operand_at) as usize)?,

        // ADT: VARIANT_ELEM carries a positional payload INDEX; MATCH_VARIANT_ARITY a
        // payload field COUNT. Neither indexes a chunk table, so there is no companion
        // length to range-check against AT THIS OP (the variant whose payload is indexed
        // is a *runtime* value — `VariantElem` is preceded only by the bare index, never a
        // const naming the enum/variant). Two facts make the bare operand safe regardless
        // of what a crafted `.aso` plants here:
        //
        //   1. It is decoded via `read_u16`, so it is intrinsically in `0..=u16::MAX` —
        //      there is no value the operand can hold that the decode does not already
        //      bound. (This is the compiler's own ceiling too: positional variant-pattern
        //      arity is bounded by `u16::try_from` in `src/compile/mod.rs`. A legitimately
        //      compiled chunk DOES emit operands well above any "practical" small cap — a
        //      300-field positional variant builds, verifies, and runs today, emitting
        //      `VariantElem(0..=299)` / `MatchVariantArity(300)`. So a tighter constant cap
        //      such as 255 would *over-reject* valid bytecode and is unsound.)
        //   2. The run loop is independently out-of-bounds-safe: `VariantElem` reads the
        //      payload with `.get(idx)` / `IndexMap::get_index(idx)` (→ `Value::Nil` when
        //      out of range) and `MatchVariantArity` tests `payload_len == Some(n)` — a
        //      false match, never an index panic. (The inline tests below assert a 0xFFFF
        //      operand verifies, and `run.rs` asserts the run loop returns Nil/false for
        //      an out-of-range operand on a real payload — no host panic.)
        //
        // So this arm intentionally stays a documented pass-through: the only sound bound
        // is `u16::MAX`, which `read_u16` already guarantees.
        VariantElem | MatchVariantArity => { /* numeric operand; u16-bounded by decode */ }

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

        // ---- DEFINE_INTERFACE: u16 interface-proto-table index (IFACE) ----
        DefineInterface => {
            let idx = chunk.read_u16(operand_at) as usize;
            let proto = chunk.interface_protos.get(idx).ok_or(
                VerifyError::OperandOutOfRange {
                    offset: off,
                    kind: "interface_proto",
                    index: idx,
                    len: chunk.interface_protos.len(),
                },
            )?;
            // Method-set well-formedness: every requirement name non-empty and unique
            // (a malformed `.aso` with a duplicate/blank requirement is rejected before
            // it can build a corrupt descriptor at `Op::DefineInterface`).
            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for (mname, _, _) in &proto.methods {
                if mname.is_empty() {
                    return Err(VerifyError::BadInterface {
                        offset: off,
                        what: "an interface method requirement has an empty name",
                    });
                }
                if !seen.insert(mname.as_str()) {
                    return Err(VerifyError::BadInterface {
                        offset: off,
                        what: "an interface declares a duplicate method requirement",
                    });
                }
            }
        }

        // ---- TEMPLATE: u16 count operand, no table ----
        Template => { /* count operand; no table */ }

        // ---- OBJECT_REST: u16 const index naming the bound-keys ARRAY (not a Str) ----
        ObjectRest => check_const(chunk.read_u16(operand_at) as usize)?,

        // ---- CHECK_LOCAL: u16 index into the type-const side-pool ----
        CheckLocal => check_type_const(chunk.read_u16(operand_at) as usize)?,

        // ---- CHECK_PARAM: u16 PARAM index (default-value contract check). The run loop
        //      indexes `closure.proto.params[param]` UNCONDITIONALLY (an unchecked slice
        //      index), so a crafted `CHECK_PARAM(0xFFFF)` on a proto with fewer params
        //      would panic the host. The param list lives on the owning FnProto (threaded
        //      in as `params_len`), not the chunk, so it is range-checked here. ----
        CheckParam => {
            let idx = chunk.read_u16(operand_at) as usize;
            if idx >= params_len {
                return Err(VerifyError::OperandOutOfRange {
                    offset: off,
                    kind: "param",
                    index: idx,
                    len: params_len,
                });
            }
        }

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

        // ---- CALL_NAMED: u16 names-ARRAY const + u8 argc (ADT §3.2) ----
        CallNamed => check_const(chunk.read_u16(operand_at) as usize)?,

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
        // CALL_METHOD / CALL_NAMED: u16 const index then u8 argc.
        Op::CallMethod | Op::CallNamed => chunk.read_u8(operand_at + 2) as usize,
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
    // Net is just pushes - pops over the ONE source of truth below (no second copy of
    // the `Op::Class` special case).
    let (pops, pushes) = op_stack_pops_pushes(chunk, op, operand_at);
    pushes as isize - pops as isize
}

/// The `(pops, pushes)` of the instruction `op` at `operand_at` in `chunk` — the SINGLE
/// authoritative source of stack effects ([`op_stack_delta`] just nets this), exposing
/// both halves so a forward stack SIMULATION (the tree-shaker's namespace receiver
/// tracker, which must know whether an op reaches DOWN to a specific stack slot, not
/// just the net delta) can reuse it. The `Op::Class` pop count comes from the class
/// proto (it pops `n_defaults + n_methods` closures + 1 superclass if `has_super`, and
/// always pushes the one class value); on an out-of-range proto index — unreachable on
/// VALID bytecode — we fall back to `(0, 1)`, the self-consistent "pushes one value"
/// answer.
pub(crate) fn op_stack_pops_pushes(chunk: &Chunk, op: Op, operand_at: usize) -> (usize, usize) {
    if op == Op::Class {
        if let Some(cp) = chunk.class_protos.get(chunk.read_u16(operand_at) as usize) {
            let pops = cp.default_fields.len() + cp.method_names.len() + usize::from(cp.has_super);
            return (pops, 1);
        }
        return (0, 1);
    }
    let e = stack_effect(op, count_operand(chunk, op, operand_at));
    (e.pops, e.pushes)
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

    /// Whether `target` appears as an opcode byte anywhere in `chunk.code`
    /// (decoding linearly so an operand byte equal to the opcode is not mistaken
    /// for the op). Only the top-level chunk is scanned.
    fn find_op(chunk: &Chunk, target: Op) -> bool {
        let mut ip = 0;
        while ip < chunk.code.len() {
            let Some(op) = Op::from_u8(chunk.code[ip]) else {
                return false;
            };
            if op == target {
                return true;
            }
            ip += 1 + op.operand_width();
        }
        false
    }

    /// Like [`find_op`] but recurses into every nested function proto's chunk — the
    /// op may be emitted inside a `fn` body rather than the top-level chunk.
    fn find_op_in_protos(chunk: &Chunk, target: Op) -> bool {
        if find_op(chunk, target) {
            return true;
        }
        chunk
            .protos
            .iter()
            .any(|p| find_op_in_protos(&p.chunk, target))
    }

    /// Recursively locate the FIRST `target` op anywhere in `chunk` (or any nested proto,
    /// at ANY depth) and rewrite its u16 operand to `new_operand`. Returns true iff one
    /// was found and patched. Used to corrupt a single operand for a negative test.
    fn patch_first_u16_operand(chunk: &mut Chunk, target: Op, new_operand: u16) -> bool {
        let mut ip = 0;
        while ip < chunk.code.len() {
            let Some(op) = Op::from_u8(chunk.code[ip]) else {
                break;
            };
            if op == target {
                let [lo, hi] = new_operand.to_le_bytes();
                chunk.code[ip + 1] = lo;
                chunk.code[ip + 2] = hi;
                return true;
            }
            ip += 1 + op.operand_width();
        }
        // Not in this chunk — recurse into nested protos (uniquely owned in a test chunk).
        for proto in chunk.protos.iter_mut() {
            let p = std::rc::Rc::get_mut(proto).expect("unique proto ref");
            if patch_first_u16_operand(&mut p.chunk, target, new_operand) {
                return true;
            }
        }
        false
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
    fn serialized_break_opcode_rejected() {
        // DBG: a `Break` byte must never appear in a loaded/`.aso` chunk — the compiler
        // never emits it and the serializer never writes it; it is a runtime-only patch.
        // A hand-crafted file with one must be rejected, not treated as a no-op safepoint.
        let mut c = Chunk::new();
        c.code.push(Op::Break as u8);
        c.code.push(Op::Return as u8);
        match verify(&c) {
            Err(VerifyError::BreakInSerializedCode { offset }) => assert_eq!(offset, 0),
            other => panic!("expected BreakInSerializedCode, got {other:?}"),
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
    fn set_global_on_empty_stack_rejected() {
        // SET_GLOBAL `peek(0)`s its target value at RUNTIME (it leaves TOS on the
        // stack — assignment is an expression). A crafted chunk that runs SET_GLOBAL
        // at abstract stack depth 0 would `Fiber::peek(0)`-PANIC at runtime. The
        // verifier's stack-effect for SET_GLOBAL therefore requires depth >= 1, so
        // this malformed chunk must be REJECTED (was wrongly accepted when the effect
        // was `(0, 0)`).
        let mut c = Chunk::new();
        let n = c.add_const(Value::Str("g".into())); // a valid name-const (Str)
        c.emit_u16(Op::SetGlobal, n, s()); // depth 0 here -> underflow
        c.emit(Op::Return, s());
        match verify(&c) {
            Err(VerifyError::StackUnderflow { offset }) => assert_eq!(offset, 0),
            other => panic!("expected StackUnderflow for SET_GLOBAL, got {other:?}"),
        }
    }

    #[test]
    fn append_arg_below_builder_depth_rejected() {
        // BLAST-RADIUS sibling of SET_GLOBAL: APPEND_POS_ARG (and its NAMED/SPREAD
        // kin) pop the trailing value, then `peek(1)`+`peek(0)` the two builder arrays
        // it mutates — so it REQUIRES abstract depth >= 3. A crafted chunk that runs it
        // with only one value present would `Fiber::peek(1)`-PANIC at runtime; the
        // verifier must reject it (was wrongly accepted when the effect was `(1, 0)`).
        let mut c = Chunk::new();
        c.emit(Op::Nil, s()); // depth 1 (just the "value"); the two builder arrays are absent
        c.emit(Op::AppendPosArg, s()); // requires depth 3 -> underflow at this op
        c.emit(Op::Return, s());
        match verify(&c) {
            Err(VerifyError::StackUnderflow { offset }) => assert_eq!(offset, 1),
            other => panic!("expected StackUnderflow for APPEND_POS_ARG, got {other:?}"),
        }
    }

    #[test]
    fn valid_spread_named_call_still_verifies() {
        // POSITIVE control for the APPEND_*_ARG depth fix. A call MIXING a named arg
        // with a `...spread` lowers to the lockstep builder (the only path that emits
        // APPEND_*_ARG), with the two builder arrays already on the stack — it must
        // still verify under the depth-3 requirement. The mix is a *runtime* error for
        // ADT construction, but `verify` runs on the COMPILED chunk regardless: the
        // point is only that the emitted bytecode is well-formed.
        let chunk = compile(
            "enum Shape { Rect(w: number, h: number) }\nlet extra = [2]\nlet r = Shape.Rect(w: 1, ...extra)\nprint(r)",
        );
        assert!(
            find_op(&chunk, Op::AppendNamedArg) && find_op(&chunk, Op::AppendSpreadArg),
            "expected the program to emit the APPEND_NAMED_ARG + APPEND_SPREAD_ARG builder ops"
        );
        verify(&chunk).expect("valid named+spread builder bytecode must verify");
    }

    #[test]
    fn valid_global_assignment_still_verifies() {
        // POSITIVE control: a real top-level reassignment lowers to SET_GLOBAL with the
        // RHS value already on the stack — it must still verify OK after the depth-1
        // requirement. (Also covered by `all_compiler_output_verifies_ok` via the
        // `while (t < 5) { t = t + 1 }` program, but asserted directly here too.)
        let chunk = compile("let t = 0\nt = t + 1\nprint(t)");
        // sanity: the program actually emits a SET_GLOBAL
        assert!(
            find_op(&chunk, Op::SetGlobal),
            "expected the program to emit SET_GLOBAL"
        );
        verify(&chunk).expect("valid global assignment must verify");
    }

    #[test]
    fn stack_join_mismatch_rejected() {
        // Build: NIL ; JUMP_IF_FALSE +k (pops the NIL) ; NIL ; <join> RETURN
        // The two edges into the join have different depths.
        let mut c = Chunk::new();
        c.emit(Op::Nil, s()); // off 0  depth 0 -> 1
        let site = c.emit_jump(Op::JumpIfFalse, s()); // opcode at off 1; operand (site=2) -> depth 0 after pop
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

    // ---- ADT: VARIANT_ELEM / MATCH_VARIANT_ARITY operand bounds (Task 0.7) ----

    #[test]
    fn variant_elem_max_operand_verifies() {
        // A bare `VariantElem(0xFFFF)` operand is in range (the operand is a u16, so
        // 0xFFFF is its legitimate maximum — see the `check_operands` arm). A crafted
        // chunk that plants it must VERIFY (it is well-formed bytecode), and the run
        // loop is independently out-of-bounds-safe (see `run.rs`
        // `variant_elem_oob_operand_is_nil_not_panic`). Stack-balanced: NIL pushes the
        // subject (depth 1), VARIANT_ELEM is net-zero (pop subject / push element),
        // RETURN consumes the element (RETURN needs depth >= 1).
        let mut c = Chunk::new();
        c.emit(Op::Nil, s()); // subject (depth 0 -> 1)
        c.emit_u16(Op::VariantElem, 0xFFFF, s()); // net 0 (depth stays 1)
        c.emit(Op::Return, s()); // returns TOS
        verify(&c).expect("VARIANT_ELEM(0xFFFF) is a valid u16 operand and must verify");
    }

    #[test]
    fn match_variant_arity_max_operand_verifies() {
        // Same shape for MATCH_VARIANT_ARITY (also net-zero: pop subject, push a bool).
        let mut c = Chunk::new();
        c.emit(Op::Nil, s());
        c.emit_u16(Op::MatchVariantArity, 0xFFFF, s()); // net 0 (depth stays 1)
        c.emit(Op::Return, s()); // returns the pushed bool
        verify(&c).expect("MATCH_VARIANT_ARITY(0xFFFF) is a valid u16 operand and must verify");
    }

    #[test]
    fn wide_positional_variant_pattern_verifies() {
        // REGRESSION GUARD against an unsound "practical" cap (e.g. 255): a positional
        // variant declared with > 255 fields compiles to `VariantElem`/
        // `MatchVariantArity` operands ABOVE 255, and that bytecode is legitimate — it
        // must verify. (If a future change introduces a sub-`u16::MAX` operand cap here,
        // this test fails, flagging the over-rejection of valid programs.)
        let fields = vec!["int"; 300].join(", ");
        let pats: Vec<String> = (0..300).map(|i| format!("x{i}")).collect();
        let src = format!(
            "enum Big {{ Wide({fields}) }}\n\
             fn check(v) {{ return match v {{ Big.Wide({}) => x0, _ => -1 }} }}\n\
             print(\"ok\")",
            pats.join(", ")
        );
        let chunk = compile(&src);
        // Sanity: the pattern path actually emitted the two ops we are bounding.
        assert!(
            find_op_in_protos(&chunk, Op::VariantElem),
            "expected a VARIANT_ELEM somewhere in the compiled output"
        );
        assert!(
            find_op_in_protos(&chunk, Op::MatchVariantArity),
            "expected a MATCH_VARIANT_ARITY somewhere in the compiled output"
        );
        verify(&chunk)
            .expect("a 300-field positional variant pattern is valid bytecode and must verify");
    }

    // ---- BLAST RADIUS: CHECK_PARAM operand bounds (unchecked param index → host panic).

    #[test]
    fn check_param_in_range_verifies() {
        // POSITIVE control: a typed default parameter compiles to a `CHECK_PARAM` whose
        // operand is a valid param index — the program must verify.
        let chunk = compile("fn greet(name: string = \"guest\") { print(name) }\ngreet()");
        assert!(
            find_op_in_protos(&chunk, Op::CheckParam),
            "expected a typed default param to emit CHECK_PARAM"
        );
        verify(&chunk).expect("a valid CHECK_PARAM param index must verify");
    }

    #[test]
    fn check_param_out_of_range_rejected() {
        // The run loop indexes `proto.params[param]` UNCONDITIONALLY (an unchecked slice
        // index), so a crafted `.aso` with an out-of-range `CHECK_PARAM` operand would
        // panic the host. Compile a real program, then corrupt the proto's CHECK_PARAM
        // operand to 0xFFFF (far beyond its single param) and confirm verify REJECTS it.
        let mut chunk = compile("fn greet(name: string = \"guest\") { print(name) }\ngreet()");
        // Valid as compiled.
        assert_eq!(verify(&chunk), Ok(()));
        assert!(
            patch_first_u16_operand(&mut chunk, Op::CheckParam, 0xFFFF),
            "expected to find a CHECK_PARAM op to corrupt"
        );
        match verify(&chunk) {
            Err(VerifyError::OperandOutOfRange { kind: "param", index, .. }) => {
                assert_eq!(index, 0xFFFF);
            }
            other => panic!("expected OperandOutOfRange(param), got {other:?}"),
        }
    }

    #[test]
    fn check_param_out_of_range_rejected_in_nested_proto() {
        // The `CHECK_PARAM` bound must be enforced at EVERY proto depth, not just depth 1
        // — `verify_proto_chunk` recurses with each proto's own param count. Here the
        // defaulted-param function is defined INSIDE another function (a depth-2 proto),
        // so the corrupted operand lives two protos deep. `patch_first_u16_operand`
        // recurses to find it; verify must still reject.
        let mut chunk = compile(
            "fn outer() {\n\
            \x20 fn inner(name: string = \"guest\") { print(name) }\n\
            \x20 inner()\n\
            }\n\
            outer()",
        );
        assert_eq!(verify(&chunk), Ok(()));
        // Sanity: the CHECK_PARAM is genuinely nested (not in the top chunk).
        assert!(
            !find_op(&chunk, Op::CheckParam),
            "the CHECK_PARAM should be inside a nested proto, not the top chunk"
        );
        assert!(
            find_op_in_protos(&chunk, Op::CheckParam),
            "expected the nested defaulted param to emit CHECK_PARAM"
        );
        assert!(
            patch_first_u16_operand(&mut chunk, Op::CheckParam, 0xFFFF),
            "expected to find a nested CHECK_PARAM op to corrupt"
        );
        match verify(&chunk) {
            Err(VerifyError::OperandOutOfRange { kind: "param", index, .. }) => {
                assert_eq!(index, 0xFFFF);
            }
            other => panic!("expected OperandOutOfRange(param) from a nested proto, got {other:?}"),
        }
    }
}
