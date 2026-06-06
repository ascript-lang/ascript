# VM Plan V9 — Classes, enums, super, methods, fields, `.from`, contracts

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development.

**Goal:** Compile and execute classes (declared fields with types/defaults, methods, `init`, `extends`/`super`, `self`), enums + variants, instance creation, method/bound-method dispatch, `instanceof`, `ClassName.from(obj, strict)` validation, and `json.parse(text, Class)`/`resp.json(Class)` typed parse — all reusing the existing `Class`/`Instance`/`FieldSchema`/contract/`validate_into` semantics, byte-identical to the tree-walker.

**Architecture:** Reuse `value.rs`'s `Class`/`Instance`/`Method`/`BoundMethod`/`EnumDef`/`EnumVariant`/`ClassMethod` unchanged. The compiler emits `CLASS`/`METHOD`/`GET_SUPER`/`INSTANCE_OF`; method bodies are `FnProto`s (compiled like functions, with `self` in slot 0). Field-type checks on assignment, `init`, `.from`, and typed-parse delegate to the EXISTING interp helpers (`validate_into`, field-schema checks) via the borrowed `Interp` — the VM does not re-implement contract logic. **Depends on V8** (methods can be async/generators).

---

## Ground truth
- `Class { name, superclass, fields: IndexMap<String,FieldSchema>, methods: IndexMap<String,Rc<Method>>, def_env }`; `Instance { class, fields: IndexMap<String,Value> }`; `Method { params, ret, body, is_async }`; `BoundMethod`; `EnumDef`/`EnumVariant`; `ClassMethod(Rc<Class>, &'static str)` (the `.from` machinery). All `src/value.rs`.
- Tree-walker class semantics (`src/interp.rs`): field defaults, required/optional/`name?:T` fields, declared-field-type checks on assignment incl. inside `init`, `self`/`super`, method resolution up the superclass chain, `Class.from(obj,strict)` → `validate_into` (recurses nested class / array<Class> / map<K,Class>, applies defaults, Object→Map coercion, recoverable field-path panic, does NOT run init), `json.parse(text, Class)`/`resp.json(Class)` fusing parse+shape into one Tier-1 `[value,err]`. Reuse `validate_into` verbatim.
- Schema fluent chaining + typed-parse + `.from` are CALL-PATH behaviors, not opcodes (spec): the VM's `CALL`/method path replicates the interp's hooks (`is_schema_value`+`is_schema_method`→`call_schema`; `Class.from`/`json.parse(_,Class)` are ordinary calls with a class argument). Since the VM delegates non-Closure calls to `interp.call_value`, these mostly work for free — verify.

---

## Tasks
- [ ] **T1 — class declaration → CLASS/METHOD.** `ClassDecl`: compile field schemas (names/types/defaults — reuse the interp's `FieldDecl`→`FieldSchema` lowering), compile each method body as a `FnProto` (params with `self` in slot 0, per resolver frame), build a `Value::Class` via `CLASS` (consts: name, field schemas, proto-idxs for methods, superclass ref). `extends` wires `superclass`. Bind the class to its slot/global. Tests via disasm + execution: a class with fields+methods constructs. Commit.
- [ ] **T2 — instance creation + field access + method dispatch.** `ClassName(args)` (a CALL where callee is `Value::Class`): create `Instance`, apply field defaults, run `init` (a method) with args, applying field-type contracts identically. `GET_PROP`/`SET_PROP` on an instance read/write fields (with declared-type checks on set — reuse interp helper) or resolve a method → `BoundMethod`. `CALL` on a `BoundMethod` runs the method with `self` bound. Delegate instance/class calls to `interp.call_value` where simplest (the VM bridge handles closures; methods are `Rc<Method>` with legacy `Vec<Stmt>` bodies UNLESS the VM compiles methods to protos — decide: compile methods to protos for VM execution; instance/bound-method CALL runs the proto on a Fiber with self). Tests: construct, call methods, field get/set, default fields, typed-field violation (identical panic). Commit.
- [ ] **T3 — self / super / instanceof.** `self` → slot 0 in a method frame. `GET_SUPER name` → resolve `name` starting from the superclass (bound to current self). `INSTANCE_OF` (`x instanceof C`? or `is`? confirm the syntax/operator) → walk the class chain. Tests: super method calls, overridden methods, `super.init(...)`, instanceof up a chain. Commit.
- [ ] **T4 — enums.** `EnumDecl` → `Value::Enum` (`CLASS`-like op or a dedicated `ENUM` op; reuse `EnumDef`/`EnumVariant`). Variant access (`Color.Red`) → `GET_PROP` on the enum → `EnumVariant`. Equality/match of variants. Tests: enum decl, variant access, equality, use in match (match is V10 — basic equality here). Commit.
- [ ] **T5 — `.from` / typed-parse / schema chaining via call path.** Verify `ClassName.from(obj, strict)`, `json.parse(text, Class)`, `resp.json(Class)`, and `schema.string().minLength(3).parse(x)` all work through the VM's `CALL`/member path (delegating to interp hooks). Add the `is_schema_value`+`is_schema_method`→`call_schema` routing in the VM's member-call path IF the delegation to `interp.call_value` doesn't already cover it (it should, since member calls on a schema Object route through the interp). Tests: `.from` valid + invalid (recoverable field-path panic), `json.parse(_,Class)` Tier-1 pair, a schema chain. Commit.
- [ ] **T6 — widen differential gate.** Add `oop.as`, `typed.as`, `typed_fields.as`, `typed_parse.as`, `shape_validation.as`, `validation.as` to the allow-list. Byte-identical stdout + exit codes. Full suite + clippy both configs. Commit.

## Done criteria (V9)
- [ ] Classes/enums/super/methods/fields/`.from`/typed-parse/schema-chaining run identically to the tree-walker (incl. contract panics + field-path messages).
- [ ] Differential gate widened; `cargo test` green; clippy clean both configs.

**Next:** V10 — match + destructuring + spread: `MATCH_*` for all patterns (Option-C), array/object destructuring `let`, spread in array/object/call. After V10 the VM covers the whole language → the differential gate turns ON for the ENTIRE `examples/` corpus + full test suite.
