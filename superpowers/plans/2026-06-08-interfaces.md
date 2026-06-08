# IFACE — Structural Interfaces — Implementation Plan

> REQUIRED SUB-SKILL: superpowers:subagent-driven-development (fresh implementer + independent reviewer
> per task; reviewer runs the commands and probes edges). Steps use `- [ ]`.

**Spec:** `superpowers/specs/2026-06-08-interfaces-design.md`. **Branch:** `feat/interfaces` off `main`
(**after NUM is merged** — rebase example/doc signatures onto NUM's `int`/`float`; the runtime predicate
is type-erased so no hard code dependency). **Depends on:** NUM merged; TYPE for the *static* half (lands
later). **Breaking:** no — `interface` is a new reserved keyword + a new top-level decl; no existing
program changes meaning; the nominal `instanceof` over classes is preserved bit-for-bit.

**The split (load-bearing):** the **RUNTIME half ships first** (Tasks 1–13): `Value::Interface`,
structural `conforms`, the `instanceof Interface` dispatch through the shared `apply_binop` arm, the
env-aware contract path, `.aso` + worker integration, both parsers, tree-sitter, fmt, LSP tokens, REPL,
and the runtime-half example corpus. The **STATIC half lands with TYPE** (Tasks 14–15, flagged TYPE-era):
`CheckTy::Interface`, `assignable`, narrowing, and `implements-violation`. **TYPE owns `conforms`'s static
analog; IFACE emits the `implements-violation` lint** (cross-spec reconciliation, `goal.md`).

**Architecture:** an interface name resolves to a `Value::Interface(Rc<InterfaceDef>)` — an immutable,
acyclic, `Trace`-trivial descriptor (no-op `trace`, like `Regex`/`Native`) naming a method set. It is
never a receiver, has no vtable, no GC edges — one `Rc` arm, the weight class of `Value::Class`.
Conformance is **structural** (class instance whose method table has every required method by
**name + arity**); v1 type-checking of signatures is TYPE's job (the named permissive-runtime /
strict-static gradual seam). The transitive method set is **flattened LAZILY** (interfaces forward-
reference as late-bound module-globals, C4) with a **runtime cycle guard**; the verdict is **memoized
per `(class, interface)` pointer pair**, per-isolate. Both engines byte-identical: the `instanceof`
branch is **one edit** to the shared free `apply_binop` `InstanceOf` arm (the VM reaches it via
`eval_binop_adaptive` delegation — no per-op VM handler).

**Tech stack:** Rust; the two front-ends; tree-sitter (`--abi 14` + publish); `src/value.rs`,
`src/interp.rs`, `src/vm/{aso,verify}.rs`, `src/worker/{dispatch,serialize}.rs`, `src/check/infer`,
`src/fmt.rs`, `src/lsp/`, `src/repl.rs`.

---

## Shared API Contract (pinned to current code — verified line numbers)
**Existing (verified):**
- `is_instance_of` free fn, nominal `Rc::as_ptr` walk — `src/value.rs:557` (target ptr `:561`, compare
  `:564`); stays the **unchanged** env-free single source of truth for the *nominal* walk.
- `Value::Class` identity-equal display `<class …>` — `src/value.rs:886`; `type_name` match `:483`;
  `is_truthy` match `:687`.
- `apply_binop` free fn `src/interp.rs:5076`; **`InstanceOf` arm** + the `"instanceof requires a class on
  the right-hand side"` error — `src/interp.rs:5100–5103`. **One edit site for both engines.**
- VM reaches `apply_binop` via `eval_binop_adaptive` (`src/vm/run.rs:664`, the adaptive fn at `:3743`);
  `Op::InstanceOf` is in the delegation set at `:647`. `binop_of`'s `Op::InstanceOf => BinOp::InstanceOf`
  at `src/vm/run.rs:4292` is the **opcode→BinOp table, NOT a handler** (spec corrects the prior miscite).
- Env-free `check_type(value, &Type)` — `src/interp.rs:5704`; its name-string-only `Type::Named` arm
  `:5744`; `contract_panic` `:5775`.
- **Env-aware `Type::Named` precedent** — `src/interp.rs:4397` (`Type::Named(name) => match (&val,
  env.get(name))`), the `coerce_field`/`validate_into` ladder that already resolves a `Named` to a
  `Value::Class` via `env.get`. The new env-aware contract path generalizes this same ladder.
- `Stmt::Class` — `src/ast.rs:317` (fields: `name, superclass, fields, methods, is_worker, span,
  name_span`); `Param` — `src/ast.rs:165` (fields `rest: bool` `:172`, `default: Option<Expr>` `:173`).
- Lexer keyword map — `src/lexer.rs:526` (`"enum"`), `:528` (`"class"`).
- Legacy `enum_decl` `src/parser.rs:281`; `class_decl`/`class_decl_inner` `:334`/`:338`; `extends`
  lexes as `Tok::Ident` (contextual). CST `class_decl` `src/syntax/parser.rs:1318`; `enum_decl` `:1449`.
- `CheckTy` enum `src/check/infer/ty.rs:41`; `CheckTy::Class` use `:111`; `assignable_depth` `:247`;
  `Object → Class` is `Unknown` `:722`. `ClassInfo` `src/check/infer/table.rs:18`; `class_id` `:126`;
  `class(id)` `:141`. `hover_type_at` `src/check/infer/mod.rs:37`.
- `ASO_FORMAT_VERSION = 18` — `src/vm/aso.rs:105` (read + `+1` at merge; **do not hardcode 19** —
  sequential by merge order, cross-cutting #5).
- Worker dispatch: `enum TopDef` `src/worker/dispatch.rs:71`; the `top_level_defs` doc table `:97`;
  `classify_binding` `:254` (the `Some((Op::Class, _)) => TopDef::Class` arm `:285`); `collect_def_refs`
  `:355` (its `match def` `:356`, `TopDef::Class` arm `:359`); `emit_dep_closure` `:789` (its
  `match defs.get` `:801`); `emit_class_recursive`-side class emit `:847`.
- Worker serialize: `check_sendable` `src/worker/serialize.rs:103`; `unsendable_kind` `:109`;
  `encode_value` `:380` + its catch-all `other => unreachable!` `:500`; `decode_value` `:525`; the
  clamp pattern `Vec::with_capacity(len.min(r.remaining()))` `:564`.

**New names (do not rename):** `Value::Interface(Rc<InterfaceDef>)`; structs `InterfaceDef { name,
own_methods: IndexMap<String, MethodReq>, extends: Vec<String>, flat: RefCell<Option<Rc<IndexMap<…>>>> }`
and `MethodReq { arity: usize, has_rest: bool }` (TYPE later adds param/ret `CheckTy`); `Tok::Interface`;
`Stmt::Interface { name, type_params: Vec<String>, extends: Vec<String>, methods, span, name_span }`;
`Stmt::Class.implements: Vec<String>`; `Interp::conforms(&self, v, iface) -> Result<bool, Control>`;
`Interp::check_type_env(&self, value, ty, env) -> bool`; VM `Op::DefineInterface` (or const-pool
descriptor + `DEFINE_GLOBAL` — implementer picks one lowering, classifier must match it); `TopDef::Interface`;
CheckTy::Interface + `InterfaceInfo` + `interface_id`/`interface(id)` (TYPE-era); codes `implements-violation`
(Error), `interface-cycle` (TYPE-era). `implements`/interface-composition `extends` stay **contextual**
(`Tok::Ident`).

## Conventions (every task)
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- `cargo test` AND `cargo test --no-default-features` green; clippy clean BOTH `--all-targets` configs.
- Four-mode byte-identity (`tree-walker == specialized-VM == generic-VM == .aso`,
  `tests/vm_differential.rs`, both feature configs) — fix the engine, never the assertion. The verdict
  cache is a pure memo (active in `--no-specialize`): warm and cold must give identical output.
- Tree-sitter: after `grammar.js`, `tree-sitter generate --abi 14` then `cargo build`.
- No `await` across a `RefCell`/resource borrow; the `InterfaceDef` is `Rc` with a no-op `Trace`.

---

## Task 1 — Value layer: `Value::Interface` + `InterfaceDef`/`MethodReq`
**Files:** `src/value.rs` (+ every exhaustive `match Value`, compiler-flushed). **Tests:** `value.rs`.
- [ ] Failing tests: `Value::Interface(rc).type_name()=="interface"`; `is_truthy()==true`; `Display` →
  `<interface Reader>` (mirroring `<class …>` `:886`); two distinct `Rc`s `!=`, same `Rc` `==` (identity
  via `Rc::ptr_eq`, like `Value::Class` at `:722`).
- [ ] Add `Value::Interface(Rc<InterfaceDef>)` + the `InterfaceDef { name, own_methods, extends, flat:
  RefCell<Option<Rc<IndexMap<String, MethodReq>>>> }` and `MethodReq { arity, has_rest }` structs (TYPE
  later adds param/ret `CheckTy` to `MethodReq` — leave the slot). Add arms in `PartialEq` (`Rc::ptr_eq`),
  `Debug`, `Display` (`:886` template), `type_name` (`:483` → `"interface"`), `is_truthy` (`:687` →
  `true`), and GC `Value::trace` (**no-op** — acyclic, no `Value` edges, like `Regex`/`Native`). Do **NOT**
  add an `implements` list to the runtime `Class` struct (conformance is structural).
- [ ] Green both configs; clippy. Independent review (greps for a missing exhaustive arm; confirms
  `trace` is a no-op and the descriptor holds no `Value`/`Cc`). Commit.

## Task 2 — `conforms` predicate + lazy flatten + cycle guard + verdict cache
**Files:** `src/interp.rs` (the `&self` `conforms`/`flatten`), `src/value.rs` (`MethodReq` helpers).
**Tests:** `interp.rs`. **(Runtime SoT — both engines call this.)**
- [ ] Failing tests for the §5.1 predicate: a class with a matching method conforms; a missing method →
  `false`; an **inherited** (superclass) method satisfies; a non-`Instance` LHS (number, bare object,
  enum, nil) → `false`. Arity table (§5.1, #6): `fn read(b, opts=nil)` (min 1, max 2) **satisfies**
  `read(b)` (req.arity 1) AND `read(b,o)` (req.arity 2); `fn read(b)` (min 1, max 1) **does NOT** satisfy
  `read(b, opts)` (req.arity 2 → `2 > 1`); `fn read(...xs)` (min 0, max ∞) satisfies any `read(...)`; a
  `req.has_rest` requirement needs a variadic method. Composition: `ReadWriter extends Reader, Writer`
  requires both; a transitive `extends`-of-`extends` flattens; a **forward-referenced** `extends`
  (`A extends B`, `B` declared after) resolves (proves lazy, not eager); a **cyclic** `extends` →
  recoverable Tier-2 panic `cyclic interface extends: A -> B -> A`; an `extends` name resolving to a
  non-interface or nothing → its own recoverable Tier-2 panic. Cache: warm == cold; access-order
  property `conforms(c,i)` constant.
- [ ] Implement `fn conforms(&self, v: &Value, iface: &Rc<InterfaceDef>) -> Result<bool, Control>` on
  `Interp` (§5.2): match `v` → `Instance(i)` else `Ok(false)`; `flatten(iface)` (lazy, §4) → for each
  required `(name, req)`, `find_method(&class, name)` then `arity_compatible(method, req)` using
  `min_required`/`declared_max` from `Param.default`/`Param.rest` (`ast.rs:172/173`). `arity_compatible`:
  `min_required <= req.arity <= declared_max` (`declared_max = ∞` if method has a rest param);
  `req.has_rest` requires the method variadic.
- [ ] Implement `flatten(iface)`: if `iface.flat` memo `Some`, return it; else resolve each `extends`
  name via the module-global lookup (the same `env.get(name)` ladder the contract path uses), recursively
  union (own-wins on name collision), carry a **visited-`Rc::as_ptr` set** for the cycle guard, memoize
  into `iface.flat`, return. Memo never invalidated within a run (descriptors are load-time-immortal).
- [ ] Add the **verdict cache** `RefCell<HashMap<(usize, usize), bool>>` on `Interp` keyed
  `(Rc::as_ptr(class) as usize, Rc::as_ptr(iface) as usize)` (§5.3) — stores `usize`+`bool` ONLY (no
  `Value`/`Rc`/`Cc`; holds nothing alive, GC never traces it). `conforms` consults it above `flatten`.
  Document per-isolate immortality + soundness (descriptors live the whole run; workers/REPL get fresh
  pointers + cold caches — §5.3). Add the matching cache field on `Vm` in Task 6.
- [ ] Green both configs; clippy. Review (probes the arity table verdicts, cycle/bad-extends panics,
  forward-ref flatten, no `await` across the cache borrow). Commit.

## Task 3 — AST: `Stmt::Interface`, `Class.implements`, `MethodReq` node + Display
**Files:** `src/ast.rs` (+ `fmt.rs`/`interp.rs` exhaustive arms, compiler-flushed). **Tests:** `ast.rs`.
- [ ] Add `Stmt::Interface { name, type_params: Vec<String> (empty in v1 — reserves generics, §6.1),
  extends: Vec<String>, methods: Vec<MethodReqNode>, span, name_span }` (a `MethodReqNode` = name +
  params + optional ret type + spans, **no body**). Add `implements: Vec<String>` to `Stmt::Class`
  (`:317`). **No new `Type` arm** — an interface annotation is a `Type::Named`, resolved at runtime by
  env lookup / statically by the checker.
- [ ] `ast.rs` `Display` for `Stmt::Interface` (`interface Name extends A, B { fn m(...) -> T }`,
  requirements one-per-line) and the `implements A, B` clause on `Stmt::Class` (after `extends`, before
  body). The compiler flushes the missing `Stmt::Interface` arm in `interp.rs`/`fmt.rs` — leave those
  as `todo!()`/stubs wired in Tasks 5/9 (or implement here if trivial).
- [ ] Green; review; commit.

## Task 4 — Lexer + legacy parser: `interface` decl, `implements`, composition `extends`
**Files:** `src/lexer.rs`, `src/token.rs`, `src/parser.rs`. **Tests:** `parser.rs`.
- [ ] Failing tests: `interface R { fn read(b) -> int }` parses (0/1/N requirements); `;`-separated
  requirements (the class-body `skip_semicolons` rule); `interface RW extends A, B {}` composition;
  `class C extends Super implements A, B { … }`; `implements`/`extends` lex as `Tok::Ident` (contextual);
  rejecting `async`/`fn*`/`static`/`worker` on a requirement (**parse error**).
- [ ] Add `interface` → `Tok::Interface` in the lexer keyword map (beside `:528 "class"`). Add an
  `interface_decl` in `src/parser.rs` (mirror `enum_decl` `:281` / `class_decl` `:334`): parse the name,
  optional `extends` composition list (`Tok::Ident`-matched), and the brace-delimited method-requirement
  list (each `fn name(params) -> ret`, no block; reject modifiers). Extend `class_decl_inner` (`:338`)
  to parse an optional `implements I1, I2` clause after the optional `extends` superclass. Dispatch
  `Tok::Interface` in both statement entry points (`:103`/`:211`).
- [ ] Green; review; commit.

## Task 5 — Tree-walker: `exec` for `Stmt::Interface` + `instanceof` dispatch + contract path
**Files:** `src/interp.rs`. **Tests:** `interp.rs`.
- [ ] Failing tests: `interface R {…}` binds a `Value::Interface` module-global (printable
  `<interface R>`); `f instanceof R` structural (`true`/`false`); `5 instanceof R` → `false`;
  `x instanceof <number>` Tier-2-panics with the **new** message `"instanceof requires a class or
  interface on the right-hand side"`; a `R`-annotated `let`/param/field rejects a non-conforming value
  with the same Tier-2 contract panic a class annotation gives (via `check_type_env` → `conforms`),
  accepts a conforming one; a `Named` that resolves to a **class** still nominal-checks; an unresolved
  name stays permissive (gradual); nested `array<R>` resolves element-wise.
- [ ] `exec` `Stmt::Interface` arm: build the `InterfaceDef` (own_methods + `extends` **names** only — no
  flatten, §4), bind as a module-`Environment` global.
- [ ] **Extend the shared `apply_binop` `InstanceOf` arm** (`:5100`): branch on RHS — `Value::Class(c)` →
  `is_instance_of` (UNCHANGED nominal walk); `Value::Interface(i)` → route to the engine `conforms`
  (Task 2); else → Tier-2 panic with the new "class or interface" message at `:5103`. (Because
  `conforms` is `&self` and `apply_binop` is free, thread the call so the tree-walker's
  `InstanceOf`-eval site passes through `self.conforms` — keep `apply_binop` the single textual SoT for
  the message; the interface branch may live in the `&self` `eval_binop` caller that wraps `apply_binop`,
  whichever keeps the message byte-identical across both engines.)
- [ ] **Add the env-aware contract path (G1, load-bearing).** New `fn check_type_env(&self, value, ty,
  env) -> bool` on `Interp`: its `Type::Named` arm does `env.get(name)` → `Some(Value::Interface(i))` →
  `conforms`; `Some(Value::Class(c))` → existing `is_instance_of`-by-name behavior; else preserve today's
  permissive name-string match. Composite arms (`Array`/`Optional`/`Union`/`Map`/…) recurse through
  `check_type_env`; non-`Named` leaves delegate to the **retained-unchanged** free `check_type` (`:5704`).
  Model on the env-aware `Type::Named` precedent at `src/interp.rs:4397`. **Thread `env`** to the ~8
  contract call sites (`:2484,4055,4079,4245,4335,4857,5592,5624`): the annotation-bearing ones
  (param/return/`let`/class-field `FieldSchema.ty`) call `check_type_env`; purely primitive/structural
  contracts keep the free `check_type`.
- [ ] Green both configs; clippy. Review (confirms the class `instanceof` path is bit-for-bit unchanged;
  the new message is identical across engines; no `await` across a borrow in the contract path). Commit.

## Task 6 — CST parser + VM define/dispatch + verdict cache on `Vm`
**Files:** `src/syntax/parser.rs`, `src/syntax/kind.rs`, `src/vm/*` (compile + run), `src/compile/`.
**Tests:** `tests/frontend_conformance.rs`, `tests/vm_differential.rs`.
- [ ] Failing tests: CST parses every Task-4 form; **frontend conformance** proves both parsers agree;
  the VM runs `interface`/`instanceof Interface`/the contract panic byte-identically to the tree-walker.
- [ ] CST `interface_decl(p)` mirroring `class_decl` (`src/syntax/parser.rs:1318`) + an `implements`
  clause in `class_decl`; new `SyntaxKind`s `InterfaceDecl`, `InterfaceKw`, `MethodReq`/`MethodReqList`,
  `ImplementsClause`, `ExtendsList` (`src/syntax/kind.rs`); reject `async`/`fn*`/`static`/`worker` on a
  requirement. Lower the CST interface decl to the typed AST `Stmt::Interface`.
- [ ] VM lowering: a top-level interface binding compiles to a run the worker classifier (Task 12) can
  recognize — either a new `Op::DefineInterface` that builds + `DEFINE_GLOBAL`s the descriptor, OR emit
  the descriptor as a const-pool constant + `DEFINE_GLOBAL` (mirror class definition). Pick ONE; record
  it so Task 9 (.aso) and Task 12 (classifier) match. The `instanceof` path needs **no** VM change — it
  flows through `eval_binop_adaptive` → `apply_binop` (Task 5).
- [ ] Add the per-`Vm` verdict cache `RefCell<HashMap<(usize,usize), bool>>` (mirror the `Interp` one,
  Task 2); the VM `conforms` call reads it. Active in BOTH specialized and generic modes (pure memo, not
  a fast path the kill switch skips).
- [ ] Three-way differential (tree-walker == specialized == generic) green both configs; review; commit.

## Task 7 — Tree-sitter grammar + highlights + publish + editors
**Files:** `tree-sitter-ascript/grammar.js`, `queries/highlights.scm`, `editors/**`.
**Tests:** `tests/treesitter_conformance.rs`.
- [ ] Add `interface_declaration` (name, optional `extends` composition list, body of
  method-requirement signatures) parallel to `class_declaration` (`grammar.js:234`); an optional
  `implements` clause on `class_declaration`; a `method_requirement` rule (signature, no block). Add
  `interface`/`implements` to the keyword set; declare any GLR conflicts. Regen `parser.c`
  (`tree-sitter generate --abi 14`) then `cargo build`. Update `queries/highlights.scm` (`interface`/
  `implements` keywords; interface names as types).
- [ ] **Publish** (mandatory per `CLAUDE.md`/CONTRIBUTING): `./scripts/sync-grammar.sh` (subtree-split +
  push to the `ascript-lang/tree-sitter-ascript` mirror; prints the SHA), then bump that SHA in
  `editors/zed/extension.toml` (`commit`) and `editors/nvim/lua/ascript/treesitter.lua` (`revision`).
  Update `editors/vscode/syntaxes/ascript.tmLanguage.json` (TextMate keyword/storage — independent of
  LSP), `editors/zed/languages/ascript/highlights.scm`, `editors/nvim/queries/ascript/highlights.scm`,
  and `editors/nvim/tests/treesitter_spec.lua` if it asserts on keyword tokens.
- [ ] Conformance green; review; commit.

## Task 8 — Worker code-shipping: `TopDef::Interface` (X1 deliverable)
**Files:** `src/worker/dispatch.rs`. **Tests:** `tests/*` worker round-trip.
- [ ] Failing test: a `worker fn` doing `x instanceof Reader` ships `Reader`'s descriptor (today it
  emits `GET_GLOBAL Reader` and the name enters the fixpoint set, but the classifier has no interface arm
  → it resolves to `None`/`ComputedConst` and the descriptor never ships); a `worker fn` using
  `ReadWriter` pulls in `Reader`+`Writer` transitively; a nested (non-top-level) interface is a clean
  "unknown global" (documented non-goal, like nested classes).
- [ ] **Three edits** (§8 X1): (a) add `TopDef::Interface` to the `TopDef` enum (`dispatch.rs:71`),
  parallel to `TopDef::Class` `:86`; update the `top_level_defs` doc table `:97`. (b) `classify_binding`
  (`:254`): a run ending in the interface-define op (whatever Task 6 chose) → `TopDef::Interface`
  (mirror the `Some((Op::Class, _)) => TopDef::Class` arm `:285`). (c) `collect_def_refs` (`:355`,
  `match def` `:356`): `TopDef::Interface => out.extend(extends_names)` — the lazy-flatten dependency edge
  (method *signatures* carry no executable bodies, so no `GET_GLOBAL`s to walk, unlike a class table).
- [ ] **Emit site:** add an `emit_dep_closure` branch (its `match defs.get` `:801`, today
  `Const`/`Fn`/`ComputedConst`/`Class|None`) — or an `emit_interface_recursive` paralleling
  `emit_class_recursive` (`:847`) — that re-emits each included interface's define op + `DEFINE_GLOBAL`
  into the fragment, so the worker isolate rebuilds the descriptor (fresh `Rc`, fresh cold cache —
  per-isolate immortality, §5.3).
- [ ] Green both configs; clippy. Review (confirms transitive `extends` shipping; nested-interface
  non-goal is a clean error). Commit.

## Task 9 — `.aso` Interface constant + worker serializer arms (C5) + version bump
**Files:** `src/vm/aso.rs`, `src/vm/verify.rs`, `src/worker/serialize.rs`. **Tests:** `aso.rs`,
`serialize.rs`.
- [ ] Failing tests: an `Interface` constant round-trips (name + **unflattened** own method-requirement
  set + `extends` names — flatten is lazy at load, `extends` targets reload as module-globals, §4); a
  class's serialized layout carries the `implements` name list (checker metadata; runtime ignores it);
  `check_sendable(Value::Interface(...))` **errors** (so the `encode_value` `unreachable!` `:500` can
  never be reached); the `.aso` reader **clamps** an attacker-controlled method-count/name-length.
- [ ] `.aso`: add the `Interface` constant kind (reader uses `Vec::with_capacity(n.min(r.remaining()))`,
  the `serialize.rs:564` clamp pattern — cross-cutting #1); add the class `implements` list to the class
  layout. **Read `ASO_FORMAT_VERSION` (`:105`, currently 18) and bump by ONE — do not hardcode 19**
  (sequential by merge order; IFACE's value is `<prior-merge> + 1` at merge time). Update `verify.rs`
  (verify the interface constant's method set: no duplicate names, non-empty names).
- [ ] **Worker serializer C5 arms** (`src/worker/serialize.rs`): add `Value::Interface(_) =>
  Some(("interface", None))` to `unsendable_kind` (`:109`) so `check_sendable` (`:103`) rejects it with
  a field path (`value of kind interface cannot be sent to a worker at <path>`). Do **NOT** add an
  `encode_value` arm — interfaces never reach it (rejected first); add a unit test asserting the
  rejection so the `unreachable!` (`:500`) stays unreachable. `decode_value` needs **no** `Interface`
  case (no tag is ever written — recorded as intentional).
- [ ] Round-trip + four-mode differential green both configs; review; commit.

## Task 10 — Formatter
**Files:** `src/fmt.rs` (+ `ast.rs` `Display` parity from Task 3). **Tests:** fmt idempotence goldens.
- [ ] Failing/golden tests: `interface Name extends A, B { fn m(...) -> T }` canonical (requirements
  one-per-line, `extends` comma-joined); a class renders `class C extends Super implements A, B { … }`
  (canonical order: after `extends`, before body); idempotent (`fmt(fmt(x)) == fmt(x)`); `ast.rs`
  `Display` renders identically to the formatter.
- [ ] `write_stmt` arm for `Stmt::Interface`; the `implements` clause in the class writer.
- [ ] Green; review; commit.

## Task 11 — REPL + LSP semantic tokens + runtime-half example corpus
**Files:** `src/repl.rs`, `src/lsp/` (semantic tokens only this milestone), `examples/`.
**Tests:** `tests/lsp.rs`, conformance + four-mode differential.
- [ ] **REPL** regression: `interface R { fn read(b)->int }` then `class F { fn read(b) { return 0 } }`
  then `F() instanceof R` → `true` (brace-delimited body uses existing delimiter-depth `is_incomplete`
  buffering; descriptor persists on the session `Vm`/`Interp` like any top-level decl).
- [ ] **LSP semantic tokens** (this milestone): `interface`/`implements` as keywords, interface names as
  types. (Hover/go-to-def/find-refs/rename/completion are TYPE-era, Task 15.)
- [ ] **Runtime-half examples** (Gate 9 split, §9.3): `examples/interfaces.as` — a `Reader`/`Writer`
  pair + `fn copy(src: Reader, dst: Writer) -> int` over multiple conforming types (some `implements`,
  some purely structural) with `instanceof Reader` guards + a `ReadWriter extends Reader, Writer`
  composition; `examples/advanced/interface_dispatch.as` — a production-shaped, fully-error-handled
  `Codec` (`encode`/`decode`) selected at runtime via `instanceof`, returning `[value, err]`. Signatures
  use NUM's `int`/`float`. These exercise ONLY runtime-half behavior and emit **no** diagnostics.
- [ ] **Gate 12 micro-benchmark** (§9.4): an `instanceof Class` tight-loop bench asserting **no
  steady-state regression** vs the pre-IFACE baseline in BOTH specialized and generic modes (the branch
  is a single `match` on the already-loaded RHS discriminant ahead of the unchanged `is_instance_of`);
  a paired `instanceof Interface` warm-vs-cold-cache bench confirming the cache earns its keep.
- [ ] All four modes byte-identical (both configs); bench shows no regression; review; commit.

## Task 12 — Runtime-half docs + Gate verification + milestone close
**Files:** `docs/content/language/classes-enums.md`, `docs/content/language/type-contracts.md`,
`README.md`, `CLAUDE.md`, the design spec, `goal.md`/`roadmap.md`. **Tests:** doc serve sanity.
- [ ] New **"Interfaces"** section in `docs/content/language/classes-enums.md` (structural conformance,
  optional `implements`, `instanceof Interface`, composition via `extends`, the runtime-vs-static split,
  the named permissive-runtime/strict-static seam, deferred default-methods/fields/generics notes) —
  page already exists, **no `NAV` change** (the orphan gotcha applies only to NEW pages). Update
  `type-contracts.md` (an interface annotation as a runtime contract). Update `README.md` (capabilities
  table: "structural interfaces" → shipped, runtime half). Update `CLAUDE.md` (§Language-features
  interface note + the `Value` paragraph for the new arm). Update `roadmap.md` status. Serve sanity
  check (`cd docs && python3 -m http.server`).
- [ ] **Verify all gates** for the runtime half: four-mode byte-identity both configs (Gate 1); clippy
  both configs (Gate 2); tests both configs (Gate 3); no `await` across borrow / GC-opaque (Gate 4);
  Gate-12 bench no regression; tooling parity green (both parsers, tree-sitter regen+publish+pins,
  formatter, LSP tokens, REPL — Gate 9). Review; **merge `feat/interfaces` runtime half `--no-ff`**
  (or hold for the TYPE-era tasks per campaign sequencing — record the decision).

---
## TYPE-era tasks (land with/after TYPE — `CheckTy::Interface`, `assignable`, the lint)

## Task 13 — Checker: `CheckTy::Interface` + `Table` + eager flatten (TYPE-era)
**Files:** `src/check/infer/{ty,table}.rs`. **Tests:** `tests/check.rs`.
- [ ] Add `CheckTy::Interface(InterfaceId)` beside `CheckTy::Class(ClassId)` (`ty.rs:41`/`:111`). Add a
  parallel `InterfaceInfo` vector to `Table` (`table.rs:18`/`:37`) + `interface_id(name)`/`interface(id)`
  (mirroring `class_id` `:126` / `class(id)` `:141`), carrying the **flattened** required method set
  (own + `extends`-transitive) lowered to `CheckTy` signatures. **Static flatten is EAGER** (the checker
  sees every decl up front) with a visited-set cycle guard → the blocking **`interface-cycle`** code
  (the static analog of the runtime guard; both compute the SAME transitive set, only *when* differs).
  Resolve an unknown `Type::Named` to `Interface(id)` when `interface_id(name)` hits (extend the
  `class_id`→`enum_id` lookup ladder, `ty.rs:111`); still `Any` if unknown (gradual default).
- [ ] Tests: flatten union; cyclic `extends` → `interface-cycle` (terminating); `Named` resolves to an
  interface. Green both configs; review; commit.

## Task 14 — Checker: `assignable`, narrowing, `implements-violation` (TYPE-era)
**Files:** `src/check/infer/{ty,pass}.rs`, `src/check/rules/`, `src/check/fix.rs`. **Tests:**
`tests/check.rs`.
- [ ] `assignable_depth` arms (`ty.rs:247`, §6): `Class(c) → Interface(i)` = `Yes` if provably conforms,
  `No` if a method is provably missing/signature-incompatible, **`Unknown`** otherwise (an
  untyped-method-present yields `Unknown` → **silent**, the gradual gate — only `No` emits);
  `Interface(i) → Interface(j)` superset rule; `Interface → Object` = `Yes`; `Interface → Class` /
  `Object → Interface` = `Unknown` (mirroring `Object → Class` Unknown, `ty.rs:722`). Reuse the existing
  **`type-mismatch`** code for a provably-non-conforming annotated value (no new diagnostic kind).
- [ ] **`implements-violation`** (default **Error**, the ONLY Error-level code IFACE adds): when
  `class C implements I` and `C` provably does not conform, fire **at the `implements` clause** with the
  missing/mismatched method. Register in `rules::ALL`; **not** auto-fixable (`src/check/fix.rs` — stub
  generation out of scope; document). **TYPE owns `conforms`'s static analog; IFACE registers/emits this
  lint** (cross-spec reconciliation).
- [ ] `instanceof Reader` narrowing in `pass.rs` (the existing `instanceof`/nil-guard machinery): inside
  `if (v instanceof Reader) { … }`, `v` is `⊓ Interface(Reader)` so `v.read(b)` checks against
  `Reader.read`; a `match`-on-`instanceof` guard narrows identically.
- [ ] **Gate 5:** `examples/**` emits ZERO `type-*`/`implements-*` in BOTH configs (the runtime-half
  corpus + any TYPE-era positive example are written to conform; the `implements-violation` /
  `type-mismatch`-on-param / narrowing **negative** cases live in `tests/check.rs`, not `examples/**`).
- [ ] Green both configs; review; commit.

## Task 15 — TYPE-era LSP + docs polish
**Files:** `src/lsp/`, `src/check/infer/mod.rs`, docs. **Tests:** `tests/lsp.rs`.
- [ ] **Hover** (`hover_type_at`, `mod.rs:37`): an interface name → `<interface Reader>` + its method
  set; a `Reader`-typed binding → `Reader`; a method call on an interface-typed receiver → the
  requirement signature. **Go-to-def:** an interface method call site → the interface requirement decl;
  a `Reader` annotation / `implements Reader` clause → the `interface Reader` decl (`WorkspaceIndex`
  indexes the decl + requirements). **Find-references / rename** over an interface name + requirement
  names. **Completion:** `interface` as a top-level keyword; `implements` after a class header; interface
  names in type position; required method names inside `class … implements I` (completion only, no
  codegen).
- [ ] Docs: extend the classes-enums "Interfaces" section with the static-half story (assignability,
  narrowing, `implements-violation`). Update the campaign `goal.md`/`roadmap.md` IFACE status to fully
  shipped.
- [ ] Green; review; commit.

---

## Done when
Runtime half (Tasks 1–12): four-mode byte-identity holds in both configs (incl. `instanceof` results,
the contract panics, and `implements`-asserted == structurally-conforming classes); the
`instanceof Class` Gate-12 bench shows no regression in both VM modes; the runtime-half corpus runs
green on four engines and emits no diagnostics; tooling parity verified (both parsers, tree-sitter
regen+publish+editor pins, formatter, LSP tokens, REPL); the worker code-shipping ships interface
descriptors (X1) and the serializer rejects an interface *value* (C5); `.aso` round-trips with the
version bumped by one. TYPE half (Tasks 13–15): `CheckTy::Interface` + `assignable` + narrowing land,
`implements-violation` (Error) fires at the clause, and **Gate 5 holds — zero `type-*`/`implements-*` on
`examples/**` in both configs**. Clippy + tests green both configs throughout. Merge `--no-ff` to `main`.
