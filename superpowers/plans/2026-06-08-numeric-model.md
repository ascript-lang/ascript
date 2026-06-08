# NUM — Numeric Model & Integers — Implementation Plan

> REQUIRED SUB-SKILL: superpowers:subagent-driven-development (fresh implementer + independent reviewer
> per task; reviewer runs the commands and probes edges). Steps use `- [ ]`. This is the FOUNDATION
> spec — it merges first and is the format template for every other campaign plan.

**Spec:** `superpowers/specs/2026-06-08-numeric-model-design.md`. **Branch:** `feat/numeric-model` off
`main`. **Depends on:** P0 (`.aso` clamp) landed. **Breaking** (literal identity, division, printing,
truthiness) — the corpus is *migrated*, not deleted (Gate 7).

**Architecture:** add `Value::Int(i64)`, rename `Value::Number(f64)→Value::Float(f64)` (791 sites,
compiler-enforced), add `MapKey::Int`. Literals lex to `Tok::Int(i64)`/`Tok::Float(f64)`. Arithmetic is
type-directed (§3.2 table); integer `+ - * ** <<` and unary `-` are checked (trap → recoverable Tier-2
panic); `+% -% *%` wrap. Bitwise `& | ^ << >> ~` are int-only at **Go precedence** (`<< >> &`
multiplicative; `| ^` additive), with bitwise-`|` on a dedicated tier the pattern/type entry points
**bypass** (as `coalesce` does today). Comparison is exact across `{int,float}`; `MapKey` folds integral
floats to `Int` (NaN carved out). Truthiness: `nil/false/0/0.0/-0.0/NaN/0m/""` falsy; collections truthy.
Code points are `int` (`string.codepoints/from_codepoints/code_at`). Both engines byte-identical.

**Tech stack:** Rust; the two front-ends; tree-sitter (`--abi 14`); `src/vm/{opcode,run,adapt,aso}.rs`;
`src/check/infer`; `src/worker/serialize.rs`.

---

## Shared API Contract (pinned to current code)
**Existing (verified):** `Value::Number(f64)` `value.rs:626`; `MapKey::Num(u64)` `value.rs:191`;
`is_truthy` `value.rs:687` (today only `Nil`/`Bool(false)` falsy — `0.0`/`""` TRUTHY, test `:923`);
`type_name` `value.rs:483` + `interp.rs:5392`; `Tok::Number(f64)` `token.rs:7`; `parse_number_text`
`lex_literals.rs:85`; `BinOp` `ast.rs:505`, `UnOp` `ast.rs:526`; `ExprKind::Number` (fmt `fmt.rs:461`,
`format_number` `:808`); `apply_binop` InstanceOf arm `interp.rs:5100` (panics on non-class RHS);
`check_type` free fn `interp.rs:5704` (no env); `ArithKind` `adapt.rs:49`; `ASO_FORMAT_VERSION` `aso.rs:105`.
**New names (do not rename):** `Value::Int(i64)`, `Value::Float(f64)`, `MapKey::Int(i64)`;
`Tok::Int(i64)`, `Tok::Float(f64)`, `Tok::{Amp,Caret,Tilde,Shl,Shr,PlusPercent,MinusPercent,StarPercent}`;
`BinOp::{BitAnd,BitOr,BitXor,Shl,Shr,WrapAdd,WrapSub,WrapMul}`, `UnOp::BitNot`; `Type::{Int,Float}`,
`CheckTy::{Int,Float}`; `ArithKind::Int`; VM `Op::{BitAnd,BitOr,BitXor,Shl,Shr,BitNot,WrapAdd,WrapSub,WrapMul}`.

## Conventions (every task)
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- `cargo test` AND `cargo test --no-default-features` green; clippy clean BOTH configs.
- Both engines byte-identical (`vm_differential.rs`, both feature configs) — fix the engine, never the
  assertion. Tree-sitter: after `grammar.js`, `tree-sitter generate --abi 14` then `cargo build`.

---

## Task 1 — Value layer: `Int`, `Number→Float` rename, `MapKey::Int`, truthiness
**Files:** `src/value.rs` (+ every exhaustive `match Value`, compiler-flushed). **Tests:** `value.rs`.
- [ ] Failing tests: `Value::Int(5).type_name()=="int"`; `Float(5.0).type_name()=="float"`;
  truthiness table (`Int(0)`/`Float(0.0)`/`Float(NaN)`/`Str("")` falsy; `Int(1)`/`[]` truthy);
  `MapKey::from(Float(1.0))==MapKey::from(Int(1))` (integral fold) but `Float(1.5)` distinct and two
  `NaN` keys handled per §3.3.
- [ ] Add `Value::Int(i64)`; rename `Number→Float` (mechanical, follow compiler errors across the
  tree). Add `MapKey::Int(i64)` + integral-fold in `MapKey::from`. Rewrite `is_truthy` to the resolved
  falsy set. Arms in `PartialEq`/`Eq`/`Hash`/`Debug`/`Display`/`type_name`; GC `trace` scalar no-op.
- [ ] Green both configs; clippy. Independent review (greps for stray `Value::Number`; confirms NaN
  carve-out). Commit.

## Task 2 — Lexer: `Tok::Int`/`Tok::Float`, octal, operator tokens
**Files:** `src/token.rs`, `src/lexer.rs`, `src/lex_literals.rs`. **Tests:** `lexer.rs`.
- [ ] Failing tests: `5`→`Int(5)`, `5.0`/`1e3`→`Float`, `0o17`→`Int(15)`, `1_000`→`Int(1000)`,
  out-of-range int → lex error; `& ^ ~ << >> +% -% *%` lex to the new tokens; `'x'` still a string;
  `a && b`/`a || b` unaffected.
- [ ] Replace `Tok::Number` with `Tok::Int(i64)`/`Tok::Float(f64)`; `parse_number_text` returns the
  subtype + octal + i64 range-check; add the operator tokens (lone `&` was an error before — now `Amp`).
- [ ] Green; review; commit.

## Task 3 — AST: BinOp/UnOp/Type additions + Display
**Files:** `src/ast.rs` (+ `fmt.rs`/`interp.rs`/`ast.rs Display` exhaustive arms). **Tests:** `ast.rs`.
- [ ] Add `BinOp::{BitAnd,BitOr,BitXor,Shl,Shr,WrapAdd,WrapSub,WrapMul}`, `UnOp::BitNot`; literal expr
  carries int-vs-float; `Type::{Int,Float}` + `Display` (`int`/`float`/`number`). Compiler flushes the
  missing arms in `interp.rs`/`fmt.rs`.
- [ ] Green; review; commit.

## Task 4 — Legacy parser: precedence + `>>` split + wrapping/bitwise
**Files:** `src/parser.rs`. **Tests:** `parser.rs`.
- [ ] Failing tests: `a & b == c` ⇒ `(a&b)==c` (Go precedence); `1 << 2 | 3`; `match x { 1|2 => …}`
  stays an or-pattern AND `a | b` is bitwise-or in value position; `let t: int | float`; `a >> b` shift
  vs `map<int, array<int>>` / `future<array<int>>` type (the `Shr` split).
- [ ] Add the bitwise tiers (Go: `<< >> &` multiplicative; `| ^` additive; `+% -%` additive, `*%`
  multiplicative) as a dedicated `bitor` level; **re-point the pattern entry (`parse_pattern`) and the
  type-union parser to BYPASS bitwise-`|`** (mirror how `coalesce` is the pattern parser's entry today).
  Split a trailing `Shr`/`Ge` in type-arg position into closing `>`s.
- [ ] Green; review; commit.

## Task 5 — CST parser + frontend conformance
**Files:** `src/syntax/parser.rs`, `src/syntax/kind.rs`. **Tests:** `tests/frontend_conformance.rs`.
- [ ] Mirror Task 4 in the CST Pratt parser (infix table reachable only from `expr()`, not pattern/type
  entry); the `Shr`-split in `type_ann`. Frontend conformance proves both parsers agree on the new forms.
- [ ] Green; review; commit.

## Task 6 — Tree-sitter grammar + publish
**Files:** `tree-sitter-ascript/grammar.js`, `queries/highlights.scm`, editors. **Tests:** `tests/treesitter_conformance.rs`.
- [ ] Int/float/octal literal rules; the operators at Go precedence (declare any GLR conflicts); `>>`
  in type position; regen `parser.c --abi 14`; highlight the new literals/operators. Run
  `./scripts/sync-grammar.sh`, bump `editors/zed/extension.toml` `commit` + `editors/nvim/.../treesitter.lua`
  `revision`; update VS Code TextMate + Zed/Neovim `highlights.scm`.
- [ ] Conformance green; review; commit.

## Task 7 — Tree-walker arithmetic, comparison, instanceof
**Files:** `src/interp.rs`. **Tests:** `interp.rs`.
- [ ] Failing tests: one per §3.2 cell incl. panics (`1/0`, `1%0`, overflow on `+ - * **`/unary `-`,
  shift-amount `1<<64`); wrapping ops don't panic; `<<` bit-loss does NOT trap (`1<<63==i64::MIN`);
  `**` int/float branches (`2**-1`→float, `2**64`→float, overflow panic); exact compare
  (`1==1.0`, `(2**53+1)==float(2**53+1)` is false); `5 instanceof int`, `5.0 instanceof float`,
  `5 instanceof number`.
- [ ] Implement the type-directed table (checked via `checked_add/mul/pow/shl`, wrapping via
  `wrapping_*`, truncating `/`, exact cross-subtype compare); add the reserved-type-name RHS to the
  shared `apply_binop` InstanceOf arm (covers both engines).
- [ ] Green; review; commit.

## Task 8 — VM opcodes + adaptive `ArithKind::Int`
**Files:** `src/vm/opcode.rs`, `src/vm/run.rs`, `src/vm/adapt.rs`, `src/vm/disasm.rs`. **Tests:** `vm_differential.rs`.
- [ ] New opcodes (`BitAnd…WrapMul`) + checked-int arithmetic in `run.rs`; `ArithKind::Int` adaptive
  path (warm two-`Int` sites to inline i64 + the checked branch; deopt on mixed/Float). Specialized and
  generic VM MUST be byte-identical (incl. which inputs panic). Disasm for the new ops.
- [ ] Three-way differential green; review; commit.

## Task 9 — `.aso` Int constant + opcodes + version bump
**Files:** `src/vm/aso.rs`, `src/vm/verify.rs`. **Tests:** `aso.rs`.
- [ ] Serialize an `Int` constant kind + the new opcodes; **read `ASO_FORMAT_VERSION` and bump by one**
  (do not hardcode 19); update `verify.rs`. Reuse the P0 clamp pattern for the new variable-length read.
  Round-trip test.
- [ ] Green; review; commit.

## Task 10 — Worker airlock: `Int` wire tag
**Files:** `src/worker/serialize.rs`. **Tests:** `serialize.rs`.
- [ ] New tag for `Int` (the `Float` tag = former `Number` tag); `encode∘decode` round-trip incl. `Int`
  as a Map key (integral-fold consistency across the boundary).
- [ ] Green; review; commit.

## Task 11 — Type contracts + checker + narrowing
**Files:** `src/interp.rs` (`check_type`), `src/check/infer/{ty,pass,env,table}.rs`. **Tests:** `tests/check.rs`.
- [ ] `Type::{Int,Float}` in `check_type`; `number` = accept `Int|Float`; `: int` rejects a float &
  vice-versa. `CheckTy::{Int,Float}`; `synth` per §3.2 (`Int+Int:Int`, `Int+Float:Float`, `Int/Int:Int`);
  bitwise/shift on a provable Float → `type-error`; `instanceof int|float|number` narrowing in `pass.rs`.
- [ ] **Gate 5:** `examples/**` emits ZERO `type-*` in BOTH configs (CI tripwire).
- [ ] Green; review; commit.

## Task 12 — Printing, JSON, stdlib
**Files:** `src/value.rs`/`src/fmt.rs` (Display), `src/stdlib/{json,math,convert,array,string,bytes}.rs`,
range. **Tests:** the module tests.
- [ ] `int`→`5`, `float`→`5.0` (always a decimal); JSON subtype round-trip (`.`/`e` → float else int);
  `math` typed returns + `floordiv/divmod/ceildiv` + bit helpers; `int()`/`float()` conversions;
  `array[i]` requires int index (float → Tier-2 panic); `string.codepoints/from_codepoints/code_at`;
  ranges over int bounds produce `int`.
- [ ] Green; review; commit.

## Task 13 — Formatter
**Files:** `src/fmt.rs`. **Tests:** fmt idempotence goldens.
- [ ] Render int vs float literals canonically (`5.0` round-trips); render the new operators; idempotent.
- [ ] Green; review; commit.

## Task 14 — LSP
**Files:** `src/lsp/providers/*`. **Tests:** `tests/lsp.rs`.
- [ ] Semantic tokens for the new operators/literals; hover shows `int`/`float`/`number`; completion
  offers the type names; the new `type-error` diagnostics flow.
- [ ] Green; review; commit.

## Task 15 — Property tests, REPL, four-mode differential
**Files:** `src/value.rs`/`tests/*`, `src/repl.rs`. **Tests:** property + REPL + differential.
- [ ] Property tests: exact-compare agrees with math equality across the 2^53 boundary; Map-key
  consistency `(a==b)⟺same key` over `{int,float}` with NaN carved out; overflow table. REPL regression
  (`5/2`→`2`, `5.0/2`→`2.5`, `1<<3`→`8`, `map<int,array<int>>` multiline). Wire worker/numeric examples
  into the four-mode differential.
- [ ] Green; review; commit.

## Task 16 — Example corpus + Gate-7 migration
**Files:** `examples/integers.as`, `examples/numeric_tower.as`, `examples/advanced/bit_codec.as`;
ALL existing `examples/**` + goldens. **Tests:** conformance + differential + fmt idempotence.
- [ ] New examples (happy + edge: overflow/div0/precision-boundary/wrapping/packing). Migrate every
  existing example + golden to the new semantics (float `5.0` printing, int division `10/3→3`, int
  indices, truthiness). Corpus is migrated, never trimmed.
- [ ] All four modes byte-identical; review; commit.

## Task 17 — Docs
**Files:** `docs/content/language/values-types.md`, `docs/content/stdlib/*.md`, `README.md`, the design
spec, `CLAUDE.md`, `roadmap.md`.
- [ ] "Numbers" section (tower, type-directed division, checked overflow, bitwise, code-points-as-int,
  truthiness); update stdlib pages (math/convert/json/string); README types table; CLAUDE.md "Values"
  paragraph; design-spec numeric section; roadmap entry. NAV unchanged (append to existing pages).
- [ ] Review; commit.

## Done when
Every task checked behind an independent review; four-mode byte-identity holds in both configs; Gate-5
zero `type-*` on `examples/**`; the corpus is migrated; clippy + tests green both configs; the
`bench/` Gate-12 check for the `ArithKind::Int` path shows no regression. Merge `--no-ff` to `main`
(NUM merges first — everyone else rebases onto `Int`/`Float`).
