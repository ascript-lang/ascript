# AScript Numeric Model & Integers — Design (NUM)

- **Status:** Draft for review
- **Date:** 2026-06-08
- **Code:** NUM (foundation of the Serious Language campaign — see `goal.md`)
- **Depends on:** nothing (this is the keystone)
- **Depended on by:** VAL (compact value), ADT (variant payload types), TYPE (int/float types),
  FFI (pointer-width ints), the JIT (integer fast paths), and any self-hosted stdlib (hashing/codecs)
- **Engines:** both (tree-walker oracle == VM, byte-identical)
- **Breaking:** **yes, deliberately** — four observable changes: (1) **literal identity** (`5` is now an
  `int`, not an `f64`); (2) **division semantics** (`int / int` truncates: `7/2==3`); (3) **number
  printing** (`float` always shows a decimal: `5.0`, not `5`); and (4) **truthiness** (`0`/`0.0`/`0m`/`""`/
  `NaN` are now falsy — the campaign-wide rule resolved in `REVIEW-FINDINGS-2026-06-08.md` §"Owner
  decisions", owned by NUM because it edits `value.rs` `is_truthy`; see §3.3). Backward compatibility is not
  a goal pre-1.0 (`goal.md`). The whole example/golden corpus is *migrated*, not deleted (Gate 7).

---

## 1. Summary & motivation

AScript today has exactly one number: `Value::Number(f64)` (`src/value.rs:626`). Hex/binary literals
already lex but are immediately cast to `f64` (`src/lex_literals.rs:97`); there are **no integers, no
bitwise operators, and no exact integer arithmetic**. This is the single biggest blocker to AScript
becoming a serious general-purpose language:

- **You cannot self-host.** A bytecode emitter, a UTF-8 decoder, a hash function, or a serializer all
  require exact integer + bitwise math. `f64` cannot represent a `u16` constant-pool index or compute
  `xxhash` without precision loss.
- **You cannot do correct systems-flavored work** — array indices, bit flags, packing/unpacking,
  protocol parsing — in a type whose every value is a float.
- **Performance is capped.** Boxing every number as `f64` and routing all arithmetic through the
  float path forecloses the integer fast paths the VM (and a future JIT) need.

This spec introduces a **real numeric model**: `int` (i64) as the default for integer literals,
`float` (f64) as the default for fractional literals, the existing exact `Decimal`, and a reserved
slot for a future `bigint`. Division becomes **type-directed** (the C/Go/Rust/Java/Swift model), so
there is no `//` operator and therefore no collision with `//` line comments. Integer overflow is
**checked by default** with explicit wrapping operators as the escape hatch. Unicode code points are
**`int`s** (the Go "rune" model), so no separate `char` kind is introduced.

### Two conflicts this spec resolves up front

1. **No `char` literal / no `char` type.** Single-quote `'...'` is already a string delimiter
   (`src/lexer.rs:396`; test `lexes_single_quoted_string`). Rather than steal it or invent a sigil, a
   Unicode scalar is just an `int` (Go's rune model). String↔code-point conversion is provided by
   `string.codepoints()` / `string.from_codepoints()` / `string.code_at(i)`. **Zero new `Value` kind,
   zero new literal, zero conflict** — a strict reduction in blast radius.
2. **`>>` vs nested generics.** `future<array<int>>` ends in `>>`. The lexer emits a `Shr` token; the
   **type-argument parser splits a trailing `Shr`/`Ge` into closing `>`s** (the Rust/Java/C#
   technique). Locked in §3.4; a required test exercises `map<int, array<int>>`.

## 2. The model: one user concept, distinct runtime subtypes

There is one *user-facing* idea — "a number" — realized as **distinct runtime kinds and distinct
type names**, because type-directed division and clear diagnostics need them distinguishable:

| Runtime kind | `type_name` | Representation | Literal form | Status |
|---|---|---|---|---|
| `Value::Int(i64)` | `"int"` | 64-bit signed two's-complement | `5`, `0xFF`, `0b1010`, `0o17`, `1_000` | **new** |
| `Value::Float(f64)` | `"float"` | IEEE-754 double | `5.0`, `1.5`, `1e3`, `.5` | renamed from `Number` |
| `Value::Decimal(d)` | `"decimal"` | exact base-10 (`rust_decimal`) | `decimal("0.1")` (constructor; unchanged) | exists |
| *bigint* | `"bigint"` | arbitrary precision | *(reserved — not in NUM)* | future |

- **`number` is the union `int | float`** — a built-in type annotation that accepts either subtype.
  No value's `type_name` is `"number"`; it is purely an annotation supertype (so existing `: number`
  annotations keep meaning "any non-exact number"). `Decimal` is **not** part of `number` (it is exact
  and opt-in, exactly as today).
- **Code points are `int`s.** No `char` type. `"A".code_at(0) == 65`; `string.from_codepoints([72,
  105]) == "Hi"`.

`Decimal` and the future `bigint` complete the tower but are **out of scope** for NUM beyond reserving
their type names and ensuring promotion rules have a defined (deferred) slot.

## 3. Surface syntax & semantics

### 3.1 Literals

- **Integer literal:** a digit sequence with **no `.` and no exponent** → `int`. Bases: decimal,
  `0x`/`0X` (hex), `0b`/`0B` (binary), **`0o`/`0O` (octal, new)**. Underscores allowed (`1_000_000`).
  `0xFF` → `Int(255)`, `0b1010` → `Int(10)`, `0o17` → `Int(15)`.
- **Float literal:** contains a `.` **or** an exponent → `float`. `5.0`, `1.5`, `.5`, `1e3`
  (= `Float(1000.0)`, exponent ⇒ float even when integral), `1.5e-3`. Hex/binary/octal floats are not
  supported (they are bit patterns ⇒ always `int`).
- An integer literal that **overflows i64** is a **lex/parse-time error** (`integer literal out of
  range for int (i64)`), not a silent wrap or float fallback. (When `bigint` lands, an explicit
  `bigint("…")` constructor covers larger exact values.)

### 3.2 Operators & result types (type-directed)

Arithmetic result type is a function of operand types — the C/Go/Rust/Java/Swift convention:

| Op | `int ⊕ int` | mixed `int ⊕ float` / `float ⊕ float` |
|---|---|---|
| `+ - *` | `int` (checked overflow → panic) | `float` |
| `/` | **`int`, truncated toward zero** (`7/2==3`, `-7/2==-3`); `int / 0` → **Tier-2 panic** | `float` (`7.0/2==3.5`; `1.0/0.0==inf`, unchanged IEEE) |
| `%` | `int` remainder, sign follows dividend (`-7%2==-1`); `% 0` → **panic** | `float` `fmod` |
| `**` | exponent ≥ 0 and ≤ `u32::MAX` → `int` via `i64::checked_pow` (overflow → panic); exponent < 0 → `float`; exponent > `u32::MAX` → `float` (see below) | `float` |
| `+% -% *%` | `int`, **two's-complement wrapping** (no panic) | **type error** (wrapping is int-only) |
| `& \| ^` | `int` bitwise | **type error** (bitwise is int-only) |
| `<< >>` | `int` shift (`>>` arithmetic/sign-extending); shift amount `< 0` or `≥ 64` → panic (`checked_shl`/`checked_shr`) | **type error** |
| unary `~` | `int` bitwise NOT | **type error** |
| unary `-` | `int` (checked: `-i64::MIN` panics) | `float` |

**Mixing rule (promotion):** in any mixed `int`/`float` arithmetic, the `int` operand is promoted to
`float` and the result is `float`. There is no implicit `float → int`. Promotion is **only** for
`+ - * / % **` and ordering comparisons — never for bitwise/shift/wrapping (those reject a `float`
operand with a Tier-2 panic: `bitwise op requires int operands, got float`).

**Why type-directed division (no `//`):** with backward-compat gone, `/` need not always produce a
float, so a second floor-division operator is unnecessary — which removes the `//`-vs-line-comment
collision entirely. Truncation is **toward zero** (matches the i64 hardware `div` instruction and the
entire C family; one VM instruction, no correction code). For flooring/ceiling/euclidean division and
combined quotient-remainder, std provides `math.floordiv(a,b)`, `math.divmod(a,b) -> [q, r]`,
`math.ceildiv(a,b)` (int→int).

**Why checked overflow (Swift/Zig model):** silent wraparound is a classic bug farm (pillar: no bugs).
`+ - * **` and `-`(unary) **trap** on i64 overflow with a recoverable Tier-2 panic
(`integer overflow in '<op>'`). The explicit `+% -% *%` wrapping operators serve the hashing/codec/
self-hosting cases that *want* modular arithmetic.

**Precise shift rule (`<<` / `>>`).** The shift uses Rust `i64::checked_shl(amount as u32)` /
`checked_shr` semantics: the **only** condition that traps is an **out-of-range shift amount** — the
amount is taken as the rhs `int`, and a value `< 0` or `≥ 64` is a Tier-2 panic (`shift amount out of
range: <n>`). **Bit-loss does NOT trap:** `1 << 63 == i64::MIN` (`-9223372036854775808`, the top bit
shifted into the sign position) and `-1 << 1 == -2` are *defined results*, matching the hardware shift —
we do **not** treat shifting bits past the top as overflow. (`checked_shl` only ever returns `None` for an
out-of-range amount, never for lost bits.) Boundary tests (§9.1): `1<<63` → `i64::MIN`, `1<<64` → panic
(amount ≥ 64), `1 << -1` → panic, `-1<<1` → `-2`, `1>>0 == 1`, `-8 >> 1 == -4` (arithmetic
sign-extension).

**Precise `**` (pow) rule.** `int ** int` routes through `i64::checked_pow`, whose exponent parameter is a
`u32`. Therefore: a **negative** exponent is always `float` (`2 ** -1 == 0.5`, computed as
`float(base).powi(exp)`); a **non-negative** exponent that **exceeds `u32::MAX`** also falls to the
`float` path (`base` would have to be 0/1/-1 to not overflow anyway, but we never truncate the exponent —
we promote to `f64::powf` so the result is defined, not a wrong int); a non-negative exponent ≤ `u32::MAX`
uses `checked_pow` and **traps on i64 overflow**. Examples (§9.1): `0**0 == 1` (`checked_pow(1,0)`),
`(-2)**3 == -8`, `2**-1 == 0.5` (float), `2**63` → panic (overflow), `2**4 == 16`.

### 3.3 Comparison & equality (exact, cross-subtype)

- `==` / `!=` compare **mathematical value, exactly** across subtypes: `1 == 1.0` → `true`,
  `2 < 2.5` → `true`. A large `int` not exactly representable as `f64` compares **exactly** (no lossy
  promotion) — e.g. `(2**53 + 1) == float(2**53 + 1)` is `false`. Implementation uses the standard
  exact i64-vs-f64 comparison (compare as integers when the float is integral and in range; otherwise
  by magnitude/sign), so there are **no precision bugs** at the boundary (a property test enforces it,
  §9).
- Ordering (`< <= > >=`) is likewise exact across subtypes.
- **Map-key consistency (correctness invariant), scoped to `{int, float}`:** for all `a`, `b` that are
  **`int` or `float`** *and* both finite (non-NaN), `a == b` ⟺ `a` and `b` are the **same map key**.
  Therefore an integral, in-range `float` key folds to the same `MapKey` as the equal `int`: `MapKey`
  gains `Int(i64)`, and `MapKey::from` maps an integral, in-range `Float` to `MapKey::Int` (non-integral
  or out-of-i64-range floats keep the canonical-bits `Num` key — they have no equal `int` so there is
  nothing to fold). **Explicit carve-out — NaN:** `NaN != NaN`, so the universal "`a==b` ⟺ same key" claim
  is *false* for NaN and is deliberately excluded; AScript's existing `MapKey` already canonicalizes NaN
  to a single bit pattern (so `map[NaN]` is storable/retrievable as one key) even though `NaN == NaN` is
  `false` — that pre-existing behavior is unchanged, and the property test below quantifies only over
  **finite** numerics. **Decimal stays distinct:** a `Decimal` key is never folded into an `Int`/`Float`
  key (Decimal is exact and opt-in; `1 == decimal("1")` value-equality in the evaluator does **not** imply
  shared map identity — they remain separate `MapKey` variants, matching today's `int`/`float`-vs-`Decimal`
  key separation). The property test (§9.1) is therefore stated as:
  `∀ finite numeric a,b ∈ {int,float}: (a==b) ⟺ (MapKey::from(a)==MapKey::from(b))`.
- **Truthiness (BREAKING — campaign-wide rule, owned by NUM).** This rewrites the historical
  "only `nil`/`false` are falsy" rule (verified current behavior: `src/value.rs:687` `is_truthy` returns
  `!matches!(self, Value::Nil | Value::Bool(false))`, and the test at `src/value.rs:923`
  `truthiness_follows_spec` asserts `Value::Number(0.0).is_truthy()` and `Value::Str("".into()).is_truthy()`
  are **true** today). The **new falsy set** is:
  `nil`, `false`, `0` (`Int(0)`), `0.0`/`-0.0` (`Float`), `NaN` (`Float`), `0m` (zero `Decimal`), and
  `""` (empty `Str`). **Collections (`Array`/`Map`/`Set`), `Object`, and `Instance` stay TRUTHY even when
  empty** — emptiness is queried explicitly (`len(x)` / `x.isEmpty()`), avoiding the "valid-but-empty
  collection reads as no-result" footgun. So `if (count)` means "non-zero", `if (name)` means
  "non-empty string", but `if (items)` is `true` for an empty array. NUM edits `value.rs` `is_truthy`
  (replacing the `!matches!(…)` body with the explicit falsy match incl. an `Int(0)` arm, a
  `Float` arm that is falsy on `0.0`/`-0.0`/`NaN`, a `Decimal` arm falsy on `Decimal::ZERO`, and a `Str`
  arm falsy on `""`) and rewrites the `truthiness_follows_spec` unit test (now asserting `Int(0)`/
  `Float(0.0)`/`Float(f64::NAN)`/`Str("")` are **falsy**, and empty array/object/`Float(1.0)` are
  **truthy**). The unit test runs under both feature configs. The doc-comment on `is_truthy` is updated to
  describe the new falsy set.

### 3.4 Lexer/parser interactions (the `>>` and `|` cases)

- **New tokens:** `Tok::Amp` (`&`), `Tok::Caret` (`^`), `Tok::Tilde` (`~`), `Tok::Shl` (`<<`),
  `Tok::Shr` (`>>`), and the wrapping `Tok::PlusPercent`/`MinusPercent`/`StarPercent` (`+%`/`-%`/`*%`).
  `Tok::Pipe` already exists (type unions / or-patterns) and is **reused** for bitwise-or in
  expression position. `^`, `~`, `&` are confirmed-free today.
- **`Tok::Number(f64)` is replaced** by a literal token that carries the subtype: either two variants
  (`Tok::Int(i64)` / `Tok::Float(f64)`) or `Tok::Num{ value, is_int }`. Decision: **two variants**
  (`Tok::Int(i64)`, `Tok::Float(f64)`) — the cleanest for the parser and exhaustive matches.
- **`>>` vs nested generics:** the lexer always emits `Shr` for `>>`. The **type-argument parser**
  (both front-ends), when it expects a closing `>` and sees `Shr` (or `Ge`), **consumes one `>` and
  pushes back the remainder** — the standard Rust/Java/C# split. Required test:
  `let x: map<int, array<int>> = ...` and `future<array<int>>` parse; `a >> b` in expression position
  shifts. The legacy oracle parser and the CST parser must agree (frontend conformance).
- **Bitwise precedence — Go's model, not C's** (avoids the `a & b == c` footgun). The **full
  re-threaded expression chain** (loosest → tightest) is:

  `??` < `||` < `&&` < equality (`== !=`) < comparison (`< <= > >= instanceof`) < **bitor-tier
  (`| ^`)** < range (`.. ..=`) < additive (`+ -` and `+% -%`) < multiplicative-tier (`* / %` and `*%`
  and **`<< >>` and `&`**) < `**` (right-assoc) < unwrap (`? !`) < unary (`- ! ~ await`) < postfix/call.

  This is **Go's binding** — shifts and `&` at the multiplicative tier; `| ^` one tier looser (tighter than
  comparison/`range`, looser than `+ -`). Go's table has `| ^` at the *additive* level and `* / % << >> &`
  at the multiplicative level; we keep `+ -` strictly tighter than `| ^` (so `a | b + c` is `a | (b + c)`)
  and `| ^` strictly tighter than `==`/`<` (so the **`a & b == c` and `a | b == c` footguns parse the
  intuitive way**: `(a & b) == c` and `(a | b) == c`). `~` is prefix-unary.

  - **[CRITICAL] `|` must NOT collide with or-patterns / union types.** Today `|` (`Tok::Pipe`) is
    **invisible to the expression precedence chain** — it is consumed *only* by (a) the match-arm
    or-pattern loop and (b) the type-union loop. Verified:
    - **Legacy parser (`src/parser.rs`).** The pattern value-entry is `parse_pattern` → `coalesce()`
      (`parser.rs:1416`), and `coalesce` (`:1234`) descends `logic_or → logic_and → equality →
      comparison → range → additive → multiplicative → exponent → unwrap_tier → unary` and **never
      matches `Tok::Pipe`**; the or-pattern `|` is eaten by a dedicated loop in the `Tok::Match` arm
      (`parser.rs:1823`: `while *self.peek() == Tok::Pipe { … patterns.push(self.parse_pattern()?) }`).
      The type-union `|` is eaten by `parse_type` (`parser.rs:509`: `while *self.peek() == Tok::Pipe`).
    - **CST parser (`src/syntax/parser.rs`).** The pattern value-entry is `pattern()` → `lhs_for_pat`
      (`:1675`) → `primary_no_arrow`/`unwrap_tier`, which **never consumes `Pipe`**; the or-pattern `|` is
      eaten by `match_arm` (`:1604–1612`, wrapping alternatives in `OrPat`), and the type-union `|` by
      `type_ann` (`:1220–1228`, building `UnionType`). The infix binding-power table
      (`infix_binding_power`, `:696`) has **no `Pipe` entry today**.

    **Therefore the design is:** bitwise-or gets its **own dedicated `bitor()` tier** — but the
    pattern value-entry deliberately **bypasses it**, exactly as the pattern entry bypasses `|` today.
    Concretely:
    - Legacy: insert a new `bitor()` method **between `comparison()` and `range()`** (so `comparison`
      calls `bitor`, and `bitor` calls `range`); `bitor` loops on `Tok::Pipe` (BitOr) and `Tok::Caret`
      (BitXor). **`parse_pattern` is re-pointed from `coalesce()` to call `range()` directly** (the tier
      *below* `bitor`) — so a bare `|` between patterns is NEVER swallowed by the value-parser and stays
      owned by the arm loop. (`coalesce`'s extra layers above `bitor` — `??`/`||`/`&&`/`==`/`<` — are not
      valid leading forms inside a single match pattern anyway, so dropping to `range()` for patterns
      loses nothing; the existing `Range`/`Ident`/`Value` classification on the returned `Expr` is
      unchanged.) Value position keeps the full chain (`coalesce → … → comparison → bitor → range → …`),
      so `a | b` shifts. **Also: add `&` and `<< >>` and `*%` into `multiplicative()`, and `+% -%` into
      `additive()`** per the chain above.
    - CST: add `Pipe`/`Caret` to `infix_binding_power` at a power **between comparison (`Lt..=Ge`, 9/10)
      and add/sub (`Plus`/`Minus`, 11/12)** — e.g. `Pipe | Caret => (10, 11)`; the shift/`&` ops go at
      the multiplicative power band (just below `* / %`'s 13/14, e.g. `Shl | Shr | Amp => (13, 14)` — they
      and `Star..=Percent` are left-assoc peers). The Pratt `expr_bp` (value position) is reached only
      from `expr()`; `pattern()`/`lhs_for_pat()` build a pattern via `primary_no_arrow`/`unwrap_tier`
      **without** entering `expr_bp`, and the or-pattern `|` is consumed by the `match_arm` loop *before*
      any `expr_bp` call — so or-patterns and union types are unaffected by the new `Pipe` binding power.

    **Required conformance test (both front-ends agree — `tests/frontend_conformance.rs`):**
    (1) `match x { 1 | 2 => "a", _ => "b" }` parses as a single arm with a two-alternative pattern
    (legacy `MatchArm.patterns.len() == 2`; CST `OrPat` with two `LiteralPat` children) — **not** a
    bitwise `1 | 2` value; (2) a value-position `let m = a | b` parses as **one** bitwise-or expression
    (`BinOp::BitOr`, CST a single infix node) — **not** a pattern; (3) `a | b == c` parses as
    `(a | b) == c` and `a & b == c` as `(a & b) == c`; (4) a type-position `let t: int | float = …`
    parses as a `UnionType`. Both engines must agree on all four (frontend conformance), and the
    tree-walker == VM differential covers the runtime result of the value cases.

  - **Lexer collision check (verified `src/lexer.rs`):** single `&` is *today an error*
    (`lexer.rs:216–229`: a lone `&` returns `"unexpected character '&'"`), so introducing `Tok::Amp` is a
    pure addition — `&` vs `&&` (`Tok::AmpAmp`, `:218`) disambiguate by the existing two-char lookahead.
    Single `|` already lexes to `Tok::Pipe` (`lexer.rs:236–244`) and `||` to `Tok::PipePipe` — unchanged.
    `??` is `Tok::QuestionQuestion` (distinct char, no overlap with `|`). `^` and `~` are confirmed-free
    today (no lexer arm). The lexer adds `Amp Caret Tilde Shl Shr PlusPercent MinusPercent StarPercent`
    with the same maximal-munch lookahead pattern.

  This is a deliberate correctness/DX choice; the legacy precedence-climbing parser and the CST Pratt
  parser both encode it, and the tree-sitter grammar mirrors it (with any GLR conflicts declared).
  Wrapping ops `+% -% *%` bind exactly like their non-wrapping counterparts (`+% -%` additive, `*%`
  multiplicative).

### 3.5 Examples

```javascript
let mask = 0xFF                  // int
let flags = mask & 0b1010        // 10  (int bitwise AND)
let packed = (r << 16) | (g << 8) | b   // int packing
let half = 7 / 2                 // 3    (int / int → int, truncating)
let exact = 7.0 / 2              // 3.5  (float involved → float)
let n = -7 / 2                   // -3   (truncate toward zero)
print(5)                         // "5"
print(5.0)                       // "5.0"   (floats always show a decimal — see §4)

// self-hostable hash — impossible before NUM (needs wrapping + bitwise + exact int):
fn fnv1a(bytes: array<int>) -> int {
  let h = 0x811c9dc5
  for (b of bytes) { h = (h ^ b) *% 0x01000193 }   // *% = wrapping multiply
  return h & 0xFFFFFFFF
}

// code points are ints (Go rune model — no char type):
let upper = string.from_codepoints(
  array.map("hi".codepoints(), fn(c) { return c - 32 })   // "HI"
)
```

## 4. Display, serialization & stdlib semantics

- **Printing/`str()`:** `int` renders with no decimal (`5`); **`float` always renders with at least
  one fractional digit** (`5.0`, not `5`) so the two subtypes are visually distinguishable — the
  Python/Swift convention. `-0.0` renders `-0.0`; `inf`/`-inf`/`nan` unchanged. **This is the largest
  single source of golden churn** (every numeric literal in output changes) and is handled by the
  corpus migration (Gate 7).
- **JSON:** `json.stringify(int)` → `5`; `json.stringify(float)` → `5.0`. `json.parse` round-trips by
  syntax: a JSON number containing `.` or `e`/`E` → `float`, otherwise → `int` (so `parse∘stringify`
  preserves subtype). Numbers outside i64 range but integral parse as `float` (documented), pending
  `bigint`. `json.parse(text, Class)` typed-parse honors `int`/`float`/`number` field annotations via
  `validate_into`.
- **`std/math`:** functions are typed by contract — `math.floor/ceil/round/trunc(float) -> int`;
  `math.sqrt/sin/...(number) -> float`; `math.abs` is subtype-preserving (`abs(int)->int`,
  `abs(float)->float`, with `abs(i64::MIN)` a checked-overflow panic); new `math.floordiv/divmod/
  ceildiv` (int→int); bit helpers `math.popcount/leading_zeros/trailing_zeros/rotl/rotr` (int→int).
- **Conversions (`std/convert` + builtins):** `int(x)` (float→int truncates toward zero; string→int
  parses, returns `[int, err]`; int→int identity), `float(x)` (int→float exact for |x|<2^53, nearest
  otherwise; string→float parses). No `as` cast operator (kept for destructuring/import rename).
- **`std/decimal`:** unchanged; `decimal + int`/`decimal + float` rules documented (int promotes to
  decimal exactly; float→decimal is an explicit `decimal(float)` to avoid hidden precision surprises).
- **Other stdlib:** array indices, `string` lengths/offsets, `Bytes` indexing, range bounds, and `for`
  loop counters all become `int` (they are conceptually integers today, stored as `f64`). `array[i]`
  requires `i` to be a non-negative `int` in range (a `float` index is a Tier-2 panic
  `array index must be an int, got float` — catching a real bug class). Ranges (`a..b`, `step`)
  produce `int` sequences when bounds are `int` (today they materialize `array<number>`).

## 5. Type-system integration (both type representations)

AScript maintains two type representations; **both** gain the new kinds:

- **Runtime contracts (`src/ast.rs` `Type`):** add `Type::Int` and `Type::Float`; make `Type::Number`
  mean the union `Int | Float` in `check_type` (accepts either). `Display`: `int`, `float`, `number`.
  `check_type` arms enforce: `: int` rejects a `float` value (and vice-versa); `: number` accepts
  both; `: decimal` unchanged.
- **Static checker (`src/check/infer/` `CheckTy`):** add `CheckTy::Int`, `CheckTy::Float`; `number`
  desugars to `Union([Int, Float])`. Update `assignable`/`synth`:
  - integer literal `synth`s `Int`; float literal `synth`s `Float`.
  - arithmetic `synth` follows the §3.2 table (e.g. `Int + Int : Int`, `Int + Float : Float`,
    `Int / Int : Int`); a bitwise/shift/wrapping op on a provable `Float` is a **`type-error`** (the
    existing arithmetic-on-non-number code, extended).
  - **Gradual gate holds:** `examples/**` emits **zero** `type-*` false positives in both feature
    configs. Only *provably* wrong annotated code emits. An unannotated literal flows as its synthed
    subtype; `number`-annotated slots accept both — so the untyped corpus is unaffected (Gate 5).
- A `: number` parameter that internally needs a specific subtype is the programmer's responsibility
  (narrow with `instanceof int` — see §6) — the checker does not silently coerce.

## 6. Reflection & narrowing

- `type_name(v)` returns `"int"` / `"float"` / `"decimal"`.
- `v instanceof int` / `v instanceof float` / `v instanceof number` are **true type guards** (the
  checker narrows a `number` to the subtype in the guarded branch, like the existing `instanceof`/nil-guard
  narrowing in `pass.rs`). This is how you safely go from `number` to `int` without a silent coercion.
  `5 instanceof number` is `true` for any int or float (union membership); `5 instanceof int` is `true`,
  `5 instanceof float` is `false`; `5.0 instanceof float` is `true`.

  **[CRITICAL] Mechanism — reserved-type-name RHS in the shared `instanceof` arm.** Today `instanceof`
  requires a `Value::Class` rhs and **panics on anything else** (verified `src/interp.rs:5100–5107`: the
  `BinOp::InstanceOf` arm of the shared `apply_binop` does `let Value::Class(cls) = &r else { … return
  Err("instanceof requires a class on the right-hand side") }`). Crucially, **the VM does NOT have a
  per-op `instanceof` handler** — `Op::InstanceOf` is dispatched through the *same* shared `apply_binop`
  (verified `src/vm/run.rs:4292`: `Op::InstanceOf => BinOp::InstanceOf` feeds the shared binop path), so a
  single edit covers both engines. The design:
  - **Parse.** `int`/`float`/`number` are reserved type names that already lex as identifiers; the
    grammar must accept a reserved-type-name (or bare ident) on the rhs of `instanceof` so `x instanceof
    int` parses in *expression* position (the rhs is an ordinary value-expr `Ident("int")` today). Both
    parsers + tree-sitter (so `instanceof int` parses), per §8.
  - **Evaluate (both engines, ONE edit in `apply_binop`'s `BinOp::InstanceOf` arm at
    `interp.rs:5100`).** *Before* the `Value::Class` extraction, recognize a reserved-type-name rhs.
    Because the rhs is evaluated to a `Value`, `int`/`float`/`number` will be unbound identifiers, so the
    arm matches them **by their pre-evaluation AST/name** — i.e. the `instanceof` lowering carries the rhs
    type-name when it is one of the three reserved names (a small dedicated `BinOp`/operand discriminant
    or an `is_reserved_type_name` check on the rhs expr before it is evaluated), routing to a subtype
    check: `instanceof int` → `matches!(l, Value::Int(_))`; `instanceof float` → `matches!(l,
    Value::Float(_))`; `instanceof number` → `matches!(l, Value::Int(_) | Value::Float(_))`. A
    non-reserved, non-class rhs keeps today's "requires a class" panic. This avoids the
    `check_type`-has-no-env limitation (cross-cutting #4): the recognition is name-based at the operator
    site, not a `Type::Named` class-name lookup.
  - **Checker narrowing (`src/check/infer/pass.rs`).** `instanceof int`/`float`/`number` narrows the lhs
    `CheckTy` in the guarded branch (and the complement in the `else`), reusing the existing
    `instanceof`-narrowing machinery, with `number` desugaring to `Union([Int, Float])`.

## 7. Determinism & the differential oracle

- The feature lives at the `Value`/`Interp` layer both engines share, so **`tree-walker ==
  specialized-VM == generic-VM` byte-identical holds** by construction — including the new printing,
  the checked-overflow panics, and the exact cross-subtype comparisons.
- Determinism (SP9) is unaffected: integer arithmetic is pure and deterministic; no new clock/RNG
  seams. The `.aso` constant pool gains an `Int` kind (§8) so compiled programs replay identically.
- The VM's **adaptive arithmetic** (`src/vm/adapt.rs`) gains `ArithKind::Int` alongside `Number`
  (renamed `Float`): a site observed with two `Int` operands specializes to inline i64 math **with the
  checked-overflow branch**; a mixed/`Float` observation deopts to the generic promoting path. The
  generic and specialized paths MUST be byte-identical (including which inputs panic) — the three-way
  differential is the guardrail.
- **Gate 12 — commit the `ArithKind::Int` checked path to a benchmark (no steady-state regression).**
  The specialized int path adds a `checked_add`/`checked_mul` branch (the overflow check) to the inline
  arithmetic. The Gate-12 acceptance bench must show this adds **no steady-state regression** on a
  tight integer loop (e.g. a `for`-counter sum / a Fibonacci / the `fnv1a` hash from §3.5): the
  overflow branch is **predictably-not-taken** (non-overflowing operands take the fast arm every
  iteration, so the branch predictor pins it ~free), and the float path is unchanged. The bench runs in
  all three VM configs (specialized, generic, and the `--no-specialize` generic-mode axis — no
  generic-mode regression either), mirroring the existing adaptive-arithmetic bench, and lands with the
  NUM PR. A measured steady-state regression on the int loop is a Gate-12 failure, not an accepted cost.

## 8. Implementation surface & cross-cutting subsystems

Per the `CLAUDE.md` "Touching syntax" checklist plus the numeric-specific surfaces. **Every item is a
required deliverable.**

**Values & core (`src/value.rs`):** add `Value::Int(i64)` (the variant sits next to `Value::Number(f64)`
at `value.rs:626`); **rename `Value::Number(f64)` → `Value::Float(f64)`** (791 call sites — a mechanical,
compiler-enforced rename pass); add `MapKey::Int(i64)` with the integral-float-folding `MapKey::from`
(§3.3); arms in `PartialEq`/`Eq`/`Hash`/`Debug`/`Display`/`is_truthy`/`type_name`; GC `Trace` (scalar,
no-op). **Truthiness (§3.3, BREAKING):** rewrite `is_truthy` (`value.rs:687`) from the current
`!matches!(self, Value::Nil | Value::Bool(false))` to the explicit falsy set
(`Nil`, `Bool(false)`, `Int(0)`, `Float` that is `0.0`/`-0.0`/`NaN`, `Decimal::ZERO`, `Str("")`;
collections/objects/instances stay truthy); update the `truthiness_follows_spec` unit test
(`value.rs:923`) and its doc-comment; the test runs in both feature configs.

**Lexer (`src/lexer.rs`, `src/lex_literals.rs`, `src/token.rs`):** `Tok::Int(i64)`/`Tok::Float(f64)`
replacing `Tok::Number`; `parse_number_text` returns the subtype (and octal `0o`); the new operator
tokens (`Amp Caret Tilde Shl Shr PlusPercent MinusPercent StarPercent`); literal-out-of-range error.

**AST (`src/ast.rs`):** `BinOp` gains `BitAnd BitOr BitXor Shl Shr WrapAdd WrapSub WrapMul`; `UnOp`
gains `BitNot`; literal expr carries int-vs-float; `Type::Int`/`Type::Float` + `Display`. Exhaustive
matches in `interp.rs`, `fmt.rs`, `ast.rs` `Display` get the new arms (compile-error-enforced).

**Both parsers:** legacy precedence-climbing (`src/parser.rs`) and CST Pratt (`src/syntax/parser.rs`)
encode the Go-style bitwise precedence with the **`bitor()` tier the pattern/type entry points bypass**
(§3.4 — legacy `parse_pattern` re-pointed to `range()`; CST patterns never enter `expr_bp`), the
`>>`/generics split, the new literal tokens, and the **reserved-type-name rhs of `instanceof`**
(`x instanceof int|float|number` must parse in expression position — §6). Frontend conformance
(`tests/frontend_conformance.rs`) proves they agree, including the required `1|2`-pattern-vs-`a|b`-value
and `a & b == c` tests from §3.4.

**Tree-sitter (`tree-sitter-ascript/`):** add int/float literal rules (octal), the bitwise/shift/
wrapping operators with Go precedence (declare any GLR conflicts), `>>`-in-type handling; regen
`parser.c` (`--abi 14`); update `queries/highlights.scm` (number-literal + operator highlighting);
**publish** via `./scripts/sync-grammar.sh` and bump the editor pins (`editors/zed/extension.toml`
`commit`, `editors/nvim/lua/ascript/treesitter.lua` `revision`); update VS Code TextMate
(`editors/vscode/syntaxes/ascript.tmLanguage.json`), Zed & Neovim `highlights.scm` copies.

**Both engines:** tree-walker arithmetic + comparison + bitwise in `interp.rs`; VM opcodes
`BitAnd BitOr BitXor Shl Shr BitNot WrapAdd WrapSub WrapMul` (+ checked-overflow on `Add/Sub/Mul/Pow/
Neg` for ints, and the out-of-range shift-amount panic on `Shl/Shr`) in `src/vm/opcode.rs` +
`src/vm/run.rs`; `src/vm/adapt.rs` `ArithKind::Int` (§7); `src/vm/disasm.rs` for the new ops.
**`instanceof int|float|number` (§6):** the ONE edit is in the shared `apply_binop` `BinOp::InstanceOf`
arm (`interp.rs:5100`) — reserved-type-name rhs recognition *before* the `Value::Class` extraction; the
VM needs **no** separate handler because `Op::InstanceOf` routes through this same arm
(`src/vm/run.rs:4292`).

**`.aso` (`src/vm/aso.rs` + `src/vm/verify.rs`):** the constant pool gains an `Int` constant kind;
new opcodes serialized; **bump `ASO_FORMAT_VERSION` by exactly one — READ the constant, do not hardcode
`19`.** Per `goal.md` §"`.aso` version bumps are sequential" and `REVIEW-FINDINGS-2026-06-08.md` §"Owner
decisions", NUM/ADT/IFACE/DBG each add `+1` in merge order; **NUM merges first** so NUM's bump is
"current value → current value + 1" (the constant is `ASO_FORMAT_VERSION = 18` at `src/vm/aso.rs:105`
*today*, but the implementer reads `ASO_FORMAT_VERSION` and adds one rather than writing a literal `19`,
so a re-sequenced merge order stays correct). This is the standalone-P0 `.aso` reader-allocation clamp's
sibling concern but is **not** NUM's bug to fix (that P0 is owned separately per the review).

**Worker airlock (`src/worker/serialize.rs`):** a new wire tag for `Int` (the `Float` tag is the
former `Number` tag, value-identical); round-trip test for ints incl. as Map keys.

**Type systems:** `Type::Int`/`Float` (ast.rs `check_type`); `CheckTy::Int`/`Float` + `assignable`/
`synth`/narrowing (`src/check/infer/`); `instanceof int|float|number` narrowing in `pass.rs`;
`std_arity.rs` unaffected (no new script-exposed fns beyond math/convert helpers — register those).

**Formatter (`src/fmt.rs`):** render int vs float literals canonically (a float that lost its `.0`
must round-trip — `5.0` stays `5.0`); render the new operators; idempotence goldens.

**LSP (`src/lsp/`):** semantic tokens for the new operators/literals; `hover` shows `int`/`float`/
`number`; diagnostics flow the new `type-error` cases; completion offers `int`/`float`/`number` as
type names.

**REPL (`src/repl.rs`):** new operators are ordinary tokens; delimiter-depth buffering unaffected;
add a regression test (`5 / 2` → `2`, `5.0 / 2` → `2.5`, `1 << 3` → `8`).

**Stdlib:** `math` (typed returns + new helpers), `convert`/builtins (`int`/`float`), `json`
(subtype round-trip), `array`/`string`/`bytes`/range (int indices/offsets/lengths), `decimal`
(promotion rules). Update each module's `docs/content/stdlib/*.md`.

**Docs:** a "Numbers" section in `docs/content/language/values-types.md` (the tower, division,
overflow, bitwise, code-points-as-int); update `README.md` if its types table lists number; the main
design-spec numeric section; `CLAUDE.md` (the "Values" paragraph + a numeric-model note);
`roadmap.md`. NAV unchanged unless a new page is added (it is not — appended to existing pages).

**Unchanged:** the GC, the `Interp` async model, structured concurrency, the worker pool/scheduler,
all non-numeric stdlib, the single-threaded hot path.

## 9. Testing, corpus & migration

### 9.1 Unit & property tests (the no-bugs pillar)
- **Lexer:** every literal form incl. octal, underscores, out-of-range error, float vs int
  discrimination, `1e3` is float.
- **Arithmetic table (§3.2):** one test per cell, including the panics (int `/0`, `%0`, overflow on
  `+ - * **` and unary `-`; wrapping ops do **not** panic). **Shift boundaries (§3.2):** `1<<63 ==
  i64::MIN`, `1<<64` panics (amount ≥ 64), `1 << -1` panics, `-1<<1 == -2`, `-8 >> 1 == -4`
  (arithmetic sign-extension) — shift **bit-loss does not panic**, only an out-of-range amount does.
  **Pow boundaries (§3.2):** `0**0 == 1`, `(-2)**3 == -8`, `2**-1 == 0.5` (float), `2**4 == 16`,
  `2**63` panics (overflow), and a > `u32::MAX` exponent takes the float path.
- **Truthiness (§3.3, both feature configs):** `is_truthy` returns `false` for `Int(0)`,
  `Float(0.0)`, `Float(-0.0)`, `Float(f64::NAN)`, `Decimal::ZERO`, `Str("")`, `Nil`, `Bool(false)`;
  and `true` for `Int(1)`, `Float(1.0)`, a non-empty `Str`, and an **empty** array/object/set/instance
  (collections stay truthy). This replaces the old `truthiness_follows_spec` assertions
  (`value.rs:923`).
- **Exact comparison (property test):** `∀ i: i64, f: f64` from a generated set,
  `(Int(i) == Float(f))` agrees with exact mathematical equality (catches the 2^53 boundary).
- **Map-key consistency (property test), scoped to finite `{int,float}`:**
  `∀ finite numeric a,b ∈ {int,float}: (a==b) ⟺ (MapKey::from(a)==MapKey::from(b))` — including integral
  floats folding to int keys. **NaN is explicitly excluded** (`NaN != NaN`, so the iff fails for NaN; the
  pre-existing single-canonical-NaN key behavior is unchanged and separately asserted), and **Decimal keys
  stay distinct** (never folded into `Int`/`Float`) — both carve-outs are asserted as their own cases.
- **Round-trips:** `json.parse∘stringify` preserves subtype; `aso` write→read preserves `Int`
  constants; worker `encode∘decode` preserves `Int` (incl. as Map keys).
- **Bitwise:** `& | ^ << >> ~`, shift-amount bounds panic, `>>` arithmetic sign-extension, Go
  precedence (`a & b == c` ⇒ `(a&b)==c`; `a | b == c` ⇒ `(a|b)==c`). **Parser collision (frontend
  conformance):** `match x { 1|2 => … }` is a two-alternative pattern while value-position `a | b` is a
  single bitwise-or expression, and `let t: int | float` is a union type — all three on both front-ends
  (§3.4).
- **Checker:** `1+1 : int`, `1+1.0 : float`, `1/2 : int`, `1.0/2 : float`, bitwise-on-float is
  `type-error`; **`examples/**` emits zero `type-*`** in both configs.

### 9.2 Four-mode byte-identity (REQUIRED)
Every numeric example runs identically on tree-walker, specialized VM, generic VM, and `.aso`-compiled
(`tests/vm_differential.rs`, both feature configs) — including the new printing, the panics, and the
adaptive-int specialization vs the generic path.

### 9.3 Example corpus
- New: `examples/integers.as` (literals, bases, division, overflow + wrapping, bitwise, packing),
  `examples/numeric_tower.as` (int/float/decimal interop, promotion, conversions),
  `examples/advanced/bit_codec.as` (a real encoder/decoder — varint or base64 — using wrapping +
  bitwise, the self-hosting proof-of-capability).
- **Migration (Gate 7):** every existing `examples/*.as`, `examples/advanced/*.as`, and golden is
  updated to the new semantics. The corpus is *migrated*, never trimmed to avoid a break. The
  differential harness churn is expected and reviewed. **Blast-radius notes (the migration drivers, in
  rough order of churn):**
  - **Division goldens flip hardest.** Any `int / int` that used to yield a fractional float now
    truncates — `10 / 3` goes `3.33…` → `3`, `7 / 2` → `3`, `1 / 2` → `0`. Every golden that printed a
    quotient must be re-derived (not mechanically search-replaced — the *value* changed). Where the
    intent was real division, the example is updated to force a float (`10.0 / 3`).
  - **Float printing `5.0`.** Every float literal that printed as `5` now prints `5.0` — the single
    largest count of golden line-edits (pure output churn, value unchanged).
  - **Truthiness.** Any `if (x)` / `while (x)` / `x && …` / `x || …` / ternary on a number or string
    where `x` could be `0`/`0.0`/`""` now takes the other branch — a *behavioral* migration (not just
    output). Grep the corpus for numeric/string truthiness sites; convert intentional
    "is-present" checks to explicit comparisons (`x != 0`, `x != ""`) or keep them if the new meaning is
    the intended one. (Empty collections are unaffected — they stay truthy.)
  - **Int indices/lengths.** `array[i]`, `string` offsets, range bounds, and `for` counters become
    `int`; goldens that printed a length/index are unchanged in value (already integral) but their
    *type* is now `int` (so a `print(len(xs))` still shows `3`, not `3.0`).

### 9.4 FUZZ hook (continuous infra)
The numeric tower is a priority target for the FUZZ spec's property/differential fuzzers (overflow
edges, comparison boundary, division-by-zero, Map-key folding). NUM lands the property tests above;
FUZZ generalizes them into the continuous fuzzing harness.

## 10. Scope & rejected alternatives

**In scope:** `int` (i64) + `float` (f64); type-directed division (no `//`); checked overflow +
wrapping ops; bitwise/shift ops (Go precedence, with the `bitor` tier the pattern/type parsers bypass);
octal literals; code-points-as-int + string codepoint methods; exact cross-subtype comparison + Map-key
consistency (finite `{int,float}`); **the campaign-wide truthiness change** (`0`/`0.0`/`0m`/`""`/`NaN`
falsy; collections truthy — NUM owns the `value.rs` `is_truthy` edit); `instanceof int|float|number`;
both type systems; the `.aso` + worker-wire + adaptive-arithmetic integration; full corpus migration;
docs.

**Out of scope (reserved/deferred):**
- **`bigint`** — type name reserved; a separate spec (its own `Value` kind + promotion-on-overflow
  option). NUM keeps overflow as *checked panic*, not silent promotion.
- **Sized integers (`i32`/`u8`/`u64`…) as runtime kinds** — these belong to **FFI** (C-ABI
  marshalling concern), expressed as annotations/marshalling over `int`, not new `Value` variants.
- **A `char` type / char literals** — rejected (§1.1); code points are `int`s. A future `c'…'` sigil
  could be added purely additively if ever justified.
- **Operator overloading for numeric user types** — out of scope (a campaign-wide non-goal).

**Rejected:**
- **Keep `f64`-only / opt-in integers (the earlier "Lua hedge").** Existed only to preserve
  `1/2==0.5` for backward-compat, which is no longer a constraint. Integer-as-default is correct for a
  serious GP language and unlocks the perf + self-hosting goals.
- **A `//` floor-division operator.** Unnecessary under type-directed division, and it collides with
  `//` line comments. Flooring lives in `math.floordiv`.
- **Silent two's-complement wraparound by default (the C/Rust-release model).** A bug farm; we trap by
  default and make wrapping explicit (`+% -% *%`), the Swift/Zig choice.
- **Lossy int→float promotion for comparison.** Introduces precision bugs at |int| > 2^53; we compare
  exactly.
- **C-style bitwise precedence.** The `a & b == c` footgun; we adopt Go's tighter binding.

## 11. Grounding (verified sources)

- Type-directed division (`int/int→int`): C, Go, Rust, Java, C#, Swift language references.
- Unified-then-distinct numeric subtypes & migration cost: Lua 5.3 `Number` integer/float subtypes
  (Lua 5.3 manual §2.1, §3.4.3) — adopted as *distinct types* here rather than hidden subtypes.
- Checked overflow + explicit wrapping operators: Swift (`&+`/`&*` trapping default), Zig (`+%`).
- Code-points-as-int (no `char`): Go `rune` = `int32`; `'A'` is an integer constant in Go.
- `>>`/nested-generics split: Rust, Java, C# parser handling of `>>` token in type-argument position.
- Go-style bitwise precedence (avoids `a & b == c`): Go spec, operator precedence table.
- Exact i64↔f64 comparison technique: standard mixed-type ordered comparison (CPython `float`/`int`
  rich-compare; Rust `PartialOrd` between integer and float crates).
