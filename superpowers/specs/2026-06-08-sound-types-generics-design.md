# AScript Sound Gradual Types & Generics — Design (TYPE)

- **Status:** Draft for review
- **Date:** 2026-06-08
- **Code:** TYPE (the type-track keystone of the Serious Language campaign — see `goal.md`)
- **Depends on:** NUM (the `int`/`float`/`number` kinds the lattice must subtype correctly —
  `2026-06-08-numeric-model-design.md` §5; NUM adds `CheckTy::Int`/`CheckTy::Float` and desugars `number`
  to `Union([Int, Float])`, NUM §"Static checker", so TYPE **rebases onto NUM's Type/CheckTy variants**),
  ADT (`2026-06-08-algebraic-types-design.md` — enums-with-payloads; a generic enum must range a param
  over a variant payload type), IFACE (`2026-06-08-interfaces-design.md` — structural interfaces, the
  only form of generic *bound* this spec admits). **Both ADT and IFACE specs now exist** (they are no
  longer `goal.md` stubs); TYPE designs against their actual locked decisions and pins the exact
  integration points (§4.5, §10). In particular, IFACE §6 explicitly **defers its static conformance
  half to TYPE** ("layers on with TYPE") and reserves `CheckTy::Interface(InterfaceId)` + an
  `InterfaceInfo` table (IFACE §6, `ty.rs:64` / `table.rs:36`); TYPE implements that static half. IFACE
  also already **owns** the `implements-violation` diagnostic (its only Error-level code — see §4.5
  below for the ownership split).
- **Depended on by:** DX (inlay hints + hover consume the inferred-type surface this spec exposes),
  IFACE (its *static* conformance check is implemented here), and every future spec that annotates code.
- **Engines:** **neither** — this is a **static-only** spec. No tree-walker change, no VM change, no
  `.aso` change, no runtime change. `vm_differential.rs` is untouched by construction (§7).
- **Breaking:** **behaviorally no, diagnostically yes.** No program's *runtime output* changes. But a
  program that is **annotated and provably wrong** that compiled-and-ran before (emitting at most an
  advisory Warning) now **fails `ascript check`/`run`'s type gate with a blocking Error**. That is the
  entire point of the spec: typed code becomes a guarantee. The example corpus is migrated to stay
  clean (Gate 5 / Gate 7).

---

## 1. Summary & motivation

AScript already ships an **advisory** static gradual type checker (SP10, `src/check/infer/`). It has the
right bones — a three-valued lattice (`CheckTy` + `Compat3{Yes,No,Unknown}`, `src/check/infer/ty.rs:41`
/ `:76`), bidirectional `synth`/`check_against` (`src/check/infer/pass.rs:569` / `:582`), flow narrowing
(nil-guard / `instanceof` / `match`), and a class/enum symbol table (`table.rs`). Its cardinal
discipline is **"only a provable `No` ever emits; everything uncertain is `Unknown` and silent"**
(`ty.rs:8`), which is exactly what keeps the untyped corpus at zero false positives. But it has two
deliberate gaps that block AScript from being a *serious* typed language:

1. **It is purely advisory.** Every type diagnostic is a default-`Warning` (`src/check/config.rs:47`),
   so even a provably-wrong annotated program builds and runs. A `Warning` does not fail the run
   (`tally`, `src/main.rs:165` — only `Severity::Error` sets `any_error`). Annotations are therefore
   *documentation that the tools sometimes complain about*, not a contract the compiler enforces. A
   serious language must make typed code a **guarantee**.

2. **There are no user-defined generics.** `CheckTy` has the built-in constructors `array<T>`/`map<K,V>`/
   `future<T>`/`Result<T>` (`ty.rs:56`), but a user cannot write `fn map<A,B>(...)`, `class Box<T>`, or a
   generic enum/interface. The lattice has no type variable. Without generics you cannot type the
   standard library's own combinators (`map`, `filter`, `reduce`, a `Stack<T>`, a `Result<T,E>`), which
   is a hard requirement for the self-hosting goal and for IFACE bounds to mean anything.

This spec closes both gaps **without touching either engine**. It flips the checker from advisory to
**blocking-for-annotated-code** (the soundness upgrade, §3) and adds **user-defined generics** with
unification-based inference and interface bounds (§4). The gradual escape (`any`, unannotated positions)
is preserved verbatim as the explicit opt-out — never a silent default (Siek–Taha gradual typing; the
`Unknown ⇒ silent` discipline is unchanged).

### The one-sentence thesis

> **`Compat3::No` already means "provably wrong." Today it whispers (Warning). After TYPE, when the slot
> was *annotated*, it blocks (Error). `Unknown` still says nothing. That is the whole soundness model —
> generics just give `synth`/`assignable` more true facts to reason from.**

## 2. The soundness model in one paragraph

The lattice is *already* sound in the sense that matters: `assignable` returns `No` **only** when a value
is provably wrong for a slot, and `Unknown` everywhere it cannot prove anything (`ty.rs` rules 1–11). The
soundness *upgrade* is therefore not a change to *what* `No` means — it is a change to *what happens* when
`No` fires **on an annotated slot**: it becomes a blocking diagnostic. Because `No` already never fires on
untyped/`any`/uncertain code, **upgrading its severity cannot introduce a single new false positive on
`examples/**`** — the Gate-5 guarantee falls directly out of the existing `Unknown ⇒ silent` invariant
(§3.4 proves this). Generics extend `synth` with new true facts (a `Box<int>`'s `.value` is `int`); they
**must not** make `assignable` return `No` where it would have returned `Unknown` for an uninstantiable
or unconstrained variable — the variable defaults to `Unknown`-yielding behavior, exactly like `any`
(§4.2). This keeps the gradual gate intact through both changes.

---

## 3. The soundness model — blocking vs gradual

### 3.1 The precise rule (LOCKED)

A `type-*` diagnostic is emitted at one of two severities, decided by a single property of its **slot**:

> **A `type-mismatch` or `type-error` is `Severity::Error` (BLOCKING) iff the destination slot is
> *syntactically annotated* in user source (or is the annotated/declared component of one). Otherwise —
> and for every `possibly-nil`, and for every diagnostic whose `assignable` result was `Unknown` (which
> never emits at all) — it stays `Severity::Warning` (advisory).**

"Annotated slot" is defined structurally, by where `check_against` is invoked with an `expected` that was
**lowered from a user type node** (not inferred, not `Any`-defaulted):

| Slot | Annotated? | Today | After TYPE |
|---|---|---|---|
| `let x: T = v` (explicit `: T`) | **yes** | Warning | **Error** |
| `fn f(p: T)` called `f(v)` (annotated param) | **yes** | Warning | **Error** |
| `fn f(): T { return v }` (annotated return) | **yes** | Warning | **Error** |
| `field: T = default` / `field: T` assigned | **yes** | Warning | **Error** |
| `let x = v` (no annotation), `x` later misused | **no** | Warning | Warning |
| arithmetic on an *inferred* non-number | **no** | Warning | Warning |
| anything where `assignable` returned `Unknown` | n/a | (silent) | (silent) |
| any operand/slot whose type is `any` | n/a | (silent) | (silent) |
| `possibly-nil` (a *latent* nil, not a type clash) | n/a | Warning | Warning |

The discriminator is **provenance of `expected`**, which the pass already tracks implicitly: every
blocking call site in `pass.rs` is one where `expected = CheckTy::from_type_node(<user node>, table)`.
There are **exactly four** such annotated-slot sites — and they are NOT uniform: two route through
`check_against`, two inline their own `assignable` + `emit`:

1. **`walk_let` with an `ann`** (`pass.rs:206`) — via `check_against`.
2. **`walk_return` with a declared return** (`pass.rs:245`) — via `check_against`.
3. **`check_call_args`** (annotated param; `from_type_node` at `pass.rs:891`) — **inline** `assignable`
   (`:892`) + `emit` (`:899`), does **not** call `check_against`.
4. **`check_field_default`** (declared field type; `from_type_node` at `pass.rs:541`) — **inline**
   `assignable` (`:552`) + `emit` (`:561`), does **not** call `check_against`.

The **inferred** call sites (`let` with no annotation, an inferred return flowing onward) never carry a
user-annotated `expected`, so they stay advisory. Because two of the four sites bypass `check_against`,
the blocking decision is realized as a **severity argument on `emit` itself** (the one common sink),
passed `Error` at all four annotated sites (via `check_against` for #1/#2, directly for #3/#4) and
`Warning` elsewhere (§5.3) — NOT a flag that only lives on `check_against`.

**Why `type-error` (arithmetic) is blocking only on an annotated operand:** `a - "x"` where `a`'s type was
*inferred* to a string is advisory (the programmer never promised a type); `let a: number = ...; a - "x"`
is over an annotated slot and the error already surfaces via the annotated binding's `check_against`. To
keep the rule crisp, `type-error` from `flag_non_numeric`/`synth_unary` (`pass.rs:724`/`:757`) is
**blocking iff a provably-non-number operand's type traces to a user annotation** (a param/let/field type
node); otherwise advisory. The conservative default when provenance is unclear is **advisory** (never
over-block).

**Why `possibly-nil` stays advisory:** it flags a *latent* runtime panic (a `T?` deref without a guard),
not a value that is *provably the wrong type for an annotated slot*. It is a true gradual-typing nicety,
not a contract violation, and blocking it would punish the extremely common "I'll guard it later /
it's-fine-here" pattern and risk corpus false positives. It remains a default-`Warning` (`--deny-warnings`
/ a project `ascript.toml` can still promote it for teams that want it).

### 3.2 How blocking is realized (no new machinery)

The exit-status path already exists: a `Severity::Error` diagnostic sets `any_error` in `tally`
(`src/main.rs:165`), which fails `ascript check` and `ascript run`'s pre-run gate. So "blocking" =
**emit at `Severity::Error` instead of `Severity::Warning`** for the annotated case. `emit` (`pass.rs:118`)
currently hard-codes `Severity::Warning`; it gains a severity argument (§5.3). `config.effective`
(`src/check/config.rs:92`) still applies, so a project can *downgrade* a blocking type error to a warning
via `ascript.toml` if it explicitly opts out — but the **default is blocking** (the soundness default;
backward-compat is not a constraint, `goal.md`). The three codes remain in `RULE_CODES`; their *default*
severity for the annotated case is the only thing that changes.

### 3.3 The gradual escape (unchanged, restated for precision)

- **`any`** lowers to `CheckTy::Any` (`ty.rs:103`), and `assignable` rule 1 (`ty.rs:255`) makes `Any`
  assignable to/from everything → always `Yes` → never a diagnostic. An `any`-typed slot or operand is
  the explicit, named opt-out. **This never changes.**
- **An unannotated position** synthesizes a concrete type for *its own* downstream reasoning but is
  **never itself a blocking slot** — there is no user annotation to violate. (It can still produce an
  *advisory* warning if it is provably misused, exactly as today.)
- **An unknown type name** lowers to `Any` (`ty.rs:115`) — we never invent a type, so a typo'd
  annotation degrades to gradual rather than producing a spurious blocking error. (A *separate*,
  pre-existing lint may warn on an unresolved type name, but the type checker treats it as `Any`.)
- **Anything the lattice cannot prove** is `Unknown` and silent (`ty.rs:434`).

### 3.4 The Gate-5 guarantee (the proof obligation)

> **Theorem (zero new false positives on `examples/**`):** Upgrading the severity of an annotated-slot
> `No` from Warning to Error adds **no** diagnostic to any file that did not already emit that exact
> diagnostic. Therefore `examples/**` — which emits **zero** `type-*` diagnostics today in both feature
> configs — still emits zero **after** the severity flip.

*Proof sketch.* The set of *emitted* diagnostics is unchanged by a severity remap (`emit` still only runs
on a `Compat3::No`; §3.1 changes the `severity` field, never the emit predicate). `examples/**` emits no
`type-*` diagnostic today (the SP10 invariant, `CLAUDE.md` SP10 §"Invariants"), so it emits none after.
The only way the flip could break Gate 5 is if **generics** (§4) cause `assignable` to newly return `No`
on corpus code. The mitigations (each a hard rule, tested):

1. **An unconstrained / un-inferred type variable is `Unknown`-yielding**, never `No` (§4.2). A `T` we
   could not solve behaves like `Any` for assignability.
2. **Generic instantiation only ever *adds resolved facts*** (`Box<int>.value : int`). A *failed*
   instantiation (arity mismatch, occurs-check failure, bound violation) is itself an annotated-slot
   error **only when the user wrote the generic annotation** — and the corpus, after migration, contains
   no such mistakes (Gate 7 migrates examples; if a generic example is provably wrong it is a *bug in the
   example*, fixed there, never by relaxing the gate).
3. **The corpus is migrated, not exempted** (Gate 7). Any new corpus `type-*` is triaged: if the example
   is genuinely wrong, fix the example; if `assignable` returned `No` where it should be `Unknown`, **the
   bug is in `assignable`/unification — fix the checker, default to `Unknown`, never relax the gate**
   (the standing SP10 rule, `CLAUDE.md`).

A CI assertion (already implied by Gate 5, made explicit here) runs `analyze` over every `examples/**`
file in both feature configs and asserts **zero `type-mismatch`/`type-error`** diagnostics survive
(`possibly-nil` is also currently zero and stays asserted). This is the regression tripwire for the whole
spec.

---

## 4. Generics

### 4.1 Surface syntax

Type parameters are declared in angle brackets immediately after the declared name, and supplied (or
inferred) in angle brackets at the use site. **Two distinct parser concerns, with different ownership:**

1. **The `>>` token split** that nested generics require is **already solved by NUM**, but **only in
   known type-argument position** — the type-argument parser, when it expects a closing `>` and sees
   `Shr`/`Ge`, consumes one `>` and re-buffers the rest (NUM §3.4 / §3.6, required test `map<int,
   array<int>>`; `a >> b` in *expression* position is explicitly left untouched, NUM §3.6). TYPE reuses
   this verbatim for type-parameter lists and for type arguments inside **type annotations**, **adding
   nothing to the lexer**.
2. **Expression-level explicit type arguments** (`Box<int>(5)`, `map<string, number>(xs, f)`) are a
   **genuinely NEW parser deliverable** that NUM does *not* cover: in *expression* position, `Box < int >
   (5)` is lexically ambiguous with the comparison chain `(Box < int) > (5)`. NUM's split only fires once
   the parser already *knows* it is in a type-arg list; it does not decide *whether* a `<` after a value
   expression opens a type-arg list at all. This disambiguation (speculative parse / declared GLR
   conflict) is owned by TYPE and gets its own frontend + tree-sitter conformance tests (§6, §8). Only the
   **lexer** is unchanged from NUM; the expression-grammar work is new.

```javascript
// Generic function. A,B are type params; bounds are optional (T: Iface).
fn map<A, B>(xs: array<A>, f: fn(A) -> B) -> array<B> {
  let out: array<B> = []
  for (x of xs) { out.push(f(x)) }
  return out
}

let lengths = map([ "a", "bb" ], fn(s) { return s.length })   // A=string, B=number, inferred

// Generic class.
class Box<T> {
  value: T
  init(value: T) { self.value = value }
  fn get() -> T { return self.value }
}
let b = Box<int>(5)          // explicit
let c = Box(5)               // T=int inferred from the init arg
let n: int = c.get()         // get() : int — sound

// Generic enum (ADT) — payload types range over the param.
enum Option<T> { Some(value: T), None }
enum Result2<T, E> { Ok(value: T), Err(error: E) }

// Generic interface (IFACE) + a bounded type param.
interface Container<T> { fn len() -> int  fn at(i: int) -> T }
fn first<T, C: Container<T>>(c: C) -> T { return c.at(0) }
```

Notes:
- **`fn(A) -> B` is a parameterized function type** — a strict extension of today's bare `CheckTy::Fn`
  (`ty.rs:54`, "AScript has no fn-arity types today"). TYPE introduces `CheckTy::FnSig` (§5.1) so a
  higher-order generic like `map` can flow `A`/`B` through the callback. A bare `fn` (no signature) stays
  `Fn` and is `Unknown`-compatible with any `FnSig` (gradual — never a false positive on the corpus,
  which uses bare `fn`).
- **Bounds use `:`** (`T: Container<T>`) and admit **only interfaces** (IFACE) — never a class, never a
  union (§4.5). An unbounded `T` is bound by the implicit top, i.e. behaves gradually.
- **Type params are contextual**, scoped to their declaration; they shadow nothing at runtime (generics
  are erased — §7).

### 4.2 `CheckTy::Var` + the gradual-by-default rule

The lattice gains one variant:

```rust
/// A generic type variable. `id` is unique within an instantiation context; `bound`
/// is the interface constraint (`None` = unbounded ⇒ gradual top).
Var(VarId, Option<Box<CheckTy>>),
```

The **cardinal rule that protects Gate 5**: an *unsolved* or *unbounded* `Var` is treated like `Any` for
assignability — `assignable(Var(_,None), _)` and `assignable(_, Var(_,None))` both return **`Unknown`**
(never `No`). A *bounded* `Var(_, Some(Iface))` is assignable-checked **against the bound** (so
`first<T,C: Container<T>>` can call `c.at` soundly), but a value flowing *into* a `Var` slot is `Unknown`
unless it provably violates the bound. **`Var` is the gradual escape generalized** — it never manufactures
a `No`. This is the single most important invariant in §4 and is unit-tested directly (§9).

After a `Var` is **solved** by unification (§4.3) it is **substituted** by its solution everywhere before
any `assignable` that could emit — so the user sees `Box<int>` / `argument expects int, found string`,
never a raw `Var`. An *unsolved* `Var` at a leaf (a genuinely un-inferrable param) substitutes to `Any`
for display and for assignability (gradual).

### 4.3 Unification (with occurs-check)

Inference is **local and constraint-based** (a small unifier, not whole-program Hindley–Milner — see §10
rejected). For a generic call/construction:

1. **Freshen.** Instantiate the declaration's type params to fresh `Var`s (`map<A,B>` → `Var(a)`,
   `Var(b)`), substituted through the param types and return type.
2. **Collect constraints.** For each supplied argument, unify `synth(arg)` against the (freshened) param
   type. `fn(A)->B` callbacks unify structurally (param-by-param, then return).
3. **Solve.** A standard union-find unifier over `Var`s:
   - `unify(Var v, t)` / `unify(t, Var v)` → bind `v := t` (after **occurs-check**: if `v` occurs in `t`,
     the binding is rejected as **non-unifiable** → the whole inference yields `Unknown` results, never a
     `No`; an infinite type is a gradual give-up, not an error).
   - `unify(C<a..>, C<b..>)` → unify componentwise (same head, same arity).
   - `unify(t1, t2)` for concrete `t1 != t2` → **mismatch**; recorded as a *constraint failure* (handled
     per §4.4).
   - Any side being `Any` → succeeds vacuously (gradual).
4. **Substitute & check.** Apply the solution to param types and return type. Then run the *normal*
   `assignable` of each arg against its **solved** param type — this is the step that can emit a blocking
   `type-mismatch` **when the param was annotated** (it always is, for a generic decl). A param that
   stayed an unsolved `Var` → `Any` → `Yes`/`Unknown` (gradual).

The unifier is **depth-capped** (reuse `TYPE_DEPTH_CAP = 8`, `ty.rs:23`) — past the cap, give up to
`Unknown`. It is **occurs-checked** (no infinite types, no non-termination). It is **bounded in width**
(reuse `UNION_WIDTH_CAP`). All three caps already exist and are the proven zero-false-positive guards.

### 4.4 Inference vs explicit type arguments

- **Explicit** `Box<int>(5)` / `map<string,number>(...)`: the type args are lowered directly to `CheckTy`s
  and used as the instantiation (no freshening needed). If an explicit arg **conflicts** with an inferred
  constraint (e.g. `map<string,number>([1,2], ...)` — `int` array vs `string` param), that's a blocking
  `type-mismatch` **on the annotated arg** (the user both annotated the generic and supplied a wrong
  value).
- **Implicit** `Box(5)` / `map([...], f)`: solve from arguments (§4.3). **If a param's `Var` is never
  constrained** (e.g. a phantom type param, or `map([], f)` over an empty array where `A` is
  unconstrained), it stays unsolved → `Any` → gradual. Crucially, **un-inferred ⇒ gradual, not an
  error** (an empty-array `map` must not error). This mirrors TypeScript's behavior of falling back to a
  permissive type when inference is underconstrained.

  **Empty-array element type (pinned against `synth_array`, `pass.rs:904`).** `synth([])` returns
  **`CheckTy::Array(Box::new(CheckTy::Any))`** *today* — the `elems.is_empty()` branch short-circuits to
  `array<any>` (`pass.rs:913`–`914`), *before* the `acc = CheckTy::Never` accumulator is ever reached (the
  `Never` seed is used only for a *non-empty* literal, `:916`). So an empty array's element type is `Any`,
  **never `Never`**. This matters for `map([], f)`: unifying the param `array<A>` against `array<Any>`
  binds `A := Any` (rule "any side being `Any` succeeds vacuously", §4.3) → `A` stays gradual → the
  callback/return type is `Any` → **no diagnostic**. The required invariant: TYPE **must not** change
  `synth_array` to seed an empty literal with `Never` (which, being the bottom that is `assignable` to
  everything, `ty.rs:260`, could flow through unification as a *concrete* lower bound and risk a `No`
  downstream). The empty-array element type stays `Any`, pinned by a unit test (§9.3: `map([], f)` emits
  nothing).
- **Return-site inference** (`let lengths = map(...)`): the solved return type (`array<B>` → `array<number>`)
  is what `synth_call` returns, so a subsequent annotated slot (`let xs: array<string> = map(...)`) gets a
  precise, soundly-checked type. If the return contains an unsolved `Var`, it surfaces as `Any` (gradual).

### 4.5 Bounds (interface constraints — ties to IFACE)

A bound `T: Iface` constrains a type param to **structurally conform** to an interface.

**Ownership split with IFACE (pinned).** Per IFACE §6, the *runtime* conformance half (the structural
`instanceof` predicate) ships first, independent of TYPE; the *static* half — `CheckTy::Interface(InterfaceId)`,
the `InterfaceInfo` table (`table.rs:36`), and the `assignable`/`conforms` rules — "layers on with TYPE"
and is implemented **here**. So:
- **TYPE provides the structural `conforms` machinery** (`CheckTy::Interface(InterfaceId)` + the
  `Class(c) → Interface(i)` / `Interface → Interface` `assignable` arms, IFACE §6) used for *both* generic
  bounds (this section) *and* ordinary interface-typed slots.
- **IFACE registers and emits `implements-violation`** (it is declared in IFACE as IFACE's only
  Error-level code, fired at the `implements` clause when `class C implements I` provably does not conform
  — IFACE §6/§"Lint config"). TYPE does **not** introduce `implements-violation`; it consumes the same
  `conforms` predicate TYPE provides. (The review suggested TYPE register it, but the IFACE spec already
  claims and specifies it — we follow the real spec: TYPE owns `conforms`, IFACE owns the lint. The two
  are wired so the lint calls TYPE's `conforms`.)

To match IFACE's reserved names, `CheckTy` gains **`Interface(InterfaceId)`** (not `IfaceId`) and the
table gains an **`InterfaceInfo`** vector (not `IfaceInfo`) — the names IFACE §6 already fixes. The
conformance predicate `conforms(t, Interface)`:
- `Yes` if `t` is a class/instance providing every required method with **assignable** signatures;
- `No` if `t` provably lacks a method or has a provably-incompatible *typed* signature **and `t` is fully
  concrete** (a present-but-untyped method yields `Unknown` for that method ⇒ overall `Unknown`, IFACE §6);
- `Unknown` otherwise (the gradual default — a partially-known `t` never blocks).

A bound is enforced at instantiation: after solving `T := S`, check `conforms(S, bound)`. A provable
`No` is a blocking `type-mismatch` **only when the instantiation came from a user annotation** (it does —
the bound is on a declared generic). Inside the generic body, a bounded `T` may have its bound's methods
called soundly (`c.at(0)` on `C: Container<T>` synthesizes `T`). **Bounds admit only interfaces**, which
keeps conformance structural and decidable; class bounds / union bounds are rejected (§10).

**Gate-5 safety of `implements-violation` on `examples/**`.** `implements-violation` only fires when a
class *explicitly writes* `implements I` and the checker can *prove* non-conformance (a required method
missing or a *typed* signature provably clashing); a present-but-untyped method is `Unknown` → silent
(IFACE §6 gradual gate). No `examples/**` file claims an `implements` it does not satisfy (and the corpus
is migrated per Gate 7), so this code emits **zero** on the corpus in both feature configs — it cannot
trip Gate 5. (This is IFACE's proof obligation, restated here because TYPE supplies the `conforms` it
depends on.)

### 4.6 Variance — INVARIANT for v1 (LOCKED)

All **user** generic type constructors (`ClassApp`/`EnumApp`/parameterized interfaces) are **invariant**
in v1: `Box<Dog>` is **not** assignable to `Box<Animal>` (neither up nor down).

**The honest state of the built-ins (NOT a model for the user heads).** The existing built-in
constructors in `assignable` rule 8 (`ty.rs:398`–`431`) are **covariant**, not invariant:
`(Array(s), Array(d)) => s.assignable_depth(d, …)` recurses **one-directionally** (`ty.rs:400`), as do
`Future`/`Result`/`Map`/`Tuple` (`ty.rs:401`–`417`). So `array<Dog>` *is* `assignable` to `array<Animal>`
today (covariant), and `array<int>` ↮ `array<string>` is `No` only because `int`/`string` are
concrete-distinct on the single recursion. TYPE does **not** rely on the built-ins being invariant and
makes **no** claim that they are — it leaves rule 8 unchanged (changing the built-ins' variance is a
behavioral diagnostic change on the existing corpus and is explicitly out of scope; §10). The **new**
generic heads get their own, genuinely invariant arm — a real addition, not a reuse of rule 8.

**The invariant rule for `ClassApp`/`EnumApp`/parameterized interfaces (NEW arm, §5.2).** Same head,
same arity → check each type-argument pair **in BOTH directions** and combine:

```text
invariant_args(sargs, dargs):
  acc = Yes
  for (s, d) in zip(sargs, dargs):
    if s or d is an unsolved/unbounded Var  → acc = meet(acc, Unknown)   // Var-bias: never No
    else:
      fwd  = s.assignable(d);  bwd = d.assignable(s)
      // No only when the pair is provably distinct in BOTH directions on concrete args
      if fwd == No and bwd == No                 → return No
      else if fwd == Yes and bwd == Yes          → acc = meet(acc, Yes)
      else                                       → acc = meet(acc, Unknown)
  return acc
```

So `Box<S>` vs `Box<D>`: **`No` only when every type-arg pair is concrete-and-distinct both ways**
(`Box<int>` ↮ `Box<string>`); **`Yes`** only when every pair is mutually assignable (i.e. equal-up-to-gradual);
any pair involving a `Var` or an `Any` → **`Unknown`** (gradual). This is what makes `Box<Dog>` ↛
`Box<Animal>` (the locked decision): `Dog → Animal` is `Yes` but `Animal → Dog` is `No` (a subclass
mismatch one way), so the pair is neither both-`Yes` nor both-`No` → **`Unknown`** (silent — not a
*blocking* assignability, which is the v1 limitation: you cannot pass `Box<Dog>` where `Box<Animal>` is
wanted, and the checker stays silent rather than emitting, preserving Gate 5). A *strict* nominal mismatch
(`Box<int>` vs `Box<string>`) is the only case that fires `No`. The `Var`-bias clause guarantees the
single most important invariant: **any arg involving an unsolved `Var` ⇒ `Unknown`, never `No`** (§4.2).

**Justification for invariance (vs declaration-site or use-site variance):**
- **Soundness for free.** Mutable generic containers (`Box<T>` with a settable `value`, a `Stack<T>` with
  `push`) are **unsound** under naive covariance (the classic "covariant array store" hole: writing an
  `Animal` into a `Box<Dog>` typed as `Box<Animal>`). Invariance is the only choice that is sound
  *without* tracking the in/out position of every type-param use — which AScript's `CheckTy` does not
  encode and v1 will not add.
- **Matches Go generics** (Go has no variance; type sets are invariant) and **Rust** (no subtyping
  variance on user generics beyond lifetimes). These are the closest design siblings and both ship
  invariant user generics successfully.
- **Declaration-site variance** (Kotlin/C#/Scala `in`/`out`) requires variance annotations + a
  position-checker (every type-param occurrence validated as co/contra/invariant) — a whole subsystem for
  a v1 gain (read-only covariant containers) that AScript can get later, additively, without breaking
  invariant code. **Use-site variance** (Java wildcards `? extends`) is notoriously hard to teach and to
  infer. Both are **rejected for v1** (§10), documented as a future additive extension.
- **The documented limitation:** you cannot pass a `Box<Dog>` where a `Box<Animal>` is wanted even when
  it would be safe (read-only use). The workaround is a generic function (`fn f<T>(b: Box<T>)`), which is
  parametric and needs no variance. This limitation is **stated in the docs** (no silent surprise; pillar
  1 / Gate 6).

### 4.7 Gradual + generics (the corpus-safety surface)

- **An un-annotated generic call stays `Unknown`.** Calling `map(xs, f)` where `xs : any` keeps `A = any`
  → `array<any>` → gradual. No corpus code that passes untyped data through a generic can be blocked.
- **`array<any>` / `map<any,any>` etc.** remain the explicit wildcard containers (each component `Any`,
  rule 1 → `Yes`).
- **A generic over an unknown type name** (`Box<Widget>` where `Widget` is unknown) lowers the arg to
  `Any` (`ty.rs:115`) → gradual.
- **Mixing typed and untyped** at a generic boundary always biases to `Unknown` (the join/meet of
  `Any` with anything is gradual), never `No`.

---

## 5. Type-system internals

### 5.1 `CheckTy` additions (`src/check/infer/ty.rs`)

```rust
pub enum CheckTy {
    // ... existing variants (ty.rs:41) ...
    /// A generic type variable (gradual-by-default; never yields `No` unsolved).
    Var(VarId, Option<Box<CheckTy>>),     // (id, optional interface bound)
    /// A parameterized function type (extends the bare `Fn`): params + return.
    FnSig(Vec<CheckTy>, Box<CheckTy>),
    /// A NOMINAL-by-id but PARAMETERIZED class/enum instantiation: the head id plus
    /// its solved type arguments. `Class(id)`/`Enum(id)` remain the zero-arg forms.
    ClassApp(ClassId, Vec<CheckTy>),
    EnumApp(EnumId, Vec<CheckTy>),
    /// A structural interface (IFACE), identified by table id; carries no args here
    /// (a generic interface instantiation is an `Interface`-headed `…App`-style entry
    /// only if/when IFACE needs it; v1 keeps interfaces by id for conformance).
    Interface(InterfaceId),
}
```

- `Var`, `FnSig`, `ClassApp`, `EnumApp` are added to `widen` (each widens its components / a leftover
  `Var` widens to `Any`), `display` (`Box<int>`, `fn(int) -> string`, `T`), `discriminant_order` /
  `secondary_key` (`ty.rs:554`/`:582`, for canonical sorting), and `assignable` (§5.2).
- **NUM interaction:** NUM adds `CheckTy::Int`/`CheckTy::Float` and makes `number` desugar to
  `Union([Int,Float])` (NUM §5). TYPE's unification and `assignable` MUST treat that union correctly:
  `int` assignable to a `T` solved as `number` is `Yes` (member of the union — rule 9 already handles
  unions, `ty.rs:287`); unifying `Var v` against `int` binds `v := int`, and a later `number`-annotated
  slot accepts it (union membership). **No special case** — the existing union rules + NUM's desugaring
  compose. A required test: `fn id<T>(x: T) -> T` called `id(5)` returns `int`, assignable to both
  `: int` and `: number`, **not** to `: string` (blocking).

### 5.2 `assignable` for the new variants

Inserted into `assignable_depth` (`ty.rs:247`) in the existing rule order, all **biased to `Unknown`**:

- **`Var` (rule 1.5, before everything concrete):** unsolved/unbounded `Var` on either side → `Unknown`.
  A bounded `Var` on the *destination* side → check the *source* `conforms` the bound (`No` only on a
  provable concrete failure). A `Var` is never compared for concrete `No` against a primitive.
- **`FnSig` vs `FnSig`:** params **contravariant**, return **covariant** — but since a `No` requires *all*
  components provable and the corpus uses bare `fn`, this almost always lands on `Unknown`. `FnSig` vs
  bare `Fn` → `Unknown` (gradual; bare `fn` is the untyped-callback escape).
- **`ClassApp(c, sargs)` vs `ClassApp(d, dargs)` (NEW invariant arm, §4.6 — NOT a reuse of rule 8):**
  `No` if `c`/`d` are provably unrelated nominal heads (or differing arity) *or* same head with
  `invariant_args(sargs, dargs) == No` (every type-arg pair concrete-and-distinct in BOTH directions);
  `Yes` if same head and `invariant_args == Yes`; else `Unknown` (including any arg involving a `Var` or
  `Any`). `ClassApp(c, _)` vs `Object` → `Yes` (an instance is an object, mirroring rule 6). `Class(c)`
  (zero-arg) vs `ClassApp(c, _)` → `Unknown` (a raw class ref vs a parameterized one is not provably
  wrong). **This arm is genuinely invariant — it calls `assignable` in both directions on each component
  (§4.6), unlike the covariant built-in rule 8, which TYPE leaves untouched.**
- **`EnumApp`** mirrors `ClassApp` (same head + the same `invariant_args` both-directions check).
- **`Interface(i)` as destination:** `conforms(src, i)` (§4.5) — `No` only on a fully-concrete provable
  failure.

Every new arm's **default fall-through is `Unknown`** (rule 11, `ty.rs:434`) — the lattice's silence
guarantee is preserved variant-by-variant.

### 5.3 The pass wiring (`src/check/infer/pass.rs`)

The inference pass is where TYPE does the bulk of its work; it stays **a single visitor wired after
`rules::ALL`** (`analyze.rs:77`→`:83`, via `infer::check`, `mod.rs:26`). Changes:

1. **`emit` gains a severity** (`pass.rs:118`, today hard-codes `Severity::Warning` at `:127`):
   `fn emit(&mut self, code, range, message, sev: Severity)`. **This is the single chokepoint where
   blocking is realized** — every blocking decision is expressed by the `sev` passed here, so ALL FOUR
   annotated sites are covered regardless of whether they route through `check_against`. `possibly-nil`
   and inferred-slot diagnostics pass `Warning`; annotated-slot `type-mismatch`/`type-error` pass
   `Error`. The `legacy_spans` de-dup and `suppress_emit` gating are unchanged.
2. **The four annotated sites pass `Error` to `emit` — two via `check_against`, two inline.** The pass
   has **four** annotated-slot emit sites, and only TWO currently funnel through `check_against`; the
   other two inline their own `assignable` + `emit` and must be updated *directly*:
   - **`walk_let` with an `ann`** (`pass.rs:206`) → `check_against` (`pass.rs:569`). `check_against` gains
     a `blocking: bool` and passes `Error`/`Warning` to `emit` accordingly.
   - **`walk_return` with a declared return** (`pass.rs:245`) → also `check_against` (same `blocking` flag).
   - **`check_call_args`** (`pass.rs:868`) — **inline** (`assignable` at `pass.rs:892`, `emit` at `:899`):
     does NOT call `check_against`. It must pass `Severity::Error` directly to its own `emit` call (the
     `expected` is always lowered from an annotated param node — `from_type_node` at `:891`).
   - **`check_field_default`** (`pass.rs:534`) — **inline** (`assignable` at `pass.rs:552`, `emit` at
     `:561`): does NOT call `check_against`. It must pass `Severity::Error` directly to its own `emit`
     call (the `expected` is the field's declared type node, `from_type_node` at `:541`).

   So the `blocking`/severity is a **new argument on `emit`** threaded through `check_against` for the
   first two sites and passed *directly* at the two inline sites — NOT a flag that only lives on
   `check_against`. Inferred-context `check_against` callers (and `possibly-nil`/`type-error` from
   inferred operands) pass `Warning`.
3. **`Table::build` records type params + bounds + generic enum/interface info** (`table.rs:47`): each
   `ClassDecl`/`EnumDecl`/`FnDecl`/`InterfaceDecl` with a `TypeParams` node stores its param names + bounds;
   `ClassInfo`/`EnumInfo` gain `type_params: Vec<(String, Option<CheckTy>)>`; the `InterfaceInfo` table
   IFACE §6 reserves (`table.rs:36`) is populated (its method-set signatures lowered to `CheckTy`). `from_type_node` (`ty.rs:85`) learns: a `NamedType` whose text matches an **in-scope type param**
   → `Var`; a `GenericType` whose head is a user class/enum/interface → `ClassApp`/`EnumApp`/parameterized
   interface (today such heads fall to `Any`, `ty.rs:150` — a strict, gradual-preserving upgrade).
4. **A `subst: HashMap<VarId, CheckTy>` instantiation context** is threaded through `synth_call` /
   `synth_call` for constructors (`pass.rs:785`) and the generic body walk: freshen → unify → substitute →
   `assignable` (§4.3). The unifier is a new `infer/unify.rs` module (occurs-check + union-find).
5. **`fn_return_type` / `method_return` instantiate** (`pass.rs:1121`, `table.rs:192`): a generic fn/method
   return is the *substituted* return type at the call site (so `Box<int>.get()` synthesizes `int`).

Everything else in `pass.rs` (narrowing, `block_always_returns`, `infer_return`, hover collection) is
**unchanged** — generics only enrich the types flowing through the existing machinery.

### 5.4 The runtime contract `Type` (`src/ast.rs:142`) — minimal touch

`ast::Type` is the **runtime** contract representation (checked by `check_type` in `interp.rs`). Generics
are **erased at runtime** (§7), so a generic type *parameter* in a signature is checked as `any` at
runtime (a `T`-annotated param accepts any value — the static checker is what enforces `T`'s consistency).
Therefore `ast::Type` needs only:
- a `Type::Param(String)` variant (renders as the bare name; `check_type` treats it as `Any` —
  accept-anything) so the legacy parser/formatter can round-trip a `T` annotation; and
- `Type::Named` already covers a generic *application* head at runtime; the type-arg list is parsed and
  **discarded** for the runtime contract (it carries no runtime obligation beyond the erased head).

This keeps the runtime contract layer (and thus both engines) untouched in behavior: a `Box<int>` field
annotated `value: T` is, at runtime, an unconstrained field — exactly as today an un-annotated field is.
The *soundness* lives entirely in the static checker (the spec's whole premise).

---

## 6. Surface syntax & semantics (parsers + tree-sitter)

Type-parameter and type-argument syntax is added in **both parsers** and the **tree-sitter grammar**, per
the `CLAUDE.md` "Touching syntax" checklist. The `>>` token split is **already implemented by NUM** (NUM
§3.4) — TYPE reuses it in *type position*; it adds **no lexer change**. But the **expression-level**
explicit-type-arg disambiguation (`Box<int>(5)` vs comparison) is **new grammar work** TYPE owns (NUM
covers only known type-arg position) — speculative parse in the legacy parser, a declared GLR conflict in
tree-sitter, with its own conformance tests (below).

**Legacy parser (`src/parser.rs`):**
- After a `fn`/`class`/`enum`/`interface` name, parse an optional `< Ident (: Type)? (, ...)* >`
  type-parameter list into a new `decl.type_params: Vec<TypeParam{name, bound: Option<Type>}>`.
- `parse_type_atom` (`:517`): an `Ident` that is an in-scope type param → `Type::Param(name)`; a known
  generic head with a `<...>` arg list (`Box<int>`) → `Type::Named(head)` + parsed-then-checked args; a
  bare `fn(A)->B` signature type → a new `Type::FnSig`. **This is "known type position"** — the NUM
  `>>`-split helper applies directly when closing nested args.
- **Expression-level explicit type args (`Box<int>(5)`, `map<string,number>(...)`) — the NEW work.** After
  a primary callee in *expression* position, a `<` is **ambiguous** with comparison. NUM does not handle
  this (its split is for already-known type-arg lists, NUM §3.6). TYPE resolves it with the standard
  **speculative parse, backtrack to comparison** technique (Rust/TypeScript): tentatively parse `<
  Type (, Type)* >` and accept the type-arg reading **only** if the `>` is immediately followed by `(`
  (the call shape); on any failure, rewind and parse a comparison expression. The `>>`-split helper is
  reused *inside* the speculative type-arg parse (for nested closers), but the *decision to enter* that
  parse is new to TYPE.

**CST parser (`src/syntax/parser.rs`):**
- New `SyntaxKind`s: `TypeParams`, `TypeParam`, `TypeBound`, and `FnType` (for `fn(A)->B`). `type_primary`
  (`:1240`) already parses `Ident` + `<TypeArgs>` into `GenericType` (`:1246`) — extend it so a head that
  is a type param yields a `NamedType` the lowering maps to `Var`, and so the arg list reuses the NUM
  `>>`-split. Add `type_params(p)` called from the fn/class/enum/interface decl parsers.
- The two front-ends MUST agree (`tests/frontend_conformance.rs`).

**Tree-sitter (`tree-sitter-ascript/`):**
- Add `type_parameters`/`type_parameter`/`type_bound` rules and a `function_type` (`fn(A)->B`); allow a
  generic head + nested `type_arguments` (the `>>`-in-type handling is already in the grammar from NUM).
- **Declare a GLR conflict for the expression-level explicit-type-arg case** (`expr < … > (…)` vs a
  comparison chain) — this is NEW to TYPE (NUM declared none for expression position) and is the
  tree-sitter counterpart of the legacy parser's speculative backtrack. The grammar lets the GLR parser
  keep both interpretations live and resolves on the trailing `(`.
- Regen `parser.c` (`tree-sitter generate --abi 14`); update `queries/highlights.scm` (type params as
  `@type.parameter`); **publish** via `./scripts/sync-grammar.sh` and bump the editor pins
  (`editors/zed/extension.toml` `commit`, `editors/nvim/lua/ascript/treesitter.lua` `revision`); update
  the Zed/Neovim/VS Code highlight copies.
- **Conformance tests (REQUIRED, called out separately because this ambiguity is new):**
  `tests/frontend_conformance.rs` proves the legacy and CST parsers agree on the disambiguation, and
  `tests/treesitter_conformance.rs` proves the tree-sitter grammar matches — each over a paired battery:
  `Box<int>(5)` / `map<string, number>(xs, f)` (type-arg readings) **vs** `a < b > c`, `f(a < b, c > d)`,
  `a < b > (c)` (comparison readings) — so the trailing-`(` rule is exercised on both sides of the
  ambiguity.

**Formatter (`src/fmt.rs` + `ast.rs` `Display`):** render type-param lists canonically (`fn map<A, B>(...)`,
`class Box<T>`, `fn first<T, C: Container<T>>`), render `fn(A) -> B` and `Box<int>` round-trip-stable;
`Type::Param`/`Type::FnSig` get `Display` arms (`ast.rs:193`); idempotence goldens.

---

## 7. Determinism & the static-only safety property

**The headline safety property: this spec changes no runtime behavior, so the four-mode byte-identity
gate (Gate 1) and `vm_differential.rs` are untouched by construction.**

- **No engine code is touched.** `interp.rs` (tree-walker) and `src/vm/` (VM) get **no new arms, no new
  opcodes, no behavior change**. Generics are **erased**: a `T`-annotated slot is, at runtime, an
  unconstrained (`any`-equivalent) slot via `Type::Param` → accept-anything in `check_type` (§5.4). The
  runtime never sees a type variable.
- **No `.aso` change.** No opcode, no constant-pool kind, no serialization-layout change ⇒
  **`ASO_FORMAT_VERSION` is NOT bumped** and `src/vm/verify.rs` is untouched. (Contrast NUM, which bumps
  it.) This is a deliberate, stated property: TYPE is purely a front-of-pipeline static analysis.
- **`vm_differential.rs` is unchanged.** Because no program's observable output changes, the corpus +
  goldens produce identical bytes on tree-walker == specialized-VM == generic-VM == `.aso`-compiled,
  exactly as before. The *only* observable difference TYPE produces is in **`ascript check`'s diagnostics
  and exit code** (and the LSP), which the differential harness does not compare.
- **Determinism (SP9) is irrelevant to TYPE** — the checker runs no code, holds no `RefCell` across
  `.await` (it is sync, interpreter-free), and touches no clock/RNG seam.

This is the cleanest gate posture in the campaign: the proof obligation reduces to "the static checker
emits the right diagnostics" (§9), with **zero** risk to runtime correctness.

---

## 8. Implementation surface & cross-cutting checklist

Per `CLAUDE.md` "Touching syntax" + the checker/LSP/editor toolchain. **Each item is a required
deliverable.** Note the *absences* (engines, `.aso`) — they are the spec's safety property, stated
explicitly so a reviewer can confirm nothing leaked.

**Both parsers (`src/parser.rs`, `src/syntax/parser.rs`):** type-param lists on fn/class/enum/interface
decls; type-param references and generic applications in type position; `fn(A)->B` function types; the
**NEW** expression-level explicit-type-arg disambiguation at call/construction sites (speculative parse,
trailing-`(` decides — NUM does *not* cover this); reuse the NUM `>>`-split inside type-arg lists.
Frontend conformance proves agreement **including a paired type-arg-vs-comparison battery** (§6).

**Tree-sitter (`tree-sitter-ascript/`):** `type_parameters`/`type_parameter`/`type_bound`/`function_type`
rules; nested type-arg `>>` (from NUM) + a **NEW declared GLR conflict for explicit-type-arg expressions**
(`expr<…>(…)` vs comparison — TYPE's own, not inherited from NUM), with its own treesitter-conformance
battery; regen `parser.c` (`--abi 14`); `queries/highlights.scm`; **publish** (`./scripts/sync-grammar.sh`) + editor-pin
bumps (Zed/Neovim) + VS Code TextMate + Zed/Neovim highlight copies.

**AST (`src/ast.rs`):** `Type::Param(String)` + `Type::FnSig` (runtime-erased; `ast::Type` is the enum at
`ast.rs:142`); `Display` arms; decl nodes gain `type_params`. **Rebase note:** NUM adds `Type::Int`/
`Type::Float` to this same enum (NUM §"Runtime contracts", `ast.rs:142`) and `CheckTy::Int`/`CheckTy::Float`
to the lattice; TYPE's `Type::Param`/`Type::FnSig` (and the `CheckTy::Var`/`FnSig`/`ClassApp`/`EnumApp`/
`Interface` additions, §5.1) land **on top of** NUM's variants, so the exhaustive matches in `interp.rs`
(`check_type`), `fmt.rs`, and `ast.rs` `Display` must cover NUM's *and* TYPE's new arms (compile-error-
enforced — a missing arm fails to build, by design). `check_type` treats `Param` as accept-anything and
discards type-arg lists on a generic head.

**Static checker — the bulk (`src/check/infer/`):**
- `ty.rs`: `CheckTy::Var`/`FnSig`/`ClassApp`/`EnumApp`/`Interface`; `assignable` arms (all `Unknown`-biased,
  §5.2); `widen`/`display`/`normalize`/sort-key updates; the NUM `Int`/`Float`/`number`-union interplay.
- `unify.rs` (**new**): occurs-checked union-find unifier; freshening; substitution; depth/width-capped.
- `table.rs`: type params + bounds on classes/enums/fns; populate the `InterfaceInfo` interface table
  (reserved by IFACE §6) + the structural `conforms` predicate; generic field/method-return *instantiation*.
- `env.rs`: unchanged shape; the instantiation `subst` lives on the `Pass`, not the `Env`.
- `pass.rs`: `emit` severity arg; `check_against` `blocking` flag; freshen/unify/substitute in
  `synth_call`/constructor/generic-body paths; instantiated returns; the §3.1 annotated-slot ⇒ `Error`
  policy. **No new false-positive surface** (every uncertain path → `Unknown`).

**Lint config (`src/check/config.rs`):** the three codes stay in `RULE_CODES` (`:47`); their *annotated-slot*
default becomes `Error` (blocking). A project `ascript.toml` may downgrade (explicit opt-out). Document the
default flip.

**LSP (`src/lsp/`):** **hover** (`infer::hover_type_at`, `mod.rs:37`) shows **instantiated** generics
(`Box<int>`, `map`'s `B = number`); **inlay hints** surface inferred type args + binding types (the
hover-collection mode already records inferred binding types, `pass.rs:229` — extend it to emit inlay hints
for solved type args; DX consumes this); diagnostics flow the new blocking severities; completion offers
type-param names in scope. (DX owns the inlay-hint *protocol* surface; TYPE provides the inferred data.)

**Runnable examples (Gate 9 — required, §9.2a):** `examples/generics.as` (introductory, clean, exercised
by the conformance tests) and `examples/advanced/<name>.as` (production-shaped, fully error-handled) — not
only `tests/check.rs` fixtures. Both sit in `examples/**`, so the Gate-5 zero-`type-*` assertion covers
them automatically.

**Docs:** a "Generics" + "Sound typing" section in `docs/content/language/type-contracts.md` (the
blocking-vs-gradual rule, `any` as the escape, generic syntax, the invariance limitation + the
`fn<T>` workaround, bounds); update `README.md` types table; the main design spec's type section;
`CLAUDE.md` (the SP10 paragraph → note the soundness upgrade + generics); `roadmap.md`. NAV unchanged
unless a new page is added (it is not — appended to the existing type-contracts page).

**Explicitly UNCHANGED (the safety property — a reviewer confirms these are untouched):** both engines
(`interp.rs` behavior / `src/vm/**`), the GC, the `Interp` async model, `.aso` (`src/vm/aso.rs` +
`ASO_FORMAT_VERSION` + `src/vm/verify.rs`), the worker airlock, all stdlib runtime behavior,
`vm_differential.rs`.

---

## 9. Testing

### 9.1 Soundness positives that SHOULD block (new true-positive corpus)
A new `tests/check.rs` block (+ small `.as` fixtures, NOT in `examples/**`) asserting each emits a
**`Severity::Error`** `type-*` and fails the gate:
- `let x: number = "s"` → blocking `type-mismatch`.
- `fn f(p: string) {}  f(1)` → blocking `type-mismatch` on arg 1.
- `fn f(): number { return "x" }` → blocking `type-mismatch` on return.
- `class C { n: number = "x" }` → blocking `type-mismatch` on field default.
- `let n: number = 1; n - "x"` style annotated arithmetic → blocking `type-error`.
- A generic: `map<string, number>([1, 2], fn(x){ return x })` → blocking on the `int`-vs-`string` arg.
- A bound: `fn first<T, C: Container<T>>(c: C)` called with a value provably lacking `at`/`len` → blocking
  conformance `type-mismatch`.

### 9.2 The zero-false-positive corpus guarantee (the tripwire)
- `examples/**` emits **zero** `type-mismatch`/`type-error` (and still zero `possibly-nil`) in **both**
  feature configs — a direct CI assertion (§3.4). This is the Gate-5 regression test for the whole spec.
- Property-style: a generated battery of untyped + `any`-typed + partially-typed programs through
  generics must emit **no blocking diagnostic** (every uncertain path → `Unknown`). The corpus is
  migrated (Gate 7): any generic example is verified runnable and clean.

### 9.2a Gate 9 — at least one RUNNABLE generic example under `examples/**`
The unit/fixture tests (§9.1, `tests/check.rs`, NOT in `examples/**`) prove the *blocking* paths, but
Gate 9 requires the feature to be **exercised by the runnable, living-documentation corpus**, not only
by checker fixtures. Ship:
- **`examples/generics.as`** (introductory): a generic `fn map<A,B>(...)`, a `class Box<T>` / `Stack<T>`,
  a generic enum (`Option<T>` or `Result2<T,E>`), and at least one bounded `fn first<T, C: Container<T>>`
  call — all **clean** (zero `type-*`), **runnable** under `target/release/ascript run`, and exercised by
  the conformance tests (it lives in `examples/**`, so the Gate-5 tripwire and the example-runner cover
  it automatically).
- **`examples/advanced/<name>.as`** (production-shaped, fully error-handled): a realistic generic
  container or combinator pipeline (e.g. a typed `Stack<T>` with `push`/`pop`, or a generic `Result`
  helper), demonstrating inference, an explicit type arg, and a bound — verified with `ascript run`.

These are **required deliverables** (not optional), and because they sit in `examples/**` they are
automatically held to the zero-`type-*` Gate-5 assertion (§3.4): a generic example that does NOT
type-check cleanly is a bug to fix in the example (or in `assignable`/unification), never a gate
relaxation.

### 9.3 Generic inference & unification tests (`ty.rs`/`unify.rs` units + `check.rs` integration)
- **Var is gradual:** `assignable(Var unsolved, anything)` and `assignable(anything, Var unbounded)` →
  `Unknown`; **never** `No`. (The single most important invariant.)
- **Occurs-check:** unifying `T` with `array<T>` is rejected → results degrade to `Unknown`, no hang.
- **Inference:** `id(5)` solves `T=int`; `map(["a"], fn(s){return s.length})` solves `A=string,B=number`;
  `Box(5).get()` synthesizes `int`; empty-array `map([], f)` leaves `A` unsolved → `array<any>` (gradual,
  no error).
- **Invariance:** `Box<int>` ↮ `Box<string>` (`No`); `Box<int>` ↔ `Box<any>` (gradual); `Box<Dog>` not
  assignable to `Box<Animal>` (documented limitation, asserted).
- **NUM interplay:** `T=int` flows into `: number` (`Yes`, union membership) and not into `: string`
  (blocking); `id<float>(5)` where 5 is `int` vs an explicit `float` arg → the explicit-vs-inferred conflict
  rule (§4.4).
- **Bounds:** a conforming class passes; a provably-non-conforming concrete value blocks; a partially-known
  value stays `Unknown`.
- **Blocking-vs-advisory:** the *same* `No` is `Error` over an annotated slot and `Warning` over an
  inferred slot — asserted on a paired fixture (`let x: number = "s"` Error vs `let x = "s"; x - 1`
  advisory).
- **Hover/inlay (`tests/lsp.rs`):** hover over `Box(5)` shows `Box<int>`; over `map(...)`'s result shows
  `array<number>`; inlay hints surface solved type args.

### 9.4 No-runtime-change guarantee
`vm_differential.rs` is **not modified** and stays green (both configs) — the static-only proof. A CI note
asserts `ASO_FORMAT_VERSION` is unchanged by this branch.

---

## 10. Scope & rejected alternatives

**In scope:** the advisory→blocking soundness flip for annotated slots (`type-mismatch`/`type-error`
default `Error`); `possibly-nil` stays advisory; user-defined generics on fn/class/enum/interface;
`CheckTy::Var`/`FnSig`/`ClassApp`/`EnumApp`/`Interface`; occurs-checked union-find unification +
local argument-driven inference + explicit type args; **invariant** generics; **interface-only** bounds +
the structural `conforms` check (IFACE's static half); the NUM `int`/`float`/`number` interplay; runtime
erasure (`Type::Param`); both parsers + tree-sitter + formatter + LSP hover/inlay; docs. **Static-only —
no engine/`.aso`/runtime change.**

**Out of scope (reserved/deferred):**
- **Declaration-site & use-site variance** — a future *additive* extension (`out T`/`in T` + a
  position-checker). Invariant v1 code stays valid when added. Rejected for v1 (§4.6).
- **Higher-kinded types / generic generics** (`F<_>`), **variadic generics**, **const generics** — not in
  a v1 gradual checker.
- **Generic *type aliases*** (`type Pair<T> = [T, T]`) — AScript has no `type` alias today; a separate
  additive feature.

**Rejected:**
- **Whole-program / global type inference (full Hindley–Milner across modules).** A standing campaign
  non-goal. Gradual typing's contract is *local* reasoning with `any` as the escape; global inference
  fights the gradual model, scales poorly, produces inscrutable "type error three files away"
  diagnostics, and would risk corpus false positives. We do **local, bidirectional, argument-driven**
  inference only (the SP10 posture, extended). (`goal.md` / `CLAUDE.md` SP10.)
- **An opt-in `strict` mode as a backward-compat hedge** (soundness only when a flag/pragma is set).
  Rejected: **backward compatibility is not a constraint** (`goal.md`), so soundness is the **default**
  for annotated code. A *downgrade* knob exists in `ascript.toml` for teams mid-migration, but the
  default is blocking — opt-out, not opt-in (matching the campaign's "batteries opt-out not opt-in" DX
  pillar).
- **Full variance for v1** (covariant read-only containers) — the sound subset still needs a
  position-checker; deferred to keep v1 sound-and-simple (§4.6).
- **Covariant generic containers by default** — unsound (the covariant-array-store hole); invariance is
  the sound v1 choice for the **new** user heads.
- **Changing the existing built-in constructors' variance.** Rule 8 (`ty.rs:398`–`431`) is **covariant**
  today (`Array`/`Future`/`Result`/`Map`/`Tuple` recurse one-directionally) and TYPE leaves it
  **untouched** — making the built-ins invariant would be a behavioral diagnostic change on the existing
  corpus (e.g. `array<Dog>` → `array<Animal>` flips from `Yes` to `Unknown`) for no in-scope benefit. The
  invariance decision (§4.6) applies **only** to the new `ClassApp`/`EnumApp`/parameterized-interface
  heads, which get their own both-directions arm. (A future spec may revisit built-in variance uniformly.)
- **Class or union bounds** (`T: SomeClass`, `T: A | B`) — class bounds reintroduce nominal-inheritance
  brittleness generics exist to avoid; union bounds complicate conformance. **Interface bounds only**
  keeps conformance structural and decidable.
- **Making `possibly-nil` blocking.** It flags a latent panic, not an annotated-slot contract violation;
  blocking it risks corpus false positives and punishes idiomatic deferred-guard code. Stays advisory
  (promotable per-project).

## 11. Grounding (verified sources)

- **Gradual typing / the `any` escape / `Unknown ⇒ silent` discipline:** Siek & Taha, "Gradual Typing for
  Functional Languages" (the consistency relation; `any` is consistent with everything). The existing
  SP10 lattice already encodes this (`ty.rs:8`, rule 1 `ty.rs:255`).
- **Sound-by-default-for-annotated-code, gradual-for-unannotated:** TypeScript `--strict` posture
  (annotated positions are checked, `any` opts out) — adopted as the **default** here (BC is not a
  constraint), not behind a flag.
- **Bidirectional typing + local inference (not global HM):** Pierce & Turner, "Local Type Inference"
  (POPL'98); Dunfield & Krishnaswami bidirectional typing — synthesis (`synth`) + checking
  (`check_against`), already the SP10 structure.
- **Unification + occurs-check:** Robinson's unification algorithm; the occurs-check as the
  infinite-type / non-termination guard.
- **Invariant user generics:** Go generics (type sets, no variance) and Rust (no subtyping variance on
  user type parameters) — the closest design siblings, both shipping invariant generics; the
  covariant-mutable-container unsoundness (the "covariant array store" hole) motivating invariance.
- **Declaration-site variance (rejected for v1):** Kotlin/Scala/C# `in`/`out`; Java use-site wildcards
  `? extends`/`? super` — surveyed and deferred as an additive future extension.
- **`>>` nested-generics token split (reused from NUM):** Rust/Java/C# type-argument `>>` handling
  (NUM §3.4).
