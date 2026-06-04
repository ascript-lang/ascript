//! Name resolution over the typed CST. See types.rs for the produced data.

pub mod types;

use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use cstree::text::TextRange;
use std::collections::{HashMap, HashSet};
use types::*;

/// Resolve a parsed source file.
pub fn resolve(root: &ResolvedNode) -> ResolveResult {
    let mut r = Resolver::new();
    r.resolve_file(root);
    r.finish()
}

/// First IDENT token's text within `node` (via the resolver-backed tree).
pub fn ident_text(node: &ResolvedNode) -> Option<String> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

/// Stable map key for a use site: its text range.
pub fn use_key(node: &ResolvedNode) -> TextRange {
    node.text_range()
}

/// True if `node` is a `MethodDecl` carrying the `static` modifier (SP1 §3),
/// detected by a direct `StaticKw` child token. Shared by the resolver, compiler,
/// formatter, and checker so the three engines agree on what is a static method.
pub fn is_static_method(node: &ResolvedNode) -> bool {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::StaticKw)
}

fn is_expr(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        Literal
            | NameRef
            | UnaryExpr
            | BinaryExpr
            | ParenExpr
            | CallExpr
            | ArgList
            | MemberExpr
            | IndexExpr
            | ArrowExpr
            | AssignExpr
            | ArrayExpr
            | ObjectExpr
            | MapExpr
            | TemplateExpr
            | OptMemberExpr
            | TryExpr
            | UnwrapExpr
            | TernaryExpr
            | AwaitExpr
            | YieldExpr
            | MatchExpr
            | RangeExpr
    )
}

struct Frame {
    bindings: Vec<Binding>,
    next_slot: u32,
    key: TextRange,
    upvalues: Vec<UpvalueDescriptor>,
    /// Index, within `scopes`, of the first scope belonging to this frame.
    scope_base: usize,
}

struct Scope {
    names: HashMap<String, u32>,
}

struct Resolver {
    result: ResolveResult,
    frames: Vec<Frame>,
    scopes: Vec<Scope>,
    /// Names of every DIRECT-child top-level binding of the `SourceFile`
    /// (`let`/`const`/`fn`/`class`/`enum`/`import`). These are MODULE-SCOPE
    /// USER-GLOBALS, not file-frame slot-locals: their references lower to
    /// `Resolution::Global(name)` (→ `GET_GLOBAL`) and their define-sites lower to
    /// `DEFINE_GLOBAL`, so a forward reference late-binds at run time — matching the
    /// tree-walker's single shared module `Environment`. Inner shadowing still wins
    /// because `resolve_local`/`resolve_upvalue` run BEFORE this set is consulted.
    module_globals: HashSet<String>,
    /// Per-module-global REASSIGNABILITY, collected up front (in
    /// `collect_module_globals`) so it is known BEFORE the resolution walk reaches any
    /// assignment — even one inside a function body that textually PRECEDES the
    /// global's declaration. A top-level `let` is mutable; `const`/`fn`/`class`/`enum`/
    /// `import` are immutable. Used to record immutable-assign targets for the
    /// guaranteed-panic store lowering.
    module_global_mutable: HashMap<String, bool>,
    /// Per-name read-use counters for module globals (the slot-based `bump_use`
    /// cannot count them — they have no file-frame slot). Mirrored into each global
    /// binding's `use_count` in `finish` so the checker's `unused-binding`/
    /// `unused-import` lints stay correct.
    global_uses: HashMap<String, u32>,
    /// The module-global bindings (one per declared top-level name), recorded for
    /// the checker. They carry no real frame slot (`slot` is unused for globals).
    global_bindings: Vec<Binding>,
}

impl Resolver {
    fn new() -> Self {
        Resolver {
            result: ResolveResult::default(),
            frames: Vec::new(),
            scopes: Vec::new(),
            module_globals: HashSet::new(),
            module_global_mutable: HashMap::new(),
            global_uses: HashMap::new(),
            global_bindings: Vec::new(),
        }
    }

    fn frame(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("a frame is open")
    }

    fn frame_ref(&self) -> &Frame {
        self.frames.last().expect("a frame is open")
    }

    fn push_scope(&mut self) {
        self.scopes.push(Scope {
            names: HashMap::new(),
        });
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// True when resolution is positioned at the DIRECT-child statement level of the
    /// `SourceFile`: the file frame is the only open frame AND only its root scope is
    /// open (no nested block/loop/match scope). A binding declared here is a
    /// module-scope user-global; a binding inside a top-level bare `{ }` block (which
    /// opens a child scope) is NOT — matching the tree-walker, where a `{ }` block is
    /// an `env.child()` so its `let` does not escape the block.
    fn at_module_top(&self) -> bool {
        self.frames.len() == 1 && self.scopes.len() == self.frame_ref().scope_base + 1
    }

    /// Whether a binding of `kind` is REASSIGNABLE. Only `let` and `param` are
    /// mutable; `const`/`fn`/`class`/`enum`/`import`/loop-var are immutable (mirroring
    /// the tree-walker's `Environment::define(..., mutable)` flag). A `PatternBind`'s
    /// mutability depends on whether it was destructured from a `let` or a `const`, so
    /// callers pass that through `declare_binding_mut` instead of relying on this.
    fn kind_is_mutable(kind: BindingKind) -> bool {
        matches!(kind, BindingKind::Let | BindingKind::Param)
    }

    /// Declare a binding, routing DIRECT-child top-level names (those in
    /// `module_globals`) to a global binding (no slot, not entered into a scope) and
    /// everything else to a normal frame-slot `declare`. Returns the assigned slot
    /// for slot-locals; for a global it returns `u32::MAX` (never used — a global has
    /// no slot, and its references lower to `GET_GLOBAL`, not `GET_LOCAL`). The
    /// binding's mutability is derived from `kind`; use `declare_binding_mut` for a
    /// pattern bind whose mutability comes from its enclosing `let`/`const`.
    fn declare_binding(&mut self, name: &str, kind: BindingKind, decl_range: TextRange) -> u32 {
        let mutable = Self::kind_is_mutable(kind);
        self.declare_binding_mut(name, kind, decl_range, mutable)
    }

    fn declare_binding_mut(
        &mut self,
        name: &str,
        kind: BindingKind,
        decl_range: TextRange,
        mutable: bool,
    ) -> u32 {
        if self.at_module_top() && self.module_globals.contains(name) {
            self.declare_global(name, kind, decl_range, mutable);
            u32::MAX
        } else {
            self.declare_mut(name, kind, decl_range, mutable)
        }
    }

    /// Record a module-scope user-global binding for the checker. It has NO file-frame
    /// slot and is NOT entered into the scope map (so `resolve_local`/`resolve_upvalue`
    /// never find it — a reference falls through to `Resolution::Global`). A REPEATED
    /// declaration of the same global name (`let x; let x`, `fn f; fn f`, …) is a
    /// same-scope redeclaration: the tree-walker rejects it at RUNTIME when the second
    /// define executes (`'<name>' is already defined in this scope`), so we (a) keep
    /// the FIRST binding canonical for the checker's use-counting, and (b) emit a
    /// resolve diagnostic so `ascript check` flags it statically. The COMPILER lowers
    /// every top-level define-site to `DEFINE_GLOBAL` regardless (it keys on
    /// `module_globals`, not this binding), so the second `DEFINE_GLOBAL` runtime-errors
    /// byte-identically.
    fn declare_global(
        &mut self,
        name: &str,
        kind: BindingKind,
        decl_range: TextRange,
        mutable: bool,
    ) {
        // Record EVERY top-level define-site (incl. a redeclaration) so the compiler
        // lowers each to `DEFINE_GLOBAL`; the second site runtime-errors.
        self.result.global_decl_ranges.insert(decl_range);
        if self.global_bindings.iter().any(|b| b.name == name) {
            self.result.diagnostics.push(ResolveDiagnostic {
                message: format!("'{name}' is already defined in this scope"),
                range: decl_range,
            });
            return;
        }
        self.global_bindings.push(Binding {
            name: name.to_string(),
            kind,
            slot: u32::MAX,
            decl_range,
            captured: false,
            mutated: false,
            use_count: 0,
            shadows: None,
            mutable,
            is_global: true,
        });
    }

    fn declare(&mut self, name: &str, kind: BindingKind, decl_range: TextRange) -> u32 {
        let mutable = Self::kind_is_mutable(kind);
        self.declare_mut(name, kind, decl_range, mutable)
    }

    fn declare_mut(
        &mut self,
        name: &str,
        kind: BindingKind,
        decl_range: TextRange,
        mutable: bool,
    ) -> u32 {
        // Shadowing is detected within the current function frame's scope stack
        // only; an inner fn shadowing an outer-fn binding is intentionally not
        // flagged (conservative). A frame-local that shadows a MODULE-SCOPE
        // user-global (e.g. an inner-block `let x` over a top-level `let x`) is also
        // flagged, since the tree-walker's single module env makes that a real shadow.
        let shadows = self
            .resolve_local(name)
            .and_then(|outer_slot| {
                self.frame_ref()
                    .bindings
                    .iter()
                    .find(|b| b.slot == outer_slot)
                    .map(|b| b.decl_range)
            })
            .or_else(|| {
                if self.module_globals.contains(name) {
                    self.global_bindings
                        .iter()
                        .find(|b| b.name == name)
                        .map(|b| b.decl_range)
                } else {
                    None
                }
            });
        let slot = self.frame().next_slot;
        self.frame().next_slot += 1;
        self.frame().bindings.push(Binding {
            name: name.to_string(),
            kind,
            slot,
            decl_range,
            captured: false,
            mutated: false,
            use_count: 0,
            shadows,
            mutable,
            is_global: false,
        });
        self.scopes
            .last_mut()
            .expect("a scope is open")
            .names
            .insert(name.to_string(), slot);
        slot
    }

    /// Whether `name` is already declared in the CURRENT (innermost) scope.
    /// Used by the hoisting pre-pass so the in-order walk reuses a hoisted slot
    /// instead of allocating a duplicate.
    fn declared_in_current_scope(&self, name: &str) -> bool {
        self.scopes
            .last()
            .is_some_and(|s| s.names.contains_key(name))
    }

    /// Pre-declare ("hoist") every DIRECT child `fn`/`class`/`enum` of a body so
    /// forward/mutual/self references resolve to a frame slot (Local/Upvalue),
    /// matching the tree-walker's late-binding outcome for define-before-call.
    /// Runs in EVERY scope the resolver opens (file frame, function frames, and
    /// bare blocks): pre-declaration only assigns a slot, and the VM's cells make
    /// late binding work, so this is uniform and cannot regress the checker
    /// (hoistable names are globally exempt from `undefined-variable`, and a
    /// forward use still counts toward `unused-binding`). Only fn/class/enum
    /// hoist — let/const/param/loop/pattern binding semantics are unchanged.
    fn hoist_decls(&mut self, children: &[ResolvedNode]) {
        use SyntaxKind::*;
        for child in children {
            // Unwrap a leading `export` to reach the hoistable decl underneath.
            let decl: &ResolvedNode = if child.kind() == ExportStmt {
                match child
                    .children()
                    .find(|c| matches!(c.kind(), FnDecl | ClassDecl | EnumDecl))
                {
                    Some(d) => d,
                    None => continue,
                }
            } else {
                child
            };
            let (name, kind) = match decl.kind() {
                FnDecl => (fn_name(decl), BindingKind::Fn),
                ClassDecl => (ident_text(decl), BindingKind::Class),
                EnumDecl => (ident_text(decl), BindingKind::Enum),
                _ => continue,
            };
            if let Some(name) = name {
                if !self.declared_in_current_scope(&name) {
                    self.declare(&name, kind, decl.text_range());
                }
            }
        }
    }

    fn resolve_local_in(&self, frame_idx: usize, name: &str) -> Option<u32> {
        let base = self.frames[frame_idx].scope_base;
        for scope in self.scopes[base..].iter().rev() {
            if let Some(&slot) = scope.names.get(name) {
                return Some(slot);
            }
        }
        None
    }

    fn resolve_local(&self, name: &str) -> Option<u32> {
        self.resolve_local_in(self.frames.len() - 1, name)
    }

    fn resolve_upvalue(&mut self, frame_idx: usize, name: &str) -> Option<u32> {
        if frame_idx == 0 {
            return None;
        }
        let parent = frame_idx - 1;
        if let Some(slot) = self.resolve_local_in(parent, name) {
            self.mark_captured(parent, slot);
            // Count the capture as a use of the declaring binding so the
            // `unused-binding`/`unused-import` lint does not flag bindings that
            // are only read from a nested function.
            self.bump_use_in(parent, slot);
            return Some(self.add_upvalue(frame_idx, UpvalueDescriptor::ParentLocal(slot)));
        }
        if let Some(idx) = self.resolve_upvalue(parent, name) {
            return Some(self.add_upvalue(frame_idx, UpvalueDescriptor::ParentUpvalue(idx)));
        }
        None
    }

    fn add_upvalue(&mut self, frame_idx: usize, desc: UpvalueDescriptor) -> u32 {
        let ups = &mut self.frames[frame_idx].upvalues;
        if let Some(i) = ups.iter().position(|u| *u == desc) {
            return i as u32;
        }
        ups.push(desc);
        (ups.len() - 1) as u32
    }

    /// Classify a NAME reference (a `NameRef`, or a `class … extends` superclass
    /// ident) into a `Resolution`, with the SAME ordering at every use site:
    /// `resolve_local` → `resolve_upvalue` → `Global`. Inner shadowing wins because
    /// the local/upvalue lookups run before the global fallthrough. When the name is
    /// a module global, its per-name read-use counter is bumped so the checker's
    /// `unused-binding`/`unused-import` lints (which run off the binding's
    /// `use_count`) stay correct even though the global has no frame slot.
    fn resolve_name(&mut self, name: &str) -> Resolution {
        if let Some(slot) = self.resolve_local(name) {
            self.bump_use(slot);
            Resolution::Local(slot)
        } else if let Some(idx) = self.resolve_upvalue(self.frames.len() - 1, name) {
            Resolution::Upvalue(idx)
        } else {
            if self.module_globals.contains(name) {
                *self.global_uses.entry(name.to_string()).or_insert(0) += 1;
            }
            Resolution::Global(name.to_string())
        }
    }

    fn mark_captured(&mut self, frame_idx: usize, slot: u32) {
        if let Some(b) = self.frames[frame_idx]
            .bindings
            .iter_mut()
            .find(|b| b.slot == slot)
        {
            b.captured = true;
        }
    }

    /// Collect the binding name(s) a DIRECT-child top-level statement introduces
    /// into `module_globals`. Mirrors what the tree-walker would bind into the
    /// module `Environment`: a `let`/`const` (ident or destructuring-pattern names),
    /// a hoisted `fn`/`class`/`enum`, and `import` names. An `export <decl>` is
    /// unwrapped to its inner decl. Statements that bind nothing are ignored.
    fn collect_module_globals(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        match node.kind() {
            LetStmt => {
                // A `const` (incl. const-destructure) is immutable; `let` is mutable.
                let mutable = let_kind(node) == BindingKind::Let;
                if let Some(arr) = node.children().find(|c| c.kind() == ArrayBindPat) {
                    self.collect_pattern_global_names(arr, mutable);
                } else if let Some(obj) = node.children().find(|c| c.kind() == ObjectBindPat) {
                    self.collect_pattern_global_names(obj, mutable);
                } else if let Some(name) = ident_text(node) {
                    self.register_global(name, mutable);
                }
            }
            FnDecl => {
                if let Some(name) = fn_name(node) {
                    self.register_global(name, false);
                }
            }
            ClassDecl | EnumDecl => {
                if let Some(name) = ident_text(node) {
                    self.register_global(name, false);
                }
            }
            ImportStmt => {
                self.collect_import_global_names(node);
            }
            ExportStmt => {
                for child in node.children() {
                    self.collect_module_globals(child);
                }
            }
            _ => {}
        }
    }

    /// Register a module-global NAME and its reassignability. The FIRST registration
    /// wins for mutability (a redeclaration is a runtime error anyway), so this never
    /// downgrades an already-recorded global.
    fn register_global(&mut self, name: String, mutable: bool) {
        self.module_global_mutable
            .entry(name.clone())
            .or_insert(mutable);
        self.module_globals.insert(name);
    }

    fn collect_pattern_global_names(&mut self, pat: &ResolvedNode, mutable: bool) {
        use SyntaxKind::*;
        for entry in pat.children() {
            match entry.kind() {
                BindEntry => {
                    let local = entry
                        .children_with_tokens()
                        .filter_map(|el| el.into_token())
                        .filter(|t| t.kind() == Ident)
                        .last()
                        .map(|t| t.text().to_string());
                    if let Some(name) = local {
                        self.register_global(name, mutable);
                    }
                }
                RestBind => {
                    if let Some(name) = ident_text(entry) {
                        self.register_global(name, mutable);
                    }
                }
                _ => {}
            }
        }
    }

    fn collect_import_global_names(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        // Imported names are IMMUTABLE bindings (tree-walker `define(..., false)`).
        if let Some(list) = node.children().find(|c| c.kind() == ImportList) {
            for t in list.children_with_tokens().filter_map(|el| el.into_token()) {
                if t.kind() == Ident {
                    self.register_global(t.text().to_string(), false);
                }
            }
        } else {
            let idents: Vec<String> = node
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .filter(|t| t.kind() == Ident)
                .map(|t| t.text().to_string())
                .collect();
            if let Some(pos) = idents.iter().position(|t| t == "as") {
                if let Some(alias) = idents.get(pos + 1) {
                    self.register_global(alias.clone(), false);
                }
            }
        }
    }

    fn resolve_file(&mut self, root: &ResolvedNode) {
        let key = root.text_range();
        self.frames.push(Frame {
            bindings: Vec::new(),
            next_slot: 0,
            key,
            upvalues: Vec::new(),
            scope_base: self.scopes.len(),
        });
        self.push_scope();
        let children: Vec<ResolvedNode> = root.children().cloned().collect();
        // CLASSIFY: every DIRECT-child top-level binding name is a module-scope
        // user-global. Collected BEFORE resolution so a forward reference (a use
        // textually earlier than its declaration) already sees the name as a global.
        for child in &children {
            self.collect_module_globals(child);
        }
        // No slot-hoisting at the file frame: top-level fn/class/enum are globals,
        // defined in SOURCE ORDER at run time (DEFINE_GLOBAL) and read late
        // (GET_GLOBAL), reproducing the tree-walker's late-binding module env. Bare
        // top-level `{ }` blocks still hoist their own fn/class/enum (handled in the
        // `Block` arm of `resolve_stmt`).
        for child in &children {
            self.resolve_stmt(child);
        }
        self.pop_scope();
        let frame = self.frames.pop().unwrap();
        self.result.bindings.extend(frame.bindings.iter().cloned());
        // Baseline VM semantics: EVERY captured local is a by-reference cell
        // (allocated nil at frame entry, filled when its declaration executes).
        // This preserves the tree-walker's late binding for forward/mutual/self
        // references. Capture-by-value for never-forward-referenced immutable
        // bindings is a FUTURE optimization (V5), not the baseline.
        let cell_slots: Vec<u32> = frame
            .bindings
            .iter()
            .filter(|b| b.captured)
            .map(|b| b.slot)
            .collect();
        self.result.frames.insert(
            (SyntaxKind::SourceFile, frame.key),
            FrameInfo {
                slot_count: frame.next_slot,
                upvalues: frame.upvalues,
                cell_slots,
            },
        );
    }

    fn resolve_stmt(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        match node.kind() {
            LetStmt => {
                // resolve the initializer BEFORE declaring (so `let x = x` sees outer x)
                for child in node.children() {
                    if is_expr(child.kind()) {
                        self.resolve_expr(child);
                    }
                }
                self.declare_let_bindings(node);
            }
            Block => {
                self.push_scope();
                let children: Vec<ResolvedNode> = node.children().cloned().collect();
                self.hoist_decls(&children);
                for child in &children {
                    self.resolve_stmt(child);
                }
                self.pop_scope();
            }
            IfStmt | WhileStmt => {
                for child in node.children() {
                    if is_expr(child.kind()) {
                        self.resolve_expr(child);
                    } else {
                        self.resolve_stmt(child); // branch Blocks open their own scope
                    }
                }
            }
            FnDecl => {
                // Reuse the slot allocated by the hoisting pre-pass if present;
                // a DIRECT-child top-level fn is a module global (no slot).
                if let Some(name) = fn_name(node) {
                    if !self.declared_in_current_scope(&name) {
                        self.declare_binding(&name, BindingKind::Fn, node.text_range());
                    }
                }
                self.resolve_function(node);
            }
            ForStmt => {
                self.push_scope();
                for child in node.children() {
                    if is_expr(child.kind()) {
                        self.resolve_expr(child);
                    }
                }
                if let Some(name) = ident_text(node) {
                    self.declare(&name, BindingKind::LoopVar, node.text_range());
                }
                if let Some(body) = node.children().find(|c| c.kind() == Block) {
                    for s in body.children() {
                        self.resolve_stmt(s);
                    }
                }
                self.pop_scope();
            }
            EnumDecl => {
                if let Some(name) = ident_text(node) {
                    if !self.declared_in_current_scope(&name) {
                        self.declare_binding(&name, BindingKind::Enum, node.text_range());
                    }
                }
                for v in node.descendants().filter(|n| n.kind() == EnumVariant) {
                    for e in v.children().filter(|c| is_expr(c.kind())) {
                        self.resolve_expr(e);
                    }
                }
            }
            ClassDecl => {
                if let Some(name) = ident_text(node) {
                    if !self.declared_in_current_scope(&name) {
                        self.declare_binding(&name, BindingKind::Class, node.text_range());
                    }
                }
                self.resolve_class(node);
            }
            ImportStmt => {
                self.declare_import_bindings(node);
            }
            ExportStmt => {
                for child in node.children() {
                    self.resolve_stmt(child);
                }
            }
            BreakStmt | ContinueStmt => {}
            ExprStmt | ReturnStmt => {
                for child in node.children() {
                    if is_expr(child.kind()) {
                        self.resolve_expr(child);
                    }
                }
            }
            _ => {
                for child in node.children() {
                    self.resolve_stmt(child);
                }
            }
        }
    }

    fn declare_let_bindings(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        // A `const` destructuring binds IMMUTABLE pattern names; `let` binds mutable.
        let mutable = let_kind(node) == BindingKind::Let;
        if let Some(arr) = node.children().find(|c| c.kind() == ArrayBindPat) {
            self.declare_pattern_names(arr, mutable);
        } else if let Some(obj) = node.children().find(|c| c.kind() == ObjectBindPat) {
            self.declare_pattern_names(obj, mutable);
        } else if let Some(name) = ident_text(node) {
            self.declare_binding(&name, let_kind(node), node.text_range());
        }
    }

    /// Declare every name introduced by a binding pattern (BindEntry's local/key,
    /// RestBind's name). `mutable` is the enclosing `let` (true) / `const` (false).
    fn declare_pattern_names(&mut self, pat: &ResolvedNode, mutable: bool) {
        use SyntaxKind::*;
        for entry in pat.children() {
            match entry.kind() {
                BindEntry => {
                    // `key` or `key as local` — the LAST IDENT is the local name.
                    let local = entry
                        .children_with_tokens()
                        .filter_map(|el| el.into_token())
                        .filter(|t| t.kind() == Ident)
                        .last()
                        .map(|t| t.text().to_string());
                    if let Some(name) = local {
                        self.declare_binding_mut(
                            &name,
                            BindingKind::PatternBind,
                            entry.text_range(),
                            mutable,
                        );
                    }
                }
                RestBind => {
                    if let Some(name) = ident_text(entry) {
                        self.declare_binding_mut(
                            &name,
                            BindingKind::PatternBind,
                            entry.text_range(),
                            mutable,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    fn declare_import_bindings(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        if let Some(list) = node.children().find(|c| c.kind() == ImportList) {
            for t in list.children_with_tokens().filter_map(|el| el.into_token()) {
                if t.kind() == Ident {
                    let name = t.text().to_string();
                    self.declare_binding(&name, BindingKind::Import, node.text_range());
                }
            }
        } else {
            // Namespace import `import * as <alias> from "..."`. The statement is a
            // flat token run; `as`/`from` lex as Idents too, so the alias is the
            // Ident immediately FOLLOWING the soft-keyword `as` (not the last Ident,
            // which would be `from`).
            let idents: Vec<String> = node
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .filter(|t| t.kind() == Ident)
                .map(|t| t.text().to_string())
                .collect();
            if let Some(pos) = idents.iter().position(|t| t == "as") {
                if let Some(alias) = idents.get(pos + 1) {
                    self.declare_binding(alias, BindingKind::Import, node.text_range());
                }
            }
        }
    }

    /// Find the superclass `Ident` token of a `ClassDecl` (the one following the
    /// soft keyword `extends`) and record a `Resolution` for it (keyed by the
    /// token's `text_range`), classifying it as a Local/Upvalue/Global exactly as
    /// `resolve_expr`'s `NameRef` arm would. No-op for a class without `extends`.
    fn record_superclass_use(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        // Tokens after (and including) `extends`: [0] = "extends", [1] = SuperName.
        let sup = node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .skip_while(|t| !(t.kind() == Ident && t.text() == "extends"))
            .filter(|t| t.kind() == Ident)
            .nth(1);
        let Some(sup) = sup else { return };
        let name = sup.text().to_string();
        let range = sup.text_range();
        let resolution = self.resolve_name(&name);
        self.result.uses.insert(range, resolution);
    }

    fn resolve_class(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        // `class X extends Y` — the superclass `Y` is a bare `Ident` TOKEN of the
        // ClassDecl (not a `NameRef` node), following the soft keyword `extends`.
        // Record a use-resolution for it (keyed by the token's text_range) so the
        // VM compiler can fetch the parent class value lexically, exactly like the
        // tree-walker's `env.get(sup_name)`. (The checker also benefits: the parent
        // counts as used.)
        self.record_superclass_use(node);
        for member in node.children() {
            match member.kind() {
                FieldDecl => self.resolve_field_default(member),
                MethodDecl => self.resolve_function(member),
                _ => {}
            }
        }
    }

    /// Resolve a field's default expression in its OWN frame, keyed by the
    /// `FieldDecl`'s range. The VM compiles each default into a standalone 0-arg
    /// thunk closure (run at CONSTRUCT time); giving the default its own frame
    /// means a name it references that lives in an enclosing scope (e.g. a
    /// module-top-level `const`, or a function local for a class declared inside a
    /// function) resolves to an `Upvalue` of the thunk frame and is captured by the
    /// SAME upvalue machinery every other closure uses (`UpvalueDescriptor` +
    /// `Op::Closure` cell capture). A default with no free references (the common
    /// case — a literal like `= "guest"`) produces an empty frame with no upvalues,
    /// so corpus programs are byte-identical to before this change.
    fn resolve_field_default(&mut self, member: &ResolvedNode) {
        let default = member.children().find(|c| is_expr(c.kind()));
        let Some(default) = default else { return };
        let key = member.text_range();
        self.frames.push(Frame {
            bindings: Vec::new(),
            next_slot: 0,
            key,
            upvalues: Vec::new(),
            scope_base: self.scopes.len(),
        });
        self.push_scope();
        self.resolve_expr(default);
        self.pop_scope();
        let frame = self.frames.pop().unwrap();
        self.result.bindings.extend(frame.bindings.iter().cloned());
        // See `resolve_file`: every captured local is a by-reference cell. (A field
        // default never declares its own locals, so `cell_slots` is normally empty;
        // it is computed uniformly for consistency with other frames.)
        let cell_slots: Vec<u32> = frame
            .bindings
            .iter()
            .filter(|b| b.captured)
            .map(|b| b.slot)
            .collect();
        self.result.frames.insert(
            (SyntaxKind::FieldDecl, frame.key),
            FrameInfo {
                slot_count: frame.next_slot,
                upvalues: frame.upvalues,
                cell_slots,
            },
        );
    }

    fn resolve_expr(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        match node.kind() {
            NameRef => {
                let name = ident_text(node).unwrap_or_default();
                let resolution = self.resolve_name(&name);
                self.result.uses.insert(node.text_range(), resolution);
            }
            MatchExpr => {
                // subject is the first expr child
                if let Some(subj) = node.children().find(|c| is_expr(c.kind())) {
                    self.resolve_expr(subj);
                }
                for arm in node.children().filter(|c| c.kind() == MatchArm) {
                    self.resolve_match_arm(arm);
                }
            }
            ArrowExpr => {
                self.resolve_function(node);
            }
            AssignExpr => {
                let mut children = node.children();
                if let Some(target) = children.next() {
                    if target.kind() == NameRef {
                        let name = ident_text(target).unwrap_or_default();
                        self.mark_mutated_target(&name, target.text_range());
                    }
                    self.resolve_expr(target);
                }
                for rest in children {
                    self.resolve_expr(rest);
                }
            }
            _ => {
                for child in node.children() {
                    self.resolve_expr(child);
                }
            }
        }
    }

    /// Mark the binding `name` resolves to as `mutated`, and — if that binding is
    /// IMMUTABLE — record the assignment target's `range` in
    /// `immutable_assign_targets` so the compiler lowers the store to a guaranteed
    /// `cannot assign to immutable binding` panic (runtime-timed). Resolution order
    /// mirrors `resolve_name`: nearest enclosing frame local/upvalue, then a
    /// module-scope user-global. A name that is NOT an in-scope binding (a bare /
    /// undefined global) records nothing — it takes the undefined-variable path.
    fn mark_mutated_target(&mut self, name: &str, range: TextRange) {
        for fi in (0..self.frames.len()).rev() {
            if let Some(slot) = self.resolve_local_in(fi, name) {
                if let Some(b) = self.frames[fi].bindings.iter_mut().find(|b| b.slot == slot) {
                    b.mutated = true;
                    if !b.mutable {
                        self.result.immutable_assign_targets.insert(range);
                    }
                }
                return;
            }
        }
        // Not a local/upvalue: a module-scope user-global (a top-level `const`/`fn`/…
        // is immutable; a top-level `let` is mutable). The mutability map is collected
        // UP FRONT, so this is correct even for an assignment inside a function body
        // that textually PRECEDES the global's declaration. A name that is not a
        // module global at all is a bare/undefined global — record nothing.
        if let Some(&mutable) = self.module_global_mutable.get(name) {
            if !mutable {
                self.result.immutable_assign_targets.insert(range);
            }
        }
    }

    fn bump_use(&mut self, slot: u32) {
        if let Some(b) = self.frame().bindings.iter_mut().find(|b| b.slot == slot) {
            b.use_count += 1;
        }
    }

    fn bump_use_in(&mut self, frame_idx: usize, slot: u32) {
        if let Some(b) = self.frames[frame_idx]
            .bindings
            .iter_mut()
            .find(|b| b.slot == slot)
        {
            b.use_count += 1;
        }
    }

    fn resolve_function(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        let key = node.text_range();
        let node_kind = node.kind();
        self.frames.push(Frame {
            bindings: Vec::new(),
            next_slot: 0,
            key,
            upvalues: Vec::new(),
            scope_base: self.scopes.len(),
        });
        self.push_scope();
        // A method (`MethodDecl`) has an implicit `self` receiver bound as the
        // FIRST local (slot 0), BEFORE its params. The slot-based VM binds the
        // receiver into slot 0 at the method CALL, so a `self` reference in a
        // method body must resolve to `Local(0)` (not a `Global`). It is declared
        // as `BindingKind::Param` so it is EXEMPT from the unused-binding lint
        // exactly like a parameter (a method that never reads `self` is fine). The
        // declaration range is the method node itself (there is no `self` token).
        //
        // A STATIC method (`static fn …`, SP1 §3) is a class-level call with NO
        // receiver, so it gets NO `self` slot: a `self` reference inside a static
        // is left unresolved (→ a Global lookup that fails at runtime on both
        // engines; `super` inside a static is flagged by the `super-misuse` lint).
        if node_kind == MethodDecl && !is_static_method(node) {
            self.declare("self", BindingKind::Param, key);
        }
        if let Some(params) = node.children().find(|c| c.kind() == ParamList) {
            for p in params.children().filter(|c| c.kind() == Param) {
                // A default-value expression resolves in a scope where the EARLIER
                // params are already bound but THIS param is not yet (so `b = a`
                // sees `a`, and `a = b` cannot see a later `b`). Resolve the
                // default child BEFORE declaring this param.
                for d in p.children().filter(|c| is_expr(c.kind())) {
                    self.resolve_expr(d);
                }
                if let Some(name) = ident_text(p) {
                    self.declare(&name, BindingKind::Param, p.text_range());
                }
            }
        }
        if let Some(body) = node.children().find(|c| c.kind() == Block) {
            let children: Vec<ResolvedNode> = body.children().cloned().collect();
            self.hoist_decls(&children);
            for child in &children {
                self.resolve_stmt(child);
            }
        }
        // Expression-body arrow (no Block): resolve the direct expression child
        // (the body) so its params/captures resolve correctly.
        if node.children().find(|c| c.kind() == Block).is_none() {
            for child in node.children().filter(|c| is_expr(c.kind())) {
                self.resolve_expr(child);
            }
        }
        self.pop_scope();
        let frame = self.frames.pop().unwrap();
        self.result.bindings.extend(frame.bindings.iter().cloned());
        // See `resolve_file`: every captured local is a by-reference cell.
        let cell_slots: Vec<u32> = frame
            .bindings
            .iter()
            .filter(|b| b.captured)
            .map(|b| b.slot)
            .collect();
        self.result.frames.insert(
            (node_kind, frame.key),
            FrameInfo {
                slot_count: frame.next_slot,
                upvalues: frame.upvalues,
                cell_slots,
            },
        );
    }

    fn resolve_match_arm(&mut self, arm: &ResolvedNode) {
        use SyntaxKind::*;
        self.push_scope();
        for pat in arm.children().filter(|c| is_pattern(c.kind())) {
            self.resolve_pattern(pat);
        }
        for child in arm.children() {
            match child.kind() {
                MatchGuard => {
                    for e in child.children().filter(|c| is_expr(c.kind())) {
                        self.resolve_expr(e);
                    }
                }
                k if is_expr(k) => self.resolve_expr(child),
                _ => {}
            }
        }
        self.pop_scope();
    }

    /// Option-C: a bare-ident LiteralPat NOT already resolvable binds the subject;
    /// a resolvable ident is a value compare. Nested array/object patterns recurse;
    /// ranges/values resolve their expressions.
    fn resolve_pattern(&mut self, pat: &ResolvedNode) {
        use SyntaxKind::*;
        match pat.kind() {
            WildcardPat => {}
            LiteralPat => {
                if let Some(name) = bare_ident_pattern(pat) {
                    // Option-C: a bare ident already in scope is a VALUE COMPARE; an
                    // unbound one BINDS the subject. A name in scope is a local, an
                    // upvalue, OR a MODULE-SCOPE user-global (a top-level `const`/`let`/
                    // `fn`/… used as a match comparand, e.g. `match s { NOT_FOUND => …`)
                    // — the tree-walker's single module env makes a top-level binding
                    // visible at match time, so it compares, never binds.
                    let resolvable = self.resolve_local(&name).is_some()
                        || self.resolve_upvalue(self.frames.len() - 1, &name).is_some()
                        || self.module_globals.contains(&name);
                    if resolvable {
                        // defined → value compare (resolve as a use)
                        for e in pat.children().filter(|c| is_expr(c.kind())) {
                            self.resolve_expr(e);
                        }
                    } else {
                        // undefined → bind
                        self.declare(&name, BindingKind::PatternBind, pat.text_range());
                    }
                } else {
                    for e in pat.children().filter(|c| is_expr(c.kind())) {
                        self.resolve_expr(e);
                    }
                }
            }
            RangePat => {
                for e in pat.children().filter(|c| is_expr(c.kind())) {
                    self.resolve_expr(e);
                }
            }
            ArrayPat | ObjectPat => {
                for sub in pat.children() {
                    match sub.kind() {
                        PatRest => {
                            if let Some(name) = ident_text(sub) {
                                self.declare(&name, BindingKind::PatternBind, sub.text_range());
                            }
                        }
                        ObjPatEntry => {
                            if let Some(subpat) = sub.children().find(|c| is_pattern(c.kind())) {
                                self.resolve_pattern(subpat);
                            } else if let Some(name) = ident_text(sub) {
                                self.declare(&name, BindingKind::PatternBind, sub.text_range());
                            }
                        }
                        k if is_pattern(k) => self.resolve_pattern(sub),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn finish(mut self) -> ResolveResult {
        // Merge the module-global bindings into the result, applying their per-name
        // read-use counts so the checker's unused-binding/unused-import lints work
        // (globals have no frame slot, so they could not use the slot-based counter).
        for mut b in self.global_bindings {
            b.use_count = self.global_uses.get(&b.name).copied().unwrap_or(0);
            self.result.bindings.push(b);
        }
        // Expose the module-global NAME set so the compiler can lower every top-level
        // define-site to DEFINE_GLOBAL (incl. a redeclaration's second site, which the
        // VM runtime-rejects), independent of any per-declaration binding range.
        self.result.module_globals = self.module_globals;
        self.result
    }
}

fn is_pattern(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        WildcardPat | IdentPat | LiteralPat | RangePat | ArrayPat | ObjectPat | OrPat
    )
}

/// If a LiteralPat is exactly a single bare `NameRef`, return its name.
fn bare_ident_pattern(pat: &ResolvedNode) -> Option<String> {
    let mut exprs = pat.children().filter(|c| is_expr(c.kind()));
    let first = exprs.next()?;
    if exprs.next().is_some() {
        return None;
    }
    if first.kind() == SyntaxKind::NameRef {
        ident_text(first)
    } else {
        None
    }
}

/// Determine whether a `LetStmt` node is a `const` or `let` binding.
fn let_kind(node: &ResolvedNode) -> BindingKind {
    let is_const = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::ConstKw);
    if is_const {
        BindingKind::Const
    } else {
        BindingKind::Let
    }
}

/// The declared name of a function (first IDENT token after `fn`/`async`/`*`).
fn fn_name(node: &ResolvedNode) -> Option<String> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse_to_tree;

    fn res(src: &str) -> ResolveResult {
        resolve(&parse_to_tree(src))
    }

    #[test]
    fn namespace_import_alias_binds_alias_not_from() {
        // `import * as t from "std/task"` binds the alias `t` (kind Import), not
        // the soft keyword `from`.
        let r = res("import * as t from \"std/task\"\n");
        assert!(
            r.bindings
                .iter()
                .any(|b| b.name == "t" && b.kind == BindingKind::Import),
            "alias `t` should be bound as an Import"
        );
        assert!(
            !r.bindings.iter().any(|b| b.name == "from"),
            "`from` must not be bound"
        );
    }

    #[test]
    fn use_in_nested_fn_counts() {
        // `x` is read only inside a nested fn, exercising the upvalue/capture path;
        // the read must still bump the binding's use_count.
        let r = res("let x = 1\nfn f() { return x }\n");
        let x = r
            .bindings
            .iter()
            .find(|b| b.name == "x")
            .expect("binding x exists");
        assert!(
            x.use_count >= 1,
            "nested read should count, got {}",
            x.use_count
        );
    }

    #[test]
    fn empty_program_resolves() {
        let r = res("");
        assert!(r.uses.is_empty());
        assert!(r.diagnostics.is_empty());
    }

    #[test]
    fn top_level_let_use_is_global() {
        // A DIRECT-child top-level `let` is now a MODULE-SCOPE user-global (not a
        // file-frame local), so BOTH `x` and the builtin `print` resolve to Global.
        let tree = parse_to_tree("let x = 1\nprint(x)");
        let r = resolve(&tree);
        let mut locals = 0;
        let mut globals = 0;
        for n in tree
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::NameRef)
        {
            match r.uses.get(&n.text_range()) {
                Some(Resolution::Local(_)) => locals += 1,
                Some(Resolution::Global(_)) => globals += 1,
                _ => {}
            }
        }
        assert_eq!(locals, 0, "no top-level locals (x is a module global)");
        assert_eq!(globals, 2, "x and print are both Global");
        // The top-level `x` binding is recorded as a global for the checker, with a
        // use count of 1 (the `print(x)` read).
        let x = r
            .bindings
            .iter()
            .find(|b| b.name == "x")
            .expect("x binding");
        assert!(x.is_global, "top-level let is a module global");
        assert_eq!(x.use_count, 1, "x is used once");
    }

    #[test]
    fn block_scoped_binding_does_not_leak() {
        // x declared inside the block; the outer use of x is Global (undefined
        // outside) — proves block scope pop. AScript blocks: `{ ... }`.
        let tree = parse_to_tree("{ let x = 1\n print(x) }\nprint(x)");
        let r = resolve(&tree);
        let refs: Vec<_> = tree
            .descendants()
            .filter(|n: &&ResolvedNode| {
                n.kind() == SyntaxKind::NameRef && ident_text(n).as_deref() == Some("x")
            })
            .map(|n| r.uses.get(&n.text_range()).cloned())
            .collect();
        assert_eq!(refs[0], Some(Resolution::Local(0)), "inner x is Local");
        assert_eq!(
            refs[1],
            Some(Resolution::Global("x".into())),
            "outer x is Global"
        );
    }

    #[test]
    fn params_are_locals_in_their_frame() {
        let tree = parse_to_tree("fn add(a, b) { return a + b }");
        let r = resolve(&tree);
        let mut local_uses = 0;
        for n in tree
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::NameRef)
        {
            if matches!(r.uses.get(&n.text_range()), Some(Resolution::Local(_))) {
                local_uses += 1;
            }
        }
        assert_eq!(local_uses, 2, "a and b resolve to locals");
    }

    #[test]
    fn inner_closure_captures_outer_as_upvalue() {
        let tree = parse_to_tree("fn outer() {\n let x = 1\n fn inner() { return x }\n}");
        let r = resolve(&tree);
        let x_use = tree
            .descendants()
            .find(|n| n.kind() == SyntaxKind::NameRef && ident_text(n).as_deref() == Some("x"))
            .unwrap();
        assert!(matches!(
            r.uses.get(&x_use.text_range()),
            Some(Resolution::Upvalue(0))
        ));
        let inner = tree
            .descendants()
            .find(|n| n.kind() == SyntaxKind::FnDecl && ident_text(n).as_deref() == Some("inner"))
            .unwrap();
        // Hoisting pre-declares `inner` (slot 0), so `x` is slot 1 in `outer`;
        // the captured upvalue therefore points at ParentLocal(1).
        let fi = r
            .frames
            .get(&(SyntaxKind::FnDecl, inner.text_range()))
            .expect("inner frame");
        assert_eq!(fi.upvalues, vec![UpvalueDescriptor::ParentLocal(1)]);
    }

    #[test]
    fn destructuring_binds_all_names() {
        // A DIRECT-child top-level destructuring `let` binds its names as MODULE
        // globals; `a` and `rest` therefore have global bindings (not file-frame
        // locals), each carrying a use count from their `print(...)` read.
        let tree = parse_to_tree("let [a, b, ...rest] = xs\nprint(a)\nprint(rest)");
        let r = resolve(&tree);
        for name in ["a", "rest"] {
            let b = r
                .bindings
                .iter()
                .find(|b| b.name == name)
                .unwrap_or_else(|| panic!("{name} binding"));
            assert!(b.is_global, "{name} is a module global");
            assert_eq!(b.use_count, 1, "{name} is read once");
        }
        // `b` is bound but unread; still a global binding with zero uses.
        let b = r
            .bindings
            .iter()
            .find(|b| b.name == "b")
            .expect("b binding");
        assert!(b.is_global && b.use_count == 0);
    }

    #[test]
    fn for_var_and_class_enum_bind() {
        // A loop variable is a frame-local; a top-level `class` is a module global.
        let r1 = resolve(&parse_to_tree("for (i in 0..3) { print(i) }"));
        assert!(
            r1.uses.values().any(|r| matches!(r, Resolution::Local(_))),
            "i is a local"
        );
        let r = resolve(&parse_to_tree("class C {}\nlet x = C"));
        assert!(
            r.uses
                .values()
                .any(|u| matches!(u, Resolution::Global(n) if n == "C")),
            "C resolves to a module global"
        );
        assert!(
            r.bindings.iter().any(|b| b.name == "C" && b.is_global),
            "C has a module-global binding"
        );
    }

    #[test]
    fn static_method_has_no_self_slot() {
        // SP1 §3: a `static fn` body has NO `self` binding — a `self` reference
        // there is unresolved (a Global lookup) — while a sibling instance method
        // still binds `self` to Local(0).
        let src = "class C {\n  fn inst() { return self }\n  static fn stat() { return self }\n}";
        let tree = parse_to_tree(src);
        let r = resolve(&tree);
        // Find the two `self` NameRef uses in document order: first = instance, second = static.
        let self_uses: Vec<_> = tree
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::NameRef && ident_text(n).as_deref() == Some("self"))
            .collect();
        assert_eq!(self_uses.len(), 2, "two `self` references");
        assert!(
            matches!(
                r.uses.get(&self_uses[0].text_range()),
                Some(Resolution::Local(0))
            ),
            "`self` in the instance method resolves to Local(0)"
        );
        assert!(
            matches!(
                r.uses.get(&self_uses[1].text_range()),
                Some(Resolution::Global(n)) if n == "self"
            ),
            "`self` in the static method is unresolved (a Global), got {:?}",
            r.uses.get(&self_uses[1].text_range())
        );
    }

    #[test]
    fn match_binds_undefined_compares_defined() {
        // Option-C: in `match v { other => other }`, `other` is undefined → binds,
        // and the body use of `other` resolves to that Local.
        let tree = parse_to_tree("let v = 1\nlet r = match v { other => other }");
        let r = resolve(&tree);
        let body_use = tree
            .descendants()
            .filter(|n| {
                n.kind() == SyntaxKind::NameRef && ident_text(n).as_deref() == Some("other")
            })
            .last()
            .unwrap();
        assert!(
            matches!(
                r.uses.get(&body_use.text_range()),
                Some(Resolution::Local(_))
            ),
            "bound pattern name is a Local in the arm body"
        );
    }

    #[test]
    fn match_arm_bindings_dont_leak() {
        let tree = parse_to_tree("let r = match v { x => x }\nprint(x)");
        let r = resolve(&tree);
        let outer_x = tree
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::NameRef && ident_text(n).as_deref() == Some("x"))
            .last()
            .unwrap();
        assert_eq!(
            r.uses.get(&outer_x.text_range()),
            Some(&Resolution::Global("x".into()))
        );
    }

    #[test]
    fn builtins_are_not_flagged_unresolved() {
        // print/len are builtins → Global, not diagnostics.
        let r = resolve(&parse_to_tree("print(len([1,2]))"));
        assert!(
            r.diagnostics.is_empty(),
            "builtins must not be flagged: {:?}",
            r.diagnostics
        );
    }

    #[test]
    fn param_default_resolves_earlier_param_as_local() {
        // In `fn f(a, b = a) { return b }`, the `a` inside `b`'s default must
        // resolve to the EARLIER param `a` (a Local), not an upvalue/global.
        let tree = parse_to_tree("fn f(a, b = a) { return b }\nprint(f(5))");
        let r = resolve(&tree);
        // The `a` use inside the default is the NameRef whose range is inside the
        // ParamList (the first `a` use after the params are declared).
        let param_list = tree
            .descendants()
            .find(|n| n.kind() == SyntaxKind::ParamList)
            .unwrap();
        let default_a = param_list
            .descendants()
            .find(|n| n.kind() == SyntaxKind::NameRef && ident_text(n).as_deref() == Some("a"))
            .expect("default references `a`");
        assert!(
            matches!(
                r.uses.get(&default_a.text_range()),
                Some(Resolution::Local(_))
            ),
            "param default must resolve an earlier param as Local, got {:?}",
            r.uses.get(&default_a.text_range())
        );
    }

    #[test]
    fn arrow_expression_body_resolves_params() {
        // x in the body of `x => x * 3` must be Local (the param), not Global.
        let tree = parse_to_tree("let triple = x => x * 3\nprint(triple(2))");
        let r = resolve(&tree);
        let body_x = tree
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::NameRef && ident_text(n).as_deref() == Some("x"))
            .last()
            .unwrap();
        assert!(
            matches!(r.uses.get(&body_x.text_range()), Some(Resolution::Local(_))),
            "arrow expression-body param must resolve to Local"
        );
    }

    #[test]
    fn captured_local_is_a_cell() {
        // Baseline VM semantics: EVERY captured local is a by-reference cell,
        // regardless of mutation. This preserves late binding — the cell is
        // allocated (nil) at frame entry and filled when its declaration runs,
        // so a closure that captured it sees the filled value at call time.
        // (Capture-by-value for immutable, never-forward-referenced bindings is
        // a FUTURE optimization (V5), not the baseline.)
        //
        // x captured but never reassigned → STILL a cell.
        let immut = parse_to_tree("fn o() {\n let x = 1\n fn i() { return x }\n}");
        let r1 = resolve(&immut);
        let oi = immut
            .descendants()
            .find(|n| n.kind() == SyntaxKind::FnDecl && ident_text(n).as_deref() == Some("o"))
            .unwrap();
        // Hoisting pre-declares `i` (slot 0), so `x` is slot 1.
        assert_eq!(
            r1.frames
                .get(&(SyntaxKind::FnDecl, oi.text_range()))
                .unwrap()
                .cell_slots,
            vec![1],
            "an immutable captured local is a cell under the baseline rule"
        );

        // y captured AND reassigned → IS a cell (unchanged).
        let mutated = parse_to_tree("fn o() {\n let y = 1\n fn i() { y = 2 }\n}");
        let r2 = resolve(&mutated);
        let oi2 = mutated
            .descendants()
            .find(|n| n.kind() == SyntaxKind::FnDecl && ident_text(n).as_deref() == Some("o"))
            .unwrap();
        // Hoisting pre-declares `i` (slot 0), so `y` is slot 1.
        assert_eq!(
            r2.frames
                .get(&(SyntaxKind::FnDecl, oi2.text_range()))
                .unwrap()
                .cell_slots,
            vec![1]
        );
    }

    #[test]
    fn uncaptured_local_is_not_a_cell() {
        // A local never captured by an inner function is NOT a cell (plain slot),
        // even if it is reassigned — cells are only for captured bindings.
        let tree = parse_to_tree("fn o() {\n let x = 1\n x = 2\n print(x)\n}");
        let r = resolve(&tree);
        let oi = tree
            .descendants()
            .find(|n| n.kind() == SyntaxKind::FnDecl && ident_text(n).as_deref() == Some("o"))
            .unwrap();
        assert!(
            r.frames
                .get(&(SyntaxKind::FnDecl, oi.text_range()))
                .unwrap()
                .cell_slots
                .is_empty(),
            "an uncaptured local must not be a cell"
        );
    }

    #[test]
    fn forward_fn_ref_resolves_to_module_global() {
        // `b` is referenced inside `a` before `b` is textually declared. A top-level
        // `fn` is a MODULE-SCOPE user-global, so the forward use of `b` resolves to
        // `Global("b")` (late-bound via GET_GLOBAL) — the call runs after both fns are
        // DEFINE_GLOBAL'd, reproducing the tree-walker's late module-env binding.
        let tree = parse_to_tree("fn a() { return b() }\nfn b() { return 7 }\n");
        let r = resolve(&tree);
        let b_use = tree
            .descendants()
            .find(|n| n.kind() == SyntaxKind::NameRef && ident_text(n).as_deref() == Some("b"))
            .unwrap();
        match r.uses.get(&b_use.text_range()) {
            Some(Resolution::Global(n)) if n == "b" => {}
            other => panic!("forward fn ref `b` should be a module global, got {other:?}"),
        }
        assert!(r.bindings.iter().any(|x| x.name == "b" && x.is_global));
    }

    #[test]
    fn self_recursion_is_a_module_global() {
        // A top-level self-recursive `fn` references itself by its module-global name
        // (late-bound), not via a captured file-frame cell.
        let tree =
            parse_to_tree("fn fac(n) {\n if (n <= 1) { return 1 }\n return n * fac(n - 1)\n}\n");
        let r = resolve(&tree);
        let fac_use = tree
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::NameRef && ident_text(n).as_deref() == Some("fac"))
            .last()
            .unwrap();
        assert!(
            matches!(r.uses.get(&fac_use.text_range()), Some(Resolution::Global(n)) if n == "fac"),
            "self-recursive top-level `fac` use must resolve to a module global"
        );
        let fac = r
            .bindings
            .iter()
            .find(|b| b.name == "fac" && b.kind == BindingKind::Fn)
            .expect("fac binding");
        assert!(fac.is_global, "top-level `fac` is a module global");
        assert_eq!(fac.use_count, 1, "the recursive call counts as one use");
    }

    #[test]
    fn mutual_recursion_both_resolve_to_module_globals() {
        // `a` calls `b`, `b` calls `a`. Both top-level fns resolve to module globals.
        let tree = parse_to_tree("fn a() { return b() }\nfn b() { return a() }\n");
        let r = resolve(&tree);
        for name in ["a", "b"] {
            let use_site = tree
                .descendants()
                .find(|n| n.kind() == SyntaxKind::NameRef && ident_text(n).as_deref() == Some(name))
                .unwrap();
            assert!(
                matches!(
                    r.uses.get(&use_site.text_range()),
                    Some(Resolution::Global(n)) if n == name
                ),
                "mutual-recursion use of `{name}` must be a module global, got {:?}",
                r.uses.get(&use_site.text_range())
            );
        }
    }
}
