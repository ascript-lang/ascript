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
    ParentLocal(u32),
    ParentUpvalue(u32),
}

#[derive(Debug, Clone, Default)]
pub struct FrameInfo {
    pub slot_count: u32,
    pub upvalues: Vec<UpvalueDescriptor>,
    pub cell_slots: Vec<u32>,
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
}
