//! Async tree-walking evaluator. `eval_expr`/`exec` are async to establish
//! the event-loop seam from spec §7, even though the skeleton never suspends.

use crate::ast::{BinOp, Expr, ExprKind, Stmt, UnOp};
use crate::env::{AssignError, Environment};
use crate::error::AsError;
use crate::span::Span;
use crate::value::Value;
use crate::{lexer, parser};
use async_recursion::async_recursion;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The callable parts shared by plain functions and methods: parameter list,
/// optional return contract, and the body to execute.
struct BodySpec<'a> {
    params: &'a [crate::ast::Param],
    ret: &'a Option<crate::ast::Type>,
    body: &'a [Stmt],
}

/// Non-local control-flow signal produced while executing statements.
#[derive(Debug)]
pub enum Flow {
    Normal,
    Return(Value),
    Break,
    Continue,
}

/// Non-local exit from expression/statement evaluation.
#[derive(Debug)]
pub enum Control {
    /// An unrecoverable programmer error (spec §6 Tier 2). Aborts unless caught
    /// by `recover`. Carries the diagnostic.
    Panic(AsError),
    /// A `?`-operator early return: the enclosing function should return this
    /// `[nil, err]` Result pair.
    Propagate(Value),
}

impl From<AsError> for Control {
    fn from(e: AsError) -> Self {
        Control::Panic(e)
    }
}

/// A fresh global environment with the built-in functions installed.
pub fn global_env() -> Environment {
    let env = Environment::global();
    for name in ["print", "Ok", "Err", "assert", "recover", "test", "len", "type", "range"] {
        env.define(name, Value::Builtin(name.into()), false)
            .expect("global env starts empty");
    }
    env
}

/// Build a `[value, err]` Result pair.
fn make_pair(value: Value, err: Value) -> Value {
    Value::Array(Rc::new(RefCell::new(vec![value, err])))
}

/// Build an error object `{ message: <msg> }`.
fn make_error(msg: Value) -> Value {
    let mut map = indexmap::IndexMap::new();
    map.insert("message".to_string(), msg);
    Value::Object(Rc::new(RefCell::new(map)))
}

#[derive(Clone)]
pub struct ModuleEntry {
    pub env: Environment,
    pub exports: Rc<RefCell<HashSet<String>>>,
}

pub struct Interp {
    /// Captured program output (what `print` writes). Exposed for testing and
    /// flushed to stdout by the CLI.
    pub output: String,
    modules: HashMap<PathBuf, ModuleEntry>,
    module_dir: PathBuf,
    current_exports: Rc<RefCell<HashSet<String>>>,
    /// Tests registered via the `test(name, fn)` builtin. Only executed by
    /// `ascript test` (via `run_registered_tests`); a normal `run` just collects them.
    tests: Vec<(String, Value)>,
}

/// Outcome of running the tests registered on an `Interp`.
#[derive(Debug, Default)]
pub struct TestSummary {
    pub passed: usize,
    pub failed: usize,
    /// `(test name, failure message)` for each failed test.
    pub failures: Vec<(String, String)>,
}

impl Interp {
    pub fn new() -> Self {
        Interp {
            output: String::new(),
            modules: HashMap::new(),
            module_dir: PathBuf::from("."),
            current_exports: Rc::new(RefCell::new(HashSet::new())),
            tests: Vec::new(),
        }
    }

    /// Run every test registered via the `test(name, fn)` builtin. Each test fn
    /// is invoked with no arguments; a `Control::Panic` (e.g. a failed `assert`)
    /// is recorded as a failure, while a clean return or a `?` propagation passes.
    pub async fn run_registered_tests(&mut self) -> TestSummary {
        let mut summary = TestSummary::default();
        // Clone out the registrations first: `call_value` borrows `&mut self`.
        let tests = self.tests.clone();
        for (name, func) in tests {
            match self.call_value(func, Vec::new(), Span::new(0, 0)).await {
                Ok(_) | Err(Control::Propagate(_)) => summary.passed += 1,
                Err(Control::Panic(e)) => {
                    summary.failed += 1;
                    summary.failures.push((name, e.message));
                }
            }
        }
        summary
    }

    /// Load (or fetch from cache) the module at `path`, returning its entry.
    #[async_recursion(?Send)]
    pub async fn load_module(&mut self, path: &Path) -> Result<ModuleEntry, Control> {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(entry) = self.modules.get(&canon) {
            return Ok(entry.clone()); // cached, or in-progress (circular)
        }
        let src = std::fs::read_to_string(&canon)
            .map_err(|e| Control::Panic(AsError::new(format!("cannot read module {}: {}", canon.display(), e))))?;
        let env = global_env();
        let exports = Rc::new(RefCell::new(HashSet::new()));
        let entry = ModuleEntry { env: env.clone(), exports: exports.clone() };
        // Cache BEFORE executing so circular imports resolve to this entry.
        self.modules.insert(canon.clone(), entry.clone());

        let dir = canon.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
        let prev_dir = std::mem::replace(&mut self.module_dir, dir);
        let prev_exports = std::mem::replace(&mut self.current_exports, exports);

        let src_info =
            Rc::new(crate::error::SourceInfo { path: canon.display().to_string(), text: src.clone() });

        let tokens = lexer::lex(&src)
            .map_err(|e| Control::Panic(e.with_source(src_info.clone())))?;
        let program = parser::parse(&tokens)
            .map_err(|e| Control::Panic(e.with_source(src_info.clone())))?;
        let result = self.exec(&program, &env).await;

        self.module_dir = prev_dir;
        self.current_exports = prev_exports;

        if let Err(Control::Panic(e)) = result {
            return Err(Control::Panic(e.with_source(src_info)));
        }
        result?; // propagate any other control flow from the module body
        Ok(entry)
    }

    fn resolve_import(&self, source: &str) -> PathBuf {
        let mut p = self.module_dir.join(source);
        if p.extension().is_none() {
            p.set_extension("as");
        }
        p
    }

    #[async_recursion(?Send)]
    pub async fn exec(&mut self, program: &[Stmt], env: &Environment) -> Result<Flow, Control> {
        for stmt in program {
            match self.exec_stmt(stmt, env).await? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    #[async_recursion(?Send)]
    async fn exec_stmt(&mut self, stmt: &Stmt, env: &Environment) -> Result<Flow, Control> {
        match stmt {
            Stmt::Expr(e) => {
                self.eval_expr(e, env).await?;
                Ok(Flow::Normal)
            }
            Stmt::Let { name, ty, value, mutable } => {
                let v = self.eval_expr(value, env).await?;
                if let Some(ty) = ty {
                    if !check_type(&v, ty) {
                        return Err(contract_panic(ty, &v, value.span));
                    }
                }
                env.define(name, v, *mutable).map_err(AsError::new)?;
                Ok(Flow::Normal)
            }
            Stmt::LetDestructure { names, value, mutable } => {
                let v = self.eval_expr(value, env).await?;
                let items = match v {
                    Value::Array(a) => a.borrow().clone(),
                    other => {
                        return Err(AsError::at(
                            format!("cannot destructure a non-array value of type {}", type_name(&other)),
                            value.span,
                        )
                        .into())
                    }
                };
                for (i, name) in names.iter().enumerate() {
                    let elem = items.get(i).cloned().unwrap_or(Value::Nil);
                    env.define(name, elem, *mutable).map_err(AsError::new)?;
                }
                Ok(Flow::Normal)
            }
            Stmt::Block(stmts) => {
                let child = env.child();
                self.exec(stmts, &child).await
            }
            Stmt::If { cond, then_branch, else_branch } => {
                if self.eval_expr(cond, env).await?.is_truthy() {
                    let child = env.child();
                    self.exec(then_branch, &child).await
                } else if let Some(else_stmts) = else_branch {
                    let child = env.child();
                    self.exec(else_stmts, &child).await
                } else {
                    Ok(Flow::Normal)
                }
            }
            Stmt::While { cond, body } => {
                while self.eval_expr(cond, env).await?.is_truthy() {
                    let child = env.child();
                    match self.exec(body, &child).await? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Continue | Flow::Normal => {}
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::ForRange { var, start, end, body } => {
                let start_v = self.eval_expr(start, env).await?;
                let end_v = self.eval_expr(end, env).await?;
                let (lo, hi) = match (start_v, end_v) {
                    (Value::Number(a), Value::Number(b)) => (a, b),
                    _ => return Err(AsError::at("for-range bounds must be numbers", start.span).into()),
                };
                let mut i = lo;
                while i < hi {
                    let child = env.child();
                    child.define(var, Value::Number(i), false).map_err(AsError::new)?;
                    match self.exec(body, &child).await? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Continue | Flow::Normal => {}
                    }
                    i += 1.0;
                }
                Ok(Flow::Normal)
            }
            Stmt::ForOf { var, iter, body } => {
                let iterable = self.eval_expr(iter, env).await?;
                let items: Vec<Value> = match iterable {
                    Value::Array(arr) => arr.borrow().clone(),
                    Value::Str(s) => s.chars().map(|c| Value::Str(c.to_string().into())).collect(),
                    other => {
                        return Err(AsError::at(
                            format!("value of type {} is not iterable", type_name(&other)),
                            iter.span,
                        )
                        .into())
                    }
                };
                for item in items {
                    let child = env.child();
                    child.define(var, item, false).map_err(AsError::new)?;
                    match self.exec(body, &child).await? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Continue | Flow::Normal => {}
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval_expr(e, env).await?,
                    None => Value::Nil,
                };
                Ok(Flow::Return(v))
            }
            Stmt::Break => Ok(Flow::Break),
            Stmt::Continue => Ok(Flow::Continue),
            Stmt::Fn { name, params, ret, body, is_async } => {
                let func = Value::Function(std::rc::Rc::new(crate::value::Function {
                    name: Some(name.clone()),
                    params: params.clone(),
                    ret: ret.clone(),
                    body: body.clone(),
                    closure: env.clone(),
                    is_async: *is_async,
                }));
                env.define(name, func, false).map_err(AsError::new)?;
                Ok(Flow::Normal)
            }
            Stmt::Enum { name, variants } => {
                let mut map = indexmap::IndexMap::new();
                for v in variants {
                    let backing = match &v.value {
                        Some(e) => self.eval_expr(e, env).await?,
                        None => Value::Nil,
                    };
                    let variant = Value::EnumVariant(std::rc::Rc::new(crate::value::EnumVariant {
                        enum_name: name.clone(),
                        name: v.name.clone(),
                        value: backing,
                    }));
                    map.insert(v.name.clone(), variant);
                }
                let def = Value::Enum(std::rc::Rc::new(crate::value::EnumDef { name: name.clone(), variants: map }));
                env.define(name, def, false).map_err(AsError::new)?;
                Ok(Flow::Normal)
            }
            Stmt::Class { name, superclass, methods } => {
                let parent = match superclass {
                    Some(sup_name) => match env.get(sup_name) {
                        Some(Value::Class(c)) => Some(c),
                        Some(_) => return Err(AsError::new(format!("'{}' is not a class", sup_name)).into()),
                        None => return Err(AsError::new(format!("undefined superclass '{}'", sup_name)).into()),
                    },
                    None => None,
                };
                let mut method_map = indexmap::IndexMap::new();
                for m in methods {
                    method_map.insert(m.name.clone(), std::rc::Rc::new(crate::value::Method {
                        params: m.params.clone(),
                        ret: m.ret.clone(),
                        body: m.body.clone(),
                        is_async: m.is_async,
                    }));
                }
                let class = Value::Class(std::rc::Rc::new(crate::value::Class {
                    name: name.clone(),
                    superclass: parent,
                    methods: method_map,
                    def_env: env.clone(),
                }));
                env.define(name, class, false).map_err(AsError::new)?;
                Ok(Flow::Normal)
            }
            Stmt::Export(inner) => {
                let flow = self.exec_stmt(inner, env).await?;
                for name in exported_names(inner) {
                    self.current_exports.borrow_mut().insert(name);
                }
                Ok(flow)
            }
            Stmt::Import { names, source } => {
                let resolved = self.resolve_import(source);
                let entry = self.load_module(&resolved).await?;
                match names {
                    crate::ast::ImportNames::Named(names) => {
                        for name in names {
                            if !entry.exports.borrow().contains(name) {
                                return Err(AsError::new(format!("module '{}' has no export '{}'", source, name)).into());
                            }
                            let v = entry.env.get(name).unwrap_or(Value::Nil);
                            env.define(name, v, false).map_err(AsError::new)?;
                        }
                    }
                    crate::ast::ImportNames::Namespace(alias) => {
                        let mut map = indexmap::IndexMap::new();
                        for name in entry.exports.borrow().iter() {
                            map.insert(name.clone(), entry.env.get(name).unwrap_or(Value::Nil));
                        }
                        env.define(alias, Value::Object(Rc::new(RefCell::new(map))), false)
                            .map_err(AsError::new)?;
                    }
                }
                Ok(Flow::Normal)
            }
        }
    }

    #[async_recursion(?Send)]
    pub async fn eval_expr(&mut self, expr: &Expr, env: &Environment) -> Result<Value, Control> {
        match &expr.kind {
            ExprKind::Number(n) => Ok(Value::Number(*n)),
            ExprKind::Str(s) => Ok(Value::Str(s.as_str().into())),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Nil => Ok(Value::Nil),
            ExprKind::Ident(name) => env
                .get(name)
                .ok_or_else(|| AsError::at(format!("undefined variable '{}'", name), expr.span).into()),
            ExprKind::Assign { target, value } => {
                let v = self.eval_expr(value, env).await?;
                self.assign_to(target, v, env).await
            }
            ExprKind::Unary { op, expr: operand } => {
                let v = self.eval_expr(operand, env).await?;
                match op {
                    UnOp::Neg => match v {
                        Value::Number(n) => Ok(Value::Number(-n)),
                        _ => Err(AsError::at("cannot negate a non-number", operand.span).into()),
                    },
                    UnOp::Not => Ok(Value::Bool(!v.is_truthy())),
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                match op {
                    BinOp::And => {
                        let l = self.eval_expr(lhs, env).await?;
                        return if l.is_truthy() { self.eval_expr(rhs, env).await } else { Ok(l) };
                    }
                    BinOp::Or => {
                        let l = self.eval_expr(lhs, env).await?;
                        return if l.is_truthy() { Ok(l) } else { self.eval_expr(rhs, env).await };
                    }
                    BinOp::Coalesce => {
                        let l = self.eval_expr(lhs, env).await?;
                        return if l == Value::Nil { self.eval_expr(rhs, env).await } else { Ok(l) };
                    }
                    _ => {}
                }

                let l = self.eval_expr(lhs, env).await?;
                let r = self.eval_expr(rhs, env).await?;

                match op {
                    BinOp::Eq => return Ok(Value::Bool(l == r)),
                    BinOp::Ne => return Ok(Value::Bool(l != r)),
                    _ => {}
                }

                // String concatenation: `+` joins two strings.
                if let BinOp::Add = op {
                    if let (Value::Str(a), Value::Str(b)) = (&l, &r) {
                        return Ok(Value::Str(format!("{}{}", a, b).into()));
                    }
                }

                let (a, b) = match (&l, &r) {
                    (Value::Number(a), Value::Number(b)) => (*a, *b),
                    _ => return Err(AsError::at("operator requires two numbers", expr.span).into()),
                };
                let result = match op {
                    BinOp::Add => Value::Number(a + b),
                    BinOp::Sub => Value::Number(a - b),
                    BinOp::Mul => Value::Number(a * b),
                    BinOp::Div => Value::Number(a / b),
                    BinOp::Mod => Value::Number(a % b),
                    BinOp::Pow => Value::Number(a.powf(b)),
                    BinOp::Lt => Value::Bool(a < b),
                    BinOp::Le => Value::Bool(a <= b),
                    BinOp::Gt => Value::Bool(a > b),
                    BinOp::Ge => Value::Bool(a >= b),
                    BinOp::Eq | BinOp::Ne | BinOp::And | BinOp::Or | BinOp::Coalesce => {
                        unreachable!("handled above")
                    }
                };
                Ok(result)
            }
            ExprKind::Arrow { params, body, is_async } => {
                let body_stmts = match body.as_ref() {
                    crate::ast::ArrowBody::Block(stmts) => stmts.clone(),
                    crate::ast::ArrowBody::Expr(e) => vec![Stmt::Return(Some((**e).clone()))],
                };
                Ok(Value::Function(std::rc::Rc::new(crate::value::Function {
                    name: None,
                    params: params.clone(),
                    ret: None,
                    body: body_stmts,
                    closure: env.clone(),
                    is_async: *is_async,
                })))
            }
            ExprKind::Array(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval_expr(item, env).await?);
                }
                Ok(Value::Array(Rc::new(RefCell::new(values))))
            }
            ExprKind::Object(entries) => {
                let mut map = indexmap::IndexMap::with_capacity(entries.len());
                for (k, v) in entries {
                    let value = self.eval_expr(v, env).await?;
                    map.insert(k.clone(), value);
                }
                Ok(Value::Object(std::rc::Rc::new(std::cell::RefCell::new(map))))
            }
            ExprKind::Template { parts } => {
                let mut out = String::new();
                for part in parts {
                    match part {
                        crate::ast::TemplatePart::Lit(s) => out.push_str(s),
                        crate::ast::TemplatePart::Expr(e) => {
                            let v = self.eval_expr(e, env).await?;
                            out.push_str(&v.to_string());
                        }
                    }
                }
                Ok(Value::Str(out.into()))
            }
            ExprKind::Match { subject, arms } => {
                let subj = self.eval_expr(subject, env).await?;
                for arm in arms {
                    let matched = match &arm.patterns {
                        None => true, // wildcard
                        Some(pats) => {
                            let mut hit = false;
                            for p in pats {
                                if self.eval_expr(p, env).await? == subj {
                                    hit = true;
                                    break;
                                }
                            }
                            hit
                        }
                    };
                    if matched {
                        return self.eval_expr(&arm.body, env).await;
                    }
                }
                Err(AsError::at("no matching arm in match expression", expr.span).into())
            }
            ExprKind::Await(inner) => self.eval_expr(inner, env).await,
            ExprKind::Paren(inner) => self.eval_expr(inner, env).await,
            ExprKind::Try(inner) => {
                let v = self.eval_expr(inner, env).await?;
                // Must be a 2-element Result pair [value, err].
                let arr = match &v {
                    Value::Array(a) if a.borrow().len() == 2 => a.clone(),
                    _ => {
                        return Err(AsError::at(
                            "the ? operator requires a Result pair [value, err]",
                            expr.span,
                        )
                        .into())
                    }
                };
                let (value, err) = {
                    let b = arr.borrow();
                    (b[0].clone(), b[1].clone())
                };
                if err == Value::Nil {
                    Ok(value)
                } else {
                    // Early-return [nil, err] from the enclosing function.
                    Err(Control::Propagate(make_pair(Value::Nil, err)))
                }
            }
            ExprKind::OptMember { .. }
            | ExprKind::Member { .. }
            | ExprKind::Index { .. }
            | ExprKind::Call { .. } => {
                let (v, _) = self.eval_chain(expr, env).await?;
                Ok(v)
            }
        }
    }

    /// Evaluate a member/index/call chain, returning (value, short_circuited).
    /// `short_circuited == true` means an earlier `?.` link hit nil and the rest
    /// of the chain must yield nil without being accessed/called.
    #[async_recursion(?Send)]
    async fn eval_chain(&mut self, expr: &Expr, env: &Environment) -> Result<(Value, bool), Control> {
        match &expr.kind {
            ExprKind::OptMember { object, name } => {
                let (obj, sc) = self.eval_chain(object, env).await?;
                if sc || obj == Value::Nil {
                    return Ok((Value::Nil, true));
                }
                Ok((self.read_member(&obj, name, object.span)?, false))
            }
            ExprKind::Member { object, name } => {
                let (obj, sc) = self.eval_chain(object, env).await?;
                if sc {
                    return Ok((Value::Nil, true));
                }
                Ok((self.read_member(&obj, name, object.span)?, false))
            }
            ExprKind::Index { object, index } => {
                let (obj, sc) = self.eval_chain(object, env).await?;
                if sc {
                    return Ok((Value::Nil, true));
                }
                let idx = self.eval_expr(index, env).await?;
                let v = match obj {
                    Value::Array(arr) => {
                        let i = array_index(&idx, expr.span)?;
                        let arr = arr.borrow();
                        arr.get(i)
                            .cloned()
                            .ok_or_else(|| AsError::at(format!("index {} out of bounds (len {})", i, arr.len()), expr.span))
                    }
                    Value::Object(map) => match idx {
                        Value::Str(key) => Ok(map.borrow().get(key.as_ref()).cloned().unwrap_or(Value::Nil)),
                        _ => Err(AsError::at("object index must be a string", expr.span)),
                    },
                    _ => Err(AsError::at("cannot index this value", object.span)),
                };
                Ok((v?, false))
            }
            ExprKind::Call { callee, args } => {
                let (callee_v, sc) = self.eval_chain(callee, env).await?;
                if sc {
                    return Ok((Value::Nil, true));
                }
                let mut values = Vec::new();
                for a in args {
                    values.push(self.eval_expr(a, env).await?);
                }
                let v = self.call_value(callee_v, values, expr.span).await;
                Ok((v?, false))
            }
            _ => Ok((self.eval_expr(expr, env).await?, false)),
        }
    }

    fn read_member(&self, obj: &Value, name: &str, span: Span) -> Result<Value, AsError> {
        match obj {
            Value::Object(map) => Ok(map.borrow().get(name).cloned().unwrap_or(Value::Nil)),
            Value::Enum(e) => e
                .variants
                .get(name)
                .cloned()
                .ok_or_else(|| AsError::at(format!("enum {} has no variant '{}'", e.name, name), span)),
            Value::EnumVariant(v) => match name {
                "name" => Ok(Value::Str(v.name.as_str().into())),
                "value" => Ok(v.value.clone()),
                other => Err(AsError::at(format!("enum variant has no property '{}'", other), span)),
            },
            Value::Instance(inst) => {
                let b = inst.borrow();
                if let Some(v) = b.fields.get(name) {
                    return Ok(v.clone());
                }
                match crate::value::find_method(&b.class, name) {
                    Some((method, def_class)) => Ok(Value::BoundMethod(std::rc::Rc::new(crate::value::BoundMethod {
                        receiver: obj.clone(),
                        method,
                        defining_class: def_class,
                        name: name.to_string(),
                    }))),
                    None => Ok(Value::Nil),
                }
            }
            Value::Super(s) => match &s.start {
                Some(start) => match crate::value::find_method(start, name) {
                    Some((method, def_class)) => Ok(Value::BoundMethod(std::rc::Rc::new(crate::value::BoundMethod {
                        receiver: s.receiver.clone(),
                        method,
                        defining_class: def_class,
                        name: name.to_string(),
                    }))),
                    None => Err(AsError::at(format!("no superclass method '{}'", name), span)),
                },
                None => Err(AsError::at(format!("no superclass method '{}' (no superclass)", name), span)),
            },
            Value::Nil => Err(AsError::at(format!("cannot read property '{}' of nil", name), span)),
            _ => Err(AsError::at(format!("cannot read property '{}' of this value", name), span)),
        }
    }

    #[async_recursion(?Send)]
    async fn call_value(&mut self, callee: Value, args: Vec<Value>, span: Span) -> Result<Value, Control> {
        match callee {
            Value::Builtin(name) => self.call_builtin(&name, &args, span).await,
            Value::Function(func) => self.call_function(&func, args, span).await,
            Value::Class(class) => self.construct(class, args, span).await,
            Value::BoundMethod(bm) => self.invoke_method(&bm, args, span).await,
            _ => Err(AsError::at("value is not callable", span).into()),
        }
    }

    /// Bind params (with contracts), run a body in `call_env`, apply the return
    /// contract. Shared by plain functions and methods.
    #[async_recursion(?Send)]
    async fn run_body<'s: 'async_recursion>(
        &mut self,
        spec: BodySpec<'s>,
        args: Vec<Value>,
        call_env: &Environment,
        span: Span,
        what: &str,
    ) -> Result<Value, Control> {
        let BodySpec { params, ret, body } = spec;
        if args.len() != params.len() {
            return Err(AsError::at(
                format!("{} expected {} argument(s), got {}", what, params.len(), args.len()),
                span,
            )
            .into());
        }
        for (p, a) in params.iter().zip(args.into_iter()) {
            if let Some(ty) = &p.ty {
                if !check_type(&a, ty) {
                    return Err(contract_panic(ty, &a, span));
                }
            }
            call_env.define(&p.name, a, true).map_err(AsError::new)?;
        }
        let result = match self.exec(body, call_env).await {
            Ok(Flow::Return(v)) => v,
            Ok(Flow::Normal) => Value::Nil,
            Ok(Flow::Break) => return Err(AsError::at("'break' outside of a loop", span).into()),
            Ok(Flow::Continue) => return Err(AsError::at("'continue' outside of a loop", span).into()),
            Err(Control::Propagate(v)) => v,
            Err(Control::Panic(e)) => return Err(Control::Panic(e)),
        };
        if let Some(ty) = ret {
            if !check_type(&result, ty) {
                return Err(contract_panic(ty, &result, span));
            }
        }
        Ok(result)
    }

    #[async_recursion(?Send)]
    async fn call_function(
        &mut self,
        func: &crate::value::Function,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let call_env = func.closure.child();
        let what = func.name.as_deref().unwrap_or("function").to_string();
        let spec = BodySpec { params: &func.params, ret: &func.ret, body: &func.body };
        self.run_body(spec, args, &call_env, span, &what).await
    }

    #[async_recursion(?Send)]
    async fn construct(
        &mut self,
        class: std::rc::Rc<crate::value::Class>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let instance = std::rc::Rc::new(std::cell::RefCell::new(crate::value::Instance {
            class: class.clone(),
            fields: indexmap::IndexMap::new(),
        }));
        let inst_val = Value::Instance(instance);
        match crate::value::find_method(&class, "init") {
            Some((method, def_class)) => {
                let bm = crate::value::BoundMethod {
                    receiver: inst_val.clone(),
                    method,
                    defining_class: def_class,
                    name: "init".to_string(),
                };
                self.invoke_method(&bm, args, span).await?;
            }
            None => {
                if !args.is_empty() {
                    return Err(AsError::at(
                        format!("{} has no init but was given {} argument(s)", class.name, args.len()),
                        span,
                    )
                    .into());
                }
            }
        }
        Ok(inst_val)
    }

    #[async_recursion(?Send)]
    async fn invoke_method(
        &mut self,
        bm: &crate::value::BoundMethod,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let call_env = bm.defining_class.def_env.child();
        call_env.define("self", bm.receiver.clone(), false).map_err(AsError::new)?;
        // `super` lookup begins at the defining class's superclass.
        let super_ref = Value::Super(std::rc::Rc::new(crate::value::SuperRef {
            receiver: bm.receiver.clone(),
            start: bm.defining_class.superclass.clone(),
        }));
        call_env.define("super", super_ref, false).map_err(AsError::new)?;
        let spec = BodySpec { params: &bm.method.params, ret: &bm.method.ret, body: &bm.method.body };
        self.run_body(spec, args, &call_env, span, &bm.name).await
    }

    #[async_recursion(?Send)]
    async fn call_builtin(&mut self, name: &str, args: &[Value], span: Span) -> Result<Value, Control> {
        match name {
            "print" => {
                let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
                self.output.push_str(&parts.join(" "));
                self.output.push('\n');
                Ok(Value::Nil)
            }
            "Ok" => {
                let value = args.first().cloned().unwrap_or(Value::Nil);
                Ok(make_pair(value, Value::Nil))
            }
            "Err" => {
                let msg = args.first().cloned().unwrap_or(Value::Nil);
                Ok(make_pair(Value::Nil, make_error(msg)))
            }
            "assert" => {
                let cond = args.first().cloned().unwrap_or(Value::Nil);
                if cond.is_truthy() {
                    Ok(Value::Nil)
                } else {
                    let msg = match args.get(1) {
                        Some(Value::Str(s)) => s.to_string(),
                        Some(v) => v.to_string(),
                        None => "assertion failed".to_string(),
                    };
                    Err(AsError::at(msg, span).into())
                }
            }
            "recover" => {
                let callee = args.first().cloned().unwrap_or(Value::Nil);
                match self.call_value(callee, Vec::new(), span).await {
                    Ok(v) => Ok(make_pair(v, Value::Nil)),
                    Err(Control::Panic(e)) => {
                        Ok(make_pair(Value::Nil, make_error(Value::Str(e.message.into()))))
                    }
                    // A `?` propagation inside `fn` is already converted to fn's return
                    // value by call_function, so this is unreachable in practice; pass it through.
                    Err(Control::Propagate(v)) => Err(Control::Propagate(v)),
                }
            }
            "test" => {
                let name = match args.first() {
                    Some(Value::Str(s)) => s.to_string(),
                    Some(v) => v.to_string(),
                    None => "<unnamed>".to_string(),
                };
                let func = args.get(1).cloned().unwrap_or(Value::Nil);
                // Register only; `ascript test` runs these via run_registered_tests.
                self.tests.push((name, func));
                Ok(Value::Nil)
            }
            "len" => {
                let v = args.first().cloned().unwrap_or(Value::Nil);
                let n = match &v {
                    Value::Str(s) => s.chars().count(),
                    Value::Array(a) => a.borrow().len(),
                    Value::Object(o) => o.borrow().len(),
                    _ => {
                        return Err(AsError::at(
                            format!("len() expects a string, array, object, or map, got {}", type_name(&v)),
                            span,
                        )
                        .into())
                    }
                };
                Ok(Value::Number(n as f64))
            }
            "type" => {
                let v = args.first().cloned().unwrap_or(Value::Nil);
                Ok(Value::Str(type_name(&v).into()))
            }
            "range" => {
                let want_num = |i: usize| -> Result<f64, Control> {
                    match args.get(i) {
                        Some(Value::Number(n)) => Ok(*n),
                        Some(v) => Err(AsError::at(
                            format!("range() expects number arguments, got {}", type_name(v)),
                            span,
                        )
                        .into()),
                        None => Ok(0.0),
                    }
                };
                let (start, end, step) = match args.len() {
                    1 => (0.0, want_num(0)?, 1.0),
                    2 => (want_num(0)?, want_num(1)?, 1.0),
                    3 => (want_num(0)?, want_num(1)?, want_num(2)?),
                    n => {
                        return Err(AsError::at(
                            format!("range() expects 1 to 3 arguments, got {}", n),
                            span,
                        )
                        .into())
                    }
                };
                if step == 0.0 {
                    return Err(AsError::at("range() step must not be zero", span).into());
                }
                let mut out = Vec::new();
                let mut i = start;
                if step > 0.0 {
                    while i < end {
                        out.push(Value::Number(i));
                        i += step;
                    }
                } else {
                    while i > end {
                        out.push(Value::Number(i));
                        i += step;
                    }
                }
                Ok(Value::Array(Rc::new(RefCell::new(out))))
            }
            other => Err(AsError::at(format!("'{}' is not a function", other), span).into()),
        }
    }

    #[async_recursion(?Send)]
    async fn assign_to(&mut self, target: &Expr, value: Value, env: &Environment) -> Result<Value, Control> {
        match &target.kind {
            ExprKind::Ident(name) => match env.assign(name, value.clone()) {
                Ok(()) => Ok(value),
                Err(AssignError::Undefined) => Err(AsError::at(
                    format!("cannot assign to undefined variable '{}'", name),
                    target.span,
                )
                .into()),
                Err(AssignError::Immutable) => Err(AsError::at(
                    format!("cannot assign to immutable binding '{}'", name),
                    target.span,
                )
                .into()),
            },
            ExprKind::Index { object, index } => {
                let obj = self.eval_expr(object, env).await?;
                let idx = self.eval_expr(index, env).await?;
                match obj {
                    Value::Array(arr) => {
                        let i = array_index(&idx, target.span)?;
                        let mut arr = arr.borrow_mut();
                        if i >= arr.len() {
                            return Err(AsError::at(
                                format!("index {} out of bounds (len {})", i, arr.len()),
                                target.span,
                            )
                            .into());
                        }
                        arr[i] = value.clone();
                        Ok(value)
                    }
                    Value::Object(map) => match idx {
                        Value::Str(key) => {
                            map.borrow_mut().insert(key.to_string(), value.clone());
                            Ok(value)
                        }
                        _ => Err(AsError::at("object index must be a string", target.span).into()),
                    },
                    _ => Err(AsError::at("cannot index-assign a non-array value", object.span).into()),
                }
            }
            ExprKind::Member { object, name } => {
                let obj = self.eval_expr(object, env).await?;
                match obj {
                    Value::Object(map) => {
                        map.borrow_mut().insert(name.clone(), value.clone());
                        Ok(value)
                    }
                    Value::Instance(inst) => {
                        inst.borrow_mut().fields.insert(name.clone(), value.clone());
                        Ok(value)
                    }
                    _ => Err(AsError::at(format!("cannot set property '{}' on this value", name), object.span).into()),
                }
            }
            _ => Err(AsError::at("invalid assignment target", target.span).into()),
        }
    }
}

/// Validate that a value is a usable array index (a non-negative integer).
fn array_index(v: &Value, span: Span) -> Result<usize, AsError> {
    match v {
        Value::Number(n) if n.fract() == 0.0 && *n >= 0.0 => Ok(*n as usize),
        Value::Number(_) => Err(AsError::at("array index must be a non-negative integer", span)),
        _ => Err(AsError::at("array index must be a number", span)),
    }
}

/// Human-readable type name for diagnostics.
fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Nil => "nil",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::Str(_) => "string",
        Value::Builtin(_) | Value::Function(_) => "function",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Enum(_) => "enum",
        Value::EnumVariant(_) => "enum variant",
        Value::Class(_) => "class",
        Value::Instance(_) => "instance",
        Value::BoundMethod(_) | Value::Super(_) => "function",
    }
}

fn exported_names(stmt: &Stmt) -> Vec<String> {
    match stmt {
        Stmt::Let { name, .. } => vec![name.clone()],
        Stmt::Fn { name, .. } => vec![name.clone()],
        Stmt::Class { name, .. } => vec![name.clone()],
        Stmt::Enum { name, .. } => vec![name.clone()],
        Stmt::LetDestructure { names, .. } => names.clone(),
        _ => Vec::new(),
    }
}

/// Runtime contract check (spec §5). Eagerly checks parametric types to full depth.
fn check_type(value: &Value, ty: &crate::ast::Type) -> bool {
    use crate::ast::Type;
    match ty {
        Type::Any => true,
        Type::Number => matches!(value, Value::Number(_)),
        Type::String => matches!(value, Value::Str(_)),
        Type::Bool => matches!(value, Value::Bool(_)),
        Type::Nil => matches!(value, Value::Nil),
        Type::Object => matches!(value, Value::Object(_)),
        Type::Fn => matches!(value, Value::Function(_) | Value::Builtin(_)),
        Type::Error => matches!(value, Value::Object(_) | Value::Nil),
        Type::Array(elem) => match value {
            Value::Array(a) => a.borrow().iter().all(|v| check_type(v, elem)),
            _ => false,
        },
        Type::Result(inner) => match value {
            Value::Array(a) => {
                let b = a.borrow();
                b.len() == 2
                    && (check_type(&b[0], inner) || matches!(b[0], Value::Nil))
                    && check_type(&b[1], &Type::Error)
            }
            _ => false,
        },
        Type::Tuple(types) => match value {
            Value::Array(a) => {
                let b = a.borrow();
                b.len() == types.len() && b.iter().zip(types.iter()).all(|(v, t)| check_type(v, t))
            }
            _ => false,
        },
        Type::Union(a, b) => check_type(value, a) || check_type(value, b),
        Type::Named(name) => match value {
            Value::Instance(inst) => {
                let mut cur = Some(inst.borrow().class.clone());
                while let Some(c) = cur {
                    if &c.name == name {
                        return true;
                    }
                    cur = c.superclass.clone();
                }
                false
            }
            Value::EnumVariant(v) => &v.enum_name == name,
            _ => false,
        },
    }
}

/// Build a contract-violation panic.
fn contract_panic(ty: &crate::ast::Type, value: &Value, span: Span) -> Control {
    AsError::at(
        format!("type contract violated: expected {}, got {} ({})", ty, type_name(value), value),
        span,
    )
    .into()
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::parser::parse;

    /// Extract the panic's AsError from a Control (test helper).
    fn panic_of(c: Control) -> AsError {
        match c {
            Control::Panic(e) => e,
            Control::Propagate(_) => panic!("expected a panic, got a `?` propagation"),
        }
    }

    /// Lex+parse+exec a program string, returning its captured `print` output.
    /// Panics (test failure) on a lex/parse error or a runtime panic.
    async fn run(src: &str) -> String {
        let mut interp = Interp::new();
        let tokens = lex(src).expect("lex");
        let stmts = parse(&tokens).expect("parse");
        let env = global_env();
        interp.exec(&stmts, &env).await.expect("program panicked");
        interp.output
    }

    /// Like `run`, but expects a runtime panic and returns its `AsError`.
    async fn run_err(src: &str) -> AsError {
        let mut interp = Interp::new();
        let tokens = lex(src).expect("lex");
        let stmts = parse(&tokens).expect("parse");
        let env = global_env();
        match interp.exec(&stmts, &env).await {
            Err(Control::Panic(e)) => e,
            Ok(_) => panic!("expected a runtime panic, but the program succeeded"),
            Err(Control::Propagate(_)) => panic!("expected a panic, got a `?` propagation"),
        }
    }

    #[tokio::test]
    async fn core_len_type_range() {
        assert_eq!(run("print(len([1,2,3]))").await, "3\n");
        assert_eq!(run("print(len(\"hello\"))").await, "5\n");
        assert_eq!(run("print(len({a:1, b:2}))").await, "2\n");
        assert_eq!(run("print(type(1))").await, "number\n");
        assert_eq!(run("print(type(\"x\"))").await, "string\n");
        assert_eq!(run("print(type([1]))").await, "array\n");
        assert_eq!(run("print(type(nil))").await, "nil\n");
        assert_eq!(run("print(range(3))").await, "[0, 1, 2]\n");
        assert_eq!(run("print(range(2, 5))").await, "[2, 3, 4]\n");
        assert_eq!(run("print(range(0, 10, 3))").await, "[0, 3, 6, 9]\n");
        assert_eq!(run("print(range(5, 0, -2))").await, "[5, 3, 1]\n");
    }

    #[tokio::test]
    async fn len_of_wrong_type_panics() {
        let err = run_err("len(5)").await;
        assert!(err.message.contains("len() expects"));
    }

    #[tokio::test]
    async fn destructures_array_into_bindings() {
        let out = run("let [a, b] = [1, 2]\nprint(a)\nprint(b)\nlet [x, y] = [9]\nprint(x)\nprint(y)").await;
        assert_eq!(out, "1\n2\n9\nnil\n");
    }

    #[tokio::test]
    async fn destructuring_non_array_panics() {
        let err = run_err("let [a, b] = 5").await;
        assert!(err.message.contains("cannot destructure"));
    }

    #[tokio::test]
    async fn enum_variants_access_and_equality() {
        let src = "enum Color { Red, Green, Blue }\nenum Status { Ok = 200, NotFound = 404 }\nprint(Color.Red)\nprint(Color.Red == Color.Red)\nprint(Color.Red == Color.Green)\nprint(Status.NotFound.value)\nprint(Status.Ok.name)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "Color.Red\ntrue\nfalse\n404\nOk\n");
    }

    #[tokio::test]
    async fn match_on_literals_and_wildcard() {
        let src = "fn label(n) { return match n { 0 => \"zero\", 1 | 2 => \"small\", _ => \"many\" } }\nprint(label(0))\nprint(label(2))\nprint(label(9))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "zero\nsmall\nmany\n");
    }

    #[tokio::test]
    async fn match_on_enum_variants() {
        let src = "enum Color { Red, Green, Blue }\nfn warm(c) { return match c { Color.Red => true, _ => false } }\nprint(warm(Color.Red))\nprint(warm(Color.Blue))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "true\nfalse\n");
    }

    #[tokio::test]
    async fn match_no_arm_panics() {
        let src = "match 5 { 1 => \"a\" }";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("no matching arm"));
    }

    #[tokio::test]
    async fn match_with_variable_and_expression_patterns() {
        // A bare-variable pattern must work (value-equality, not arrow-function).
        let src = "let k = 2\nprint(match 2 { k => \"hit\", _ => \"miss\" })\nprint(match 3 { k => \"hit\", _ => \"miss\" })\nlet n = 5\nprint(match 6 { n + 1 => \"plus\", _ => \"no\" })";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "hit\nmiss\nplus\n");
    }

    #[tokio::test]
    async fn class_construction_fields_and_methods() {
        let src = "class Animal {\n  fn init(name) { self.name = name }\n  fn speak() { return self.name + \" makes a sound\" }\n}\nlet a = Animal(\"Rex\")\nprint(a.name)\nprint(a.speak())";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "Rex\nRex makes a sound\n");
    }

    #[tokio::test]
    async fn class_without_init_rejects_args() {
        let src = "class Empty {}\nEmpty(1)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("no init"));
    }

    #[tokio::test]
    async fn typed_code_runs_without_enforcement_yet() {
        let src = "let x: number = 5\nfn f(a: number): number { return a + 1 }\nprint(f(x))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "6\n");
    }

    #[tokio::test]
    async fn let_contract_passes_and_fails() {
        // passes
        let src = "let x: number = 5\nprint(x)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "5\n");

        // fails
        let bad = "let x: number = \"oops\"";
        let stmts = parse(&lex(bad).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("type contract violated"));
        assert!(err.message.contains("expected number"));
    }

    #[tokio::test]
    async fn parametric_and_union_contracts() {
        // array<number> with a bad element fails
        let bad = "let xs: array<number> = [1, \"two\", 3]";
        let stmts = parse(&lex(bad).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        assert!(interp.exec(&stmts, &env).await.is_err());

        // union passes for either member
        let ok = "let a: number | nil = nil\nlet b: number | nil = 7\nprint(b)";
        let stmts = parse(&lex(ok).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "7\n");

        // Result<number>: Ok(5) passes, Ok("x") fails
        let r = "let r: Result<number> = Ok(5)\nprint(r[0])";
        let stmts = parse(&lex(r).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "5\n");
    }

    #[tokio::test]
    async fn result_contract_accepts_both_ok_and_err() {
        // Both Ok and Err must satisfy a Result<T> contract (spec §6).
        let src = "
fn parseNum(s): Result<number> {
  if (s == \"bad\") { return Err(\"not a number\") }
  return Ok(42)
}
let good: Result<number> = parseNum(\"ok\")
let bad: Result<number> = parseNum(\"bad\")
print(good[0])
print(bad[1].message)
";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "42\nnot a number\n");
    }

    #[tokio::test]
    async fn param_contract_enforced() {
        let src = "fn double(n: number): number { return n * 2 }\nprint(double(\"x\"))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("type contract violated"));
        assert!(err.message.contains("expected number"));
    }

    #[tokio::test]
    async fn return_contract_enforced() {
        // returns a string but annotated number
        let src = "fn f(): number { return \"nope\" }\nf()";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("type contract violated"));
    }

    #[tokio::test]
    async fn typed_function_happy_path() {
        let src = "fn add(a: number, b: number): number { return a + b }\nprint(add(2, 3))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "5\n");
    }

    #[tokio::test]
    async fn contract_failure_is_recoverable() {
        // a contract panic is catchable by recover (it's a Panic, M5)
        let src = "fn f(n: number) { return n }\nlet r = recover(() => f(\"bad\"))\nprint(r[0])\nprint(r[1].message)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert!(interp.output.starts_with("nil\n"));
        assert!(interp.output.contains("type contract violated"));
    }

    #[tokio::test]
    async fn ok_and_err_construct_result_pairs() {
        let src = "let r = Ok(5)\nprint(r[0])\nprint(r[1])\nlet e = Err(\"boom\")\nprint(e[0])\nprint(e[1].message)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "5\nnil\nnil\nboom\n");
    }

    #[tokio::test]
    async fn assert_passes_and_panics() {
        // passing assert returns nil
        let ok = "assert(1 < 2)\nprint(\"ok\")";
        let stmts = parse(&lex(ok).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "ok\n");

        // failing assert panics with the message
        let bad = "assert(false, \"nope\")";
        let stmts = parse(&lex(bad).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("nope"));
    }

    #[tokio::test]
    async fn question_unwraps_ok_and_propagates_err() {
        // A function that uses `?`: returns the value on Ok, propagates [nil, err] on Err.
        let src = "
fn parse(x) {
  if (x < 0) { return Err(\"negative\") }
  return Ok(x * 2)
}
fn run(x) {
  let v = parse(x)?
  return Ok(v + 1)
}
let good = run(5)
print(good[0])
print(good[1])
let bad = run(-1)
print(bad[0])
print(bad[1].message)
";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        // run(5): parse->Ok(10), v=10, returns Ok(11) -> [11, nil]
        // run(-1): parse->Err, ? propagates [nil, {message:"negative"}]
        assert_eq!(interp.output, "11\nnil\nnil\nnegative\n");
    }

    #[tokio::test]
    async fn question_on_non_result_panics() {
        let src = "let x = 5\nlet y = x?";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("requires a Result pair"));
    }

    #[tokio::test]
    async fn recover_catches_a_panic() {
        // A function that panics (index out of bounds) is recovered into [nil, err].
        let src = "
fn boom() {
  let a = [1]
  return a[10]
}
let r = recover(boom)
print(r[0])
print(r[1].message)
";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        // r[0] is nil; r[1].message carries the panic text (index out of bounds).
        assert!(interp.output.starts_with("nil\n"));
        assert!(interp.output.contains("out of bounds"));
    }

    #[tokio::test]
    async fn recover_passes_through_success() {
        let src = "
fn good() { return 42 }
let r = recover(good)
print(r[0])
print(r[1])
";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "42\nnil\n");
    }

    async fn eval_to_value(src: &str) -> Value {
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let (last, rest) = stmts.split_last().expect("at least one statement");
        interp.exec(rest, &env).await.unwrap();
        match last {
            Stmt::Expr(e) => interp.eval_expr(e, &env).await.unwrap(),
            _ => panic!("last statement must be an expression"),
        }
    }

    #[tokio::test]
    async fn evaluates_arithmetic_with_precedence() {
        match eval_to_value("1 + 2 * 3").await {
            Value::Number(n) => assert_eq!(n, 7.0),
            other => panic!("expected number, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn print_writes_to_the_output_buffer() {
        let stmts = parse(&lex("print(1 + 2 * 3)").unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "7\n");
    }

    #[tokio::test]
    async fn comparison_and_equality() {
        assert_eq!(eval_to_value("1 < 2").await, Value::Bool(true));
        assert_eq!(eval_to_value("2 == 2").await, Value::Bool(true));
        assert_eq!(eval_to_value("1 != 2").await, Value::Bool(true));
        assert_eq!(eval_to_value("\"a\" == \"a\"").await, Value::Bool(true));
    }

    #[tokio::test]
    async fn string_concatenation() {
        // `Str + Str` concatenates.
        assert_eq!(
            eval_to_value("\"a\" + \"b\"").await,
            Value::Str("ab".into())
        );

        // `Str + Number` must error (no coercion).
        let stmts = parse(&lex("\"a\" + 1").unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        assert!(interp.exec(&stmts, &env).await.is_err());

        // `Number + Str` must error (no coercion in the other direction).
        let stmts = parse(&lex("1 + \"a\"").unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        assert!(interp.exec(&stmts, &env).await.is_err());
    }

    #[tokio::test]
    async fn exponent_evaluates() {
        assert_eq!(eval_to_value("2 ** 10").await, Value::Number(1024.0));
    }

    #[tokio::test]
    async fn short_circuit_and_coalesce() {
        assert_eq!(eval_to_value("false && nope").await, Value::Bool(false));
        assert_eq!(eval_to_value("true || nope").await, Value::Bool(true));
        assert_eq!(eval_to_value("nil ?? 5").await, Value::Number(5.0));
        assert_eq!(eval_to_value("3 ?? nope").await, Value::Number(3.0));
        assert_eq!(eval_to_value("!0").await, Value::Bool(false));
    }

    #[tokio::test]
    async fn calling_an_undefined_name_is_an_error() {
        // `nope` is not a binding, so resolving the callee fails before the call.
        let stmts = parse(&lex("nope(1)").unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("undefined variable"));
    }

    #[tokio::test]
    async fn call_site_errors_carry_a_span() {
        // Undefined callee name: the resolution error must carry a span.
        let stmts = parse(&lex("nope(1)").unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("undefined variable"));
        assert!(err.span.is_some());

        // Non-callable callee value: "not callable" error must carry the callee span.
        let stmts = parse(&lex("(1)(2)").unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("not callable"));
        assert!(err.span.is_some());
    }

    #[tokio::test]
    async fn let_binding_resolves() {
        let stmts = parse(&lex("let x = 5\nprint(x + 1)").unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "6\n");
    }

    #[tokio::test]
    async fn undefined_variable_errors_with_span() {
        let stmts = parse(&lex("print(missing)").unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("undefined variable 'missing'"));
        assert!(err.span.is_some());
    }

    #[tokio::test]
    async fn optional_semicolons_are_accepted() {
        let stmts = parse(&lex("let x = 1; print(x);").unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "1\n");
    }

    #[tokio::test]
    async fn assignment_updates_a_mutable_binding() {
        let src = "let x = 1\nx = x + 4\nprint(x)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "5\n");
    }

    #[tokio::test]
    async fn compound_assignment_runs() {
        let src = "let x = 10\nx *= 3\nprint(x)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "30\n");
    }

    #[tokio::test]
    async fn assigning_to_const_errors() {
        let src = "const x = 1\nx = 2";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("immutable"));
    }

    #[tokio::test]
    async fn if_else_chooses_branch() {
        let src = "let x = 3\nif (x < 5) { print(\"small\") } else { print(\"big\") }";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "small\n");
    }

    #[tokio::test]
    async fn else_if_chain() {
        let src = "let x = 7\nif (x < 5) { print(\"a\") } else if (x < 10) { print(\"b\") } else { print(\"c\") }";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "b\n");
    }

    #[tokio::test]
    async fn block_scope_does_not_leak() {
        let src = "{ let y = 1 }\nprint(y)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("undefined variable 'y'"));
    }

    #[tokio::test]
    async fn while_loop_accumulates() {
        let src = "let i = 1\nlet sum = 0\nwhile (i <= 5) { sum += i\ni += 1 }\nprint(sum)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "15\n");
    }

    #[tokio::test]
    async fn for_range_iterates_half_open() {
        let src = "let sum = 0\nfor (i in 0..5) { sum += i }\nprint(sum)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        // 0 + 1 + 2 + 3 + 4
        assert_eq!(interp.output, "10\n");
    }

    #[tokio::test]
    async fn for_range_loop_var_is_scoped_per_iteration() {
        let src = "for (i in 0..3) { print(i) }";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "0\n1\n2\n");
    }

    #[tokio::test]
    async fn break_exits_loop_early() {
        let src = "let sum = 0\nfor (i in 0..10) { if (i == 5) { break }\nsum += i }\nprint(sum)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "10\n"); // 0+1+2+3+4
    }

    #[tokio::test]
    async fn continue_skips_iteration() {
        let src = "let sum = 0\nfor (i in 0..5) { if (i == 2) { continue }\nsum += i }\nprint(sum)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "8\n"); // 0+1+3+4
    }

    #[tokio::test]
    async fn break_in_while() {
        let src = "let i = 0\nwhile (true) { if (i >= 3) { break }\ni += 1 }\nprint(i)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "3\n");
    }

    #[tokio::test]
    async fn print_is_a_resolvable_builtin_value() {
        assert_eq!(eval_to_value("print").await, Value::Builtin("print".into()));
    }

    #[tokio::test]
    async fn break_outside_loop_errors_at_top_level() {
        let err = crate::run_source("break").await.unwrap_err();
        assert!(err.message.contains("outside of a loop"));
    }

    #[tokio::test]
    async fn calls_a_user_function() {
        let src = "fn add(a, b) { return a + b }\nprint(add(2, 3))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "5\n");
    }

    #[tokio::test]
    async fn recursion_works() {
        let src = "fn fact(n) { if (n <= 1) { return 1 }\nreturn n * fact(n - 1) }\nprint(fact(5))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "120\n");
    }

    #[tokio::test]
    async fn closures_capture_their_environment() {
        // makeAdder returns a function that closes over `x`.
        let src = "fn makeAdder(x) { fn adder(y) { return x + y }\nreturn adder }\nlet add10 = makeAdder(10)\nprint(add10(5))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "15\n");
    }

    #[tokio::test]
    async fn arity_mismatch_errors() {
        let src = "fn f(a, b) { return a }\nf(1)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("expected 2 argument"));
    }

    #[tokio::test]
    async fn function_without_return_yields_nil() {
        let src = "fn noop() { let x = 1 }\nprint(noop())";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "nil\n");
    }

    #[tokio::test]
    async fn arrow_expression_body() {
        let src = "let double = x => x * 2\nprint(double(21))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "42\n");
    }

    #[tokio::test]
    async fn arrow_multi_param_and_closure() {
        let src = "let base = 100\nlet f = (a, b) => a + b + base\nprint(f(1, 2))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "103\n");
    }

    #[tokio::test]
    async fn arrow_block_body_with_return() {
        let src = "let f = (n) => { if (n > 0) { return \"pos\" }\nreturn \"nonpos\" }\nprint(f(5))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "pos\n");
    }

    #[tokio::test]
    async fn array_literal_and_indexing() {
        let src = "let a = [10, 20, 30]\nprint(a[0])\nprint(a[2])";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "10\n30\n");
    }

    #[tokio::test]
    async fn index_assignment() {
        let src = "let a = [1, 2, 3]\na[1] = 99\nprint(a[1])\nprint(a)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "99\n[1, 99, 3]\n");
    }

    #[tokio::test]
    async fn out_of_bounds_index_errors() {
        let src = "let a = [1]\nprint(a[5])";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("out of bounds"));
    }

    #[tokio::test]
    async fn object_literal_member_and_computed_access() {
        let src = "let o = { name: \"Ada\", age: 36 }\nprint(o.name)\nprint(o[\"age\"])\nprint(o.missing)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "Ada\n36\nnil\n");
    }

    #[tokio::test]
    async fn member_and_computed_assignment() {
        let src = "let o = { a: 1 }\no.b = 2\no[\"c\"] = 3\nprint(o.a + o.b + o.c)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "6\n");
    }

    #[tokio::test]
    async fn member_of_nil_errors() {
        let src = "let x = nil\nprint(x.foo)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("cannot read property 'foo' of nil"));
    }

    #[tokio::test]
    async fn optional_chaining_short_circuits_on_nil() {
        let src = "let o = { a: nil }\nprint(o?.a)\nprint(o.a?.deep)\nprint((o.a ?? 42))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        // o?.a -> nil; o.a is nil so o.a?.deep -> nil; nil ?? 42 -> 42
        assert_eq!(interp.output, "nil\nnil\n42\n");
    }

    #[tokio::test]
    async fn optional_chaining_reads_when_present() {
        let src = "let o = { a: { b: 7 } }\nprint(o?.a?.b)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "7\n");
    }

    #[tokio::test]
    async fn optional_chaining_short_circuits_rest_of_chain() {
        // a is nil: the WHOLE chain a?.b.c yields nil (not an error on .c).
        let src = "let a = nil\nprint(a?.b.c)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "nil\n");
    }

    #[tokio::test]
    async fn optional_chaining_full_chain_with_index_and_present() {
        // present chain reads through; nil mid-chain short-circuits the rest.
        let src = "let o = { a: { b: [10, 20] } }\nprint(o?.a.b[1])\nlet z = nil\nprint(z?.a.b[1])";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "20\nnil\n");
    }

    #[tokio::test]
    async fn parentheses_break_the_optional_chain() {
        // (a?.b) evaluates to nil, then .c on nil ERRORS (chain broken by parens).
        let src = "let a = nil\nprint((a?.b).c)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("cannot read property 'c' of nil"));
    }

    #[tokio::test]
    async fn for_of_iterates_array() {
        let src = "let total = 0\nfor (x of [10, 20, 30]) { total += x }\nprint(total)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "60\n");
    }

    #[tokio::test]
    async fn for_of_iterates_string_chars() {
        let src = "let out = \"\"\nfor (c of \"abc\") { out = out + c + \".\" }\nprint(out)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "a.b.c.\n");
    }

    #[tokio::test]
    async fn for_of_non_iterable_errors() {
        let src = "for (x of 42) { print(x) }";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("not iterable"));
    }

    #[tokio::test]
    async fn template_string_interpolates() {
        let src = "let name = \"Ada\"\nlet n = 3\nprint(`hi ${name}, ${n + 1} times`)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "hi Ada, 4 times\n");
    }

    #[tokio::test]
    async fn nested_template_and_plain() {
        let src = "print(`outer ${ `inner ${1 + 1}` } end`)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "outer inner 2 end\n");
    }

    #[tokio::test]
    async fn inheritance_and_super() {
        let src = "class Animal {\n  fn init(name) { self.name = name }\n  fn speak() { return self.name + \" makes a sound\" }\n}\nclass Dog extends Animal {\n  fn init(name) { super.init(name) }\n  fn speak() { return super.speak() + \" - woof\" }\n}\nlet d = Dog(\"Rex\")\nprint(d.name)\nprint(d.speak())";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "Rex\nRex makes a sound - woof\n");
    }

    #[tokio::test]
    async fn inherited_method_without_override() {
        let src = "class A { fn greet() { return \"hi\" } }\nclass B extends A {}\nlet b = B()\nprint(b.greet())";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "hi\n");
    }

    #[tokio::test]
    async fn undefined_superclass_errors() {
        let src = "class B extends Nope {}";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("undefined superclass"));
    }

    #[tokio::test]
    async fn named_type_contracts() {
        let src = "class Animal { fn init() { self.ok = true } }\nclass Dog extends Animal {}\nenum Color { Red, Green }\nfn pet(a: Animal): bool { return a.ok }\nlet d: Dog = Dog()\nprint(pet(d))\nlet c: Color = Color.Red\nprint(c.name)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        // Dog is-an Animal (subclass), so pet(d) passes; c: Color accepts a Color variant.
        assert_eq!(interp.output, "true\nRed\n");
    }

    #[tokio::test]
    async fn named_contract_rejects_wrong_type() {
        let src = "class Animal { fn init() {} }\nclass Plant { fn init() {} }\nfn pet(a: Animal) { return a }\npet(Plant())";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("type contract violated"));
        assert!(err.message.contains("expected Animal"));
    }

    #[tokio::test]
    async fn async_fn_and_await_surface() {
        let src = "async fn fetch(x) { return x * 2 }\nlet r = await fetch(21)\nprint(r)\nprint(await 5)\nlet g = async (n) => n + 1\nprint(await g(9))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "42\n5\n10\n");
    }
}
