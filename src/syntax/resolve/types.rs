//! Data produced by name resolution. Keyed by source `TextRange` so any consumer
//! (compiler, checker) can look results up without holding the tree's node types.

use crate::syntax::kind::SyntaxKind;
use cstree::text::TextRange;
use std::collections::HashMap;

/// What an identifier use resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Local(u32),
    Upvalue(u32),
    Global(String),
    Unresolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    Let,
    Const,
    Param,
    Fn,
    Class,
    /// IFACE: a structural interface declaration — an immutable module-global (or
    /// frame-local) binding, treated like `Class`/`Enum` for resolution.
    Interface,
    Enum,
    Import,
    PatternBind,
    LoopVar,
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub name: String,
    pub kind: BindingKind,
    pub slot: u32,
    pub decl_range: TextRange,
    pub captured: bool,
    pub mutated: bool,
    pub use_count: u32,
    /// If this binding shadows an outer binding, the outer's decl range.
    pub shadows: Option<TextRange>,
    /// Whether this binding is REASSIGNABLE (a `let`/`param`), as opposed to an
    /// immutable binding (`const`/`fn`/`class`/`enum`/`import`/`loop var`, and a
    /// pattern binding destructured from a `const`). Mirrors the tree-walker's
    /// `Environment::define(..., mutable)` flag — an assignment to an immutable
    /// binding is the runtime panic `cannot assign to immutable binding '<name>'`.
    pub mutable: bool,
    /// A MODULE-SCOPE USER-GLOBAL: a DIRECT-child top-level binding of the
    /// `SourceFile` (`let`/`const`/`fn`/`class`/`enum`/`import`). Such a binding has
    /// NO file-frame slot (`slot` is meaningless for it); its references resolve to
    /// `Resolution::Global(name)` and its define-site lowers to `DEFINE_GLOBAL`, so a
    /// forward reference late-binds at run time (matching the tree-walker's single
    /// shared module `Environment`). `false` for every nested-frame binding.
    pub is_global: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpvalueDescriptor {
    /// Capture a binding from the IMMEDIATE parent frame's slot `slot`. `by_value`
    /// (SP8 #136): when the source binding is NEVER reassigned (`!mutated`), the value
    /// is copied into a fresh private cell at `Op::Closure` instead of sharing the
    /// parent's heap cell — the parent's slot then needs no cell at all (plain
    /// `GET_LOCAL`, no `RefCell` borrow). `false` (the V5 baseline) keeps the shared
    /// by-reference cell, required for a reassigned binding (a counter closure).
    ParentLocal { slot: u32, by_value: bool },
    /// Capture a binding the parent itself captured (a transitive upvalue). It KEEPS
    /// the source upvalue's representation (no new bit — its kind is already fixed).
    ParentUpvalue(u32),
}

#[derive(Debug, Clone, Default)]
pub struct FrameInfo {
    pub slot_count: u32,
    pub upvalues: Vec<UpvalueDescriptor>,
    /// Slots that need a heap CELL: `captured && mutated` (SP8 #136 narrowed this from
    /// "every captured"). A `captured && !mutated` slot is NOT here — it is captured
    /// by value (see `value_capture_slots`) and stays a plain stack local.
    pub cell_slots: Vec<u32>,
    /// Slots captured BY VALUE: `captured && !mutated` (SP8 #136). They are plain stack
    /// locals in this frame (no cell, no `FreshCell`); a closure copies the slot's
    /// value into its own private upvalue cell at `Op::Closure`.
    pub value_capture_slots: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct ResolveDiagnostic {
    pub message: String,
    pub range: TextRange,
}

#[derive(Debug, Clone, Default)]
pub struct ResolveResult {
    pub uses: HashMap<TextRange, Resolution>,
    pub frames: HashMap<(SyntaxKind, TextRange), FrameInfo>,
    pub diagnostics: Vec<ResolveDiagnostic>,
    /// Every binding declared anywhere (across all frames), for the checker.
    pub bindings: Vec<Binding>,
    /// Text ranges of assignment-target `NameRef`s whose resolved binding is
    /// IMMUTABLE (`const`/`fn`/`class`/`enum`/`import`/loop-var, or a const-pattern
    /// bind). The compiler lowers such an assignment to a guaranteed-panic store
    /// (`cannot assign to immutable binding '<name>'`) anchored at the target span —
    /// runtime-timed (only panics if reached), matching the tree-walker's
    /// `Environment::assign` immutable error. A name that is NOT an in-scope binding
    /// (a bare/undefined global) is NOT recorded here (it gets the undefined-variable
    /// path instead).
    pub immutable_assign_targets: std::collections::HashSet<TextRange>,
    /// The names of every DIRECT-child top-level binding of the `SourceFile` — the
    /// MODULE-SCOPE user-globals. Used to classify a top-level reassignment target and
    /// (with `immutable_assign_targets`) the const checks.
    pub module_globals: std::collections::HashSet<String>,
    /// The `decl_range` of EVERY DIRECT-child top-level declaration site that binds a
    /// module global — including a REDECLARATION's second site (`let x; let x`), which
    /// `module_globals`/`bindings` dedupe away. The compiler keys on this to lower a
    /// declaration to `DEFINE_GLOBAL` IFF its range is here (so a same-named BLOCK or
    /// function-body `let` — which has its OWN range, NOT in this set — stays a frame
    /// slot-local, exactly as the resolver classified it). The redeclaration's second
    /// `DEFINE_GLOBAL` runtime-errors `'<name>' is already defined in this scope`.
    pub global_decl_ranges: std::collections::HashSet<TextRange>,
}
