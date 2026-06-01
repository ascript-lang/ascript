# Fluent Schema Validation Design (Phase 6 follow-up)

- **Date:** 2026-06-01
- **Status:** Design — owner approved fluent + additive.
- **Owner:** Mahmoud Kayyali

## Goal

Add **fluent method chaining** to `std/schema` so refiners and `parse` can be called as methods on
a schema value, in addition to the existing free functions (ADDITIVE — no breaking change):

```ascript
let username = schema.string().minLength(3).maxLength(12).pattern("^[a-z0-9_]+$")
let [v, err] = username.parse(input)
```

The existing free-function form stays fully valid:
```ascript
let b = schema.pattern(schema.minLength(schema.string(), 3), "^x")   // still works
schema.parse(b, input)                                               // still works
```

## Approach (call-site hook — chosen)

Schemas remain **tagged Objects** (`{__kind:"string", minLength:3, ...}`). The fluent methods are
the SAME operations as the free functions; we just route a method call on a schema value to
`call_schema`. The interception lives in the interpreter's `Call` evaluator (`interp.rs`,
`ExprKind::Call`):

- For `ExprKind::Call { callee: Member{ object, name }, args }`: evaluate `object` to `recv`. If
  `recv` is a **schema value** (a `Value::Object` whose `__kind` field is a known schema kind) AND
  `name` is a **known schema method** → evaluate the args and dispatch
  `self.call_schema(name, [recv, ...args], span)`. Return its result.
- Otherwise → fall back to the EXISTING behavior (read the member off `recv`, then `call_value`) —
  byte-for-byte the current instance/native/generator/module/object-field-fn dispatch. No
  regression.

**Why a call-site hook, not a method value:** a refined schema stores constraints as object
fields, so `s.minLength` (bare member access) already resolves to the stored constraint value.
The hook distinguishes **call context** (`s.minLength(5)` → re-refine) from **access context**
(`s.minLength` → read the stored field), avoiding that collision. Reusing `call_schema` means the
methods are exactly the free functions — zero new schema logic, zero representation change, zero
change to `parse_value`, the `json.parse`/`resp.json` bridge, or `fromClass`.

## Method set (all existing `call_schema` ops usable as methods)
- Refiners/composites taking a schema as first arg: `minLength`, `maxLength`, `pattern`, `min`,
  `max`, `refine`, `default`, and the composite wrappers that take a schema first (`array` —
  N/A as method; `optional`, `strict`, `map`?, etc. — only those whose FIRST arg is the receiver
  schema). The hook routes `s.<name>(args)` → `call_schema(name, [s, ...args])`, so any op whose
  first parameter is the schema works as a method automatically.
- Terminal: `parse` — `s.parse(v)` → `call_schema("parse", [s, v])`.
- NOT methods: the source constructors (`string`/`number`/`bool`/`nilType`/`any`/`literal`/
  `object`/`array`/`union`/`oneOf`/`map`/`fromClass`) — they don't take a receiver schema; they
  stay `schema.*(...)` module functions (the chain entry points).

The set of "schema methods" = the `call_schema` function names whose first parameter is a schema
(everything except the sources). Implement `is_schema_method(name)` as a static membership check
of that set.

## Helpers (in `src/stdlib/schema.rs`, `pub(crate)`)
- `is_schema_value(v: &Value) -> bool` — `Value::Object` with a `__kind` string field that is a
  known schema kind (`string|number|bool|nil|any|literal|array|object|map|optional|union|oneOf`).
  Narrow enough to never match a module namespace or an unrelated user object.
- `is_schema_method(name: &str) -> bool` — membership in the refiner/terminal set above.

## Blast radius
- `src/interp.rs` — ONE hook in the `ExprKind::Call` arm (must preserve all existing call
  behavior: instance/native/generator/class methods, module calls like `math.abs`/`schema.string`,
  plain object field-fn calls `o.f()`, optional `o?.m()` falls through). The fallback path must
  replicate today's `eval_chain(callee) → call_value` exactly.
- `src/stdlib/schema.rs` — add `is_schema_value` + `is_schema_method` helpers; no change to
  `parse_value` / constructors / dispatch.
- NO change to: schema representation, `json.parse`/`resp.json` bridge, `fromClass`, tree-sitter,
  fmt, LSP, `Value` enum. NON-BREAKING (free functions untouched; existing tests pass).

## Known limitation (documented, not a deferral)
Schema methods work in **call position** (`s.minLength(3)`). Extract-then-call
(`let f = s.minLength; f(3)`) is NOT supported — `s.minLength` (bare access) reads the stored
constraint field, not a bound method. This is the deliberate consequence of the call-vs-access
distinction that avoids the field/method collision. Fluent chaining (the feature's purpose) uses
call position throughout, so this is fully functional for all intended usage. Documented in the
schema docs.

## Sub-phases
- **A — core:** the `Call`-arm hook + `is_schema_value`/`is_schema_method` + tests (fluent chains,
  parse-as-method, mixed free+fluent, re-refine `minLength(3).minLength(5)`, regression: all
  existing method/module/object-fn calls unchanged).
- **B — integration:** example (`examples/fluent_schema.as` or extend `validation.as`), docs
  (`docs/content/stdlib/schema.md` — fluent section + the call-position note), README, CLAUDE.md
  (note the schema fluent call-site hook), full gates, holistic review, merge `--no-ff`.

## Decisions (made)
1. ADDITIVE — fluent methods + existing free functions both valid. **Settled.**
2. Call-site hook (not native-handle migration, not a method Value variant) — avoids the
   field/method collision + zero rep/bridge change. **Settled.**
3. Sources stay module functions; refiners/composites-with-schema-first-arg + `parse` are methods.
   **Settled.**
4. Call-position only (no extract-then-call) — documented limitation, not a deferral. **Settled.**

## Verification bar (per the goal)
Both `cargo test` configs green, both clippy `--all-targets` clean, fmt clean, BOTH conformance
tests pass (no grammar change → must be unaffected), the example runs + is fmt-idempotent, docs +
README + CLAUDE.md updated, no TODO/deferral. Holistic review confirms no regression to the call
path.
