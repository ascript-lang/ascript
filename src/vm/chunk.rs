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

/// DBG (debug-info): a CHAR-offset → `(line, col)` index over a module's source
/// text. AScript `Span`s are CHAR offsets (`src/span.rs`), so the index counts
/// `'\n'` chars to find line-start char offsets, then binary-searches them. This is
/// a small, self-contained, CORE copy of the LSP's `LineIndex` (which depends on
/// `tower-lsp` + UTF-16 columns and is feature-gated) — DBG needs plain 0-based
/// `(line, col)` available under `--no-default-features`. Built ON DEMAND by the
/// debug-info derivation (`build_line_starts`/`line_col_at`), NEVER on the hot path.
struct LineIndex {
    /// Char offset of the first char of each line. `line_starts[0] == 0`. Sorted
    /// ascending, so `partition_point` finds the line of any char offset.
    line_starts: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut line_starts = vec![0usize];
        for (i, c) in text.chars().enumerate() {
            if c == '\n' {
                line_starts.push(i + 1);
            }
        }
        LineIndex { line_starts }
    }

    /// The 0-based line containing char `offset` (the greatest line start <= offset).
    fn line_of(&self, offset: usize) -> u32 {
        // `partition_point` counts entries with `line_start <= offset`; the last such
        // entry's index is the line. There is always at least one (`line_starts[0]`).
        let count = self.line_starts.partition_point(|&start| start <= offset);
        (count.saturating_sub(1)) as u32
    }

    /// The 0-based `(line, col)` of char `offset`. `col` is char offsets from the
    /// line start (good enough for DBG's source mapping; the DAP layer is char-based).
    fn line_col(&self, offset: usize) -> (u32, u32) {
        let line = self.line_of(offset);
        let line_start = self.line_starts[line as usize];
        let col = offset.saturating_sub(line_start) as u32;
        (line, col)
    }
}

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
    /// For each defaulted field (parallel to `default_fields`, declaration order),
    /// the free names its default expression captures from an enclosing scope,
    /// paired with the captured value's upvalue index within that field's thunk
    /// closure. At construct time the thunk reads these via `GET_UPVALUE`; the
    /// SHARED `validate_into` (`.from`/typed-parse) instead resolves the default
    /// AST against the class's `def_env`, so `Op::Class` copies each captured
    /// `name -> thunk.upvalues[idx]` cell value into `def_env` — making both paths
    /// resolve the SAME enclosing binding (e.g. a module-top-level `const`).
    /// Normally empty (a literal default captures nothing).
    pub default_captures: Vec<Vec<(String, u16)>>,
    /// Source-text for field defaults that are lowered by RE-PARSING the legacy
    /// front-end (`cst_default_expr` → `reparse_default_expr`): arrow and `match`
    /// defaults, whose bodies are arbitrary statement/pattern subtrees the flat
    /// `.aso` expr serializer does not encode structurally. Each entry is
    /// `(field name, padded source)` where the source is left-padded with spaces up
    /// to the node's start byte offset (so re-lexing keeps the original spans). The
    /// `.aso` writer consults this map and emits an `EX_REPARSE` tag carrying the
    /// source inline; on load the reader re-lowers it through the SAME legacy
    /// lexer/parser the tree-walker uses, so the reconstructed default is identical.
    /// Empty for the common case (every default is structurally lowered). NOT used
    /// by the in-memory VM run path (which keeps the reparsed `Expr` directly); it
    /// exists only to make `ascript build` round-trip arrow/match defaults (SP1 §4).
    pub default_sources: Vec<(String, String)>,
    /// The method names, in declaration order, matching the method closures
    /// pushed immediately before `Op::Class` (after the default thunks).
    pub method_names: Vec<String>,
    /// The STATIC method names (SP1 §3), in declaration order, matching the static
    /// closures pushed immediately AFTER the instance-method closures (so the stack
    /// below `Op::Class` is `[super?, ..thunks.., ..methods.., ..statics..]`).
    /// `Op::Class` pops them into the `class_static_methods` side table keyed by the
    /// class's `Rc` identity — a separate namespace from `method_names`.
    pub static_method_names: Vec<String>,
    /// Whether this class has an `extends` clause (V9-T2). When true, the compiler
    /// emits the superclass class-value expression FIRST (below the default thunks
    /// and method closures); `Op::Class` pops it and builds a fresh `Rc<Class>`
    /// whose `superclass` is that value. The prebuilt `class` field above is then a
    /// TEMPLATE (its `superclass` is `None`) — only its name/field-schemas are used.
    pub has_super: bool,
}

/// IFACE: a compiled interface definition referenced by an `Op::DefineInterface`
/// operand. Carries only DATA (name + own method requirements + `extends` names) —
/// the transitive flatten is lazy at runtime, and `def_env` is supplied by the VM
/// (its shared class/module env) at `DefineInterface` time, so siblings/forward refs
/// resolve. Whether the binding is a module-global or a frame-local is decided by
/// the surrounding DEFINE_GLOBAL/SET_LOCAL the compiler emits.
#[derive(Debug, Clone)]
pub struct InterfaceProto {
    pub name: String,
    /// `(method name, arity, has_rest)` in declaration order.
    pub methods: Vec<(String, usize, bool)>,
    pub extends: Vec<String>,
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
    /// `import { a, b } from "std/x"` — bind each named export.
    Named {
        source: String,
        /// `(export name, local slot, is_cell, is_global)` per imported name, in
        /// source order. When `is_global`, the name binds into the module-scope
        /// user-globals table (a DIRECT-child top-level import — the common case) and
        /// `slot`/`is_cell` are unused; otherwise it binds into the frame slot.
        names: Vec<(String, u16, bool, bool)>,
    },
    /// `import * as alias from "std/x"` — bind the namespace Object. When `is_global`
    /// the alias binds into the user-globals table under `alias` (and `slot`/`is_cell`
    /// are unused); otherwise it binds into the frame slot.
    Namespace {
        source: String,
        alias: String,
        slot: u16,
        is_cell: bool,
        is_global: bool,
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

/// A bytecode-capacity limit that a [`Chunk`] hit while being built (SP3 §A). The
/// chunk builder records the **first** such overflow (sticky / first-wins) into
/// `Chunk::overflow` and returns a safe placeholder index / skips the emit instead
/// of panicking; the compiler checks the flag after sealing each chunk and turns it
/// into a clean [`crate::compile::CompileError`] (non-zero exit, never a SIGABRT).
/// Each variant carries the [`Span`] of the triggering construct for the diagnostic.
#[derive(Debug, Clone, Copy)]
pub enum ChunkLimit {
    /// The constant pool would exceed `u16::MAX` entries.
    Consts(Span),
    /// The proto (nested-function) table would exceed `u16::MAX` entries.
    Protos(Span),
    /// The class-proto table would exceed `u16::MAX` entries.
    ClassProtos(Span),
    /// The import table would exceed `u16::MAX` entries.
    Imports(Span),
    /// The type-const side-pool (annotated `let`/`const` contract types) would
    /// exceed `u16::MAX` entries.
    TypeConsts(Span),
    /// A forward jump displacement would exceed the `i16` range (a single function
    /// body emits > 32 KB of bytecode between a jump and its target).
    Jump(Span),
    /// A backward loop displacement would exceed the `i16` range.
    Loop(Span),
}

impl ChunkLimit {
    /// The triggering span (for the diagnostic).
    fn span(self) -> Span {
        match self {
            ChunkLimit::Consts(s)
            | ChunkLimit::Protos(s)
            | ChunkLimit::ClassProtos(s)
            | ChunkLimit::Imports(s)
            | ChunkLimit::TypeConsts(s)
            | ChunkLimit::Jump(s)
            | ChunkLimit::Loop(s) => s,
        }
    }

    /// The actionable message (SP3 §A2).
    fn message(self) -> &'static str {
        match self {
            ChunkLimit::Consts(_) => {
                "module exceeds 65535 constants; split the module into smaller files"
            }
            ChunkLimit::Protos(_) => {
                "module exceeds 65535 function definitions; split the module into smaller files"
            }
            ChunkLimit::ClassProtos(_) => {
                "module exceeds 65535 class definitions; split the module into smaller files"
            }
            ChunkLimit::Imports(_) => {
                "module exceeds 65535 imports; split the module into smaller files"
            }
            ChunkLimit::TypeConsts(_) => {
                "module exceeds 65535 annotated-binding type contracts; split the module into smaller files"
            }
            ChunkLimit::Jump(_) | ChunkLimit::Loop(_) => {
                "function body too large to compile (a single jump exceeds 32 KB of bytecode); split it into smaller functions"
            }
        }
    }

    /// Lower this capacity overflow into a clean compile error.
    pub fn into_compile_error(self) -> crate::compile::CompileError {
        crate::compile::CompileError::new(self.message(), self.span())
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
    /// IFACE: compiled interface definitions referenced by `DEFINE_INTERFACE` operands.
    /// (NOT serialized to `.aso` until Task 9; empty for non-interface programs.)
    pub interface_protos: Vec<Rc<InterfaceProto>>,
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
    /// Type contracts referenced by `CHECK_LOCAL` operands (NUM). Each
    /// `Op::CheckLocal` carries a u16 index into this side-pool; the run loop reads
    /// the declared `Type` and runs the SAME `interp::check_type` the tree-walker's
    /// `Stmt::Let` + `Op::CheckParam` use. A `Type` is not a `Value`, so it cannot
    /// live in the `consts` pool — this dedicated pool mirrors the proto's stored
    /// param/return types. Empty for any module with no annotated `let`/`const`.
    pub type_consts: Vec<crate::ast::Type>,
    /// Optional name (function name / `<script>`), for the disassembler & traces.
    pub name: Option<String>,
    /// The FIRST bytecode-capacity overflow this chunk hit while being built
    /// (SP3 §A). Sticky / first-wins: a capacity site records here and returns a
    /// safe placeholder instead of panicking; the compiler checks it after sealing
    /// each chunk ([`Chunk::take_overflow`]) and returns a clean `CompileError`.
    /// `None` for every valid (sub-65535) module — the placeholder is never
    /// executed because compile aborts the moment the flag is observed set.
    pub overflow: std::cell::Cell<Option<ChunkLimit>>,
    /// The MODULE source (`path` + full text) this chunk's spans index, for
    /// cross-module diagnostic provenance (SP4 §3). Set at compile/load time on a
    /// module's whole proto tree (see [`Chunk::set_module_source`]); read by the
    /// VM's panic path to bind the span to its OWN module's text so a panic raised
    /// in module A but propagating to B's call site renders the caret in A.
    /// Runtime-only: NOT serialized to `.aso` (an `.aso` has no source to render),
    /// defaults to `None` (the renderer falls back to the entry source — the
    /// pre-SP4 single-module behavior).
    pub source: RefCell<Option<std::rc::Rc<crate::error::SourceInfo>>>,
}

impl Chunk {
    /// Recursively bind `src` as the module source of this chunk AND every nested
    /// function/class-method proto chunk, so a panic in ANY function of the module
    /// resolves to the module's own source. Idempotent / innermost-wins: a chunk
    /// that already has a source is left as-is (a re-entered cached module).
    pub fn set_module_source(&self, src: &std::rc::Rc<crate::error::SourceInfo>) {
        {
            let mut slot = self.source.borrow_mut();
            if slot.is_some() {
                return; // already bound (cached module) — innermost-wins
            }
            *slot = Some(src.clone());
        }
        // Every nested function/method/arrow body is a `FnProto` in `protos`
        // (method closures are created via `CLOSURE` ops referencing `protos`), so
        // recursing through `protos` covers the whole module proto tree.
        for proto in &self.protos {
            proto.chunk.set_module_source(src);
        }
    }
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
    /// True when the function was declared with the contextual `worker` modifier
    /// (`worker fn f()`). The VM will spawn this closure on a worker thread
    /// (Task 3+). Propagated from the CST `WorkerKw` token by the compiler.
    pub is_worker: bool,
    /// For a `static worker fn` the name of the ENCLOSING class, set by the
    /// compiler when compiling static method protos. `None` for free functions
    /// and non-static/non-worker methods. Used by the VM's
    /// `dispatch_worker_closure` to route to
    /// `build_code_slice_for_static_method` instead of `build_code_slice` (which
    /// only handles top-level fns). Serialized in `.aso` format version 17+.
    pub owning_class: Option<Rc<str>>,
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
    /// DBG (debug-info, §5.1 #2): slot → source-name table for this frame's locals
    /// (`let`/`const`/loop-var/param), populated by the compiler from the resolver's
    /// `bindings` grouped by frame. PURE debug metadata — NEVER read by the VM
    /// run loop; consulted only by an attached debugger to label `scopes`/`variables`
    /// (without it a local is `slot_N`). Strippable: a `--strip`ped `.aso` omits it
    /// (Task 6), and an empty table simply means "no names" (the `slot_N` fallback).
    /// Not serialized this task — reconstructed at attach time / serialized in Task 6.
    pub local_names: Vec<(u32, Rc<str>)>,
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

    /// Emit `op` followed by its operand bytes copied VERBATIM from `operand_bytes`.
    /// Used by the worker code-slice builder to relocate an instruction whose operand
    /// carries no pool index (a local slot, an upvalue index, a RELATIVE jump
    /// displacement, or a count) — those bytes are position-independent under a
    /// contiguous range copy, so they are reproduced unchanged.
    pub fn emit_raw(&mut self, op: Op, operand_bytes: &[u8], span: Span) {
        self.record_span(span);
        self.code.push(op as u8);
        self.code.extend_from_slice(operand_bytes);
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
    /// On capacity overflow (displacement out of `i16` range — a function body
    /// emitting > 32 KB of bytecode between the jump and its target) records a
    /// sticky [`ChunkLimit::Jump`] and writes a `0` placeholder (SP3 §A); the
    /// compiler turns the recorded overflow into a clean `CompileError`.
    pub fn patch_jump(&mut self, site: usize) {
        let target = self.code.len();
        let from = site + 2;
        let disp = i64::try_from(target).unwrap() - i64::try_from(from).unwrap();
        let disp = i16::try_from(disp).unwrap_or_else(|_| {
            self.record_overflow(ChunkLimit::Jump(self.cur_span()));
            0
        });
        self.code[site..site + 2].copy_from_slice(&disp.to_le_bytes());
    }

    /// Emit `JUMP_IF_ARG_SUPPLIED(u16 param_index, i16 disp)` with a placeholder
    /// `0` displacement. Returns the patch site (the i16 operand offset) for a
    /// later [`Chunk::patch_jump`] — the same `from = site + 2` accounting the
    /// 2-byte jumps use, since the run loop measures the target from the byte
    /// after the i16 displacement (the instruction end).
    pub fn emit_jump_if_arg_supplied(&mut self, param_index: u16, span: Span) -> usize {
        self.record_span(span);
        self.code.push(Op::JumpIfArgSupplied as u8);
        self.code.extend_from_slice(&param_index.to_le_bytes());
        let site = self.code.len();
        self.code.extend_from_slice(&0i16.to_le_bytes());
        site
    }

    /// Emit a backward (loop) jump whose displacement lands at `target`. The
    /// displacement is measured from the byte after the operand to `target`, so it
    /// is negative for a real backward jump.
    ///
    /// On capacity overflow (displacement out of `i16` range — a loop body > 32 KB
    /// of bytecode) records a sticky [`ChunkLimit::Loop`] and writes a `0`
    /// placeholder (SP3 §A); the compiler turns the recorded overflow into a clean
    /// `CompileError`.
    pub fn emit_loop(&mut self, op: Op, target: usize, span: Span) {
        self.record_span(span);
        self.code.push(op as u8);
        let from = self.code.len() + 2;
        let disp = i64::try_from(target).unwrap() - i64::try_from(from).unwrap();
        let disp = i16::try_from(disp).unwrap_or_else(|_| {
            self.record_overflow(ChunkLimit::Loop(span));
            0
        });
        self.code.extend_from_slice(&disp.to_le_bytes());
    }

    // ---- pools ------------------------------------------------------------

    /// Intern a constant, returning its index. Primitive constants
    /// (`Number`/`Str`/`Bool`/`Nil`/`Decimal`) are de-duplicated by structural
    /// value (numbers by *bit pattern*, so `NaN` folds together and `-0.0`/`0.0`
    /// stay distinct); non-dedupable values are always appended.
    ///
    /// On capacity overflow (> `u16::MAX` entries) this records a sticky
    /// [`ChunkLimit::Consts`] and returns the placeholder index `u16::MAX` instead
    /// of panicking (SP3 §A); the compiler turns the recorded overflow into a clean
    /// `CompileError` after sealing the chunk, so the placeholder never executes.
    pub fn add_const(&mut self, v: Value) -> u16 {
        if const_is_dedupable(&v) {
            if let Some(i) = self.consts.iter().position(|e| const_eq(e, &v)) {
                // A dedup hit only returns a valid index past `u16::MAX` once the
                // pool is already oversized; record + clamp consistently with the
                // append path below.
                return u16::try_from(i).unwrap_or_else(|_| {
                    self.record_overflow(ChunkLimit::Consts(self.cur_span()));
                    u16::MAX
                });
            }
        }
        let idx = self.consts.len();
        let idx = u16::try_from(idx).unwrap_or_else(|_| {
            self.record_overflow(ChunkLimit::Consts(self.cur_span()));
            u16::MAX
        });
        self.consts.push(v);
        idx
    }

    /// Append a type contract to the `type_consts` side-pool (for `CHECK_LOCAL`),
    /// returning its index. Not deduplicated (annotated bindings are rare and a
    /// `Type` has no cheap structural-equality key here); each annotated `let`/
    /// `const` appends one entry.
    ///
    /// On capacity overflow (> `u16::MAX` entries) records a sticky
    /// [`ChunkLimit::TypeConsts`] and returns the placeholder `u16::MAX` (SP3 §A);
    /// the compiler turns the recorded overflow into a clean `CompileError` after
    /// sealing the chunk, so the placeholder never executes.
    pub fn add_type_const(&mut self, t: crate::ast::Type) -> u16 {
        let idx = self.type_consts.len();
        let idx = u16::try_from(idx).unwrap_or_else(|_| {
            self.record_overflow(ChunkLimit::TypeConsts(self.cur_span()));
            u16::MAX
        });
        self.type_consts.push(t);
        idx
    }

    /// Append a function prototype, returning its index.
    ///
    /// On capacity overflow (> `u16::MAX` protos) records a sticky
    /// [`ChunkLimit::Protos`] and returns the placeholder `u16::MAX` (SP3 §A).
    pub fn add_proto(&mut self, p: Rc<FnProto>) -> u16 {
        let idx = self.protos.len();
        let idx = u16::try_from(idx).unwrap_or_else(|_| {
            self.record_overflow(ChunkLimit::Protos(self.cur_span()));
            u16::MAX
        });
        self.protos.push(p);
        idx
    }

    /// Append a class definition, returning its index.
    ///
    /// On capacity overflow (> `u16::MAX` classes) records a sticky
    /// [`ChunkLimit::ClassProtos`] and returns the placeholder `u16::MAX` (SP3 §A).
    pub fn add_class_proto(&mut self, p: Rc<ClassProto>) -> u16 {
        let idx = self.class_protos.len();
        let idx = u16::try_from(idx).unwrap_or_else(|_| {
            self.record_overflow(ChunkLimit::ClassProtos(self.cur_span()));
            u16::MAX
        });
        self.class_protos.push(p);
        idx
    }

    /// IFACE: append an interface definition, returning its `DEFINE_INTERFACE` operand
    /// index. Overflow reuses the `ClassProtos` sticky limit (same `u16` cap class).
    pub fn add_interface_proto(&mut self, p: Rc<InterfaceProto>) -> u16 {
        let idx = self.interface_protos.len();
        let idx = u16::try_from(idx).unwrap_or_else(|_| {
            self.record_overflow(ChunkLimit::ClassProtos(self.cur_span()));
            u16::MAX
        });
        self.interface_protos.push(p);
        idx
    }

    /// Append an import descriptor, returning its index (the `IMPORT` operand).
    ///
    /// On capacity overflow (> `u16::MAX` imports) records a sticky
    /// [`ChunkLimit::Imports`] and returns the placeholder `u16::MAX` (SP3 §A).
    pub fn add_import(&mut self, desc: ImportDesc) -> u16 {
        let idx = self.imports.len();
        let idx = u16::try_from(idx).unwrap_or_else(|_| {
            self.record_overflow(ChunkLimit::Imports(self.cur_span()));
            u16::MAX
        });
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

    /// DBG: runtime-patch ONE opcode byte at `off` to `b` through a shared `&Chunk`
    /// (software-breakpoint patching, `src/vm/instrument.rs`). Used both to write
    /// `Op::Break` at a breakpoint and to restore the original byte when the trap
    /// fires (the v1 un-patch-on-hit re-execution, `Op::Break` arm in `run.rs`).
    ///
    /// # Why interior mutability here
    ///
    /// The run loop reaches a proto's `Chunk` only through a shared `Rc<FnProto>`
    /// (`&Chunk`), but breakpoint patching is the textbook self-modifying-bytecode
    /// technique (GDB/LLDB `int3`, the JVM `Breakpoint` bytecode, V8 debug bytecode):
    /// the byte stream IS mutated in place. `Chunk.code` stays a plain `Vec<u8>` so
    /// every other reader (the disassembler, verifier, `.aso` serializer, and the hot
    /// fetch `code[ip]`) is byte-identical and pays nothing — the NOT-attached path is
    /// completely untouched (Gate 12). Only this one method mutates through `&self`,
    /// and only when a debugger is attached.
    ///
    /// # Safety
    ///
    /// Sound because the VM is single-threaded and `!Send` (one `Interp`/`Vm` per OS
    /// thread; the chunk is never shared across threads). At the call site (the
    /// `Op::Break` trap arm) the current instruction's byte has already been fetched
    /// for this iteration, so no live `&u8` into `code[off]` is outstanding when the
    /// write happens — the next loop iteration re-reads the freshly-written byte.
    /// `off` is always an in-bounds opcode offset the breakpoint mechanism recorded.
    pub fn patch_byte(&self, off: usize, b: u8) {
        assert!(off < self.code.len(), "patch_byte offset out of bounds");
        // SAFETY: see the doc-comment above — single-threaded `!Send` VM, no aliasing
        // read of this byte is live across the write (the iteration already fetched
        // it), in-bounds offset.
        unsafe {
            let ptr = self.code.as_ptr() as *mut u8;
            *ptr.add(off) = b;
        }
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

    // ---- DBG debug-info: line ↔ bytecode-offset (derived) -----------------

    /// DBG (debug-info, §5.1 #1): build the `line → first-bytecode-offset` table
    /// from this chunk's `spans` + its module `source`. A debugger uses it for
    /// `setBreakpoints` (a source line → the FIRST executable instruction on it,
    /// the breakpoint target). It is a PURE FUNCTION of `spans` + source, so it is
    /// reconstructed lazily at attach time (and serialized in the later Task 6),
    /// NEVER read by the run loop. Returns a `Vec<(line, offset)>` sorted ascending
    /// by `line`, with at most one entry per distinct source line (the first
    /// instruction whose span starts on that line). Lines are 0-based (matching the
    /// internal newline index); the DAP layer adds the +1 the protocol expects.
    ///
    /// If the module source is not bound (`source == None`, e.g. an `.aso`-only run
    /// before the Task-6 debug section lands) the table is empty — a debugger then
    /// reports "no debug info" rather than guessing.
    pub fn build_line_starts(&self) -> Vec<(u32, u32)> {
        let source = self.source.borrow();
        let Some(src) = source.as_ref() else {
            return Vec::new();
        };
        let index = LineIndex::new(&src.text);
        let mut out: Vec<(u32, u32)> = Vec::new();
        // `spans` is sorted ascending by code offset (emission is monotonic), so a
        // forward scan visits offsets in order. For each distinct source line, keep
        // the FIRST (lowest-offset) instruction whose span starts on that line.
        let mut last_line: Option<u32> = None;
        for (offset, span) in &self.spans {
            let line = index.line_of(span.start);
            if last_line == Some(line) {
                // Same line as the previous instruction — already recorded its first.
                continue;
            }
            // A line can still already be present out of monotonic order only if a
            // later instruction's span jumps back to an earlier line (it cannot, as
            // spans are emission-ordered and source-monotonic in practice); guard
            // anyway so the first wins.
            if out.iter().any(|(l, _)| *l == line) {
                last_line = Some(line);
                continue;
            }
            let off = u32::try_from(*offset).unwrap_or(u32::MAX);
            out.push((line, off));
            last_line = Some(line);
        }
        out.sort_unstable_by_key(|(line, _)| *line);
        out
    }

    /// DBG: map a bytecode `offset` to its `(line, col)` (both 0-based) using the
    /// instruction's recorded span + the module source. Returns `None` when the
    /// source is unbound. Used for `stackTrace` (a frame's `ip` → its source line).
    pub fn line_col_at(&self, offset: usize) -> Option<(u32, u32)> {
        let source = self.source.borrow();
        let src = source.as_ref()?;
        let index = LineIndex::new(&src.text);
        let span = self.span_at(offset);
        Some(index.line_col(span.start))
    }

    /// DBG: the breakpoint target for a source `line` (0-based) — the FIRST
    /// bytecode offset whose span starts on `line`, or, when `line` itself carries
    /// no instruction (a blank/comment line), the first offset of the NEXT
    /// executable line at or after it. Returns `None` when no instruction is on or
    /// after `line` (a trailing blank line / unbound source). Binary-searches the
    /// `line_starts` table via `partition_point`.
    pub fn first_offset_for_line(&self, line: u32) -> Option<u32> {
        let table = self.build_line_starts();
        // First entry whose line >= the requested line.
        let idx = table.partition_point(|(l, _)| *l < line);
        table.get(idx).map(|(_, off)| *off)
    }

    /// Record a span for the instruction about to be emitted at the current code
    /// length. Kept sorted by construction (offsets are monotonic).
    fn record_span(&mut self, span: Span) {
        self.spans.push((self.code.len(), span));
    }

    /// The most-recently recorded instruction span (or an empty span if none yet).
    /// Pool-capacity sites (`add_const`/`add_proto`/…) have no `Span` argument, so
    /// they attribute their overflow diagnostic to the last instruction emitted —
    /// close enough to point the user at the offending region.
    fn cur_span(&self) -> Span {
        self.spans
            .last()
            .map(|(_, s)| *s)
            .unwrap_or_else(|| Span::new(0, 0))
    }

    /// Record the FIRST bytecode-capacity overflow (sticky / first-wins). Later
    /// overflows are ignored so the diagnostic points at the first offending
    /// construct.
    fn record_overflow(&self, limit: ChunkLimit) {
        if self.overflow.get().is_none() {
            self.overflow.set(Some(limit));
        }
    }

    /// Take the recorded capacity overflow, if any. The compiler calls this after
    /// sealing each chunk; a `Some` becomes a clean `CompileError`.
    pub fn take_overflow(&self) -> Option<ChunkLimit> {
        self.overflow.take()
    }
}

/// Whether a constant value participates in pool de-duplication.
fn const_is_dedupable(v: &Value) -> bool {
    matches!(
        v,
        Value::Nil | Value::Bool(_) | Value::Float(_) | Value::Str(_) | Value::Decimal(_)
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
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
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
        let a = c.add_const(Value::Float(1.0));
        let b = c.add_const(Value::Float(1.0));
        assert_eq!(a, b, "equal numbers dedup to the same slot");

        let s1 = c.add_const(Value::Str(Rc::from("hi")));
        let s2 = c.add_const(Value::Str(Rc::from("hi")));
        assert_eq!(s1, s2, "equal strings dedup");

        let n = c.add_const(Value::Float(2.0));
        assert_ne!(a, n, "distinct numbers get distinct slots");

        let t = c.add_const(Value::Bool(true));
        let f = c.add_const(Value::Bool(false));
        assert_ne!(t, f);

        // -0.0 and 0.0 are distinct constants (different bit patterns).
        let pz = c.add_const(Value::Float(0.0));
        let nz = c.add_const(Value::Float(-0.0));
        assert_ne!(pz, nz, "-0.0 and 0.0 are distinct constants");

        // NaN constants fold together (bit-pattern dedup).
        let nan1 = c.add_const(Value::Float(f64::NAN));
        let nan2 = c.add_const(Value::Float(f64::NAN));
        assert_eq!(nan1, nan2, "NaN constants fold together");
    }

    #[test]
    fn add_const_does_not_dedup_nondedupable() {
        let mut c = Chunk::new();
        let arr1 = c.add_const(Value::Array(crate::value::ArrayCell::new(
            vec![],
        )));
        let arr2 = c.add_const(Value::Array(crate::value::ArrayCell::new(
            vec![],
        )));
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
            is_worker: false,
            owning_class: None,
            params: Vec::new(),
            ret: None,
            local_names: Vec::new(),
        });
        assert_eq!(c.add_proto(p.clone()), 0);
        assert_eq!(c.add_proto(p), 1);
        assert_eq!(c.protos.len(), 2);
    }

    // ---- DBG debug-info: LineIndex (line ↔ char-offset) -------------------

    #[test]
    fn line_index_basic() {
        // "ab\ncde\nf" — char offsets: a0 b1 \n2 c3 d4 e5 \n6 f7
        let idx = LineIndex::new("ab\ncde\nf");
        assert_eq!(idx.line_of(0), 0); // 'a'
        assert_eq!(idx.line_of(1), 0); // 'b'
        assert_eq!(idx.line_of(2), 0); // '\n' belongs to line 0
        assert_eq!(idx.line_of(3), 1); // 'c'
        assert_eq!(idx.line_of(5), 1); // 'e'
        assert_eq!(idx.line_of(7), 2); // 'f'
        assert_eq!(idx.line_col(0), (0, 0));
        assert_eq!(idx.line_col(4), (1, 1)); // 'd', col 1 on line 1
        assert_eq!(idx.line_col(7), (2, 0));
    }

    #[test]
    fn line_index_empty_and_oob() {
        let idx = LineIndex::new("");
        assert_eq!(idx.line_of(0), 0);
        // Out-of-range offset still resolves to the last line (no panic).
        let idx2 = LineIndex::new("x\ny");
        assert_eq!(idx2.line_of(999), 1);
    }

    // ---- DBG debug-info: line_starts / line_col / first_offset_for_line ---

    /// Build a chunk whose instructions sit at known source-line offsets, with a
    /// bound module source so the derived tables have line numbers to work with.
    fn chunk_with_source(text: &str, instrs: &[(Op, Span)]) -> Chunk {
        let mut c = Chunk::new();
        for (op, span) in instrs {
            c.emit(*op, *span);
        }
        let src = Rc::new(crate::error::SourceInfo {
            path: "<test>".into(),
            text: text.into(),
        });
        c.set_module_source(&src);
        c
    }

    #[test]
    fn line_starts_no_source_is_empty() {
        // No module source bound -> no line numbers -> empty (debugger reports
        // "no debug info" rather than guessing).
        let mut c = Chunk::new();
        c.emit(Op::Nil, s(0, 1));
        assert!(c.build_line_starts().is_empty());
        assert_eq!(c.line_col_at(0), None);
        assert_eq!(c.first_offset_for_line(0), None);
    }

    #[test]
    fn line_starts_first_instruction_per_line() {
        // Source: three lines. Line 0 spans chars 0..4 ("aaa\n"), line 1 chars 4..8
        // ("bbb\n"), line 2 chars 8.. ("ccc"). Two instructions on line 0, one on
        // line 2 — line 1 (blank of executable code) has none.
        // code offsets: Nil@0 (line0), Pop@1 (line0), Add@2 (line2)
        let text = "aaa\nbbb\nccc";
        let c = chunk_with_source(
            text,
            &[
                (Op::Nil, s(0, 1)),  // line 0
                (Op::Pop, s(1, 2)),  // line 0 (same line, not a new line-start)
                (Op::Add, s(8, 9)),  // line 2
            ],
        );
        let table = c.build_line_starts();
        // One entry per distinct source line that has an instruction: line 0 -> off 0,
        // line 2 -> off 2. Line 1 has no instruction → absent.
        assert_eq!(table, vec![(0u32, 0u32), (2u32, 2u32)]);
    }

    #[test]
    fn line_starts_blank_line_maps_to_next_executable_offset() {
        let text = "aaa\nbbb\nccc";
        let c = chunk_with_source(text, &[(Op::Nil, s(0, 1)), (Op::Add, s(8, 9))]);
        // Line 0 -> its own first offset.
        assert_eq!(c.first_offset_for_line(0), Some(0));
        // Line 1 (no instruction) -> the NEXT executable line's first offset (line 2 → off 1).
        assert_eq!(c.first_offset_for_line(1), Some(1));
        // Line 2 -> its own first offset.
        assert_eq!(c.first_offset_for_line(2), Some(1));
        // Past the last instruction's line -> None.
        assert_eq!(c.first_offset_for_line(3), None);
    }

    #[test]
    fn offset_to_line_round_trips() {
        let text = "let a = 1\nlet b = 2\nb + a";
        // Nil on line 0 (offset 0), Pop on line 1 (offset 1), Add on line 2 (offset 2).
        let c = chunk_with_source(
            text,
            &[
                (Op::Nil, s(0, 3)),   // line 0
                (Op::Pop, s(10, 13)), // line 1 (after the first '\n' at char 9)
                (Op::Add, s(20, 25)), // line 2 (after the second '\n' at char 19)
            ],
        );
        assert_eq!(c.line_col_at(0), Some((0, 0)));
        assert_eq!(c.line_col_at(1), Some((1, 0)));
        assert_eq!(c.line_col_at(2), Some((2, 0)));
        // line → offset and offset → line agree (round-trip) for each.
        for (line, off) in c.build_line_starts() {
            let (got_line, _col) = c.line_col_at(off as usize).unwrap();
            assert_eq!(got_line, line);
            assert_eq!(c.first_offset_for_line(line), Some(off));
        }
    }
}
