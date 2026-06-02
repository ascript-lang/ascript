//! Name resolution over the typed CST. See types.rs for the produced data.

pub mod types;

use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use cstree::text::TextRange;
use std::collections::HashMap;
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

    fn declare(&mut self, name: &str, kind: BindingKind, decl_range: TextRange) -> u32 {
        let shadows = self.resolve_local(name).and_then(|outer_slot| {
            self.frame_ref()
                .bindings
                .iter()
                .find(|b| b.slot == outer_slot)
                .map(|b| b.decl_range)
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
        });
        self.scopes
            .last_mut()
            .expect("a scope is open")
            .names
            .insert(name.to_string(), slot);
        slot
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

    fn mark_captured(&mut self, frame_idx: usize, slot: u32) {
        if let Some(b) = self.frames[frame_idx]
            .bindings
            .iter_mut()
            .find(|b| b.slot == slot)
        {
            b.captured = true;
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
        for child in root.children() {
            self.resolve_stmt(child);
        }
        self.pop_scope();
        let frame = self.frames.pop().unwrap();
        let cell_slots: Vec<u32> = frame
            .bindings
            .iter()
            .filter(|b| b.captured && b.mutated)
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
                for child in node.children() {
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
                if let Some(name) = fn_name(node) {
                    self.declare(&name, BindingKind::Fn, node.text_range());
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
                    self.declare(&name, BindingKind::Enum, node.text_range());
                }
                for v in node.descendants().filter(|n| n.kind() == EnumVariant) {
                    for e in v.children().filter(|c| is_expr(c.kind())) {
                        self.resolve_expr(e);
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
        if let Some(arr) = node.children().find(|c| c.kind() == ArrayBindPat) {
            self.declare_pattern_names(arr);
        } else if let Some(obj) = node.children().find(|c| c.kind() == ObjectBindPat) {
            self.declare_pattern_names(obj);
        } else if let Some(name) = ident_text(node) {
            self.declare(&name, BindingKind::Let, node.text_range());
        }
    }

    /// Declare every name introduced by a binding pattern (BindEntry's local/key,
    /// RestBind's name).
    fn declare_pattern_names(&mut self, pat: &ResolvedNode) {
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
                    if let Some(name) = ident_text(entry) {
                        self.declare(&name, BindingKind::PatternBind, entry.text_range());
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
                    self.declare(t.text(), BindingKind::Import, node.text_range());
                }
            }
        } else if let Some(alias) = node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|t| t.kind() == Ident)
            .last()
            .map(|t| t.text().to_string())
        {
            self.declare(&alias, BindingKind::Import, node.text_range());
        }
    }

    fn resolve_class(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        for member in node.children() {
            match member.kind() {
                FieldDecl => {
                    for e in member.children().filter(|c| is_expr(c.kind())) {
                        self.resolve_expr(e);
                    }
                }
                MethodDecl => self.resolve_function(member),
                _ => {}
            }
        }
    }

    fn resolve_expr(&mut self, node: &ResolvedNode) {
        use SyntaxKind::*;
        match node.kind() {
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
            ArrowExpr => {
                self.resolve_function(node);
            }
            AssignExpr => {
                let mut children = node.children();
                if let Some(target) = children.next() {
                    if target.kind() == NameRef {
                        let name = ident_text(target).unwrap_or_default();
                        self.mark_mutated(&name);
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

    fn bump_use(&mut self, slot: u32) {
        if let Some(b) = self
            .frame()
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
        if let Some(params) = node.children().find(|c| c.kind() == ParamList) {
            for p in params.children().filter(|c| c.kind() == Param) {
                if let Some(name) = ident_text(p) {
                    self.declare(&name, BindingKind::Param, p.text_range());
                }
            }
        }
        if let Some(body) = node.children().find(|c| c.kind() == Block) {
            for child in body.children() {
                self.resolve_stmt(child);
            }
        }
        self.pop_scope();
        let frame = self.frames.pop().unwrap();
        let cell_slots: Vec<u32> = frame
            .bindings
            .iter()
            .filter(|b| b.captured && b.mutated)
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

    fn finish(self) -> ResolveResult {
        self.result
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
    fn empty_program_resolves() {
        let r = res("");
        assert!(r.uses.is_empty());
        assert!(r.diagnostics.is_empty());
    }

    #[test]
    fn let_then_use_is_local() {
        let tree = parse_to_tree("let x = 1\nprint(x)");
        let r = resolve(&tree);
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

    #[test]
    fn block_scoped_binding_does_not_leak() {
        // x declared inside the block; the outer use of x is Global (undefined
        // outside) — proves block scope pop. AScript blocks: `{ ... }`.
        let tree = parse_to_tree("{ let x = 1\n print(x) }\nprint(x)");
        let r = resolve(&tree);
        let refs: Vec<_> = tree
            .descendants()
            .filter(|n: &&ResolvedNode| n.kind() == SyntaxKind::NameRef && ident_text(n).as_deref() == Some("x"))
            .map(|n| r.uses.get(&n.text_range()).cloned())
            .collect();
        assert_eq!(refs[0], Some(Resolution::Local(0)), "inner x is Local");
        assert_eq!(refs[1], Some(Resolution::Global("x".into())), "outer x is Global");
    }

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
        let tree = parse_to_tree("fn outer() {\n let x = 1\n fn inner() { return x }\n}");
        let r = resolve(&tree);
        let x_use = tree
            .descendants()
            .find(|n| n.kind() == SyntaxKind::NameRef && ident_text(n).as_deref() == Some("x"))
            .unwrap();
        assert!(matches!(r.uses.get(&x_use.text_range()), Some(Resolution::Upvalue(0))));
        let inner = tree
            .descendants()
            .find(|n| {
                n.kind() == SyntaxKind::FnDecl && ident_text(n).as_deref() == Some("inner")
            })
            .unwrap();
        let fi = r
            .frames
            .get(&(SyntaxKind::FnDecl, inner.text_range()))
            .expect("inner frame");
        assert_eq!(fi.upvalues, vec![UpvalueDescriptor::ParentLocal(0)]);
    }

    #[test]
    fn destructuring_binds_all_names() {
        let tree = parse_to_tree("let [a, b, ...rest] = xs\nprint(a)\nprint(rest)");
        let r = resolve(&tree);
        let locals = tree
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::NameRef)
            .filter(|n| matches!(r.uses.get(&n.text_range()), Some(Resolution::Local(_))))
            .count();
        assert_eq!(locals, 2, "a and rest are locals (xs/print are not)");
    }

    #[test]
    fn for_var_and_class_enum_bind() {
        let r1 = resolve(&parse_to_tree("for (i in 0..3) { print(i) }"));
        assert!(
            r1.uses.values().any(|r| matches!(r, Resolution::Local(_))),
            "i is a local"
        );
        let r = resolve(&parse_to_tree("class C {}\nlet x = C"));
        assert!(
            r.uses.values().any(|u| matches!(u, Resolution::Local(_))),
            "C resolves to a local binding"
        );
    }

    #[test]
    fn captured_immutable_is_not_a_cell() {
        // x captured but never reassigned → NOT a cell.
        let immut = parse_to_tree("fn o() {\n let x = 1\n fn i() { return x }\n}");
        let r1 = resolve(&immut);
        let oi = immut
            .descendants()
            .find(|n| {
                n.kind() == SyntaxKind::FnDecl && ident_text(n).as_deref() == Some("o")
            })
            .unwrap();
        assert!(r1
            .frames
            .get(&(SyntaxKind::FnDecl, oi.text_range()))
            .unwrap()
            .cell_slots
            .is_empty());

        // y captured AND reassigned → IS a cell.
        let mutated = parse_to_tree("fn o() {\n let y = 1\n fn i() { y = 2 }\n}");
        let r2 = resolve(&mutated);
        let oi2 = mutated
            .descendants()
            .find(|n| {
                n.kind() == SyntaxKind::FnDecl && ident_text(n).as_deref() == Some("o")
            })
            .unwrap();
        assert_eq!(
            r2.frames
                .get(&(SyntaxKind::FnDecl, oi2.text_range()))
                .unwrap()
                .cell_slots,
            vec![0]
        );
    }
}
