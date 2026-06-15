# AScript Contract Elision via Static Proof — Design (ELIDE)

- **Status:** Draft for review
- **Date:** 2026-06-12
- **Code:** ELIDE (the "types pay you back" spec of the PERF campaign — see `goal-perf.md`)
- **Depends on:** **TYPE merged** (the sound-for-annotated checker is the proof source —
  `superpowers/specs/2026-06-08-sound-types-generics-design.md`; `src/check/infer/` as shipped);
  **LANE Task-0 bench corpus** (the call-heavy workload that isolates the contract share — if it
  has not landed when this plan executes, the plan's Task 0 ships ELIDE's own call-heavy
  workloads in a LANE-compatible shape; see §8.2).
- **Depended on by:** nothing (but it compounds with CALL — fewer per-call checks shrink the
  call path CALL is dieting — and with DECODE — an elided site is one fewer op to pre-decode).
- **Engines:** **BOTH** — and this is the subtle part. Elision decisions are made at
  COMPILE/CHECK time from the **source**, and the SAME per-module proof set is applied by the
  bytecode compiler (VM) and by a pre-execution marking pass (tree-walker), so all four
  differential modes elide identically. A wrong proof can therefore never split the engines —
  it is caught instead by the **elide-on vs elide-off cross-axis** and the paranoid mode (§6).
  *(This supersedes the one-line `goal-perf.md` entry's "the tree-walker keeps full checks" —
  see §2.4 for why that framing was wrong and how it is corrected.)*
- **Breaking:** **no.** A program that satisfies its annotations behaves byte-identically (a
  passing runtime contract check is pure and side-effect-free — eliding it is invisible). A
  program that would *fail* an elided check is, **by proof, unreachable** — and the proof
  predicate is engineered (§2, §3) so that claim is actually true, not aspirational. One
  artifact-level change: a new opcode (`Op::CallElided`) bumps `ASO_FORMAT_VERSION`
  (currently **27**, `src/vm/aso.rs:167` — read the constant, never hardcode).

---

## 0. Read this first — raw `Compat3::Yes` is NOT a runtime guarantee

The naïve design — "elide every site where the checker's `assignable` returned `Yes` on every
param" — is **unsound against the shipped code**, and this spec exists to say precisely why and
what the sound predicate is. Three audited landmines, each verified against the live binary
(2026-06-12, probes reproduced in §3.4):

1. **Rule 1 makes `Any` → anything `Yes`, not `Unknown`** (`src/check/infer/ty.rs:342-345`:
   `if matches!(self, Any) || matches!(dst, Any) { return Compat3::Yes }`). Every untyped
   argument flowing into a typed param is a `Compat3::Yes`. Eliding on raw `Yes` would elide
   essentially **every** call site, typed or not — maximally unsound. The gradual escape that
   keeps Gate 5 at zero false positives is a *permission to stay silent*, not a proof.

2. **The runtime does not contract-check reassignment, and the checker does not flow-update an
   annotated binding on assignment.** `interp.rs` documents it (`src/interp.rs:2965-2967`:
   *"the language does not contract-check later assignments"*), and `pass.rs` mirrors it
   (`AssignExpr` → synth children, return `Any`, **no env update, no diagnostic** —
   `src/check/infer/pass.rs:675-678`; assignment is not one of TYPE's four annotated sites).
   Verified live: `let x: int = 5; x = "s"; f(x)` with `fn f(p: int)` — **`ascript check` exits
   0** (the checker still believes `x : int`, proven by a follow-up probe where
   `let y: string = x` *after* the string assignment still reports "found `int`") while
   **`ascript run` panics** with the contract violation at `f(x)`. The static type of a
   **mutated** binding is an unenforced assumption; a `Yes` derived from it is not a proof.

3. **At least one concrete-`Yes` verdict contradicts the runtime today.** `assignable` rule 6
   says an instance is assignable to `object` (`Class(_) → Object = Yes`), but the runtime
   contract **rejects** it: `fn f(p: object){}; f(C())` panics `type contract violated: expected
   object, got instance` while the checker stays silent. This is a pre-existing checker bug by
   TYPE's own model (a *provably-runtime-failing* value marked `Yes`); ELIDE both **fixes it**
   (→ `Unknown`, the corpus-safe direction — §6.6) and **excludes `object` from the elidable
   type forms** as defense in depth.

The consequence: ELIDE's proof predicate is **strictly stronger than `Compat3::Yes`**. A site
is elided only when three independent conditions hold (§2): the destination's declared type is
**ElideSafe** (its runtime check is a pure function of the value's stable kind, env-free), the
verdict is a **concrete** `Yes` (not via rule 1 / rule 2 gradual arms), and every argument is
**Anchored** (its synthesized type is a runtime-guaranteed fact, not a static assumption). Every
other site — every gradual boundary, every mutated binding, every deep container, every
env-resolved name — **keeps its full runtime check, exactly as today.** That is sound gradual
typing: checks live exactly at the typed↔untyped boundary, and annotations buy performance only
where they are genuinely load-bearing.

The deliberate side benefit (stated as a design goal, not an accident): because elision claims
"this check can never fire," **every elided site is a machine-checked assertion about TYPE's
soundness**. The elide-on vs elide-off differential axis and the paranoid mode (§6) turn the
whole corpus + fuzzer into a continuous soundness fuzzer for the type checker — any wrong proof
surfaces as a divergence instead of hiding behind the runtime check that used to mask it.

## 1. Summary & motivation

Every AScript call pays `check_call_args` (`src/interp.rs:7898`) — arity + per-param
`check_type_env` + rest collection — on **both** engines (`goal-perf.md` evidence table: "Every
call pays `check_call_args` contract validation (`src/vm/run.rs:3656`)"). Annotated `let`s pay
`Op::CheckLocal` (`src/vm/run.rs:2257`), annotated returns pay a `check_type` at every frame pop
(`return_from_frame`, `src/vm/run.rs:5796-5807`), and param defaults pay `Op::CheckParam`
(`src/vm/run.rs:2240`). For *typed* code, the checker can already *prove* many of these checks
can never fire — and then the runtime runs them anyway, forever.

TypeScript erases types (the checks never existed — and neither does the guarantee at the
boundary). Sorbet and typed Python keep runtime checks because their checkers don't gate the
runtime. AScript owns the checker, the compiler, and both engines, so it can do what Static
Python / Cinder did at Instagram: **let static proof remove dynamic checks, site by site,
keeping full checks at every unproven boundary.** The more you annotate, the faster you run —
the loop TypeScript and Sorbet structurally cannot close.

What ELIDE is **not**: it is not a strict mode, not a checker semantics change, and not a new
dialect. The checker's diagnostics are byte-identical before/after (§6.5 — the proof collector
is a side-channel, exactly like the existing hover-collection mode, `pass.rs` `hover`); the
gradual model stands; a program that runs today runs identically tomorrow.

## 2. The proof predicate

### 2.1 Definitions

A **site** is one of: a call expression's argument-binding boundary, an annotated
`let`/`const`'s initializer check, or a function's declared-return check (the v1 surface; the
full classification is §3). For a site to be **proven** (and thus elided), all of:

- **(E) ElideSafe destination.** Every *typed* param (resp. the annotation / the return type)
  has a declared type whose runtime check is a **pure function of the value's stable kind**,
  resolved **without the environment**. The exact form list is §2.2. Untyped params, `any`, and
  the runtime-erased `Type::Param(T)` / `Type::FnSig` impose no/kind-only runtime obligation and
  need no argument proof (their rows in §3 say which are free-pass vs excluded).
- **(Y) Concrete Yes.** `assignable(synth(arg), expected) == Compat3::Yes` reached through the
  **concrete** arms — never rule 1 (`Any` on either side), never rule 2 (`Never` source), never
  a `Var` arm. Operationally the collector doesn't re-classify rule paths: condition (A) below
  already excludes every gradual source (an `Any`/`Never`/`Var` synth is never Anchored), so
  (Y) ∧ (A) jointly select concrete `Yes` only. The spec states (Y) separately so the invariant
  is explicit and unit-tested on its own.
- **(A) Anchored argument.** The argument expression's synthesized type is **runtime-anchored**:
  the runtime is *guaranteed* (by an executed check, a literal, or kind-exact evaluation rules)
  to produce a value whose kind is in `kinds(synth(arg))`. The anchored expression forms are
  §2.3.

**The soundness theorem the design rests on** (each ElideSafe row in §2.2 is an instance):

> For an ElideSafe destination type `T`, the runtime check `check_type_env(v, T, env)` depends
> only on the **kind** of `v` (scalar subtype / string / bool / nil / callable / …), and
> `kinds(S) ⊆ accepted_kinds(T)` whenever `assignable(S, T) == Yes` through a concrete arm.
> Therefore: **(E) ∧ (Y) ∧ (A) ⟹ the runtime check passes**, for every execution of the site.

The "every execution" universality holds because anchoring is defined against binding-level
facts that are execution-invariant (an *unmutated* binding's checked annotation; a literal; a
kind-exact operator), never against a single flow state. A wrong link anywhere in this chain is
a **checker soundness bug**, surfaced by §6's nets — never silently absorbed.

### 2.2 ElideSafe type forms (`ast::Type`, `src/ast.rs:150`)

| `ast::Type` form | Runtime check | ElideSafe? | Why |
|---|---|---|---|
| `Int` / `Float` / `Number` | value kind is `Int` / `Float` / either | **yes** | kind-only; scalar kind is immutable per value |
| `String` / `Bool` / `Nil` | kind-only | **yes** | immutable kinds |
| `Any` | always passes | **yes (free-pass)** | the check is a no-op; elidable with NO argument proof |
| `Param(T)` (TYPE, erased) | always passes (`check_type` treats as `Any`) | **yes (free-pass)** | runtime-erased by construction |
| `Fn` | value is callable (kind set) | **yes** | kind-only |
| `FnSig(..)` (erased to callable) | plain callable check | **yes** | runtime obligation is the bare `Fn` kind check |
| `Optional(T)` / `Union(a,b)` of ElideSafe members | member-wise kind check | **yes** | union of kind-only checks is kind-only |
| `Object` | kind `Object` — **rejects instances** | **NO (v1)** | checker rule 6 says instance→object `Yes`; runtime disagrees (§0 #3). Excluded + the rule-6 verdict fixed to `Unknown` (§6.6) |
| `Array(T)`, `T ≠ Any` / `Map(K,V)` non-any / `Tuple` / `Result` | **deep** per-element / length check | **NO** | interior mutation (`xs.push("s")`) invalidates depth between check sites; tuple length is mutable |
| `Array(Any)` / `Map(Any,Any)` | extensionally kind-only (elements vacuously pass) | **yes** | the O(n) walk always succeeds; elision also removes the walk |
| `Future(T)` | future kind (+T?) | **NO (v1)** | async sites are out of v1 eligibility anyway |
| `Named(name)` (class / interface / enum / unresolved) | **env-resolved** (`check_type_env`, `src/interp.rs:5568`): interface → structural `conforms`, class → nominal, unresolved → name-dependent | **NO** | resolution depends on the callee frame's env chain at run time (shadowing, late binding); the checker resolves lexically — the two can disagree. Interface conformance additionally differs in granularity (checker: typed-signature assignability; runtime: name+arity). Deferred with justification, not dropped |
| `Error` | `object \| nil` hybrid | **NO (v1)** | asymmetric semantics; not worth the audit in v1 |
| (`decimal` has no `Type`/`CheckTy` form — a `decimal` annotation parses as unresolved `Named` → gradual; never proven, never elided.) | | | |

### 2.3 Anchored expression forms (the v1 allowlist — everything else is NOT anchored)

Anchoring is computed by the collector alongside `synth`, with per-scope tracking that mirrors
`Env` (so frame-local slot keys never collide across functions — `BindingKey::Local(slot)` is
frame-relative, `src/check/infer/env.rs`).

| Expression form | Anchored when | Runtime guarantee |
|---|---|---|
| int/float/string/bool/nil **literal** | always | the value IS the literal |
| **template string** | always | always produces a string |
| `ParenExpr` | operand anchored | transparent |
| **unary `!`** | always (`Bool`) | runtime `!` always yields a bool |
| **unary `-`/`+`** | operand anchored & numeric | kind-exact (overflow panics *before* the site — a panic is not a wrong elision) |
| **comparisons** (`< <= > >= == != instanceof`) | always (`Bool`) | always bool or a panic-before-site |
| **arithmetic** (`+ - * / % ** +% -% *%`, bitwise, shifts) | both operands anchored AND the collector's operand-kind → result-kind table **mirrors NUM's runtime promotion exactly** (int∘int→int incl. truncating `/`; mixed→float; `+` over strings→string; bitwise→int) | NUM's type-directed rules are deterministic in operand kinds; div-by-zero / overflow panic before the call. A required unit battery pins synth-vs-runtime kind agreement over the full operand-kind matrix (§6.7) |
| **logical `&&` / `\|\|` / `??`** | **NOT anchored (v1)** | runtime returns an *operand* (truthiness), not a bool; the join logic is not worth the v1 audit |
| **NameRef** | the resolver binding has `mutated == false` (`src/syntax/resolve/types.rs:34` — the same final-flag the capture-by-value pass trusts) AND (a) it is a param/`let`/`const` **annotated with an ElideSafe type** (the runtime check at entry/init anchors it — or that check was itself elided *because proven*, which is inductively sound), or (b) it is unannotated & unmutated with an **anchored initializer** | an unmutated binding holds the checked/anchored value forever; the `mutated` gate also kills every stale-narrowing and loop-carried-update hazard in one move |
| **NameRef to a narrowed binding** | base binding anchored (narrowing only removes union members based on **executed** guards: nil-guard, truthiness, `instanceof`, `match`) | narrowing's positive claims become load-bearing for the first time — §6.7 mandates a narrowing-soundness battery |
| **CallExpr** | callee is the unique in-file fn (`resolve_in_file_fn`, `pass.rs:1964` — single non-shadowed `Fn` binding), non-async/non-generator/non-worker, with a **declared ElideSafe return type** | the runtime return check enforces it (`run.rs:5796` / `interp.rs:5220-5226`) — or was elided because all its returns were proven (inductive) |
| **TernaryExpr** | both branches anchored | union of anchored kinds |
| loop variables | **NOT anchored (v1)** — `walk_loop` binds nothing (`pass.rs:486-494`), the loop var synths `Any` | no proof source exists today; a future TYPE improvement that types range loop-vars `Int` would make them anchored for free |
| everything else (members, indexes, awaits, `?`/`!`, match exprs, object/array literals as *scalar* sources, …) | **NOT anchored** | default-closed: not anchored ⇒ not proven ⇒ check kept (always the sound direction) |

### 2.4 Both engines elide, identically — the corrected engine posture

`goal-perf.md`'s ELIDE stanza said "the tree-walker keeps full checks." That framing is
**rejected** here, for two reasons. (1) It makes default four-mode identity conditional on the
checker being perfect: a single wrong proof would split tree-walker from VM **in the default
configuration**, violating the campaign's prime invariant exactly where users live. (2) It
conflates two different jobs: *engine identity* (every mode behaves the same, by construction)
and *proof verification* (some configuration runs the un-elided program and compares). ELIDE
assigns those jobs properly: **both engines consume the same source-derived per-module proof
set** (identity by construction), and the **elide-off axis + paranoid mode** do the verifying
(§6.1–§6.3). The plan updates the `goal-perf.md` stanza to match.

## 3. The classification table — every runtime check form

This is the normative inventory. "Elidable" always means *under the §2 predicate*; "kept" means
byte-identical to today. Every "elidable" row gets a positive (elided & behaviorally identical)
and negative (gradual boundary keeps the check & still panics) test in both feature configs
(§6.7).

| # | Runtime check form | Where (verified) | v1 verdict | Notes / conditions |
|---|---|---|---|---|
| 1 | **Call-site parameter contracts** | `check_call_args` per-param `check_type_env` (`interp.rs:7977-7990`); called from tree-walker `run_body` (`interp.rs:5172`) and every VM call path (`run.rs:1670`, `:1775`, `:4511`, `:4534`, `:5616`, `:5719`) | **ELIDABLE** (the headline) | Site-level, all-or-nothing. Eligibility: callee = unique in-file `fn` (`resolve_in_file_fn`), non-async / non-generator / non-worker, **no rest param**, call has **no spread** (`pass.rs:1146` already bails on `SpreadElem`) and **no named args**; arity statically verified by the collector (supplied ∈ [min-required, positional-count] — `pass.rs`'s `check_call_args` does NOT check arity, `call-arity` is a separate rule, so the collector counts both sides itself); every typed param satisfies (E)(Y)(A); untyped/`any`/`Param(T)` params are free-pass. **Arity + binding + rest collection are NOT elided** — only the per-arg `check_type_env` calls. Methods, constructors, std/builtin calls: see rows 8–10 |
| 2 | **Typed local init** (`let x: T = v`) | VM `Op::CheckLocal` (`run.rs:2257`, emitted at `compile/mod.rs:3905-3911`, type in `chunk.type_consts`, `chunk.rs:446`); tree-walker `Stmt::Let` (`interp.rs:2956-2962`) | **ELIDABLE** | `walk_let` already computes the blocking verdict (`pass.rs:225-240`). Conditions: annotation ElideSafe + initializer concrete-`Yes` + Anchored. Point-in-time check, so universality follows from anchoring's execution-invariance. Destructuring lets: never (no annotation form; `binding_key_of_decl` bails on patterns) |
| 3 | **Declared-return contracts** | VM `return_from_frame` (`run.rs:5796-5807`, env-FREE `check_type`); tree-walker `run_body` (`interp.rs:5220-5226`, env-aware) | **ELIDABLE (per-function)** | Conditions: declared return ElideSafe; **every** `return` expr in the body concrete-`Yes` + Anchored against it (collected in `walk_return`, `pass.rs:265`); AND (body provably always-returns — the existing `block_always_returns` machinery — OR `nil` is `Yes` against the return type, covering the implicit fall-off-nil return). fn declarations only (named `Stmt::Fn`/`FnDecl`); fn-expressions/arrows/methods: never (v1). Mechanism: the contract is **dropped at definition** — VM `proto.ret = None`, tree-walker `Stmt::Fn.ret = None` at marking (§4.3) — no new runtime branch at all |
| 4 | **Param-default contracts** | `Op::CheckParam` (`run.rs:2240`, emitted `compile/mod.rs:3342`); tree-walker default-eval check (`interp.rs:5195-5201`) | **KEPT (never elide v1)** | No proof source exists: `bind_params` (`pass.rs:528`) never synths default exprs against annotations — there is no verdict to consume. Sound by default-closed. (Defaulted params at a proven call site are fine: the *supplied* args are proven; an omitted default still runs its kept CheckParam.) |
| 5 | **Rest-param element checks** | `check_call_args` rest loop (`interp.rs:8000-8031`) | **KEPT** | `pass.rs` `bind_params` types rest as `Any`; eligibility (row 1) excludes rest callees outright |
| 6 | **Typed field assignment / init field checks** (`FieldDecl`/`FieldSchema`) | `interp.rs:5391`, `:5485` (+ VM mirrors) | **KEPT** | The checker has no field-assignment site (only `check_field_default`, a *default-expr* check at class-decl time, `pass.rs:565`); receiver certainty + env-resolved field types make this a real future spec, not a v1 row |
| 7 | **Field-default check at class decl** | `check_field_default` verdict exists | **KEPT (v1)** | Elidable in principle (same shape as row 2) but the runtime site is inside class setup, executed once per class — zero measurable win; not worth its share of the audit. Recorded as a trivial future extension |
| 8 | **Method-call param contracts** | `invoke_compiled_method` (`run.rs:5595+`), tree-walker `invoke_method` | **KEPT** | No static proof source: the pass checks method args only for worker patterns / generic receivers (`synth_member_call`, `pass.rs:964`), and NB#3 (goal.md) records generic method args as unchecked. Future extension after TYPE closes NB#3 |
| 9 | **Constructor / `init` contracts, `validate_into`, `Class.from`, typed parse** | `interp.rs` `validate_into` / `auto_init_bindings` (`interp.rs:8060`) | **KEPT — never elide, by policy** | Data boundaries. `validate_into` also *coerces* (Object→Instance, defaults) — it is semantically a constructor, not a check; removing it changes values, not just performance |
| 10 | **Std/builtin argument validation** | native fns validate internally (Tier-2 panics) | **KEPT — out of scope by construction** | builtins never route through `check_call_args` |
| 11 | **`std/schema` checks, `instanceof` guards, worker airlock sendability, FFI marshalling validation, capability checks** | respective subsystems | **KEPT — never elide, by policy** | schema/instanceof are user-observable *operations* (they return values), not contracts; the airlock and caps are security boundaries (FFI §, caps §); marshalling guards memory safety. Listed so no future reader "optimizes" them under this spec's banner |
| 12 | **`?`-propagation / unwrap `!` pair-shape checks, match `MatchNoArm` backstop, redeclaration / const-immutability errors** | various | **KEPT** | not type contracts; out of scope |

### 3.4 The probe battery (reproduced in tests)

The four probes from §0, checked into `tests/elide.rs` as permanent semantics pins (they
document why the predicate is shaped this way):

1. `fn f(p: string){}; f(1)` — `run` executes & runtime-panics; `check` blocks. (**`ascript
   run` does not run the type checker today** — its only static gate is parse errors + resolver
   `blocking` diagnostics, `src/main.rs:744-760` / `src/lib.rs:1983`. The TYPE spec's §3.2
   "fails … `ascript run`'s pre-run gate" never shipped for `run`; recorded as a spec-vs-code
   divergence ELIDE must not silently change — §5.3.)
2. `let x: int = 5; x = "s"; print(x)` — runs clean on both engines (no assignment contract).
3. `let x: int = 5; x = "s"; f(x)` with `f(p:int)` — `check` exits 0, `run` panics → the
   mutated-binding landmine (§0 #2). Under ELIDE this site is **not** elided (`x` is mutated ⇒
   not Anchored) and still panics.
4. `class C{}; fn f(p: object){}; f(C())` — runtime rejects, checker rule 6 said `Yes` →
   fixed to `Unknown` + excluded form (§6.6).

## 4. Mechanism

### 4.1 The proof set: `ElisionSet`, collected by the existing pass

A new collection mode on the inference pass, exactly following the shipped **hover-collection
precedent** (`pass.rs` `hover: Option<…>`, surfaced via `infer::hover_type_at`,
`src/check/infer/mod.rs:36` — "hover-mode-only … behavior-preserving and emits no
diagnostics"):

```rust
/// src/check/infer/elide.rs (new). All spans are CHAR offsets into the module
/// source (the legacy front-end's Span convention), converted once from the
/// CST's byte ranges using the module text.
pub struct ElisionSet {
    /// Proven call sites, keyed by the call expression's trivia-trimmed
    /// (start_char, end_char) extent.
    pub calls: HashSet<(u32, u32)>,
    /// Proven annotated-let sites, keyed by the INITIALIZER expression's extent
    /// (the same span `Op::CheckLocal` is emitted at and the tree-walker panics
    /// at — `compile/mod.rs:3905-3911` / `interp.rs:2961` both use it).
    pub lets: HashSet<(u32, u32)>,
    /// Proven whole-fn return contracts, keyed by the fn's NAME-token span
    /// (`Stmt::Fn.name_span` exists on the legacy AST; the CST FnDecl name token
    /// gives the same extent — a single token is the most collision-proof key).
    pub fn_rets: HashSet<(u32, u32)>,
}

/// Run the pass in elision-collection mode. NEVER changes diagnostics (the
/// hover-mode invariant, re-asserted by a dedicated test); ignores the lint
/// config entirely (a downgraded-to-warn proven-WRONG site is a `No` — it is
/// never in the set, so it keeps its runtime check and panics as today; §5.4).
pub fn collect(tree: &ResolvedNode, resolved: &ResolveResult, src: &str) -> ElisionSet
```

The collector piggybacks on `walk_let` / `walk_return` / `check_call_args` (`pass.rs:225`,
`:265`, `:1138`), evaluating (E)(Y)(A) per §2 and recording proven keys. Anchoring state lives
beside the `Env` binding types (per-scope, so `BindingKey::Local(slot)` never collides across
frames). The collector consults the resolver's `Binding.mutated` (`syntax/resolve/types.rs:34`)
for the unmutated gate and re-derives arity itself (row 1).

### 4.2 VM consumption — the compiler emits the elision

`compile_source` (`src/compile/mod.rs:1009`) gains an `Option<&ElisionSet>` (threaded through
`compile_source_inner`; `None` ⇒ byte-identical output to today, the kill-switch path):

- **Row 2 (lets):** `emit_check_local` (`compile/mod.rs:3905`) skips emission when the
  initializer's `node_code_span` is in `set.lets`. No opcode, no side table — the check simply
  does not exist in the artifact.
- **Row 3 (returns):** the fn-proto builder sets `proto.ret = None` when the decl's name-token
  span is in `set.fn_rets`. `FnProto.ret` (`chunk.rs:518`) serializes as the already-supported
  `None` — indistinguishable from an unannotated fn, zero format impact.
- **Row 1 (calls):** the call compiler (the three `emit_u8(Op::Call, argc, span)` sites,
  `compile/mod.rs:4882`, `:5034`, `:5251`) emits **`Op::CallElided`** (new opcode, same `u8`
  argc operand) when the call node's span is in `set.calls`. The run-loop arm joins
  `Op::Call | Op::CallSpread` (`run.rs:1570`) and differs in exactly one way: it passes
  `elide_contracts = true` into the shared binder (§4.4). A non-closure callee at a
  `CallElided` site (impossible under eligibility; defensive) behaves exactly like `Op::Call`.
  - **`.aso`:** the opcode serializes — **built artifacts keep the win** (`ascript build`
    runs the collector under the same default/kill-switch as `run`, so BIN/native bundles and
    multi-core servers benefit). `ASO_FORMAT_VERSION` bumps (27 → next; read, don't hardcode);
    `verify.rs` gains the arm (`Effect::new(argc+1, 1)`-style, mirroring `Call`); the
    disassembler, `bcanalysis`, and the opcode round-trip tables gain the variant; the `.aso`
    reader/fuzzers cover it via the existing roundtrip targets.

Why an opcode and not a side table: the proof is per *call site* but `proto.params` belongs to
the *callee* (other, unproven sites still need the contracts), so the site must carry the bit;
a serialized side table is a bigger format change than one opcode for strictly less dispatch
benefit, and an in-memory-only table would silently strand `.aso`/native deployments at zero
win. Rejected alternatives in §8.

### 4.3 Tree-walker consumption — mark the AST before execution

The tree-walker path (`run_file_with_packages` → `interp.load_module`, `src/lib.rs:127`) and
the module loader both parse legacy ASTs per module. A new **marking pass** runs per module,
after parse and before execution/caching, with that module's own `ElisionSet` (computed from
that module's source — per-module scoping is therefore **by construction**, and cross-module
span collisions are structurally impossible):

- **Row 2:** strip `Stmt::Let.ty` to `None` when the initializer span matches `set.lets`.
- **Row 3:** strip `Stmt::Fn.ret` to `None` when `name_span` matches `set.fn_rets` (the
  `Function` value then carries `ret: None`, `value.rs:837` — `run_body`'s existing
  `if let Some(ty) = ret` does the rest; zero new runtime branch).
- **Row 1:** `ExprKind::Call` (`ast.rs:33`) gains an `elide_args: bool` field (parser sets
  `false`; marking sets `true` on a span match). The `Call` evaluator (`interp.rs:4122`)
  threads it via wrapper entries (`call_value_elided` → `call_function` → `run_body` →
  `check_call_args`; existing `call_value` callers delegate with `false` — no signature churn
  outside the one chain). `fmt`/`Display` ignore the field (the formatter parses fresh,
  unmarked ASTs).

**Cross-front-end key discipline.** The legacy and CST front-ends carry a documented ±1-column
span discrepancy in *diagnostic carets* (CLAUDE.md, accepted SP1 trade-off), so the marking
lookup is **exact-match, fail-safe**: a key that doesn't match marks nothing — the check is
kept, which is always sound. A *wrong* match would require two distinct expressions of the same
syntactic kind with identical `(start, end)` extents in one module — impossible within a single
parse (distinct expressions of the same kind either nest, changing the extent, or are disjoint),
and a systematic ±1 shift cannot land one proven site's key on another site's exact extent
without that second site being the same source text. The residual risk is **misses**
(asymmetric non-elision), which are behaviorally invisible but would silently erode both the win
and the "identical decisions" invariant — so the gate is a **count-parity assertion**: over the
typed corpus, `marked(tree-walker) == consumed(VM compiler) == |ElisionSet|`, per module, in
both feature configs (§6.4). Any mismatch is a front-end span bug to fix, never to shrug at.

### 4.4 The shared binder

`check_call_args` (`interp.rs:7898`) — already the single source of truth for both engines —
gains `elide_contracts: bool`. When `true`: skip the per-param `check_type_env`/`check_type`
calls (`interp.rs:7977-7990`) and the rest-element checks (unreachable under eligibility, but
the skip is total for coherence); **keep** arity validation, binding order, default-range
computation, and rest collection byte-for-byte. Keeping arity is deliberate defense in depth: a
checker-bug-grade wrong callee still dies on arity exactly as today, and the check is
O(1) — the measured win is the per-arg type walk, not the arity compare.

### 4.5 Where the pass runs (the decision) — see §5.

### 4.6 What never elides (operational list, beyond §3's rows)

- **REPL** — per-line compiles never run the collector (cross-line proofs don't exist; a
  session is not a module). Zero elision, documented.
- **Worker-shipped code slices** (pooled `worker fn` / actors / streams) — isolate-side
  compiles don't run the collector in v1 (the slice is a synthetic module). Full checks; the
  paranoid/differential story stays simple. (`.aso`-shipped worker slices built by `ascript
  build` DO carry whatever the build elided — same artifact, same behavior.)
- **DAP `evaluate`** — the debugger's tree-walker evaluation uses unmarked ASTs.
- **`--no-elide` / `ASCRIPT_NO_ELIDE=1`** — everything kept (§5.2).

## 5. Where inference runs — the decision

### 5.1 The options, evaluated honestly

| Option | Shape | Pros | Cons |
|---|---|---|---|
| **(a) `run` + `build` always run the collector** (per module, at compile time) | the pass becomes part of the front-of-pipeline | one behavior everywhere; `.aso`/native keep the win; simplest mental model ("AScript elides proven checks", full stop) | adds the pass to every cold start — must fit a measured budget |
| (b) elision only under `build` / an opt-in `run --release` flag | warm-path-only | zero cold-start risk | two performance dialects of `run`; the headline "annotations buy performance" dies in the default path; flag proliferation |
| (c) elision metadata as a new `.aso` section consulted at load | decouples proof from compile | none over (a) — `Op::CallElided` already IS the durable encoding, strictly smaller | a whole serialized side-table format for nothing; rejected |

**Recommendation: (a), contingent on the measured budget, with the permanent kill switch.**
Preliminary envelope (this machine, release binary, 2026-06-12, same-session): `ascript run` of
a trivial file ≈ **5.5 ms**/invocation including process spawn; `ascript check` (full analyze:
parse + resolve + all rules + **the entire infer pass**) ≈ **4.0 ms** trivial, ≈ **6.8 ms** for
the 266-line `examples/all_features.as` — i.e. the *whole* analysis of a mid-size module costs
~3 ms, of which the infer pass is a fraction, on top of a `run` startup that already parses the
module 3× (parse-error pre-check, resolver blocking gate `lib.rs:1983`, and the compile). The
collector adds roughly *one more resolve+walk* per module.

**The plan's measurement task (REQUIRED, before the decision task)** instruments the real cost:
collector-only wall time per module over `examples/**` + the largest advanced examples + a
synthetic 5k-line module, and end-to-end `ascript run` A/B (collector on/off) over the corpus.
**Budget: ≤ 2% added end-to-end wall-clock geomean on the example corpus AND ≤ 1 ms absolute
for a typical (≤500-line) module.** Inside budget → (a) ships as default. Outside →
fall back to (b)'s flag shape *as the recorded decision*, with the spec's table updated — never
a silent switch.

### 5.1.1 THE MEASURED DECISION (Task 4.1, recorded 2026-06-16)

**Verdict: DEFAULT-OFF, opt-in via `--elide` / `ASCRIPT_ELIDE=1` (the (b) flag shape).**
The on-branch measurement (`bench/ELIDE_RESULTS.md` → "Decision measurement"; Apple M4,
release) against the fixed §5.1 budget:

| §5.1 criterion | budget | measured | verdict |
|---|---|---|---|
| example-corpus end-to-end geomean | ≤ 2% | **+6.99%** (68 runnable files, on/off) | **OVER** |
| ≤500-line module collector cost | ≤ 1 ms | 0.130 ms median, but **1.42 ms** at 266 lines (`all_features.as`) | **OVER below ~300 lines** |

Both criteria fail, so option (a) (always-on `run`) is NOT taken. The collector itself is
cheap in absolute terms (median 0.13 ms/module) and a real **compute-bound** A/B is a WIN
(typed `call_heavy` **−6%**; untyped / concurrent ≈0) — but `ascript run` does not type-check
today, so default-on would add a second parse+resolve+infer pass to EVERY run's startup, and
the example corpus is dominated by sub-10-ms demo programs where that fixed cost is a large
proportional tax (`core_types.as` +112%, `system.as` +46%). Per the plan's honesty mandate,
default-OFF is the correct recorded outcome, not a failure.

`ELIDE_DEFAULT_ON = false` (`src/lib.rs`). The opt-in is `--elide` / `ASCRIPT_ELIDE=1` on
`run`/`build`/`test`; the kill switch `--no-elide` / `ASCRIPT_NO_ELIDE=1` is the explicit
force-off (wins over the opt-in). **`ascript build --elide` is the natural elide surface** —
a one-shot compile whose collector cost is amortised over every later `run` of the durable
`.aso` (the `Op::CallElided` opcode serializes, §4.2).

**Remaining-plan implication:** later tasks written assuming inside-budget default-on are read
with default-OFF. The differential elide axis (Task 4.3) drives the `--elide`/`vm_run_source_elided`
path explicitly (independent of the run-path default), so its soundness coverage is unaffected;
the perf A/B (Task 5.1) reports `--elide` vs `--no-elide`. A future task that shares the
compiler's parse+resolve with the collector (eliminating the duplicate front-end pass) would
close the startup gap and could flip `ELIDE_DEFAULT_ON` to `true` for `run` — a one-constant
change behind the same kill switch.

**In-branch fix (Gate 14) surfaced by this measurement:** the first A/B showed a +19%
regression on UNTYPED `call_heavy.as` — impossible if elision were ≈0 on untyped code. An
all-untyped-param call is a free-pass elide site that emits `Op::CallElided`, but
`Op::CallElided` was missing from `sync_lane_op()` (`src/vm/run.rs`) and the DECODE
block-terminator set (`src/vm/decode.rs`), so every elided call escalated out of LANE's sync
fast lane. Fixed in-branch (CallElided shares `Op::Call`'s sync-lane admission + terminator
status); post-fix untyped −1.96% (≈0), typed −5.97%. Guarded permanently by the elide-on ==
elide-off differential (which now also exercises the sync lane).

### 5.2 The kill switch (permanent, mirrors `--no-specialize`)

`--no-elide` CLI flag on `run`/`build`/`test` + `ASCRIPT_NO_ELIDE=1` env (exactly the
`ASCRIPT_NO_SPECIALIZE` pattern, `lib.rs:1937`'s seam): the collector never runs, the compiler
gets `None`, the marker never marks. Output bytecode and AST are **byte-identical to
pre-ELIDE** — zero-cost-when-off by construction (Gate 12/17: no new hot-loop branch exists in
the off configuration; the only on-cost is the startup pass, governed by §5.1's budget).

### 5.3 `run`'s diagnostic gate is UNCHANGED

Running the pass for proofs does **not** make `run` start failing on blocking type errors.
Today `run` executes programs `check` would block (§3.4 probe 1 — the TYPE spec's run-gate
never shipped; only `ascript check` tallies severities, `main.rs:493`). Corpus programs and
goldens rely on runtime contract panics being *runtime* events. ELIDE consumes verdicts and
discards diagnostics. Flipping `run`'s gate is somebody else's spec (a TYPE follow-up), with
its own corpus migration — recorded, not smuggled.

### 5.4 Blocking severity & `ascript.toml` downgrades

TYPE makes a provably-wrong annotated call a blocking Error, so a program that passes `check`
has no proven-wrong sites. But a project may downgrade (`[lint] type-mismatch = "warn"`) and
**run** a proven-wrong program. Stated explicitly: **a proven-WRONG site is `Compat3::No` — it
is never in the `ElisionSet`, keeps its full runtime check, and panics exactly as today.** The
collector ignores the lint config entirely (§4.1); severity remapping affects what *blocks*,
never what is *proven*. Elision-eligibility is `Yes`-only and config-independent.

## 6. Correctness — the gates

### 6.1 Differential modes (Gate 15)

`tests/vm_differential.rs` gains the **elide axis**. With (a) as default, the existing four
modes (tree-walker / specialized VM / generic VM / `.aso`) run **elide-on**; a fifth
configuration runs all four **elide-off**; and the harness asserts BOTH:

1. **Within-axis identity:** tree-walker == specialized == generic == `.aso` under elide-on,
   and under elide-off — over the whole corpus + goldens, both feature configs.
2. **Cross-axis identity:** elide-on output == elide-off output for every program. **This is
   the checker-soundness fuzzer** (§0): a wrong proof manifests as "elide-off panics where
   elide-on succeeded" — a loud, minimized, reproducible divergence. Fix the checker (or the
   collector's predicate); never the assertion.

### 6.2 Fuzzer axis

`fuzz/fuzz_targets/differential.rs` adds elide-on vs elide-off to its comparison set (the
grammar-aware generator already produces annotated forms via the TYPE-era surface). The `.aso`
roundtrip target covers `Op::CallElided` bytes through the existing reader/verifier fuzz.

### 6.3 Paranoid mode (the proof-violation tripwire)

`ASCRIPT_ELIDE_PARANOID=1` (env, available in release builds; also exercised by a
`debug_assertions` CI job): compiles/marks **as if elide-off** (every check emitted/kept) but
retains the per-module `ElisionSet`; when a contract check **fails** at a proven site
(failure-path-only lookup — zero hot cost), the panic escalates to a distinct
`ELIDE proof violated (checker soundness bug): …` message carrying the site span and the
expected/actual types. CI runs the corpus + a fuzzer batch under paranoid mode; any escalation
is a release-blocking checker bug. On a healthy corpus, paranoid output == elide-off output
byte-for-byte (asserted).

### 6.4 Coverage assertions (anti-false-green, Gate 15)

- The typed corpus (the new typed examples + bench variants, §7) must produce
  **`|ElisionSet| > 0`** per file, with the elision *rate* (proven sites / candidate sites)
  reported by the test (a number in the test log, tracked in the bench report).
- **Count parity per module:** `consumed(VM compiler) == marked(tree-walker) == |ElisionSet|`
  (§4.3) — the cross-front-end key-agreement gate, both feature configs.
- The untyped corpus must produce **zero** elisions on every pre-ELIDE example that has no
  annotations (proves the predicate doesn't fire on gradual code) — and byte-identical bytecode.

### 6.5 Gate 5 untouched — the checker is not loosened (or tightened) by ELIDE

The collector emits **no diagnostics** and changes **no verdicts**; a dedicated test asserts
`analyze`'s diagnostic output is byte-identical with and without collection mode over the whole
corpus (the hover-mode invariant, re-proven for elide mode). The Gate-5 tripwire
(`tests/check.rs corpus::`, zero `type-*` on `examples/**`, both configs) is inherited
unchanged — including over the NEW typed examples this spec adds, which must check clean.

### 6.6 The pre-existing rule-6 bug (fix in-branch, Gate 14)

`assignable`'s `Class(_) → Object` arm returns `Yes` while the runtime rejects instances for an
`object` contract (§0 #3, probe 4). Per the production-grade mandate (any bug surfaced while
working is fixed in-branch with a failing-test-first guard), ELIDE fixes the verdict to
**`Unknown`** — the corpus-safe direction (silent; cannot add a diagnostic; cannot remove a
true `No`); making it `No`/blocking is a TYPE-semantics decision deliberately NOT taken here
(it could add corpus diagnostics — that is TYPE-follow-up territory). A regression test pins:
checker silent + runtime panic + site never elided.

### 6.7 Unit batteries (each §2/§3 row, happy + edge, both configs)

- **Kind-table battery:** the collector's operator result-kind table vs the runtime's NUM
  promotion, exhaustively over operand-kind pairs (int/float/string/bool/nil × ops) — the §2.3
  arithmetic row's pin.
- **Narrowing-soundness battery:** for each narrowing form (nil-guard, truthiness,
  `instanceof`, `match`, early-return negation), a program where the narrowed type feeds a
  proven site — elide-on == elide-off output asserted (narrowing's positive claims are now
  load-bearing; §2.3).
- **Per-row positive/negative tests:** for each elidable row — a typed program that is elided
  and runs identically, and a gradual-boundary twin (mutated binding / `any` source / spread /
  rest / interface type / deep container) that is NOT elided and still panics where it panics
  today. The §3.4 probes are permanent members.
- **`check_call_args(elide_contracts=true)`** unit tests: arity/defaults/rest behavior
  byte-identical, only type checks skipped.

## 7. Performance — honest expectations & required measurements

No number is promised; these are the **required measurements** (Gate 16, same-session A/B, the
shipped profiler as instrument, RSS via `/usr/bin/time -l`, all recorded in
`bench/ELIDE_RESULTS.md`):

1. **The untyped corpus** (`bench/profiling/*.as`, the example corpus): expected **≈0 change**
   — this is the no-regression proof (plus the §5.1 startup budget for the collector itself).
2. **`bench/profiling/call_heavy.as` (untyped) vs `call_heavy_typed.as` (fully annotated)** —
   the headline. The typed variant is the same workload with `int`/`string` annotations on
   every param/return/let; the A/B is elide-on vs `--no-elide` on the typed variant (isolates
   the contract share), plus typed-vs-untyped elide-on (shows what annotations buy end-to-end).
   If LANE Task 0 has landed its call-heavy workload, the typed variant derives from it;
   otherwise ELIDE ships both (and LANE adopts them — §8.2).
3. **Typed advanced examples** (`examples/advanced/` typed entries) — realistic annotated code;
   report the elision rate alongside the time delta.
4. **RSS unchanged** across all of the above (the `ElisionSet` is per-module, dropped after
   compile/marking; the marked AST adds one `bool` per call node).

Expectation, stated honestly: the win scales with **annotation density** and call-site
frequency. On untyped code it is zero by design. On fully-annotated call-heavy code the elided
work is the per-arg `check_type_env` walk + the let/return checks — a real but bounded slice of
a call path that LANE/CALL are independently dieting; the *compounding* (fewer checks on a
thinner call path) is the campaign-level story, and the same-session A/B is what keeps this
spec from over-claiming. The Gate-12/17 floor (spec/tw geomean ≥2×, zero cost when off) is
re-run at merge.

## 8. Scope & rejected alternatives

**In scope (v1):** rows 1–3 of §3 under the §2 predicate; the `ElisionSet` collector
(diagnostic-neutral pass mode); `Op::CallElided` + ASO bump + verifier/disasm/bcanalysis/fuzz
coverage; the tree-walker marking pass + `check_call_args` elide mode; the §5 decision with
measured budget; `--no-elide`/`ASCRIPT_NO_ELIDE` permanent kill switch; paranoid mode;
differential elide axis + cross-axis + fuzz axis + count-parity + coverage assertions; the
rule-6 fix; typed bench variants + typed examples (intro + advanced, happy + gradual-boundary
edge); docs (`docs/content/language/type-contracts.md` gains "Annotations and performance");
`CLAUDE.md`/`roadmap.md`/`goal-perf.md` updates.

**Deferred (recorded, owner-noted — each becomes eligible only with its named precondition):**
- **Interface/class-typed (`Named`) contracts** — needs an env-resolution-stability proof (the
  runtime resolves contract names through the callee frame's env chain; the checker resolves
  lexically) AND checker-vs-runtime conformance granularity reconciliation (typed-signature
  assignability vs name+arity). §2.2.
- **Deep container contracts (`array<int>` etc.)** — point-in-time provable for literal
  initializers (row 2) but excluded v1 to keep the audit minimal; interior mutation makes them
  permanently ineligible as *binding anchors*.
- **Method calls / constructors** — blocked on TYPE NB#3 (generic-method arg checking) and a
  receiver-certainty story; `validate_into` paths stay never-elide regardless (row 9).
- **Worker-slice elision; loop-var anchoring; field-default elision (row 7); logical-op
  anchoring** — each a small additive follow-up with its precondition named in §2.3/§3.

**Rejected:**
- **Changing check semantics for elision's benefit** — the collector consumes verdicts; it
  never relaxes (or tightens) what `check` reports. The one verdict change in this branch
  (rule 6 → `Unknown`) is a bug fix under Gate 14, in the silent direction, with its own test.
- **Eliding on raw `Compat3::Yes`** — unsound three ways (§0). The predicate is (E)(Y)(A).
- **Eliding from inferred-but-unannotated types** — annotations are the contract; the runtime
  has no check at an unannotated slot to elide anyway (only synth-sourced *arguments* matter,
  and those are governed by anchoring). Inference-only call-proofs (e.g. eliding because an
  inferred return "should" be int) add checker-trust surface for ~no win. v1 anchors only on
  runtime-enforced facts.
- **`.aso` elision-metadata section (option c)** — `Op::CallElided` is already the durable,
  minimal encoding; a side-table section is strictly more format for no benefit.
- **A "strict mode" dialect** — no second dialect; the gradual model stands (campaign rule).
- **VM-only elision (tree-walker keeps checks)** — rejected; it makes *default* four-mode
  identity conditional on checker perfection and conflates engine identity with proof
  verification (§2.4). The verification job belongs to the elide-off axis + paranoid mode.
- **Per-arg partial elision bitmasks at call sites** — site-level all-or-nothing is simpler,
  and a single unprovable arg usually means a gradual call site whose other args are cheap to
  keep checking; revisit only with profile evidence.
- **Making `run` enforce the blocking type gate while we're at it** — out of scope, behavior
  change with corpus consequences (§5.3).

### 8.2 LANE Task-0 interaction

ELIDE is scheduled "alongside any wave" (`goal-perf.md`). If LANE's bench-corpus task has not
merged when ELIDE executes, ELIDE's plan Task 0 creates `bench/profiling/call_heavy.as` +
`call_heavy_typed.as` in the LANE harness's shape (a `run.sh`-driven `.as` workload, headline
counter printed via `time.monotonic()`), and LANE rebases onto them. No double corpus.

## 9. Grounding (verified 2026-06-12, against the working tree at `feat/debugger-profiler`-era `main`)

- **Runtime contract sites:** `src/interp.rs:7898` (`check_call_args`; per-param checks
  `:7977-7990`, rest `:8000-8031`, arity `:7929-7968`); `:5172` (`run_body` call-in);
  `:5195-5201` (default-value contract); `:5220-5226` (return contract, env-aware);
  `:2956-2968` (`Stmt::Let` + the "does not contract-check later assignments" doc);
  `:5568` (`check_type_env`); `:8105` (`check_type`); `:8060` (`auto_init_bindings`).
- **VM sites:** `src/vm/run.rs:1570` (`Op::Call | Op::CallSpread` arm); `:2240`
  (`Op::CheckParam`); `:2257` (`Op::CheckLocal`); `:5796-5807` (`return_from_frame` ret check —
  note it is env-FREE `check_type` while the tree-walker's is env-aware; both agree for
  ElideSafe forms, one more reason `Named` is excluded); `:4462+` (`Vm::call_value` →
  `check_call_args` at `:4511`/`:4534`); `:5616`/`:5719` (method paths); `:117`
  (`specialize` kill-switch precedent), `:221-226` (`new_generic`).
- **Compiler:** `src/compile/mod.rs:1009` (`compile_source`); `:3342` (CheckParam emission);
  `:3905-3911` (`emit_check_local`, span = initializer code span); `:4882`/`:5034`/`:5251`
  (`Op::Call` emissions); `src/vm/chunk.rs:446` (`type_consts`), `:492-518` (`FnProto`,
  `params`, `ret`).
- **Checker:** `src/check/infer/ty.rs:330-345` (`assignable`, rule 1 `Any`⇒`Yes`), `:109-115`
  (`Compat3`); `src/check/infer/pass.rs:118-151` (`emit`/`emit_with`), `:225` (`walk_let`,
  blocking), `:265` (`walk_return`), `:486` (`walk_loop` — no loop-var binding), `:496`
  (`walk_fn`), `:528` (`bind_params` — rest⇒Any, defaults unchecked), `:565`
  (`check_field_default`), `:607` (`check_against`), `:675-678` (`AssignExpr` ⇒ Any, no env
  update), `:902` (`synth_call`), `:1138-1174` (`check_call_args`, spread bail `:1146`, NO
  arity), `:1964` (`resolve_in_file_fn`, unique-binding gate); `src/check/infer/mod.rs:27`
  (`check`), `:36` (`hover_type_at` — the collection-mode precedent);
  `src/check/infer/env.rs` (`BindingKey::{Global,Local}`).
- **Resolver:** `src/syntax/resolve/types.rs:34` (`Binding { mutated, mutable, is_global, … }`).
- **CLI/pipeline:** `src/main.rs:605` (Run), `:744-760` (parse + resolver-blocking pre-gates —
  NO type gate on `run`), `:493` (`tally`), `:918+` (check command, `analyze_with_config`);
  `src/lib.rs:1983` (`collect_blocking_diagnostics` — resolver `blocking` only), `:127`
  (`run_file_with_packages` → `load_module`), `:1937` (`run_file_on_vm` +
  `ASCRIPT_NO_SPECIALIZE` seam).
- **AST:** `src/ast.rs:150` (`Type` enum incl. `Param`/`FnSig` erasure docs), `Stmt::Fn`
  (`span`, `name_span`), `ExprKind::Call` (`ast.rs:33`); `src/value.rs:837`
  (`Function { ret, … }`).
- **Format:** `src/vm/aso.rs:167` (`ASO_FORMAT_VERSION = 27`); `src/vm/opcode.rs:200/205/544`
  (CheckParam/Call/CheckLocal); `src/vm/verify.rs:252-254`, `:609-620` (Check* effects/operand
  validation — the pattern `CallElided` follows).
- **Live probes (this machine, release build, 2026-06-12):** §3.4's four probes; timing
  envelope §5.1 (`run` trivial ≈5.5 ms, `check` trivial ≈4.0 ms, `check` 266-line ≈6.8 ms,
  20-iteration loops, same session).
- **Precedents:** Static Python / Cinder (Instagram) — static types compiled to elided/cheap
  checks with gradual boundaries enforced; PEP 659 side-table discipline (`src/vm/adapt.rs`) —
  proof-adjacent state never perturbs `Chunk.code` semantics; the JIT spec's coverage-assertion
  rule (anti-false-green) adopted in §6.4; Siek–Taha gradual soundness (checks live at the
  typed↔untyped boundary).
