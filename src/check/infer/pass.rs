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
        for stmt in node.children() {
            self.walk_stmt(stmt, env);
        }
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

    /// Walk a block in a child environment seeded from `env` (block scoping).
    fn walk_child_block(&mut self, block: &ResolvedNode, env: &Env) {
        let mut inner = Env::new();
        copy_env(env, &mut inner);
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
        for block in stmt.children().filter(|c| c.kind() == SyntaxKind::Block) {
            self.walk_child_block(block, env);
        }
        for c in stmt.children().filter(|c| c.kind() == SyntaxKind::IfStmt) {
            self.walk_if(c, env);
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
            let Some(name_tok) = param
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .find(|t| t.kind() == SyntaxKind::Ident)
            else {
                continue;
            };
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
            if let Some(key) = self.key_for_use(&name_tok.text_range()) {
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
            MatchExpr => {
                self.synth_children_exprs(expr, env);
                CheckTy::Any
            }
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
        env.lookup(&key).unwrap_or(CheckTy::Any)
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
                // string/number result is synthesizable; never a type-error.
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

    fn synth_member(&mut self, expr: &ResolvedNode, env: &mut Env) -> CheckTy {
        let recv = expr
            .children()
            .find(|c| crate::check::rules::is_expr_kind(c.kind()));
        let recv_ty = recv.map(|r| self.synth(r, env)).unwrap_or(CheckTy::Any);
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
        if is_async(fn_decl) {
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

    /// Infer a function's return type (T3: join of return synths + Nil if it can
    /// fall off the end). T2: `Any`.
    fn infer_return(&mut self, fn_decl: &ResolvedNode) -> CheckTy {
        let _ = fn_decl;
        CheckTy::Any
    }
}

// ============================================================ free helpers ====

/// Shallow-copy base bindings of `from` into `to` (block scoping).
fn copy_env(from: &Env, to: &mut Env) {
    for (k, v) in from.iter_base() {
        to.define(k.clone(), v.clone());
    }
}

/// First expression-kind child of a node.
fn first_expr_child(node: &ResolvedNode) -> Option<ResolvedNode> {
    node.children()
        .find(|c| crate::check::rules::is_expr_kind(c.kind()))
        .cloned()
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
}
