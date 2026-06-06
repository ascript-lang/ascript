# Phase 3 — Core Value Types Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** Add `Value::Set` (over `IndexSet<MapKey>`) and `Value::Decimal` (over `rust_decimal`), constructor-only (no grammar change), with Decimal operator overloading.

**Architecture:** Two new `Value` variants. The compiler enumerates every exhaustive `Value` match that needs an arm (`cargo build` after adding the variant). New stdlib modules `std/set` (mirrors `std/map`) and `std/decimal`. Decimal arithmetic is wired into the binary-op evaluator (`interp.rs` `ExprKind::Binary`, ~:1288). NO lexer/parser/grammar/tree-sitter/fmt-grammar changes (constructor-only).

**Conventions:** Tier-2 panic on type misuse / non-hashable Set element / invalid `decimal.from` string; Tier-1 `[v,err]` for `decimal.parse`; stdlib module = `exports()` + dispatch, registered in BOTH `mod.rs` arms; clippy clean under `--all-targets` AND `--no-default-features --all-targets`; run BOTH `cargo test` configs (not just clippy); update docs + README + example. Full spec: `docs/superpowers/specs/2026-06-01-phase3-core-types-design.md`.

---

## Sub-phase 3a: `Set`

**Files:** `src/value.rs` (Value::Set variant + PartialEq/Display/MapKey/type arms), `src/interp.rs` (type_name + any Value dispatch + Set method dispatch mirroring Map), `src/stdlib/set.rs` (new), `src/stdlib/mod.rs` (register both arms), `src/stdlib/json.rs` (serialize as array), `src/stdlib/object.rs` (deep_equal/deep_clone arms), tests + example.

- [ ] **Step 1 — failing tests** (in set.rs): `set.new()`/`set.from([1,1,2])` (dedup → size 2), `add`/`has`/`delete`/`size`/`values` (insertion order), `union`/`intersection`/`difference`, non-hashable element (`set.from([[1]])`) → Tier-2 panic, `deep_equal` order-independent, json serializes a set as an array. If Map supports method-call dispatch (`m.get(k)`), add `s.has(x)` method test.
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:**
  - `value.rs`: add `Set(Rc<RefCell<indexmap::IndexSet<MapKey>>>)` to `enum Value`. Add arms: `PartialEq` → `Rc::ptr_eq`; `Display` → match Map's convention (check how `Map` Displays; use the analogous `set{...}` or `Set(len N)`); `MapKey::from_value` → `None` (Set not hashable).
  - Build; fix every compiler-flagged exhaustive `Value` match (type_name→"set", json `to_json`/`to_json_lossy`→array of values, object.rs `deep_equal`→order-independent set eq + `deep_clone`→new set, plus any others the compiler finds).
  - `set.rs`: `exports()` + dispatch for new/from/add/has/delete/size/values/union/intersection/difference. Mutating ops (add/delete) mutate in place; algebra ops return new sets. Element insertion uses `MapKey::from_value(v).ok_or_else(|| Tier-2 panic)`; `values()` reconstructs `Value`s from `MapKey` (mirror how `map.keys()` does it). Register in `mod.rs` both arms (gate: core — Set is fundamental; no feature gate, matching `map`).
  - Method dispatch: find where `Value::Map` method calls (`m.get(k)`) are routed in the interpreter; add the `Value::Set` parallel so `s.add(v)` etc. work. If Map has no method-dispatch path, skip (module-qualified only) and note it.
- [ ] **Step 4 — verify:** `cargo test` + `cargo test --no-default-features` (RUN both) + `cargo clippy --all-targets` + `--no-default-features --all-targets`. Green, 0 warnings.
- [ ] **Step 5 — commit:** `feat(set): Value::Set + std/set (new/from/add/has/delete/union/intersection/difference)`

---

## Sub-phase 3b: `Decimal`

**Files:** `Cargo.toml` (rust_decimal dep), `src/value.rs` (Value::Decimal + arms + maybe MapKey::Decimal), `src/interp.rs` (binary-op overloading + type_name + truthiness), `src/stdlib/decimal.rs` (new), `src/stdlib/mod.rs`, `src/stdlib/json.rs`, `src/stdlib/object.rs` (deep arms), tests + example.

- [ ] **Step 1 — failing tests** (in decimal.rs + interp binary-op tests): `decimal.from("1.50")` scale-preserving; `from(1.1)` exact; `decimal.parse("x")` → `[nil,err]`, `parse("1.5")` → `[d,nil]`; THE HEADLINE — `decimal.from("0.1") + decimal.from("0.2") == decimal.from("0.3")` (exact, unlike f64); Decimal×Number mixing (`decimal.from(2) * 3`); comparisons; `decimal.from(1)/decimal.from(0)` → Tier-2 panic; round/abs/floor/ceil; toString/toNumber; `decimal.from("1.5") == decimal.from("1.5")`; invalid `decimal.from("x")` → Tier-2 panic.
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:**
  - `Cargo.toml`: `rust_decimal = "1"` in `[dependencies]` (NOT optional/gated — Decimal is core). `cargo build` to update Cargo.lock.
  - `value.rs`: add `Decimal(rust_decimal::Decimal)` to `enum Value`. Arms: `PartialEq` → `a == b`; `Display` → canonical string; `MapKey` → add `MapKey::Decimal(Decimal)` (rust_decimal is Hash+Eq) so a Decimal can be a Map key / Set element (Number and Decimal keys distinct) — OR make Decimal non-hashable if it complicates canonicalization; document the choice.
  - Build; fix compiler-flagged matches: type_name→"decimal"; json→number token from canonical string; object.rs deep_equal (by value) + deep_clone (Copy); truthiness (match Number's zero rule).
  - `interp.rs` binary-op (`ExprKind::Binary`, ~:1288): when either operand is Decimal, coerce Number→Decimal via `Decimal::from_f64` (non-finite f64 + Decimal → Tier-2 panic), compute exact Decimal for `+ - * / %` (div-by-zero → Tier-2 panic, matching Number), and `bool` for `< > <= >=`. For `Eq`/`Ne` (~:1321) add cross-type Decimal/Number coercion BEFORE the generic `l == r`. Unary minus on Decimal. Extend the "operands must be numbers" panic wording to include decimal.
  - `decimal.rs`: `exports()` + dispatch for from (number|string; invalid string → Tier-2 panic), parse (→[d,err]), toString, toNumber, round(d,places=0) (round_dp half-up; document), abs, floor, ceil, trunc. Register in `mod.rs` both arms (core, no gate).
- [ ] **Step 4 — verify:** both `cargo test` configs + both clippy. Green, 0 warnings. Confirm the `0.1+0.2==0.3` exactness test passes.
- [ ] **Step 5 — commit:** `feat(decimal): Value::Decimal + std/decimal + operator overloading`

---

## Sub-phase 3 integration

- [ ] `examples/core_types.as`: sets (dedup, membership, algebra) + exact decimal money (sum a price list exactly; show `0.1+0.2`). Run it; ensure normal completion (no `exit`). Verify it parses under both parsers (`treesitter_conformance`, `frontend_conformance`) and is formatter-idempotent.
- [ ] Docs: `docs/content/stdlib/collections.md` (set), a decimal section (new page or in an existing numeric page), `docs/content/language/values-types.md` (add Set + Decimal to the value-kinds table), README stdlib table.
- [ ] FULL gates: `cargo test`, `cargo test --no-default-features`, `cargo clippy --all-targets`, `cargo clippy --no-default-features --all-targets`, `cargo fmt --check`, the example, both conformance tests.
- [ ] Holistic review (focus: every exhaustive Value-match site handled correctly — no panics/wrong arms; operator-overloading coercion correctness incl. div-by-zero and non-finite; json/deep_equal/deep_clone arms; no regressions; no TODOs). Then merge `--no-ff`.

## Self-review notes
- Riskiest: 3b operator overloading (logic, not compiler-enforced) + the MapKey::Decimal canonicalization choice. The 3b reviewer must specifically probe Decimal/Number coercion edges (non-finite f64, div-by-zero, equality across types) and confirm no existing Number arithmetic regressed.
- The compiler enforces exhaustive-match coverage for the new variants — but the *correctness* of each arm (esp. json, deep_equal, MapKey) is on the implementer + reviewer.
- No grammar/tree-sitter/fmt-grammar changes — confirm conformance tests still pass unchanged.
