//! The bytecode instruction set for the AScript register-of-stack VM.
//!
//! Each [`Op`] is a single opcode byte. Inline operands (if any) follow the
//! opcode byte in the chunk's `code` stream, little-endian. The number of inline
//! operand bytes is given by [`Op::operand_width`]; the disassembler and the VM
//! decode loop use it to advance the instruction pointer.
//!
//! Operand encodings used below:
//! - `u16` — a 2-byte little-endian index (into the const pool, proto table, or
//!   a local/upvalue slot).
//! - `u8` — a single byte (call argument count).
//! - `i16` — a signed 2-byte little-endian jump displacement, measured from the
//!   byte *after* the operand to the jump target.
//!
//! Inline caches: a handful of ops are *specializable* (see
//! [`Op::has_inline_cache`]). Their per-site cache state is NOT stored inline in
//! the code stream; V11 will keep a parallel `Vec<InlineCache>` indexed by a slot
//! reserved at compile time. Therefore [`Op::operand_width`] reflects only the
//! real inline operands and never reserves space for IC bytes.

/// A single bytecode instruction.
///
/// `#[repr(u8)]` so the discriminant is exactly the opcode byte written into a
/// [`crate::vm::chunk::Chunk`]'s `code` vector. Exec arms for these ops are
/// implemented incrementally across VM plan slices V2–V10; until then several
/// variants are constructed only via [`Op::from_u8`] / `op as u8`.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    // ---- stack / constants ------------------------------------------------
    /// `CONST(u16)` — push `consts[idx]`.
    Const,
    /// Push `nil`.
    Nil,
    /// Push `true`.
    True,
    /// Push `false`.
    False,
    /// Pop and discard the top of stack.
    Pop,
    /// Duplicate the top of stack.
    Dup,

    // ---- locals / upvalues ------------------------------------------------
    /// `GET_LOCAL(u16)` — push the value in local slot `n`.
    GetLocal,
    /// `SET_LOCAL(u16)` — store TOS into local slot `n` (leaves TOS in place).
    SetLocal,
    /// `GET_UPVALUE(u16)` — push the captured upvalue `n`.
    GetUpvalue,
    /// `SET_UPVALUE(u16)` — store TOS into captured upvalue `n`.
    SetUpvalue,
    /// `CLOSE_UPVALUE(u16)` — close (hoist to the heap) the upvalue cell for
    /// local slot `n` as it leaves scope.
    CloseUpvalue,

    // ---- globals ----------------------------------------------------------
    /// `GET_GLOBAL(u16)` — push the global named by `consts[idx]` (a `Str`).
    /// The VM's globals are the bare builtins (`crate::interp::BUILTIN_NAMES`);
    /// the result is the corresponding `Value::Builtin`.
    GetGlobal,
    /// `SET_GLOBAL(u16)` — store TOS into the global named by `consts[idx]`.
    ///
    /// **Currently unused — never emitted by the compiler, never executed.**
    /// AScript has no writable user globals: a top-level `let`/`const` binds a
    /// *frame-local* of the SourceFile frame (so `x = ...` at top level lowers to
    /// `SET_LOCAL`, handled by the locals slice), and the only true globals — the
    /// bare builtins — are *immutable* (the tree-walker rejects `print = 5` with
    /// "cannot assign to immutable binding 'print'", so the compiler never reaches
    /// an assignment whose target resolves to a builtin global). The opcode stays
    /// declared (it was reserved in V1 and keeps the byte layout stable for the
    /// disassembler / future host-injected mutable globals), but `Vm::run` has no
    /// arm for it; if one were ever emitted it would hit the "not yet implemented"
    /// guard rather than silently mis-store.
    SetGlobal,

    // ---- arithmetic / logic ----------------------------------------------
    /// `a b -- (a + b)`.
    Add,
    /// `a b -- (a - b)`.
    Sub,
    /// `a b -- (a * b)`.
    Mul,
    /// `a b -- (a / b)`.
    Div,
    /// `a b -- (a % b)`.
    Mod,
    /// `a b -- (a ** b)`.
    Pow,
    /// `a -- (-a)`.
    Neg,
    /// `a -- (!a)`.
    Not,
    /// `a b -- (a == b)`.
    Eq,
    /// `a b -- (a != b)`.
    Ne,
    /// `a b -- (a < b)`.
    Lt,
    /// `a b -- (a <= b)`.
    Le,
    /// `a b -- (a > b)`.
    Gt,
    /// `a b -- (a >= b)`.
    Ge,
    /// `a b -- [a, a+1, .. b)` — eager half-open `array<number>` (step 1). Mirrors
    /// the tree-walker's `BinOp::Range`; both bounds must be `Number`.
    Range,

    // ---- control flow -----------------------------------------------------
    /// `JUMP(i16)` — unconditional relative jump.
    Jump,
    /// `JUMP_IF_FALSE(i16)` — pop TOS; jump if falsy.
    JumpIfFalse,
    /// `JUMP_IF_TRUE(i16)` — pop TOS; jump if truthy.
    JumpIfTrue,
    /// `LOOP(i16)` — unconditional backward relative jump (negative displacement).
    Loop,

    // ---- calls / returns --------------------------------------------------
    /// `CALL(u8)` — call with `argc` arguments already on the stack above the
    /// callee.
    Call,
    /// Return TOS from the current frame.
    Return,
    /// `CLOSURE(u16)` — build a closure from `protos[idx]`, capturing upvalues.
    Closure,

    // ---- collections ------------------------------------------------------
    /// `NEW_ARRAY(u16)` — pop `n` elements, push a new array.
    NewArray,
    /// `NEW_OBJECT(u16)` — pop `n` key/value pairs, push a new object.
    NewObject,
    /// `... v --` — spread `v` into the array/object/call being built.
    Spread,
    /// `obj key -- obj[key]`.
    GetIndex,
    /// `obj key val -- val` — store `obj[key] = val`.
    SetIndex,
    /// `GET_PROP(u16)` — `obj -- obj.<name>` (name = `consts[idx]`).
    GetProp,
    /// `SET_PROP(u16)` — `obj val -- val` — store `obj.<name> = val`.
    SetProp,
    /// `GET_PROP_OPT(u16)` — `obj -- obj?.<name>` (nil-short-circuiting).
    GetPropOpt,

    // ---- classes / enums --------------------------------------------------
    /// `CLASS(u16)` — push a new class named by `consts[idx]`.
    Class,
    /// `METHOD(u16)` — attach TOS (a closure) as method `consts[idx]` on the
    /// class below it.
    Method,
    /// `GET_SUPER(u16)` — resolve super-method `consts[idx]`.
    GetSuper,
    /// `inst cls -- bool` — `inst instanceof cls`.
    InstanceOf,

    // ---- strings ----------------------------------------------------------
    /// `TEMPLATE(u16)` — pop `n` parts, concatenate into a string.
    Template,

    // ---- AScript-specific -------------------------------------------------
    /// Await the future on TOS.
    Await,
    /// Yield TOS from the current generator.
    Yield,
    /// Wrap the current frame as a generator object.
    MakeGenerator,
    /// `?` — propagate a `[value, err]` Result pair (early-return on error).
    Propagate,
    /// `!` — force-unwrap a `[value, err]` pair (recoverable panic on error).
    Unwrap,
    /// `IMPORT(u16)` — import the module named by `consts[idx]`.
    Import,

    // ---- iteration --------------------------------------------------------
    /// `iterable -- iterator` — obtain an iterator for `for`/`for await`.
    GetIter,
    /// `iterator -- iterator value done` — advance the iterator.
    IterNext,
}

impl Op {
    /// Decode an opcode byte. Returns `None` if `b` is not a valid discriminant.
    ///
    /// This match is exhaustive over every [`Op`] variant; adding a variant
    /// without a corresponding arm is a compile error.
    pub fn from_u8(b: u8) -> Option<Op> {
        use Op::*;
        // The wildcard arm only catches bytes outside the declared range; the
        // listed arms are exhaustive over the enum (compiler-verified by the
        // round-trip unit test).
        Some(match b {
            x if x == Const as u8 => Const,
            x if x == Nil as u8 => Nil,
            x if x == True as u8 => True,
            x if x == False as u8 => False,
            x if x == Pop as u8 => Pop,
            x if x == Dup as u8 => Dup,

            x if x == GetLocal as u8 => GetLocal,
            x if x == SetLocal as u8 => SetLocal,
            x if x == GetUpvalue as u8 => GetUpvalue,
            x if x == SetUpvalue as u8 => SetUpvalue,
            x if x == CloseUpvalue as u8 => CloseUpvalue,

            x if x == GetGlobal as u8 => GetGlobal,
            x if x == SetGlobal as u8 => SetGlobal,

            x if x == Add as u8 => Add,
            x if x == Sub as u8 => Sub,
            x if x == Mul as u8 => Mul,
            x if x == Div as u8 => Div,
            x if x == Mod as u8 => Mod,
            x if x == Pow as u8 => Pow,
            x if x == Neg as u8 => Neg,
            x if x == Not as u8 => Not,
            x if x == Eq as u8 => Eq,
            x if x == Ne as u8 => Ne,
            x if x == Lt as u8 => Lt,
            x if x == Le as u8 => Le,
            x if x == Gt as u8 => Gt,
            x if x == Ge as u8 => Ge,
            x if x == Range as u8 => Range,

            x if x == Jump as u8 => Jump,
            x if x == JumpIfFalse as u8 => JumpIfFalse,
            x if x == JumpIfTrue as u8 => JumpIfTrue,
            x if x == Loop as u8 => Loop,

            x if x == Call as u8 => Call,
            x if x == Return as u8 => Return,
            x if x == Closure as u8 => Closure,

            x if x == NewArray as u8 => NewArray,
            x if x == NewObject as u8 => NewObject,
            x if x == Spread as u8 => Spread,
            x if x == GetIndex as u8 => GetIndex,
            x if x == SetIndex as u8 => SetIndex,
            x if x == GetProp as u8 => GetProp,
            x if x == SetProp as u8 => SetProp,
            x if x == GetPropOpt as u8 => GetPropOpt,

            x if x == Class as u8 => Class,
            x if x == Method as u8 => Method,
            x if x == GetSuper as u8 => GetSuper,
            x if x == InstanceOf as u8 => InstanceOf,

            x if x == Template as u8 => Template,

            x if x == Await as u8 => Await,
            x if x == Yield as u8 => Yield,
            x if x == MakeGenerator as u8 => MakeGenerator,
            x if x == Propagate as u8 => Propagate,
            x if x == Unwrap as u8 => Unwrap,
            x if x == Import as u8 => Import,

            x if x == GetIter as u8 => GetIter,
            x if x == IterNext as u8 => IterNext,

            _ => return None,
        })
    }

    /// Number of inline operand bytes that follow this opcode byte in the code
    /// stream. Does NOT include any inline cache slot (see [`Op::has_inline_cache`]).
    pub fn operand_width(self) -> usize {
        use Op::*;
        match self {
            // u16-operand ops.
            Const | GetLocal | SetLocal | GetUpvalue | SetUpvalue | CloseUpvalue | GetGlobal
            | SetGlobal | Closure | NewArray | NewObject | GetProp | SetProp | GetPropOpt
            | Class | Method | GetSuper | Template | Import => 2,

            // i16-operand (jump) ops.
            Jump | JumpIfFalse | JumpIfTrue | Loop => 2,

            // u8-operand ops.
            Call => 1,

            // Zero-operand ops.
            Nil | True | False | Pop | Dup | Add | Sub | Mul | Div | Mod | Pow | Neg | Not
            | Eq | Ne | Lt | Le | Gt | Ge | Range | Return | Spread | GetIndex | SetIndex
            | InstanceOf | Await | Yield | MakeGenerator | Propagate | Unwrap | GetIter
            | IterNext => 0,
        }
    }

    /// Whether this op is *specializable* via an inline cache.
    ///
    /// The cache state lives in a separate parallel array (built in V11), indexed
    /// by a slot reserved per call site at compile time — it is NOT encoded in the
    /// code stream, so [`Op::operand_width`] is unaffected. This predicate only
    /// marks which ops participate.
    pub fn has_inline_cache(self) -> bool {
        use Op::*;
        matches!(self, Add | GetGlobal | GetProp | SetProp | Call)
    }
}

#[cfg(test)]
mod tests {
    use super::Op;

    /// Every declared opcode, in one place, so the round-trip test is exhaustive.
    /// If you add an `Op` variant, add it here too (and the `from_u8` arm).
    const ALL: &[Op] = &[
        Op::Const,
        Op::Nil,
        Op::True,
        Op::False,
        Op::Pop,
        Op::Dup,
        Op::GetLocal,
        Op::SetLocal,
        Op::GetUpvalue,
        Op::SetUpvalue,
        Op::CloseUpvalue,
        Op::GetGlobal,
        Op::SetGlobal,
        Op::Add,
        Op::Sub,
        Op::Mul,
        Op::Div,
        Op::Mod,
        Op::Pow,
        Op::Neg,
        Op::Not,
        Op::Eq,
        Op::Ne,
        Op::Lt,
        Op::Le,
        Op::Gt,
        Op::Ge,
        Op::Range,
        Op::Jump,
        Op::JumpIfFalse,
        Op::JumpIfTrue,
        Op::Loop,
        Op::Call,
        Op::Return,
        Op::Closure,
        Op::NewArray,
        Op::NewObject,
        Op::Spread,
        Op::GetIndex,
        Op::SetIndex,
        Op::GetProp,
        Op::SetProp,
        Op::GetPropOpt,
        Op::Class,
        Op::Method,
        Op::GetSuper,
        Op::InstanceOf,
        Op::Template,
        Op::Await,
        Op::Yield,
        Op::MakeGenerator,
        Op::Propagate,
        Op::Unwrap,
        Op::Import,
        Op::GetIter,
        Op::IterNext,
    ];

    #[test]
    fn from_u8_round_trips_every_variant() {
        for &op in ALL {
            assert_eq!(Op::from_u8(op as u8), Some(op), "round-trip failed for {op:?}");
        }
    }

    #[test]
    fn discriminants_are_unique_and_dense() {
        // ALL must list each variant exactly once: discriminants 0..ALL.len().
        for (i, &op) in ALL.iter().enumerate() {
            assert_eq!(op as usize, i, "discriminant gap/dup at {op:?}");
        }
    }

    #[test]
    fn from_u8_rejects_out_of_range() {
        assert_eq!(Op::from_u8(ALL.len() as u8), None);
        assert_eq!(Op::from_u8(u8::MAX), None);
    }

    #[test]
    fn operand_width_for_representative_ops() {
        // u16 operands.
        assert_eq!(Op::Const.operand_width(), 2);
        assert_eq!(Op::GetLocal.operand_width(), 2);
        assert_eq!(Op::GetGlobal.operand_width(), 2);
        assert_eq!(Op::Closure.operand_width(), 2);
        assert_eq!(Op::GetProp.operand_width(), 2);
        // i16 jump operands.
        assert_eq!(Op::Jump.operand_width(), 2);
        assert_eq!(Op::JumpIfFalse.operand_width(), 2);
        assert_eq!(Op::Loop.operand_width(), 2);
        // u8 operand.
        assert_eq!(Op::Call.operand_width(), 1);
        // zero-operand ops.
        assert_eq!(Op::Nil.operand_width(), 0);
        assert_eq!(Op::Add.operand_width(), 0);
        assert_eq!(Op::Return.operand_width(), 0);
        assert_eq!(Op::GetIndex.operand_width(), 0);
        assert_eq!(Op::Await.operand_width(), 0);
    }

    #[test]
    fn inline_cache_marks_only_specializable_ops() {
        assert!(Op::Add.has_inline_cache());
        assert!(Op::GetGlobal.has_inline_cache());
        assert!(Op::GetProp.has_inline_cache());
        assert!(Op::SetProp.has_inline_cache());
        assert!(Op::Call.has_inline_cache());
        // A sampling of non-specializable ops.
        assert!(!Op::Const.has_inline_cache());
        assert!(!Op::Sub.has_inline_cache());
        assert!(!Op::GetIndex.has_inline_cache());
        assert!(!Op::Jump.has_inline_cache());
    }
}
