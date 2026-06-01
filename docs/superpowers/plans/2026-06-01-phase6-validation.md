# Phase 6 — Validation & Schema Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** `std/schema` — composable validator; schemas as tagged AScript Objects, `schema.parse(s, v) -> [value, {path,message}]`. Full spec: `docs/superpowers/specs/2026-06-01-phase6-validation-design.md`.

**Architecture:** New `src/stdlib/schema.rs` (core, no feature gate). Schemas are tagged `Value::Object`s (`{__kind: "...", ...}`) built by constructors; `schema.parse` is a native recursive validator. Because `refine` predicates call user fns, the parse engine is async → `impl Interp` dispatch (`call_schema`) like `call_array`; constructors are pure and can be in `schema::call` OR also routed through `call_schema` for uniformity. Tier-1 `[value, err]` (err = `{path, message}` Object). NO new value variant / grammar.

**Conventions:** register BOTH `mod.rs` arms; Tier-2 panic for misuse (non-schema to parse); no `RefCell` borrow across `.await` (clone schema parts before calling refine fns); clippy clean BOTH configs; RUN both `cargo test` configs; docs+README+example.

Sub-phases: 6a core+primitives → 6b composites → 6c constraints/refine/coerce → 6d integration. Each builds on the prior parse engine.

---

## Sub-phase 6a: Core + primitives

**Files:** `src/stdlib/schema.rs` (new), `src/stdlib/mod.rs` (register `"std/schema"` + `"schema"` dispatch, core/no gate), tests.

- [ ] **Step 1 — failing tests** (lex→parse→exec helpers): `schema.parse(schema.string(), "hi")` → `["hi", nil]`; `schema.parse(schema.number(), "x")` → `[nil, err]` where err.message contains "expected number"; `schema.literal(5)` parses 5 → `[5,nil]`, rejects 6; `schema.any()` accepts a number and a string; `schema.parse({}, 1)` (non-schema, no __kind) → Tier-2 panic.
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:** constructors `string/number/bool/nil/any/literal` returning tagged Objects (`{__kind:"string"}`, `{__kind:"literal", value:v}`). `call_schema` async dispatch with `parse(schema, value)`: read `__kind` (Tier-2 panic if the schema arg has no `__kind`/isn't an Object), dispatch on it. Primitive: `type_name(value)` vs kind; mismatch → `err_obj(path, format!("expected {kind}, got {actual}"))` returned as `make_pair(nil, errObj)`. Success → `make_pair(value.clone(), nil)`. Helper `err_obj(path, msg) -> Value::Object{path, message}`. Register `pub mod schema` + both mod.rs arms (core). Build the err as an Object with string fields `path`,`message`.
- [ ] **Step 4 — verify:** both `cargo test` configs + both clippy.
- [ ] **Step 5 — commit:** `feat(schema): std/schema core + primitives (string/number/bool/nil/any/literal)`

---

## Sub-phase 6b: Composites

**Files:** `src/stdlib/schema.rs` (extend), tests.

- [ ] **Step 1 — failing tests:** array(number()) over [1,2] ok / over [1,"x"] → err path `"[1]"`; object({a:number(), b:string()}) over {a:1,b:"x"} ok / over {a:1,b:2} → err path `"<root>.b"`; nested object err path (`"user.address.zip"`); optional(number()) over nil → `[nil,nil]`, over "x" → err; union([string(),number()]) accepts both, rejects bool; enum(["a","b"]) membership; object ignores extra keys, `schema.strict(obj)` rejects extra; map(string(),number()).
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:** constructors `array(elem)`, `object(fields)`, `map(key,val)`, `optional(inner)`, `union(list)`, `enum(list)`, `strict(objSchema)`. Extend `parse` dispatch: recurse with extended `path` (`format!("{path}[{i}]")` for arrays, `format!("{path}.{field}")` or `field` at root for objects). object: iterate declared fields, parse each (fail-fast on first err); ignore extra keys unless `strict` flag set on the schema. map: Object→Map coercion at boundary (like validate_into). union: try each option, first success wins, else err "expected one of ...". optional: nil→ok else inner. Build validated containers (return coerced/validated value).
- [ ] **Step 4 — verify:** both configs + clippy.
- [ ] **Step 5 — commit:** `feat(schema): composites (array/object/map/optional/union/enum/strict)`

---

## Sub-phase 6c: Constraints, refinements, coercion

**Files:** `src/stdlib/schema.rs` (extend), tests.

- [ ] **Step 1 — failing tests:** min/max numeric bounds (parse fails out-of-range with a clear message); minLength/maxLength on string + array; pattern match/mismatch (regex); refine(s, fn, msg) — predicate truthy ok / falsy → err with msg (refine fn is a user closure → engine awaits/calls it); default fills an absent object field; `schema.parse(number(), "42", {coerce:true})` → `[42, nil]` (coerced), and without coerce → err.
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:** `min/max/minLength/maxLength/pattern` add constraint fields to the tagged schema; `refine(s, fn, msg)` stores the fn + msg; `default(s, value)` stores a default. Extend `parse`: after the base kind check, apply constraints (numeric bounds, length, regex via std/regex, refine via `self.call_value(fn, [value])` — clone out, no borrow across await); apply `default` when value is nil/absent under optional/object. Add a `coerce` option: `parse` accepts an optional 3rd arg options object `{coerce: bool}`; when true, attempt conservative coercions (string→number via parse, number→string, "true"/"false"→bool) before validating; document the table.
- [ ] **Step 4 — verify:** both configs + clippy.
- [ ] **Step 5 — commit:** `feat(schema): constraints/refine/default + coerce option`

---

## Sub-phase 6d: Integration

**Files:** `src/stdlib/json.rs` (extend parse to accept a schema), `src/stdlib/schema.rs` (`fromClass`), `resp.json` if applicable, tests + example.

- [ ] **Step 1 — failing tests:** `json.parse(validJson, schema)` → `[value, nil]`; `json.parse(badShapeJson, schema)` → `[nil, {path,message}]`; `json.parse(malformedJson, schema)` → `[nil, err]` (parse failure fused); `schema.fromClass(SomeClass)` then `schema.parse(obj)` validates like `.from`.
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:** in `json.parse`, detect the 2nd arg: if it's a `Value::Class` → existing typed-parse (validate_into); if it's a tagged-object schema (has `__kind`) → parse JSON then `schema.parse`, fusing failures into one Tier-1 pair. `schema.fromClass(Class)`: read `merged_field_schema(class)`, map each `Type` to a schema (number→number(), string→string(), bool→bool(), `T?`→optional, `array<T>`→array, nested class→fromClass/object, map<K,V>→map); cover common types, Tier-1-reject (or document) unusual ones. Mirror for `resp.json(schema)` if `resp.json(Class)` exists.
- [ ] **Step 4 — verify:** both configs + clippy.
- [ ] **Step 5 — commit:** `feat(schema,json): json.parse(text, schema) + schema.fromClass bridge`

---

## Sub-phase 6 integration

- [ ] `examples/validation.as`: a nested schema (user {name:string, age:number(min 0), email:refined, address: object, tags: array<string>, nickname: optional}); parse valid + invalid + malformed JSON via `json.parse(text, schema)`; print the structured error path for the invalid case. Bounded, terminates, prints success.
- [ ] Docs: `docs/content/stdlib/` schema page (constructors, parse, errors, constraints, coerce, fromClass, json.parse integration); README stdlib table.
- [ ] FULL gates: both `cargo test` configs, both clippy `--all-targets`, `fmt --check`, the example, both conformance tests.
- [ ] Holistic review (focus: parse engine correctness + error paths; no borrow across await when calling refine fns; Tier-1 discipline; the json.parse class-vs-schema detection doesn't break existing json.parse(text)/json.parse(text,Class); no regression; no TODOs). Merge `--no-ff`.

## Self-review notes
- Riskiest: 6d's json.parse arg-detection (must NOT break `json.parse(text)` 1-arg, `json.parse(text, strict)` bool, or `json.parse(text, Class)`) — the schema detection must be unambiguous (tagged Object with __kind vs Class vs bool). And the async refine call in 6c (borrow discipline).
- Error-path string format must be consistent (`parent.field`, `arr[i]`) across all composites.
- No new syntax → conformance unchanged. schema.parse engine recursion is acyclic (schemas built bottom-up).
