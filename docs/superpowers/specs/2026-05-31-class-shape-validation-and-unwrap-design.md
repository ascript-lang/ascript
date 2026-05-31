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

The target one-liner this design makes real:

```javascript
let user = recover(() => User.from((await resp.json())!))?
```

### Non-goals (and why)

- **Structural `interface` types.** Would introduce a second, parallel type-declaration
  concept (structural alongside nominal) and force `check_type` to become
  environment-aware (it must look up the interface's declared fields, which live in the
  environment, not on the value). This rebuilds the validator's benefit at much higher
  conceptual + architectural cost. Rejected. The validating boundary (§4) delivers the
  same capability inside the existing nominal model.
- **User-facing generics / `resp.json<User>()`.** AScript has no user-definable type
  parameters and this design adds none. The force-unwrap operator (§5) plus `.from`
  already produce the one-liner; a class is a first-class value, so any future
  "typed parse" sugar can pass the class *as a value argument* (`resp.json(User)`)
  rather than as a type parameter. Deferred; not required.

---

## 2. Design Principles Preserved

- **Tiny core / gradual contracts.** Declared fields are checked; undeclared fields stay
  dynamic and unchecked. Existing classes are unaffected.
- **Value-introspective typing stays intact.** `check_type` remains a pure function over
  `(Value, Type)`. The only structural operation is the explicit `.from` crossing, which
  *produces* a self-describing instance; type checks downstream remain nominal.
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
  nickname?: string       // optional      — sugar for `nickname: string | nil`
  role: string = "guest"  // optional with default
  fn init(...) { ... }    // methods unchanged
}
```

Grammar (field declaration, inside a class body):

```
field_decl := Ident "?"? ":" type ("=" expression)?
```

- The `?` suffix appears **only** in field-declaration position (immediately after the
  field name, before `:`). It is unambiguous there and does not interact with the
  expression-level `?` (ternary / propagate).
- `name?: T` desugars to a field of type `T | nil`.
- A default expression makes a field optional regardless of `?`; it is evaluated lazily
  (see §3.3).

### 3.2 Data model

- **AST** (`ast.rs`, the `Class` node): add `fields: Vec<FieldDecl>` where
  `FieldDecl { name: String, ty: Type, optional: bool, default: Option<Expr>, span: Span }`.
- **Runtime** (`value.rs`, `struct Class`): add
  `fields: IndexMap<String, FieldSchema>` where
  `FieldSchema { ty: Type, optional: bool, default: Option<Expr> }`. (`optional` is
  pre-folded so the stored `ty` already includes `| nil` when `?` was used.)

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

Lexer (no new tokens), parser (class-body parsing), tree-sitter grammar
(`tree-sitter generate --abi 14`), `fmt.rs` (emit field declarations), `ast.rs` Display,
interp (store schema on `Class`; check on declared-field assignment), LSP (no semantic
change; field names become known identifiers — best-effort).

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
- **Cycles:** JSON cannot express cycles, and `.from` consumes plain data, so unbounded
  recursion is not a concern in practice. (No cycle guard in v1; revisit only if `.from`
  is ever pointed at script-constructed graphs.)

### 4.4 Error reporting

Validation panics carry a **field path** so a deep mismatch is diagnosable:
`type contract violated at user.address.zip: expected number, got string ("90210")`.
Because panics are recoverable, the whole `.from` call composes with `recover` (§6).

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

Parser (new precedence level; move `Try`; add unwrap), `ast.rs`
(`ExprKind::Unwrap` + Display), interp eval arm, `fmt.rs` `write_expr_inner` arm,
tree-sitter grammar (declare the prefix/postfix `!` interaction; regen `--abi 14`),
LSP keyword/operator awareness (best-effort). Per CLAUDE.md, every `ExprKind` addition
needs arms in interp (eval), `fmt.rs`, and `ast.rs` (Display).

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
  nickname?: string          // optional
  role: string = "guest"     // defaulted
  address: Address           // nested — User.from recurses
}

async fn loadUser(resp): Result<User> {
  // `!` promotes a JSON parse failure to a panic AND unwraps the pair;
  // `User.from` panics on a shape mismatch; `recover` catches either;
  // `?` propagates the unified error.
  return recover(() => User.from((await resp.json())!))?
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

- **Typed fields:** declared-field assignment checks (pass + violation); `?` accepts
  `nil`; default applied when absent; undeclared field stays dynamic/unchecked;
  back-compat (existing field-free classes unchanged). Unit tests in `interp.rs`.
- **`.from`:** happy path; missing required → panic; optional/defaulted handling;
  `strict` rejects/ignores extras; nested object recursion; `array<Class>` recursion;
  field-path in error message; recoverable via `recover`.
- **Postfix `!`:** unwraps `[v, nil]` → `v`; `[nil, e]!` panics carrying `e`; misuse on
  non-pair; precedence (`await f()!` ⇒ `(await f())!`); chaining; `recover` round-trips
  the original error object losslessly.
- **Grammar guardrails:** add an `examples/*.as` exercising all three; both the
  hand-written parser and the tree-sitter grammar must accept it
  (`tests/treesitter_conformance.rs`, `tests/frontend_conformance.rs`).
- **Formatter:** field declarations and `expr!` round-trip through `fmt`.
- Clippy clean under **both** `--all-targets` and `--no-default-features --all-targets`.

---

## 8. Documentation Impact

- Update the language guide: class fields (`docs/content/language/…`), the error-tier
  page (add `!` next to `?` and `recover`), and the types/contracts section.
- Update `README.md` feature list and `CLAUDE.md` (note the new `ExprKind::Unwrap` match
  obligations and the `?`/`!` precedence relationship to `await`).
- Add the end-to-end example under `examples/` (introductory) and/or
  `examples/advanced/` (HTTP + validation).

---

## 9. Open Questions / Deferrals

- **`map<K, V>` recursion in `.from`:** v1 recurses into nested *class*-typed fields and
  arrays thereof. Recursing into `map<string, SomeClass>` values is a natural extension;
  deferred unless needed.
- **`resp.json(User)` convenience:** deferred (see §1 non-goals); revisit if the
  `recover(… .from((await …)!))?` shape proves common enough to warrant sugar.
- **Field declarations as constructor surface:** this design keeps `init` and field
  declarations independent. A future "auto-`init` from declared fields" is possible but
  out of scope.
