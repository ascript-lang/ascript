# Phase 6 — Validation & Schema Design (standout feature)

- **Date:** 2026-06-01
- **Status:** Design — proceeding under the standing multi-phase goal.
- **Roadmap:** Phase 6 of `2026-05-31-batteries-completeness-roadmap.md`.
- **Owner:** Mahmoud Kayyali

## Goal

A first-class, composable **schema/validation** library (`std/schema`) — the zod-like standout
feature. Validate and coerce arbitrary values against declaratively-built schemas, with
**structured field-path errors**. Complements the existing class-shape system (`.from(obj)`,
`T?` fields, `json.parse(text, Class)`), which stays as-is; Phase 6 adds a standalone validator
for shapes you don't want to declare as a class, plus a bridge so `json.parse`/`resp.json` accept
a schema.

## Representation (no new Value variant, no grammar change)

A **schema is a tagged AScript `Object`** built by `schema.*` constructors, e.g.
`schema.string()` → `{ __kind: "string" }`; `schema.object({a: schema.number()})` →
`{ __kind: "object", fields: { a: {__kind:"number"} } }`. Constraints add fields
(`{__kind:"number", min: 0}`); `refine` stores a predicate `Value::Function`. Schemas are thus
ordinary inspectable values; `schema.parse` is a native function that recursively walks the
tagged object. **Purely additive stdlib** — touches no `value.rs`/grammar/tree-sitter; only a new
`src/stdlib/schema.rs` (core, no feature gate) + the Phase-6d bridge edits to `json.rs`.

Errors: `schema.parse` returns a Tier-1 `[value, err]` pair. On failure, `err` is an object
`{ path: string, message: string }` (path like `"user.address.zip"`, `""` at root), matching the
field-path style of the existing class validator.

## Sub-phases
- **6a — Core + primitives:** representation, constructors `string/number/bool/nil/any/literal`,
  and `schema.parse` with structured errors.
- **6b — Composites:** `array/object/map/optional/union/enum` with nested field-path errors.
- **6c — Constraints + refinements + coercion:** `min/max/minLength/maxLength/pattern/refine/
  default`, and an opt-in `coerce`.
- **6d — Integration:** `json.parse(text, schema)` & `resp.json(schema)` accept a schema;
  `schema.fromClass(Class)` bridge; docs + example.

Conventions: native module (`exports()` + `call`; `parse`/`refine`-with-callback need the
interpreter → an `impl Interp` async dispatch since `refine`/coerce may call user fns and
`schema.parse` may invoke refine predicates); register BOTH `mod.rs` arms; Tier-1 for validation
failures, Tier-2 panic for misuse (e.g. `schema.parse` given a non-schema); clippy clean both
configs; RUN both test configs; docs+README+example.

---

## 6a — Core + primitives

Constructors (return tagged Objects):
- `schema.string() / number() / bool() / nil() / any()` — `{__kind: "<t>"}`. `any` matches
  anything.
- `schema.literal(v)` — `{__kind:"literal", value: v}`; matches only `== v`.

`schema.parse(schema, value) -> [value, err]` (the engine — extended by later sub-phases):
- Dispatch on `__kind`. Primitive: check `type(value)` matches; on mismatch →
  `[nil, {path, message: "expected <t>, got <actual>"}]`. `literal`: `value == literal.value`
  else err. `any`: pass through.
- Returns the validated value (unchanged for primitives) on success.
- Misuse: a `schema` argument that isn't a tagged-object schema (no `__kind`) → Tier-2 panic.

### Tests (6a)
`schema.parse(schema.string(), "hi")` → `["hi", nil]`; `schema.parse(schema.number(), "x")` →
`[nil, {path:"", message:"expected number, got string"}]`; `literal(5)` matches 5, rejects 6;
`any()` accepts anything; non-schema first arg → panic.

---

## 6b — Composites

- `schema.array(elem)` — `{__kind:"array", elem}`; value must be an array; each element parsed
  against `elem`; error path `"[i]"` (e.g. `"items[2]"`).
- `schema.object(fields)` — `{__kind:"object", fields}` where `fields` is an Object of
  name→schema. Value must be an Object; each declared field parsed against its schema (path
  `"<parent>.<field>"`). **Decision:** unknown extra keys are **ignored** (lenient/stripped),
  with a `schema.strict(objSchema)` variant that errors on unknown keys. Document.
- `schema.map(keySchema, valSchema)` — `{__kind:"map", key, val}`; value must be a Map (or
  Object — coerce Object→Map at the boundary like the class validator does); each entry's
  key/value parsed.
- `schema.optional(inner)` — `{__kind:"optional", inner}`; `nil`/absent → ok (returns nil);
  otherwise parse against `inner`.
- `schema.union(schemas)` — `{__kind:"union", options: [..]}`; ok if value parses against ANY
  option (first success wins); else an err listing the union (`"expected one of ..."`).
- `schema.enum(values)` — `{__kind:"enum", values: [..]}`; value must `==` one of the literals.

Nested errors carry the full path (`"order.items[0].price"`). `object` validation collects the
FIRST error (fail-fast) — document; a collect-all-errors mode is future work.

### Tests (6b)
nested object with a bad inner field → err path points at the inner field; array element type
error → `"[i]"` path; optional absent → ok with nil; optional present-but-wrong → err; union
matches one option, rejects when none; enum membership; object ignores extra keys, `strict`
rejects them; map of string→number.

---

## 6c — Constraints, refinements, coercion

Chainable refiners (each takes a schema, returns a new tagged schema with the constraint added):
- `schema.min(s, n)` / `schema.max(s, n)` — numeric value bounds (on a number schema).
- `schema.minLength(s, n)` / `schema.maxLength(s, n)` — length bounds (string/array).
- `schema.pattern(s, regexString)` — string must match the regex (reuse `std/regex`).
- `schema.refine(s, fn, message)` — custom predicate; `fn(value)` truthy → ok, else err with
  `message`. (Needs the interpreter to call `fn` → the `schema.parse` engine is async / on
  `impl Interp`.)
- `schema.default(s, value)` — for use under `optional`/object fields: absent/nil → `value`.
- Coercion: `schema.parse(s, value, {coerce: true})` — when set, attempt safe coercions before
  validating: string→number (if parses), number→string, "true"/"false"→bool, etc. Default
  `coerce:false` (strict). Document the coercion table. (Alternatively a `schema.coerce(s)`
  wrapper that marks a schema coercing — pick one; prefer the parse-option for simplicity.)

Constraint failures produce `{path, message}` with a clear message (e.g.
`"order.qty: expected <= 100, got 250"`).

### Tests (6c)
min/max numeric bounds; minLength/maxLength on string + array; pattern match/mismatch; refine
predicate pass/fail with custom message; default fills an absent field; coerce string→number
under `{coerce:true}` and that strict mode rejects it.

---

## 6d — Integration

- `json.parse(text, schema)` — extend the existing typed-parse (which already accepts a Class)
  to ALSO accept a schema value: parse the JSON, then run `schema.parse` on it, fusing a
  parse-failure and a validation-failure into ONE Tier-1 `[value, err]`. (Detect schema vs Class
  by the argument's kind — a tagged-object schema vs a `Value::Class`.)
- `resp.json(schema)` — same, on the HTTP response body (if `resp.json(Class)` exists; mirror it).
- `schema.fromClass(Class) -> schema` — derive a schema from a class's declared fields (reuse
  `merged_field_schema` + the `Type`→schema mapping: `number`→number(), `T?`→optional,
  `array<T>`→array, nested class→object/fromClass). Lets users reuse class declarations as
  runtime schemas.
- Example `examples/validation.as`: define a schema for a nested record (user with address +
  tags array + optional fields + a refined email), parse valid + invalid JSON via
  `json.parse(text, schema)`, show the structured error path.

### Tests (6d)
`json.parse(validJson, schema)` → `[value, nil]`; `json.parse(badShapeJson, schema)` →
`[nil, {path, message}]`; `json.parse(malformedJson, schema)` → `[nil, err]` (parse failure fused);
`schema.fromClass(SomeClass)` then parse an object → validates like `.from`.

---

## Cross-cutting
- NO new language syntax — all stdlib; tree-sitter/parser/formatter unaffected; conformance passes.
- `schema.parse` is async (refine predicates call user fns) → `impl Interp` dispatch like
  `call_array`. No `RefCell` borrow across `.await` when invoking refine fns (clone the schema
  parts out first).
- Recursion safety: a schema is an acyclic tagged object (constructed bottom-up) — no cycle risk;
  but guard against pathological depth if cheap.
- Full gates (both test configs, clippy both, fmt, conformance, idempotence); holistic review;
  merge `--no-ff`.

## Decisions (made; flagged)
1. Schemas are tagged AScript Objects (no new Value variant, no grammar). **Settled.**
2. `schema.parse` returns Tier-1 `[value, {path, message}]`. **Settled.**
3. `object` ignores extra keys by default; `schema.strict` to reject them. **Settled.**
4. Coercion is opt-in via a `{coerce:true}` parse option (default strict). **Settled.**
5. `object` fails fast on first error (collect-all is future work). **Settled.**
6. New `std/schema` module (core, no feature gate). **Settled.**
7. 6d bridges `json.parse(text, schema)` + `schema.fromClass(Class)`. **Settled.**

## Open implementation choices (decide during impl, document)
- coerce table breadth (which coercions); keep conservative + documented.
- whether refine predicates may be async (await them) — yes if cheap, else sync-only + document.
- `schema.fromClass` Type→schema coverage for unusual field types (union types, map<K,Class>) —
  cover the common ones, document any gaps as Tier-1-rejecting rather than silent.
