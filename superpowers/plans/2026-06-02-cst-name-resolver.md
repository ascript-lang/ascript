# CST Name Resolver (Plan 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A scope/name-resolution pass over the typed CST that maps every identifier use to its declaration, assigns each local a per-frame slot, detects captured variables and builds each closure's upvalue plan (clox-style), records capture-by-value eligibility, and applies Option-C match-ident resolution — as **shared infrastructure** consumed first by the bytecode compiler and later by the checker.

**Architecture:** A standalone `src/syntax/resolve/` module that walks the resolver-backed `SyntaxNode` tree by kind (no interpreter dependency). It maintains a stack of scopes (block/function/class/loop/match-arm), threads a per-function frame for slot allocation, and produces a `ResolveResult` keyed by source `TextRange` so any consumer can look up "what does this use resolve to" and "what is this function's frame shape." Resolution is `Local(slot)` / `Upvalue(index)` / `Global(name)` / `Unresolved`.

**Tech Stack:** Rust, the Plan 1/2 `src/syntax/*` CST + typed AST, `cstree` (text via the resolver-backed tree).

**Scope note:** Plan 3 of the CST front-end (spec: `docs/superpowers/specs/2026-06-02-cst-frontend-migration-design.md`). It produces the resolution **data** the compiler needs (slots, upvalues, capture-by-value, Option-C). *Lint* derivations (unused, shadowing) are the checker's job (later sub-project) and only need the binding/use data this plan records; Plan 3 records that data and flags unresolved references, but does not itself emit style lints. Depends on Plans 2 / 2b. Does not touch the interpreter.

---

## File Structure

- Create `src/syntax/resolve/mod.rs` — the public `resolve(root) -> ResolveResult` + the tree walker.
- Create `src/syntax/resolve/types.rs` — `Resolution`, `Binding`, `BindingKind`, `UpvalueDescriptor`, `FrameInfo`, `ResolveResult`, `ResolveDiagnostic`.
- Modify `src/syntax/mod.rs` — `pub mod resolve;` + a `resolve_source(src)` convenience.

**Helpers used throughout** (defined in Task 1): `ident_text(&SyntaxNode) -> Option<String>` (first IDENT token's text, via the resolver-backed tree) and `use_key(&SyntaxNode) -> TextRange` (the stable map key for a use site).

---

## Task 1: Types module + resolve scaffold

**Files:**
- Create: `src/syntax/resolve/types.rs`
- Create: `src/syntax/resolve/mod.rs`
- Modify: `src/syntax/mod.rs`

- [ ] **Step 1: Define the result types**

Create `src/syntax/resolve/types.rs`:

```rust
//! Data produced by name resolution. Keyed by source `TextRange` so any consumer
//! (compiler, checker) can look results up without holding the tree's node types.

use cstree::text::TextRange;
use std::collections::HashMap;

/// What an identifier use resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// A local in the current function frame, at `slot`.
    Local(u32),
    /// A captured variable, at `index` into the current closure's upvalues.
    Upvalue(u32),
    /// A free name resolved against globals/builtins at runtime.
    Global(String),
    /// Not found in any scope (a likely error; the checker reports it).
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
    /// A name introduced by a destructuring pattern or a match-arm binding.
    PatternBind,
    /// A `for` loop variable.
    LoopVar,
}

/// A declared name within some function frame.
#[derive(Debug, Clone)]
pub struct Binding {
    pub name: String,
    pub kind: BindingKind,
    /// Slot within its owning function frame.
    pub slot: u32,
    /// Range of the declaration's name token (for go-to-def / diagnostics).
    pub decl_range: TextRange,
    /// Captured by an inner closure → must be a heap cell (`Rc<RefCell<Value>>`).
    pub captured: bool,
    /// Reassigned somewhere after declaration. With `captured`, this forces a
    /// mutable cell; `captured && !mutated` is eligible for capture-by-value.
    pub mutated: bool,
    /// Number of read uses (for the checker's unused-binding lint).
    pub use_count: u32,
}

/// One upvalue a closure captures (clox model): either a local in the immediately
/// enclosing function, or an upvalue of that enclosing function (chained capture).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpvalueDescriptor {
    /// Capture the enclosing function's local at `slot`.
    ParentLocal(u32),
    /// Capture the enclosing function's upvalue at `index`.
    ParentUpvalue(u32),
}

/// The shape of one function's frame — what the compiler needs to emit code.
#[derive(Debug, Clone, Default)]
pub struct FrameInfo {
    /// Total local slots needed (params + locals across all nested blocks).
    pub slot_count: u32,
    /// Upvalues this function captures, in capture order (the `OP_CLOSURE` plan).
    pub upvalues: Vec<UpvalueDescriptor>,
    /// Slots that must be heap cells because they are captured-and-mutated.
    pub cell_slots: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct ResolveDiagnostic {
    pub message: String,
    pub range: TextRange,
}

/// The complete resolution result for one source file.
#[derive(Debug, Clone, Default)]
pub struct ResolveResult {
    /// use-site range → what it resolves to.
    pub uses: HashMap<TextRange, Resolution>,
    /// function-node range → its frame shape. The top-level (main) frame is keyed
    /// by the `SourceFile` node's range.
    pub frames: HashMap<TextRange, FrameInfo>,
    /// Unresolved references and other resolution errors (checker surfaces them).
    pub diagnostics: Vec<ResolveDiagnostic>,
}
```

- [ ] **Step 2: Scaffold the resolver + a global-resolution test**

Create `src/syntax/resolve/mod.rs`:

```rust
//! Name resolution over the typed CST. See types.rs for the produced data.

pub mod types;

use crate::syntax::cst::SyntaxNode;
use crate::syntax::kind::SyntaxKind;
use cstree::text::TextRange;
use types::*;

/// Resolve a parsed source file.
pub fn resolve(root: &SyntaxNode) -> ResolveResult {
    let mut r = Resolver::new();
    r.resolve_file(root);
    r.finish()
}

/// First IDENT token's text within `node` (via the resolver-backed tree).
pub fn ident_text(node: &SyntaxNode) -> Option<String> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

/// Stable map key for a use site: its text range.
pub fn use_key(node: &SyntaxNode) -> TextRange {
    node.text_range()
}

struct Resolver {
    result: ResolveResult,
    // scope/frame state added in later tasks
}

impl Resolver {
    fn new() -> Self {
        Resolver { result: ResolveResult::default() }
    }

    fn resolve_file(&mut self, _root: &SyntaxNode) {
        // Filled in across Tasks 2–7. For now: nothing (every name is Global).
    }

    fn finish(self) -> ResolveResult {
        self.result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse_to_tree;

    fn res(src: &str) -> ResolveResult {
        resolve(&parse_to_tree(src))
    }

    #[test]
    fn empty_program_resolves() {
        let r = res("");
        assert!(r.uses.is_empty());
        assert!(r.diagnostics.is_empty());
    }
}
```

- [ ] **Step 3: Wire the module**

In `src/syntax/mod.rs` add:

```rust
pub mod resolve;

/// Parse + resolve in one step (convenience for tests/tools).
pub fn resolve_source(src: &str) -> resolve::types::ResolveResult {
    resolve::resolve(&parse_to_tree(src))
}
```

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::resolve 2>&1 | tail -15`
Expected: `empty_program_resolves` PASS. (If `t.text()` needs an explicit resolver in cstree 0.14, the tree built by Plan 1/2 is resolver-backed — use the same text-access call those plans used; the alias may be `ResolvedNode`.)

```bash
git add src/syntax/resolve/ src/syntax/mod.rs
git commit -m "feat(resolve): result types + resolver scaffold"
```

---

## Task 2: Scopes, let/const bindings, local slot allocation & resolution

**Files:**
- Modify: `src/syntax/resolve/mod.rs`

- [ ] **Step 1: Write the local-resolution test**

Add to the `tests` mod:

```rust
    #[test]
    fn let_then_use_is_local() {
        // `let x = 1  print(x)` — the use of x resolves to Local(0); print is Global.
        let tree = parse_to_tree("let x = 1\nprint(x)");
        let r = resolve(&tree);
        // find the NameRef nodes and check resolutions
        let mut locals = 0;
        let mut globals = 0;
        for n in tree.descendants().filter(|n| n.kind() == SyntaxKind::NameRef) {
            match r.uses.get(&n.text_range()) {
                Some(Resolution::Local(_)) => locals += 1,
                Some(Resolution::Global(_)) => globals += 1,
                _ => {}
            }
        }
        assert_eq!(locals, 1, "x should be Local");
        assert_eq!(globals, 1, "print should be Global");
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::resolve::tests::let_then_use_is_local 2>&1 | tail -15`
Expected: FAIL — nothing is resolved yet.

- [ ] **Step 3: Implement scopes + a single (top-level) frame**

Replace the `Resolver` struct + impl in `src/syntax/resolve/mod.rs` with a scope/frame-bearing version. This task handles a single frame (the top level); functions come in Task 4.

```rust
struct Frame {
    /// Bindings by slot.
    bindings: Vec<Binding>,
    /// Next slot to allocate.
    next_slot: u32,
    /// Range identifying this frame (SourceFile or a function node).
    key: TextRange,
}

struct Scope {
    /// name → slot in the current frame.
    names: std::collections::HashMap<String, u32>,
}

struct Resolver {
    result: ResolveResult,
    frames: Vec<Frame>,
    scopes: Vec<Scope>,
}

impl Resolver {
    fn new() -> Self {
        Resolver {
            result: ResolveResult::default(),
            frames: Vec::new(),
            scopes: Vec::new(),
        }
    }

    fn frame(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("a frame is open")
    }

    fn push_scope(&mut self) {
        self.scopes.push(Scope { names: std::collections::HashMap::new() });
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Declare a binding in the current scope + frame, returning its slot.
    fn declare(&mut self, name: &str, kind: BindingKind, decl_range: TextRange) -> u32 {
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
        });
        self.scopes.last_mut().expect("a scope is open").names.insert(name.to_string(), slot);
        slot
    }

    /// Resolve a name to a slot in the CURRENT frame's open scopes (innermost
    /// first). Returns None if not a current-frame local.
    fn resolve_local(&self, name: &str) -> Option<u32> {
        for scope in self.scopes.iter().rev() {
            if let Some(&slot) = scope.names.get(name) {
                return Some(slot);
            }
        }
        None
    }

    fn resolve_file(&mut self, root: &SyntaxNode) {
        let key = root.text_range();
        self.frames.push(Frame { bindings: Vec::new(), next_slot: 0, key });
        self.push_scope();
        for child in root.children() {
            self.resolve_stmt(&child);
        }
        self.pop_scope();
        let frame = self.frames.pop().unwrap();
        self.result.frames.insert(
            frame.key,
            FrameInfo { slot_count: frame.next_slot, upvalues: Vec::new(), cell_slots: Vec::new() },
        );
    }

    fn resolve_stmt(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        match node.kind() {
            LetStmt => {
                // Resolve the initializer BEFORE declaring (so `let x = x` sees
                // the outer x), then declare the name.
                for child in node.children() {
                    if is_expr(child.kind()) {
                        self.resolve_expr(&child);
                    }
                }
                if let Some(name) = ident_text(node) {
                    self.declare(&name, BindingKind::Let, node.text_range());
                }
            }
            ExprStmt | Block | IfStmt | WhileStmt | ReturnStmt => {
                for child in node.children() {
                    if is_expr(child.kind()) {
                        self.resolve_expr(&child);
                    } else {
                        self.resolve_stmt(&child);
                    }
                }
            }
            _ => {
                for child in node.children() {
                    self.resolve_stmt(&child);
                }
            }
        }
    }

    fn resolve_expr(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        match node.kind() {
            NameRef => {
                let name = ident_text(node).unwrap_or_default();
                let resolution = match self.resolve_local(&name) {
                    Some(slot) => {
                        self.bump_use(slot);
                        Resolution::Local(slot)
                    }
                    None => Resolution::Global(name),
                };
                self.result.uses.insert(node.text_range(), resolution);
            }
            _ => {
                for child in node.children() {
                    self.resolve_expr(&child);
                }
            }
        }
    }

    /// Increment the read-use count of a current-frame local by slot.
    fn bump_use(&mut self, slot: u32) {
        if let Some(b) = self.frame().bindings.iter_mut().find(|b| b.slot == slot) {
            b.use_count += 1;
        }
    }

    fn finish(self) -> ResolveResult {
        self.result
    }
}

/// True if a node kind is an expression (so the walker recurses via resolve_expr).
fn is_expr(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        Literal | NameRef | UnaryExpr | BinaryExpr | ParenExpr | CallExpr | ArgList
            | MemberExpr | IndexExpr | ArrowExpr | AssignExpr | ArrayExpr | ObjectExpr
            | TemplateExpr | OptMemberExpr | TryExpr | UnwrapExpr | TernaryExpr
            | AwaitExpr | YieldExpr | MatchExpr | RangeExpr
    )
}
```

> `is_expr` lets the statement walker dispatch children to the right resolver. `MemberExpr`/`OptMemberExpr` property names are NOT name references (they're keys), so `resolve_expr`'s default recursion over `NameRef` only catches real variable uses — the member name token isn't a `NameRef` node.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::resolve 2>&1 | tail -15`
Expected: PASS.

```bash
git add src/syntax/resolve/mod.rs
git commit -m "feat(resolve): scopes, let/const bindings, local slot allocation"
```

---

## Task 3: Block scoping & shadowing

**Files:**
- Modify: `src/syntax/resolve/mod.rs`

- [ ] **Step 1: Tests**

Add to the `tests` mod:

```rust
    #[test]
    fn block_scoped_binding_does_not_leak() {
        // x declared inside the block; the outer use of x is Global (undefined
        // outside) — proves block scope pop.
        let tree = parse_to_tree("{ let x = 1\n print(x) }\nprint(x)");
        let r = resolve(&tree);
        let refs: Vec<_> = tree
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::NameRef && ident_text(n).as_deref() == Some("x"))
            .map(|n| r.uses.get(&n.text_range()).cloned())
            .collect();
        assert_eq!(refs[0], Some(Resolution::Local(0)), "inner x is Local");
        assert_eq!(refs[1], Some(Resolution::Global("x".into())), "outer x is Global");
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::resolve::tests::block_scoped_binding_does_not_leak 2>&1 | tail -15`
Expected: FAIL — blocks don't push/pop a scope yet.

- [ ] **Step 3: Push/pop a scope for blocks**

In `resolve_stmt`, give `Block` its own scope. Change the `Block` handling — split it out of the shared `ExprStmt | Block | ...` arm:

```rust
            Block => {
                self.push_scope();
                for child in node.children() {
                    self.resolve_stmt(&child);
                }
                self.pop_scope();
            }
            IfStmt | WhileStmt => {
                // condition is an expr; the branch Blocks open their own scopes.
                for child in node.children() {
                    if is_expr(child.kind()) {
                        self.resolve_expr(&child);
                    } else {
                        self.resolve_stmt(&child);
                    }
                }
            }
            ExprStmt | ReturnStmt => {
                for child in node.children() {
                    if is_expr(child.kind()) {
                        self.resolve_expr(&child);
                    }
                }
            }
```

> Slots are allocated per *frame*, not per scope (a block's locals still consume frame slots — simplest correct model; slot reuse across sibling blocks is an optional later optimization). Popping a scope only removes name visibility, not the slot.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::resolve 2>&1 | tail -15`
Expected: PASS.

```bash
git add src/syntax/resolve/mod.rs
git commit -m "feat(resolve): block scoping (names pop, slots persist per frame)"
```

---

## Task 4: Function frames, params, and upvalue capture

**Files:**
- Modify: `src/syntax/resolve/mod.rs`

- [ ] **Step 1: Tests**

Add to the `tests` mod:

```rust
    #[test]
    fn params_are_locals_in_their_frame() {
        let tree = parse_to_tree("fn add(a, b) { return a + b }");
        let r = resolve(&tree);
        let mut local_uses = 0;
        for n in tree.descendants().filter(|n| n.kind() == SyntaxKind::NameRef) {
            if matches!(r.uses.get(&n.text_range()), Some(Resolution::Local(_))) {
                local_uses += 1;
            }
        }
        assert_eq!(local_uses, 2, "a and b resolve to locals");
    }

    #[test]
    fn inner_closure_captures_outer_as_upvalue() {
        // `fn outer() { let x = 1  fn inner() { return x } }`
        // inside inner, x is an Upvalue; outer's x binding becomes captured.
        let tree = parse_to_tree("fn outer() {\n let x = 1\n fn inner() { return x }\n}");
        let r = resolve(&tree);
        let x_use = tree
            .descendants()
            .find(|n| n.kind() == SyntaxKind::NameRef && ident_text(n).as_deref() == Some("x"))
            .unwrap();
        assert!(matches!(r.uses.get(&x_use.text_range()), Some(Resolution::Upvalue(0))));
        // inner's frame has one upvalue capturing outer's local 0.
        let inner = tree.descendants().find(|n| n.kind() == SyntaxKind::FnDecl
            && ident_text(n).as_deref() == Some("inner")).unwrap();
        let fi = r.frames.get(&inner.text_range()).expect("inner frame");
        assert_eq!(fi.upvalues, vec![UpvalueDescriptor::ParentLocal(0)]);
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::resolve::tests::inner_closure_captures_outer_as_upvalue 2>&1 | tail -20`
Expected: FAIL — functions don't open frames; capture not modeled.

- [ ] **Step 3: Add function frames + the clox upvalue algorithm**

Add per-frame upvalue tracking and function handling. First extend `Frame`:

```rust
struct Frame {
    bindings: Vec<Binding>,
    next_slot: u32,
    key: TextRange,
    /// Upvalues captured by THIS frame, in capture order.
    upvalues: Vec<UpvalueDescriptor>,
    /// Index, within `scopes`, of the first scope belonging to this frame.
    scope_base: usize,
}
```

Update `frame()`/`resolve_file` to set `upvalues: Vec::new()` and `scope_base`, and `resolve_local` to only search scopes of the **current** frame (`self.scopes[scope_base..]`). Replace `resolve_local` and add the upvalue resolver:

```rust
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

    /// Resolve `name` as an upvalue of frame `frame_idx`, recursively capturing
    /// through enclosing frames (clox algorithm). Marks captured locals.
    fn resolve_upvalue(&mut self, frame_idx: usize, name: &str) -> Option<u32> {
        if frame_idx == 0 {
            return None; // no enclosing function frame
        }
        let parent = frame_idx - 1;
        if let Some(slot) = self.resolve_local_in(parent, name) {
            self.mark_captured(parent, slot);
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
            return i as u32; // dedup
        }
        ups.push(desc);
        (ups.len() - 1) as u32
    }

    fn mark_captured(&mut self, frame_idx: usize, slot: u32) {
        if let Some(b) = self.frames[frame_idx].bindings.iter_mut().find(|b| b.slot == slot) {
            b.captured = true;
        }
    }
```

Update `NameRef` resolution in `resolve_expr` to try local → upvalue → global:

```rust
            NameRef => {
                let name = ident_text(node).unwrap_or_default();
                let resolution = if let Some(slot) = self.resolve_local(&name) {
                    self.bump_use(slot);
                    Resolution::Local(slot)
                } else if let Some(idx) = self.resolve_upvalue(self.frames.len() - 1, &name) {
                    Resolution::Upvalue(idx)
                } else {
                    Resolution::Global(name)
                };
                self.result.uses.insert(node.text_range(), resolution);
            }
```

Add `FnDecl` handling in `resolve_stmt` (open a new frame; params are locals):

```rust
            FnDecl => {
                // The function NAME binds in the enclosing frame.
                if let Some(name) = fn_name(node) {
                    self.declare(&name, BindingKind::Fn, node.text_range());
                }
                self.resolve_function(node);
            }
```

Add the function resolver + helpers:

```rust
    fn resolve_function(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        let key = node.text_range();
        self.frames.push(Frame {
            bindings: Vec::new(),
            next_slot: 0,
            key,
            upvalues: Vec::new(),
            scope_base: self.scopes.len(),
        });
        self.push_scope();
        // params
        if let Some(params) = node.children().find(|c| c.kind() == ParamList) {
            for p in params.children().filter(|c| c.kind() == Param) {
                if let Some(name) = ident_text(&p) {
                    self.declare(&name, BindingKind::Param, p.text_range());
                }
            }
        }
        // body
        if let Some(body) = node.children().find(|c| c.kind() == Block) {
            for child in body.children() {
                self.resolve_stmt(&child);
            }
        }
        self.pop_scope();
        let frame = self.frames.pop().unwrap();
        self.result.frames.insert(
            frame.key,
            FrameInfo {
                slot_count: frame.next_slot,
                upvalues: frame.upvalues,
                cell_slots: frame
                    .bindings
                    .iter()
                    .filter(|b| b.captured && b.mutated)
                    .map(|b| b.slot)
                    .collect(),
            },
        );
    }
```

```rust
/// The declared name of a function (the IDENT after `fn`/`async`/`*`).
fn fn_name(node: &SyntaxNode) -> Option<String> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}
```

Also update `resolve_file` to set `upvalues`/`scope_base` on the top frame, and have `resolve_stmt`'s `ArrowExpr` (an expression) route through `resolve_function` — handle arrows inside `resolve_expr`:

```rust
            ArrowExpr => self.resolve_function(node),
```

> `fn_name` and `ident_text` coincide for `FnDecl` (first IDENT is the name). For arrows there is no name; `resolve_function` reads params + body regardless.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::resolve 2>&1 | tail -20`
Expected: PASS.

```bash
git add src/syntax/resolve/mod.rs
git commit -m "feat(resolve): function frames, params, clox-style upvalue capture"
```

---

## Task 5: Reassignment tracking → capture-by-value eligibility

**Files:**
- Modify: `src/syntax/resolve/mod.rs`

- [ ] **Step 1: Test**

Add to the `tests` mod:

```rust
    #[test]
    fn captured_immutable_is_not_a_cell() {
        // x is captured but never reassigned → NOT in cell_slots (capture-by-value).
        let immut = parse_to_tree("fn o() {\n let x = 1\n fn i() { return x }\n}");
        let r1 = resolve(&immut);
        let oi = immut.descendants().find(|n| n.kind() == SyntaxKind::FnDecl
            && ident_text(n).as_deref() == Some("o")).unwrap();
        assert!(r1.frames.get(&oi.text_range()).unwrap().cell_slots.is_empty());

        // y is captured AND reassigned → IS a cell.
        let mut_ = parse_to_tree("fn o() {\n let y = 1\n fn i() { y = 2 }\n}");
        let r2 = resolve(&mut_);
        let oi2 = mut_.descendants().find(|n| n.kind() == SyntaxKind::FnDecl
            && ident_text(n).as_deref() == Some("o")).unwrap();
        assert_eq!(r2.frames.get(&oi2.text_range()).unwrap().cell_slots, vec![0]);
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::resolve::tests::captured_immutable_is_not_a_cell 2>&1 | tail -20`
Expected: FAIL — reassignment isn't tracked, so `y` isn't marked `mutated`.

- [ ] **Step 3: Mark `mutated` on assignment targets**

In `resolve_expr`, handle `AssignExpr` by marking the target binding mutated (in whichever frame owns it). Add an arm:

```rust
            AssignExpr => {
                // First child is the target; if it's a bare NameRef, mark mutated.
                let mut children = node.children();
                if let Some(target) = children.next() {
                    if target.kind() == NameRef {
                        let name = ident_text(&target).unwrap_or_default();
                        self.mark_mutated(&name);
                    }
                    self.resolve_expr(&target);
                }
                for rest in children {
                    self.resolve_expr(&rest);
                }
            }
```

Add `mark_mutated`, which searches frames innermost-out and sets `mutated` on the binding (so a captured-then-mutated outer var is flagged):

```rust
    fn mark_mutated(&mut self, name: &str) {
        for fi in (0..self.frames.len()).rev() {
            if let Some(slot) = self.resolve_local_in(fi, name) {
                if let Some(b) = self.frames[fi].bindings.iter_mut().find(|b| b.slot == slot) {
                    b.mutated = true;
                }
                return;
            }
        }
    }
```

> `cell_slots` is computed in `resolve_function`/`resolve_file` as `captured && mutated`. Because assignment marks `mutated` on the *owning* frame's binding even from an inner frame, a variable captured and then reassigned by the inner closure is correctly flagged as a cell.

Also apply the same `cell_slots` computation to the top-level frame in `resolve_file` (replace its `FrameInfo` construction to mirror `resolve_function`'s `cell_slots`).

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::resolve 2>&1 | tail -20`
Expected: PASS.

```bash
git add src/syntax/resolve/mod.rs
git commit -m "feat(resolve): reassignment tracking + capture-by-value (cell_slots)"
```

---

## Task 6: Destructuring, `for` vars, class/enum/import bindings

**Files:**
- Modify: `src/syntax/resolve/mod.rs`

- [ ] **Step 1: Tests**

Add to the `tests` mod:

```rust
    #[test]
    fn destructuring_binds_all_names() {
        let tree = parse_to_tree("let [a, b, ...rest] = xs\nprint(a)\nprint(rest)");
        let r = resolve(&tree);
        let locals = tree.descendants()
            .filter(|n| n.kind() == SyntaxKind::NameRef)
            .filter(|n| matches!(r.uses.get(&n.text_range()), Some(Resolution::Local(_))))
            .count();
        assert_eq!(locals, 2, "a and rest are locals (xs/print are not)");
    }

    #[test]
    fn for_var_and_class_enum_bind() {
        assert!(resolve(&parse_to_tree("for (i in 0..3) { print(i) }"))
            .uses.values().any(|r| matches!(r, Resolution::Local(_))));
        // class/enum names bind in the enclosing frame
        let r = resolve(&parse_to_tree("class C {}\nlet x = C"));
        let cuse = parse_to_tree("class C {}\nlet x = C");
        let _ = cuse;
        assert!(r.uses.values().any(|u| matches!(u, Resolution::Local(_))),
            "C resolves to a local binding");
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::resolve::tests::destructuring_binds_all_names 2>&1 | tail -15`
Expected: FAIL.

- [ ] **Step 3: Handle the remaining binding forms**

Extend `resolve_stmt`. For `LetStmt`, the binding may be a pattern; declare every name in `ArrayBindPat`/`ObjectBindPat`. Replace the `LetStmt` arm:

```rust
            LetStmt => {
                for child in node.children() {
                    if is_expr(child.kind()) {
                        self.resolve_expr(&child); // initializer
                    }
                }
                self.declare_let_bindings(node);
            }
```

Add binding-declaration helpers + the new statement arms:

```rust
            ForStmt => {
                self.push_scope();
                // resolve the iterable/range expressions
                for child in node.children() {
                    if is_expr(child.kind()) {
                        self.resolve_expr(&child);
                    }
                }
                // the loop variable is the first IDENT token in the header
                if let Some(name) = ident_text(node) {
                    self.declare(&name, BindingKind::LoopVar, node.text_range());
                }
                if let Some(body) = node.children().find(|c| c.kind() == Block) {
                    for s in body.children() {
                        self.resolve_stmt(&s);
                    }
                }
                self.pop_scope();
            }
            EnumDecl => {
                if let Some(name) = ident_text(node) {
                    self.declare(&name, BindingKind::Enum, node.text_range());
                }
                // enum variant values are exprs in the class-def scope
                for v in node.descendants().filter(|n| n.kind() == EnumVariant) {
                    for e in v.children().filter(|c| is_expr(c.kind())) {
                        self.resolve_expr(&e);
                    }
                }
            }
            ClassDecl => {
                if let Some(name) = ident_text(node) {
                    self.declare(&name, BindingKind::Class, node.text_range());
                }
                self.resolve_class(node);
            }
            ImportStmt => {
                self.declare_import_bindings(node);
            }
            ExportStmt => {
                for child in node.children() {
                    self.resolve_stmt(&child);
                }
            }
            BreakStmt | ContinueStmt => {}
```

```rust
    fn declare_let_bindings(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        // plain `let name`
        if let Some(arr) = node.children().find(|c| c.kind() == ArrayBindPat) {
            self.declare_pattern_names(&arr);
        } else if let Some(obj) = node.children().find(|c| c.kind() == ObjectBindPat) {
            self.declare_pattern_names(&obj);
        } else if let Some(name) = ident_text(node) {
            self.declare(&name, BindingKind::Let, node.text_range());
        }
    }

    /// Declare every name introduced by a binding pattern (BindEntry's local /
    /// key, RestBind's name).
    fn declare_pattern_names(&mut self, pat: &SyntaxNode) {
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
                        self.declare(&name, BindingKind::PatternBind, entry.text_range());
                    }
                }
                RestBind => {
                    if let Some(name) = ident_text(&entry) {
                        self.declare(&name, BindingKind::PatternBind, entry.text_range());
                    }
                }
                _ => {}
            }
        }
    }

    fn declare_import_bindings(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        if let Some(list) = node.children().find(|c| c.kind() == ImportList) {
            for t in list.children_with_tokens().filter_map(|el| el.into_token()) {
                if t.kind() == Ident {
                    self.declare(&t.text().to_string(), BindingKind::Import, node.text_range());
                }
            }
        } else {
            // namespace import `* as alias` — alias is the last IDENT token.
            if let Some(alias) = node
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .filter(|t| t.kind() == Ident)
                .last()
                .map(|t| t.text().to_string())
            {
                self.declare(&alias, BindingKind::Import, node.text_range());
            }
        }
    }

    fn resolve_class(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        // Field defaults + method bodies. Methods are functions (own frames);
        // `self`/`super` resolve as Global (the VM injects them) — fine here.
        for member in node.children() {
            match member.kind() {
                FieldDecl => {
                    for e in member.children().filter(|c| is_expr(c.kind())) {
                        self.resolve_expr(&e);
                    }
                }
                MethodDecl => self.resolve_function(&member),
                _ => {}
            }
        }
    }
```

> Note: `ident_text(node)` for `ForStmt` returns the first IDENT in the header, which is the loop variable (the `for`/`await`/`(` are keywords/punct, and `in`/`of` come after the variable). For `ClassDecl`/`EnumDecl` the first IDENT is the declared type name. Verified by the tests.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::resolve 2>&1 | tail -20`
Expected: PASS.

```bash
git add src/syntax/resolve/mod.rs
git commit -m "feat(resolve): destructuring, for vars, class/enum/import bindings"
```

---

## Task 7: Match-arm scopes + Option-C ident resolution

**Files:**
- Modify: `src/syntax/resolve/mod.rs`

- [ ] **Step 1: Tests**

Add to the `tests` mod:

```rust
    #[test]
    fn match_binds_undefined_compares_defined() {
        // Option-C: in `match v { other => other }`, `other` is undefined → binds,
        // and the body use of `other` resolves to that Local.
        let tree = parse_to_tree("let v = 1\nlet r = match v { other => other }");
        let r = resolve(&tree);
        let body_use = tree.descendants()
            .filter(|n| n.kind() == SyntaxKind::NameRef && ident_text(n).as_deref() == Some("other"))
            .last()
            .unwrap();
        assert!(matches!(r.uses.get(&body_use.text_range()), Some(Resolution::Local(_))),
            "bound pattern name is a Local in the arm body");
    }

    #[test]
    fn match_arm_bindings_dont_leak() {
        let tree = parse_to_tree("let r = match v { x => x }\nprint(x)");
        let r = resolve(&tree);
        let outer_x = tree.descendants()
            .filter(|n| n.kind() == SyntaxKind::NameRef && ident_text(n).as_deref() == Some("x"))
            .last().unwrap();
        assert_eq!(r.uses.get(&outer_x.text_range()), Some(&Resolution::Global("x".into())));
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::resolve::tests::match_binds_undefined_compares_defined 2>&1 | tail -20`
Expected: FAIL — match arms/patterns not handled.

- [ ] **Step 3: Resolve `MatchExpr` with per-arm scopes + Option-C**

Add a `MatchExpr` arm to `resolve_expr`:

```rust
            MatchExpr => {
                // subject
                for child in node.children().filter(|c| is_expr(c.kind())) {
                    self.resolve_expr(&child);
                    break; // only the subject; arms handled below
                }
                for arm in node.children().filter(|c| c.kind() == MatchArm) {
                    self.resolve_match_arm(&arm);
                }
            }
```

Add the arm resolver. Each arm gets its own scope; pattern bindings (Option-C: a bare-ident `LiteralPat` whose name is NOT already resolvable → bind; otherwise it's a value compare) are declared, then the guard and body resolve in that scope:

```rust
    fn resolve_match_arm(&mut self, arm: &SyntaxNode) {
        use SyntaxKind::*;
        self.push_scope();
        // Patterns: declare bindings; resolve value/range pattern expressions.
        for pat in arm.children().filter(|c| is_pattern(c.kind())) {
            self.resolve_pattern(&pat);
        }
        // Guard + body (resolve in arm scope so bindings are visible).
        for child in arm.children() {
            match child.kind() {
                MatchGuard => {
                    for e in child.children().filter(|c| is_expr(c.kind())) {
                        self.resolve_expr(&e);
                    }
                }
                k if is_expr(k) => self.resolve_expr(&child), // arm body
                _ => {}
            }
        }
        self.pop_scope();
    }

    /// Option-C: a bare-ident LiteralPat that is NOT already resolvable binds the
    /// subject; an ident that IS resolvable (a defined name / enum variant) is a
    /// value compare. Nested array/object patterns recurse; ranges/values resolve
    /// their expressions.
    fn resolve_pattern(&mut self, pat: &SyntaxNode) {
        use SyntaxKind::*;
        match pat.kind() {
            WildcardPat => {}
            LiteralPat => {
                // Is the whole pattern a single bare NameRef?
                if let Some(name) = bare_ident_pattern(pat) {
                    if self.resolve_local(&name).is_none()
                        && self.resolve_upvalue(self.frames.len() - 1, &name).is_none()
                        && !self.is_global_enum_like(&name)
                    {
                        // undefined → bind
                        self.declare(&name, BindingKind::PatternBind, pat.text_range());
                    } else {
                        // defined → value compare (resolve as a use)
                        for e in pat.children().filter(|c| is_expr(c.kind())) {
                            self.resolve_expr(&e);
                        }
                    }
                } else {
                    for e in pat.children().filter(|c| is_expr(c.kind())) {
                        self.resolve_expr(&e);
                    }
                }
            }
            RangePat => {
                for e in pat.children().filter(|c| is_expr(c.kind())) {
                    self.resolve_expr(&e);
                }
            }
            ArrayPat | ObjectPat => {
                for sub in pat.children() {
                    match sub.kind() {
                        PatRest => {
                            if let Some(name) = ident_text(&sub) {
                                self.declare(&name, BindingKind::PatternBind, sub.text_range());
                            }
                        }
                        ObjPatEntry => {
                            // `{key}` binds key; `{key: subpat}` recurses.
                            if let Some(subpat) = sub.children().find(|c| is_pattern(c.kind())) {
                                self.resolve_pattern(&subpat);
                            } else if let Some(name) = ident_text(&sub) {
                                self.declare(&name, BindingKind::PatternBind, sub.text_range());
                            }
                        }
                        k if is_pattern(k) => self.resolve_pattern(&sub),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// Heuristic for Option-C: treat an all-caps / enum-like global name as a
    /// value compare rather than a binding even when not locally resolvable.
    /// (Enum variants are module/global values; the runtime confirms at match
    /// time. Conservative: only names already in scope are definitely compares;
    /// this lets the checker flag genuinely-unknown compares later.)
    fn is_global_enum_like(&self, _name: &str) -> bool {
        false
    }
```

Add the pattern helpers:

```rust
/// True if a node kind is a match pattern.
fn is_pattern(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        WildcardPat | IdentPat | LiteralPat | RangePat | ArrayPat | ObjectPat | OrPat
    )
}

/// If a LiteralPat is exactly a single bare `NameRef`, return its name.
fn bare_ident_pattern(pat: &SyntaxNode) -> Option<String> {
    let mut exprs = pat.children().filter(|c| is_expr(c.kind()));
    let first = exprs.next()?;
    if exprs.next().is_some() {
        return None;
    }
    if first.kind() == SyntaxKind::NameRef {
        ident_text(&first)
    } else {
        None
    }
}
```

> Option-C nuance: the *correct* runtime rule is "defined-in-scope name → compare; undefined → bind." This pass resolves the lexical part of that (locals/upvalues). Enum-variant compares (module globals like `NOT_FOUND`) are resolved at runtime by the VM; `is_global_enum_like` is a deliberate `false` stub so such names *bind* lexically here, and the checker/runtime refine it. This matches the AST's `Pattern::Ident` (Option-C resolved at match time) — the resolver provides the lexical slot for genuine bindings and leaves runtime-decidable cases to the runtime.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::resolve 2>&1 | tail -20`
Expected: PASS.

```bash
git add src/syntax/resolve/mod.rs
git commit -m "feat(resolve): match-arm scopes + Option-C pattern bindings"
```

---

## Task 8: Unresolved-reference diagnostics + corpus smoke test

**Files:**
- Modify: `src/syntax/resolve/mod.rs`
- Create: `tests/cst_resolve.rs`

- [ ] **Step 1: Tests**

Add to the `tests` mod in `mod.rs`:

```rust
    #[test]
    fn builtins_are_not_flagged_unresolved() {
        // print/len are builtins → Global, not diagnostics.
        let r = resolve(&parse_to_tree("print(len([1,2]))"));
        assert!(r.diagnostics.is_empty(), "builtins must not be flagged: {:?}", r.diagnostics);
    }
```

Create `tests/cst_resolve.rs`:

```rust
//! Smoke test: resolution runs over the whole example corpus without panicking
//! and produces a frame for every function. (Correctness of individual
//! resolutions is unit-tested in src/syntax/resolve.)

use std::fs;
use std::path::{Path, PathBuf};

fn corpus() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() { walk(&p, out); }
            else if p.extension().and_then(|x| x.to_str()) == Some("as") { out.push(p); }
        }
    }
    let mut v = Vec::new();
    walk(Path::new("examples"), &mut v);
    v.sort();
    v
}

#[test]
fn resolve_runs_over_corpus() {
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        let r = ascript::syntax::resolve_source(&src);
        // At minimum, the top-level frame exists.
        assert!(!r.frames.is_empty(), "no frames for {}", path.display());
    }
}
```

- [ ] **Step 2: Run (expect failure on the builtin test if needed)**

Run: `cargo test --lib syntax::resolve::tests::builtins_are_not_flagged_unresolved 2>&1 | tail -10`
Expected: PASS (the resolver currently records *no* diagnostics — it classifies frees as `Global`, never `Unresolved`). This test pins that builtins/globals are not mistaken for errors.

- [ ] **Step 3: (Design note — keep `Unresolved` for the checker)**

Resolution deliberately classifies any non-local/non-upvalue name as `Global` rather than `Unresolved`, because AScript resolves builtins and imported/module globals at runtime — the resolver cannot know the full global set. Distinguishing "genuinely undefined" from "valid runtime global" is the **checker's** job (it has the builtin list + import analysis). So Plan 3 leaves `diagnostics` empty for name resolution; the `Unresolved` variant + `diagnostics` field exist for the checker to populate later. No code change needed for this step — it documents the boundary.

- [ ] **Step 4: Run corpus + full suite + clippy both configs**

Run: `cargo test --test cst_resolve 2>&1 | tail -10`
Expected: PASS.
Run: `cargo test 2>&1 | tail -15`
Expected: green.
Run: `cargo clippy --all-targets 2>&1 | tail -5 && cargo clippy --no-default-features --all-targets 2>&1 | tail -5`
Expected: clean both.

- [ ] **Step 5: Commit**

```bash
git add src/syntax/resolve/mod.rs tests/cst_resolve.rs
git commit -m "feat(resolve): corpus smoke test + Global/Unresolved boundary for the checker"
```

---

## Done criteria for Plan 3

- [ ] `cargo test` green; `cargo clippy` clean in both feature configs.
- [ ] Every variable use resolves to `Local(slot)` / `Upvalue(index)` / `Global(name)`; frames carry `slot_count`, the upvalue plan, and `cell_slots`.
- [ ] Block/function/loop/match-arm scoping is correct (no leaks); shadowing works.
- [ ] Upvalue capture is clox-correct (chained `ParentLocal`/`ParentUpvalue`, deduped); capture-by-value vs cell is decided by `captured && mutated`.
- [ ] All binding forms covered: let/const, params (incl. rest), destructuring (array/object + rest + `as`), `for` vars, fn/class/enum names, imports, match-arm Option-C bindings.
- [ ] Resolution runs over the whole corpus without panic.
- [ ] The interpreter and binary remain unchanged.

**Next plan:** `cst-comment-preserving-formatter.md` (Plan 4) — rewrite `fmt` to walk the CST and re-emit trivia: canonical layout (whitespace/`;`→newline/fields-before-methods/`name?: T`→`name: T?`/quote escaping) **threading comments through reordering by node attachment**, the blank-line rule (1 significant, 2+ collapse), the enumerated comment edge-cases, and idempotence — the user-visible deliverable and the CST acceptance gate. (The resolver from this plan is not needed by the formatter; it's consumed by the bytecode compiler and, later, the checker.)
