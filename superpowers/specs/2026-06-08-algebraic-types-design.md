# AScript Algebraic Enums & Exhaustive Match — Design (ADT)

- **Status:** Draft for review
- **Date:** 2026-06-08
- **Code:** ADT (capability + correctness core of the Serious Language campaign — see `goal.md`)
- **Depends on:** **NUM** (variant payload field types use `int`/`float`/`number` per the numeric
  model; NUM must be merged first). No other dependency.
- **Depended on by:** **TYPE** (enums become a type to generalize over; generic enums `enum Box<T>` are
  a TYPE deliverable, and the representation here is built to admit type params additively); pairs with
  typed errors (a `Result`/error value can now be a payload-carrying enum).
- **Engines:** both (tree-walker oracle == VM, byte-identical — four-mode gate, `goal.md` Gate 1).
- **Breaking:** **yes, deliberately** — the enum surface is redesigned for coherence and `match` gains a
  blocking exhaustiveness diagnostic. Backward compatibility is not a goal pre-1.0 (`goal.md`). The
  `.name`/`.value` accessors of payload-less variants keep working semantically (§4); the corpus is
  *migrated*, not deleted (Gate 7).

---

## 1. Summary & motivation

AScript's enums today are **C-style constant tags**. The declaration

```javascript
enum Color { Red, Green = 2, Blue }
```

builds a `Value::Enum(Rc<EnumDef>)` whose `variants: IndexMap<String, Value>` maps each name to an
interned `Value::EnumVariant { enum_name, name, value }` (`src/value.rs:274-283`,
`src/value.rs:659-660`). A variant's only data is an optional **backing scalar** (`= 2`), exposed as
`.value`; the variant's own name is `.name` (`src/interp.rs:3578-3585`). Variants compare by `Rc`
pointer identity (`src/value.rs:719-720`). `match` already exists with a rich `Pattern` enum
(`src/ast.rs:385-409`) — wildcard, ident (Option-C bind/compare), value, range, array, object — but the
no-arm case is a **runtime** panic on both engines (`src/interp.rs:3137`, VM `Op::MatchNoArm`
`src/compile/mod.rs:3553`), never a compile error.

This is the single largest correctness-per-effort gap left in the core language:

- **Variants cannot carry data.** A real `Result`, an AST node, a parser token, a state-machine event —
  every sum type with *fields* — is impossible. You reach for tagged Objects (`{__kind: "circle",
  radius}`), which the schema/workflow subsystems already do as a workaround, losing identity,
  exhaustiveness, and any type help.
- **`match` is not exhaustive.** Adding a `Color.Cyan` variant silently leaves every existing `match
  c { … }` falling into a wildcard or panicking at runtime — the classic "forgot a case" bug that Rust
  and Swift turn into a compile error. This is the highest-value static check we don't yet have.

This spec makes enums **algebraic** (variants carry typed payloads — positional or named) and makes
`match` over an enum-typed subject **exhaustiveness-checked**: a missing variant with no wildcard is a
new **blocking** diagnostic (`non-exhaustive-match`, default **Error** — a correctness gate, not a
lint). It reuses the existing `match` engine wholesale — the work is in the *pattern* surface
(variant-destructuring), the *value* representation (payload-carrying `EnumVariant`), and a *static
analysis* (exhaustiveness). Generics on enums are deferred to TYPE, but the representation is laid out
so a `Vec<Type>` of type params can be added later without a value-layout change (§10).

### Two design conflicts resolved up front

1. **`.value` of payload-less variants must keep working.** The simple enum `Color.Red.value`
   (backing scalar, defaulting to `Nil`) is load-bearing in the corpus and the existing tests. The
   redesign keeps the **backing-scalar** concept for `= expr` variants and the `.name`/`.value`
   accessors for **payload-less** variants exactly as today; payload-carrying variants are a *new,
   orthogonal* axis (a variant has EITHER a `= scalar` backing OR a `(…payload…)` constructor, never
   both — §3.4). No accessor collision (§4).
2. **A constructed variant is NOT a new top-level `Value` kind.** `Shape.Circle(2.0)` produces a
   `Value::EnumVariant` carrying payload data, reusing the existing variant kind and its
   identity/equality machinery — *not* a 17th value tag. This keeps `Value` small (the VAL pillar) and
   means every existing `EnumVariant` arm (display, equality, worker-serialize, `.aso`, GC) extends
   rather than forks (§5, justified in §10).

## 2. The model: variants, payloads, exhaustiveness

An **enum** is a closed sum of named **variants**. Each variant is one of three shapes:

| Shape | Declaration | Construction | A constructed value is |
|---|---|---|---|
| **Unit** (payload-less) | `Point` or `Red = 2` | `Shape.Point` (the interned variant itself) | the **interned** variant `Value::EnumVariant` (identity-equal, as today) |
| **Positional payload** | `Pair(int, int)` | `Shape.Pair(3, 4)` | a **fresh** `Value::EnumVariant` carrying `[3, 4]` |
| **Named payload** | `Circle(radius: float)` | `Shape.Circle(2.0)` | a **fresh** `Value::EnumVariant` carrying `{radius: 2.0}` |

- A **unit variant** is exactly today's interned constant — `Shape.Point == Shape.Point` by identity,
  zero allocation per use. (A `= expr` *scalar backing* — `Red = 2` — is a unit variant with a non-`Nil`
  `.value`; this is the only form the simple enum had.)
- A **payload variant** is a one-argument-list **constructor**: referencing `Shape.Circle` (without a
  call) yields a *constructor* value; **calling** it (`Shape.Circle(2.0)`) validates the payload arity +
  field types (per NUM) and produces a constructed `EnumVariant`. Two constructed variants are equal iff
  same enum, same variant name, and **structurally equal payloads** (§5.2) — *not* identity (so
  `Shape.Circle(2.0) == Shape.Circle(2.0)` is `true`).
- **Exhaustiveness:** a `match` whose subject is statically known to be a specific enum `E` must handle
  **every** variant of `E` — by naming each variant (`Circle(_) => …`) or by a catch-all (a `_`
  wildcard, or a bare binding identifier `other => …`). Otherwise → `non-exhaustive-match` (Error),
  listing the missing variants. When the subject's enum type **cannot be proven** (gradual / untyped),
  the check stays **silent** — exactly the gradual gate that keeps `examples/**` at zero false positives
  (Gate 5). **CST-nesting caveat (load-bearing):** the CST front-end nests only the FIRST `match` arm
  under the `MatchExpr` node — every subsequent arm is a *sibling statement* in the enclosing block
  (`src/check/infer/pass.rs:949-951` doc-comment; `synth_match` at `:952` iterates only
  `expr.children().filter(MatchArm)`, so it sees one arm). A naive exhaustiveness pass that enumerates
  `MatchExpr` children therefore sees a single arm and flags **every** multi-arm `match` as
  non-exhaustive — a Gate-5 false-positive flood. The analysis MUST instead gather ALL arms across the
  sibling chain, exactly as `walk_stmts` already does when it visits the trailing sibling arms (§7.3
  specifies the gather precisely). This is cross-cutting finding #3, owned here for ADT.

## 3. Surface syntax & semantics

### 3.1 Declaration

```javascript
enum Shape {
  Circle(radius: float),      // named payload (single field)
  Rect(w: float, h: float),   // named payload (multiple fields)
  Pair(int, int),             // positional payload (unnamed fields)
  Point,                      // unit variant (payload-less, as today)
}

enum Status { Active, Inactive = 0, Pending = 1 }   // scalar-backed units (unchanged)
```

- A variant is `Name`, `Name = scalarExpr` (unit, scalar-backed — unchanged), or
  `Name(field, field, …)` (payload). Within one variant's parens, the fields are **uniformly** named
  (`id: T`) **or** uniformly positional (`T`) — **mixing is a parse error** (`enum variant fields must
  be all named or all positional`). Field types use NUM type names (`int`/`float`/`number`/`string`/…,
  including `T?` and nested containers); a field type is **required** in a payload variant (no untyped
  payload field — the payload is the typed core of the feature).
- Variants are comma-delimited, trailing comma allowed (unchanged; `;` is **not** a variant separator —
  enums are comma-lists per `CLAUDE.md` "`;` separators").
- A variant may have **either** a `= scalar` backing **or** a `(…)` payload, never both
  (`a variant cannot have both a '= value' backing and a '(…)' payload`).

### 3.2 Construction & the constructor value

- **Unit:** `Shape.Point` reads the interned variant (member access on the `Value::Enum`, the existing
  `read_member` enum arm, `src/interp.rs:3575`).
- **Payload:** `Shape.Circle` reads a **variant constructor** (also a `Value::EnumVariant`, but flagged
  as an unsaturated constructor — §5.1). **Calling** it constructs:
  - positional: `Shape.Pair(3, 4)` — arity-checked against the declared field count; each arg
    type-checked against the declared field type (the same `validate_into` field-coercion path classes
    use, `src/interp.rs`); arity mismatch / type mismatch → recoverable Tier-2 panic with the
    enum-variant-field path (`Shape.Pair expects 2 fields, got 1` / `Shape.Circle.radius: expected
    float, got string`).
  - named: `Shape.Circle(2.0)` for single-field; `Shape.Rect(w: 3.0, h: 4.0)` for multi-field, using
    **named call arguments**. For a single named field, a positional call (`Shape.Circle(2.0)`) is also
    accepted (the one-field convenience); multi-field named variants **require named args** (avoids
    positional ambiguity), a parse/eval error otherwise (`Shape.Rect requires named fields (w:, h:)`).
- Calling a **unit** variant (`Shape.Point()`) is an error (`Shape.Point is a unit variant and takes no
  payload`). Referencing a payload variant **without calling** it in value position yields the
  constructor (first-class: `let mk = Shape.Circle; mk(2.0)` works, and `array.map(radii, Shape.Circle)`
  is the motivating ergonomic).

### 3.3 Pattern matching (variant-destructuring)

```javascript
fn area(s: Shape): float {
  return match s {
    Circle(r) => 3.14159 * r * r,          // positional bind of the single field
    Rect(w, h) => w * h,                    // positional bind of both fields
    Pair(a, b) => float(a) * float(b),      // positional payload
    Point => 0.0,                           // unit variant
  }
}
```

- **Variant patterns** name the variant and (for payload variants) **destructure** the payload:
  - `Point` — a unit-variant pattern (matches the interned variant). Today this is a
    `Pattern::Value(Color.Red)` — an enum *member reference* compared with `==`. **Unchanged** for unit
    variants: `Point` / `Shape.Point` flows through the existing value-pattern path.
  - `Circle(r)` / `Rect(w, h)` / `Pair(a, b)` — a **new** `Pattern::Variant`: matches when the subject
    is an `EnumVariant` of that variant, then binds each sub-pattern against the payload (positional by
    index, or named by field). Sub-patterns are full patterns (nesting works:
    `Circle(0.0) => …` matches a literal radius; `Pair(a, b) if a == b => …` with a guard).
  - **Named destructuring:** `Circle(radius)` (the field name binds it) and `Rect(w: ww, h: hh)`
    (rename) — named variant patterns mirror object patterns: `{field}` binds by name,
    `{field: subpat}` matches a sub-pattern. Positional `Rect(a, b)` binds the fields **in declaration
    order** by position (this is the convenience the examples use).
- **Bare vs qualified — the Option-C gap (must be tightened, NOT hand-waved):** both `Circle(r)` and
  `Shape.Circle(r)` are accepted for **payload** patterns (the trailing `(…)` makes them unambiguously a
  `Pattern::Variant`, never an Option-C ident). The hard case is a **bare unit** pattern `Point`
  (no parens) colliding with an Option-C *binding* identifier. The runtime `match_pattern`
  `Pattern::Ident` arm uses `env.get(name)` with **no subject-type knowledge whatsoever**
  (`src/interp.rs:3269-3277`): a bare `Point` *compares* only if `Point` is a name **defined in the
  current scope** (e.g. the enum variant was hoisted/imported into scope), and otherwise **binds** the
  subject. There is no path by which the runtime consults "the subject is a `Shape`, so `Point` means
  `Shape.Point`." Therefore the earlier "zero new ambiguity" claim is **withdrawn** — a bare unit
  variant genuinely can be silently captured as a binding. **Decision (chosen):** for the
  exhaustiveness-checked path, a unit variant in an exhaustiveness-relevant `match` must be written
  **qualified** (`Shape.Point`) OR the *checker* diagnoses a bare `Ident` that collides with a
  known-subject variant name. Concretely:
  - **Runtime** is unchanged (`env.get`-based bind/compare, byte-identical on both engines) — no
    subject-type lookup is added to the hot path.
  - **Checker** (§7.3): when the subject resolves to a concrete `Enum(E)` and an arm is a bare
    `Pattern::Ident(n)` where `n` is a variant name of `E` **but** `n` is NOT a binding the resolver
    bound in that scope (i.e. the runtime would *bind*, not *compare*), emit a default-Warning
    `enum-variant-binding-shadow` ("`Point` here binds the subject; write `Shape.Point` to match the
    variant"). This converts the silent footgun into an author-time diagnostic AND keeps the
    exhaustiveness counter honest: such a bare binding is treated as a **catch-all** (it always
    matches), exactly mirroring the runtime — so it is never miscounted as covering `Point`.
  - The fully-qualified `Shape.Point` and the parenthesized payload forms are unaffected; §7 narrowing
    makes the enum-typed case precise.
- **Or-patterns + guards** compose unchanged: `Circle(_) | Rect(_, _) => "round-ish"`,
  `Pair(a, b) if a > b => …`.

### 3.4 Reflection on a constructed variant

- `.name` → the variant name string (`"Circle"`) — **unchanged** (`src/interp.rs:3579`).
- `.value` → for a **unit** variant, the backing scalar or `Nil` (**unchanged**,
  `src/interp.rs:3580`). For a **payload** variant, `.value` returns the payload as **data**: an
  `Object` (named payload, `{radius: 2.0}`) or an `Array` (positional payload, `[3, 4]`). This is the
  one *semantic addition* to `.value`, and it is additive: payload-less variants are byte-identical to
  today. **The positional `.value` Array is stored on the constructed variant, not freshly allocated per
  access** — a `Payload::Positional(Vec<Value>)` already holds the elements, so `.value` returns the
  same `Array` handle (an `Rc` clone) each read, giving stable identity (`v.value is v.value`) and O(1)
  access; likewise the `Named` payload's `.value` returns the stored `Cc<ObjectCell>` Object. (Equality
  of two such Arrays/Objects is structural, §5.2, so a stable handle vs a fresh one is observably
  equal — but a stable handle avoids per-access allocation and matches Array/Object `.value` reflection
  elsewhere.)
- Named-payload field access sugar: `c.radius` on a `Circle` reads the named field directly (an
  `EnumVariant` member-read arm extension); positional payloads have no field names so use `.value[0]`
  or destructuring. (Field-access sugar is convenience; pattern destructuring is the primary path.)

### 3.5 Examples

```javascript
enum Json {
  Null,
  Bool(value: bool),
  Num(value: float),
  Str(value: string),
  Arr(items: array<Json>),      // recursive — payload references the enum itself
}

fn render(j: Json): string {
  return match j {
    Null => "null",
    Bool(b) => str(b),
    Num(n) => str(n),
    Str(s) => json.stringify(s),
    Arr(xs) => "[" + array.join(array.map(xs, render), ",") + "]",
    // no `_`: if a variant is added, this match is a compile error until handled.
    // (This "missing-arm ⇒ compile error" is an EXERCISED test, not just this comment —
    //  see §12.1 "Exhaustiveness as an EXERCISED `check` failure".)
  }
}

// typed-error enum (typed-errors synergy, §8):
enum DbError { NotFound(key: string), Timeout(ms: int), Conn(detail: string) }
fn lookup(k: string): [string, DbError] {
  if (!store.has(k)) { return [nil, DbError.NotFound(k)] }   // `?`/`!` unchanged
  return [store.get(k), nil]
}
```

## 4. The accessor-compatibility contract (the `.value` redesign)

The single behavioral subtlety. Stated as a contract so the migration is mechanical:

| Variant shape | `.name` | `.value` | `==` | Construction |
|---|---|---|---|---|
| `Red` (bare unit) | `"Red"` | `Nil` | identity | `Color.Red` |
| `Green = 2` (scalar unit) | `"Green"` | `2` (int per NUM) | identity | `Color.Green` |
| `Circle(radius: float)` (named) | `"Circle"` | `{radius: 2.0}` (Object) | structural | `Shape.Circle(2.0)` |
| `Pair(int, int)` (positional) | `"Pair"` | `[3, 4]` (Array) | structural | `Shape.Pair(3, 4)` |

**Locked:** unit variants (rows 1–2) are **byte-identical to today** — same interning, same identity
equality, same `.name`/`.value`. Payload variants (rows 3–4) are the new axis; their `.value` is the
payload-as-data. Nothing that worked before changes meaning; the redesign is **purely additive at the
value level** and the surface change is the new `(…)` declaration/construction/pattern form.

## 5. Representation & GC

### 5.1 `Value::EnumVariant` payload

Extend the existing `EnumVariant` struct (`src/value.rs:279-283`) — **no new `Value` variant**:

```rust
// Value::EnumVariant stays Rc<EnumVariant> (the WRAPPER is not Cc — §5.3 decision):
pub struct EnumVariant {
    pub enum_name: String,
    pub name: String,
    pub value: Value,             // unit backing scalar (or Nil) — UNCHANGED
    pub payload: Option<Payload>, // None = unit / constructor; Some = constructed payload variant
    pub ctor: bool,               // true = unsaturated constructor (Shape.Circle, not called yet)
}

pub enum Payload {
    // The cycle-capable part: these containers are GC-traced (§5.3), the Rc wrapper is not Cc.
    Positional(Vec<Value>),
    Named(Cc<ObjectCell>),        // reuse the shape/Object machinery (§5.3)
}
```

- A **unit** variant has `payload: None, ctor: false` — the interned constant, exactly as today.
- A **constructor** (referencing `Shape.Circle`) has `payload: None, ctor: true` — carries the
  *declared field schema* (arity + field names + field types) so a call can validate. The schema is
  looked up on the owning `EnumDef` by variant name (stored once per variant, not per constructor), so
  the constructor value stays cheap.
- A **constructed** variant has `payload: Some(Positional | Named), ctor: false`.

`EnumDef` gains the per-variant schema:

```rust
pub struct EnumDef {
    pub name: String,
    pub variants: IndexMap<String, Value>,   // interned UNIT/constructor values — UNCHANGED
    pub variant_schemas: IndexMap<String, VariantSchema>,  // arity, field names, field types
}
pub struct VariantSchema {
    pub fields: Vec<(Option<Rc<str>>, Type)>,  // name (None = positional), declared type
}
```

The full ordered variant list (`variant_schemas.keys()`) is what the exhaustiveness checker enumerates
(§7) and what worker-serialize / `.aso` reconstruct against.

### 5.2 Equality, identity & hashing

- **Unit / constructor variants:** `Rc::ptr_eq` (unchanged, `src/value.rs:719-720`) — interned, so
  identity-equal. **Identity is per-isolate.** The "unit variants are byte-identical, identity-equal"
  guarantee is scoped to **within a single isolate** (a single `Vm`/`Interp`): interning is per-`Vm`
  (§9), so two unit variants compare `true` only because they are the same interned `Rc`. This is
  *not* automatically preserved across a worker boundary — see §6, which makes far-side re-interning a
  NEW, explicit requirement (today the decoder builds a fresh `Rc`, so a unit variant that crosses a
  worker boundary fails `==` against the far isolate's interned constant).
- **Constructed payload variants:** **structural** — equal iff same `enum_name`, same `name`, and
  payloads equal element-wise (positional) or key-wise (named) using the existing `Value` `PartialEq`.
  This is the only change to the `EnumVariant` equality arm (a `payload.is_some()` branch). Justified:
  `Shape.Circle(2.0) == Shape.Circle(2.0)` must be `true` for `match`-by-value and for using a
  constructed variant as data; identity would make every construction a distinct object (a footgun).
- **Map keys / hashing:** constructed variants with payloads are **not** hashable as `MapKey` in v1
  (like `Array`/`Map`/`Set` — identity-only containers; a payload variant used as a Map key is the same
  Tier-2 panic those produce). Unit variants remain hashable exactly as today (by interned identity).
  (Hashable structural payload keys are a possible additive future, deferred — §10.)

### 5.3 GC `Trace` (the load-bearing invariant)

Payload values are reachable GC roots and **must be traced** (`goal.md` Gate 4 / `CLAUDE.md`
"Cycle-collecting GC"). Today `Value::EnumVariant` is `Rc<EnumVariant>` with a no-op `Trace` (the
backing `value` is a scalar and the variant is acyclic). With payloads, a variant can hold containers —
and the `Json::Arr(items: array<Json>)` example shows a variant payload can form a **cycle**. Therefore:

**Decision: keep unit-variant construction `Rc`-cheap; pay `Cc` only for a payload.** A uniform
`Cc<EnumVariant>` would register **every** unit variant (including the interned constants, allocated
once per declaration, and every `Color.Red` *use* that clones the `Rc`) with the Bacon–Rajan cycle
collector — a per-construction cost on the most common, provably-acyclic case, for no benefit (a unit
variant can never form a cycle). The review flags this; rather than add a Gate-12 benchmark to justify a
regression, ADT **avoids** it:

- The value layout becomes **`Value::EnumVariant(Rc<EnumVariant>)` UNCHANGED for unit/constructor
  variants** (no `Cc`, no cycle-collector registration — byte-identical to today, interning preserved),
  and the **payload** lives behind a separately-collected handle: `Payload::Positional(Vec<Value>)` /
  `Payload::Named(Cc<ObjectCell>)` where the *payload containers* are the cycle-capable part. The only
  cycle-forming path (`Json.Arr(items)` containing itself) runs **through** the payload's `Vec`/
  `ObjectCell`, which are GC-traced exactly as a free `Array`/`Object` is. So the `EnumVariant` wrapper
  stays `Rc` with a `Trace` impl that traces `value` + (when `Some`) the payload elements — and a unit
  variant traces a scalar `value` + `None` payload (cheap, no registration churn).
- **Construction sites to update (enumerated, the `Rc→…` blast radius):** the `Value::EnumVariant`
  constructions are `src/value.rs:660` (the variant type), the interning loop in `Stmt::Enum` eval
  (`src/interp.rs:2750`, builds the per-variant interned constants), the worker decoder
  `src/worker/serialize.rs:624` and the encode-side test fixture `:897`, plus the NEW
  construction-call path (`call_value` validating `Shape.Circle(2.0)`) and the VM mirror. Because we
  keep the wrapper on `Rc`, these sites are **edited in place** (add the `payload`/`ctor` fields), not
  re-typed to `Cc` — far smaller churn than a uniform `Cc` switch, and the interned-constant sites stay
  registration-free.
- **`Trace` mirror:** add `EnumVariant` to the `Value::trace` container set (`CLAUDE.md` "When adding a
  cycle-capable `Value` container, mirror it in `Value::trace`") so the collector reaches a payload's
  elements; a `payload: None` variant traces nothing cyclic. **Doc updates required:** the
  `src/gc.rs:34` doc-comment currently lists `EnumVariant` among "immutable / acyclic … stay on `Rc`"
  — reword to "the *wrapper* stays on `Rc`; a `Some(payload)` is traced into (it can hold cycle-capable
  containers)". The `CLAUDE.md` "Values" `EnumVariant` paragraph gets the same correction.
- **Gate 12 note:** because unit-variant construction is unchanged (`Rc`, no collector registration),
  there is **no** steady-state benchmark obligation for the common path; the only new allocation is a
  `Vec`/`Cc<ObjectCell>` per *payload* construction (proportional to the data, unavoidable). A
  micro-benchmark over unit-variant-heavy `match` (`examples/oop.as`-shape) is added only to *prove the
  no-regression*, not to license one.
- Native-resource opacity is unaffected (a payload can never legally contain a `Native` handle — it is
  built from sendable/serializable data; a payload field typed to hold one would be rejected at
  construction the same way worker-serialize rejects it, §6).

## 6. Worker serialize (payload-carrying `EnumVariant` wire format)

`src/worker/serialize.rs` already has an `EnumVariant` wire tag (tag `10`:
`enum_name + variant_name + backing value`, `src/worker/serialize.rs:18`, `:224`, `:466`, `:624`). The
structured-clone airlock (workers Spec A §5) must round-trip the payload.

**Far-side re-interning is NEW work, not existing (own it).** The current TAG_ENUM decoder builds a
**fresh** `Value::EnumVariant(Rc::new(EnumVariant{enum_name, name, value}))` with **no `EnumDef`
lookup** (`src/worker/serialize.rs:620-630`; the encode side that the round-trip test exercises is at
`:897`). Consequently, **today** a unit variant that crosses a worker boundary already fails `==`
against the far isolate's interned constant by `Rc` identity — i.e. the current behavior is a latent
bug the moment any code compares a received variant against `Shape.Point`. ADT must therefore add
far-side re-interning as a stated requirement, and the equality contract for "unit variants are
identity-equal" is **scoped to within-isolate** (§5.2). The chosen cross-isolate semantics:

- **Unit variant decode** looks the variant up on the **far-side `EnumDef`** (guaranteed present by
  code-shipping — the worker closure ships enums as value consts; the global-name fixpoint at
  `src/worker/dispatch.rs:285` already classifies `TopDef::Class`/`Const` and walks `GET_GLOBAL` names
  recursively, so the enum's interned constants are in scope on the far side) and returns **that
  isolate's interned constant**, so `received == Shape.Point` is `true` within the receiver. If the
  far-side `EnumDef` is somehow absent, the decoder falls back to a fresh `Rc` (today's behavior) rather
  than erroring — a documented best-effort floor, surfaced by the round-trip test below.
- **Payload variant decode** reconstructs a **fresh** constructed `EnumVariant` with the cloned payload
  (payload variants compare **structurally**, §5.2, so a fresh allocation is correct and re-interning is
  neither needed nor possible).

- **Wire format (tag 10 extended):** `enum_name(str) + variant_name(str) + backing value + payload_tag`
  where `payload_tag` is `0` (unit — the old format, value-identical), `1` (positional: `len` +
  elements), or `2` (named: it is an `Object`, reuse the Object serializer).
- **Cycles:** payload variants participate in the visited-reference table exactly like `Array`/`Object`
  (workers Spec A §5 "cycles are handled") — a recursive `Json::Arr` payload that contains itself
  serializes once and refers by id. This is mandatory, not optional.
- **Sendability:** a payload that (illegally) contains a non-sendable kind
  (`Function`/`Native`/`Future`/`Generator`) is the existing recoverable Tier-2 path-error
  (`value of kind <Kind> cannot be sent to a worker at <path>`), with the path extended through the
  payload (`arg[0].payload.items[2]`). A round-trip test covers a payload variant incl. as a nested
  field and incl. a cyclic recursive payload.
- **Cross-isolate equality test (NEW requirement, asserts the chosen semantics):** a unit variant sent
  to a worker and compared on the far side against that isolate's own `Shape.Point` literal asserts
  `==` is `true` (re-interning succeeded); a payload variant sent and compared against a freshly
  far-side-constructed `Shape.Circle(2.0)` asserts `==` is `true` by **structural** equality. The test
  also pins the best-effort fallback (absent far-side `EnumDef`) so a regression in code-shipping is
  caught, not silently downgraded to identity inequality.

## 7. Type-system integration

### 7.1 Runtime contracts (`src/ast.rs` `Type`)

- The enum **name is already a usable type** in annotations (`c: Color`). Payload **field types** in a
  variant declaration are ordinary `Type`s (per NUM: `int`/`float`/`number`/…/`T?`/containers), stored
  in `VariantSchema`. `check_type` for `: Shape` accepts any `EnumVariant` whose `enum_name` matches
  (unchanged shape, extended to ignore the payload — a `Circle(2.0)` *is a* `Shape`).
- Construction-site validation (§3.2) reuses the `validate_into` field-coercion path (the same engine
  `Class.from` and typed-parse use, `CLAUDE.md` "Nullable `T?` + typed class fields"): each payload arg
  is checked against its declared field `Type`; a mismatch is the byte-identical recoverable field-path
  panic. This is a **runtime** check on both engines (no `init`, like `validate_into`).

### 7.2 Static checker (`src/check/infer/`) — types + narrowing

The static checker already models enums: `CheckTy::Enum(EnumId)` and `CheckTy::EnumVariant(EnumId,
name)` (`src/check/infer/ty.rs:66-68`), with variant→enum widening (`ty.rs:190`), the `Table` enum
registry (`src/check/infer/table.rs:29-32, 69-74, 250-253`), and member-access synth of a variant
(`src/check/infer/pass.rs:1033-1036`). ADT extends this:

- **`EnumInfo`** (`table.rs:29`) gains per-variant arity + field types (parsed from the `EnumVariant`
  CST node's new payload fields). `enum_variants(node)` (`table.rs:250-253`) is extended to also collect
  field schemas. This is what powers both construction-synth and exhaustiveness.
- **Construction synth:** `Shape.Circle(2.0)` synths `CheckTy::EnumVariant(Shape, "Circle")` (an
  artifact that widens to `Enum(Shape)`); a provably-wrong payload arg type against the declared field
  type is a `type-mismatch` (the existing annotated-slot code, extended to variant fields) — but
  **only** when provable (gradual: an `Unknown` arg stays silent, Gate 5).
- **Relationship to the existing `unknown-enum-variant` rule (stated — they are orthogonal):** the
  current `unknown-enum-variant` lint (registered at `src/check/config.rs:43`, implemented in
  `src/check/rules/unknown_enum_variant.rs`) flags **member access** of a name that is not a variant on
  a statically-known, uniquely-declared enum receiver (`Shape.Nope` → "enum has no variant"). ADT keeps
  it as-is and **extends** it to two new surfaces, since the same "is this a real variant of `E`?"
  check applies: (a) a **payload constructor call** `Shape.Nope(…)` — the receiver/member resolution is
  identical, the trailing call does not change variant-existence checking; (b) a **qualified variant
  pattern** `Shape.Nope(r)` in pattern position. A **bare** variant pattern (`Nope(r)`) is NOT covered
  by this rule (no enum receiver to resolve against) — it surfaces instead through exhaustiveness /
  the `enum-variant-binding-shadow` diagnostic (§3.3) when the subject type is known, and is otherwise
  a runtime no-arm fall-through (gradual-silent). The rule stays **conservative** (it already skips any
  shadowed/reassigned/ambiguous receiver), so the extension adds no false positives.
- **Per-variant narrowing in match arms:** a `Circle(r) => …` arm narrows the subject to
  `EnumVariant(Shape, "Circle")` within the arm, and binds `r` to the field's declared type (`float`).
  This reuses the existing match/instanceof/nil-guard narrowing in `pass.rs` — the arm body sees `r:
  float`, so downstream arithmetic/`possibly-nil` reasoning is precise. (This is also what lets the
  arm-body `r * r` be `float` arithmetic per NUM with no false `type-error`.)

### 7.3 Exhaustiveness analysis (the new blocking diagnostic)

A new analysis in the inference pass (`src/check/infer/pass.rs`, wired after the existing per-arm
synth), emitting **`non-exhaustive-match`** (default **Error** — `goal.md` "a missing variant is a
compile error", a correctness gate):

- **Arm gathering — gather the FULL sibling chain, not just `MatchExpr` children (must-fix, the Gate-5
  tripwire).** The CST nests only the **first** `match` arm under the `MatchExpr` node; every subsequent
  arm is a **sibling statement** in the enclosing block (`src/check/infer/pass.rs:949-951`; `synth_match`
  at `:952` deliberately iterates only `expr.children().filter(MatchArm)` and stays `Any` on the rest —
  fine for narrowing, **fatal** for exhaustiveness, which would see one arm and flag every multi-arm
  `match` as non-exhaustive). The exhaustiveness pass MUST therefore collect arms the way `walk_stmts`
  reaches them: the `MatchArm` directly under `MatchExpr` **plus** the trailing run of sibling
  `MatchArm` statements that follow the `MatchExpr` in its parent block (the contiguous sibling chain
  belonging to this match), in source order. (Implementation: walk forward from the `MatchExpr`'s
  position in its parent's child list, consuming consecutive `MatchArm` siblings.) If the CST is instead
  *fixed* to nest all arms, the analysis enumerates `MatchExpr` children directly and this gather is a
  no-op — the spec mandates the **behavior** (all arms counted), and the gather is the
  no-CST-change route. **Required Gate-5 tests:** `cargo run -- check examples/oop.as` and
  `examples/all_features.as` emit **zero** `non-exhaustive-match` — both contain a multi-arm `match`
  whose only catch-all is a trailing `_` arm that is a *sibling* of the `MatchExpr`
  (`examples/oop.as:29-32`, `examples/all_features.as:149-152, 161-166, 179-182`); if the gather is
  wrong, the trailing `_` is invisible and these flood. These two are added to `tests/check.rs` as
  explicit zero-diagnostic assertions (not only via the blanket `examples/**` sweep, so a regression
  names the file).
- **Trigger (gradual):** runs **only** when the `match` subject's type resolves to a concrete
  `CheckTy::Enum(E)` (or `EnumVariant(E,_)`). If the subject type is `Any`/`Unknown`/non-enum, the
  analysis is **silent** — the gradual escape that keeps `examples/**` at zero false positives (Gate 5).
  This is the same `Compat3::Unknown ⇒ silent` discipline the rest of the checker uses.
- **Coverage computation:** collect the set of variant names handled by the arms. An arm covers variant
  `V` if it is a variant pattern `V(…)` / bare/qualified unit `V` / `E.V`, OR if it is a **catch-all** —
  a `Pattern::Wildcard` (`_`) or a bare **binding** `Pattern::Ident` that is *not* a defined variant
  (Option-C bind), or an or-pattern alternative that is a catch-all. A **guarded** arm (`V(x) if …`)
  does **not** count as covering `V` (the guard may fail), unless another unguarded arm also covers it —
  matching Rust's rule. (Value-equality arms like `Circle(2.0)` likewise don't fully cover `Circle`.)
- **Diagnostic:** if any variant of `E` is uncovered and there is no catch-all →
  `non-exhaustive-match: match on enum 'Shape' does not cover: Rect, Point` (the missing names, ordered,
  caret on the `match` keyword). A **redundant** arm after a catch-all is a *separate, default-Warning*
  `unreachable-match-arm` (additive, secondary — not required for the correctness gate).
- **Interaction with the runtime panic:** the runtime `MatchNoArm` / `no matching arm` panic
  (`src/interp.rs:3137`, `Op::MatchNoArm`) is **retained unchanged** — it is the dynamic backstop for
  the gradual (unproven-subject) case and for guarded-only coverage. Exhaustiveness is a *static*
  guarantee layered on top; the engines are untouched (so the four-mode differential is unaffected,
  §9). This is the key architectural decision: **exhaustiveness adds a checker analysis, not an engine
  change.**

### 7.4 Generics-readiness (TYPE forward-compat, deferred)

`VariantSchema.fields` is a `Vec<(Option<Rc<str>>, Type)>` and `EnumInfo` is keyed by `EnumId`; adding
enum type params (`enum Option<T> { Some(value: T), None }`) is then a matter of TYPE introducing a
`Type::Param`/`CheckTy::Var` and substituting at construction/narrowing — **no value-layout change** and
no change to the wire/`.aso` format (a constructed variant always carries *concrete* payload values).
ADT must **not** hardcode anything that blocks this: the field-type slot is a full `Type`, and the
representation stores values, not types. (Parsing `enum Name<T> { … }` is out of scope for ADT; the
grammar leaves room for an optional type-param list, declared but unused — a TYPE deliverable.)

## 8. Typed-errors synergy (`?` / `!` unchanged)

A `Result` in AScript is the `[value, err]` pair convention (`CLAUDE.md` "`?` is overloaded"). ADT
makes the **`err` slot a payload-carrying enum** the natural way to model typed errors
(`DbError.NotFound(key)`, §3.5) — strictly better than a bare string:

- **`?` (propagate, `ExprKind::Try`) is unchanged:** it inspects the pair shape (`[value, err]`), not
  the error's *kind*; an enum error rides the `err` slot as ordinary data. `let v = lookup(k)?` propagates
  the `DbError` enum value untouched.
- **`!` (unwrap, `ExprKind::Unwrap`) is unchanged:** force-unwrap yields the value or a recoverable
  panic with the original message; the enum error's `.name`/payload can feed the message but the unwrap
  path itself is byte-identical.
- The caller then **`match`es the error enum exhaustively** (`match err { NotFound(k) => …, Timeout(ms)
  => …, Conn(d) => … }`) — exhaustiveness now guarantees every error case is handled. This is the whole
  point of the synergy: typed errors + exhaustive match = no silently-dropped error case. **No new `?`/
  `!` semantics, no grammar change to propagation** — purely a usage pattern the new representation
  enables.

## 9. Determinism & the four-mode differential

- The feature lives at the `Value`/`Interp`/compiler layer both engines share. Construction, payload
  equality, pattern destructuring, and `.value`/field reflection are pure and identical on tree-walker,
  specialized VM, generic VM, and `.aso`-compiled — **`tree-walker == specialized == generic ==
  .aso`** byte-identical holds by construction (`goal.md` Gate 1). The exhaustiveness check is
  **static-only** (it runs no code, like SP10) → `vm_differential` is unchanged by it.
- **Determinism (SP9) unaffected:** no clock/RNG seam; variant construction and payload matching are
  deterministic. Variant interning is per-`Vm`/per-`Interp` (as today), so identity is stable within an
  isolate and reconstructed across the worker boundary by name (§6), never compared cross-isolate by
  pointer.
- **Pattern-match compile path:** `Pattern::Variant` lowers to a tag-test (enum name + variant name
  equality against the subject) followed by payload sub-pattern tests, in `compile_pattern_test`
  (`src/compile/mod.rs:3596`) — built to be byte-identical to the tree-walker `match_pattern` Variant
  arm (the existing `compile_match` doc-comment, `src/compile/mod.rs:3475`, demands BYTE-FOR-BYTE
  parity). A new VM op is **not** required: the variant tag-test is `read_member`-style equality + the
  existing destructure into fail-jump sites; if a fused `Op::MatchVariant` proves cleaner it is additive
  and both paths stay differential-checked (the three-way guard, `CLAUDE.md` "--no-specialize kill
  switch"). No `.aso` opcode change beyond what §11 records.

## 10. Scope & rejected alternatives

**In scope:** payload-carrying variants (positional + named); the variant-constructor value
(first-class, callable, arity/type-validated); `Pattern::Variant` destructuring (positional + named +
nested + guarded + or-patterns) across both parsers + tree-sitter + interp + fmt + ast Display; the
payload-extended `EnumVariant` representation + GC `Trace` + structural payload equality; worker-wire +
`.aso` payload serialization; the `non-exhaustive-match` blocking checker analysis + per-variant
narrowing + construction-site type synth; the `.value`-compat contract; typed-errors usage; full corpus
migration + new examples; docs.

**Out of scope / deferred (reserved, not dropped — `goal.md` no-silent-deferral):**
- **Generic enums (`enum Option<T> { … }`).** A **TYPE** deliverable. ADT lays the representation +
  schema out to admit it additively (§7.4); ADT ships *monomorphic* algebraic enums.
- **Hashable structural payload variants as Map keys.** v1 treats payload variants as identity-style
  containers (not `MapKey`-hashable), matching `Array`/`Map`. A future additive `MapKey::EnumVariant`
  (structural) is possible; deferred to avoid expanding the hashing invariant now.
- **Methods on enums / `impl`-style variant methods.** Out of scope (a class-vs-enum boundary the
  campaign does not blur in ADT); a free function over a `match` is the idiom.
- **Exhaustiveness over non-enum closed sets** (e.g. boolean `match`, string-literal unions). Deferred;
  the analysis fires only on concrete enum subjects.

**Rejected:**
- **Keep simple-enums-only (the current spec non-goal).** The original enum design explicitly limited a
  variant's backing to a scalar; that non-goal is **now reversed** by this campaign (`goal.md` pillar 3:
  "algebraic enums + exhaustive match"). Tagged-Object workarounds (`{__kind, …}`) lose identity,
  exhaustiveness, and type help — the schema/workflow subsystems already pay this cost; ADT removes the
  need.
- **A new top-level `Value` kind for constructed variants.** Rejected: it would fork every existing
  `EnumVariant` arm (display/equality/worker/`.aso`/GC) and grow `Value` against the VAL pillar. An
  `EnumVariant`-with-optional-payload is strictly smaller blast radius (§5) and keeps unit variants
  byte-identical.
- **Runtime-only exhaustiveness (no static check).** The runtime `MatchNoArm` panic already exists; it
  is a backstop, not a guarantee — "compile error on a missing variant" is the stated goal and the
  highest-value half of the feature. We keep the runtime panic *and* add the static gate.
- **Making exhaustiveness a default-Warning lint.** Rejected: `goal.md` says "a missing variant is a
  **compile error**". Default **Error**, gated on a provable enum subject (gradual-silent otherwise) so
  the corpus stays at zero false positives.
- **Mixed named+positional fields in one variant.** Rejected for coherence (parse error); a variant is
  uniformly named or uniformly positional (Rust's rule).

## 11. Implementation surface & cross-cutting checklist

Per the `CLAUDE.md` "Touching syntax" checklist (this adds a `Pattern` variant *and* changes the enum
surface — both parsers, both engines, the grammar, fmt, ast Display, checker, LSP, REPL, docs, examples
all move). **Every item is a required deliverable**; not done until green in both feature configs.

**Values & core (`src/value.rs`):** extend `EnumVariant` with `payload: Option<Payload>` + `ctor: bool`
(§5.1, editing the construction sites in place — §5.3 enumerates them); add `Payload`/`VariantSchema`;
`variant_schemas` on `EnumDef`. **Keep `Value::EnumVariant(Rc<EnumVariant>)` (the wrapper is NOT
re-typed to `Cc` — §5.3 decision: unit-variant construction stays registration-free; only the payload
`Vec`/`Cc<ObjectCell>` is cycle-collected).** Add a real `Trace` impl that traces `value` + (when
`Some`) the payload elements, mirrored into `Value::trace` (`CLAUDE.md` "Values"); structural payload
equality in the `PartialEq` `EnumVariant` arm (`src/value.rs:719-720`); `Display` for a constructed
variant (`Shape.Circle(2.0)`) (`src/value.rs:885`); `MapKey` rejects payload variants (unit unchanged).
Audit the `EnumVariant`-touching arms that today match on the wildcard inner and confirm they stay
correct with a payload: `is_truthy` (`src/value.rs:687`, today `!matches!(self, Nil | Bool(false))` —
a constructed variant is neither, so it is **truthy**, consistent with NUM's model and the campaign
note; no code change needed but it is an asserted test) and the runtime `type_name`
(`src/interp.rs:5410`, currently `EnumVariant(_) => "enum variant"` — matches the wildcard, unchanged
string, no payload distinction). Update the
`src/gc.rs:34` doc-comment (drop `EnumVariant` from "immutable/acyclic … stay on `Rc`"; note the
wrapper is `Rc` but a `Some(payload)` is traced).

**AST (`src/ast.rs`):** `EnumVariantDecl` gains payload fields (`Vec<(Option<Rc<str>>, Type)>` or a
`backing: Option<Expr>` XOR `payload: Vec<VariantField>`); new **`Pattern::Variant { enum_name:
Option<Rc<str>>, variant: Rc<str>, fields: VariantPatFields }`** (positional `Vec<Pattern>` or named
`Vec<(Rc<str>, Option<Pattern>)>`); exhaustive arms added (compile-error-enforced) in `interp.rs`
(eval), `fmt.rs` (`write_pattern`), and `ast.rs` `Pattern` `Display` (§ `src/ast.rs:420`).

**Both parsers:**
- **Legacy `src/parser.rs`:** `enum_decl` (`:281`) parses payload field lists (named/positional,
  uniformity error, backing-XOR-payload error); `parse_pattern` (`:1341`) recognizes a variant pattern
  `Name(…)` / `Name.Variant(…)` and produces `Pattern::Variant` (after the value-expression parse, when
  the parsed primary is a variant-ref followed by `(`).
- **CST `src/syntax/parser.rs`:** `enum_decl` (`:1449`) + `enum_variant` payload fields; `pattern`
  (`:1691`) gains a `VariantPat` CST node (a variant-ref followed by a paren sub-pattern list);
  `enum_variants`/schema extraction in `src/check/infer/table.rs` (`:250`). Compiler `compile_enum`
  (`src/compile/mod.rs:1949`) builds the `variant_schemas`; `compile_pattern_test`
  (`src/compile/mod.rs:3596`) lowers `Pattern::Variant` byte-identically to the tree-walker.
- **Frontend conformance** (`tests/frontend_conformance.rs`) proves the two front-ends agree on payload
  enums + variant patterns.

**Tree-sitter (`tree-sitter-ascript/grammar.js`):** extend `enum_variant` (`:273`) with an optional
payload field list (named/positional). **Pattern grammar — decision (semantic recovery for positional,
a node only for named):**

- `_match_pattern_single` (`:447`) already routes through `_match_subject` (`:605`), which includes
  `_postfix_expression` and therefore `call_expression`. So `Circle(r)` and `Shape.Circle(r)` in
  pattern position **already parse today** — as a `call_expression` whose callee is a name/member and
  whose args are sub-pattern names. For **positional** payload patterns we **adopt semantic recovery**
  (the same lineage as `Range`-pattern recovery, where a value-position expression is *re-interpreted*
  as a pattern downstream): no new grammar node, no new GLR conflict — the existing
  `[$._expression, $._match_subject]` conflict (`:77`) already keeps the pattern reading alive, and the
  legacy/CST parsers re-classify the parsed call into `Pattern::Variant` when its callee is a
  variant-ref (§ legacy `parse_pattern`, CST `pattern`). The previously-cited `array_pattern_match` vs
  `array_literal` precedent (`grammar.js:73`) is a **bracketed `[…]`** form and is therefore the wrong
  precedent for a `Name(…)` call form — it is dropped.
- **Named / nested sub-patterns** (`Rect(w: ww)`, `Circle(radius: 0.0)`) **cannot** ride
  `call_expression`: a call argument is `key: value`-shaped only in the unrelated named-call-arg surface,
  and a sub-pattern position needs `_match_pattern_single`, not `_expression`. For these we add a small
  `variant_pattern` node ONLY for the named/renamed/nested case — `Name`/`Name.Variant` followed by a
  paren list of `field (':' _match_pattern_single)?` entries — with a single declared GLR conflict
  against `call_expression` (the positional `Circle(r)` form stays a call; the named `Rect(w: ww)` form
  reduces to `variant_pattern`). This is the minimal node that the call-recovery path cannot cover.

Regen `parser.c` (`tree-sitter generate --abi 14`);
update `queries/highlights.scm` (variant-name + field highlighting). **Publish** via
`./scripts/sync-grammar.sh`, then bump the editor pins (`editors/zed/extension.toml` `commit`,
`editors/nvim/lua/ascript/treesitter.lua` `revision`); update the bundled `highlights.scm` copies
(Zed `editors/zed/languages/ascript/highlights.scm`, Neovim `editors/nvim/queries/ascript/`) and the
VS Code TextMate grammar (`editors/vscode/syntaxes/ascript.tmLanguage.json`).

**Both engines:**
- **Tree-walker (`src/interp.rs`):** `Stmt::Enum` eval (`:2750`) builds `variant_schemas` +
  constructor variants; `read_member` (`:3575`) returns a constructor for a payload variant and extends
  `.value` (`:3580`) + named-field sugar for the `EnumVariant` arm; `call_value` validates a variant
  constructor call (arity + field types via `validate_into`); `match_pattern` (`:3259`) gains the
  `Pattern::Variant` arm (tag-test + payload destructure).
- **VM:** `compile_enum`/`compile_pattern_test`/`call` mirror the above byte-identically; a variant
  constructor call routes through the same validation. If a fused `Op::MatchVariant` is introduced,
  add it to `src/vm/opcode.rs` + `run.rs` + `disasm.rs` and keep both specialize modes equal.

**`.aso` (`src/vm/aso.rs` + `src/vm/verify.rs`):** the enum/variant constant layout gains the per-variant
schema (field names + types) and a constructed-variant constant gains the payload; serialize/verify the
new layout; **bump `ASO_FORMAT_VERSION` by reading the current constant and adding 1** — do NOT
hardcode `19`. The constant is `src/vm/aso.rs:105` (today `18`), but the campaign merge order is
sequential and load-bearing (NUM merges first and also bumps it; cross-cutting #5: "never hardcode 19").
The implementer reads `ASO_FORMAT_VERSION` at merge time and bumps it by one relative to whatever NUM
left it. Update `verify.rs` bounds checks for the payload arrays; **clamp** every payload-length
`reserve`/`with_capacity` in the new reader paths with `.min(r.remaining())` (cross-cutting #1 — the
existing reader has unclamped allocations; do not add new ones).

**Worker airlock (`src/worker/serialize.rs`):** extend tag 10 with the `payload_tag` (§6); **add
far-side re-interning for unit variants** — the decoder at `:620-630` currently builds a fresh
`Rc::new(EnumVariant{…})` with NO `EnumDef` lookup (a NEW requirement, not existing behavior — §6),
so a received unit variant must instead resolve to the receiver isolate's interned constant (best-effort
fallback to a fresh `Rc` if the far-side `EnumDef` is absent); the encode-side fixture is `:897`.
Round-trip test for positional + named + cyclic recursive payloads, incl. as nested fields and Map
values, **plus the cross-isolate equality test** (received unit variant `==` far-side literal; received
payload variant `==` far-side construction, structural); the sendability path-error extends through the
payload. Clamp any payload-length `with_capacity` with `.min(r.remaining())` (cross-cutting #1).

**Type systems:** `check_type` for `: Enum` accepts payload variants (`src/ast.rs`); `EnumInfo` +
`enum_variants` schema (`src/check/infer/table.rs`); construction synth + per-variant narrowing +
the **`non-exhaustive-match`** analysis (`src/check/infer/pass.rs`, `ty.rs`). **Register two new codes
in the registry (`src/check/config.rs`, the `KNOWN_CODES`/severity tables near `:43`):**
`non-exhaustive-match` (default **Error** — the correctness gate) and `enum-variant-binding-shadow`
(default **Warning** — the bare-unit-vs-Option-C diagnostic, §3.3). **Extend** the existing
`unknown-enum-variant` rule to payload-constructor calls + qualified variant patterns (§7.2). Optional
secondary `unreachable-match-arm` (default Warning) if shipped. `std_arity.rs` unaffected (variant
constructors are not `std/*` fns). **Invariant:** `examples/**` emits **zero**
`non-exhaustive-match` / `enum-variant-binding-shadow` / `type-*` false positives in both feature
configs (gradual gate, Gate 5).

**Formatter (`src/fmt.rs`):** render payload variant declarations (`Circle(radius: float)`,
`Pair(int, int)`) and `Pattern::Variant` (`Circle(r)`, `Rect(w: ww, h: hh)`) canonically; idempotence
goldens; `ast.rs` `Display` matches the formatter.

**LSP (`src/lsp/`):** semantic tokens for variant fields; `hover` shows a variant's payload signature
and a constructed value's enum type; go-to-def / find-references / rename cover payload variants and
their fields (the `workspace.rs` index); the `non-exhaustive-match` diagnostic flows the existing
`check::analyze` → LSP path; completion offers variant constructors with their field placeholders.

**REPL (`src/repl.rs`):** payload variant decls + variant patterns use parens/braces → existing
delimiter-depth `is_incomplete` buffering handles multi-line entry; cross-line persistence via the
session `Vm`/`Interp`. Regression test (declare a payload enum, construct, match, observe `.value`).

**Docs:** rewrite the enum section in
`docs/content/language/classes-enums.md` (algebraic variants, construction, the `.value` contract) and
the `match` content (variant patterns + exhaustiveness) in the language guide; note the typed-error
pattern in `docs/content/language/errors.md`; update `README.md`'s feature line; the main design spec's
enum/match sections; `CLAUDE.md` (the "Values" `EnumVariant` paragraph + the "Match pattern extensions"
note + an exhaustiveness bullet); `roadmap.md`. **NAV unchanged** (content appends to existing pages —
no new slug — but re-verify the served site).

**Tests:** `frontend_conformance.rs` (payload enums + variant patterns agree across front-ends),
`treesitter_conformance.rs` (new grammar rules parse), `vm_differential.rs` (the new examples, all four
modes, both feature configs), `check.rs` (the `non-exhaustive-match` rule: missing-variant errors,
wildcard/binding/exhaustive-coverage pass, guarded-arm rule, gradual-silent on unknown subject, zero
false positives on `examples/**`), `lsp.rs` (tokens/hover/nav for variants).

**Unchanged:** the GC algorithm itself, the `Interp` async model, structured concurrency, the worker
pool/scheduler, `?`/`!` propagation semantics, the runtime `MatchNoArm` backstop, all non-enum stdlib.

## 12. Testing & example corpus

### 12.1 Unit & checker tests (the no-bugs pillar)
- **Parse errors (each an asserted diagnostic, not a comment):** mixed named+positional fields in one
  variant (`Pair(int, h: float)` → `enum variant fields must be all named or all positional`);
  backing-XOR-payload (`Foo = 2(int)` / a variant with both `= scalar` and `(…)` →
  `a variant cannot have both a '= value' backing and a '(…)' payload`); a multi-field **named** variant
  called positionally (`Shape.Rect(3.0, 4.0)` → `Shape.Rect requires named fields (w:, h:)`). Each is a
  `tests/frontend_conformance.rs` case (both front-ends produce the same error) where it is a parse
  error, and a `tests/cli.rs`/eval case where it is an eval-time error.
- **Construction:** positional/named/single-field-positional convenience; arity error; field-type
  error (per NUM: `Circle("x")` → field-path panic); unit-variant-called error; payload-variant-as-
  Map-key panic; first-class constructor (`array.map(radii, Shape.Circle)`); positional `.value` returns
  a **stable** Array handle (`v.value is v.value`, §3.4).
- **Exhaustiveness as an EXERCISED `check` failure (Gate 9, not a comment):** a fixture enum with a
  `match` missing one variant and **no** `_` runs through `cargo run -- check` (and `tests/check.rs`) and
  asserts a `non-exhaustive-match` **Error** naming the missing variant — this is the canonical failing
  case, materialized as a real test, not the inline `// no _: …` comment in §3.5's `render`. The passing
  twin (add the missing arm OR a `_`) asserts **zero** diagnostics.
- **Equality:** `Shape.Circle(2.0) == Shape.Circle(2.0)` true (structural); `Shape.Circle(2.0) !=
  Shape.Circle(3.0)`; unit variants identity-equal (unchanged); recursive payload equality.
- **`.value`/`.name` contract (§4):** every row of the table, asserting unit variants byte-identical to
  pre-ADT behavior. **Truthiness:** a constructed payload variant is **truthy** (`if Shape.Circle(2.0)`
  takes the branch), matching NUM's truthiness model and the unchanged `is_truthy` wildcard arm.
- **Pattern matching:** positional / named / renamed / nested / guarded / or-pattern variant patterns;
  bind correctness; a non-matching variant falls through; the runtime no-arm panic still fires for an
  uncovered gradual subject.
- **Exhaustiveness checker:** missing-variant → `non-exhaustive-match` (Error) with the missing names;
  full coverage / `_` / bare-binding catch-all → no diagnostic; guarded-only coverage does NOT satisfy;
  **unknown/`any` subject → silent**; `examples/**` → zero false positives (both configs); **explicit
  zero-diagnostic assertions on `examples/oop.as` and `examples/all_features.as`** (their catch-all `_`
  is a *sibling* of the `MatchExpr` — the CST-sibling-gather regression guard, §7.3). A bare unit
  variant that would *bind* (not compare) on a known-enum subject → `enum-variant-binding-shadow`
  (Warning), and that arm counts as a catch-all (§3.3); qualified `Shape.Point` → no warning, counts as
  covering `Point`. `Shape.Nope(…)` constructor call / `Shape.Nope(r)` pattern → `unknown-enum-variant`
  (the extended rule, §7.2).
- **GC:** a cyclic recursive payload (`Json::Arr` containing itself) is collected (no leak; the
  cycle-collector reaches it via the new `Trace`).
- **Round-trips:** `.aso` write→read preserves payload variants + schemas; worker encode→decode
  preserves positional/named/cyclic payloads.

### 12.2 Four-mode byte-identity (REQUIRED)
Every new enum example runs identically on tree-walker, specialized VM, generic VM, and `.aso`-compiled
(`tests/vm_differential.rs`, both feature configs) — construction, equality, destructuring, `.value`,
and the runtime no-arm panic.

### 12.3 Example corpus
- New `examples/enums_adt.as` — `Shape` (Circle/Rect/Pair/Point): construction, `match` area, the
  `.value` reflection, first-class constructor via `map`.
- New `examples/advanced/state_machine.as` — a payload-carrying event enum driving a state machine
  (`enum Event { KeyPress(code: int), Resize(w: int, h: int), Quit }`), exhaustively matched. The
  "exhaustiveness catches a forgotten case" demo is carried by the **exercised** `tests/check.rs`
  fixture (§12.1), not only an inline comment in this example; the example itself stays a clean,
  zero-diagnostic program (Gate 5).
- New `examples/advanced/json_adt.as` — the recursive `Json` enum (§3.5) with a render + a parse,
  exercising recursive payloads + GC.
- Optional `examples/advanced/typed_errors.as` — a `DbError` enum threaded through `[value, err]` with
  `?` and an exhaustive error `match` (the §8 synergy).
- **Migration (Gate 7):** existing enum examples/goldens (`examples/all_features.as:143`,
  `examples/oop.as:1`) and any `.name`/`.value` goldens are reviewed against the §4 contract; unit
  enums need no change, but the corpus is verified, never trimmed to dodge the surface change.

### 12.4 FUZZ hook (continuous infra)
Payload enums are a target for the FUZZ spec's differential/property fuzzers: random variant
construction + match (assert `tree-walker == specialized == generic`), the structured-clone round-trip
over payload variants (incl. cycles), and exhaustiveness-analysis stability. ADT lands the property
tests above; FUZZ generalizes them.

## 13. Grounding (verified sources)

- Algebraic data types / sum types with payloads: Rust `enum` (variants with tuple + struct fields;
  *The Rust Reference* §"Enumerations"); Swift `enum` with associated values (*The Swift Programming
  Language* §"Enumerations").
- Exhaustive pattern matching as a compile error: Rust `match` non-exhaustive-patterns (E0004); Swift
  `switch` "must be exhaustive"; OCaml/Haskell incomplete-match warnings (the lineage exhaustiveness
  descends from).
- Guarded-arm coverage rule (a guarded arm does not by itself cover a constructor): Rust match
  exhaustiveness with match guards.
- Structural-clone payload copy across isolates: WHATWG structured-clone algorithm (workers Spec A §5),
  extended to the variant payload.
- Gradual-silent-on-unknown discipline: AScript SP10 checker (`CLAUDE.md` — `Compat3::Unknown` never
  emits; the gradual escape that keeps the untyped corpus at zero false positives).
