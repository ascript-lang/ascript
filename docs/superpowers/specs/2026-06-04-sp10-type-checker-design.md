# SP10 — Static Gradual Type Checker with Inference — Design

> **Status:** approved design, ready for implementation planning (superpowers:writing-plans).
> **Sub-project of** the post-cutover gap program (companion to SP1 engine-parity; SP2–SP9 precede or
> run alongside). Supersedes `docs/superpowers/specs/2026-06-04-sp10-type-checker-PROPOSAL.md` (the
> survey/fork doc); every fork in §8 of that proposal is now resolved below by the owner decisions.
> **Plan:** `docs/superpowers/plans/2026-06-04-sp10-type-checker.md`.

**Goal:** Add an **advisory, local-bidirectional, intra-procedural gradual type checker** to AScript
that predicts likely runtime contract violations ahead of time, surfacing them as default-Warning
`type-*` lint diagnostics through the **existing** `src/check/` machinery (`AsDiagnostic` / `Severity`
/ `LintConfig` / inline `ascript-ignore` / `--deny`/`--warn`/`--allow` / `ascript.toml [lint]`). It
is a **layer above** the existing conservative lints, **subsumes** `contract-mismatch` and
`field-default-type`, and is **orthogonal** to `call-arity` and the control/effect lints.

**The one non-negotiable constraint (forces everything else):** AScript has **no static-reject
phase** — every semantic error is *runtime-timed* so the bytecode VM and the `--tree-walker`
reference engine stay **byte-identical** (the SP1 three-way differential invariant). Therefore SP10
is **advisory only**: it NEVER gates execution, NEVER changes runtime semantics, NEVER touches the
lexer / parser / grammar / formatter / VM / tree-walker / `value.rs`. A program with `type-*`
diagnostics still runs and still produces byte-identical output on both engines. **Zero
language/runtime/VM change ⇒ zero byte-identity risk.** This is the SP10 analogue of "the tree-walker
never touches the shape registry": the checker is a pure static observer.

**Soundness bar (reframed for an advisory tool).** A gatekeeping checker is held to soundness; an
advisory one is held to a *noise* bar: **zero false positives on the existing untyped corpus**
(`examples/*.as` + `examples/advanced/*.as`, ~55 programs today), high signal on annotated code.
Where soundness and zero-false-positives conflict, **zero-false-positives wins** (the Luau
"nonstrict" philosophy). The concrete enforcement is a **whole-corpus zero-new-diagnostic
differential** gating every task — the SP10 analogue of the VM three-way differential.

**Architecture:** a single new **stateful inference pass** in `src/check/` (`src/check/infer/`),
feature-independent (must build + pass under `--no-default-features`), static-only, reusing the CST
front-end (`lexer → parser → tree_builder → resolve`) and *never* the interpreter (the same invariant
the rest of `src/check/` carries — confirmed: `analyze_with_config` in `src/check/analyze.rs` runs
`parse → build_tree → resolve` and never instantiates `Interp`). The pass plugs into
`analyze_with_config` after `resolve`, alongside the `rules::ALL` loop, and emits ordinary
`AsDiagnostic`s — no new diagnostic plumbing, no new CLI surface beyond new rule codes in
`RULE_CODES`.

**Tech stack:** Rust. Front-end: `src/syntax/{lexer,parser,tree_builder,kind,cst}.rs` →
`src/syntax/resolve` (`ResolveResult`: `uses: HashMap<TextRange, Resolution>`, `bindings:
Vec<Binding>`, frames). Diagnostics: `src/check/diagnostic.rs` (`AsDiagnostic`, `Severity`,
`ByteSpan`). Surface types: `ast::Type` (`src/ast.rs:127`). No new dependency.

---

## Non-goals (explicitly out of SP10 v1)

These are **deferred** (ranked in §9), not silently dropped:

- **Whole-program / global constraint inference.** No inferring parameter types from call sites, no
  cross-module type inference, no global constraint solve. Inference is **intra-procedural** and
  parameters with no annotation default to `any`. (Owner decision; SP4 already drew the cross-module
  line at "arity yes, types no".)
- **Any new surface syntax.** No function-type syntax (`fn(number): bool`), no type aliases (`type
  UserId = number`), no custom type-guard predicates (`x is Foo`), no user generics (`fn first<T>`),
  no literal/singleton types (`"GET" | "POST"`). v1 checks **exactly** the annotations the language
  already parses (`ast::Type` / the `NamedType`/`GenericType`/`OptionalType`/`UnionType`/`TupleType`
  CST nodes). This keeps both parsers, the formatter, the tree-sitter grammar, and the LSP keyword set
  **untouched** — the reason v1 is "checker only, no language change" and respects the design's
  standing **no-user-generics** rule.
- **Strict mode / strictness ladder.** v1 ships **nonstrict-only** (only provable `No` diagnoses).
  `--strict` (advisory "this is `any` / un-annotated" hints) is a documented future lever, off by
  default, never on the corpus.
- **Stdlib / builtin signatures.** Everything `import`ed or native returns `any` in v1 (keeps the
  corpus quiet). A Sorbet-RBI-style signature-stub program is a future expansion.
- **Alias / closure / custom-guard narrowing** (see §4 v1 non-goals).
- **Cross-module return-type inference.** A function's inferred return type is used only at call sites
  **within the same file**.

---

## §0 — Prior art (cited; the four analogues)

AScript sits closest to **Luau** (a dynamically-typed scripting language retrofitted with a gradual,
inferring checker) and **Sorbet** (gradual + *nominal* classes + runtime-checked signatures —
AScript's contracts are exactly Sorbet's runtime sigs).

- **Gradual typing theory (Siek & Taha 2006).** A distinguished `dynamic` type (`any` here) relates
  to every type via a **consistency** relation that is *reflexive and symmetric but NOT transitive*;
  consistency and subtyping are orthogonal and compose. This is the formal core of the gradual
  boundary in §5: `any ~ T` and `T ~ any` for all `T`, but `T ~ U` does **not** follow from
  `T ~ any ~ U`. ([Siek][siek], [Wikipedia: gradual typing][gt-wiki], [Refined Criteria][refined])
- **Luau (Roblox).** Two modes: **nonstrict** ("infer `any` whenever you can't figure a type out
  early") and **strict**. Its **local type inference** RFC tracks per-binding bounds and solves them
  locally. We lift the nonstrict-default + local-inference model directly. ([Luau LTI RFC][luau-lti],
  [Luau type checking][luau-tc], [Goals of the Luau Type System][luau-goals])
- **Sorbet (Ruby).** Gradual, **nominal**, with `T.untyped` as the gradual top/bottom and runtime sig
  checks that mirror static checks — precisely AScript's "static checker predicts, runtime contract
  enforces" split. ([Sorbet gradual][sorbet-gradual], [Sorbet static][sorbet-static],
  [T.untyped][sorbet-untyped])
- **TypeScript.** The canonical source for **control-flow narrowing** (`typeof`, `instanceof`,
  truthiness, equality, discriminated unions); narrowing does **not** persist across callback
  boundaries. We adopt its flow-sensitive machinery (§4) but over AScript's **nominal** class lattice
  and `nil`-guards. ([TS Narrowing][ts-narrow], [TS control-flow][ts-cfa])
- **mypy.** `Any` as the dynamic boundary (every type assignable to/from `Any`, un-annotated
  functions un-checked) — confirms "unannotated ⇒ `any` ⇒ silent". ([gradual typing in Python][gt-python])

**Takeaway:** the gradual boundary (`any`) makes a retrofit checker tolerable on a large untyped
codebase, and **local (not whole-program) inference** is what every successful retrofit shipped first.

---

## §1 — The `CheckTy` lattice over AScript's existing types

The lattice is built over the **existing `ast::Type`** (verified `src/ast.rs:127`: `Number`, `String`,
`Bool`, `Nil`, `Any`, `Fn`, `Object`, `Error`, `Array(Box<Type>)`, `Result(Box<Type>)`,
`Tuple(Vec<Type>)`, `Union(Box<Type>, Box<Type>)`, `Named(String)`, `Map(Box<Type>, Box<Type>)`,
`Future(Box<Type>)`, `Optional(Box<Type>)`). No new *surface* types in v1. We introduce an *internal*
checker type `CheckTy` (a new type in `src/check/infer/ty.rs`) that mirrors `ast::Type` plus the
lattice endpoints the surface language does not name:

```rust
// src/check/infer/ty.rs
pub enum CheckTy {
    Any,                          // gradual dynamic: the consistency wildcard
    Never,                        // INTERNAL only — empty type; result of exhaustive narrowing
    Number, String, Bool, Nil, Bytes, Object, Regex, Error,
    Fn,                           // unparameterized callable (AScript has no fn-arity types today)
    Array(Box<CheckTy>),          // array<T>
    Map(Box<CheckTy>, Box<CheckTy>),
    Tuple(Vec<CheckTy>),          // [T, U, ...]
    Result(Box<CheckTy>),         // Result<T> == [T, error]
    Future(Box<CheckTy>),         // future<T>
    Union(Vec<CheckTy>),          // normalized, flattened, dedup'd, sorted; T? == Union[T, Nil]
    Class(ClassId),               // NOMINAL — identified by declaration site, carries inheritance chain
    Enum(EnumId),                 // NOMINAL — accepts any of its variants
    EnumVariant(EnumId, Rc<str>), // INTERNAL refinement from match narrowing (a single variant)
    Literal(LitVal),              // INTERNAL refinement: Number(f64)/String/Bool(bool)/Nil from flow
}
```

`ClassId` / `EnumId` are indices into a **class/enum table** the pass builds itself by walking
`ClassDecl` / `EnumDecl` CST nodes (see §6 — the resolver does NOT expose a typed class table; the
existing `contract.rs` already builds an ad-hoc `by_name: HashMap<String, FnDecl>`, and SP10
generalizes that into a proper symbol table).

**Lowering `ast::Type` → `CheckTy` (`CheckTy::from_type_node`).** The pass lowers a *type-annotation
CST node* (the `is_type_kind` set: `NamedType | GenericType | OptionalType | UnionType | TupleType`,
verified in `src/check/rules/mod.rs:168`) into `CheckTy`:

- `NamedType` whose text is a primitive (`number/string/bool/nil/any/object/bytes/regex/error/fn`) →
  the matching `CheckTy`; whose text names a known class → `Class(id)`; a known enum → `Enum(id)`; an
  **unknown** name → `Any` (zero-false-positive default — we never invent a class).
- `GenericType` `array<T>`/`map<K,V>`/`Result<T>`/`future<T>` → the matching parameterized `CheckTy`;
  any other generic head → `Any`.
- `OptionalType` `T?` → `normalize(Union[from(T), Nil])` (canonical; matches the runtime treating `T?`
  as `T | nil`).
- `UnionType` → `normalize(Union[...])`.
- `TupleType` → `Tuple([...])`.

`Never`, `Literal`, and `EnumVariant` are **internal narrowing artifacts**, never written by a user
and never rendered verbatim in a message — they **widen back** to their base type
(`Literal(1) → Number`, `EnumVariant(E,v) → Enum(E)`, `Never → Any` for display) before any diagnostic
text is produced.

### §1.1 — The two endpoints

- **`Any` (gradual dynamic / consistency wildcard).** It is *both* assignable-from-everything and
  assignable-to-everything, **non-transitively** (Siek–Taha). Practically: an `any` value flowing into
  a typed slot is silently accepted (keeps the corpus quiet), and a typed value flowing into an `any`
  slot is silently accepted. An expression whose type the checker cannot determine is `Any` (Luau
  nonstrict default). `Any` is the lowering of every unknown name, every unannotated parameter, and
  every stdlib/native call result.
- **`Never` (internal bottom).** Produced only by exhaustive narrowing (a union all of whose members
  were ruled out). v1 emits **no** diagnostic from `Never` (it would risk false-positive interaction
  with the existing `unreachable-code` rule); it just stops propagating along that path.

### §1.2 — Assignability — the three-valued core invariant

Define `assignable(src, dst) -> Compat3` = "may a value of type `src` flow into a slot expecting `dst`
**without a provable contract violation**?" The result is **three-valued** and is the single most
important invariant in SP10:

```rust
pub enum Compat3 { Yes, No, Unknown }
```

**Only a provable `No` ever produces a diagnostic.** `Unknown ⇒ silent` is the discipline that already
makes `contract-mismatch` zero-false-positive (today's `Compat::Unknown` arm in
`src/check/rules/mod.rs:202` `type_compat`); SP10 **generalizes** it to whole types (not just literals
vs types), never loosens it. `Compat3` is the spiritual successor of the existing `Compat` enum
(`src/check/rules/mod.rs:151`).

The relation, checked **in order** (first matching arm wins):

1. **Gradual escape.** If `src == Any` **or** `dst == Any` ⇒ `Yes`. (Consistency. This rule alone
   guarantees zero false positives on un-annotated code.)
2. **`Never`.** `Never` assignable to anything ⇒ `Yes` (bottom). Anything assignable to `Never` is
   `Unknown` (we never use it to diagnose).
3. **Reflexive / primitive.** Identical primitives ⇒ `Yes`. Two *distinct concrete* primitives
   (`Number` vs `String`, `Bool` vs `Number`, …) ⇒ `No`. (`Object`/`Error`/`Regex`/`Bytes`/`Fn` are
   concrete here too.) **This is the only thing today's `contract-mismatch` proves; SP10 subsumes it.**
4. **Literal refinement.** `Literal(v)` assignable to `dst` ⇒ widen `Literal(v)` to its base primitive
   and recurse. `Literal(Nil)` is `Nil` (handled by rule 5).
5. **`nil` and optionals.** `Nil` assignable to `dst` ⇒ `Yes` iff `dst` is `Nil`, is `Any`, or is a
   `Union` containing `Nil` (i.e. was a `T?`); else `No`. This is the `nil`-guard backbone of §4 and
   the `possibly-nil` engine.
6. **Nominal classes (subtyping via inheritance).** `Class(S)` assignable to `Class(D)` ⇒ `Yes` iff
   `S == D` **or `S` transitively `extends` `D`** (walk the class table's superclass chain, with a
   visited-set to bound cyclic/erroneous graphs). *Nominal*, not structural: two unrelated classes
   with identical fields are **not** assignable (matches the runtime's subclass-aware contract). A
   `Class` assignable to `Object` ⇒ `Yes` (an instance *is* an object at runtime); `Object` assignable
   to a `Class` ⇒ `Unknown` (not provable → silent).
7. **Enums.** `EnumVariant(E,v)` assignable to `Enum(E)` ⇒ `Yes`; `Enum(E)` to `Enum(E)` ⇒ `Yes`; a
   *different* enum ⇒ `No`; `EnumVariant(E,v)` to `EnumVariant(E,v)` ⇒ `Yes`, to a different variant of
   the same enum ⇒ `No`.
8. **Constructors (covariant, depth-limited).** `Array(S)` to `Array(D)` ⇒ `assignable(S, D)`; `Map`,
   `Future`, `Result` likewise (component-wise); `Tuple` positionally with equal length (unequal
   length ⇒ `No`). Covariance is unsound under mutation in theory, but **matches the runtime contract
   semantics** (contracts check element types eagerly at the binding/param site) and the checker is
   advisory, so covariance is correct here. Depth is capped (§1.4); past the cap ⇒ `Unknown`.
9. **Unions.** `src` assignable to `Union[D1..Dn]` ⇒ `Yes` if `src` assignable to *some* `Di` is
   `Yes`; `No` only if assignable to *every* `Di` is `No`; else `Unknown`. `Union[S1..Sm]` assignable
   to `dst` ⇒ `Yes` if *every* `Si` is `Yes`; `No` if *any* `Si` is `No`; else `Unknown`. (Standard
   union variance; mirrors today's `UnionType` handling in `type_compat`.)
10. **`Fn`.** `Fn` assignable to `Fn` ⇒ `Yes`; `Fn` to `Any` (rule 1) ⇒ `Yes`; `Fn` vs any concrete
    non-`Fn` primitive ⇒ `No`. (Structural fn subtyping is a §9 deferral.)
11. **Default (anything else uncertain) ⇒ `Unknown` (silent).** The default arm is *permissive*,
    exactly like the existing `Compat::Unknown ⇒ no-diagnostic` discipline.

### §1.3 — Join (for inference)

The inference engine needs a **join** (least upper bound), used when a `let` has no annotation and is
assigned from multiple branches, when an array literal has mixed elements, or when merging
narrowed facts at a control-flow join point:

- `join(T, T) = T`; `join(T, Any) = Any`; `join(Number, Nil) = Union[Number, Nil]` (i.e. `Number?`).
- `join(Literal(a), Literal(b))` widens both to base primitives first.
- `join(Class(A), Class(B))` = **nearest common ancestor** in the inheritance chain (walk both chains,
  intersect); else `Object`; else `Union[A,B]` capped at width 1 (then `Any`). (Owner-implied: prefer
  nearest common ancestor, fall back to `Union`, then collapse.)
- A join that would exceed the width cap (§1.4) collapses to `Any` (Luau-style giving-up keeps
  inference cheap and quiet).

`meet` (greatest lower bound) is used by narrowing (§4) and is the dual; in v1 only the cases narrowing
needs are implemented (subtract a member from a union, intersect a class with a class).

### §1.4 — Normalization & cost control

Unions are **flattened** (no nested `Union`), **deduplicated**, **`nil`-canonicalized**
(`Union[T, Nil]` is the one representation of `T?`), and **sorted** by a stable discriminant order so
`CheckTy` has a canonical form (required for dedup, for `Eq`, and for the corpus differential to be
deterministic). A **width cap of 8 members** collapses an oversized union to `Any`. Constructor
recursion (`Array`/`Map`/`Tuple`/…) is **depth-capped at 8**; past the cap, `from_type_node` yields
`Any` and `assignable` yields `Unknown`. Class-graph traversal always carries a **visited-set** (no
unbounded recursion on a cyclic/erroneous `extends`). These caps are the SP10 analogue of the GC's
bounded trial-deletion traversal — bounded work, never a hang, never noise from runaway unions.

---

## §2 — The bidirectional local-inference algorithm

Inference is **local, bidirectional, intra-procedural** (Luau LTI / TS-local / Sorbet — the model
every successful dynamic-language retrofit shipped first). It is **modular** (one function/body at a
time), **fast** (no global constraint solve), **incremental** (fits the LSP per-document model), and
**predictable** (errors are local to where annotations meet inferred values). Two directions:

- **Synthesis (infer up): `synth(expr) -> CheckTy`.** Compute an expression's type bottom-up.
  - literals → `Literal(v)` (a number/string/bool/nil literal or a `TemplateExpr`, which is always
    `String`); widened to the base primitive at most use sites.
  - array literal `[a, b, ...]` → `Array(join of element synths)`; an empty literal → `Array(Any)`.
  - object literal `{...}` → `Object`.
  - `new C(...)` / class construction `C(...)` where `C` resolves to a known class → `Class(id)`.
  - enum variant access `E.Variant` → `EnumVariant(id, "Variant")`.
  - arithmetic `a + b`: `String` if either side is provably `String` (the `+` overload), else `Number`
    if both sides are provably numeric, else `Any`. `- * / %` etc. → `Number` if both provably numeric,
    else `Any`. (Synthesis stays `Any` when unsure — never invents `Number`.)
  - comparisons (`== != < > <= >=`), logical (`&& || !`), `instanceof` (SP2) → `Bool`.
  - `await e` → unwrap `Future(T)` to `T` (a non-future → identity, matching the runtime).
  - `e?` (propagate, `TryExpr`) → unwrap `Result(T)` / 2-tuple to `T`; else `Any`.
  - `e!` (unwrap, `UnwrapExpr`) → unwrap likewise.
  - a call to a **locally-declared, in-file** function (or method) with a declared/inferred return →
    that return type; a call to anything else (stdlib/native/`any`-typed callee) → `Any`.
  - `x ?? default` → `join(narrow(synth(x), non-nil), synth(default))`.
  - a `NameRef` → the binding's *current narrowed type* (§4) if present, else its inferred/declared
    type, else `Any`.
  - **everything else → `Any`.** (The synthesis default is the gradual escape hatch — it is what keeps
    unfamiliar expressions silent.)

- **Checking (push down): `check(expr, expected)`.** A slot with a known expected type — an annotated
  binding initializer, an annotated parameter at a call site, an annotated `return` against the
  function's declared return type, an annotated class-field default, an array element against the
  declared element type — computes `assignable(synth(expr), expected)`; on `No` it emits
  `type-mismatch` (or `possibly-nil` when the `No` is specifically a non-nil slot receiving a
  provably-`T?` source — see §6). `Unknown`/`Yes` are silent.

**Statement-level inference (`let`/`const`/`return`):**

- `let x: T = e` / `const x: T = e` (annotated) → `check(e, from(T))`; bind `x : from(T)`.
- `let x = e` / `const x = e` (unannotated) → `synth(e)` widened (drop `Literal` to base primitive);
  bind `x` to that inferred type. **Unannotated bindings are the inference workhorse** — they give
  downstream slots a real type without any annotation.
- a **parameter** with no annotation → `Any` (we do NOT infer params from call sites — that is
  whole-program); an annotated parameter → its declared type, available inside the body.
- `return e` inside a function → if the function has a **declared** return type, `check(e, that)`; the
  function's **inferred** return type is `join` of all `synth(e)` over its `return`s (used at in-file
  call sites — §2.1).
- reassignment `x = e` to an inferred binding → widen `x`'s type to `join(old, synth(e))` for the rest
  of the scope (keeps inference monotone and cheap; never narrows on assignment in v1).

**Sub-detail — return-type inference depth.** A function's inferred return type is computed from its
own `return`s and used at call sites **within the same file** only. Cross-module return inference is a
§9 deferral (SP4 drew the cross-module line at "arity yes, types no").

---

## §3 — (reserved — see §4 for narrowing)

*(Numbering kept aligned with the proposal: narrowing is §3 there; here the lattice/inference precede
it. Narrowing is §4 below.)*

---

## §4 — Flow narrowing (v1 scope)

Narrowing is **per-binding, flow-sensitive, intra-procedural**, computed over the CST during the
pass's walk. The resolver already gives a `Resolution` for every `NameRef` (`ResolveResult.uses:
HashMap<TextRange, Resolution>`, verified `src/syntax/resolve/types.rs:77`), so narrowing **keys off
the resolved binding, not the textual name** — `let y = x; if (y != nil) {...}` does NOT narrow `x`
because `y` and `x` are different bindings (this is also why alias narrowing is a non-goal).

**Implementation:** narrowing state is a `HashMap<BindingKey, CheckTy>` overlay on the inferred
environment, **pushed/popped at branch boundaries** — *not* mutation of the binding's declared/inferred
type. `BindingKey` is derived from the `Resolution` (a `Local(slot)`/`Upvalue(slot)` within the current
frame, or a `Global(name)`). This keeps narrowing intra-procedural and cheap, and composes with the
existing scope machinery.

**v1 narrowing forms:**

1. **`nil`-guards (highest value by far — AScript's `T?` is everywhere).**
   - `if (x != nil) { /* x : T */ } else { /* x : Nil */ }` and the symmetric `if (x == nil)`.
   - early return: after `if (x == nil) { return / break / continue / panic }`, the **negation**
     (`x : T`) holds for the rest of the block.
   - `x ?? default` narrows the left operand to non-nil for the right-hand synthesis.
   - **Truthiness** `if (x)`: narrow away `Nil` **only** (NOT `Bool(false)`, NOT `0`/`""`) — AScript
     truthiness differs from JS (`0`/`""` are truthy), so narrowing only `Nil` stays provable.
2. **`instanceof` narrowing (DEPENDS ON SP2).** SP2 ships `x instanceof C` as a `bool` operator. In
   the then-branch, narrow `x` to `Class(C)`; in the else-branch, *subtract* `Class(C)` from a union of
   classes. This is the nominal analogue of TS `instanceof` narrowing, provable against the runtime's
   `is_instance_of`. **Dependency note:** T4 of the plan is gated on SP2 having landed `instanceof`;
   if SP2 has not landed, T4 ships `match`-narrowing + early-return-merge only and `instanceof`
   narrowing is deferred to an SP2-follow-up task (the plan calls this out explicitly).
3. **`match` pattern narrowing.** In a `match` arm, narrow the subject to the arm's pattern type: an
   enum-variant pattern → `EnumVariant`; a literal pattern → `Literal`; a class-shaped pattern → that
   class; a `nil` pattern → `Nil`. Phase-8 `ast::Pattern` already carries the structure; the pass
   reuses the pattern shape. An exhaustive `match` over an enum narrows the fall-through to `Never`
   (but per §1.1, no diagnostic from `Never` in v1).
4. **Early-return / control-flow merge.** After a guarded early exit, the negation of the guard holds
   for the rest of the block. At a join point (e.g. after an `if/else` where neither branch exits),
   merge per-binding narrowed types by `join` over the predecessors. (The existing
   `missing-return`/`unreachable-code` rules already model block control flow — reuse that flow shape.)

**v1 NON-goals for narrowing (deferred, §9):**

- **Assignment-alias narrowing** (`let y = x; if (y != nil) { ...x... }`) — TS doesn't do this either;
  deferred. (Falls out naturally from keying on `BindingKey`.)
- **Narrowing across callback / closure boundaries** — TS explicitly drops this (the callback may run
  later); AScript's eager-async makes it doubly unsafe. Inside a nested `fn`/arrow, a captured upvalue
  resets to its declared/widened type.
- **Custom type-guard predicates** (`fn isFoo(x): x is Foo`) — needs new annotation syntax (§9).
- **`in`-operator narrowing** on object keys — low value given nominal classes; deferred.

---

## §5 — The gradual boundary — staying ZERO false positives on the corpus

The make-or-break section. The corpus (`examples/*.as`, `examples/advanced/*.as`) is overwhelmingly
*un-annotated* dynamic code; it must stay **silent** under the default checker. Mechanisms, in
priority order:

1. **Unannotated ⇒ `any` ⇒ silent (mypy / Luau-nonstrict default).** An unannotated **parameter** is
   `Any`; an unannotated **binding** is *inferred* from its initializer but its inference never
   *manufactures* a concrete type it can't prove (synthesis defaults to `Any`); a bare function with no
   annotations anywhere participates in *no* `assignable` check that could fire — every slot is `Any`.
2. **Three-valued `assignable` with `Unknown ⇒ silent` (§1.2 rule 11).** The default arm never
   diagnoses — the same discipline that already makes `contract-mismatch` zero-false-positive,
   generalized.
3. **`any` consistency is non-transitive and one-hop (Siek–Taha).** A value passing *through* `any`
   (`let x: any = foo(); bar(x)`) loses its type — `bar` sees `Any`, no diagnostic.
4. **Builtins / stdlib / native calls return `any`.** Until stdlib signatures exist (a §9 follow-up),
   every `import`ed/native call result is `Any`, so nothing downstream fires.
5. **Unknown named type ⇒ `Any`.** A `NamedType` that resolves to no known class/enum/primitive lowers
   to `Any` (never invented as a phantom class).
6. **Nonstrict-only in v1.** No strict-mode "this is `any`" hints. (`--strict` is a §9 lever.)

**The differential test obligation (the regression lock).** A corpus-wide test asserts the **full
existing corpus produces zero new `type-*` diagnostics**, in **both feature configs**, byte-for-byte.
This is the SP10 analogue of the VM three-way differential. It extends the existing
`tests/check.rs::corpus::checker_is_clean_on_the_corpus` (verified — it walks `examples/` recursively
and asserts no `Error`/`Warning` diagnostics): SP10 adds a sibling test that specifically counts
`type-mismatch`/`type-error`/`possibly-nil` codes and asserts **0** across the corpus, in default AND
`--no-default-features` builds. **Any new corpus diagnostic is a bug in `assignable`/`synth` (relax the
GUARD, never the differential)** — the same rule as the VM three-way differential.

---

## §6 — Architecture, the diagnostic tier, and relationship to existing lints

**Where it lives.** `src/check/infer/` — `ty.rs` (`CheckTy`, `Compat3`, `assignable`, `join`, `meet`,
normalization), `table.rs` (the class/enum symbol table built from CST `ClassDecl`/`EnumDecl`),
`env.rs` (inferred-binding environment + narrowing overlay), and `pass.rs` (the stateful multi-visitor
that drives synthesis/checking/narrowing and emits diagnostics). It is **feature-independent** (builds
+ passes under `--no-default-features`), **static-only**, reuses the CST front-end, and **never** the
interpreter.

**Shape — a single stateful pass, not flat rules.** Unlike the existing rules (each a stateless
`fn(&ResolvedNode, &ResolveResult, &str) -> Vec<AsDiagnostic>`, signature verified in
`src/check/rules/mod.rs:25`), a type checker needs a *stateful* pass (an environment of inferred
binding types, a narrowing overlay, a class/enum table). **It is integrated as ONE entry —**
`infer::check(tree, resolved, src) -> Vec<AsDiagnostic>` **— with the SAME signature as a `Rule`**, so
it slots into `analyze_with_config` exactly where the `rules::ALL` loop runs (verified
`src/check/analyze.rs:77`):

```rust
// src/check/analyze.rs, after the rules::ALL loop:
diagnostics.extend(crate::check::infer::check(&tree, &resolved, src));
```

It internally is a multi-visitor pass (not a flat AST scan), but to the driver it is a single
diagnostic-producing function. It plugs into the *same* `AsDiagnostic` / `Severity` / `LintConfig` /
inline-`ascript-ignore` / `--deny`/`--warn`/`--allow` / `ascript.toml [lint]` machinery — **no new
diagnostic plumbing, no new CLI surface** beyond the new rule codes. (Rejected alternative: several
independent type rules — each would re-derive inference, duplicating work and risking inconsistency.)

**The class/enum symbol table (`table.rs`) — new, because the resolver doesn't expose one.** Verified:
`src/syntax/resolve` records *bindings* and *uses* but NOT a typed class table with field/method
types; `resolve_class` (`src/syntax/resolve/mod.rs:740`) only resolves field defaults and method
bodies and records the superclass *use*. So the pass builds its own table by walking every
`ClassDecl`/`EnumDecl` once up front (the same shape `contract.rs` uses for its `by_name` FnDecl map,
generalized): for each class — its `ClassId`, superclass `ClassId` (resolved by name, with a
visited-set on the chain), field name→`CheckTy` (from `FieldDecl` type annotations), method name→
return `CheckTy`; for each enum — its `EnumId` and variant names. Built **once per `analyze` call**,
shared across the pass.

**New rule codes** (added to `RULE_CODES` in `src/check/config.rs:27`, all default **Warning**, all
`ascript-ignore`-able, all `--deny`/`--warn`/`--allow`-configurable, all NON-immune unlike
`syntax-error`):

- **`type-mismatch`** — a value provably the wrong type for an **annotated slot** (binding
  initializer / parameter at a call / `return` / class-field default). **Subsumes and generalizes**
  `contract-mismatch` (literal-arg-only) and `field-default-type` (literal-field-default-only): SP10
  checks *any* synthesizable expression against the slot, not just literals.
- **`type-error`** — an operation provably ill-typed **regardless of a declared slot**: arithmetic on
  a provably non-`number` (and non-`string`-for-`+`), indexing a provably non-indexable, calling a
  provably non-callable. **Highest-value NEW category** — nothing today catches
  `let x: string = "a"; x - 1`.
- **`possibly-nil`** — a `T?` value used in a position that requires `T` (member access `x.f`, call
  `x()`, index `x[i]`, arithmetic) *without* a `nil`-guard narrowing it. **Default-Warning but heavily
  narrowing-gated** (owner decision): it fires **only** when the receiver's type is *provably* `T?`
  (a `Union` containing `Nil`) **and** no §4 narrowing applies. It is the noisiest category in every
  retrofit (TS `strictNullChecks` is opt-in for this reason), so the corpus differential is the
  acceptance gate before it is allowed to ship enabled (T3 validates noise on the corpus; if a corpus
  program legitimately lights up, the narrowing is extended or the form is gated tighter — never the
  differential relaxed).

**Relationship to existing conservative lints — layer-above, with two explicit subsumptions:**

| Existing rule | Disposition under SP10 |
|---|---|
| `contract-mismatch` (literal arg vs param) | **Subsumed** by `type-mismatch` (checks *any* synthesizable arg). See migration below. |
| `field-default-type` (literal field default vs field type) | **Subsumed** by `type-mismatch` (a default is an expression checked against the field's declared type). |
| `call-arity` | **Orthogonal — KEEP.** Arity is a structural fact, not a type; SP4 extended it cross-module. The type checker does not touch arity. |
| `invalid-propagate`, `ignored-result`, `dead-recover`, `unawaited-future` | **Orthogonal — KEEP.** Control/effect lints; some (`unawaited-future`) *could* benefit from `Future(T)` types but don't depend on SP10. |
| `undefined-variable`, `unused-*`, `shadowing`, `missing-return`, `unreachable-code`, `range-step`, `unknown-enum-variant`, `duplicate-member`, `super-misuse`, `unresolved-import`, `duplicate-binding` | **Orthogonal — KEEP** (resolver/flow lints). |

**Legacy-lint subsumption & migration (owner decision — one-release overlap, then deprecate).** SP10
v1 **keeps both legacy codes firing on their exact old cases** (so no existing `ascript.toml` config
naming `contract-mismatch`/`field-default-type` breaks), while `type-mismatch` fires on the strict
**superset**. Concretely: the legacy `contract.rs`/`field_default_type.rs` rules stay in `rules::ALL`
unchanged for one release; the new pass additionally emits `type-mismatch` for every case (literal
*and* non-literal). To avoid **double-diagnosing the identical literal case at the identical span**,
the new pass **suppresses its own `type-mismatch` when the legacy rule already emitted at the same
span for the same root cause** (a span-keyed de-dup within the pass; the literal-vs-primitive cases are
exactly the legacy subset). Migration sequence, documented in `docs/content` and the changelog:
1. **Release N (SP10 v1):** `type-mismatch` superset + legacy codes on their old subset; docs mark
   `contract-mismatch`/`field-default-type` **deprecated, prefer `type-mismatch`**.
2. **Release N+1:** the legacy rules are removed from `rules::ALL`; their codes stay in `RULE_CODES`
   as **accepted-but-no-op** aliases (like `syntax-error` is accepted in config) so a config naming
   them still validates; `type-mismatch` is the sole emitter.

**Incremental delivery (staged, matching SP/V13 discipline — land-before-load-bearing):**

- **T1 — Lattice + assignability + corpus differential machinery.** `CheckTy`, normalization,
  three-valued `assignable`/`join`, the class/enum table, **with NO diagnostics emitted yet** (pure,
  unit-tested in isolation) PLUS the zero-new-diagnostic corpus differential test wired up (it asserts
  0 today because the pass emits nothing) — exactly how V13-T1 added `Trace` impls before they were
  load-bearing.
- **T2 — Annotated-signature checking + synthesis (no narrowing).** Check annotated binding
  initializers, annotated params at call sites, annotated returns, annotated field defaults; synthesize
  expression types. Emit `type-mismatch`/`type-error`. This *already subsumes*
  `contract-mismatch`/`field-default-type`.
- **T3 — Local return-type inference + `nil`-guard narrowing + `possibly-nil`.**
- **T4 — `instanceof` (SP2-gated) + `match` narrowing; early-return flow merge.**
- **T5 — LSP surface:** hover shows the inferred/declared type; `type-*` diagnostics in the editor
  (reuses the existing per-document `analysis` path; no interpreter).
- **(future, §9):** strict mode, stdlib signature stubs, structural fn types, custom type guards,
  type aliases, literal types.

Each task carries the corpus differential as a gate; any new corpus diagnostic is a bug in
`assignable`/`synth` (relax the **guard**, never the differential).

---

## §7 — No new surface syntax in v1 (confirmed)

v1 adds **no surface syntax** — it checks exactly the annotations the language already parses
(`ast::Type` / the `NamedType`/`GenericType`/`OptionalType`/`UnionType`/`TupleType` CST nodes). This
keeps both parsers, the formatter, the tree-sitter grammar, and the LSP keyword set **untouched** and
respects the design's standing **no-user-generics** rule. Richer annotation forms are ranked deferrals
in §9.

---

## §8 — Resolved owner decisions (the forks from the proposal §8, now settled)

1. **Advisory-only — CONFIRMED.** A richer lint tier emitting default-Warning `type-*` diagnostics
   through the existing `AsDiagnostic`/`LintConfig`/`ascript-ignore`/`--deny` machinery. NEVER a
   compile gate. No language/runtime/VM change → zero byte-identity risk.
2. **Inference scope — local bidirectional, intra-procedural; parameters default `any`; do not infer
   params from call sites.** NOT whole-program. (§2)
3. **`possibly-nil` — default-on Warning, heavily narrowing-gated.** (§6)
4. **Legacy-lint migration — keep both codes one release, then deprecate;** `type-mismatch` subsumes
   `contract-mismatch` + `field-default-type`; `call-arity` + control/effect lints stay orthogonal.
   (§6)
5. **Narrowing v1 — nil-guards + `instanceof` (SP2-dependent) + `match` + early-return merge;** defer
   alias/closure/custom-guard. (§4)
6. **Core invariant — three-valued `assignable`; only a provable `No` diagnoses;** every task gated on
   the whole-corpus zero-new-diagnostic differential. (§1.2, §5)
7. **No new syntax in v1.** (§7)

---

## §9 — Deferrals (ranked; surfaced so the owner can prioritize a follow-up)

1. **Stdlib / builtin signature stubs** (Sorbet-RBI-style) so `math.abs(x): number` etc. flow types —
   the highest-leverage follow-up once v1 is quiet; no syntax change (an internal stub table). Unlocks
   real signal on idiomatic stdlib-heavy code.
2. **`instanceof` narrowing** — ships in T4 **iff SP2 has landed** `instanceof`; otherwise an
   SP2-follow-up. (Listed as a deferral only for the no-SP2 branch.)
3. **Type aliases** `type UserId = number` — pure quality-of-life, no soundness impact; needs new
   syntax + a new binding kind in the resolver. Focused follow-up.
4. **Function / arrow type syntax** `fn(number, string): bool` — makes `Fn` parameterized, enables
   higher-order/callback checking. Big surface-syntax change (lexer/both parsers/grammar/fmt/LSP);
   corpus uses few annotated callbacks → lower priority.
5. **Custom type-guard predicates** `fn isUser(x): x is User` — unlocks user-defined narrowing (§4).
   New return-annotation form.
6. **Literal / singleton types** (`"GET" | "POST"`) — supercharges discriminated-union narrowing
   (TS's killer feature) but is a real type-system expansion.
7. **Strict mode** `--strict` / `ascript.toml [check] strict` — advisory "this is `any` / un-annotated"
   hints, off by default (Sorbet sigils / Luau modes).
8. **Cross-module return-type inference** — used only in-file in v1.
9. **User generics** `fn first<T>(xs: array<T>): T` — the design's standing non-goal; revisiting it is
   a separate, larger decision than SP10.

---

## §10 — Testing & quality bar (whole sub-project)

- **Corpus zero-new-diagnostic differential never relaxed:** the whole corpus (`examples/*.as` +
  `examples/advanced/*.as`) produces **0** `type-mismatch`/`type-error`/`possibly-nil` diagnostics, in
  **both** feature configs. Any new corpus diagnostic = fix the root cause in `assignable`/`synth`
  (relax the guard), never weaken the assertion. (Extends `tests/check.rs::corpus`.)
- **Unit-tested lattice:** `assignable`/`join`/`normalize` have exhaustive `#[test]`s (the three-valued
  result on representative pairs — primitives, `any`, `nil`/optional, nominal class chains, unions,
  constructors, the width/depth caps), in `src/check/infer/ty.rs`.
- **Behavior tests** in `tests/check.rs`: each new code (`type-mismatch`/`type-error`/`possibly-nil`)
  has positive (fires) and negative (silent on `any`/unknown/unannotated/narrowed) cases; the legacy
  subsumption + de-dup (no double diagnosis at the same span) is asserted.
- **Both feature configs:** `cargo test` green default AND `cargo test --no-default-features`.
- **Clippy clean** under `cargo clippy --all-targets` AND `cargo clippy --no-default-features
  --all-targets`.
- **No front-end / VM / value.rs change.** SP10 touches only `src/check/` (+ a thin LSP hover hook in
  T5). The SP1 three-way VM differential and goldens stay **byte-identical** (SP10 doesn't run code) —
  the implementer confirms `cargo test --test vm_differential` is unchanged.
- **LSP (T5):** hover type tests in `src/lsp/analysis.rs`; the existing per-document `analysis` path
  surfaces the new codes with no interpreter.
- **Per-task commit** with trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
  Independent per-task review (re-read spec, re-run gates, adversarial false-positive hunt over the
  corpus) before sign-off.
- **Docs:** update `docs/content` (a "type checking" section under the language guide; the legacy-code
  deprecation note) and the language spec.

## §11 — File-touch map (for the plan)

| Area | Files |
|---|---|
| Lattice / assignability | `src/check/infer/ty.rs` (NEW: `CheckTy`, `Compat3`, `assignable`, `join`, `meet`, normalize, caps) |
| Class/enum table | `src/check/infer/table.rs` (NEW) |
| Inferred env + narrowing | `src/check/infer/env.rs` (NEW) |
| The pass | `src/check/infer/pass.rs` (NEW: synthesis/checking/narrowing, emits diagnostics) + `src/check/infer/mod.rs` (NEW: `pub fn check(...)`) |
| Driver wiring | `src/check/analyze.rs` (one `diagnostics.extend(infer::check(...))` line after `rules::ALL`); `src/check/mod.rs` (`pub mod infer`) |
| Rule codes | `src/check/config.rs` (`RULE_CODES` += `type-mismatch`, `type-error`, `possibly-nil`) |
| Legacy subsumption | `src/check/rules/{contract,field_default_type}.rs` (UNCHANGED in v1; removed from `rules::ALL` in release N+1) |
| LSP (T5) | `src/lsp/analysis.rs` (hover shows inferred/declared type) |
| Tests | `tests/check.rs` (corpus type-diagnostic differential + per-code behavior + subsumption de-dup), inline `#[test]`s in `src/check/infer/*` |
| Docs | `docs/content/*` (type-checking guide + legacy-code deprecation), language spec |

---

### Sources

- Siek & Taha, gradual typing foundations — [What Is Gradual Typing (Jeremy Siek)][siek];
  [Gradual typing (Wikipedia)][gt-wiki]; [Refined Criteria for Gradual Typing (SNAPL 2015)][refined].
- Luau — [Local Type Inference RFC][luau-lti]; [Type checking (strict/nonstrict)][luau-tc];
  [Position Paper: Goals of the Luau Type System][luau-goals].
- Sorbet — [Gradual Type Checking & Sorbet][sorbet-gradual]; [Enabling Static Checks][sorbet-static];
  [T.untyped][sorbet-untyped].
- TypeScript — [Narrowing (handbook)][ts-narrow]; [Control Flow Analysis (Retool)][ts-cfa].
- mypy / Python — [Gradual typing in Python (GeeksforGeeks)][gt-python].

[siek]: https://jsiek.github.io/home/WhatIsGradualTyping.html
[gt-wiki]: https://en.wikipedia.org/wiki/Gradual_typing
[refined]: https://drops.dagstuhl.de/storage/00lipics/lipics-vol032-snapl2015/LIPIcs.SNAPL.2015.274/LIPIcs.SNAPL.2015.274.pdf
[luau-lti]: https://rfcs.luau.org/local-type-inference.html
[luau-tc]: https://luau.org/typecheck
[luau-goals]: https://arxiv.org/pdf/2109.11397
[sorbet-gradual]: https://sorbet.org/docs/gradual
[sorbet-static]: https://sorbet.org/docs/static
[sorbet-untyped]: https://sorbet.org/docs/untyped
[ts-narrow]: https://www.typescriptlang.org/docs/handbook/2/narrowing.html
[ts-cfa]: https://retool.com/blog/typescript-control-flow-analysis-best-of
[gt-python]: https://www.geeksforgeeks.org/gradual-typing-in-python/
