# SP10 — Static Gradual Type Checker with Inference — PROPOSAL

> **Status: PROPOSAL FOR DISCUSSION — type-system-scale.** This is NOT an approved design and NOT a
> plan. It surveys prior art, lays out a type lattice and assignability rules for AScript, and
> surfaces the major forks the owner must decide before this becomes a real design doc + plan. Treat
> every "Recommend" below as the author's opinion, not a settled decision.
>
> **Sub-project of** the post-cutover gap program (companion to SP1 engine-parity; SP2–SP9 precede or
> run alongside). SP4 explicitly listed "type inference / a type checker" as a non-goal and deferred
> it here; the original design's `### Non-goals (v1)` ("No static type inference or compile-time type
> checking") is now being deliberately revisited by the owner, exactly as "No bytecode VM" already
> was (the VM shipped).

---

## 0. Framing & the one non-negotiable constraint

AScript is **gradually typed**: annotations are optional and, where present, enforced **at runtime as
contracts** (design §5). The runtime is the source of truth for semantics — the bytecode VM and the
tree-walker must stay **byte-identical**, and there is *no static-reject phase* (the ranges-step
analyzer design and SP4 both lock this in: every semantic error is **runtime-timed** so the two
engines agree).

That single constraint shapes everything: **the type checker MUST be advisory.** It is a richer
**lint layer**, emitting diagnostics into the existing `src/check/` pipeline (`AsDiagnostic` +
`Severity` + `LintConfig` + inline `ascript-ignore`), *never* a gate that refuses to run a program.
A program with `type-error` diagnostics still runs and still produces byte-identical output on both
engines; the contract may or may not actually panic at runtime, and the checker's job is to *predict
likely panics ahead of time*, not to change execution. This is the deepest design fork resolved
before we start (see §5), and it is the right one — confirmed below.

This also reframes the soundness bar. A traditional static type checker is a *gatekeeper* and is
held to soundness ("well-typed programs don't go wrong"). An **advisory** checker is held to a
*usefulness/noise* bar: **zero false positives on the existing untyped corpus** (the dynamic code in
`examples/` and the stdlib `.as` snippets must not light up), high signal on annotated code. We are
building a *type-aware lint*, not a proof system. Where soundness and zero-false-positives conflict,
**zero-false-positives wins** — this is the Luau "nonstrict" philosophy, not the Sorbet "strong" one.

---

## 1. Prior art (cited)

Four systems are the relevant analogues; AScript sits closest to **Luau** (a dynamically-typed
scripting language retrofitted with a gradual, inferring checker) and **Sorbet** (gradual + *nominal*
classes + runtime-checked signatures — AScript's contracts are exactly Sorbet's runtime sigs).

- **Gradual typing theory (Siek & Taha 2006).** A distinguished `dynamic` type (`any` in AScript)
  relates to every type via a **consistency** relation that is *reflexive and symmetric but NOT
  transitive*; consistency and subtyping are orthogonal and compose. This is the formal core of the
  gradual boundary in §4 — `any ~ T` for all `T`, but `T ~ U` does not follow from `T ~ any ~ U`.
  ([Siek][siek], [Wikipedia: gradual typing][gt-wiki], [Refined Criteria for Gradual Typing][refined])

- **Luau (Roblox).** Two modes: **nonstrict** ("be as helpful as possible for un-annotated code" —
  infers `any` whenever it can't figure a type out early) and **strict** (tracks types across
  statements, flags `string + number`). Its **local type inference** RFC tracks per-binding *lower
  bounds* (from assignments/returns/uses) and *upper bounds* (from annotations/builtins) and solves
  `T <: 't` by widening lower bounds, `'t <: T` by narrowing upper bounds. Return types are inferred
  by *generalization*. This is the model we lift most directly. ([Luau local type inference
  RFC][luau-lti], [Luau type checking][luau-tc], [Goals of the Luau Type System][luau-goals])

- **Sorbet (Ruby).** Gradual, **nominal**, with per-file **strictness sigils** (`ignore` / `false` /
  `true` / `strict` / `strong`) and `T.untyped` as the gradual top/bottom. Un-sig'd methods are
  `T.untyped` in and out. Static checker + runtime sig checks are *two modes over the same
  signatures* — precisely AScript's "static checker predicts, runtime contract enforces" split. The
  strictness ladder is the model for our opt-in `--strict` levels (§5, §6). ([Sorbet gradual][sorbet-gradual],
  [Sorbet static][sorbet-static], [T.untyped][sorbet-untyped])

- **TypeScript.** Structural (AScript is nominal for classes — a key divergence), but the canonical
  source for **control-flow narrowing**: `typeof`, `instanceof`, `in`, truthiness, equality, and
  discriminated unions narrow a union inside a guarded branch; narrowing does **not** persist across
  callback boundaries. We adopt its *flow-sensitive narrowing* machinery (§3) but over AScript's
  nominal class lattice and `nil`-guards. ([TS Narrowing handbook][ts-narrow], [TS control-flow][ts-cfa])

- **mypy.** `Any` is the dynamic boundary: every type is assignable to `Any` and `Any` to every type;
  un-annotated functions are (by default) un-checked. Confirms the "unannotated ⇒ `any` ⇒ silent"
  default that keeps the corpus quiet. ([gradual typing in Python][gt-python])

**Key takeaway across all four:** the *gradual boundary* (`any`/dynamic) is what makes a retrofit
checker tolerable on a large untyped codebase, and **local (not whole-program) inference** is what
every successful retrofit shipped first. Whole-program/global inference (Hindley–Milner-style) is
not what any of these do for the dynamic-language case — they all do bidirectional/local inference
with explicit annotation boundaries.

---

## 2. The type lattice over AScript's existing types

The lattice is built over the **existing `ast::Type`** variants (no new surface types in v1 — see
§7). We introduce an *internal* checker type `CheckTy` that mirrors `ast::Type` plus the two lattice
endpoints the surface language does not name:

```
CheckTy :=
    Any                       // gradual dynamic: the consistency wildcard (TOP for assignability-in, BOTTOM-ish out)
  | Never                     // INTERNAL only — the empty type, result of narrowing a union to nothing
  | Number | String | Bool | Nil | Bytes | Object | Regex | Error
  | Fn                        // unparameterized callable (AScript has no fn-arity types today)
  | Array(CheckTy)            // array<T>
  | Map(CheckTy, CheckTy)     // map<K,V>
  | Tuple(Vec<CheckTy>)       // [T, U, ...]
  | Result(CheckTy)           // Result<T> == [T, error]
  | Future(CheckTy)           // future<T>
  | Union(Set<CheckTy>)       // normalized, flattened, dedup'd; T? == Union{T, Nil}
  | Class(ClassId)            // NOMINAL — identified by declaration, carries the inheritance chain
  | Enum(EnumId)              // NOMINAL — accepts any of its variants
  | EnumVariant(EnumId, name) // INTERNAL refinement from match narrowing (a single variant)
  | Literal(LitVal)           // INTERNAL refinement: Number(1)/String("x")/Bool(true)/Nil from flow
```

`Never` and `Literal`/`EnumVariant` are **internal narrowing artifacts**, never written by a user
and never surfaced in a diagnostic message verbatim (they widen back to their base type, e.g.
`Literal(1) → Number`, before any message is rendered). `Optional(T)` from `ast::Type` is normalized
on entry to `Union{T, Nil}` — there is one canonical representation, matching how the runtime treats
`T?` as `T | nil`.

### 2.1 The two endpoints

- **`any` (gradual dynamic / consistency wildcard).** It is *both* assignable-from-everything and
  assignable-to-everything. It is NOT a normal lattice top/bottom; it is the consistency wildcard:
  `any ~ T` and `T ~ any` for all `T`, **non-transitively** (Siek–Taha). Practically: an `any`-typed
  value flowing into a typed slot is silently accepted (this is what keeps the corpus quiet), and a
  typed value flowing into an `any` slot is silently accepted. An expression whose type the checker
  cannot determine is `any` (Luau nonstrict default).
- **`Never` (internal bottom).** Produced only by exhaustive narrowing (a union all of whose members
  were ruled out). Code reachable only with a `Never`-typed value is dead w.r.t. that value;
  v1 does NOT emit a diagnostic from `Never` (it would risk false positives interacting with the
  existing `unreachable-code` rule), it just stops propagating.

### 2.2 Assignability (consistency-aware subtyping)

We define `assignable(src, dst)` = "a value of type `src` may flow into a slot expecting `dst`
without a *provable* contract violation". A `type-mismatch` diagnostic fires **only** when
`assignable` is provably false. The relation (checked in order):

1. **Gradual escape.** If `src` is `Any` or `dst` is `Any` ⇒ **assignable** (consistency). This rule
   alone guarantees zero false positives on un-annotated code.
2. **Reflexive / primitive.** Identical primitives ⇒ assignable. Distinct primitives
   (`Number` vs `String`) ⇒ **NOT** assignable (the only thing today's `contract-mismatch` proves;
   this rule subsumes it — see §6).
3. **`nil` and optionals.** `Nil` assignable to `dst` iff `dst` is `Nil`, contains `Nil` in a union
   (i.e. was a `T?`), or is `Any`. This is the `nil`-guard backbone of §3.
4. **Nominal classes (subtyping via inheritance).** `Class(S)` assignable to `Class(D)` iff `S == D`
   **or `S` transitively `extends` `D`**. *Nominal*, not structural: two unrelated classes with the
   same fields are NOT assignable (matches the runtime's `Type::Named` subclass-aware contract from
   M7). `Object` (the unparameterized record type) is assignable from any `Class` (an instance *is* an
   object at runtime) but the reverse is **not** provable ⇒ stays silent.
5. **Enums.** `EnumVariant(E, v)` assignable to `Enum(E)`; `Enum(E)` assignable to `Enum(E)`; a
   different enum ⇒ not assignable.
6. **Constructors (covariant, depth-limited).** `Array(S)` assignable to `Array(D)` iff
   `assignable(S, D)`; `Map`, `Future`, `Result`, `Tuple` (positionally) likewise. Covariance is
   *unsound under mutation* in theory (the classic array-store problem) but **matches the runtime
   contract semantics exactly** — AScript contracts check element types eagerly at the binding/param
   site (§5 "parametric depth"), and the checker is advisory, so covariance is the right call and is
   not a soundness hole we are responsible for.
7. **Unions.** `src` assignable to `Union{D1..Dn}` iff `src` assignable to *some* `Di`.
   `Union{S1..Sm}` assignable to `dst` iff *every* `Si` assignable to `dst`. (Standard union
   variance; mirrors today's `type_compat` over `UnionType` in `rules/mod.rs`.)
8. **`Fn`.** Today AScript has no parameterized function types (just bare `fn`). `Fn` is assignable to
   `Fn` and to `Any`; anything else with `Fn` is unprovable ⇒ silent. (Structural fn subtyping is a
   §7 fork, deferred.)
9. **Anything else uncertain ⇒ assignable (silent).** The default arm is *permissive*, exactly like
   the existing `Compat::Unknown` ⇒ no-diagnostic discipline. **Three-valued logic is mandatory:**
   `assignable` returns `Yes | No | Unknown`, and *only `No` ever produces a diagnostic.* This is the
   single most important implementation invariant for keeping the corpus quiet.

### 2.3 Join (for inference)

The inference engine needs a **join** (least upper bound, used when a `let` has no annotation and is
assigned from multiple branches, or an array literal has mixed elements):

- `join(T, T) = T`; `join(T, Any) = Any`; `join(Number, Nil) = Union{Number, Nil}` (i.e. `Number?`).
- `join(Class(A), Class(B))` = nearest common ancestor in the inheritance chain, else `Object`,
  else `Union{A,B}` — **fork:** common-ancestor vs union (Recommend: nearest common ancestor, falling
  back to `Union`, capped at width 1 — see §2.4).
- Joins that would exceed a width cap collapse to `Any` (Luau-style giving-up keeps inference cheap
  and quiet).

### 2.4 Normalization & cost control

Unions are flattened, deduplicated, and `nil`-canonicalized. A **width cap** (Recommend: 8 members)
collapses oversized unions to `Any` — unbounded union growth is the classic way a retrofit inferrer
becomes both slow and noisy. There is no recursion into cyclic class graphs without a visited-set
(reuse the GC's trial-deletion mindset: bounded traversal only).

---

## 3. Flow narrowing (v1 scope)

Narrowing is **per-binding, flow-sensitive, intra-procedural**, computed over the CST during the
checker's walk (the resolver already gives us `Resolution` for every `NameRef`, so we know *which
binding* a name refers to — narrowing keys off the resolved binding, not the textual name). We adopt
TypeScript's model but restrict it to what is *provably correct given AScript's runtime* and *cheap*.

**v1 narrowing forms (Recommend):**

1. **`nil`-guards** — the highest-value form by far (AScript's `T?` is everywhere). In the
   then-branch of `if (x != nil)` / `if (x == nil)`, after `if (x == nil) { return }` (early return),
   and across `x ?? default`, narrow `x` from `Union{T, Nil}` to `T` (or to `Nil`). Truthiness
   (`if (x)`) narrows away `Nil` (and `Bool(false)`? — **fork:** AScript truthiness rules; Recommend
   narrow only `Nil` in v1 to stay provable, since `0`/`""` are truthy in AScript unlike JS).
2. **`instanceof` narrowing (SP2).** SP2 ships `x instanceof C` as a `bool` operator. In the
   then-branch, narrow `x` to `Class(C)`; in the else-branch, *subtract* `Class(C)` from a union if
   `x`'s type is a union of classes. This is the nominal analogue of TS `instanceof` narrowing and is
   provable against the runtime's `is_instance_of`.
3. **`match` pattern narrowing.** In a `match` arm, narrow the subject to the arm's pattern type:
   an enum-variant pattern ⇒ `EnumVariant`; a literal pattern ⇒ `Literal`; a class/`instanceof`-style
   pattern ⇒ that class; `nil` ⇒ `Nil`. Phase-8 patterns (`ast::Pattern`) already carry the structure;
   the checker reuses `match_pattern`'s shape. Exhaustive `match` over an enum can narrow the
   fall-through to `Never` (but see §2.1 — no diagnostic from it in v1).
4. **Early-return / control-flow merge.** After a guarded early `return`/`break`/`continue`/panic, the
   negation of the guard holds for the rest of the block (the `missing-return`/`unreachable` rules
   already model block control flow; reuse that flow graph). Merge narrowed facts at join points by
   `join`-ing the per-binding types from each predecessor branch.

**v1 NON-goals for narrowing (forks deferred):**

- Narrowing through **assignment aliasing** (`let y = x; if (y != nil) { ...x... }`) — TS doesn't do
  this either; deferred.
- Narrowing that **persists across callback / closure boundaries** — TS explicitly drops this (the
  callback may run later); we drop it too (and AScript's eager-async makes it doubly unsafe). Inside
  a nested `fn`/arrow, a captured upvalue resets to its *declared/widened* type.
- **Custom type-guard predicates** (`fn isFoo(x): x is Foo`) — requires new annotation syntax (§7),
  deferred.
- `in`-operator narrowing on object keys — low value given nominal classes; deferred.

**Implementation note.** Narrowing state is a `HashMap<BindingId, CheckTy>` overlay on the inferred
environment, pushed/popped at branch boundaries — *not* mutation of the binding's declared type. This
keeps it intra-procedural and cheap, and means it composes with the existing scope machinery in
`src/syntax/resolve`.

---

## 4. The gradual boundary — staying ZERO false positives on the corpus

This is the make-or-break section. The corpus (`examples/*.as`, `examples/advanced/*.as`, stdlib
`.as`) is overwhelmingly *un-annotated* dynamic code. It must stay **silent** under the default
checker. The mechanisms, in priority order:

1. **Unannotated ⇒ `any` ⇒ silent (mypy/Luau-nonstrict default).** An un-annotated `let`/`const`
   binding's type is *inferred from its initializer* (§5 local inference), but an un-annotated
   **parameter** is `any` (we can't see call sites cheaply intra-procedurally), and an un-annotated
   function's **return** is inferred but used only locally. A bare function with no annotations
   anywhere participates in *no* `assignable` checks that could fire — every slot is `any`.
2. **Three-valued `assignable` with `Unknown ⇒ silent` (§2.2 rule 9).** The default arm never
   diagnoses. This is the same discipline that already makes `contract-mismatch` zero-false-positive
   (`Compat::Unknown`); we are generalizing it, not loosening it.
3. **`any` consistency is non-transitive and one-hop.** A value that passes *through* `any` (e.g.
   `let x: any = foo(); bar(x)`) loses its type — `bar` sees `any`, no diagnostic. This is *exactly*
   the gradual guarantee (Siek–Taha) and is what users expect from `any`.
4. **Builtins/stdlib return `any` by default.** Until stdlib signatures exist (a large §6/§7 follow-up),
   every `import`ed/native call returns `any`. So `let n = math.abs(x)` gives `n : any`, and nothing
   downstream fires. (Opt-in stdlib signature stubs are a future expansion, Sorbet-RBI-style.)
5. **Opt-in strictness ladder (Sorbet sigils / Luau modes).** The default (`check`) is *nonstrict*:
   only provable `No` results diagnose. A `--strict` flag (and `ascript.toml [check] strict = true`)
   can later turn on *advisory* "this is `any` / un-annotated" hints — but these are **off by
   default** and never on the corpus. v1 ships nonstrict-only; strict is a documented expansion lever.

**Differential test obligation:** a corpus-wide test asserts the *full existing corpus produces zero
new `type-*` diagnostics*, in both feature configs, byte-for-byte. This is the SP10 analogue of the
three-way VM differential — it is the regression lock that makes the feature safe to ship.

---

## 5. Inference scope — the central fork

**Fork A — local bidirectional inference (Luau/TS-local/Sorbet) vs whole-program inference (HM).**

- **Local bidirectional (RECOMMEND).** Infer `let`/`const` types from initializers; infer a
  function's return type from its `return` statements (join of all returns); *propagate annotations
  inward* (a `let xs: array<number> = [...]` checks the literal against the annotation — the
  "checking" direction — and a call argument checks against the declared param — also checking
  direction); infer outward where no annotation exists (the "synthesis" direction). Parameters with
  no annotation are `any` (do NOT infer params from call sites — that is whole-program). This is
  *exactly* what every successful dynamic-language retrofit shipped (Luau local type inference RFC,
  TS, Sorbet). It is **modular** (one function at a time), **fast** (no global constraint solve),
  **incremental** (fits the LSP per-document model — SP4's index is per-file), and **predictable**
  (errors are local to where annotations meet inferred values).

- **Whole-program / global constraint inference.** Infer parameter types from all call sites, infer
  across module boundaries, solve a global constraint set. **Reject for v1:** it is heavier, it makes
  diagnostics non-local and surprising, it fights the LSP's per-file incremental model, it is far more
  likely to produce false positives (a single mis-inference cascades), and no comparable gradual
  retrofit does it first. It is also philosophically wrong for an *advisory* tool — the user opted
  into types where they wrote annotations; inferring globally re-imposes a whole-program discipline
  they explicitly didn't ask for.

**Recommendation: local bidirectional inference, intra-procedural, with annotation boundaries.**
Concretely the bidirectional rules:

- **Synthesis (infer up):** literals → their primitive (`Literal` then widen); array literal →
  `Array(join of elements)`; object literal → `Object`; `new C()` / class construction → `Class(C)`;
  enum variant → `EnumVariant`; arithmetic `a + b` → `Number` (or `String` if either side is provably
  `String`, matching `+` overload); comparisons/`instanceof`/`!` → `Bool`; `await e` → unwrap
  `Future(T)` to `T`; `e?` (propagate) → unwrap `Result(T)`/tuple to `T`; `e!` (unwrap) → unwrap; a
  call to a locally-declared annotated `fn` → its declared/inferred return; everything else → `Any`.
- **Checking (push down):** a slot with a known expected type (annotated binding, annotated param at a
  call, annotated return) checks the expression's *synthesized* type against the expected via
  `assignable`; on `No`, emit `type-mismatch`.

**Sub-fork A′ — return-type inference depth.** Recommend: infer a function's return type from its
own `return`s (join), use it at call sites *within the same file*. Cross-module return inference is a
§7/SP-future expansion (SP4 already drew the cross-module line at "arity yes, types no").

---

## 6. Architecture & relationship to existing lints

**Where it lives.** `src/check/` — same home as every other rule, **feature-independent** (must build
+ pass under `--no-default-features`), **static-only**, reusing the CST front-end
(`lexer → parser → tree_builder → resolve`) and *never* the interpreter (same invariant SP4 carries).

**Shape.** Unlike the existing rules (each a stateless `fn(&ResolvedNode, &ResolveResult, &str) ->
Vec<AsDiagnostic>`), a type checker needs a *stateful pass* (an environment of inferred binding types,
a narrowing overlay, a class/enum table). Two integration options:

- **Option 1 (RECOMMEND): a single new "infer" pass that runs once and emits all `type-*`
  diagnostics**, added to the pipeline in `analyze_with_config` *after* `resolve` and *alongside* the
  `rules::ALL` loop (it consumes `&tree` + `&resolved` like a rule, but internally is a multi-visitor
  pass, not a flat AST scan). It plugs into the *same* `AsDiagnostic`/`Severity`/`LintConfig`/inline-
  `ascript-ignore` machinery — no new diagnostic plumbing, no new CLI surface beyond new rule codes.
- **Option 2:** several independent type rules. Rejected — they would each re-derive inference,
  duplicating work and risking inconsistency.

**New rule codes** (added to `RULE_CODES` in `config.rs`, all default **Warning**, all
`ascript-ignore`-able, all `--deny`/`--warn`/`--allow`-configurable, all immune-exempt unlike
`syntax-error`):

- `type-mismatch` — a value provably the wrong type for an annotated slot (binding / param at a call /
  return). **Subsumes and generalizes** `contract-mismatch` (which is literal-arg-only) and
  `field-default-type` (field default vs declared field type) — see below.
- `type-error` — an operation provably ill-typed regardless of a declared slot, e.g. arithmetic on a
  provably non-`number` (and non-`string`-for-`+`), indexing a non-indexable, calling a non-callable.
  (Highest-value *new* category; nothing today catches `let x: string = "a"; x - 1`.)
- `possibly-nil` — a `T?` value used in a position that requires `T` (member access, call, arithmetic)
  *without* a `nil`-guard narrowing it. **Off by default? FORK** — this is the single noisiest
  category in every retrofit (TS `strictNullChecks` is opt-in for exactly this reason). Recommend:
  ship it **default-Warning but heavily narrowing-gated** (only fires when the type is *provably*
  `T?` and *no* narrowing applies), and reassess noise on the corpus before enabling.

**Relationship to existing conservative lints — layer-above, with two explicit subsumptions:**

| Existing rule | Disposition under SP10 |
|---|---|
| `contract-mismatch` (literal arg vs param) | **Subsumed** by `type-mismatch` (which checks *any* synthesizable arg, not just literals). Keep the code as an alias / fold its tests into `type-mismatch`. Migration fork below. |
| `field-default-type` (field default vs field type) | **Subsumed** by `type-mismatch` (a default is just an expression checked against the field's declared type). |
| `call-arity` | **Orthogonal, KEEP.** Arity is a structural fact, not a type; SP4 already extended it cross-module. The type checker does not touch arity. |
| `invalid-propagate`, `ignored-result`, `dead-recover`, `unawaited-future` | **Orthogonal, KEEP.** These are control/effect lints; some (`unawaited-future`) could *benefit* from `Future(T)` types but don't depend on the checker. |
| `undefined-variable`, `unused-*`, `shadowing`, `missing-return`, `unreachable-code`, etc. | **Orthogonal, KEEP** (resolver/flow lints). |

**Migration fork:** do we (a) delete `contract-mismatch`/`field-default-type` and re-emit their cases
under `type-mismatch` (cleaner, but changes existing diagnostic codes users may have configured), or
(b) keep both codes, with the new checker simply being strictly-more-powerful and the old rules
becoming a guaranteed subset? **Recommend (b) for one release** (the new checker emits `type-mismatch`
for the *superset*; the two legacy codes keep firing on their exact old cases so no config breaks),
then deprecate the legacy codes in a later release. This preserves the zero-false-positive guarantee
incrementally and keeps `ascript.toml` configs working.

**Incremental delivery (the staged plan, matching SP discipline):**

- **T1 — Lattice + assignability + corpus differential.** `CheckTy`, normalization, three-valued
  `assignable`/`join`, and the zero-false-positive corpus differential test (the safety net) — with
  NO diagnostics emitted yet (pure, tested in isolation, exactly how V13-T1 added `Trace` impls before
  they were load-bearing).
- **T2 — Annotated-signature checking + synthesis (no narrowing).** Check annotated binding
  initializers, annotated params at call sites, annotated returns; synthesize expression types. Emit
  `type-mismatch`/`type-error`. This *already subsumes* `contract-mismatch`/`field-default-type`.
- **T3 — Local return-type inference + `nil`-guard narrowing + `possibly-nil`.**
- **T4 — `instanceof` (SP2) + `match` narrowing; early-return flow merge.**
- **T5 — LSP surface:** hover-shows-inferred-type, and `type-*` diagnostics in the editor (reuses the
  existing per-document `analysis` path; no interpreter).
- **(future) — strict mode, stdlib signature stubs, structural fn types, custom type guards.**

Each task carries the corpus differential as a gate; any new corpus diagnostic is a bug in
`assignable` (relax the *guard*, never the differential — same rule as the VM three-way differential).

---

## 7. Syntax additions? (fork — Recommend: NONE in v1)

The design says **no user generics** (no generic *functions*); `array<T>`/`map<K,V>`/`Result<T>`/
`future<T>` are the only parameterized types and they are *built-in*. v1 of the checker **respects
this and adds no surface syntax** — it checks exactly the annotations the language already parses
(`ast::Type` / the `NamedType`/`GenericType`/`OptionalType`/`UnionType`/`TupleType` CST nodes). This
keeps both parsers, the formatter, the tree-sitter grammar, and the LSP keyword set untouched — a
huge scope reduction and the reason v1 is "checker only, no language change."

**Surfaced forks for richer annotations (all DEFERRED, listed so the owner can rank them):**

- **Function/arrow type syntax** `fn(number, string): bool` — would make `Fn` parameterized and
  enable real higher-order checking (callbacks). Big surface-syntax change (lexer/both parsers/
  grammar/fmt/LSP). **Fork: worth it?** Recommend *defer* — the corpus uses few annotated callbacks.
- **Type aliases** `type UserId = number` — pure quality-of-life, no soundness impact, but new syntax
  + a new binding kind in the resolver. **Recommend defer to a focused follow-up.**
- **Custom type-guard predicates** `fn isUser(x): x is User` — unlocks user-defined narrowing (§3).
  New return-annotation form. **Recommend defer.**
- **User generics** `fn first<T>(xs: array<T>): T` — the design's standing non-goal. **Keep deferred**
  unless the owner wants to revisit the design-level non-goal (a separate, larger decision than SP10).
- **Literal / singleton types** (`"GET" | "POST"`) — would make discriminated-union narrowing far
  more powerful (TS's killer feature) but is a real type-system expansion. **Defer.**

---

## 8. Open design questions for the owner (prioritized)

1. **Advisory-only — confirm (highest).** SP10 is a richer lint layer emitting `type-*` diagnostics,
   **never** a compile gate; programs with type errors still run byte-identically on both engines.
   This is forced by the "no static-reject phase / runtime-timed errors / VM↔tree-walker byte-identity"
   invariant. **Confirm this framing before any planning.** (Author is confident this is the only
   option consistent with the codebase.)

2. **Inference scope (§5) — local bidirectional vs whole-program.** Recommend **local bidirectional,
   intra-procedural, parameters default `any`**. Confirm we are NOT doing whole-program/global
   inference in v1.

3. **`possibly-nil` default severity & even *whether* it ships in v1 (§6).** This is the noise risk.
   Recommend default-Warning + aggressively narrowing-gated, validated against the corpus before
   enabling. Owner may want it `--strict`-only.

4. **Legacy-lint migration (§6).** Subsume `contract-mismatch`/`field-default-type` into
   `type-mismatch` immediately (rename, may break configs) vs keep both for one release then deprecate.
   Recommend the latter.

5. **Narrowing scope for v1 (§3).** Recommend: `nil`-guards + `instanceof` (SP2) + `match` patterns +
   early-return merge; NO alias/closure/custom-guard narrowing. Confirm `instanceof` (SP2) lands first
   so SP10 can depend on it.

6. **Strictness ladder (§4/§5).** Ship nonstrict-only in v1 with `--strict` as a documented future
   lever (Sorbet sigils / Luau modes), or design the ladder up front? Recommend ship nonstrict-only,
   design the lever later.

7. **Stdlib/builtin signatures.** Everything imported/native is `any`-returning in v1 (keeps the
   corpus quiet). Owner ranking: is a Sorbet-RBI-style signature-stub program (so `math.abs(x): number`
   etc. flow types) a near-term follow-up or a long-tail one?

8. **Surface-syntax forks (§7).** Rank fn-types / type-aliases / type-guards / literal-types /
   user-generics. Recommend all deferred; v1 = checker-only, zero language change.

---

## 9. Summary recommendation

Build SP10 as an **advisory, local-bidirectional, intra-procedural gradual type checker** living in
`src/check/` as a single stateful inference pass that emits new default-Warning `type-*` diagnostics
through the existing lint machinery. Anchor it on a **three-valued `assignable` (only provable `No`
diagnoses)** over a `CheckTy` lattice that adds only `Any`/`Never`/internal-refinement types to the
existing `ast::Type`. Keep the gradual boundary wide (unannotated/`any`/stdlib ⇒ silent) and **gate
every task on a whole-corpus zero-new-diagnostic differential**, the SP10 analogue of the VM three-way
differential. Ship `nil`-guard + `instanceof` + `match` narrowing; defer whole-program inference, all
new surface syntax, strict mode, and stdlib signatures. It **subsumes** `contract-mismatch` and
`field-default-type` (one-release overlap, then deprecate) and is **orthogonal** to `call-arity` and
the control/effect lints. No language change, no runtime change, no VM change, no byte-identity risk.

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
