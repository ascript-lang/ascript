# Phase 1 — Stdlib Completeness ("the everyday API")

- **Date:** 2026-05-31
- **Status:** Design — pending user review.
- **Roadmap:** Phase 1 of `2026-05-31-batteries-completeness-roadmap.md`.
- **Owner:** Mahmoud Kayyali

## Goal

Fill out `array`, `string`, `math`, and `object` with the everyday methods their
Go/Deno/JS-equivalents have, plus non-crypto checksums. Purely **additive native
functions** — no `value.rs`/grammar/interpreter-core change — with one deliberate,
pre-audited breaking change (`string.replace` semantics). This is the lowest-risk,
highest-daily-value phase and a warm-up for the harder phases.

## Inherited conventions (from the existing modules — new code matches these)

- **Non-mutating transforms.** `map`/`filter`/`slice`/`sort` return *new* arrays; only
  `push`/`pop` mutate. All new transforms (`reverse`, `concat`, `unique`, `flat`, …) return
  new arrays.
- **Equality.** `==` / `Value: PartialEq` is **value-equality for primitives**
  (nil/bool/number/str) and **identity (`Rc::ptr_eq`) for containers** (array/object/map/
  instance/bytes). `array.contains` already relies on this.
- **Errors.** Type misuse (wrong arg kind) is a **Tier-2 panic** (`want_array`/`want_string`
  /`want_number`). "Not found" / out-of-range is **graceful**: `nil` for element lookups,
  `-1` for index lookups (matches `array.get` → nil, `string.find` → -1).
- **Strings are char-indexed** (Unicode scalar values): `string.slice` operates on
  `s.chars()`, `string.find` returns a char index. All new string fns are char-based.
- **Callback-taking fns live on `impl Interp`** (they call user functions, are `async`);
  pure fns live in the module's `call`. New callback fns (`array.find`, `object.mapValues`,
  …) go on `impl Interp`.

## Decisions log (resolved forks)

1. **`array.groupBy` → `Map`** (key-type fidelity; no silent key→string coercion).
2. **`unique`/`indexOf` equality → follow language `==`** (primitives by value, containers
   by identity). Consistent with `contains`; fast; predictable. Two equal-*looking* objects
   are **not** deduped.
3. **`math.randomInt(min, max)` → inclusive `[min, max]`** (Python `randint` style).
4. **Stats live in `math`**: `sum/mean/median/stddev/variance` take a **single array**;
   `min`/`max` stay **variadic** (see Decision 6). `array` gets no numeric-only helpers.
5. **`string.indexOf` dropped** — redundant with existing `string.find`.
6. **`math.min`/`max` stay variadic-over-numbers; arrays via spread.** No array-overload —
   spread (`...`, shipped) already covers it: `math.min(...arr)`. Rationale for the split
   (variadic min/max, array-taking stats): min/max are general comparisons (often a few
   literals, JS-style); statistics are over datasets you already hold as an array
   (Python-style), and spreading a large column into a variadic call is noisy.
7. **`object.freeze` deferred out of Phase 1** — see Open Decisions; it is the only listed
   item that is *not* purely additive (needs mutation-path enforcement in the interpreter).

---

## `array` (all on `impl Interp::call_array`)

Qualified form `array.fn(arr, …)` shown; method form `arr.fn(…)` works via dispatch.

### Callback-taking (async)

| Fn | Signature | Semantics |
|----|-----------|-----------|
| `find` | `(arr, fn) -> value` | First element where `fn(el)` is truthy; `nil` if none. |
| `findIndex` | `(arr, fn) -> number` | Index of first match; `-1` if none. |
| `some` | `(arr, fn) -> bool` | `true` if any element matches. `false` on empty. Short-circuits. |
| `every` | `(arr, fn) -> bool` | `true` if all match. **`true` on empty.** Short-circuits. |
| `flatMap` | `(arr, fn) -> array` | `map(fn)` then flatten one level. Non-array results kept as-is. |
| `groupBy` | `(arr, keyFn) -> map` | `Map` from `keyFn(el)` → array of elements (insertion-ordered groups; elements in original order). |
| `partition` | `(arr, fn) -> array` | `[pass, fail]` — a 2-element array of arrays. |

### Pure

| Fn | Signature | Semantics |
|----|-----------|-----------|
| `indexOf` | `(arr, value) -> number` | First index where `el == value`; `-1` if none. |
| `flat` | `(arr, depth = 1) -> array` | Flatten to `depth` levels (default 1). `depth` is a non-negative integer (panic otherwise). Non-array elements kept. |
| `reverse` | `(arr) -> array` | New reversed array (non-mutating). |
| `concat` | `(arr, ...more) -> array` | New array; each `more` arg **must be an array** (panic otherwise) and is appended. |
| `first` | `(arr) -> value` | `arr[0]` or `nil` if empty. |
| `last` | `(arr) -> value` | Last element or `nil` if empty. |
| `unique` | `(arr) -> array` | New array, duplicates removed by `==`, first occurrence kept. |
| `chunk` | `(arr, n) -> array` | Array of arrays of size `n`; last chunk may be smaller. `n` is a positive integer (panic on `n <= 0` / non-integer). |
| `zip` | `(...arrays) -> array` | Array of tuples; length = shortest input. Requires ≥1 array (each arg must be an array). |
| `take` | `(arr, n) -> array` | First `n` elements; clamps to `[0, len]` (negative `n` → empty). |
| `drop` | `(arr, n) -> array` | All but the first `n`; clamps (negative `n` → full copy). |

---

## `string` (module `call`, char-based)

| Fn | Signature | Semantics |
|----|-----------|-----------|
| `startsWith` | `(s, prefix) -> bool` | |
| `endsWith` | `(s, suffix) -> bool` | |
| `contains` | `(s, sub) -> bool` | Substring test (empty `sub` → `true`). |
| `replace` | `(s, from, to) -> string` | **⚠ first occurrence only** (see breaking change). Empty `from` → `s` unchanged. |
| `replaceAll` | `(s, from, to) -> string` | All occurrences (the old `replace`). Empty `from` → `s` unchanged. |
| `chars` | `(s) -> array` | Array of one-char strings (Unicode scalars). |
| `lines` | `(s) -> array` | Split into lines (Rust `str::lines` semantics: handles `\n`/`\r\n`, no trailing empty for a final newline). |
| `reverse` | `(s) -> string` | Reversed by scalar. (Doc caveat: combining characters / graphemes are not reordered as clusters.) |
| `count` | `(s, sub) -> number` | Count of non-overlapping occurrences. Empty `sub` → `0`. |
| `splitN` | `(s, sep, n) -> array` | Split into at most `n` parts (Rust `splitn`). |

*Not added:* `indexOf` (use existing `find`).

---

## `math` (module `call`)

### Trig & exponential

`sin`, `cos`, `tan`, `asin`, `acos`, `atan` — `(x) -> number` (radians).
`atan2` — `(y, x) -> number`. `exp` — `(x) -> number`.
`ln` — natural log. `log2`, `log10` — base-2 / base-10 logs.

### Scalar helpers

| Fn | Signature | Semantics |
|----|-----------|-----------|
| `sign` | `(x) -> number` | `-1`/`0`/`1`; `sign(0) == 0`; `sign(NaN) == NaN`. |
| `trunc` | `(x) -> number` | Toward zero. |
| `clamp` | `(x, lo, hi) -> number` | `lo` if `x < lo`, `hi` if `x > hi`. Panic if `lo > hi`. |
| `hypot` | `(x, y) -> number` | `sqrt(x² + y²)`. |
| `gcd` | `(a, b) -> number` | On integer values (panic on non-integer); operates on absolute values; `gcd(0,0) == 0`. |
| `lcm` | `(a, b) -> number` | On integer values; `lcm(0, _) == 0`. |

### Aggregation / statistics (take an array)

| Fn | Signature | Semantics |
|----|-----------|-----------|
| `sum` | `(arr) -> number` | Sum of numbers; empty → `0`; non-number element → panic. |
| `min` / `max` | `(...nums) -> number` | **Unchanged** — variadic over numbers (≥1 arg). For an array, spread it: `math.min(...arr)`. |
| `mean` | `(arr) -> number` | `sum/len`; **panic on empty array**. |
| `median` | `(arr) -> number` | Middle of a sorted copy (mean of two middles for even length); panic on empty. |
| `variance` | `(arr, sample = false) -> number` | `sample=false` → population (÷ n), panic on empty. `sample=true` → sample (÷ n−1), panic if `len < 2`. |
| `stddev` | `(arr, sample = false) -> number` | `sqrt(variance(arr, sample))`; same `sample` flag and edge cases. |

### Random (extends existing `math.random`)

| Fn | Signature | Semantics |
|----|-----------|-----------|
| `randomInt` | `(min, max) -> number` | Uniform integer in **`[min, max]` inclusive**. `min`/`max` integers, `min <= max` (panic otherwise). |
| `shuffle` | `(arr) -> array` | New array, Fisher–Yates shuffled (non-mutating). |
| `choice` | `(arr) -> value` | Uniform random element; `nil` if empty. |

(Decision note: `stddev`/`variance` are **population**, not sample, for predictability; a
`sample: true` option can be added later if needed.)

---

## `object`

| Fn | Signature | Where | Semantics |
|----|-----------|-------|-----------|
| `fromEntries` | `(arr) -> object` | `call` | Build an Object from `[[k, v], …]` pairs; keys stringified; later key wins. |
| `pick` | `(obj, keys) -> object` | `call` | New Object with only `keys` (array of strings) that are present. |
| `omit` | `(obj, keys) -> object` | `call` | New Object without `keys`. |
| `mapValues` | `(obj, fn) -> object` | `impl Interp` | New Object; value = `fn(value, key)`; keys/order preserved. |
| `deepClone` | `(v) -> value` | `call` | Deep copy (see below). |
| `deepEqual` | `(a, b) -> bool` | `call` | Structural deep compare (see below). |

`pick`/`omit`/`mapValues` accept an `Object` or `Instance` (Instance → its fields), returning
an `Object`; other kinds panic.

### `deepEqual(a, b)` semantics

- Primitives: by value (`==`), so `NaN != NaN`, `-0.0 == 0.0` (per existing `==`).
- `Array`: equal length and elementwise `deepEqual`.
- `Object`: same key **set** and `deepEqual` values (order-independent).
- `Map`: same key set and `deepEqual` values.
- `Instance`: same class (identity) and `deepEqual` over the same field set.
- `Bytes`: byte-for-byte equal.
- Functions / natives / regex / enums / classes: fall back to identity `==`.
- Different kinds → `false`.

### `deepClone(v)` semantics

- Primitives: returned as-is.
- `Array`/`Object`/`Map`/`Bytes`: new container, values deep-cloned.
- `Instance`: new instance of the same class with deep-cloned fields (does **not** run
  `init`).
- Functions / natives / regex / enums / classes / futures / generators: **shared by
  reference** (not cloneable).
- **Shared/cyclic structure:** an identity map (`Rc` pointer → clone) preserves sharing and
  terminates on cycles (a node appearing twice clones once; a cycle re-points to the
  in-progress clone). No infinite recursion.

---

## Checksums (non-crypto) — in `crypto`

| Fn | Signature | Semantics |
|----|-----------|-----------|
| `crc32` | `(bytes \| string) -> number` | CRC-32 (IEEE). String → UTF-8 bytes. |
| `xxhash` | `(bytes \| string) -> number` | xxHash (64-bit, returned as number — document the >2^53 precision caveat, or return hex string; see Open Decisions). |

Distinct from the cryptographic hashes already in `crypto` (`sha256`/`sha512`/`md5`/
`hmacSha256`). Pick small, well-maintained crates (`crc32fast`, `xxhash-rust`) behind the
existing `crypto` feature.

---

## The `string.replace` breaking change (folded into this phase)

`string.replace` currently calls Rust `str::replace` → replaces **every** occurrence
(`src/stdlib/string.rs:78`; codified by the test at `:173`). This phase splits the two:

- `replace(s, from, to)` → **first occurrence only** (`str::replacen(from, to, 1)`).
- `replaceAll(s, from, to)` → all occurrences (the old behavior).

**Blast radius (fully enumerated, pre-audited):**
- `src/stdlib/string.rs:78` — impl change.
- `src/stdlib/string.rs:173`, `:191` — update tests; add `replaceAll` tests.
- `docs/content/stdlib/collections.md:121` — update the one doc example; document
  `replaceAll`.
- No `examples/*.as` relies on the old behavior (only `regex.replace`, unaffected).

Rationale: a plain `replace` for a targeted single edit, explicit `replaceAll` when you mean
all — makes the two non-redundant and matches the common ergonomic split.

---

## Resolved decisions (were open)

1. **`object.freeze` — DEFERRED out of Phase 1.** It is the only non-additive item
   (enforcing immutability needs a "frozen" flag on the container and a check on the
   assignment path in `interp.rs`). Revisit later as its own small item / alongside other
   mutation-path work. Keeps Phase 1 at zero interpreter-core risk.
2. **`xxhash` returns a hex `string`** (lossless). `crc32` returns a `number` (fits exactly
   in 32 bits).
3. **`stddev`/`variance` take a `sample` flag (default `false`).** `false` → population
   (÷ n, defined for n=1); `true` → sample (÷ n−1, Bessel's correction, panic if `len < 2`).
   Shipped in Phase 1 so the API is complete from the start.

## Test plan

- Unit tests inline in each module (mirror the existing `#[tokio::test]`/`#[test]` style):
  happy path, empty-input edges, not-found (`nil`/`-1`), type-misuse panics, and the
  decided semantics (e.g. `every([]) == true`, `unique` identity-vs-value, `randomInt`
  inclusivity bounds, `groupBy` returns a `Map`, `min/max` array-vs-variadic overload).
- `string.replace` first-occurrence + `replaceAll` all-occurrence tests; update the two
  existing tests.
- One new `examples/*.as` exercising the everyday API (exercised by the conformance tests),
  or extend an existing example — keep it runnable.

## Docs to update

- `docs/content/stdlib/collections.md` — `array`/`string`/`object` new fns; fix the
  `string.replace` example; document `replaceAll`.
- `docs/content/stdlib/*` math page — new `math` fns.
- `crypto` stdlib page — `crc32`/`xxhash`.
- `README.md` stdlib table if function-level lists are shown there.

## Out of scope (this phase)

`object.freeze` enforcement (see Open Decisions); anything touching `value.rs`/grammar;
`Set`/`Decimal` (Phase 3); sample-statistics variants.
