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

    fn push_scope(&mut self) {
        self.scopes.push(Scope {
            names: HashMap::new(),
        });
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

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
        self.scopes
            .last_mut()
            .expect("a scope is open")
            .names
            .insert(name.to_string(), slot);
        slot
    }

    fn resolve_local(&self, name: &str) -> Option<u32> {
        for scope in self.scopes.iter().rev() {
            if let Some(&slot) = scope.names.get(name) {
                return Some(slot);
            }
        }
        None
    }

    fn resolve_file(&mut self, root: &ResolvedNode) {
        let key = root.text_range();
        self.frames.push(Frame {
            bindings: Vec::new(),
            next_slot: 0,
            key,
        });
        self.push_scope();
        for child in root.children() {
            self.resolve_stmt(child);
        }
        self.pop_scope();
        let frame = self.frames.pop().unwrap();
        self.result.frames.insert(
            frame.key,
            FrameInfo {
                slot_count: frame.next_slot,
                upvalues: Vec::new(),
                cell_slots: Vec::new(),
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
                if let Some(name) = ident_text(node) {
                    self.declare(&name, BindingKind::Let, node.text_range());
                }
            }
            ExprStmt | Block | IfStmt | WhileStmt | ReturnStmt => {
                for child in node.children() {
                    if is_expr(child.kind()) {
                        self.resolve_expr(child);
                    } else {
                        self.resolve_stmt(child);
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

    fn resolve_expr(&mut self, node: &ResolvedNode) {
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
                    self.resolve_expr(child);
                }
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
}
