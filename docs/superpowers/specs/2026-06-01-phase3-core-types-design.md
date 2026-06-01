# Phase 3 — Core Value Types: `Set` + `Decimal`

- **Date:** 2026-06-01
- **Status:** Design — proceeding under the standing multi-phase goal; key syntax decisions confirmed with the owner.
- **Roadmap:** Phase 3 of `2026-05-31-batteries-completeness-roadmap.md`.
- **Owner:** Mahmoud Kayyali

## Goal

Add the two fundamental value types the language is missing: a `Set` collection (membership,
dedup, union/intersection/difference) and an exact `Decimal` number (money, ints > 2^53). Both
are new `Value` variants. **Confirmed decisions:** constructor-only construction (NO literal
syntax → NO lexer/parser/grammar/tree-sitter changes), Decimal participates in operator
overloading (`+ - * / %` and comparisons), and the phase is split into **3a (Set)** then
**3b (Decimal)**.

This is the first phase to touch the value model. The risk is contained because there is **no
grammar change**: the work is `value.rs` (2 variants), the interpreter's exhaustive `Value`
matches (compiler-enforced — `cargo build` surfaces every site), the binary-op evaluator (3b
only), and two additive stdlib modules. `fmt.rs`/tree-sitter/LSP grammar are untouched (no new
syntax); the only LSP touch is adding any new global/module names to completion lists.

## Cross-cutting: updating exhaustive `Value` matches

Adding a `Value` variant breaks every non-`_` match on `Value`. The compiler enumerates them;
each must get a correct arm. Known sites (verify by building):
- `value.rs`: `impl PartialEq for Value` (~:291), `impl Display` (~:340), `MapKey::from_value`
  (~:24), any `Hash`/helper.
- `interp.rs`: `type_name`, the binary-op evaluator (`ExprKind::Binary`, ~:1288), truthiness,
  any value-kind dispatch.
- `fmt.rs`: value rendering if it matches `Value` (Display-level only; no expr grammar).
- `src/stdlib/json.rs`: `to_json`/`to_json_lossy` serialization.
- `src/stdlib/object.rs`: `deep_equal` / `deep_clone` (add arms for the new variants).
- `src/diagnostics.rs` / anywhere matching `Value` for messages.

---

## 3a — `Set`

### Representation
`Value::Set(Rc<RefCell<IndexSet<MapKey>>>)`.
- `IndexSet` (from the already-present `indexmap` crate) preserves insertion order and gives
  O(1) membership.
- Elements are stored as `MapKey` — the same hashable, canonicalized key type `Map` uses
  (numbers canonicalized −0.0→+0.0 / NaN unified; primitives only). **Inserting a non-hashable
  element (array/object/instance/etc.) is a Tier-2 panic**, exactly like using one as a `Map`
  key (`MapKey::from_value` returns `None`). Iteration reconstructs `Value`s from `MapKey` (the
  same conversion `Map.keys()`/`entries()` already use). Because only hashable primitives can be
  stored, **a Set cannot contain cycles**.

### Construction & API — `std/set` module (mirrors `std/map`)
Register `src/stdlib/set.rs` like `map`. Set is mutated in place by add/delete (like
`map.set`); algebra ops return new Sets.
- `set.new() -> set` — empty.
- `set.from(array) -> set` — dedup an array into a set (non-hashable element → Tier-2 panic).
- `set.add(s, v) -> set` — insert (returns `s` for chaining); idempotent.
- `set.has(s, v) -> bool`.
- `set.delete(s, v) -> bool` — true if it was present.
- `set.size(s) -> number`.
- `set.values(s) -> array` — elements in insertion order.
- `set.union(a, b) -> set` · `set.intersection(a, b) -> set` · `set.difference(a, b) -> set`
  — new sets (a − b for difference).
- `set.clear(s)` — empties in place. (Include only if `map` has a parallel; otherwise omit to
  match map's surface.)

**Method-call dispatch:** if the interpreter routes `m.get(k)` for `Value::Map` to the map
module (it does — Phase 1's `groups.get(1)` worked), mirror the SAME mechanism so `s.add(v)`,
`s.has(v)`, etc. work on `Value::Set`. Check the Map method-dispatch site and add the Set
parallel. If Map has no such path and is only called as `map.get(m,k)`, then Set matches that
(module-qualified) and no method dispatch is added.

### Value-match arms (3a)
- `PartialEq`: `(Set(a), Set(b)) => Rc::ptr_eq(a, b)` (identity, like other containers).
- `Display`: `set{1, 2, 3}` style (e.g. `format!("set{{{}}}", elems)`), or `Set(len N)` if
  matching the `Map(len N)` convention — pick consistency with how `Map` Displays (check it).
- `type_name`: `"set"`.
- `MapKey::from_value`: a Set is NOT hashable → `None` (can't be a Set element or Map key), like
  arrays.
- `json` (`to_json`/`to_json_lossy`): serialize a Set as a JSON **array** of its values.
- `deep_equal`: structural — same size and same elements (order-independent set equality).
- `deep_clone`: new Set with the same elements (no cycle concern; primitives).

### Tests (3a)
new/from/add/has/delete/size/values; dedup via `set.from([1,1,2])`; insertion-order preserved
in `values`; union/intersection/difference correctness; non-hashable element → Tier-2 panic;
`deep_equal` order-independence; json round-trips as array; (if method dispatch) `s.has(x)`.
Plus an example.

---

## 3b — `Decimal`

### Representation
`Value::Decimal(rust_decimal::Decimal)`.
- Add `rust_decimal = "1"` to `[dependencies]` (core — NOT feature-gated; Decimal is
  fundamental). `Decimal` is `Copy`, 96-bit mantissa + scale, `Hash + Eq + Ord`.

### Construction & API — `std/decimal` module
Avoid a mixed-return constructor by splitting fallible/infallible:
- `decimal.from(x) -> decimal` — `x` is a number (exact: integers exact; non-integer f64 via
  `Decimal::from_f64` shortest-round-trip so `decimal.from(1.1)` is exactly `1.1`, not f64
  noise) or a **valid** decimal string. Invalid string → **Tier-2 panic** (constructor for
  known-good values).
- `decimal.parse(s) -> [decimal, err]` — Tier-1 safe parse for untrusted input.
- `decimal.toString(d) -> string` (canonical, scale-preserving) · `decimal.toNumber(d) ->
  number` (lossy f64).
- `decimal.round(d, places=0) -> decimal` (banker's or half-up — pick `round_dp` half-up;
  document) · `decimal.abs(d)` · `decimal.floor(d)` · `decimal.ceil(d)` · `decimal.trunc(d)`.
- Keep the surface focused; arithmetic is via operators (below), not methods.

### Operator overloading (the core change)
In the binary-op evaluator (`interp.rs` `ExprKind::Binary`, ~:1288), extend the arithmetic and
comparison handling: **if either operand is `Decimal`**, coerce the other and produce a
`Decimal` (arithmetic) or `bool` (comparison) result.
- Coercion: `Number → Decimal` exactly via `Decimal::from_f64` (a non-finite f64 with a Decimal
  operand → Tier-2 panic, since Decimal has no NaN/Inf). `Decimal op Decimal` direct.
- Arithmetic: `+ - * /` and `%` → exact `Decimal`. **Division by zero → Tier-2 panic** (consistent
  with whatever Number division does — check; match it). Multiplication/precision: rust_decimal
  handles scale; document that `/` uses rust_decimal's default precision.
- Comparisons: `< > <= >=` between Decimal/Decimal and Decimal/Number → `bool` (coerce Number).
- Equality (`==`/`!=`, handled at ~:1321 via `l == r`): make `Decimal == Decimal` work (via
  `PartialEq` arm) AND `Decimal == Number` compare by coercing the Number to Decimal. Since the
  `Eq`/`Ne` arms currently short-circuit on `Value::PartialEq`, add explicit cross-type handling
  in the evaluator BEFORE the generic `l == r`, OR make `PartialEq` itself coerce (prefer
  evaluator-level cross-type so `PartialEq` stays simple identity/value).
- Mixing Decimal with a non-numeric operand → the existing Tier-2 "operands must be numbers"
  panic (extend its wording to mention decimal).
- Unary minus on a Decimal → negated Decimal.

### Value-match arms (3b)
- `PartialEq`: `(Decimal(a), Decimal(b)) => a == b` (value). (Cross-type Decimal/Number equality
  lives in the evaluator, not here.)
- `Display`: the decimal's canonical string (scale-preserving, e.g. `1.50`).
- `type_name`: `"decimal"`.
- `MapKey::from_value`: Decimal IS `Hash + Eq` → allow it as a key/Set element via a new
  `MapKey::Decimal` variant. (Keep Number and Decimal keys distinct — `1` number and `1`
  decimal are different keys; document. If this complicates canonicalization, defer Decimal keys
  and make Decimal non-hashable instead — note which you chose.)
- `json`: serialize Decimal as a JSON **number** using its canonical string (JSON permits
  arbitrary-precision number tokens); `to_json_lossy` likewise. (If the JSON value model forces
  f64, serialize as a string and document — prefer number token.)
- `deep_equal`: by value. `deep_clone`: `Decimal` is `Copy` → clone trivially.
- Truthiness: a Decimal is truthy unless zero? Match Number's truthiness rule (Number 0 is
  falsy? check — mirror it for Decimal `0`).

### Tests (3b)
`decimal.from("1.50")` preserves scale; `from(1.1)` exact; `from(int)`; `parse` Tier-1 ok/err;
operator results exact (`0.1 + 0.2 == 0.3` in decimal — the headline); Decimal×Number mixing;
comparisons; division-by-zero panic; round/abs/floor/ceil; toString/toNumber; equality
Decimal==Number; json round-trip; (if hashable) Decimal as a Map key / Set element. Plus the
example showing exact money math vs f64.

---

## Sub-phase order & gates
1. **3a Set** — value variant + matches + `std/set` + tests + example slice → review → commit.
2. **3b Decimal** — value variant + matches + operator overloading + `std/decimal` + tests +
   example slice → review → commit.
3. **Integration** — example `examples/core_types.as` (sets + exact decimal money), docs
   (`docs/content/stdlib/collections.md` for set, a decimal page; `values-types.md` for the two
   new value kinds; README table), full gates (`cargo test` both configs, clippy both
   `--all-targets`, `fmt --check`, conformance, formatter idempotence), holistic review, merge
   `--no-ff`.

## Decisions (confirmed)
1. Set & Decimal are constructor-only — **no literal syntax, no grammar/tree-sitter changes.**
2. Decimal uses **operator overloading** for `+ - * / %` and comparisons.
3. Phase split **3a Set, 3b Decimal**.
4. Set elements / Map keys: non-hashable → Tier-2 panic (existing MapKey rule).
5. `decimal.from` panics on invalid string; `decimal.parse` is the Tier-1 safe variant (avoids a
   mixed-return-type constructor).

## Open implementation choices (decide during impl, document the pick)
- Decimal as a hashable key (new `MapKey::Decimal`) vs non-hashable — prefer hashable if clean.
- Set `Display` format (`set{...}` vs `Set(len N)`) — match Map's convention.
- Decimal rounding mode for `round` — half-up via `round_dp` (document).
