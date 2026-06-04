# Ranges (inclusive + `step`) and the Analyzer Batch — Design

- **Date:** 2026-06-03
- **Status:** Design approved; ready for implementation plan
- **Scope:** Range semantics overhaul (`..=` everywhere, `step`, sequence direction, validation), `stream.range` migration to the unified model, and three new `ascript check` rules.
- **Suggested branch:** `feat/ranges-step-analyzer`

---

## 1. Motivation

Two papercuts started this:

1. **The upper bound reads as misleading.** `for (i in 1..6)` prints `1..5`; a half-open
   bound that *looks* like a last value surprises readers.
2. **Descending ranges silently do nothing.** `for (i in 10..1)` runs zero times today —
   a silent no-op the author almost never intended.

We resolve both by making `..` a **sequence** (direction follows the bounds, so `10..1`
counts down), adding an explicit **inclusive** form `..=`, and adding a **`step`** modifier
for strided iteration in either direction. The same model is applied to the existing
`stream.range` stdlib primitive so the language has exactly **one** range mental model.

Alongside the language change we land three **`ascript check`** rules — one that makes the
new range/step rules teachable at author-time, plus two high-value gaps from the analyzer
menu (`?`-validity and import-path resolution).

This document is the authoritative design. It supersedes the range bullet in
`2026-05-29-ascript-design.md` where they conflict.

---

## 2. Background — current state (as of merge `0479c06`)

Knowing exactly what exists keeps the plan honest. The big shifts since the original spec:

- **The bytecode VM is the default engine.** `ascript run file.as` →
  `run_file_on_vm` (compile to bytecode + VM), `src/main.rs:94-103`. The async
  tree-walker (`src/interp.rs`) is retained as a **differential oracle** and the
  `--tree-walker` / `ASCRIPT_ENGINE=tree-walker` debugging engine, kept **byte-identical**
  to the VM.
- **Two front-ends.** The VM/checker path uses the lossless `cstree` CST in `src/syntax/`
  (`lexer → parser → tree_builder → resolve → compile → vm`). The tree-walker still uses
  the legacy `src/lexer.rs` / `src/parser.rs` / `src/ast.rs`. A syntax change touches
  **both**, plus the tree-sitter grammar.
- **A resolver exists** (`src/syntax/resolve/`), but it is a name→slot/upvalue lowering pass
  for the compiler plus binding bookkeeping for the checker. It emits exactly one
  diagnostic (top-level duplicate binding, `src/syntax/resolve/mod.rs:194`). It is **not**
  the home for new static checks.
- **A linter exists** (`src/check/`), with ~10 rules and a full severity-config system
  (`ascript.toml [lint]`, `--deny/--warn/--allow`, inline `// ascript-ignore[code]`),
  wired into both `ascript check` and the LSP (`src/lsp/analysis.rs:73`). `check` is
  **opt-in** — `run`/`build` never call it.
- **The error model is still two tiers.** Tier-1 (recoverable `[value, err]`) and Tier-2
  (runtime panic). The bytecode compiler/verifier add only *structural/integrity* errors;
  semantic errors (undefined vars, immutability, contracts) remain **runtime-timed and
  byte-identical across engines**. There is **no** semantic static-reject phase, by design.

Range-specific current behavior:

| Construct | Status today | File |
|---|---|---|
| `..` exclusive in for-range | works; lazy | `src/compile/mod.rs` `compile_for` |
| `..` exclusive as a value | works; materializes to `array` | `src/compile/mod.rs` |
| `10..1` (descending bare) | **empty** (ascending-only) | both engines |
| `..=` inclusive | **only in match patterns** | `Pattern::Range.inclusive`, `src/ast.rs:347` |
| `..=` in for-range | **rejected**: "inclusive for-range … not supported" | `src/compile/mod.rs:2461-2466` |
| `..=` as a value | **rejected**: "inclusive range (..=) as a value is not yet supported in V2" | `src/compile/mod.rs:322-323` |
| `step` | **does not exist** — not a token, keyword, AST field, or production | — |
| `stream.range(a,b,s?)` | interval + signed step; `range(10,1)` → `[]` | `src/stdlib/stream.rs:285,506-514` |

**Update 2026-06-04:** the rejections in this snapshot were closed by the ranges/step work.
`print(1..=5)` → `[1,2,3,4,5]` and descending `for (i in 10..=1 step -2)` iterates `10,8,6,4,2`;
`..=` and descending+`step` now run on both engines (verified). The rows above are the historical
"status today" snapshot, kept intact for the record.

Tokens already present: `Tok::DotDot` (`..`), `Tok::DotDotEq` (`..=`),
`Tok::DotDotDot` (`...`, spread/rest — **not** a range token).
(`src/token.rs:45-48`, `src/syntax/kind.rs:91-93`.)

---

## 3. Range semantics — the unified model

One model, applied identically wherever a range appears (for-range, value position, match
pattern, and `stream.range`).

### 3.1 Direction (sequence)

- **`step` omitted:** direction is inferred from the bounds. `start < end` → ascending,
  step `+1`; `start > end` → descending, step `−1`; `start == end` → empty.
- **`step` present:** the step's **sign is honored** as the direction.

### 3.2 Boundary

- `a..b` — **exclusive** upper/lower endpoint (half-open).
- `a..=b` — **inclusive** endpoint.

### 3.3 `step`

`step` is a **contextual keyword** (recognized only in range position). It must remain
usable as an ordinary identifier elsewhere — e.g. the generator example
`let step = yield n` in `2026-05-29-ascript-design.md:487` must keep working. `step`
accepts any numeric expression, including negative and floating-point values.

### 3.4 Validation (Tier-2 panic at range materialization)

A range is **invalid** and panics when materialized (loop entry, value construction, or
`stream.range` call) if either:

1. `step` is `0`, `NaN`, or `±Infinity` →
   *"step must be a finite, non-zero number."*
2. `start != end` **and** `sign(step) != sign(end − start)` (direction mismatch) →
   *"step `<k>` moves away from end (`<end>`); range can never progress."*

`start == end` is always the empty (or, for `..=`, single-element) range and never a
mismatch — there is no direction to disagree with. A valid step that overshoots the end
simply stops (`1..10 step 100` → `[1]`).

These are **runtime panics** (Tier-2), mirroring how the contract system already treats
malformed code. Statically detectable cases are *additionally* surfaced by the
`range-step` lint (§5.1) so they are caught at author-time, but the runtime behavior is the
single source of truth and is identical across both engines.

### 3.5 Canonical truth table

| Expression | Result |
|---|---|
| `1..5` | `1, 2, 3, 4` |
| `1..=5` | `1, 2, 3, 4, 5` |
| `5..1` | `5, 4, 3, 2` |
| `5..=1` | `5, 4, 3, 2, 1` |
| `1..10 step 2` | `1, 3, 5, 7, 9` |
| `1..=10 step 2` | `1, 3, 5, 7, 9` |
| `10..1 step -2` | `10, 8, 6, 4, 2` |
| `10..=1 step -2` | `10, 8, 6, 4, 2` |
| `0..=1 step 0.25` | `0, 0.25, 0.5, 0.75, 1.0` |
| `10..1 step 2` | **panic** (mismatch) |
| `1..10 step -2` | **panic** (mismatch) |
| `1..10 step 0` | **panic** (zero) |
| `5..5` | `[]` |
| `5..=5` | `[5]` |

### 3.6 Value position

A range used as a value **materializes to an `array<number>`**, honoring the same model:

- `print(1..=5)` → `[1, 2, 3, 4, 5]` (unblocks the V2 deferral at
  `src/compile/mod.rs:322-323`).
- `print(10..1 step -2)` → `[10, 8, 6, 4, 2]`.

For-range iteration stays **lazy/allocation-free** (no intermediate array); only explicit
value-position use materializes.

### 3.7 Match patterns — strided membership

`step` is permitted in match-range patterns. A pattern `start..end step k` matches `x` iff:

- `x` is within bounds (respecting `..`/`..=` and direction), **and**
- `x` is on the stride from the anchor: `(x − start)` is an integer multiple of `k`.

So `match n { 1..=10 step 2 => … }` matches `{1, 3, 5, 7, 9}`; `0..=10 step 2` matches
`{0, 2, …, 10}`. The **anchor is `start`** — parity/offset depends on where the range
begins, not only on the bounds.

Match-range bounds are constant expressions (as today), so the **`range-step` lint
validates every stepped pattern statically** — a bad pattern (`step 0`, mismatch) is caught
before run, and a *float* step in a pattern is flagged as unreliable (§5.1). The runtime
validation rules (§3.4) still apply uniformly, so the engines stay byte-identical between
expression-ranges and pattern-ranges.

### 3.8 Float steps

Float steps are allowed everywhere (`0..=1 step 0.25`). As with the existing `i += 1.0`
iteration, repeated `i += step` **accumulates rounding** — `0..1 step 0.1` will not land
cleanly on `0.9`. This is documented, not corrected (no decimal/rational iteration). In
match patterns the stride test is exact-equality on floats and is therefore fragile; see
the `range-step` advisory (§5.1).

---

## 4. `stream.range` migration

`stream.range(start, end, step?)` (`src/stdlib/stream.rs:285`) is migrated from its current
interval + signed-step semantics to the **unified model** above, so the syntax and the
stdlib agree:

- `range(1, 5)` → `1, 2, 3, 4` *(unchanged)*.
- `range(0, 10, 2)` → `0, 2, 4, 6, 8` *(unchanged)*.
- `range(10, 0, -3)` → `10, 7, 4, 1` *(unchanged — descending bounds + negative step agree)*.
- `range(10, 1)` → `10, 9, 8, …, 2` — **CHANGED** (was `[]`; now direction inferred).
- `range(1, 10, -2)` → **panic** (mismatch) — **CHANGED** (was silently `[]`).
- `range(a, b, 0)` → panic *(unchanged)*.

The three **documented** examples (`range(0,5)`, `range(0,10,2)`, `range(10,0,-3)`) are
unchanged; only undocumented edge behavior tightens (descending-default now counts down;
sign-mismatch now panics instead of empty). Update:

- the advance logic at `src/stdlib/stream.rs:506-514` (direction now from bounds when step
  omitted; mismatch → panic),
- the docs at `docs/content/stdlib/stream.md:67-74`,
- the tests at `src/stdlib/stream.rs:871` (`range_with_step`) and `:921`
  (`range_negative_step_counts_down`), adding cases for the new inferred-direction and
  mismatch-panic behavior.

---

## 5. Analyzer batch — three new `check` rules

All three follow the existing pattern: a plain `fn(&ResolvedNode, &ResolveResult, &str) ->
Vec<AsDiagnostic>` registered in `src/check/rules/mod.rs:20` (`ALL`), with its code added to
`RULE_CODES` (`src/check/config.rs:27`). They flow to `ascript check` and the LSP for free.
None blocks execution (`check` is opt-in); they are advisory diagnostics. The
`contract-mismatch` rule (`src/check/rules/contract.rs:27`) is the precedent — a
literal-vs-annotation mismatch surfaced as a configurable lint while the runtime violation
remains a Tier-2 panic.

### 5.1 `range-step`

Flags statically-detectable bad ranges over a `RangeExpr`/`RangePat` with literal operands:

- literal `step 0` / `NaN` / `±Infinity` → matches the guaranteed runtime panic.
- literal **direction mismatch** (`sign(step) != sign(end − start)`, `start != end`) →
  matches the guaranteed runtime panic.
- **advisory:** a **float** `step` inside a **match pattern** → "float-step membership may
  not match exactly; consider a guard." (Correctness hazard unique to the predicate
  position; does not apply to loops/values.)

### 5.2 `invalid-propagate` (the `?`-validity check, menu item C1)

Flags a postfix `?` (`Try`) used inside a function whose **declared** return type is not a
`Result`/pair. Closes the long-unenforced promise in `2026-05-29-ascript-design.md:257`
("Using `?` in a function that does not return a Result pair is a compile-time error").
Only enforceable when the function carries a return-type annotation; an unannotated function
cannot be statically proven to violate it and is not flagged.

### 5.3 `unresolved-import` (menu item F1)

Flags an `import` whose path does not resolve: a `std/*` path not in the static export
registry (`std_module_exports`), or a relative file path that does not exist on disk. This
is cross-file yet fully reliable — a registry/filesystem lookup, no module execution needed.
(Distinct from the existing `unused-import`, which checks *usage*, not *resolvability*.)

### 5.4 Default severities

| Code | Default severity | Notes |
|---|---|---|
| `range-step` | Warning | configurable; bump to Error via config if desired |
| `invalid-propagate` | Warning | spec §257 calls it an error; default Warning to fit the lint model |
| `unresolved-import` | Warning | configurable |

Rationale: consistency with the existing advisory rules (`contract-mismatch` etc.) and with
the project's "advisory lint, not hard block" stance. Any of these can be raised to Error
per-project via `ascript.toml [lint]` or `--deny`.

---

## 6. Error-model positioning

This feature **adds no new error lifecycle.** Bad ranges are Tier-2 **runtime panics**
(both engines, byte-identical), and statically-detectable cases are *additionally* surfaced
as **advisory lint diagnostics**. We deliberately do **not** introduce a static-reject
("refuse to run") phase, because the project keeps all semantic errors runtime-timed to
preserve the VM↔tree-walker byte-identical guarantee. This mirrors `contract-mismatch`
exactly (lint + runtime panic) and keeps the two-tier model intact.

---

## 7. Implementation surface

A syntax change now costs two front-ends plus the parity machinery. Touch-points:

**Legacy front-end (tree-walker):**
- `src/lexer.rs` — `step` contextual keyword (tokens `..`/`..=` already lex).
- `src/parser.rs` — accept `..=` and `step` in for-range and value-range position
  (currently `..=` is rejected outside patterns). **Update 2026-06-04:** done — `..=` and `step`
  are accepted in for-range and value position; `..=` is no longer rejected outside patterns.
- `src/ast.rs` — extend `ForRange` (`:248`) and the value-range node (`BinOp::Range`,
  `:467`) with `inclusive: bool` and `step: Option<Expr>`; extend `Pattern::Range` (`:347`)
  with `step: Option<Expr>`.
- `src/interp.rs` — sequence direction, `step`, validation panics; lazy for-range, array
  materialization for value position; strided pattern membership.

**CST front-end (VM/checker):**
- `src/syntax/lexer.rs` — `step` contextual keyword.
- `src/syntax/parser.rs` / `src/syntax/tree_builder.rs` / `src/syntax/ast/` — `inclusive`
  + `step` in `RangeExpr`/`RangePat`/`ForStmt`.
- `src/compile/mod.rs` — remove the V2 rejections (`:322-323`, `:2461-2466`); codegen for
  sequence direction, `step`, inclusive boundary, value-position `..=`.
- `src/vm/` — loop/range opcodes for direction + step; **bump `ASO_FORMAT_VERSION`** if the
  opcode set or `Chunk` layout changes (`src/vm/aso.rs`); update the verifier's
  `stack_effect` for any new/changed opcode (`src/vm/verify.rs`).

**Shared / surrounding:**
- Tree-sitter grammar — `..=` and `step` in expression/for positions; **regenerate
  `parser.c` with `tree-sitter generate --abi 14`**.
- `src/fmt.rs` — render `..=` and `step` (and the precedence so `a..b step c` round-trips).
- LSP keyword list — add `step` (contextual).
- `src/stdlib/stream.rs` + `docs/content/stdlib/stream.md` — the migration (§4).
- `src/check/rules/` (three new files) + `src/check/config.rs` `RULE_CODES` + checker docs.
- Docs: language guide range section, `docs/content/*`, README range mentions.

**Engine parity & differential:**
- Regolden the whole-corpus differential for the `..` count-down behavior change and the
  `stream.range` migration (`tests/vm_differential.rs:712-726`, `tests/aso.rs`).
- Both engines must produce byte-identical output or the three-way/whole-corpus differential
  fails — fix the engine, never weaken the assertion.

---

## 8. Migration / breakage assessment

- **Corpus:** no `examples/*.as` uses a descending bare range, so the `..` count-down change
  does not silently alter example output (verified by grep). The differential goldens still
  must be refreshed for any newly-exercised behavior.
- **`stream.range`:** the documented examples are unchanged; only undocumented edge behavior
  tightens. The two existing stream tests are updated (§4).
- **`step` as identifier:** because `step` is a *contextual* keyword, existing code using
  `step` as a variable name is unaffected.

---

## 9. Testing strategy

- **Unit (both engines):** the §3.5 truth table as assertions; mismatch/zero/non-finite
  panics; value-position materialization; inclusive boundaries; float-step iteration;
  strided pattern membership including the anchor-parity cases.
- **Differential:** add range/step programs to the VM↔tree-walker byte-identical gate; build
  → run-`.aso` parity.
- **`stream.range`:** updated + new cases (inferred direction, mismatch panic).
- **Checker:** fixture programs that trip `range-step` (incl. float-step-in-pattern advisory),
  `invalid-propagate`, and `unresolved-import`; severity-config overrides; LSP diagnostics.
- **Conformance:** new `examples/*.as` exercising `..=`, `step`, and descending ranges, kept
  runnable and accepted by both the grammar and both parsers.

---

## 10. Non-goals / deferrals

- No decimal/rational iteration; float-step rounding is documented, not fixed.
- No static-reject phase; validation stays runtime-panic + advisory lint.
- The broader analyzer menu beyond the three rules here (flow-typed field/arity/enum checks,
  class-shape checks) is a **separate** future milestone, intentionally not bundled.
- `...` (`DotDotDot`) remains spread/rest and is untouched.

---

## 11. Decisions log (rationale captured from the design dialogue)

- **Sequence over interval for `..`.** Chosen for ergonomic count-down and to fix the
  silent-descending-no-op. Cost: a behavior change to shipped, parity-gated code. Accepted.
- **Migrate `stream.range` rather than diverge.** A sequence syntax that disagreed with an
  interval `stream.range` would put two range models in one language. Migrating the stdlib
  keeps a single model; the documented contract is preserved, only edge behavior tightens.
- **Signed step, honored; default sign inferred only when step is omitted.** The single
  place inference happens is an omitted step. An explicit step states intent; a sign that
  fights the bounds is a mismatch error, not a silent empty.
- **`step` in match patterns: included.** Consistency (ranges behave the same in every
  position) and opt-in expressiveness. The float-membership hazard is handled by an advisory
  lint rather than a restriction, honoring "available but optional."
- **Validation = Tier-2 panic + advisory lint, no static-reject.** Matches the project's
  established `contract-mismatch` shape and preserves engine parity. (Earlier framing that
  conflated Tier-1/Tier-2 with static/dynamic was a terminology error; the tiers are the
  error *mechanism*, not its timing.)
- **Analyzer is extended, not built.** The resolver and `check` linter already exist with
  much of the original menu; this feature adds three rules into the existing framework.
