//! Class / enum symbol table for the type checker (SP10 §6).
//!
//! The resolver records *bindings* and *uses* but NOT a typed class table with
//! field/method types. This module builds one by walking every `ClassDecl` /
//! `EnumDecl` CST node once: each class gets a [`ClassId`], its superclass is
//! resolved by name to a parent id (with a visited-set so a cyclic `extends`
//! terminates), and field / method-return types are lowered to [`CheckTy`]. Each
//! enum gets an [`EnumId`] and its variant names.

use crate::check::infer::ty::{CheckTy, ClassId, EnumId, InterfaceId};
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
    /// True if the class was declared `worker class` (Spec B: stateful actor).
    pub is_worker: bool,
}

/// One payload field of an enum variant (ADT §5.1). `name` is `Some` for a named
/// field (`Circle(radius: float)`), `None` for a positional field (`Pair(int, int)`).
#[derive(Debug, Clone)]
pub struct VariantFieldInfo {
    pub name: Option<String>,
    pub ty: CheckTy,
}

/// An enum entry: name, ordered variant names, and per-variant payload schemas.
///
/// `variants` and `variant_fields` are index-parallel: `variant_fields[i]` is the
/// (possibly empty) payload field list of `variants[i]`. A unit/scalar variant has
/// an empty field list; a payload variant has one entry per declared field.
#[derive(Debug, Clone, Default)]
pub struct EnumInfo {
    pub name: String,
    pub variants: Vec<String>,
    pub variant_fields: Vec<Vec<VariantFieldInfo>>,
}

impl EnumInfo {
    /// The declared payload fields of `variant`, if it exists.
    pub fn fields_of(&self, variant: &str) -> Option<&[VariantFieldInfo]> {
        let i = self.variants.iter().position(|v| v == variant)?;
        self.variant_fields.get(i).map(|v| v.as_slice())
    }
}

/// A structural-interface entry (IFACE §6 — reserved name). `methods` maps each
/// required method NAME to its lowered signature: the (declared-or-`Any`) parameter
/// types and the (declared-or-`Any`) return type. Conformance is structural over
/// this set (see [`Table::conforms`]).
#[derive(Debug, Clone, Default)]
pub struct InterfaceInfo {
    pub name: String,
    /// Required method name → (param types, return type).
    pub methods: HashMap<String, MethodSig>,
    /// Parent interface ids from `extends A, B` (their requirements are inherited).
    pub extends: Vec<InterfaceId>,
    /// Declared type-parameter names + optional bounds (TYPE §4 — `interface C<T>`).
    pub type_params: Vec<(String, Option<CheckTy>)>,
}

/// A lowered method signature (IFACE §6 / TYPE): ordered parameter types + return.
#[derive(Debug, Clone)]
pub struct MethodSig {
    pub params: Vec<CheckTy>,
    pub ret: CheckTy,
}

impl Default for MethodSig {
    fn default() -> Self {
        MethodSig {
            params: Vec::new(),
            ret: CheckTy::Any,
        }
    }
}

/// The class/enum/interface symbol table, built once per `analyze` call.
#[derive(Debug, Clone, Default)]
pub struct Table {
    classes: Vec<ClassInfo>,
    enums: Vec<EnumInfo>,
    interfaces: Vec<InterfaceInfo>,
    class_by_name: HashMap<String, ClassId>,
    enum_by_name: HashMap<String, EnumId>,
    interface_by_name: HashMap<String, InterfaceId>,
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
                    let is_worker = crate::syntax::resolve::is_worker_class(node);
                    t.classes.push(ClassInfo {
                        name: name.clone(),
                        is_worker,
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
                    let variant_fields = vec![Vec::new(); variants.len()];
                    t.enums.push(EnumInfo {
                        name: name.clone(),
                        variants,
                        variant_fields,
                    });
                    if !name.is_empty() {
                        t.enum_by_name.insert(name, id);
                    }
                }
                _ => {}
            }
        }

        // Pass 2b — lower per-variant payload field types (a field type may forward-
        // reference a class/enum declared later, so this runs after pass 1 registered
        // every name). Index-parallel to the enum's `variants`.
        let mut enum_idx = 0usize;
        for node in tree.descendants() {
            if node.kind() != EnumDecl {
                continue;
            }
            let id = enum_idx;
            enum_idx += 1;
            let fields = variant_field_schemas(node, &t);
            // Guard length parity (a malformed variant could desync — keep the
            // already-sized empty vector rather than panic).
            if fields.len() == t.enums[id].variants.len() {
                t.enums[id].variant_fields = fields;
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

    /// True if the class at `id` was declared `worker class` (Spec B actor).
    pub fn is_worker_class(&self, id: ClassId) -> bool {
        self.classes.get(id).map(|c| c.is_worker).unwrap_or(false)
    }

    /// The [`EnumId`] for an enum name, if known.
    pub fn enum_id(&self, name: &str) -> Option<EnumId> {
        self.enum_by_name.get(name).copied()
    }

    /// The [`InterfaceId`] for an interface name, if known (IFACE §6). Populated in
    /// TYPE Task 9; until then the table holds none.
    pub fn interface_id(&self, name: &str) -> Option<InterfaceId> {
        self.interface_by_name.get(name).copied()
    }

    /// Interface info by id (IFACE §6).
    pub fn interface_info(&self, id: InterfaceId) -> Option<&InterfaceInfo> {
        self.interfaces.get(id)
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

/// The per-variant payload field schemas of an `EnumDecl`, index-parallel to
/// [`enum_variants`]. Each `EnumVariant` child contributes its `VariantField`
/// children (a named field has an `Ident` then a type; a positional field is a bare
/// type). A unit/scalar variant has no `VariantField` children → an empty list.
fn variant_field_schemas(node: &ResolvedNode, table: &Table) -> Vec<Vec<VariantFieldInfo>> {
    use SyntaxKind::*;
    node.children()
        .filter(|c| c.kind() == EnumVariant)
        .map(|variant| {
            variant
                .children()
                .filter(|c| c.kind() == VariantField)
                .map(|f| {
                    // A named field's name is the `VariantField`'s leading `Ident`
                    // token (positional fields have none — their first token is the
                    // type). Distinguish by whether an `Ident` precedes a `Colon`.
                    let name = variant_field_name(f);
                    let ty = f
                        .children()
                        .find(|c| crate::check::rules::is_type_kind(c.kind()))
                        .map(|t| CheckTy::from_type_node(t, table))
                        .unwrap_or(CheckTy::Any);
                    VariantFieldInfo { name, ty }
                })
                .collect()
        })
        .collect()
}

/// The field name of a `VariantField` if it is NAMED (`radius: float`): the leading
/// `Ident` token that is immediately followed (ignoring trivia) by a `Colon`. A
/// positional field (`int`) — whose type may itself start with an `Ident` such as a
/// class name — has no such `Ident :` lead and returns `None`.
fn variant_field_name(field: &ResolvedNode) -> Option<String> {
    use SyntaxKind::*;
    let toks: Vec<_> = field
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .collect();
    match (toks.first(), toks.get(1)) {
        (Some(a), Some(b)) if a.kind() == Ident && b.kind() == Colon => Some(a.text().to_string()),
        _ => None,
    }
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
    fn variant_payload_schemas_recorded() {
        let t = build("enum Shape { Circle(radius: float), Rect(w: float, h: float), Pair(int, int), Point }");
        let id = t.enum_id("Shape").unwrap();
        let ei = t.enum_info(id).unwrap();
        assert_eq!(ei.variants, vec!["Circle", "Rect", "Pair", "Point"]);
        // Circle: one named float field.
        let circle = ei.fields_of("Circle").unwrap();
        assert_eq!(circle.len(), 1);
        assert_eq!(circle[0].name.as_deref(), Some("radius"));
        assert_eq!(circle[0].ty, CheckTy::Float);
        // Rect: two named float fields.
        let rect = ei.fields_of("Rect").unwrap();
        assert_eq!(rect.len(), 2);
        assert_eq!(rect[0].name.as_deref(), Some("w"));
        assert_eq!(rect[1].name.as_deref(), Some("h"));
        // Pair: two positional int fields (no names).
        let pair = ei.fields_of("Pair").unwrap();
        assert_eq!(pair.len(), 2);
        assert_eq!(pair[0].name, None);
        assert_eq!(pair[0].ty, CheckTy::Int);
        // Point: unit variant, no fields.
        assert_eq!(ei.fields_of("Point").unwrap().len(), 0);
    }

    #[test]
    fn variant_payload_field_can_reference_enum() {
        // A recursive payload (`array<Json>`) forward-references the enum itself.
        let t = build("enum Json { Null, Arr(items: array<Json>) }");
        let id = t.enum_id("Json").unwrap();
        let ei = t.enum_info(id).unwrap();
        let arr = ei.fields_of("Arr").unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].name.as_deref(), Some("items"));
        assert_eq!(arr[0].ty, CheckTy::Array(Box::new(CheckTy::Enum(id))));
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
