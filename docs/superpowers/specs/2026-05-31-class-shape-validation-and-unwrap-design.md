# AScript — Class Shape Validation & Force-Unwrap Design Spec

- **Status:** Draft for review
- **Date:** 2026-05-31
- **Spec:** extends `specs/2026-05-29-ascript-design.md` (§§ types/contracts, classes, errors)
- **Milestone:** (TBD — assign on roadmap)

---

## 1. Motivation

When a script consumes external data — most commonly JSON from an HTTP response — it
currently lands as a raw `Object` (an insertion-ordered bag of fields). There is no
ergonomic way to assert *"this payload has the shape I expect, with the right field
types"* and get a checked, typed value back.

The obstacle is structural, not cosmetic. AScript's type contracts are **nominal and
value-introspective**: `check_type(value, ty)` answers *"does this value satisfy this
type?"* using only what the value itself carries. A class contract `x: User` succeeds
only for a `Value::Instance` whose class chain is named `User` — the instance is a
self-describing token. A parsed JSON `Object` carries no such token and there is no
registry to consult, so:

```javascript
let [data, _] = await resp.json()
let u: User = data        // ✗ panic: type contract violated: expected User, got object
```

There is also no terse way to bridge AScript's two error tiers. `resp.json()` returns a
Tier-1 `[val, err]` pair (recoverable); a contract violation is a Tier-2 panic
(programmer error). Funnelling a fallible parse + a fallible validation into a single
result today requires several lines of manual destructuring.

### Goals

1. Let a class declare its **expected shape** (typed fields), including **optional** and
   **defaulted** fields.
2. Provide **one validating boundary** that turns an untrusted `Object` into a checked,
   nominal instance — recursively, for nested objects.
3. Add a small, general **force-unwrap operator** that is the dual of `?`, so the
   common case collapses to one expression.

The target one-liner this design makes real (no parens around `await` needed — see
the precedence rule in §5.2):

```javascript
let user = recover(() => User.from(await resp.json()!))?   // general primitives (§4–§5)
let user = await resp.json(User)?                          // typed-parse shortcut (§4.5)
```

Both are valid; the second is sugar built from the first, for the common HTTP-JSON case.

### Non-goals (and why)

- **Structural `interface` types.** Would introduce a second, parallel type-declaration
  concept (structural alongside nominal) and force `check_type` to become
  environment-aware (it must look up the interface's declared fields, which live in the
  environment, not on the value). This rebuilds the validator's benefit at much higher
  conceptual + architectural cost. Rejected. The validating boundary (§4) delivers the
  same capability inside the existing nominal model.
- **User-facing generics / type parameters (`resp.json<User>()`).** AScript has no
  user-definable type parameters and this design adds none. The typed-parse shortcut
  in §4.5 (`resp.json(User)`) passes the class *as an ordinary value argument*, not as a
  type parameter — that is precisely what lets us deliver the ergonomics without
  generics. Generic *syntax* (`<User>`) remains rejected.

---

## 2. Design Principles Preserved

- **Tiny core / gradual contracts.** Declared fields are checked; undeclared fields stay
  dynamic and unchecked. Existing classes are unaffected.
- **Value-introspective typing stays intact.** `check_type` remains a pure function over
  `(Value, Type)`. The only structural operation is the explicit validation crossing —
  `.from` and the typed-parse shortcut (§4.5), which share one `validate_into` core — and
  it *produces* a self-describing instance; type checks downstream remain nominal.
- **Two-tier error model stays intact.** `!` lifts Tier-1 → Tier-2 (pair → panic);
  `recover` lowers Tier-2 → Tier-1 (panic → pair); `?` moves a Tier-1 error outward.
  Each arrow has one direction and one meaning.

---

## 3. Feature 1 — Typed Class Fields

### 3.1 Syntax

Class bodies gain **field declarations** that may appear alongside methods:

```javascript
class User {
  id: number              // required
  name: string            // required
  nickname: string?       // optional — `T?` is sugar for `T | nil`
  avatar?: string         // also optional — `name?:` field marker, same meaning
  role: string = "guest"  // optional with default
  fn init(...) { ... }    // methods unchanged
}
```

**Optionality has two accepted spellings, both lowering to the same thing
(`T | nil`):**

1. **`T?` — a type-level suffix (the general feature).** Valid in *any* type position —
   field, `let`/`const` binding, function parameter, and return type:
   ```javascript
   let port: number? = nil
   fn lookup(key: string): User? { ... }
   ```
   `T?` desugars to `T | nil` and is represented by a `Type::Optional(Box<Type>)` AST
   node (see §3.2). `check_type(v, Optional(T))` ≡ `check_type(v, T) || v == nil`.

2. **`name?: T` — a field-only marker (accepted alias).** Familiar to JS/TS authors.
   In field-declaration position only, it lowers to a field of type `Type::Optional(T)`
   — i.e. exactly what `name: T?` produces. It carries no separate semantics.

Grammar:

```
type          := … | type "?"                       // nullable suffix, any type position
field_decl    := Ident "?"? ":" type ("=" expression)?
```

- **Both spellings run identically.** The parser accepts `name?: T` and `name: T?`
  equally and lowers both to the same `Type::Optional` node, so they are the same program
  to the interpreter, LSP, and tree-sitter. Neither requires the formatter to be valid or
  to run — writing `name?: T` and never formatting changes nothing about execution.
- **Canonical form (formatter output only): `name: Type?`.** The formatter is a separate,
  optional, *cosmetic* tool (`ascript fmt`); it is never in the `ascript run` path and
  never changes semantics. When run, it rewrites the `name?: T` alias *to* `name: T?` so
  `?` always sits in one place across fields, bindings, params, and returns. (An
  explicitly written `T | nil` union is *not* rewritten to `T?`; the formatter preserves
  authorial intent and only normalizes the two `?` spellings.)
- A default expression also makes a field optional regardless of `?` (it is evaluated
  lazily — see §3.3).
- `?` in **type position** (nullable suffix) never overlaps `?` in **expression
  position** (ternary / propagate): a type only appears after `:` in a declaration,
  parameter, return, or field. The tree-sitter grammar declares the boundary conflict
  (see §8).

### 3.2 Data model

- **Type AST** (`ast.rs`, the `Type` enum): add `Type::Optional(Box<Type>)`. Both `T?`
  and the `name?:` marker produce it; `check_type` treats `Optional(T)` as `T | nil`.
  This is a general type-system addition, not class-specific.
- **AST** (`ast.rs`, the `Class` node): add `fields: Vec<FieldDecl>` where
  `FieldDecl { name: String, ty: Type, default: Option<Expr>, span: Span, name_span: Span }`.
  No separate `optional` flag is needed — optionality lives in the type (`Type::Optional`)
  or is implied by a `default`.
- **Runtime** (`value.rs`, `struct Class`): add
  `fields: IndexMap<String, FieldSchema>` where `FieldSchema { ty: Type, default: Option<Expr> }`.

### 3.3 Semantics

- **Assignment checking.** Assigning to a *declared* field — including `self.x = …`
  inside `init` — runs `check_type` against the declared type. A violation is a Tier-2
  contract panic (recoverable), identical in message shape to existing contract panics.
- **Undeclared fields remain dynamic.** Assigning to a field the class did not declare is
  allowed and unchecked, preserving backward compatibility and the gradual ethos.
- **Defaults** are evaluated only when a field is absent at construction time (see §4),
  in the class definition environment. A default value is itself checked against the
  field type (catches a wrong default at definition-exercise time).

### 3.4 Touch points

See the consolidated subsystem inventory in §8 (AST `FieldDecl`, parser class-body,
runtime `Class` schema + assignment checks, formatter `write_field`, tree-sitter
`class_member`, LSP `PROPERTY` symbols, docs).

---

## 4. Feature 2 — `ClassName.from(obj, strict = false)`

The single structural → nominal crossing. It is a built-in associated function available
on every class value.

### 4.1 Signature & behavior

```
ClassName.from(obj: object, strict: bool = false) -> Instance   // panics on mismatch
```

For each field declared on the class (and its superclasses), in declaration order:

1. Read `obj[name]`. A missing key reads as `nil`.
2. If the value is `nil` **and** the field has a default, evaluate and use the default.
3. **Recurse:** if the field's declared type is a class `C` and the current value is a
   raw `Object`, replace it with `C.from(value, strict)`. (Arrays of a class type
   recurse element-wise — see §4.3.)
4. Run `check_type(value, fieldType)`. On failure, **panic** with a contract error naming
   the field path (e.g. `user.address.zip`).

The result is a fully populated `Value::Instance` of the class. `.from` **does not call
`init`** — it is a pure shape-validator/constructor, independent of whatever constructor
signature or side effects `init` has.

### 4.2 `strict`

- `strict = false` (default, omittable): keys in `obj` that are not declared fields are
  **ignored**. Matches how JSON APIs evolve (servers add fields).
- `strict = true`: any undeclared key in `obj` is a validation error (panic), catching
  typos and unexpected payloads.

### 4.3 Nested & collection recursion

- **Nested class field** (`address: Address`): a raw nested `Object` is validated via
  `Address.from(value, strict)`. An already-`Address` instance passes through unchanged.
- **Array of class** (`tags: array<Tag>`): each element that is a raw `Object` is
  validated via `Tag.from(element, strict)`; the contract `array<Tag>` then holds.
- **Map of class** (`byId: map<string, Tag>`): each *value* is validated via
  `Tag.from(value, strict)` and the contract `map<string, Tag>` then holds. (Keys are not
  recursed — map keys are scalars.) **Object→Map boundary coercion:** because JSON objects
  decode to AScript `Object`s (not `Map`s), a `map<K, V>`-typed field whose value is a raw
  `Object` is coerced into a `Map` *at the `.from` boundary* (string keys → map keys, each
  value recursed through the value type) before the contract is checked. This coercion is
  scoped strictly to `.from`/typed-parse (untrusted-data ingestion) — it is **not** a
  general language Object↔Map coercion — and it is what makes `map<string, Tag>` usable for
  the common JSON-dictionary shape `{"byId": {"1": {…}}}`. An already-`Map` value recurses
  its values directly. This completes recursion symmetry across all three container shapes
  (nested class, `array<Class>`, `map<K, Class>`).
- **Cycles:** JSON cannot express cycles, and `.from` consumes plain data, so unbounded
  recursion is not a concern in practice. (No cycle guard in v1; revisit only if `.from`
  is ever pointed at script-constructed graphs.)

### 4.4 Error reporting

Validation panics carry a **field path** so a deep mismatch is diagnosable:
`type contract violated at user.address.zip: expected number, got string ("90210")`.
Because panics are recoverable, the whole `.from` call composes with `recover` (§6).

### 4.5 Non-panicking core + typed-parse shortcut

The validation logic is implemented **once** as a non-panicking core that returns a
`Result`-style outcome rather than raising:

```
fn validate_into(class, obj, strict) -> Result<Instance, ValidationError>
```

Two thin adapters wrap it, placing the result on the correct error tier:

- **`ClassName.from(obj, strict = false)`** (§4.1) → on `Err`, **panics** (Tier-2). This
  is the boundary used inside `recover`/`!`.
- **Typed parse on decode functions** → on `Err`, returns a **Tier-1 `[nil, err]` pair**,
  fusing validation failure into the *same* channel as a parse failure:
  - **`resp.json(Class)`** (`std/net/http`): decode the body, then validate against
    `Class`. A malformed body and a shape mismatch both surface as the single `err`.
  - **`json.parse(text, Class)`** (`std/json`): parse, then validate. Same fusion.

  In both, the class argument is **optional**: `resp.json()` / `json.parse(text)` keep
  their current behavior (return the raw decoded value); passing a class adds validation.
  The class rides in as an ordinary value argument — **no generics, no type parameters**.

This is why the §1 shortcut works and reads so cleanly:

```javascript
let user = await resp.json(User)?     // ≡ (await resp.json(User))?  — see §5.2
let [user, err] = await resp.json(User)   // or handle the pair explicitly
```

The shortcut uses default lenient (`strict = false`) matching. A caller needing strict
matching parses raw and then validates explicitly: `Class.from(raw, true)` (§4.2).

---

## 5. Feature 3 — Postfix `!` (force-unwrap)

The dual of the postfix `?` propagate operator.

### 5.1 Semantics

`expr!` evaluates `expr`, which must be a Tier-1 pair `[val, err]`:

- `err == nil`  → the expression evaluates to `val`.
- `err != nil`  → **panic, carrying `err` itself as the payload**. This is the key
  property: because the original error object is the panic payload, a surrounding
  `recover` round-trips the *exact* message (no laundering into a generic error).
- `expr` is not a 2-element `[val, err]` pair → Tier-2 misuse panic.

`!` is usable **anywhere on any pair**, in any expression position.

### 5.2 Grammar & precedence

`?` and `!` become the **loosest postfix tier — looser than `await` and prefix unary**
(`!x`, `-x`). Concretely, `Try` moves out of the `postfix()` loop into a new precedence
level between `unary()` and the binary operators, and the new unwrap-`!` joins it there.

Resulting precedence (tightest → loosest):

```
. () []   >   await / prefix ! / unary -   >   postfix ? / postfix !   >   binary ops
```

Consequences:

- `await resp.json()!` parses as `(await resp.json())!` — `!` unwraps the **resolved
  pair**, not the Future. (This is the "better for devs" choice: no mandatory parens.)
- As a free bonus, `await f()?` now means `(await f())?`. This is new behavior for a
  combination no existing code uses (verified: the repo never combines `await` with `?`),
  so there is **no migration cost**.
- `?` and `!` share one left-associative level; `f()!?` / `f()?!` chain left-to-right.

The token is the existing `Tok::Bang`. Prefix `!x` (logical not, in `unary()`) vs postfix
`x!` (unwrap, in the new level) is disambiguated by position — exactly as the existing
`?` is already disambiguated between ternary and propagate.

### 5.3 Touch points

See §8. Note the CLAUDE.md invariant: every new `ExprKind` (here `Unwrap`) needs arms in
interp (eval), `fmt.rs` (`write_expr_inner`), and `ast.rs` (`Display`) — plus the
precedence-tier move (`Try`/`Unwrap` looser than `await`) mirrored in both the parser and
the formatter's `expr_prec`, and in the tree-sitter grammar (regen `parser.c --abi 14`).

---

## 6. End-to-End Example

```javascript
class Address {
  street: string
  zip: number
}

class User {
  id: number
  name: string
  nickname: string?          // optional (canonical spelling)
  role: string = "guest"     // defaulted
  address: Address           // nested — User.from recurses
}

// Using the general primitives (§4–§5):
async fn loadUser(resp): Result<User> {
  // `!` promotes a JSON parse failure to a panic AND unwraps the pair;
  // `User.from` panics on a shape mismatch; `recover` catches either;
  // `?` propagates the unified error.
  // No parens around `await` — `!` binds looser than `await` (§5.2).
  return recover(() => User.from(await resp.json()!))?
}

// Equivalent, using the typed-parse shortcut (§4.5):
async fn loadUser2(resp): Result<User> {
  return await resp.json(User)?   // parse + recursive validation, fused into one error
}
```

Failure modes, all surfaced as one Tier-1 error out of `loadUser`:

- Body is not JSON → `!` panics with the parse error → `recover` → `Err(parseErr)`.
- JSON is valid but `id` is a string → `User.from` panics
  `…at user.id: expected number…` → `recover` → `Err(...)`.
- `address.zip` missing → `…at user.address.zip: expected number, got nil`.
- `nickname` missing → fine (`nil`); `role` missing → defaults to `"guest"`.

---

## 7. Testing

- **Optional type `T?`:** desugars to `T | nil` and `check_type` accepts both `T` and
  `nil`, in *all* positions — field, `let`/`const`, parameter, return. Both spellings
  (`name: T?` and `name?: T`) parse to the same `Type::Optional` node.
- **Typed fields:** declared-field assignment checks (pass + violation); optional field
  accepts `nil` and absent; default applied when absent; undeclared field stays
  dynamic/unchecked; back-compat (existing field-free classes unchanged). Unit tests in
  `interp.rs`.
- **`.from`:** happy path; missing required → panic; optional/defaulted handling;
  `strict` rejects/ignores extras; nested object recursion; `array<Class>` and
  `map<K, Class>` recursion; field-path in error message; recoverable via `recover`.
- **Non-panicking core:** `validate_into` returns `Err` (not a panic) on mismatch;
  `.from` adapter panics on that `Err`; the decode adapter returns `[nil, err]`.
- **Typed parse:** `resp.json(User)` and `json.parse(text, User)` return `[instance, nil]`
  on a valid payload, `[nil, err]` on either a parse failure *or* a shape mismatch (one
  fused channel); `resp.json()` / `json.parse(text)` with no class argument behave exactly
  as before (raw decoded value); `await resp.json(User)?` unwraps to the instance.
- **Postfix `!`:** unwraps `[v, nil]` → `v`; `[nil, e]!` panics carrying `e`; misuse on
  non-pair; precedence (`await f()!` ⇒ `(await f())!`); chaining; `recover` round-trips
  the original error object losslessly.
- **Grammar guardrails:** add an `examples/*.as` exercising all three; both the
  hand-written parser and the tree-sitter grammar must accept it
  (`tests/treesitter_conformance.rs`, `tests/frontend_conformance.rs`).
- **Formatter:** field declarations and `expr!` round-trip through `fmt`; `name?: T`
  normalizes to canonical `name: T?`; `T?` round-trips in `let`/param/return positions;
  an explicit `T | nil` is left unchanged (not rewritten to `T?`).
- Clippy clean under **both** `--all-targets` and `--no-default-features --all-targets`.

---

## 8. Implementation Surface (subsystem-by-subsystem)

This is the authoritative checklist of everything that must be aware of the new syntax
and semantics. Subsystems are listed with the *specific* change and a verdict.
"No change — why" entries are intentional: they record that the subsystem was audited.

### 8.1 Front-end (compiler core)

| Subsystem | File(s) | Change |
|---|---|---|
| Lexer / tokens | `src/lexer.rs`, `src/token.rs` | **No new tokens.** `Tok::Bang` (`!`), `Tok::Question` (`?`), `Colon`, `Eq`, `Ident` all exist; `!=` is already a single `BangEq`. Field decls and postfix `!` reuse existing tokens. |
| AST | `src/ast.rs` | Add `Type::Optional(Box<Type>)` (the `T?` / `name?:` nullable type); add `FieldDecl { name, ty, default, span, name_span }` (no `optional` flag — optionality lives in `ty`); add `fields: Vec<FieldDecl>` to `Stmt::Class`; add `ExprKind::Unwrap(Box<Expr>)`. Add `Display` arms for `Type::Optional` (renders `T?`), the new `ExprKind`, and field declarations in `Stmt::Class`. |
| Parser | `src/parser.rs` | **Type parser:** accept a trailing `?` after any type → `Type::Optional` (works in `let`/`const`/param/return/field — general, not class-only). **Class body:** parse field declarations, accepting both `name?: T` (marker, lower to `Optional`) and `name: T?`. **Expressions:** add postfix `!`; **restructure precedence** so `?` (`Try`) and `!` (`Unwrap`) move out of the `postfix()` loop into a new tier *looser than `await`/unary* (so `await x!` ⇒ `(await x)!`, `await x?` ⇒ `(await x)?`). Add parser unit tests (`sexpr(...)`) pinning both the grouping and `T?` desugaring. |
| Interpreter | `src/interp.rs` | Add a `check_type` arm for `Type::Optional(T)` (≡ `check_type(v, T) || v == nil`); store `FieldSchema` on the runtime `Class` (`value.rs`); check declared-field types on assignment (incl. inside `init`); implement `validate_into(class, obj, strict) -> Result<Instance, _>` (recurses into nested class / `array<Class>` / `map<K,Class>` fields, applies defaults, builds field path for errors); `.from` adapter (panics on `Err`); `ExprKind::Unwrap` eval arm (panic carrying the original error on `[_, err!=nil]`). |
| Values | `src/value.rs` | Add `fields: IndexMap<String, FieldSchema>` to `struct Class`. |

### 8.2 Tooling that consumes the front-end

| Subsystem | File(s) | Change |
|---|---|---|
| Formatter | `src/fmt.rs` | Render `Type::Optional(T)` as `T?` (type rendering, used in all positions); new `write_field` emitting the **canonical** `name: Type? = default` form (i.e. normalize the `name?:` marker to a type-suffix `?`), **fields before methods** in the class body; do **not** rewrite an explicit `T \| nil` union to `T?`. Add `ExprKind::Unwrap` arm to `write_expr_inner`; **add a precedence tier** in `expr_prec` (`PREC_TRY` between assign and unary), move `Try` there, add `Unwrap` at postfix, so the formatter does **not** wrongly parenthesize `await x?` / `await x!`. Round-trip tests (incl. `name?:` → `name: T?` normalization). |
| Tree-sitter grammar | `docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js` | New `optional_type` rule (`seq($._type, '?')`) added to `_type` — the general nullable suffix. `class_body` → `repeat($.class_member)` where `class_member = choice(field_declaration, method_definition)`; new `field_declaration` accepting **both** `name?: T` (`?` after name, before `:`) and `name: T?` (`field('name'), optional('?'), ':', field('type'), optional(seq('=', $._expression))`). New `unwrap_expression` (postfix `!`); move `propagate_expression` (`?`) to a precedence tier looser than `unary`. Keep `[$._expression, $.propagate_expression]`; **add a declared conflict for `?` at the type-position boundary** (nullable-suffix vs ternary/propagate) if `tree-sitter generate` reports one; watch for new `!` conflicts (prefix-vs-postfix is position-disambiguated, `!=` is one token). **Regenerate `parser.c` with `tree-sitter generate --abi 14`.** |
| LSP | `src/lsp/analysis.rs` | Extend `document_symbols` `Stmt::Class` arm to emit declared fields as `SymbolKind::PROPERTY` children (currently methods-only). **Keyword list unchanged** (operators aren't completion items; `!`/`?` need no entry). No `ExprKind::Unwrap` arm needed (symbols walk statements, not expressions). Completion/hover for `obj.field` is **out of scope** (would need member type inference, which the LSP does not do). |
| REPL | `src/repl.rs` | **No change.** It delegates entirely to `lexer::lex` + `parser::parse` + `Interp`; single-line (no completeness heuristic to update). Multi-line class bodies are a pre-existing limitation, unaffected. |

### 8.3 Conformance & lint gates

| Gate | Change |
|---|---|
| `tests/treesitter_conformance.rs` | The new `examples/*.as` must be accepted by **both** the hand-written parser and the tree-sitter grammar with no errors. |
| `tests/frontend_conformance.rs` | Differential guardrail must still pass on the new syntax. |
| Clippy | Clean under **both** `--all-targets` and `--no-default-features --all-targets`. |

### 8.4 Documentation (prose + examples + renderer)

| Doc | Change |
|---|---|
| `docs/content/language/*` | Types page: document the nullable suffix `T?` (≡ `T \| nil`, all positions) on the types/contracts page. Classes page: typed fields, both optional spellings (`name: T?` and `name?: T`), defaults. Error-tier page: add `!` next to `?`/`recover` and note the `?`/`!` precedence-vs-`await` rule. |
| `docs/content/stdlib/net.md` | Document `resp.json(Class)` typed-parse argument. |
| `docs/content/stdlib/data.md` | Document `json.parse(text, Class)` typed-parse argument. |
| `docs/assets/app.js` (renderer) | **No change required.** The custom `highlightAScript()` keyword lists need no additions (no new keywords; field decls use existing tokens), and its operator regex already colorizes `!`/`?`. **Action: visually verify** the new examples render correctly once added (it's an audit, not an edit). |
| `README.md` | Update the feature/stdlib summary. |
| `CLAUDE.md` | Record the new `ExprKind::Unwrap` match obligations (interp eval, `fmt.rs`, `ast.rs` Display), the `class_body` grammar change, and the `?`/`!`-looser-than-`await` precedence rule. |
| `examples/` | Add an introductory example (typed fields + `.from` + `!`); optionally an `examples/advanced/` HTTP + `resp.json(User)` example. Both must stay runnable (conformance). |

---

## 9. Open Questions / Deferrals

- **Field declarations as constructor surface (auto-`init`):** this design keeps `init`
  and field declarations independent. Deriving a constructor from declared fields is a
  separable records/dataclasses feature with its own design questions (positional vs.
  named construction, interaction with an explicit `init`, default/optional mapping) and
  is orthogonal to JSON validation. Deferred to its own spec.
- **Typed parse for other decoders:** `resp.json(Class)` and `json.parse(text, Class)`
  cover the common cases. Extending the class-as-value pattern to other decoders (e.g. a
  future typed CSV/TOML/YAML row mapping) is a natural follow-up; deferred unless needed.
