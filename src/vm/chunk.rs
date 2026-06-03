//! [`Chunk`] — a compiled unit of bytecode plus the side tables the VM needs to
//! run and to report diagnostics: a constant pool, nested function prototypes, an
//! upvalue capture plan, and a code-offset → [`Span`] table.
//!
//! Spans are recorded one entry per emitted instruction, keyed by the opcode
//! byte's offset. Because emission is strictly monotonic (offsets only grow), the
//! `spans` vector is naturally sorted ascending and [`Chunk::span_at`] can binary
//! search it (nearest-preceding lookup).

use crate::span::Span;
use crate::syntax::resolve::types::UpvalueDescriptor;
use crate::value::Value;
use crate::vm::adapt::{ArithCache, GlobalCache};
use crate::vm::ic::{InlineCache, MethodCache};
use crate::vm::opcode::Op;
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;

/// A trivial pass-through [`Hasher`] for the offset-keyed VM side maps
/// (`field_ics`/`method_ics`/`arith_caches`/`global_caches`).
///
/// V11-T6 PERF: those maps are consulted on EVERY `GET_PROP`/`SET_PROP`/
/// `CALL_METHOD`/arithmetic/`GET_GLOBAL` op, keyed by the op's bytecode OFFSET — a
/// dense, distinct, already-well-distributed `usize`. The default `HashMap` hashes
/// it with SipHash, which costs more than the `f64` add behind a specialized
/// arithmetic site, leaving a few-percent overhead on tight monomorphic numeric
/// loops vs the kill-switch-off (generic) path. Hashing the offset to ITSELF
/// removes that overhead while keeping the byte-identical offset-keyed side-map
/// design (no bytecode change, no new dependency). Single-key inserts/lookups, so
/// collisions are impossible (each offset is unique within one chunk).
#[derive(Default)]
pub struct OffsetHasher(u64);

impl Hasher for OffsetHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        // The maps are only ever keyed by `usize` (a bytecode offset), whose
        // `Hash` impl issues one `write_usize`. This byte path is never taken in
        // practice; fold defensively so a stray key still hashes deterministically.
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(b);
        }
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.0 = i as u64;
    }
}

/// A `HashMap<usize, V>` keyed by bytecode offset, hashed by [`OffsetHasher`].
type OffsetMap<V> = HashMap<usize, V, BuildHasherDefault<OffsetHasher>>;

/// A compiled class definition referenced by an `Op::Class` operand.
///
/// The compiler builds the [`crate::value::Class`] (name, superclass, field
/// schemas) at compile time, but the class's METHODS are compiled to
/// [`FnProto`]s and dispatched as VM `Value::Closure`s — which [`Value::Class`]
/// cannot hold (its `methods` map is the tree-walker `Rc<Method>` shape, frozen).
/// So `Op::Class` carries the prebuilt `Rc<Class>` plus the *names* of its
/// methods (in declaration order); at runtime the matching method closures are
/// already on the stack (one `Op::Closure` per method emitted just before
/// `Op::Class`), and the VM registers them in its per-class compiled-method side
/// table keyed by the class's `Rc` identity (see `Vm::register_class_methods`).
/// Field DEFAULTS are likewise compiled to zero-arg thunk closures (one per
/// defaulted field), pushed before the method closures, and run at construct time.
pub struct ClassProto {
    /// The prebuilt class value (name, superclass, field schemas). Its `methods`
    /// map is left EMPTY — compiled methods live in the VM side table.
    pub class: Rc<crate::value::Class>,
    /// The defaulted-field names, in declaration order, paired with the stack
    /// position of their default thunk closures (pushed first, before methods).
    pub default_fields: Vec<String>,
    /// The method names, in declaration order, matching the method closures
    /// pushed immediately before `Op::Class` (after the default thunks).
    pub method_names: Vec<String>,
    /// Whether this class has an `extends` clause (V9-T2). When true, the compiler
    /// emits the superclass class-value expression FIRST (below the default thunks
    /// and method closures); `Op::Class` pops it and builds a fresh `Rc<Class>`
    /// whose `superclass` is that value. The prebuilt `class` field above is then a
    /// TEMPLATE (its `superclass` is `None`) — only its name/field-schemas are used.
    pub has_super: bool,
}

impl std::fmt::Debug for ClassProto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClassProto")
            .field("class", &self.class.name)
            .field("default_fields", &self.default_fields)
            .field("method_names", &self.method_names)
            .finish()
    }
}

/// What an `Op::Import` should bind. The `Op::Import(u16)` operand indexes the
/// chunk's `imports` side table to one of these descriptors (the source string +
/// the binding plan), keeping the const pool clean and the instruction stream a
/// single u16-wide op. Mirrors the tree-walker's `ast::ImportNames`.
///
/// Each `(name, slot)` records an imported name (or the namespace alias) and the
/// resolver-assigned local slot it binds into. The compiler resolves the slots
/// from the resolver's `BindingKind::Import` bindings (matched by import-stmt
/// `decl_range` + name), exactly as `let` resolves its slot.
#[derive(Debug, Clone)]
pub enum ImportDesc {
    /// `import { a, b } from "std/x"` — bind each named export into its slot.
    Named {
        source: String,
        /// `(export name, local slot, is_cell)` per imported name, in source order.
        names: Vec<(String, u16, bool)>,
    },
    /// `import * as alias from "std/x"` — bind the namespace Object into `slot`.
    Namespace {
        source: String,
        slot: u16,
        is_cell: bool,
    },
}

impl ImportDesc {
    /// The module source string (`"std/…"`), for diagnostics / disassembly.
    pub fn source(&self) -> &str {
        match self {
            ImportDesc::Named { source, .. } | ImportDesc::Namespace { source, .. } => source,
        }
    }
}

/// A compiled function body (or top-level script body) plus its metadata.
#[derive(Debug, Default)]
pub struct Chunk {
    /// The raw instruction stream: opcode bytes interleaved with LE operands.
    pub code: Vec<u8>,
    /// The constant pool. Holds only literal [`Value`]s
    /// (`Number`/`Str`/`Bool`/`Nil`/`Decimal`).
    pub consts: Vec<Value>,
    /// Nested function prototypes referenced by `CLOSURE` operands.
    pub protos: Vec<Rc<FnProto>>,
    /// Compiled class definitions referenced by `CLASS` operands (V9).
    pub class_protos: Vec<Rc<ClassProto>>,
    /// `(code offset, span)` pairs, one per instruction, sorted ascending by
    /// offset (emission is monotonic).
    pub spans: Vec<(usize, Span)>,
    /// The upvalue capture plan for the closure this chunk belongs to: each entry
    /// says where the closure pulls a captured variable from. Indexed by upvalue
    /// number (matching the resolver's `Resolution::Upvalue(idx)`).
    pub upvalues: Vec<UpvalueDescriptor>,
    /// Local slots that are heap *cells* (`Rc<RefCell<Value>>`) rather than plain
    /// stack slots — the resolver's `cell_slots` for this frame (every captured
    /// local). A cell is allocated nil at frame entry and accessed via
    /// `GET_LOCAL_CELL`/`SET_LOCAL_CELL`, so a closure capturing it by reference
    /// observes mutation. (Late-binding-correct baseline; V5 optimizes.)
    pub cell_slots: Vec<u32>,
    /// Number of local slots this frame needs (stack window size).
    pub slot_count: u16,
    /// Number of reserved inline-cache slots (V11). Counted by the compiler at
    /// each `GET_PROP`/`SET_PROP`/`CALL_METHOD` site; kept as a metric/sanity
    /// bound. The caches themselves are keyed by the op's bytecode offset (see
    /// [`Chunk::field_ic`]/[`Chunk::method_ic`]) rather than a dense slot index,
    /// so adding ICs leaves the bytecode and disassembly BYTE-IDENTICAL (no new
    /// inline operand). The maps below start empty (cold) and fill on first run.
    pub ic_count: u16,
    /// Field-read/write inline caches (`GET_PROP`/`SET_PROP`), keyed by the op's
    /// bytecode offset within `code`. Lazily populated; a missing entry is `Cold`.
    /// `RefCell` because the run loop mutates a cache through a `&self` chunk
    /// borrow (the VM runs on `&self`); never shared across threads (`!Send`).
    pub field_ics: RefCell<OffsetMap<InlineCache>>,
    /// Method-dispatch inline caches (`CALL_METHOD`), keyed by the op's bytecode
    /// offset. Lazily populated; a missing entry is `Cold`.
    pub method_ics: RefCell<OffsetMap<MethodCache>>,
    /// PEP-659 adaptive arithmetic state (`ADD`/`SUB`/`MUL`/…), keyed by the op's
    /// bytecode offset (V11-T4). A site warms up then specializes to a guarded
    /// `ADD_NUMBER`/`ADD_DECIMAL`/`CONCAT_STR`-style fast path; a guard miss
    /// deopts. Lazily populated; a missing entry is the default (generic, warmup
    /// 0). Same offset-keyed side-map pattern as the inline caches → bytecode and
    /// disassembly stay byte-identical.
    pub arith_caches: RefCell<OffsetMap<ArithCache>>,
    /// PEP-659 adaptive global-resolution cache (`GET_GLOBAL`), keyed by the op's
    /// bytecode offset (V11-T4). Caches the resolved builtin value guarded by the
    /// global-table version. Lazily populated; a missing entry is `Cold`.
    pub global_caches: RefCell<OffsetMap<GlobalCache>>,
    /// Import descriptors referenced by `IMPORT` operands (V12). Each `Op::Import`
    /// carries a u16 index into this table; the run loop reads the descriptor to
    /// resolve the `std/*` module and bind its exports into the recorded slots.
    pub imports: Vec<ImportDesc>,
    /// Optional name (function name / `<script>`), for the disassembler & traces.
    pub name: Option<String>,
}

/// A compiled function prototype: a [`Chunk`] plus the calling-convention flags
/// the VM needs to set up a frame.
#[derive(Debug)]
pub struct FnProto {
    pub chunk: Chunk,
    pub arity: u8,
    pub has_rest: bool,
    pub is_async: bool,
    pub is_generator: bool,
    /// The parameter list in declaration order (including a trailing rest param),
    /// carrying each param's name, declared type contract, and `rest` flag. The VM
    /// CALL feeds this straight into [`crate::interp::check_call_args`] — the SAME
    /// arity/contract/rest checker the tree-walker uses — so the two engines bind
    /// arguments and panic byte-identically. Built from the function's CST param
    /// nodes by the compiler.
    pub params: Vec<crate::ast::Param>,
    /// The declared return-type contract (`fn f(): T`), if any. Checked against the
    /// returned value at RETURN, panicking exactly as the tree-walker's `run_body`.
    pub ret: Option<crate::ast::Type>,
}

impl Chunk {
    /// A fresh, empty chunk.
    pub fn new() -> Self {
        Chunk::default()
    }

    // ---- emission ---------------------------------------------------------

    /// Emit a zero-operand op, recording its span at the opcode byte's offset.
    pub fn emit(&mut self, op: Op, span: Span) {
        self.record_span(span);
        self.code.push(op as u8);
    }

    /// Emit an op with a `u16` little-endian operand.
    pub fn emit_u16(&mut self, op: Op, operand: u16, span: Span) {
        self.record_span(span);
        self.code.push(op as u8);
        self.code.extend_from_slice(&operand.to_le_bytes());
    }

    /// Emit an op with a single `u8` operand (e.g. `CALL` argc).
    pub fn emit_u8(&mut self, op: Op, operand: u8, span: Span) {
        self.record_span(span);
        self.code.push(op as u8);
        self.code.push(operand);
    }

    /// Emit an op with a `u16` little-endian operand followed by a `u8` operand
    /// (e.g. `CALL_METHOD name, argc`). The `u16` comes first, then the `u8`.
    pub fn emit_u16_u8(&mut self, op: Op, a: u16, b: u8, span: Span) {
        self.record_span(span);
        self.code.push(op as u8);
        self.code.extend_from_slice(&a.to_le_bytes());
        self.code.push(b);
    }

    /// Emit a jump op with a placeholder `i16` displacement of `0`. Returns the
    /// offset of the operand bytes (the *patch site*) for a later
    /// [`Chunk::patch_jump`].
    pub fn emit_jump(&mut self, op: Op, span: Span) -> usize {
        self.record_span(span);
        self.code.push(op as u8);
        let site = self.code.len();
        self.code.extend_from_slice(&0i16.to_le_bytes());
        site
    }

    /// Backpatch the `i16` placeholder at `site` so the jump lands at the current
    /// end of `code`. The displacement is measured from the byte *after* the
    /// operand (`site + 2`) to the target.
    ///
    /// # Panics
    /// If the forward distance does not fit in an `i16`.
    pub fn patch_jump(&mut self, site: usize) {
        let target = self.code.len();
        let from = site + 2;
        let disp = i64::try_from(target).unwrap() - i64::try_from(from).unwrap();
        let disp = i16::try_from(disp)
            .unwrap_or_else(|_| panic!("jump displacement {disp} out of i16 range"));
        self.code[site..site + 2].copy_from_slice(&disp.to_le_bytes());
    }

    /// Emit a backward (loop) jump whose displacement lands at `target`. The
    /// displacement is measured from the byte after the operand to `target`, so it
    /// is negative for a real backward jump.
    ///
    /// # Panics
    /// If the backward distance does not fit in an `i16`.
    pub fn emit_loop(&mut self, op: Op, target: usize, span: Span) {
        self.record_span(span);
        self.code.push(op as u8);
        let from = self.code.len() + 2;
        let disp = i64::try_from(target).unwrap() - i64::try_from(from).unwrap();
        let disp = i16::try_from(disp)
            .unwrap_or_else(|_| panic!("loop displacement {disp} out of i16 range"));
        self.code.extend_from_slice(&disp.to_le_bytes());
    }

    // ---- pools ------------------------------------------------------------

    /// Intern a constant, returning its index. Primitive constants
    /// (`Number`/`Str`/`Bool`/`Nil`/`Decimal`) are de-duplicated by structural
    /// value (numbers by *bit pattern*, so `NaN` folds together and `-0.0`/`0.0`
    /// stay distinct); non-dedupable values are always appended.
    ///
    /// # Panics
    /// If the pool would exceed `u16::MAX` entries.
    pub fn add_const(&mut self, v: Value) -> u16 {
        if const_is_dedupable(&v) {
            if let Some(i) = self.consts.iter().position(|e| const_eq(e, &v)) {
                return u16::try_from(i).expect("const index fits u16");
            }
        }
        let idx = self.consts.len();
        let idx = u16::try_from(idx).expect("const pool exceeded u16::MAX");
        self.consts.push(v);
        idx
    }

    /// Append a function prototype, returning its index.
    ///
    /// # Panics
    /// If the proto table would exceed `u16::MAX` entries.
    pub fn add_proto(&mut self, p: Rc<FnProto>) -> u16 {
        let idx = self.protos.len();
        let idx = u16::try_from(idx).expect("proto table exceeded u16::MAX");
        self.protos.push(p);
        idx
    }

    /// Append a class definition, returning its index.
    ///
    /// # Panics
    /// If the class-proto table would exceed `u16::MAX` entries.
    pub fn add_class_proto(&mut self, p: Rc<ClassProto>) -> u16 {
        let idx = self.class_protos.len();
        let idx = u16::try_from(idx).expect("class-proto table exceeded u16::MAX");
        self.class_protos.push(p);
        idx
    }

    /// Append an import descriptor, returning its index (the `IMPORT` operand).
    ///
    /// # Panics
    /// If the import table would exceed `u16::MAX` entries.
    pub fn add_import(&mut self, desc: ImportDesc) -> u16 {
        let idx = self.imports.len();
        let idx = u16::try_from(idx).expect("import table exceeded u16::MAX");
        self.imports.push(desc);
        idx
    }

    // ---- reads (disassembler / VM) ---------------------------------------

    /// Read the `u16` little-endian operand starting at byte `at`.
    pub fn read_u16(&self, at: usize) -> u16 {
        u16::from_le_bytes([self.code[at], self.code[at + 1]])
    }

    /// Read the `u8` operand at byte `at`.
    pub fn read_u8(&self, at: usize) -> u8 {
        self.code[at]
    }

    /// Read the `i16` little-endian (jump) operand starting at byte `at`.
    pub fn read_i16(&self, at: usize) -> i16 {
        i16::from_le_bytes([self.code[at], self.code[at + 1]])
    }

    // ---- inline caches (V11) ---------------------------------------------

    /// The current field IC for the op at bytecode offset `op_off`. A site that
    /// has never been executed has no map entry and reads as [`InlineCache::Cold`].
    /// `InlineCache` is `Copy`, so this returns by value (no borrow held).
    pub fn field_ic(&self, op_off: usize) -> InlineCache {
        self.field_ics
            .borrow()
            .get(&op_off)
            .copied()
            .unwrap_or_default()
    }

    /// Store the updated field IC for the op at bytecode offset `op_off`.
    pub fn set_field_ic(&self, op_off: usize, ic: InlineCache) {
        self.field_ics.borrow_mut().insert(op_off, ic);
    }

    /// The current method IC for the `CALL_METHOD` op at bytecode offset
    /// `op_off`, cloned out so no borrow is held across the call. A never-run site
    /// reads as [`MethodCache::Cold`].
    pub fn method_ic(&self, op_off: usize) -> MethodCache {
        self.method_ics
            .borrow()
            .get(&op_off)
            .cloned()
            .unwrap_or_default()
    }

    /// Store the updated method IC for the `CALL_METHOD` op at bytecode offset
    /// `op_off`.
    pub fn set_method_ic(&self, op_off: usize, ic: MethodCache) {
        self.method_ics.borrow_mut().insert(op_off, ic);
    }

    // ---- adaptive specialization (V11-T4) --------------------------------

    /// The current adaptive arithmetic state for the op at bytecode offset
    /// `op_off`. A never-run site has no entry and reads as the default (generic,
    /// warmup 0). `ArithCache` is `Copy`, so this returns by value (no borrow).
    pub fn arith_cache(&self, op_off: usize) -> ArithCache {
        self.arith_caches
            .borrow()
            .get(&op_off)
            .copied()
            .unwrap_or_default()
    }

    /// Store the updated adaptive arithmetic state for the op at `op_off`.
    pub fn set_arith_cache(&self, op_off: usize, c: ArithCache) {
        self.arith_caches.borrow_mut().insert(op_off, c);
    }

    /// The current global cache for the `GET_GLOBAL` op at bytecode offset
    /// `op_off`, cloned out so no borrow is held. A never-run site reads `Cold`.
    pub fn global_cache(&self, op_off: usize) -> GlobalCache {
        self.global_caches
            .borrow()
            .get(&op_off)
            .cloned()
            .unwrap_or_default()
    }

    /// Store the updated global cache for the `GET_GLOBAL` op at `op_off`.
    pub fn set_global_cache(&self, op_off: usize, c: GlobalCache) {
        self.global_caches.borrow_mut().insert(op_off, c);
    }

    // ---- spans ------------------------------------------------------------

    /// The span of the instruction whose opcode byte is at or just before
    /// `offset` (nearest-preceding). Returns a zero span if no spans are
    /// recorded.
    pub fn span_at(&self, offset: usize) -> Span {
        if self.spans.is_empty() {
            return Span::new(0, 0);
        }
        // partition_point gives the count of entries with key <= offset; the last
        // such entry is the nearest-preceding instruction.
        let idx = self.spans.partition_point(|(off, _)| *off <= offset);
        if idx == 0 {
            // offset precedes the first recorded instruction.
            self.spans[0].1
        } else {
            self.spans[idx - 1].1
        }
    }

    /// Record a span for the instruction about to be emitted at the current code
    /// length. Kept sorted by construction (offsets are monotonic).
    fn record_span(&mut self, span: Span) {
        self.spans.push((self.code.len(), span));
    }
}

/// Whether a constant value participates in pool de-duplication.
fn const_is_dedupable(v: &Value) -> bool {
    matches!(
        v,
        Value::Nil | Value::Bool(_) | Value::Number(_) | Value::Str(_) | Value::Decimal(_)
    )
}

/// Structural equality for de-dupable constants. Numbers compare by bit pattern
/// (so `NaN` constants fold together and `-0.0` stays distinct from `0.0`), which
/// is the correct identity for a constant pool. All other kinds are never passed
/// here (guarded by [`const_is_dedupable`]).
fn const_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Nil, Value::Nil) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x.to_bits() == y.to_bits(),
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Decimal(x), Value::Decimal(y)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn s(a: usize, b: usize) -> Span {
        Span::new(a, b)
    }

    #[test]
    fn emit_writes_bytes_and_spans() {
        let mut c = Chunk::new();
        c.emit_u16(Op::Const, 0x0102, s(0, 5)); // bytes 0,1,2
        c.emit(Op::Add, s(6, 7)); // byte 3
        c.emit_u8(Op::Call, 3, s(8, 12)); // bytes 4,5

        // Code layout: [Const, 0x02, 0x01, Add, Call, 0x03]
        assert_eq!(c.code[0], Op::Const as u8);
        assert_eq!(c.read_u16(1), 0x0102);
        assert_eq!(c.code[3], Op::Add as u8);
        assert_eq!(c.code[4], Op::Call as u8);
        assert_eq!(c.read_u8(5), 3);
        assert_eq!(c.code.len(), 6);
    }

    #[test]
    fn span_at_nearest_preceding() {
        let mut c = Chunk::new();
        c.emit_u16(Op::Const, 7, s(10, 15)); // op offset 0
        c.emit(Op::Add, s(20, 21)); // op offset 3

        // Exact opcode offsets.
        assert_eq!(c.span_at(0), s(10, 15));
        assert_eq!(c.span_at(3), s(20, 21));
        // Mid-instruction offset (operand byte of Const) -> the Const span.
        assert_eq!(c.span_at(1), s(10, 15));
        assert_eq!(c.span_at(2), s(10, 15));
        // Past the end -> last instruction's span.
        assert_eq!(c.span_at(99), s(20, 21));
    }

    #[test]
    fn span_at_empty_is_zero() {
        let c = Chunk::new();
        assert_eq!(c.span_at(0), s(0, 0));
        assert_eq!(c.span_at(42), s(0, 0));
    }

    #[test]
    fn add_const_dedups_primitives() {
        let mut c = Chunk::new();
        let a = c.add_const(Value::Number(1.0));
        let b = c.add_const(Value::Number(1.0));
        assert_eq!(a, b, "equal numbers dedup to the same slot");

        let s1 = c.add_const(Value::Str(Rc::from("hi")));
        let s2 = c.add_const(Value::Str(Rc::from("hi")));
        assert_eq!(s1, s2, "equal strings dedup");

        let n = c.add_const(Value::Number(2.0));
        assert_ne!(a, n, "distinct numbers get distinct slots");

        let t = c.add_const(Value::Bool(true));
        let f = c.add_const(Value::Bool(false));
        assert_ne!(t, f);

        // -0.0 and 0.0 are distinct constants (different bit patterns).
        let pz = c.add_const(Value::Number(0.0));
        let nz = c.add_const(Value::Number(-0.0));
        assert_ne!(pz, nz, "-0.0 and 0.0 are distinct constants");

        // NaN constants fold together (bit-pattern dedup).
        let nan1 = c.add_const(Value::Number(f64::NAN));
        let nan2 = c.add_const(Value::Number(f64::NAN));
        assert_eq!(nan1, nan2, "NaN constants fold together");
    }

    #[test]
    fn add_const_does_not_dedup_nondedupable() {
        let mut c = Chunk::new();
        let arr1 = c.add_const(Value::Array(gcmodule::Cc::new(std::cell::RefCell::new(vec![]))));
        let arr2 = c.add_const(Value::Array(gcmodule::Cc::new(std::cell::RefCell::new(vec![]))));
        assert_ne!(arr1, arr2, "non-dedupable values are always appended");
    }

    #[test]
    fn emit_jump_patch_round_trip() {
        let mut c = Chunk::new();
        c.emit(Op::Nil, s(0, 1)); // offset 0
        let site = c.emit_jump(Op::Jump, s(2, 3)); // op at 1, operand at site=2
        assert_eq!(site, 2);
        c.emit(Op::True, s(4, 5)); // offset 4
        c.emit(Op::False, s(6, 7)); // offset 5
        c.patch_jump(site); // target = current len (6)

        // Displacement is from after-operand (site+2 = 4) to target (6) = +2.
        assert_eq!(c.read_i16(site), 2);
        // Verify the VM's would-be computation lands at the target.
        let ip_after_operand = site + 2;
        let landed = (ip_after_operand as i64 + c.read_i16(site) as i64) as usize;
        assert_eq!(landed, 6);
    }

    #[test]
    fn emit_loop_backward_offset() {
        let mut c = Chunk::new();
        let top = c.code.len(); // 0
        c.emit(Op::Nil, s(0, 1)); // offset 0
        c.emit(Op::Pop, s(2, 3)); // offset 1
        c.emit_loop(Op::Loop, top, s(4, 5)); // op at 2, operand at 3..5

        // After-operand offset is 5; target is 0; disp = -5.
        assert_eq!(c.read_i16(3), -5);
        let ip_after_operand = 5i64;
        let landed = (ip_after_operand + c.read_i16(3) as i64) as usize;
        assert_eq!(landed, top);
    }

    #[test]
    fn add_proto_appends() {
        let mut c = Chunk::new();
        let p = Rc::new(FnProto {
            chunk: Chunk::new(),
            arity: 2,
            has_rest: false,
            is_async: false,
            is_generator: false,
            params: Vec::new(),
            ret: None,
        });
        assert_eq!(c.add_proto(p.clone()), 0);
        assert_eq!(c.add_proto(p), 1);
        assert_eq!(c.protos.len(), 2);
    }
}
