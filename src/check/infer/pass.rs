//! The type-checking pass (SP10 §2 / §4).
//!
//! A single stateful visitor that performs local bidirectional inference over each
//! function body (and the top level), checks annotated slots, and emits
//! `type-mismatch`/`type-error`/`possibly-nil`. The cardinal discipline: **only a
//! provable `Compat3::No` ever emits** — everything uncertain synthesizes `Any`
//! and stays silent (the gradual escape that keeps the untyped corpus at zero).
//!
//! Scope:
//! - **T2:** synthesis (`synth`) + checking (`check_against`) of annotated binding
//!   initializers, annotated parameters at call sites, annotated returns, and
//!   annotated class-field defaults; `type-error` for provably ill-typed
//!   operations. Legacy `contract-mismatch`/`field-default-type` spans are
//!   de-duplicated (the new pass suppresses its own `type-mismatch` there).
//! - **T3:** local in-file return-type inference + nil-guard narrowing +
//!   `possibly-nil`.
//! - **T4:** `match`/`instanceof` narrowing + early-return flow merge.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::check::infer::env::{BindingKey, Env};
use crate::check::infer::table::Table;
use crate::check::infer::ty::{CheckTy, Compat3, LitVal};
use crate::check::rules::{code_range, is_type_kind};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;
use std::collections::HashMap;

/// A flow refinement: a binding narrowed to a `CheckTy` within a branch.
type Refinement = (BindingKey, CheckTy);
/// The (then-branch, else-branch) refinements computed for an `if` condition.
type BranchRefinements = (Vec<Refinement>, Vec<Refinement>);

/// A collected hover type: the byte span of a name use and its rendered `CheckTy`.
pub struct HoverType {
    pub range: ByteSpan,
    pub ty: String,
}

/// Drive the inference pass purely to COLLECT hover types (no diagnostics emitted):
/// every `NameRef` use's synthesized/declared type, rendered via `CheckTy::display`.
/// Used by the LSP hover hook (no interpreter).
pub fn collect_hover_types(
    tree: &ResolvedNode,
    resolved: &ResolveResult,
    src: &str,
    table: &Table,
) -> Vec<HoverType> {
    let mut pass = Pass::new(resolved, src, table);
    pass.hover = Some(Vec::new());
    pass.run(tree);
    pass.hover.take().unwrap_or_default()
}

/// Drive the inference pass over the whole file.
pub fn run(
    tree: &ResolvedNode,
    resolved: &ResolveResult,
    src: &str,
    table: &Table,
) -> Vec<AsDiagnostic> {
    let mut pass = Pass::new(resolved, src, table);
    pass.run(tree);
    pass.finish()
}

/// The stateful pass.
struct Pass<'a> {
    resolved: &'a ResolveResult,
    table: &'a Table,
    out: Vec<AsDiagnostic>,
    /// Spans (start offsets) already covered by a legacy `contract-mismatch`/
    /// `field-default-type` diagnostic — the new pass suppresses its own
    /// `type-mismatch` there (§6 one-release overlap de-dup).
    legacy_spans: std::collections::HashSet<usize>,
    /// In-file function inferred return types, cached by the function's decl-range
    /// start (declared return wins; otherwise the join of return synths from T3).
    fn_returns: HashMap<usize, CheckTy>,
    /// The declared return type stack (for checking `return` against it).
    expected_return: Vec<Option<CheckTy>>,
    /// Functions (by decl-range start) currently under return inference — a self
    /// call resolves to `Any` (recursion guard, no fixpoint loop).
    inferring: std::collections::HashSet<usize>,
    /// When > 0, `emit` is suppressed (we're synthesizing inside return inference,
    /// not the real diagnosing walk — the real walk reports those nodes).
    suppress_emit: u32,
    /// When `Some`, every `NameRef` synth records its (range, displayed type) here
    /// (the LSP hover collection mode). `None` for the normal diagnosing pass.
    hover: Option<Vec<HoverType>>,
}

impl<'a> Pass<'a> {
    fn new(resolved: &'a ResolveResult, src: &'a str, table: &'a Table) -> Pass<'a> {
        Pass {
            resolved,
            table,
            out: Vec::new(),
            legacy_spans: legacy_covered_spans(resolved, src),
            fn_returns: HashMap::new(),
            expected_return: Vec::new(),
            inferring: std::collections::HashSet::new(),
            suppress_emit: 0,
            hover: None,
        }
    }

    fn finish(self) -> Vec<AsDiagnostic> {
        self.out
    }

    fn run(&mut self, tree: &ResolvedNode) {
        let mut env = Env::new();
        self.walk_stmts(tree, &mut env);
    }

    // ----------------------------------------------------------------- emit ----

    fn emit(&mut self, code: &str, range: ByteSpan, message: String) {
        if self.suppress_emit > 0 {
            return; // synthesizing during return inference — the real walk reports.
        }
        if code == "type-mismatch" && self.legacy_spans.contains(&range.start) {
            return; // de-dup against the legacy rule at this span
        }
        self.out.push(AsDiagnostic {
            range,
            severity: Severity::Warning,
            code: code.to_string(),
            message,
            fix: None,
        });
    }

    // --------------------------------------------------------- statement walk --

    fn walk_stmts(&mut self, node: &ResolvedNode, env: &mut Env) {
        // Open a block-level narrowing scope so an early-return guard's negation can
        // refine bindings for the statements that follow it (§4 form 1/4).
        env.push_narrowing();
        for stmt in node.children() {
            self.walk_stmt(stmt, env);
        }
        env.pop_narrowing();
    }

    fn walk_stmt(&mut self, stmt: &ResolvedNode, env: &mut Env) {
        use SyntaxKind::*;
        match stmt.kind() {
            LetStmt => self.walk_let(stmt, env),
            ReturnStmt => self.walk_return(stmt, env),
            ExprStmt => {
                if let Some(e) = first_expr_child(stmt) {
                    self.synth(&e, env);
                }
            }
            IfStmt => self.walk_if(stmt, env),
            WhileStmt | ForStmt => self.walk_loop(stmt, env),
            Block => self.walk_child_block(stmt, env),
            FnDecl => self.walk_fn(stmt),
            ClassDecl => self.walk_class(stmt),
            ExportStmt => {
                for c in stmt.children() {
                    self.walk_stmt(c, env);
                }
            }
            _ => {
                // defensively synth nested exprs / walk nested blocks.
                for c in stmt.children() {
                    if crate::check::rules::is_expr_kind(c.kind()) {
                        self.synth(c, env);
                    } else if c.kind() == Block {
                        self.walk_child_block(c, env);
                    }
                }
            }
        }
    }

    /// Walk a block in a child environment seeded from `env`'s CURRENT view (base +
    /// active narrowing), plus extra `refinements` (e.g. an `if`-condition narrowing
    /// applied to the then/else branch).
    fn walk_child_block(&mut self, block: &ResolvedNode, env: &Env) {
        self.walk_child_block_with(block, env, &[]);
    }

    fn walk_child_block_with(
        &mut self,
        block: &ResolvedNode,
        env: &Env,
        refinements: &[(BindingKey, CheckTy)],
    ) {
        let mut inner = Env::new();
        // Seed with the parent's CURRENT (possibly-narrowed) view of each binding.
        for (k, _) in env.iter_base() {
            if let Some(ty) = env.lookup(k) {
                inner.define(k.clone(), ty);
            }
        }
        // Apply branch refinements on top.
        for (k, ty) in refinements {
            inner.define(k.clone(), ty.clone());
        }
        self.walk_stmts(block, &mut inner);
    }

    fn walk_let(&mut self, stmt: &ResolvedNode, env: &mut Env) {
        let ann = stmt.children().find(|c| is_type_kind(c.kind()));
        let init = first_expr_child(stmt);
        let key = self.binding_key_of_decl(stmt);

        let bound_ty = match (&ann, &init) {
            (Some(ty_node), Some(init_expr)) => {
                let expected = CheckTy::from_type_node(ty_node, self.table);
                self.check_against(init_expr, &expected, env);
                expected
            }
            (Some(ty_node), None) => CheckTy::from_type_node(ty_node, self.table),
            (None, Some(init_expr)) => self.synth(init_expr, env).widen(),
            (None, None) => CheckTy::Any,
        };
        // LSP hover/inlay collection mode: record the inferred type of an
        // un-annotated `let`/`const` binding on its NAME-token range, so the inlay
        // provider can surface an inferred-type hint there (a binding site is not a
        // `NameRef` use, so it is not otherwise covered by `synth_nameref`). Only
        // for a concrete (non-`Any`) type, and never for an annotated binding (the
        // annotation is already visible in the source). Hover-mode-only: the normal
        // diagnosing pass leaves `hover == None`, so this is behavior-preserving and
        // emits no diagnostics.
        if self.hover.is_some() && ann.is_none() && bound_ty != CheckTy::Any {
            if let Some(name_range) = decl_name_range(stmt) {
                let display = bound_ty.display(self.table);
                if let Some(h) = self.hover.as_mut() {
                    h.push(HoverType {
                        range: name_range,
                        ty: display,
                    });
                }
            }
        }
        if let Some(k) = key {
            env.define(k, bound_ty);
        }
    }

    fn walk_return(&mut self, stmt: &ResolvedNode, env: &mut Env) {
        let expr = first_expr_child(stmt);
        let expected = self.expected_return.last().cloned().flatten();
        match (expr, expected) {
            (Some(e), Some(exp)) => self.check_against(&e, &exp, env),
            (Some(e), None) => {
                self.synth(&e, env);
            }
            _ => {}
        }
    }

    fn walk_if(&mut self, stmt: &ResolvedNode, env: &mut Env) {
        let cond = stmt
            .children()
            .find(|c| crate::check::rules::is_expr_kind(c.kind()));
        if let Some(c) = &cond {
            self.synth(c, env);
        }
        // Compute nil-guard narrowing for the then / else branches.
        let (then_refs, else_refs) = cond
            .as_ref()
            .map(|c| self.condition_narrowing(c, env))
            .unwrap_or_default();

        let blocks: Vec<ResolvedNode> = stmt
            .children()
            .filter(|c| c.kind() == SyntaxKind::Block)
            .cloned()
            .collect();
        let else_if: Vec<ResolvedNode> = stmt
            .children()
            .filter(|c| c.kind() == SyntaxKind::IfStmt)
            .cloned()
            .collect();

        // then-block: narrowed by then_refs.
        if let Some(then_block) = blocks.first() {
            self.walk_child_block_with(then_block, env, &then_refs);
        }
        // else-block: narrowed by else_refs.
        if let Some(else_block) = blocks.get(1) {
            self.walk_child_block_with(else_block, env, &else_refs);
        }
        // `else if` chain: walk with else_refs in scope (the else side of THIS cond).
        for c in &else_if {
            // Seed a child env carrying else_refs so the chained condition sees them.
            let mut chained = Env::new();
            for (k, _) in env.iter_base() {
                if let Some(ty) = env.lookup(k) {
                    chained.define(k.clone(), ty);
                }
            }
            for (k, ty) in &else_refs {
                chained.define(k.clone(), ty.clone());
            }
            self.walk_if(c, &mut chained);
        }

        // Early-return flow merge (§4 form 1/4): if the THEN branch always exits,
        // the negation (else_refs) holds for the rest of THIS block. Symmetric for a
        // sole else that always exits (then_refs hold).
        let then_exits = blocks.first().map(block_always_returns).unwrap_or(false);
        let has_else = blocks.len() >= 2 || !else_if.is_empty();
        if then_exits && !has_else {
            for (k, ty) in else_refs {
                env.narrow(k, ty);
            }
        }
    }

    /// Compute the (then, else) nil-guard refinements for an `if` condition (§4
    /// form 1). Recognizes `x != nil` / `x == nil` (either operand order), bare
    /// truthiness `x` (narrows away `Nil` only — AScript `0`/`""` are truthy), and
    /// `!x`. Keys off the resolved binding (`BindingKey`), never the textual name.
    /// Returns refinements only for a binding whose current type is `T?`.
    fn condition_narrowing(&self, cond: &ResolvedNode, env: &Env) -> BranchRefinements {
        use SyntaxKind::*;
        match cond.kind() {
            ParenExpr => first_expr_child(cond)
                .map(|c| self.condition_narrowing(&c, env))
                .unwrap_or_default(),
            BinaryExpr => {
                let op = binary_op(cond);
                // `x instanceof C` (SP2): then narrows x to Class(C); else
                // meet-subtracts Class(C) from a class union (§4 form 2).
                if op == Some(InstanceofKw) {
                    return self.instanceof_narrowing(cond, env);
                }
                if !matches!(op, Some(EqEq | BangEq)) {
                    return Default::default();
                }
                let operands: Vec<ResolvedNode> = cond
                    .children()
                    .filter(|c| crate::check::rules::is_expr_kind(c.kind()))
                    .cloned()
                    .collect();
                // Find the `nil` literal operand and the OTHER (name) operand.
                let (name_node, has_nil) = match (operands.first(), operands.get(1)) {
                    (Some(a), Some(b)) if is_nil_literal(b) => (Some(a), true),
                    (Some(a), Some(b)) if is_nil_literal(a) => (Some(b), true),
                    _ => (None, false),
                };
                if !has_nil {
                    return Default::default();
                }
                let Some(name_node) = name_node else {
                    return Default::default();
                };
                let Some(key) = self.narrowable_key(name_node, env) else {
                    return Default::default();
                };
                let cur = env.lookup(&key).unwrap_or(CheckTy::Any);
                let non_nil = cur.without_nil();
                let nil_only = cur.only_nil();
                // `x == nil`: then = nil, else = non-nil. `x != nil`: swapped.
                if op == Some(EqEq) {
                    (vec![(key.clone(), nil_only)], vec![(key, non_nil)])
                } else {
                    (vec![(key.clone(), non_nil)], vec![(key, nil_only)])
                }
            }
            NameRef => {
                // truthiness: then narrows away Nil ONLY (not Bool(false)/0/"").
                let Some(key) = self.narrowable_key(cond, env) else {
                    return Default::default();
                };
                let cur = env.lookup(&key).unwrap_or(CheckTy::Any);
                (vec![(key, cur.without_nil())], Vec::new())
            }
            UnaryExpr => {
                // `!x`: the ELSE branch (x falsy → here x truthy) narrows away Nil.
                let is_bang = cond
                    .children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .any(|t| t.kind() == Bang);
                if !is_bang {
                    return Default::default();
                }
                let Some(inner) = first_expr_child(cond) else {
                    return Default::default();
                };
                if inner.kind() != NameRef {
                    return Default::default();
                }
                let Some(key) = self.narrowable_key(&inner, env) else {
                    return Default::default();
                };
                let cur = env.lookup(&key).unwrap_or(CheckTy::Any);
                (Vec::new(), vec![(key, cur.without_nil())])
            }
            _ => Default::default(),
        }
    }

    /// Narrowing for `x instanceof C` (§4 form 2). then: x → `Class(C)`; else:
    /// meet-subtract `Class(C)` from x when x is currently a union of classes (for
    /// `Any`/non-class-union x, the else refinement is omitted — stays gradual).
    fn instanceof_narrowing(&self, cond: &ResolvedNode, env: &Env) -> BranchRefinements {
        let operands: Vec<ResolvedNode> = cond
            .children()
            .filter(|c| crate::check::rules::is_expr_kind(c.kind()))
            .cloned()
            .collect();
        let (Some(lhs), Some(rhs)) = (operands.first(), operands.get(1)) else {
            return Default::default();
        };
        if lhs.kind() != SyntaxKind::NameRef || rhs.kind() != SyntaxKind::NameRef {
            return Default::default();
        }
        // RHS must name a known class.
        let class_name = crate::syntax::resolve::ident_text(rhs).unwrap_or_default();
        let Some(cid) = self.table.class_id(&class_name) else {
            return Default::default();
        };
        let Some(key) = self.key_for_use(&lhs.text_range()) else {
            return Default::default();
        };
        let cur = env.lookup(&key).unwrap_or(CheckTy::Any);
        let then = vec![(key.clone(), CheckTy::Class(cid))];
        // else: subtract Class(cid) from a class union; otherwise no refinement.
        let else_ref = match &cur {
            CheckTy::Union(ms) if ms.iter().all(|m| matches!(m, CheckTy::Class(_))) => {
                let kept: Vec<CheckTy> = ms
                    .iter()
                    .filter(|m| !matches!(m, CheckTy::Class(c) if *c == cid))
                    .cloned()
                    .collect();
                vec![(key, crate::check::infer::ty::normalize(CheckTy::Union(kept)))]
            }
            _ => Vec::new(),
        };
        (then, else_ref)
    }

    /// The `BindingKey` of a `NameRef` whose CURRENT type is a provable `T?` (so
    /// narrowing it is meaningful). `None` for non-names or non-optional bindings.
    fn narrowable_key(&self, name_node: &ResolvedNode, env: &Env) -> Option<BindingKey> {
        if name_node.kind() != SyntaxKind::NameRef {
            return None;
        }
        let key = self.key_for_use(&name_node.text_range())?;
        let cur = env.lookup(&key)?;
        if cur.is_provable_optional() {
            Some(key)
        } else {
            None
        }
    }

    fn walk_loop(&mut self, stmt: &ResolvedNode, env: &mut Env) {
        for c in stmt.children() {
            if crate::check::rules::is_expr_kind(c.kind()) {
                self.synth(c, env);
            } else if c.kind() == SyntaxKind::Block {
                self.walk_child_block(c, env);
            }
        }
    }

    fn walk_fn(&mut self, fn_decl: &ResolvedNode) {
        let expected = declared_return(fn_decl, self.table);
        let mut env = Env::new();
        self.bind_params(fn_decl, &mut env);
        self.expected_return.push(expected);
        if let Some(body) = fn_decl.children().find(|c| c.kind() == SyntaxKind::Block) {
            self.walk_stmts(body, &mut env);
        }
        self.expected_return.pop();
    }

    fn walk_class(&mut self, class: &ResolvedNode) {
        use SyntaxKind::*;
        for member in class.children() {
            match member.kind() {
                FieldDecl => self.check_field_default(member),
                MethodDecl => {
                    let expected = declared_return(member, self.table);
                    let mut env = Env::new();
                    self.bind_params(member, &mut env);
                    self.expected_return.push(expected);
                    if let Some(body) = member.children().find(|c| c.kind() == Block) {
                        self.walk_stmts(body, &mut env);
                    }
                    self.expected_return.pop();
                }
                _ => {}
            }
        }
    }

    /// Bind each annotated parameter to its declared type; unannotated/rest → Any.
    fn bind_params(&self, decl: &ResolvedNode, env: &mut Env) {
        let Some(list) = decl.children().find(|c| c.kind() == SyntaxKind::ParamList) else {
            return;
        };
        for param in list.children().filter(|c| c.kind() == SyntaxKind::Param) {
            let is_rest = param
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.kind() == SyntaxKind::DotDotDot);
            let ty = if is_rest {
                CheckTy::Any
            } else {
                param
                    .children()
                    .find(|c| is_type_kind(c.kind()))
                    .map(|t| CheckTy::from_type_node(t, self.table))
                    .unwrap_or(CheckTy::Any)
            };
            // A param is a DECLARATION (its name token is not a `uses` entry); key it
            // by the resolver binding recorded at this Param node's range — that
            // binding's slot is exactly what a use of the param resolves to.
            if let Some(b) = self
                .resolved
                .bindings
                .iter()
                .find(|b| b.decl_range == param.text_range())
            {
                let key = if b.is_global {
                    BindingKey::Global(b.name.clone())
                } else {
                    BindingKey::Local(b.slot)
                };
                env.define(key, ty);
            }
        }
    }

    fn check_field_default(&mut self, field: &ResolvedNode) {
        let Some(ty_node) = field.children().find(|c| is_type_kind(c.kind())) else {
            return;
        };
        let Some(default) = first_expr_child(field) else {
            return;
        };
        let mut expected = CheckTy::from_type_node(ty_node, self.table);
        let optional_marker = field
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::Question);
        if optional_marker && !expected.includes_nil() {
            expected =
                crate::check::infer::ty::normalize(CheckTy::Union(vec![expected, CheckTy::Nil]));
        }
        let mut env = Env::new();
        let actual = self.synth(&default, &mut env);
        if let Compat3::No = actual.assignable(&expected, self.table) {
            // Anchor at the FIELD node so de-dup against the legacy
            // `field-default-type` diagnostic (which uses `code_range(field)`) hits.
            let name = crate::syntax::resolve::ident_text(field).unwrap_or_default();
            let msg = format!(
                "field '{name}' default is `{}`, which violates its declared type `{}`",
                actual.display(self.table),
                expected.display(self.table)
            );
            self.emit("type-mismatch", code_range(field), msg);
        }
    }

    // ------------------------------------------------------------- check/synth --

    /// Check `expr` against `expected`; on a provable `No`, emit `type-mismatch`.
    /// `Unknown`/`Yes` are silent (gradual discipline).
    fn check_against(&mut self, expr: &ResolvedNode, expected: &CheckTy, env: &mut Env) {
        let actual = self.synth(expr, env);
        if let Compat3::No = actual.assignable(expected, self.table) {
            let msg = format!(
                "expected `{}`, found `{}`",
                expected.display(self.table),
                actual.display(self.table)
            );
            self.emit("type-mismatch", code_range(expr), msg);
        }
    }

    /// Synthesize `expr`'s type bottom-up (§2). Default `Any` (gradual escape).
    fn synth(&mut self, expr: &ResolvedNode, env: &mut Env) -> CheckTy {
        use SyntaxKind::*;
        match expr.kind() {
            Literal => literal_type(expr),
            TemplateExpr => {
                self.synth_children_exprs(expr, env);
                CheckTy::String
            }
            NameRef => self.synth_nameref(expr, env),
            ParenExpr => first_expr_child(expr)
                .map(|e| self.synth(&e, env))
                .unwrap_or(CheckTy::Any),
            BinaryExpr => self.synth_binary(expr, env),
            UnaryExpr => self.synth_unary(expr, env),
            CallExpr => self.synth_call(expr, env),
            ArrayExpr => self.synth_array(expr, env),
            ObjectExpr => {
                self.synth_children_exprs(expr, env);
                CheckTy::Object
            }
            AwaitExpr => {
                let inner = first_expr_child(expr)
                    .map(|e| self.synth(&e, env))
                    .unwrap_or(CheckTy::Any);
                match inner {
                    CheckTy::Future(t) => *t,
                    other => other,
                }
            }
            TryExpr | UnwrapExpr => {
                let inner = first_expr_child(expr)
                    .map(|e| self.synth(&e, env))
                    .unwrap_or(CheckTy::Any);
                match inner {
                    CheckTy::Result(t) => *t,
                    _ => CheckTy::Any,
                }
            }
            TernaryExpr => self.synth_ternary(expr, env),
            MemberExpr | OptMemberExpr => self.synth_member(expr, env),
            IndexExpr => {
                self.synth_children_exprs(expr, env);
                CheckTy::Any
            }
            AssignExpr => {
                self.synth_children_exprs(expr, env);
                CheckTy::Any
            }
            MatchExpr => self.synth_match(expr, env),
            _ => {
                self.synth_children_exprs(expr, env);
                CheckTy::Any
            }
        }
    }

    fn synth_children_exprs(&mut self, node: &ResolvedNode, env: &mut Env) {
        for c in node.children() {
            if crate::check::rules::is_expr_kind(c.kind()) {
                self.synth(c, env);
            }
        }
    }

    fn synth_nameref(&mut self, expr: &ResolvedNode, env: &mut Env) -> CheckTy {
        let Some(key) = self.key_for_use(&expr.text_range()) else {
            return CheckTy::Any;
        };
        let looked_up = env.lookup(&key);
        let ty = looked_up.clone().unwrap_or(CheckTy::Any);
        // Record a hover type ONLY for a name that resolves to a TRACKED binding (so a
        // bare undefined/builtin name yields no spurious `any` hover).
        if self.hover.is_some() && looked_up.is_some() {
            let display = ty.display(self.table);
            if let Some(h) = self.hover.as_mut() {
                h.push(HoverType {
                    range: code_range(expr),
                    ty: display,
                });
            }
        }
        ty
    }

    fn synth_binary(&mut self, expr: &ResolvedNode, env: &mut Env) -> CheckTy {
        use SyntaxKind::*;
        let op = binary_op(expr);
        let operands: Vec<ResolvedNode> = expr
            .children()
            .filter(|c| crate::check::rules::is_expr_kind(c.kind()))
            .cloned()
            .collect();

        if op == Some(QuestionQuestion) {
            let lt = operands
                .first()
                .map(|e| self.synth(e, env))
                .unwrap_or(CheckTy::Any);
            let rt = operands
                .get(1)
                .map(|e| self.synth(e, env))
                .unwrap_or(CheckTy::Any);
            return lt.without_nil().join(&rt, self.table);
        }

        let lt = operands
            .first()
            .map(|e| self.synth(e, env))
            .unwrap_or(CheckTy::Any);
        let rt = operands
            .get(1)
            .map(|e| self.synth(e, env))
            .unwrap_or(CheckTy::Any);

        match op {
            Some(Plus) => {
                // `+` is overloaded (string concat OR numeric add): only a provable
                // string/number result is synthesizable; never a type-error. A `T?`
                // operand still panics on nil, so flag possibly-nil.
                self.flag_possibly_nil_operands(&operands, &[&lt, &rt]);
                if is_string(&lt) || is_string(&rt) {
                    CheckTy::String
                } else if is_number(&lt) && is_number(&rt) {
                    CheckTy::Number
                } else {
                    CheckTy::Any
                }
            }
            Some(Minus | Star | Slash | Percent | StarStar) => {
                self.flag_non_numeric(&operands, &[&lt, &rt]);
                self.flag_possibly_nil_operands(&operands, &[&lt, &rt]);
                CheckTy::Number
            }
            Some(EqEq | BangEq | Lt | Le | Gt | Ge | InstanceofKw) => CheckTy::Bool,
            Some(AmpAmp | PipePipe) => lt.join(&rt, self.table),
            _ => CheckTy::Any,
        }
    }

    /// Emit `type-error` for any operand PROVABLY non-numeric in an arithmetic op.
    /// Gradual escape: if ANY operand is `Any`, the whole op is gradual — flag
    /// nothing (Siek–Taha consistency; keeps `let a: any = 1; a - "x"` silent).
    fn flag_non_numeric(&mut self, operands: &[ResolvedNode], types: &[&CheckTy]) {
        if types.iter().any(|t| matches!(t.widen(), CheckTy::Any)) {
            return;
        }
        for (node, ty) in operands.iter().zip(types.iter()) {
            if is_provably_non_number(ty) {
                let msg = format!(
                    "arithmetic operand is `{}`, not a number",
                    ty.display(self.table)
                );
                self.emit("type-error", code_range(node), msg);
            }
        }
    }

    /// Emit `possibly-nil` for any arithmetic operand whose type is PROVABLY a `T?`
    /// (a union containing `Nil`) — a `nil` here is a runtime panic. Heavily gated:
    /// only a provable optional (never `Any`, never a narrowed binding).
    fn flag_possibly_nil_operands(&mut self, operands: &[ResolvedNode], types: &[&CheckTy]) {
        for (node, ty) in operands.iter().zip(types.iter()) {
            if ty.is_provable_optional() {
                self.emit(
                    "possibly-nil",
                    code_range(node),
                    format!(
                        "value is `{}` and may be nil here; guard it (`if x != nil`, `x ?? default`)",
                        ty.display(self.table)
                    ),
                );
            }
        }
    }

    fn synth_unary(&mut self, expr: &ResolvedNode, env: &mut Env) -> CheckTy {
        use SyntaxKind::*;
        let op = expr
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .map(|t| t.kind())
            .find(|k| matches!(k, Minus | Bang));
        let operand = first_expr_child(expr);
        let ot = operand
            .as_ref()
            .map(|e| self.synth(e, env))
            .unwrap_or(CheckTy::Any);
        match op {
            Some(Bang) => CheckTy::Bool,
            Some(Minus) => {
                if let (Some(node), true) = (&operand, is_provably_non_number(&ot)) {
                    let msg = format!(
                        "negation operand is `{}`, not a number",
                        ot.display(self.table)
                    );
                    self.emit("type-error", code_range(node), msg);
                }
                CheckTy::Number
            }
            _ => CheckTy::Any,
        }
    }

    fn synth_call(&mut self, expr: &ResolvedNode, env: &mut Env) -> CheckTy {
        use SyntaxKind::*;
        let callee = expr.children().next();
        let arg_list = expr.children().find(|c| c.kind() == ArgList);

        if let Some(callee) = callee {
            if callee.kind() == NameRef {
                let name = crate::syntax::resolve::ident_text(callee).unwrap_or_default();
                if let Some(cid) = self.callee_class_id(callee, &name) {
                    self.synth_arg_list(arg_list, env);
                    return CheckTy::Class(cid);
                }
                if let Some(fn_decl) = self.resolve_in_file_fn(callee, &name) {
                    self.check_call_args(&fn_decl, arg_list, &name, env);
                    return self.fn_return_type(&fn_decl);
                }
            }
        }
        self.synth_arg_list(arg_list, env);
        CheckTy::Any
    }

    fn synth_arg_list(&mut self, arg_list: Option<&ResolvedNode>, env: &mut Env) {
        if let Some(al) = arg_list {
            for a in al
                .children()
                .filter(|c| crate::check::rules::is_expr_kind(c.kind()))
            {
                self.synth(a, env);
            }
        }
    }

    /// Check each positional arg against the corresponding annotated param.
    fn check_call_args(
        &mut self,
        fn_decl: &ResolvedNode,
        arg_list: Option<&ResolvedNode>,
        name: &str,
        env: &mut Env,
    ) {
        let Some(arg_list) = arg_list else { return };
        if arg_list.children().any(|c| c.kind() == SyntaxKind::SpreadElem) {
            self.synth_arg_list(Some(arg_list), env);
            return;
        }
        let params = param_type_nodes(fn_decl);
        let args: Vec<ResolvedNode> = arg_list
            .children()
            .filter(|c| crate::check::rules::is_expr_kind(c.kind()))
            .cloned()
            .collect();
        for (i, arg) in args.iter().enumerate() {
            let actual = self.synth(arg, env);
            let Some(Some(ptype_node)) = params.get(i) else {
                continue;
            };
            let expected = CheckTy::from_type_node(ptype_node, self.table);
            if let Compat3::No = actual.assignable(&expected, self.table) {
                let msg = format!(
                    "argument {} of `{name}` expects `{}`, found `{}`",
                    i + 1,
                    expected.display(self.table),
                    actual.display(self.table)
                );
                self.emit("type-mismatch", code_range(arg), msg);
            }
        }
    }

    fn synth_array(&mut self, expr: &ResolvedNode, env: &mut Env) -> CheckTy {
        let elems: Vec<ResolvedNode> = expr
            .children()
            .filter(|c| crate::check::rules::is_expr_kind(c.kind()))
            .cloned()
            .collect();
        for e in &elems {
            self.synth(e, env);
        }
        if expr.children().any(|c| c.kind() == SyntaxKind::SpreadElem) || elems.is_empty() {
            return CheckTy::Array(Box::new(CheckTy::Any));
        }
        let mut acc = CheckTy::Never;
        for e in &elems {
            let t = self.synth(e, env);
            acc = acc.join(&t, self.table);
        }
        CheckTy::Array(Box::new(acc.widen()))
    }

    fn synth_ternary(&mut self, expr: &ResolvedNode, env: &mut Env) -> CheckTy {
        let operands: Vec<ResolvedNode> = expr
            .children()
            .filter(|c| crate::check::rules::is_expr_kind(c.kind()))
            .cloned()
            .collect();
        if let Some(c) = operands.first() {
            self.synth(c, env);
        }
        let t = operands
            .get(1)
            .map(|e| self.synth(e, env))
            .unwrap_or(CheckTy::Any);
        let f = operands
            .get(2)
            .map(|e| self.synth(e, env))
            .unwrap_or(CheckTy::Any);
        t.join(&f, self.table)
    }

    /// Synthesize a `match` expression with per-arm subject narrowing (§4 form 3).
    /// The subject is synthesized once; for each `match`-arm that is a proper child
    /// of the `MatchExpr`, the subject binding is narrowed per the arm's pattern (a
    /// `nil` pattern → `Nil`; a non-nil literal/variant pattern → `without_nil`)
    /// while synthesizing that arm's body. The result is the join of arm-body synths
    /// (or `Any`). NOTE: the CST front-end nests only the FIRST arm under `MatchExpr`
    /// (subsequent arms are sibling statements walked by `walk_stmts`); this narrows
    /// what is reachable and stays silent (Any) on the rest — never a false positive.
    fn synth_match(&mut self, expr: &ResolvedNode, env: &mut Env) -> CheckTy {
        // The subject is the first expression-kind child (a ParenExpr or bare expr).
        let subject = first_expr_child(expr);
        let subject_key = subject.as_ref().and_then(|s| {
            let inner = if s.kind() == SyntaxKind::ParenExpr {
                first_expr_child(s)
            } else {
                Some(s.clone())
            }?;
            if inner.kind() == SyntaxKind::NameRef {
                self.key_for_use(&inner.text_range())
            } else {
                None
            }
        });
        if let Some(s) = &subject {
            self.synth(s, env);
        }

        let mut result = CheckTy::Never;
        for arm in expr.children().filter(|c| c.kind() == SyntaxKind::MatchArm) {
            // The arm body is its trailing expression-kind child.
            let body = arm
                .children()
                .filter(|c| crate::check::rules::is_expr_kind(c.kind()))
                .last()
                .cloned();
            // Narrow the subject for this arm per its pattern.
            let refinement = subject_key.as_ref().and_then(|key| {
                let pat = arm.children().find(|c| is_pattern_kind(c.kind()))?;
                let cur = env.lookup(key).unwrap_or(CheckTy::Any);
                if pattern_is_nil(pat) {
                    Some((key.clone(), cur.only_nil()))
                } else {
                    // a concrete non-nil pattern → subject is non-nil in this arm.
                    Some((key.clone(), cur.without_nil()))
                }
            });
            let arm_ty = if let Some((k, ty)) = refinement {
                env.push_narrowing();
                env.narrow(k, ty);
                let t = body.map(|b| self.synth(&b, env)).unwrap_or(CheckTy::Any);
                env.pop_narrowing();
                t
            } else {
                body.map(|b| self.synth(&b, env)).unwrap_or(CheckTy::Any)
            };
            result = result.join(&arm_ty.widen(), self.table);
        }
        if matches!(result, CheckTy::Never) {
            CheckTy::Any
        } else {
            result
        }
    }

    fn synth_member(&mut self, expr: &ResolvedNode, env: &mut Env) -> CheckTy {
        let recv = expr
            .children()
            .find(|c| crate::check::rules::is_expr_kind(c.kind()));
        let recv_ty = recv
            .as_ref()
            .map(|r| self.synth(r, env))
            .unwrap_or(CheckTy::Any);
        // A plain `.` on a PROVABLY `T?` receiver panics on nil → possibly-nil. A
        // `?.` (OptMemberExpr) tolerates nil, so it never fires.
        if expr.kind() == SyntaxKind::MemberExpr && recv_ty.is_provable_optional() {
            if let Some(r) = &recv {
                self.emit(
                    "possibly-nil",
                    code_range(r),
                    format!(
                        "value is `{}` and may be nil here; guard it before accessing a member",
                        recv_ty.display(self.table)
                    ),
                );
            }
        }
        let field = member_name(expr);
        match (&recv_ty, field) {
            (CheckTy::Class(cid), Some(f)) => self.table.field_type(*cid, &f).unwrap_or(CheckTy::Any),
            (CheckTy::Enum(eid), Some(f)) => {
                if let Some(ei) = self.table.enum_info(*eid) {
                    if ei.variants.iter().any(|v| v == &f) {
                        return CheckTy::EnumVariant(*eid, f.into());
                    }
                }
                CheckTy::Any
            }
            _ => CheckTy::Any,
        }
    }

    // -------------------------------------------------------------- resolving --

    fn key_for_use(&self, range: &cstree::text::TextRange) -> Option<BindingKey> {
        let res = self.resolved.uses.get(range)?;
        BindingKey::from_resolution(res)
    }

    /// The binding key declared by a simple (non-destructuring) `let`/`const`.
    fn binding_key_of_decl(&self, stmt: &ResolvedNode) -> Option<BindingKey> {
        if stmt.children().any(|c| {
            matches!(
                c.kind(),
                SyntaxKind::ArrayBindPat | SyntaxKind::ObjectBindPat
            )
        }) {
            return None;
        }
        let decl_range = stmt.text_range();
        let b = self
            .resolved
            .bindings
            .iter()
            .find(|b| b.decl_range == decl_range)?;
        if b.is_global {
            Some(BindingKey::Global(b.name.clone()))
        } else {
            Some(BindingKey::Local(b.slot))
        }
    }

    /// If `callee` resolves to an in-file `fn` declared exactly once, its `FnDecl`.
    fn resolve_in_file_fn(&self, callee: &ResolvedNode, name: &str) -> Option<ResolvedNode> {
        use crate::syntax::resolve::types::BindingKind;
        let mut same = self.resolved.bindings.iter().filter(|b| b.name == name);
        let only = same.next()?;
        if same.next().is_some() || only.kind != BindingKind::Fn {
            return None;
        }
        if crate::check::rules::resolves_to_unique(
            callee,
            name,
            only.decl_range,
            BindingKind::Fn,
            self.resolved,
        ) {
            find_node_by_range(callee, only.decl_range, SyntaxKind::FnDecl)
        } else {
            None
        }
    }

    /// If `callee` uniquely names a known class (constructor call), its id.
    fn callee_class_id(&self, callee: &ResolvedNode, name: &str) -> Option<usize> {
        use crate::syntax::resolve::types::BindingKind;
        let cid = self.table.class_id(name)?;
        let mut same = self.resolved.bindings.iter().filter(|b| b.name == name);
        let only = same.next()?;
        if same.next().is_some() || only.kind != BindingKind::Class {
            return None;
        }
        if crate::check::rules::resolves_to_unique(
            callee,
            name,
            only.decl_range,
            BindingKind::Class,
            self.resolved,
        ) {
            Some(cid)
        } else {
            None
        }
    }

    /// The type produced by CALLING an in-file function: its return type, wrapped
    /// per the function's kind — an `async fn` call yields `future<R>`; a generator
    /// (`fn*`/`async fn*`) call yields an opaque generator (→ `Any` in v1).
    fn fn_return_type(&mut self, fn_decl: &ResolvedNode) -> CheckTy {
        if is_generator(fn_decl) {
            return CheckTy::Any; // a generator handle, not the declared scalar
        }
        let ret = self.fn_declared_or_inferred(fn_decl);
        if is_async(fn_decl) || is_worker(fn_decl) {
            CheckTy::Future(Box::new(ret))
        } else {
            ret
        }
    }

    /// The function's RETURN value type (NOT wrapped for async/gen): declared, else
    /// inferred (T3), with a recursion guard (a self-call under inference → `Any`).
    fn fn_declared_or_inferred(&mut self, fn_decl: &ResolvedNode) -> CheckTy {
        if let Some(declared) = declared_return(fn_decl, self.table) {
            return declared;
        }
        let key = usize::from(fn_decl.text_range().start());
        if self.inferring.contains(&key) {
            return CheckTy::Any; // recursive self-call → Any
        }
        if let Some(cached) = self.fn_returns.get(&key) {
            return cached.clone();
        }
        self.inferring.insert(key);
        let inferred = self.infer_return(fn_decl);
        self.inferring.remove(&key);
        self.fn_returns.insert(key, inferred.clone());
        inferred
    }

    /// Infer a function's return type: the `join` of all its `return` expression
    /// synths, plus `Nil` if it can fall off the end (no return value reached on
    /// some path). Synthesis runs with emission SUPPRESSED (the real walk reports).
    fn infer_return(&mut self, fn_decl: &ResolvedNode) -> CheckTy {
        let Some(body) = fn_decl.children().find(|c| c.kind() == SyntaxKind::Block) else {
            return CheckTy::Any;
        };
        let mut env = Env::new();
        self.bind_params(fn_decl, &mut env);

        let mut returns: Vec<ResolvedNode> = Vec::new();
        collect_returns(body, &mut returns);

        self.suppress_emit += 1;
        let mut acc = CheckTy::Never;
        let mut saw_value_return = false;
        for ret in &returns {
            match first_expr_child(ret) {
                Some(e) => {
                    let t = self.synth(&e, &mut env);
                    acc = acc.join(&t.widen(), self.table);
                    saw_value_return = true;
                }
                None => {
                    // `return` with no value → Nil.
                    acc = acc.join(&CheckTy::Nil, self.table);
                }
            }
        }
        self.suppress_emit -= 1;

        // Can it fall off the end? If the body doesn't end in a terminator on every
        // path, a `nil` is implicitly returned. Conservatively: add Nil unless the
        // body's last statement is a return/panic-like terminator.
        if !block_always_returns(body) {
            acc = acc.join(&CheckTy::Nil, self.table);
        }
        if !saw_value_return && matches!(acc, CheckTy::Nil | CheckTy::Never) {
            // a pure side-effect fn — its return is `nil`, but treat as Any to stay
            // maximally silent at call sites (a no-value fn rarely flows into a slot).
            return CheckTy::Any;
        }
        acc.widen()
    }
}

// ============================================================ free helpers ====

/// Collect every `ReturnStmt` in `node`'s subtree that belongs to THIS function
/// (not descending into a nested `FnDecl`/`MethodDecl`/`ArrowExpr` body, whose
/// returns are the inner function's).
fn collect_returns(node: &ResolvedNode, out: &mut Vec<ResolvedNode>) {
    use SyntaxKind::*;
    for child in node.children() {
        match child.kind() {
            ReturnStmt => out.push(child.clone()),
            // do not descend into a nested function — its returns aren't ours.
            FnDecl | MethodDecl | ArrowExpr => {}
            _ => collect_returns(child, out),
        }
    }
}

/// Does this `Block` always reach a terminating statement (a `return`, or a panic
/// via a bare `panic(...)` call) on its straight-line tail? Conservative: only the
/// LAST statement being a `return`/`break`/`continue`, or an `if/else` where both
/// branches always return, counts. Anything else → may fall off the end.
fn block_always_returns(block: &ResolvedNode) -> bool {
    let last = block
        .children()
        .filter(|c| is_stmt_kind(c.kind()))
        .last();
    match last {
        Some(s) => stmt_always_returns(s),
        None => false,
    }
}

fn stmt_always_returns(stmt: &ResolvedNode) -> bool {
    use SyntaxKind::*;
    match stmt.kind() {
        ReturnStmt => true,
        Block => block_always_returns(stmt),
        IfStmt => {
            // both a then-block AND an else-block (or else-if chain) must always return.
            let blocks: Vec<ResolvedNode> = stmt
                .children()
                .filter(|c| c.kind() == Block)
                .cloned()
                .collect();
            let else_if: Vec<ResolvedNode> = stmt
                .children()
                .filter(|c| c.kind() == IfStmt)
                .cloned()
                .collect();
            if blocks.is_empty() {
                return false;
            }
            // Must have an else (a second block) or an else-if that always returns.
            let has_else = blocks.len() >= 2 || !else_if.is_empty();
            if !has_else {
                return false;
            }
            blocks.iter().all(block_always_returns)
                && else_if.iter().all(stmt_always_returns)
        }
        _ => false,
    }
}

/// Is `kind` a statement-kind node (used to find a block's tail statement)?
fn is_stmt_kind(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        LetStmt
            | ReturnStmt
            | ExprStmt
            | IfStmt
            | WhileStmt
            | ForStmt
            | Block
            | BreakStmt
            | ContinueStmt
            | FnDecl
            | ClassDecl
            | EnumDecl
            | ImportStmt
            | ExportStmt
    )
}

/// First expression-kind child of a node.
fn first_expr_child(node: &ResolvedNode) -> Option<ResolvedNode> {
    node.children()
        .find(|c| crate::check::rules::is_expr_kind(c.kind()))
        .cloned()
}

/// The byte span of a `let`/`const` binding's NAME token: the first `Ident` token
/// directly under the `LetStmt` (after the `let`/`const` keyword). Used by the LSP
/// hover/inlay collection mode to anchor an inferred-type hint at the binding name.
fn decl_name_range(stmt: &ResolvedNode) -> Option<ByteSpan> {
    stmt.children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| ByteSpan::from(t.text_range()))
}

/// Is `kind` a match-pattern node kind?
fn is_pattern_kind(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        WildcardPat | IdentPat | LiteralPat | RangePat | ArrayPat | ObjectPat | OrPat
    )
}

/// Is this pattern the `nil` literal pattern (`LiteralPat` wrapping `nil`)?
fn pattern_is_nil(pat: &ResolvedNode) -> bool {
    pat.kind() == SyntaxKind::LiteralPat
        && pat
            .descendants_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::NilKw)
}

/// Is `node` the `nil` literal?
fn is_nil_literal(node: &ResolvedNode) -> bool {
    node.kind() == SyntaxKind::Literal
        && node
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::NilKw)
}

/// The `CheckTy::Literal` of a `Literal` node.
fn literal_type(node: &ResolvedNode) -> CheckTy {
    use SyntaxKind::*;
    let tok = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| !t.kind().is_trivia());
    match tok.map(|t| t.kind()) {
        Some(Number) => CheckTy::Literal(LitVal::Number),
        Some(Str) => CheckTy::Literal(LitVal::String),
        Some(TrueKw | FalseKw) => CheckTy::Literal(LitVal::Bool),
        Some(NilKw) => CheckTy::Literal(LitVal::Nil),
        _ => CheckTy::Any,
    }
}

/// The binary operator token kind of a `BinaryExpr`.
fn binary_op(expr: &ResolvedNode) -> Option<SyntaxKind> {
    use SyntaxKind::*;
    expr.children_with_tokens()
        .filter_map(|el| el.into_token())
        .map(|t| t.kind())
        .find(|k| {
            matches!(
                k,
                Plus | Minus
                    | Star
                    | Slash
                    | Percent
                    | StarStar
                    | EqEq
                    | BangEq
                    | Lt
                    | Le
                    | Gt
                    | Ge
                    | AmpAmp
                    | PipePipe
                    | QuestionQuestion
                    | InstanceofKw
            )
        })
}

/// The member name accessed by a `MemberExpr`/`OptMemberExpr` (trailing Ident).
fn member_name(expr: &ResolvedNode) -> Option<String> {
    expr.children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::Ident)
        .last()
        .map(|t| t.text().to_string())
}

/// Per-fixed-parameter declared type node (None for unannotated/rest). Stops at
/// the first rest param.
fn param_type_nodes(fn_decl: &ResolvedNode) -> Vec<Option<ResolvedNode>> {
    use SyntaxKind::*;
    let Some(list) = fn_decl.children().find(|c| c.kind() == ParamList) else {
        return Vec::new();
    };
    list.children()
        .filter(|c| c.kind() == Param)
        .take_while(|p| {
            !p.children_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.kind() == DotDotDot)
        })
        .map(|p| p.children().find(|c| is_type_kind(c.kind())).cloned())
        .collect()
}

/// Is this fn/method declared `async` (carries an `AsyncKw` token)?
fn is_async(decl: &ResolvedNode) -> bool {
    decl.children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::AsyncKw)
}

/// Is this fn/method declared `worker` (carries a `WorkerKw` token)?
/// A worker fn call, like an async fn call, synthesizes `future<T>`.
fn is_worker(decl: &ResolvedNode) -> bool {
    decl.children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::WorkerKw)
}

/// Is this fn/method a generator (`fn*`/`async fn*` — carries a `Star` token at the
/// decl level, before the param list)?
fn is_generator(decl: &ResolvedNode) -> bool {
    // The `*` is a direct token child of the fn/method decl (after `fn`).
    decl.children_with_tokens()
        .filter_map(|el| el.into_token())
        .take_while(|t| t.kind() != SyntaxKind::LParen)
        .any(|t| t.kind() == SyntaxKind::Star)
}

/// The declared return type of a fn/method (its `RetType` child's type), if any.
fn declared_return(decl: &ResolvedNode, table: &Table) -> Option<CheckTy> {
    let ret = decl.children().find(|c| c.kind() == SyntaxKind::RetType)?;
    let ty = ret.children().find(|c| is_type_kind(c.kind()))?;
    Some(CheckTy::from_type_node(ty, table))
}

/// Find a node of `kind` at exactly `range`, searching from the tree root reachable
/// via `anchor`'s parent chain.
fn find_node_by_range(
    anchor: &ResolvedNode,
    range: cstree::text::TextRange,
    kind: SyntaxKind,
) -> Option<ResolvedNode> {
    let mut root = anchor.clone();
    while let Some(p) = root.parent() {
        root = p.clone();
    }
    let found = root
        .descendants()
        .find(|n| n.kind() == kind && n.text_range() == range)
        .cloned();
    found
}

/// Start offsets of spans already covered by a legacy `contract-mismatch`/
/// `field-default-type` diagnostic (so the new pass de-dups its `type-mismatch`).
fn legacy_covered_spans(resolved: &ResolveResult, src: &str) -> std::collections::HashSet<usize> {
    let tree = crate::syntax::tree_builder::build_tree(crate::syntax::parser::parse(src));
    let mut spans = std::collections::HashSet::new();
    for d in crate::check::rules::contract::check(&tree, resolved, src) {
        spans.insert(d.range.start);
    }
    for d in crate::check::rules::field_default_type::check(&tree, resolved, src) {
        spans.insert(d.range.start);
    }
    spans
}

// ---------------------------------------------- string / number proof helpers --

fn is_string(t: &CheckTy) -> bool {
    matches!(t.widen(), CheckTy::String)
}
fn is_number(t: &CheckTy) -> bool {
    matches!(t.widen(), CheckTy::Number)
}

/// Is `t` PROVABLY not a number? `Any`/`Never`/a union with a numeric member are
/// NOT provable (stay silent).
fn is_provably_non_number(t: &CheckTy) -> bool {
    use CheckTy::*;
    match t.widen() {
        Any | Never | Number => false,
        Union(ms) => ms.iter().all(is_provably_non_number),
        String | Bool | Nil | Bytes | Object | Regex | Error | Fn | Array(_) | Map(_, _)
        | Tuple(_) | Result(_) | Future(_) | Class(_) | Enum(_) => true,
        EnumVariant(_, _) | Literal(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;

    fn codes(src: &str) -> Vec<String> {
        analyze(src)
            .diagnostics
            .into_iter()
            .map(|d| d.code)
            .collect()
    }
    fn has(src: &str, code: &str) -> bool {
        codes(src).iter().any(|c| c == code)
    }
    fn count(src: &str, code: &str) -> usize {
        codes(src).iter().filter(|c| c.as_str() == code).count()
    }

    #[test]
    fn annotated_let_mismatch() {
        assert!(has("let n: number = \"x\"\n", "type-mismatch"));
    }

    #[test]
    fn type_error_arith_on_string_slot() {
        assert!(has("let x: string = \"a\"\nx - 1\n", "type-error"));
    }

    #[test]
    fn nonliteral_arg_mismatch() {
        let src = "fn f(n: number) { return n }\nlet s = \"x\"\nf(s)\n";
        assert!(has(src, "type-mismatch"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn literal_arg_mismatch_dedup_with_legacy() {
        let src = "fn f(n: number) { return n }\nf(\"x\")\n";
        assert!(has(src, "contract-mismatch"));
        // de-dup: type-mismatch suppressed at the legacy span.
        assert_eq!(
            count(src, "type-mismatch"),
            0,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn field_default_subsumed_dedup() {
        let src = "class P { n: number = \"x\" }\n";
        assert!(has(src, "field-default-type"));
        assert_eq!(count(src, "type-mismatch"), 0);
    }

    #[test]
    fn unknown_callee_silent() {
        assert!(!has("let x = foo()\nx - 1\n", "type-error"));
        assert!(!has("let x = foo()\nx - 1\n", "type-mismatch"));
    }

    #[test]
    fn unannotated_param_silent() {
        assert!(!has("fn g(x) { return x }\ng(\"x\")\n", "type-mismatch"));
    }

    #[test]
    fn any_typed_silent() {
        assert!(!has("let a: any = 1\na - \"x\"\n", "type-error"));
    }

    #[test]
    fn correct_annotated_let_silent() {
        assert!(!has("let n: number = 1\n", "type-mismatch"));
        assert!(!has("let s: string = \"ok\"\n", "type-mismatch"));
    }

    #[test]
    fn union_param_accepts() {
        let src = "fn f(x: number | string) { return x }\nf(\"ok\")\nf(1)\n";
        assert!(!has(src, "type-mismatch"));
    }

    #[test]
    fn return_against_declared_type() {
        let src = "fn f(): number { return \"x\" }\n";
        assert!(has(src, "type-mismatch"), "{:?}", analyze(src).diagnostics);
    }

    // ----- T3.1 local return inference -----

    #[test]
    fn inferred_return_flows_to_slot() {
        // id returns number (inferred); y: number; z: string = y → type-mismatch.
        let src = "fn id(x: number) { return x }\nlet y = id(1)\nlet z: string = y\n";
        assert!(has(src, "type-mismatch"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn recursive_fn_no_false_positive() {
        // a recursive fn under inference resolves self-calls to Any → no FP.
        let src = "fn f(n: number) {\n  if (n <= 0) { return 0 }\n  return f(n - 1)\n}\nlet z: string = f(3)\n";
        // inferred return = join(0:number, Any) = Any → silent (no mismatch).
        assert!(!has(src, "type-mismatch"), "{:?}", analyze(src).diagnostics);
    }

    // ----- T3.2 nil-guard narrowing + possibly-nil -----

    #[test]
    fn possibly_nil_deref_without_guard() {
        let src = "fn f(x: number?) { return x + 1 }\n";
        assert!(has(src, "possibly-nil"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn nil_guard_then_branch_narrows() {
        let src = "fn f(x: number?) {\n  if (x != nil) { return x + 1 }\n  return 0\n}\n";
        assert!(!has(src, "possibly-nil"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn early_return_eq_nil_narrows_tail() {
        let src = "fn f(x: number?) {\n  if (x == nil) { return 0 }\n  return x + 1\n}\n";
        assert!(!has(src, "possibly-nil"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn coalesce_narrows() {
        let src = "fn f(x: number?) {\n  let y = x ?? 0\n  return y + 1\n}\n";
        assert!(!has(src, "possibly-nil"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn truthiness_narrows_nil() {
        let src = "fn f(x: number?) {\n  if (x) { return x + 1 }\n  return 0\n}\n";
        assert!(!has(src, "possibly-nil"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn possibly_nil_member_without_guard() {
        let src = "class P { n: number }\nfn f(p: P?) { return p.n }\n";
        assert!(has(src, "possibly-nil"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn opt_member_does_not_fire_possibly_nil() {
        let src = "class P { n: number }\nfn f(p: P?) { return p?.n }\n";
        assert!(!has(src, "possibly-nil"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn any_receiver_no_possibly_nil() {
        // an unannotated (Any) param → no possibly-nil.
        let src = "fn f(x) { return x + 1 }\n";
        assert!(!has(src, "possibly-nil"));
    }

    // ----- T4 instanceof narrowing (SP2 landed) -----

    #[test]
    fn instanceof_narrows_then_branch() {
        // x narrowed to Dog → x.bark() member resolves; no diagnostics.
        let src = "class Dog { fn bark(): nil { return nil } }\nfn f(x) {\n  if (x instanceof Dog) { return x.name }\n  return nil\n}\n";
        assert!(!has(src, "possibly-nil"), "{:?}", analyze(src).diagnostics);
        assert!(!has(src, "type-error"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn instanceof_unknown_class_silent() {
        // RHS not a known class → no narrowing, no FP.
        let src = "fn f(x) {\n  if (x instanceof Nonexistent) { return x }\n  return nil\n}\n";
        assert!(!has(src, "type-error"), "{:?}", analyze(src).diagnostics);
        assert!(!has(src, "possibly-nil"));
    }

    // ----- T4 early-return flow merge (also exercised in T3) -----

    #[test]
    fn early_return_break_narrows_tail_in_loop_free_block() {
        // after `if (x == nil) { return 0 }` the tail sees x : number.
        let src = "fn f(x: number?): number {\n  if (x == nil) { return 0 }\n  let y = x + 1\n  return y\n}\n";
        assert!(!has(src, "possibly-nil"), "{:?}", analyze(src).diagnostics);
    }

    // ----- worker fn call synthesizes future<T> (SP workers Task 11) -----

    #[test]
    fn worker_call_infers_future_like_async() {
        // Awaiting a worker fn yields the scalar; the inference must NOT flag a
        // type-mismatch when the awaited number is used as a number.
        let src = "
            worker fn sq(n: number): number { return n * n }
            fn use_it(): number { return await sq(3) }
        ";
        assert!(
            !codes(src).iter().any(|c| c.starts_with("type-")),
            "worker fn awaited call produced unexpected type-* diagnostics: {:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn worker_call_unawaited_no_false_positive() {
        // A non-awaited worker fn call (assigned to a let) must not cause
        // type-mismatch or possibly-nil false positives.
        let src = "
            worker fn sq(n: number): number { return n * n }
            let f = sq(3)
        ";
        assert!(
            !codes(src).iter().any(|c| c.starts_with("type-")),
            "unawaited worker fn call produced unexpected type-* diagnostics: {:?}",
            analyze(src).diagnostics
        );
    }
}
