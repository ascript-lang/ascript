# AScript Milestone 7 — Classes, Enums & Match Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Implement spec §8: JS-style single-inheritance classes (`class`/`extends`/`super`/`self`/`init`, callable to construct), simple enums (named variants with optional backing values), the `match` expression (literal/enum/wildcard/or-patterns), and class-name/enum-name type contracts (`Type::Named`).

**Architecture:** Five new `Value` kinds — `Enum`, `EnumVariant`, `Class`, `Instance`, `BoundMethod`, plus a `Super` reference. `self`/`super` are ordinary identifiers bound in a method's call environment (no new keywords for them). Method resolution walks the superclass chain; a `BoundMethod` carries its defining class so `super` starts lookup one level up. `match` evaluates the subject once and tests each arm's patterns by structural `==` (with `_` wildcard and `a | b` or-patterns). `Type::Named` checks an instance's class chain (subclass-aware) or an enum-variant's enum name.

**Tech Stack:** Rust 2021, tokio (current_thread), async-recursion, indexmap. No new crates.

**Starting state (end of M6, on `main`):** Full language core + data + Result/error + type contracts + comments. `Value` = `Nil|Bool|Number|Str|Builtin|Function|Array|Object`. `call_value` dispatches `Builtin`/`Function`. `read_member` handles `Object`/`Nil`. `parse_type_atom` errors on unknown idents ("Milestone 7"). The `Pipe` token exists (M6). 107 lib + 7 integration tests.

**Conventions:** spans char offsets; single-threaded `Rc`/`RefCell`; `?Send` async recursion; failed contracts/bad ops are `Control::Panic`.

## Spec §8 semantics decided

- **Classes:** `class Name [extends Super] { fn init(...) {...} fn m(...) {...} }`. No `new` — `Name(args)` constructs. `init` is the constructor; absent `init` + args → panic. `self` = the instance (bound in method env). Fields are set via `self.x = …` (dynamic; no field declarations). Single inheritance. Method resolution walks the chain; `super.m(args)` calls the parent's `m` with the same `self`.
- **Enums:** `enum Color { Red, Green, Blue }` or `enum Status { Ok = 200, … }` (backing value a number/string). Access `Color.Red`. `.name` → variant name string; `.value` → backing value or `nil`. Variants are interned (per enum) → `Color.Red == Color.Red`; never equal across enums or to their raw value. Enums are simple: no payloads, no methods.
- **`match`:** `match subj { pat => expr, pat | pat => expr, _ => expr, }` — an expression. Arms tested top-down; a pattern matches by `==` against the subject (or `_` always matches). First match wins; its body expr is the result. No match → panic "no matching arm".
- **`Type::Named`:** a class name matches an instance whose class chain includes that name (subclass-aware); an enum name matches any variant of that enum.
- **Equality:** classes/instances/enums/bound-methods by identity (`Rc::ptr_eq`); enum variants by identity (interned).

## Scope & Justified Deferrals

| Deferred | Why | Milestone |
|---|---|---|
| `map<K,V>` type | Map value kind | **M8** |
| Tagged-union enums (payloads) | Spec §8.2 explicit non-goal | — (non-goal) |
| Static method / property syntax sugar | Not in spec §8 | — |

---

## Task 1: Enums

**Files:** `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/value.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/token.rs`** — add `Enum,` before `Eof`.
- [ ] **Step 2: `src/lexer.rs`** — keyword `"enum" => Tok::Enum,`.

- [ ] **Step 3: `src/value.rs`** — add the enum value kinds. Add:

```rust
use indexmap::IndexMap; // already imported

pub struct EnumDef {
    pub name: String,
    pub variants: IndexMap<String, Value>, // each is a Value::EnumVariant
}

pub struct EnumVariant {
    pub enum_name: String,
    pub name: String,
    pub value: Value, // backing value, or Nil
}
```

Add variants to `Value`: `Enum(Rc<EnumDef>)`, `EnumVariant(Rc<EnumVariant>)`.

Update `is_truthy` (both truthy — `matches!` already covers). Update `PartialEq`: `Enum` by `Rc::ptr_eq`; `EnumVariant` by `Rc::ptr_eq` (interned). Update `Debug` (shallow: `write!(f, "Enum({})", e.name)`, `EnumVariant({}.{})`). Update `write_display`: `Enum(e) => write!(f, "<enum {}>", e.name)`, `EnumVariant(v) => write!(f, "{}.{}", v.enum_name, v.name)`.

- [ ] **Step 4: `src/ast.rs`** — add the statement:

```rust
    Enum { name: String, variants: Vec<EnumVariantDecl> },
```
and
```rust
#[derive(Clone, Debug)]
pub struct EnumVariantDecl {
    pub name: String,
    pub value: Option<Expr>,
}
```

- [ ] **Step 5: `src/parser.rs`** — `statement` dispatch `Tok::Enum => self.enum_decl()`, and:

```rust
    fn enum_decl(&mut self) -> Result<Stmt, AsError> {
        self.eat(&Tok::Enum)?;
        let name = match self.advance() {
            Tok::Ident(n) => n,
            other => return Err(AsError::at(format!("expected enum name, found {:?}", other), self.tokens[self.pos - 1].span)),
        };
        self.eat(&Tok::LBrace)?;
        let mut variants = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            let vname = match self.advance() {
                Tok::Ident(n) => n,
                other => return Err(AsError::at(format!("expected variant name, found {:?}", other), self.tokens[self.pos - 1].span)),
            };
            let value = if *self.peek() == Tok::Eq {
                self.advance();
                Some(self.expr()?)
            } else {
                None
            };
            variants.push(crate::ast::EnumVariantDecl { name: vname, value });
            if *self.peek() == Tok::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.eat(&Tok::RBrace)?;
        Ok(Stmt::Enum { name, variants })
    }
```

- [ ] **Step 6: `src/interp.rs`** — declare enums and read variants.

`exec_stmt` arm:
```rust
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
```

Extend `read_member` for `Enum`/`EnumVariant`:
```rust
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
```

Update the free `type_name` to add `Enum` → "enum", `EnumVariant(v)` → return a `&'static str` — careful, `type_name` returns `&'static str`; for variants just return `"enum variant"`.

- [ ] **Step 7: Tests** (interp):
```rust
    #[tokio::test]
    async fn enum_variants_access_and_equality() {
        let src = "enum Color { Red, Green, Blue }\nenum Status { Ok = 200, NotFound = 404 }\nprint(Color.Red)\nprint(Color.Red == Color.Red)\nprint(Color.Red == Color.Green)\nprint(Status.NotFound.value)\nprint(Status.Ok.name)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "Color.Red\ntrue\nfalse\n404\nOk\n");
    }
```

- [ ] **Step 8: Run** `cargo test` + `cargo clippy --all-targets`. **Commit:** `feat: add enums with variants, backing values, and access`

---

## Task 2: `match` expression

**Files:** `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/token.rs`** — add `Match,` before `Eof`. **Step 2: `src/lexer.rs`** — keyword `"match" => Tok::Match,`.

- [ ] **Step 3: `src/ast.rs`** — add:
```rust
    Match { subject: Box<Expr>, arms: Vec<MatchArm> },
```
and
```rust
#[derive(Clone, Debug)]
pub struct MatchArm {
    /// Patterns are value-expressions compared with `==`; `None` patterns list
    /// means a wildcard `_`. Multiple patterns = an or-pattern.
    pub patterns: Option<Vec<Expr>>,
    pub body: Expr,
}
```
`Display`: `ExprKind::Match { .. } => write!(f, "(match)")`.

- [ ] **Step 4: `src/parser.rs`** — `match` is a primary expression. In `primary`'s `match self.advance()`, add `Tok::Match => return self.match_expr(start_span)` — but `primary` already consumed the token; instead handle it: add a `Tok::Match` arm that builds the match. Implement:

```rust
            Tok::Match => {
                let subject = self.expr()?;
                self.eat(&Tok::LBrace)?;
                let mut arms = Vec::new();
                while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
                    // pattern: `_` (wildcard) or expr ( `|` expr )*
                    let patterns = if *self.peek() == Tok::Ident("_".to_string()) {
                        // NOTE: `_` lexes as an Ident("_") — match on that.
                        self.advance();
                        None
                    } else {
                        let mut pats = vec![self.expr()?];
                        while *self.peek() == Tok::Pipe {
                            self.advance();
                            pats.push(self.expr()?);
                        }
                        Some(pats)
                    };
                    self.eat(&Tok::FatArrow)?;
                    let body = self.expr()?;
                    arms.push(crate::ast::MatchArm { patterns, body });
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::RBrace)?;
                let span = Span::new(tok_span.start, self.prev_end());
                return Ok(Expr { kind: ExprKind::Match { subject: Box::new(subject), arms }, span });
            }
```

NOTE on `_`: the identifier `_` lexes as `Tok::Ident("_")` (the lexer's identifier rule allows leading `_`). The pattern check above compares `*self.peek() == Tok::Ident("_".to_string())`. Simpler/robust: peek and match `Tok::Ident(s) if s == "_"`. Implement the wildcard check that way:
```rust
                    let is_wildcard = matches!(self.peek(), Tok::Ident(s) if s == "_");
                    let patterns = if is_wildcard {
                        self.advance();
                        None
                    } else { /* parse expr patterns */ };
```

- [ ] **Step 5: `src/interp.rs`** — evaluate `Match` in `eval_expr`:
```rust
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
```

- [ ] **Step 6: Tests** (interp):
```rust
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
```

- [ ] **Step 7: Run** `cargo test` + `cargo clippy --all-targets`. **Commit:** `feat: add match expression with literal, enum, wildcard, and or-patterns`

---

## Task 3: Classes (construction, fields, methods, `self`)

No inheritance yet (superclass always `None`). `self` is bound in method calls.

**Files:** `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/value.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/token.rs`** — add `Class,` before `Eof`. **Step 2: `src/lexer.rs`** — keyword `"class" => Tok::Class,`.

- [ ] **Step 3: `src/value.rs`** — add class value kinds:
```rust
pub struct Method {
    pub params: Vec<crate::ast::Param>,
    pub ret: Option<crate::ast::Type>,
    pub body: Vec<Stmt>,
}

pub struct Class {
    pub name: String,
    pub superclass: Option<Rc<Class>>,
    pub methods: IndexMap<String, Rc<Method>>,
    pub def_env: Environment,
}

pub struct Instance {
    pub class: Rc<Class>,
    pub fields: IndexMap<String, Value>,
}

pub struct BoundMethod {
    pub receiver: Value,
    pub method: Rc<Method>,
    pub defining_class: Rc<Class>,
    pub name: String,
}
```
Add `Value` variants: `Class(Rc<Class>)`, `Instance(Rc<RefCell<Instance>>)`, `BoundMethod(Rc<BoundMethod>)`. Update `is_truthy` (truthy), `PartialEq` (`Rc::ptr_eq` for each), `Debug` (shallow names), `write_display`: `Class(c) => write!(f, "<class {}>", c.name)`, `Instance(i) => write!(f, "<{} instance>", i.borrow().class.name)`, `BoundMethod(b) => write!(f, "<method {}>", b.name)`.

Add a free helper in `value.rs`:
```rust
/// Walk a class chain for a method, returning it plus the class that defined it.
pub fn find_method(class: &Rc<Class>, name: &str) -> Option<(Rc<Method>, Rc<Class>)> {
    let mut cur = Some(class.clone());
    while let Some(c) = cur {
        if let Some(m) = c.methods.get(name) {
            return Some((m.clone(), c.clone()));
        }
        cur = c.superclass.clone();
    }
    None
}
```

- [ ] **Step 4: `src/ast.rs`** — add:
```rust
    Class { name: String, superclass: Option<String>, methods: Vec<MethodDecl> },
```
and
```rust
#[derive(Clone, Debug)]
pub struct MethodDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub body: Vec<Stmt>,
}
```

- [ ] **Step 5: `src/parser.rs`** — `statement` dispatch `Tok::Class => self.class_decl()`, and (no `extends` yet — Task 4 adds it; for now reject `extends` is unnecessary, just parse the base form, but DO parse optional `extends Ident` and store it so Task 4 only adds interp support):

```rust
    fn class_decl(&mut self) -> Result<Stmt, AsError> {
        self.eat(&Tok::Class)?;
        let name = match self.advance() {
            Tok::Ident(n) => n,
            other => return Err(AsError::at(format!("expected class name, found {:?}", other), self.tokens[self.pos - 1].span)),
        };
        let superclass = if matches!(self.peek(), Tok::Ident(s) if s == "extends") {
            // `extends` is a soft keyword here (lexes as Ident)
            self.advance();
            match self.advance() {
                Tok::Ident(n) => Some(n),
                other => return Err(AsError::at(format!("expected superclass name, found {:?}", other), self.tokens[self.pos - 1].span)),
            }
        } else {
            None
        };
        self.eat(&Tok::LBrace)?;
        let mut methods = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            self.eat(&Tok::Fn)?;
            let mname = match self.advance() {
                Tok::Ident(n) => n,
                other => return Err(AsError::at(format!("expected method name, found {:?}", other), self.tokens[self.pos - 1].span)),
            };
            let params = self.param_list()?;
            let ret = if *self.peek() == Tok::Colon {
                self.advance();
                Some(self.parse_type()?)
            } else {
                None
            };
            let body = self.block()?;
            methods.push(crate::ast::MethodDecl { name: mname, params, ret, body });
        }
        self.eat(&Tok::RBrace)?;
        Ok(Stmt::Class { name, superclass, methods })
    }
```

(NOTE: `extends` is treated as a soft keyword — it lexes as `Tok::Ident("extends")`. That's fine; no token needed.)

- [ ] **Step 6: `src/interp.rs`** — declare classes, construct instances, bind/invoke methods, set fields.

`exec_stmt` arm (no superclass resolution yet — pass `None`; Task 4 wires `extends`):
```rust
            Stmt::Class { name, superclass: _, methods } => {
                let mut method_map = indexmap::IndexMap::new();
                for m in methods {
                    method_map.insert(m.name.clone(), std::rc::Rc::new(crate::value::Method {
                        params: m.params.clone(),
                        ret: m.ret.clone(),
                        body: m.body.clone(),
                    }));
                }
                let class = Value::Class(std::rc::Rc::new(crate::value::Class {
                    name: name.clone(),
                    superclass: None,
                    methods: method_map,
                    def_env: env.clone(),
                }));
                env.define(name, class, false).map_err(AsError::new)?;
                Ok(Flow::Normal)
            }
```

Extend `call_value` dispatch:
```rust
            Value::Class(class) => self.construct(class, args, span).await,
            Value::BoundMethod(bm) => self.invoke_method(&bm, args, span).await,
```

Add `construct` and `invoke_method`, and factor the shared body-runner. Add a shared helper `run_body` that both `call_function` and `invoke_method` use:

```rust
    /// Bind params (with contracts), run a body in `call_env`, apply the return
    /// contract. Shared by plain functions and methods.
    #[async_recursion(?Send)]
    async fn run_body(
        &mut self,
        params: &[crate::ast::Param],
        ret: &Option<crate::ast::Type>,
        body: &[Stmt],
        args: Vec<Value>,
        call_env: &Environment,
        span: Span,
        what: &str,
    ) -> Result<Value, Control> {
        if args.len() != params.len() {
            return Err(AsError::at(
                format!("{} expected {} argument(s), got {}", what, params.len(), args.len()),
                span,
            ).into());
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
```

Rewrite `call_function` to delegate:
```rust
    #[async_recursion(?Send)]
    async fn call_function(&mut self, func: &crate::value::Function, args: Vec<Value>, span: Span) -> Result<Value, Control> {
        let call_env = func.closure.child();
        let what = func.name.as_deref().unwrap_or("function").to_string();
        self.run_body(&func.params, &func.ret, &func.body, args, &call_env, span, &what).await
    }
```

Add `construct` + `invoke_method`:
```rust
    #[async_recursion(?Send)]
    async fn construct(&mut self, class: std::rc::Rc<crate::value::Class>, args: Vec<Value>, span: Span) -> Result<Value, Control> {
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
                    ).into());
                }
            }
        }
        Ok(inst_val)
    }

    #[async_recursion(?Send)]
    async fn invoke_method(&mut self, bm: &crate::value::BoundMethod, args: Vec<Value>, span: Span) -> Result<Value, Control> {
        let call_env = bm.defining_class.def_env.child();
        call_env.define("self", bm.receiver.clone(), false).map_err(AsError::new)?;
        // `super` lookup begins at the defining class's superclass (Task 4 sets it).
        let super_ref = Value::Super(std::rc::Rc::new(crate::value::SuperRef {
            receiver: bm.receiver.clone(),
            start: bm.defining_class.superclass.clone(),
        }));
        call_env.define("super", super_ref, false).map_err(AsError::new)?;
        self.run_body(&bm.method.params, &bm.method.ret, &bm.method.body, args, &call_env, span, &bm.name).await
    }
```

(NOTE: `Value::Super`/`SuperRef` are added in Task 4. For Task 3, either add the `Super` variant + `SuperRef` struct now as a stub (so `invoke_method` compiles) OR omit the `super` binding in Task 3 and add it in Task 4. CLEANER: add `Value::Super(Rc<SuperRef>)` + `SuperRef { receiver, start: Option<Rc<Class>> }` to value.rs NOW in Task 3 — including its `PartialEq`/`Debug`/`Display`/`is_truthy` arms and a `read_member` arm — so methods can use `super` even though no class has a superclass yet. This avoids reworking `invoke_method` in Task 4.)

Add the `Super` read in `read_member`:
```rust
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
```

Extend `read_member` for `Instance` (field, else bound method, else nil):
```rust
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
```

Extend `assign_to`'s `Member` arm so instance fields can be set (`self.x = v`):
```rust
                    Value::Instance(inst) => {
                        inst.borrow_mut().fields.insert(name.clone(), value.clone());
                        Ok(value)
                    }
```
(add this case alongside the existing `Object` case in the Member assign arm.)

`Value::Super(Rc<SuperRef>)` + `SuperRef` in value.rs:
```rust
pub struct SuperRef {
    pub receiver: Value,
    pub start: Option<Rc<Class>>,
}
```
Add `Value::Super(Rc<SuperRef>)`; PartialEq `Rc::ptr_eq`; Debug `write!(f, "Super")`; Display `write!(f, "<super>")`; truthy.

Update `type_name`: `Class` → "class", `Instance(i)` → "instance" (or the class name — but `&'static str` so use "instance"), `BoundMethod`/`Super` → "function".

- [ ] **Step 7: Tests** (interp):
```rust
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
```

- [ ] **Step 8: Run** `cargo test` + `cargo clippy --all-targets`. **Commit:** `feat: add classes with construction, fields, methods, and self`

---

## Task 4: Inheritance (`extends`, `super`)

**Files:** `src/interp.rs`.

- [ ] **Step 1: Resolve the superclass at class declaration.** In the `Stmt::Class` arm, resolve `superclass` (now used, not `_`): if `Some(super_name)`, look it up in `env`; it must be a `Value::Class`; store `Some(rc)` as the new class's `superclass`. Error if the name isn't a defined class.

```rust
            Stmt::Class { name, superclass, methods } => {
                let parent = match superclass {
                    Some(sup_name) => match env.get(sup_name) {
                        Some(Value::Class(c)) => Some(c),
                        Some(_) => return Err(AsError::new(format!("'{}' is not a class", sup_name))),
                        None => return Err(AsError::new(format!("undefined superclass '{}'", sup_name))),
                    },
                    None => None,
                };
                let mut method_map = indexmap::IndexMap::new();
                for m in methods {
                    method_map.insert(m.name.clone(), std::rc::Rc::new(crate::value::Method {
                        params: m.params.clone(), ret: m.ret.clone(), body: m.body.clone(),
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
```

(`find_method`, `invoke_method`'s `super` binding, and the `Super` `read_member` arm were all built in Task 3, so inheritance + `super` now work end-to-end once `superclass` is populated.)

- [ ] **Step 2: Tests** (interp):
```rust
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
```

- [ ] **Step 3: Run** `cargo test` + `cargo clippy --all-targets`. **Commit:** `feat: add single inheritance with extends and super`

---

## Task 5: `Type::Named` contracts for classes & enums

**Files:** `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/ast.rs`** — add `Named(String)` to `Type`; Display: `Type::Named(n) => write!(f, "{}", n)`.

- [ ] **Step 2: `src/parser.rs`** — in `parse_type_atom`, replace the unknown-ident error arm so an unknown identifier becomes `Type::Named(name)` (keep `map` → M8 error, keep the primitive matches before it):
```rust
                "map" => Err(AsError::at("map<K,V> type annotations arrive in Milestone 8", span)),
                _ => Ok(Type::Named(name)),
```

- [ ] **Step 3: `src/interp.rs`** — add a `Named` arm to `check_type`:
```rust
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
```
(`check_type` is sync; the `inst.borrow()` is released each loop iteration via `.clone()`. Walk the class chain by name — subclass-aware.)

- [ ] **Step 4: Tests** (interp):
```rust
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
```

- [ ] **Step 5: Run** `cargo test` + `cargo clippy --all-targets`. **Commit:** `feat: add class/enum named type contracts (subclass-aware)`

---

## Task 6: End-to-end demo + integration test

**Files:** `examples/oop.as` (new), `tests/cli.rs` (modify).

- [ ] **Step 1: Create `examples/oop.as`**

```
enum Shape { Circle, Square, Triangle }

class Animal {
  fn init(name) { self.name = name }
  fn describe() { return `${self.name} is an animal` }
}

class Dog extends Animal {
  fn init(name) { super.init(name) }
  fn describe() { return super.describe() + ", specifically a dog" }
  fn sound() { return "woof" }
}

fn shapeName(s: Shape): string {
  return match s {
    Shape.Circle => "circle",
    Shape.Square => "square",
    _ => "other",
  }
}

let d: Animal = Dog("Rex")
print(d.describe())
print(d.sound())
print(shapeName(Shape.Square))
print(shapeName(Shape.Triangle))
```

- [ ] **Step 2: Integration test in `tests/cli.rs`**
```rust
#[test]
fn runs_oop_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin).arg("run").arg("examples/oop.as").output().unwrap();
    assert!(output.status.success(), "process failed: {:?}", output);
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("Rex is an animal, specifically a dog"));
    assert!(out.contains("woof"));
    assert!(out.contains("square"));
    assert!(out.contains("other"));
}
```

- [ ] **Step 3: Run** `cargo test` (incl. `runs_oop_example`), then `cargo run --quiet -- run examples/oop.as` (paste output). `cargo clippy --all-targets`. **Commit:** `test: add classes/enums/match end-to-end example`

---

## Definition of Done

- `cargo test` passes (all unit + integration); `cargo clippy --all-targets` clean.
- `cargo run -- run examples/oop.as` shows inheritance + `super`, a `match` over enum variants, and a `Type::Named` contract (`d: Animal` accepting a `Dog`).
- AScript supports: classes (construct, fields, methods, `self`), single inheritance (`extends`, `super`), simple enums (variants, `.name`/`.value`, equality), `match` (literals/enums/wildcard/or-patterns), and class/enum named type contracts.

## Hand-off to Milestone 8 ("Modules")

Adds ESM `import`/`export`, namespace import, and a module graph with once-only evaluation + caching (spec §9). Classes/enums/functions/consts become exportable bindings. `run_source` grows into a module loader keyed by resolved path; `std/*` paths resolve to built-in modules (the stdlib milestones M8+ register these). The `map<K,V>` type and `Map` value kind also land in M8's collection work.
