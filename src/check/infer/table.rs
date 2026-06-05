//! Class / enum symbol table for the type checker (SP10 §6).
//!
//! The resolver records *bindings* and *uses* but NOT a typed class table with
//! field/method types. This module builds one by walking every `ClassDecl` /
//! `EnumDecl` CST node once: each class gets a [`ClassId`], its superclass is
//! resolved by name to a parent id (with a visited-set so a cyclic `extends`
//! terminates), and field / method-return types are lowered to [`CheckTy`]. Each
//! enum gets an [`EnumId`] and its variant names.

use crate::check::infer::ty::{CheckTy, ClassId, EnumId};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;
use std::collections::HashMap;

/// A class entry: name, optional parent id, field types, method-return types.
#[derive(Debug, Clone, Default)]
pub struct ClassInfo {
    pub name: String,
    pub parent: Option<ClassId>,
    pub fields: HashMap<String, CheckTy>,
    pub methods: HashMap<String, CheckTy>,
}

/// An enum entry: name and variant names.
#[derive(Debug, Clone, Default)]
pub struct EnumInfo {
    pub name: String,
    pub variants: Vec<String>,
}

/// The class/enum symbol table, built once per `analyze` call.
#[derive(Debug, Clone, Default)]
pub struct Table {
    classes: Vec<ClassInfo>,
    enums: Vec<EnumInfo>,
    class_by_name: HashMap<String, ClassId>,
    enum_by_name: HashMap<String, EnumId>,
}

impl Table {
    /// Build the table from a resolved CST. Two passes so forward class references
    /// resolve: pass 1 registers every class/enum NAME, pass 2 lowers superclass +
    /// field/method types (which may reference a class declared later).
    pub fn build(tree: &ResolvedNode, _resolved: &ResolveResult) -> Table {
        use SyntaxKind::*;
        let mut t = Table::default();

        // Pass 1 — register names (last declaration of a name wins for lookup, but
        // each declaration still gets a distinct id, mirroring the runtime's
        // late-binding redeclaration).
        for node in tree.descendants() {
            match node.kind() {
                ClassDecl => {
                    let name = class_name(node).unwrap_or_default();
                    let id = t.classes.len();
                    t.classes.push(ClassInfo {
                        name: name.clone(),
                        ..Default::default()
                    });
                    if !name.is_empty() {
                        t.class_by_name.insert(name, id);
                    }
                }
                EnumDecl => {
                    let name = enum_name(node).unwrap_or_default();
                    let id = t.enums.len();
                    let variants = enum_variants(node);
                    t.enums.push(EnumInfo {
                        name: name.clone(),
                        variants,
                    });
                    if !name.is_empty() {
                        t.enum_by_name.insert(name, id);
                    }
                }
                _ => {}
            }
        }

        // Pass 2 — lower superclass + field/method types.
        let mut class_idx = 0usize;
        for node in tree.descendants() {
            if node.kind() != ClassDecl {
                continue;
            }
            let id = class_idx;
            class_idx += 1;

            let parent = superclass_name(node).and_then(|n| t.class_by_name.get(&n).copied());

            let mut fields: HashMap<String, CheckTy> = HashMap::new();
            let mut methods: HashMap<String, CheckTy> = HashMap::new();
            for member in node.children() {
                match member.kind() {
                    FieldDecl => {
                        if let Some(name) = crate::syntax::resolve::ident_text(member) {
                            let ty = field_type(member, &t);
                            fields.insert(name, ty);
                        }
                    }
                    MethodDecl => {
                        if let Some(name) = crate::syntax::resolve::ident_text(member) {
                            // Method return type: the declared RetType if any, else
                            // Any (in-file return inference is a later task).
                            let ret = method_return_type(member, &t);
                            methods.insert(name, ret);
                        }
                    }
                    _ => {}
                }
            }
            t.classes[id].parent = parent;
            t.classes[id].fields = fields;
            t.classes[id].methods = methods;
        }

        t
    }

    /// The [`ClassId`] for a class name, if known.
    pub fn class_id(&self, name: &str) -> Option<ClassId> {
        self.class_by_name.get(name).copied()
    }

    /// The [`EnumId`] for an enum name, if known.
    pub fn enum_id(&self, name: &str) -> Option<EnumId> {
        self.enum_by_name.get(name).copied()
    }

    /// Class info by id.
    pub fn class(&self, id: ClassId) -> Option<&ClassInfo> {
        self.classes.get(id)
    }

    /// Enum info by id.
    pub fn enum_info(&self, id: EnumId) -> Option<&EnumInfo> {
        self.enums.get(id)
    }

    /// Is `child` the same as `ancestor`, or does it transitively `extends` it?
    /// Bounded by a visited-set (cyclic/erroneous `extends` terminates).
    pub fn is_subclass(&self, child: ClassId, ancestor: ClassId) -> bool {
        let mut cur = Some(child);
        let mut visited = Vec::new();
        while let Some(c) = cur {
            if c == ancestor {
                return true;
            }
            if visited.contains(&c) {
                return false; // cycle
            }
            visited.push(c);
            cur = self.classes.get(c).and_then(|ci| ci.parent);
        }
        false
    }

    /// The nearest common ancestor of two classes (walk both chains, intersect),
    /// or `None` if they share no ancestor.
    pub fn nearest_common_ancestor(&self, a: ClassId, b: ClassId) -> Option<ClassId> {
        let chain_a = self.ancestry(a);
        let chain_b = self.ancestry(b);
        chain_a.iter().find(|x| chain_b.contains(x)).copied()
    }

    /// The class itself followed by every ancestor (bounded by a visited-set).
    fn ancestry(&self, start: ClassId) -> Vec<ClassId> {
        let mut chain = Vec::new();
        let mut cur = Some(start);
        while let Some(c) = cur {
            if chain.contains(&c) {
                break;
            }
            chain.push(c);
            cur = self.classes.get(c).and_then(|ci| ci.parent);
        }
        chain
    }

    /// The declared/inferred return [`CheckTy`] of a method on a class (walking the
    /// superclass chain), or `None` if no such method is known.
    pub fn method_return(&self, class: ClassId, name: &str) -> Option<CheckTy> {
        let mut cur = Some(class);
        let mut visited = Vec::new();
        while let Some(c) = cur {
            if visited.contains(&c) {
                break;
            }
            visited.push(c);
            let ci = self.classes.get(c)?;
            if let Some(ty) = ci.methods.get(name) {
                return Some(ty.clone());
            }
            cur = ci.parent;
        }
        None
    }

    /// The declared field [`CheckTy`] of a class (walking the superclass chain).
    pub fn field_type(&self, class: ClassId, name: &str) -> Option<CheckTy> {
        let mut cur = Some(class);
        let mut visited = Vec::new();
        while let Some(c) = cur {
            if visited.contains(&c) {
                break;
            }
            visited.push(c);
            let ci = self.classes.get(c)?;
            if let Some(ty) = ci.fields.get(name) {
                return Some(ty.clone());
            }
            cur = ci.parent;
        }
        None
    }
}

/// The declared name of a `ClassDecl` (its first `Ident` token).
fn class_name(node: &ResolvedNode) -> Option<String> {
    crate::syntax::resolve::ident_text(node)
}

/// The declared name of an `EnumDecl`.
fn enum_name(node: &ResolvedNode) -> Option<String> {
    crate::syntax::resolve::ident_text(node)
}

/// The superclass name of a `class X extends Y` (the second `Ident` token after
/// the soft keyword `extends`). Mirrors `resolve::record_superclass_use`.
fn superclass_name(node: &ResolvedNode) -> Option<String> {
    use SyntaxKind::Ident;
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .skip_while(|t| !(t.kind() == Ident && t.text() == "extends"))
        .filter(|t| t.kind() == Ident)
        .nth(1)
        .map(|t| t.text().to_string())
}

/// The variant names of an `EnumDecl` (each `EnumVariant` child's first `Ident`).
fn enum_variants(node: &ResolvedNode) -> Vec<String> {
    node.children()
        .filter(|c| c.kind() == SyntaxKind::EnumVariant)
        .filter_map(crate::syntax::resolve::ident_text)
        .collect()
}

/// Lower a field's declared type annotation to [`CheckTy`] (`Any` if unannotated).
fn field_type(member: &ResolvedNode, table: &Table) -> CheckTy {
    member
        .children()
        .find(|c| crate::check::rules::is_type_kind(c.kind()))
        .map(|ty| CheckTy::from_type_node(ty, table))
        .unwrap_or(CheckTy::Any)
}

/// Lower a method's declared return type (its `RetType` child's type) to
/// [`CheckTy`] (`Any` if undeclared — in-file return inference is a later task).
fn method_return_type(member: &ResolvedNode, table: &Table) -> CheckTy {
    let Some(ret) = member.children().find(|c| c.kind() == SyntaxKind::RetType) else {
        return CheckTy::Any;
    };
    ret.children()
        .find(|c| crate::check::rules::is_type_kind(c.kind()))
        .map(|ty| CheckTy::from_type_node(ty, table))
        .unwrap_or(CheckTy::Any)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::infer::ty::CheckTy;

    fn build(src: &str) -> Table {
        let tree = crate::syntax::tree_builder::build_tree(crate::syntax::parser::parse(src));
        let resolved = crate::syntax::resolve::resolve(&tree);
        Table::build(&tree, &resolved)
    }

    #[test]
    fn class_gets_id_and_fields() {
        let t = build("class P { n: number\n s: string }");
        let id = t.class_id("P").expect("P has an id");
        let ci = t.class(id).unwrap();
        assert_eq!(ci.fields.get("n"), Some(&CheckTy::Number));
        assert_eq!(ci.fields.get("s"), Some(&CheckTy::String));
    }

    #[test]
    fn extends_resolves_to_parent() {
        let t = build("class A {}\nclass B extends A {}");
        let a = t.class_id("A").unwrap();
        let b = t.class_id("B").unwrap();
        assert_eq!(t.class(b).unwrap().parent, Some(a));
        assert!(t.is_subclass(b, a));
        assert!(t.is_subclass(a, a));
        assert!(!t.is_subclass(a, b));
    }

    #[test]
    fn forward_superclass_reference() {
        // B extends A, but A is declared AFTER B (forward reference).
        let t = build("class B extends A {}\nclass A {}");
        let a = t.class_id("A").unwrap();
        let b = t.class_id("B").unwrap();
        assert_eq!(t.class(b).unwrap().parent, Some(a));
    }

    #[test]
    fn unknown_superclass_no_parent() {
        let t = build("class B extends Nonexistent {}");
        let b = t.class_id("B").unwrap();
        assert_eq!(t.class(b).unwrap().parent, None);
    }

    #[test]
    fn cyclic_extends_terminates() {
        // pathological: A extends B, B extends A — is_subclass must terminate.
        let t = build("class A extends B {}\nclass B extends A {}");
        let a = t.class_id("A").unwrap();
        let b = t.class_id("B").unwrap();
        // Either direction terminates (no hang); the result is bounded.
        let _ = t.is_subclass(a, b);
        let _ = t.is_subclass(b, a);
        assert!(t.is_subclass(a, a));
    }

    #[test]
    fn enum_variants_recorded() {
        let t = build("enum Color { Red, Green, Blue }");
        let id = t.enum_id("Color").unwrap();
        let ei = t.enum_info(id).unwrap();
        assert_eq!(ei.variants, vec!["Red", "Green", "Blue"]);
    }

    #[test]
    fn method_return_type_recorded() {
        let t = build("class C { fn f(): number { return 1 } }");
        let id = t.class_id("C").unwrap();
        assert_eq!(t.method_return(id, "f"), Some(CheckTy::Number));
    }

    #[test]
    fn nearest_common_ancestor_works() {
        let t = build("class A {}\nclass B extends A {}\nclass C extends A {}");
        let a = t.class_id("A").unwrap();
        let b = t.class_id("B").unwrap();
        let c = t.class_id("C").unwrap();
        assert_eq!(t.nearest_common_ancestor(b, c), Some(a));
    }

    #[test]
    fn inherited_field_type() {
        let t = build("class A { n: number }\nclass B extends A {}");
        let b = t.class_id("B").unwrap();
        assert_eq!(t.field_type(b, "n"), Some(CheckTy::Number));
    }
}
