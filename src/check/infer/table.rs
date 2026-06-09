//! Class / enum symbol table for the type checker (SP10 §6).
//!
//! The resolver records *bindings* and *uses* but NOT a typed class table with
//! field/method types. This module builds one by walking every `ClassDecl` /
//! `EnumDecl` CST node once: each class gets a [`ClassId`], its superclass is
//! resolved by name to a parent id (with a visited-set so a cyclic `extends`
//! terminates), and field / method-return types are lowered to [`CheckTy`]. Each
//! enum gets an [`EnumId`] and its variant names.

use crate::check::infer::ty::{CheckTy, ClassId, Compat3, EnumId, InterfaceId};
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
    /// Field types in DECLARATION order (the `fields` map is unordered). Used for
    /// positional construction (`Box(5)` binds arg 0 to the first field), which
    /// TYPE §4 needs to infer a generic class's type args when there is no `init`.
    pub field_order: Vec<(String, CheckTy)>,
    pub methods: HashMap<String, CheckTy>,
    /// Full method signatures (params + return) — used by structural `conforms`.
    pub method_sigs: HashMap<String, MethodSig>,
    /// True if the class was declared `worker class` (Spec B: stateful actor).
    pub is_worker: bool,
    /// TYPE §4: declared type-parameter names + optional (interface) bounds. Empty
    /// for a non-generic class. The names lower (via `from_type_node`) to template
    /// `Var`s the unifier freshens at each construction/method call.
    pub type_params: Vec<(String, Option<CheckTy>)>,
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
    /// TYPE §4: declared type-parameter names + optional bounds (`enum Option<T>`).
    pub type_params: Vec<(String, Option<CheckTy>)>,
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
                        type_params: Vec::new(),
                    });
                    if !name.is_empty() {
                        t.enum_by_name.insert(name, id);
                    }
                }
                InterfaceDecl => {
                    let name = crate::syntax::resolve::ident_text(node).unwrap_or_default();
                    let id = t.interfaces.len();
                    t.interfaces.push(InterfaceInfo {
                        name: name.clone(),
                        ..Default::default()
                    });
                    if !name.is_empty() {
                        t.interface_by_name.insert(name, id);
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
            // TYPE §4: record the enum's declared type parameters + bounds.
            t.enums[id].type_params = type_param_list(node, &t);
        }

        // Pass 2c — lower interface method signatures + extends + type params (IFACE
        // §6 / TYPE §4). Runs after pass 1 registered every interface name, so an
        // `extends` / a bound can reference a forward-declared interface.
        let mut iface_idx = 0usize;
        for node in tree.descendants() {
            if node.kind() != InterfaceDecl {
                continue;
            }
            let id = iface_idx;
            iface_idx += 1;
            let type_params = type_param_list(node, &t);
            let extends = interface_extends(node, &t);
            let methods = interface_method_sigs(node, &t);
            t.interfaces[id].type_params = type_params;
            t.interfaces[id].extends = extends;
            t.interfaces[id].methods = methods;
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
            let mut field_order: Vec<(String, CheckTy)> = Vec::new();
            let mut methods: HashMap<String, CheckTy> = HashMap::new();
            let mut method_sigs: HashMap<String, MethodSig> = HashMap::new();
            for member in node.children() {
                match member.kind() {
                    FieldDecl => {
                        if let Some(name) = crate::syntax::resolve::ident_text(member) {
                            let ty = field_type(member, &t);
                            fields.insert(name.clone(), ty.clone());
                            field_order.push((name, ty));
                        }
                    }
                    MethodDecl => {
                        if let Some(name) = crate::syntax::resolve::ident_text(member) {
                            // Method return type: the declared RetType if any, else
                            // Any (in-file return inference is a later task).
                            let ret = method_return_type(member, &t);
                            methods.insert(name.clone(), ret.clone());
                            method_sigs.insert(
                                name,
                                MethodSig {
                                    params: method_param_types(member, &t),
                                    ret,
                                },
                            );
                        }
                    }
                    _ => {}
                }
            }
            t.classes[id].parent = parent;
            t.classes[id].fields = fields;
            t.classes[id].field_order = field_order;
            t.classes[id].methods = methods;
            t.classes[id].method_sigs = method_sigs;
            // TYPE §4: record the class's declared type parameters + bounds.
            t.classes[id].type_params = type_param_list(node, &t);
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

    /// The full method signature of a class method (walking the superclass chain).
    pub fn method_sig(&self, class: ClassId, name: &str) -> Option<&MethodSig> {
        let mut cur = Some(class);
        let mut visited = Vec::new();
        while let Some(c) = cur {
            if visited.contains(&c) {
                break;
            }
            visited.push(c);
            let ci = self.classes.get(c)?;
            if let Some(sig) = ci.method_sigs.get(name) {
                return Some(sig);
            }
            cur = ci.parent;
        }
        None
    }

    /// The declared type parameters (name + optional bound) of a class.
    pub fn class_type_params(&self, class: ClassId) -> &[(String, Option<CheckTy>)] {
        self.classes.get(class).map(|c| c.type_params.as_slice()).unwrap_or(&[])
    }

    /// The declared type parameters of an enum.
    pub fn enum_type_params(&self, e: EnumId) -> &[(String, Option<CheckTy>)] {
        self.enums.get(e).map(|ei| ei.type_params.as_slice()).unwrap_or(&[])
    }

    /// Every required method of an interface, INCLUDING those inherited via `extends`
    /// (bounded by a visited-set so a cyclic `extends` terminates). Returns
    /// `(name, sig)` pairs; a method redeclared closer wins (rare).
    pub fn interface_all_methods(&self, iface: InterfaceId) -> Vec<(String, MethodSig)> {
        let mut out: HashMap<String, MethodSig> = HashMap::new();
        let mut stack = vec![iface];
        let mut visited: Vec<InterfaceId> = Vec::new();
        while let Some(i) = stack.pop() {
            if visited.contains(&i) {
                continue;
            }
            visited.push(i);
            let Some(info) = self.interfaces.get(i) else {
                continue;
            };
            for (name, sig) in &info.methods {
                out.entry(name.clone()).or_insert_with(|| sig.clone());
            }
            stack.extend(info.extends.iter().copied());
        }
        out.into_iter().collect()
    }

    /// **The structural conformance predicate** (TYPE §4.5 / IFACE §6): does a value
    /// of type `t` conform to interface `iface`?
    ///
    /// Three-valued, biased to `Unknown` (the gradual gate):
    /// - `Yes` if `t` is a class/instance providing EVERY required method with an
    ///   ASSIGNABLE signature;
    /// - `No` if `t` is a fully-CONCRETE class that provably LACKS a required method,
    ///   or has a present method with a provably-incompatible *typed* signature;
    /// - `Unknown` otherwise — a non-class `t`, an `Any`/`Var`, or any uncertainty
    ///   (a present-but-untyped method yields `Unknown` for that method ⇒ the whole
    ///   predicate is `Unknown`). A partially-known `t` NEVER blocks.
    pub fn conforms(&self, t: &CheckTy, iface: InterfaceId) -> Compat3 {
        use CheckTy::*;
        // Gradual escapes: never block on `Any`/`Var`/`Object`/an interface-typed t.
        let t = t.widen();
        let cid = match &t {
            Class(c) => *c,
            ClassApp(c, _) => *c,
            // Any other shape (primitive, container, Any, Object, another interface)
            // is not provably a non-conforming *class*, so stay gradual.
            _ => return Compat3::Unknown,
        };
        let required = self.interface_all_methods(iface);
        if required.is_empty() {
            // An empty interface is conformed-to by every class (trivially).
            return Compat3::Yes;
        }
        let mut acc = Compat3::Yes;
        for (mname, req_sig) in &required {
            match self.method_sig(cid, mname) {
                None => {
                    // Provably missing a required method on a concrete class → No.
                    return Compat3::No;
                }
                Some(have) => {
                    // Compare signatures: arity + each param (contravariant-ish, but
                    // we only ever escalate to No on a PROVABLE clash) + the return.
                    // An untyped (`Any`) component on either side is gradual → Unknown
                    // for that method, never No.
                    let m = self.method_sig_compat(have, req_sig);
                    acc = meet3(acc, m);
                }
            }
        }
        acc
    }

    /// Three-valued compatibility of a class method signature `have` against an
    /// interface requirement `req`. `No` only on a PROVABLE clash (a concrete
    /// distinct param/return); any `Any`/uncertainty → `Unknown`. Arity differing →
    /// `Unknown` (gradual — AScript params are loosely arity-checked; never block on
    /// arity alone for conformance, keeping Gate 5 safe).
    fn method_sig_compat(&self, have: &MethodSig, req: &MethodSig) -> Compat3 {
        let mut acc = Compat3::Yes;
        // Return: the implementation's return must structurally match the requirement.
        // An untyped (`Any`) component on EITHER side is gradual → `Unknown` for that
        // component (NOT a free `Yes` — a present-but-untyped method is "we can't
        // prove conformance", IFACE §6), so a typo'd-but-untyped impl never silently
        // counts as conforming and an untyped impl never blocks.
        acc = meet3(acc, self.component_match(&have.ret, &req.ret));
        // Params: pairwise where both positions exist. We do NOT block on arity (a
        // gradual call surface), so only overlapping positions are compared.
        let n = have.params.len().min(req.params.len());
        for i in 0..n {
            acc = meet3(acc, self.component_match(&have.params[i], &req.params[i]));
        }
        acc
    }

    /// Three-valued STRUCTURAL match of one signature component (a param or return)
    /// for conformance: `Any`/`Var` on either side → `Unknown` (gradual — an untyped
    /// method component can neither prove nor disprove conformance); else `Yes` when
    /// the two are mutually assignable, `No` only when provably distinct both ways,
    /// `Unknown` otherwise.
    fn component_match(&self, a: &CheckTy, b: &CheckTy) -> Compat3 {
        use CheckTy::*;
        if matches!(a.widen(), Any) || matches!(b.widen(), Any) {
            return Compat3::Unknown;
        }
        let fwd = a.assignable(b, self);
        let bwd = b.assignable(a, self);
        match (fwd, bwd) {
            (Compat3::No, _) | (_, Compat3::No) => Compat3::No,
            (Compat3::Yes, Compat3::Yes) => Compat3::Yes,
            _ => Compat3::Unknown,
        }
    }
}

/// Three-valued meet for conformance accumulation: `No` dominates, then `Unknown`.
fn meet3(a: Compat3, b: Compat3) -> Compat3 {
    match (a, b) {
        (Compat3::No, _) | (_, Compat3::No) => Compat3::No,
        (Compat3::Unknown, _) | (_, Compat3::Unknown) => Compat3::Unknown,
        _ => Compat3::Yes,
    }
}

/// The declared name of a `ClassDecl` (its first `Ident` token).
fn class_name(node: &ResolvedNode) -> Option<String> {
    crate::syntax::resolve::ident_text(node)
}

/// TYPE §4: the declared type-parameter list of a decl (`<T, C: Bound>`), as
/// (name, optional-bound-`CheckTy`) pairs. A bound lowers via `from_type_node` — it
/// is only meaningful when it names an interface (`Interface(id)`); a non-interface
/// bound lowers to whatever it is (the instantiation check just won't `conforms`).
fn type_param_list(decl: &ResolvedNode, table: &Table) -> Vec<(String, Option<CheckTy>)> {
    use SyntaxKind::*;
    let Some(list) = decl.children().find(|c| c.kind() == TypeParams) else {
        return Vec::new();
    };
    list.children()
        .filter(|c| c.kind() == TypeParam)
        .filter_map(|tp| {
            let name = crate::syntax::resolve::ident_text(tp)?;
            let bound = tp
                .children()
                .find(|c| c.kind() == TypeBound)
                .and_then(|b| b.children().find(|c| crate::check::rules::is_type_kind(c.kind())))
                .map(|ty| CheckTy::from_type_node(ty, table));
            Some((name, bound))
        })
        .collect()
}

/// IFACE §6: the parent interface ids of an `interface X extends A, B`. Each name in
/// the `ExtendsList` resolved to a known interface id (unknown names dropped).
fn interface_extends(node: &ResolvedNode, table: &Table) -> Vec<InterfaceId> {
    use SyntaxKind::*;
    let Some(list) = node.children().find(|c| c.kind() == ExtendsList) else {
        return Vec::new();
    };
    list.children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == Ident)
        .filter_map(|t| table.interface_by_name.get(t.text()).copied())
        .collect()
}

/// IFACE §6: lower each `MethodReq` of an interface body to a `(name, MethodSig)`.
fn interface_method_sigs(node: &ResolvedNode, table: &Table) -> HashMap<String, MethodSig> {
    use SyntaxKind::*;
    let mut out = HashMap::new();
    for req in node.children().filter(|c| c.kind() == MethodReq) {
        let Some(name) = crate::syntax::resolve::ident_text(req) else {
            continue;
        };
        let params = method_param_types(req, table);
        let ret = method_return_type(req, table);
        out.insert(name, MethodSig { params, ret });
    }
    out
}

/// The ordered parameter types of a method/method-requirement's `ParamList`
/// (`Any` for an unannotated or rest param). Stops at the first rest param.
fn method_param_types(member: &ResolvedNode, table: &Table) -> Vec<CheckTy> {
    use SyntaxKind::*;
    let Some(list) = member.children().find(|c| c.kind() == ParamList) else {
        return Vec::new();
    };
    list.children()
        .filter(|c| c.kind() == Param)
        .take_while(|p| {
            !p.children_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.kind() == DotDotDot)
        })
        .map(|p| {
            p.children()
                .find(|c| crate::check::rules::is_type_kind(c.kind()))
                .map(|ty| CheckTy::from_type_node(ty, table))
                .unwrap_or(CheckTy::Any)
        })
        .collect()
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

    // ----------------------------- TYPE Task 9 -----------------------------------

    #[test]
    fn class_records_type_params() {
        let t = build("class Box<T> { value: T }");
        let id = t.class_id("Box").unwrap();
        let tp = t.class_type_params(id);
        assert_eq!(tp.len(), 1);
        assert_eq!(tp[0].0, "T");
        assert!(tp[0].1.is_none());
    }

    #[test]
    fn fn_bound_records_interface() {
        // `fn first<T, C: Container<T>>` — C's bound is the Container interface.
        let src = "interface Container<T> { fn len(): int\n fn at(i: int): T }\nfn first<T, C: Container<T>>(c: C): T { return c.at(0) }";
        let t = build(src);
        // The interface is registered.
        let iid = t.interface_id("Container").expect("Container interface");
        // The interface's method set is recorded.
        let info = t.interface_info(iid).unwrap();
        assert!(info.methods.contains_key("len"));
        assert!(info.methods.contains_key("at"));
        assert_eq!(info.type_params.len(), 1);
        assert_eq!(info.type_params[0].0, "T");
    }

    #[test]
    fn conforms_yes_when_methods_present() {
        let src = "interface Shape { fn area(): number }\nclass Circle { fn area(): number { return 1 } }";
        let t = build(src);
        let iid = t.interface_id("Shape").unwrap();
        let cid = t.class_id("Circle").unwrap();
        assert_eq!(t.conforms(&CheckTy::Class(cid), iid), Compat3::Yes);
    }

    #[test]
    fn conforms_no_when_method_missing() {
        let src = "interface Shape { fn area(): number }\nclass Empty { fn name(): string { return \"x\" } }";
        let t = build(src);
        let iid = t.interface_id("Shape").unwrap();
        let cid = t.class_id("Empty").unwrap();
        // A concrete class provably lacking `area` → No.
        assert_eq!(t.conforms(&CheckTy::Class(cid), iid), Compat3::No);
    }

    #[test]
    fn conforms_unknown_for_untyped_method() {
        // The class HAS `area` but its return is untyped → that method is Unknown ⇒
        // overall Unknown (gradual — never a corpus false positive).
        let src = "interface Shape { fn area(): number }\nclass Circle { fn area() { return 1 } }";
        let t = build(src);
        let iid = t.interface_id("Shape").unwrap();
        let cid = t.class_id("Circle").unwrap();
        assert_eq!(t.conforms(&CheckTy::Class(cid), iid), Compat3::Unknown);
    }

    #[test]
    fn conforms_no_on_provably_wrong_return() {
        // `area` is present but returns string where the interface wants number → No.
        let src = "interface Shape { fn area(): number }\nclass Circle { fn area(): string { return \"x\" } }";
        let t = build(src);
        let iid = t.interface_id("Shape").unwrap();
        let cid = t.class_id("Circle").unwrap();
        assert_eq!(t.conforms(&CheckTy::Class(cid), iid), Compat3::No);
    }

    #[test]
    fn conforms_unknown_for_non_class() {
        let src = "interface Shape { fn area(): number }";
        let t = build(src);
        let iid = t.interface_id("Shape").unwrap();
        // A primitive / Any / Object is never provably a non-conforming class.
        assert_eq!(t.conforms(&CheckTy::Int, iid), Compat3::Unknown);
        assert_eq!(t.conforms(&CheckTy::Any, iid), Compat3::Unknown);
        assert_eq!(t.conforms(&CheckTy::Object, iid), Compat3::Unknown);
    }

    #[test]
    fn conforms_inherits_extends() {
        // ReadWriter extends Reader+Writer; a class with both methods conforms.
        let src = "interface Reader { fn read(): int }\ninterface Writer { fn write(): int }\ninterface RW extends Reader, Writer {}\nclass F { fn read(): int { return 0 }\n fn write(): int { return 0 } }";
        let t = build(src);
        let iid = t.interface_id("RW").unwrap();
        let cid = t.class_id("F").unwrap();
        assert_eq!(t.conforms(&CheckTy::Class(cid), iid), Compat3::Yes);
        // Missing `write` → No.
        let src2 = "interface Reader { fn read(): int }\ninterface Writer { fn write(): int }\ninterface RW extends Reader, Writer {}\nclass G { fn read(): int { return 0 } }";
        let t2 = build(src2);
        let iid2 = t2.interface_id("RW").unwrap();
        let cid2 = t2.class_id("G").unwrap();
        assert_eq!(t2.conforms(&CheckTy::Class(cid2), iid2), Compat3::No);
    }
}

