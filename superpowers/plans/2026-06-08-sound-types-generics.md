# TYPE — Sound Gradual Types & Generics — Implementation Plan

> REQUIRED SUB-SKILL: superpowers:subagent-driven-development (fresh implementer + independent reviewer
> per task; reviewer runs the commands and probes edges). Steps use `- [ ]`.

**Spec:** `superpowers/specs/2026-06-08-sound-types-generics-design.md`. **Branch:**
`feat/sound-types-generics` off `main`. **Depends on:** NUM (`Value::Int`/`Float`, `CheckTy::Int`/`Float`,
`number` desugars to `Union([Int,Float])`), ADT (generic enum payloads), IFACE (structural interfaces +
the runtime `conforms` half + the reserved `CheckTy::Interface`/`InterfaceInfo` names + `implements-violation`
ownership) — **all merged first**. **Breaking diagnostically, NOT behaviorally:** no program's runtime
output changes; an annotated-and-provably-wrong program that built before now fails the type gate. The
corpus is *migrated*, not exempted (Gate 7).

**THE SAFETY PROPERTY (state it in every task that could touch runtime — none should):** this is a
**STATIC-ONLY** spec. No tree-walker change, no VM change, **no `.aso` change → `ASO_FORMAT_VERSION` is NOT
bumped** (`src/vm/aso.rs`), `src/vm/verify.rs` untouched, **`tests/vm_differential.rs` is not modified and
stays green in both configs** (§7). Generics are runtime-**erased**: a `T`-annotated slot checks as `any`
(accept-anything) in `check_type` (`interp.rs`). Any task that finds itself editing an engine file or
bumping `ASO_FORMAT_VERSION` has gone wrong.

**Architecture:** Two deliverables. **(1) Soundness:** flip a `Compat3::No` on a *syntactically-annotated*
slot from advisory `Warning` to blocking `Severity::Error`, realized as a **severity argument on `emit`**
(`pass.rs:118`, hard-codes `Severity::Warning` at `:127`) — the single chokepoint covering ALL FOUR
annotated sites: `walk_let` (`pass.rs:206`→`check_against` `:569`), `walk_return` (`pass.rs:245`→
`check_against` `:569`), **`check_call_args` INLINE** (`assignable` `pass.rs:892`, `emit` `:899`), and
**`check_field_default` INLINE** (`assignable` `pass.rs:552`, `emit` `:561`). The two inline sites do NOT
route through `check_against` and must get `Severity::Error` passed directly. `possibly-nil` and
inferred-slot diagnostics stay `Warning`. **(2) Generics:** `CheckTy::Var(VarId, Option<Box<CheckTy>>)` +
`FnSig` + `ClassApp(ClassId, Vec)` + `EnumApp(EnumId, Vec)` + `Interface(InterfaceId)` (`ty.rs:41`);
occurs-checked union-find unification (new `unify.rs`); local argument-driven inference + explicit type
args; a **NEW genuinely-invariant arm** for `ClassApp`/`EnumApp`/parameterized interfaces (both-directions
`assignable` per type-arg pair) that leaves the **covariant built-in rule 8 untouched** (`ty.rs:398`–`431`);
interface bounds via TYPE's `conforms` predicate (IFACE owns `implements-violation`, consumes TYPE's
`conforms`).

**THE CARDINAL GATE-5 INVARIANT (unit-tested directly, repeated in every generics task):** an unsolved or
unbounded `Var` → **`Unknown`, NEVER `No`** (it is `Any` generalized). `synth([])` already returns
`Array(Any)` (`pass.rs:913`–`914`, short-circuits BEFORE the `Never` seed at `:916`) — TYPE **must NOT**
re-seed the empty literal with `Never`. A failed unification (occurs-check, arity, depth-cap) degrades to
`Unknown`. `examples/**` emits **zero** `type-mismatch`/`type-error`/`possibly-nil` in BOTH feature configs
— a CI tripwire (Task 14) is the regression gate for the whole spec.

**Tech stack:** Rust; `src/check/infer/{ty,table,pass,unify,mod}.rs`; both front-ends (`src/parser.rs`,
`src/syntax/{parser,kind}.rs`); tree-sitter (`--abi 14` + publish); `src/ast.rs` + `src/fmt.rs`;
`src/check/config.rs`; `src/lsp/`. Wired after `rules::ALL` via `infer::check` (`analyze.rs:83`).

---

## Shared API Contract (pinned to current code, verified)
**Existing (verified):** `emit` hard-codes `Severity::Warning` `pass.rs:127`; `check_against` `pass.rs:569`;
`walk_let` ann branch `pass.rs:211`–`216`; `walk_return` `pass.rs:249`; `check_call_args` inline
`assignable`/`emit` `pass.rs:892`/`:899` (`from_type_node` at `:891`); `check_field_default` inline
`pass.rs:552`/`:561` (`from_type_node` at `:541`); `synth_call` `pass.rs:785`; constructor path returns
`CheckTy::Class(cid)` `pass.rs:795`; `fn_return_type` `pass.rs:1121`; `synth_array` empty/spread →
`Array(Any)` `pass.rs:913`–`914`, `Never` seed only for non-empty `:916`. `CheckTy` enum `ty.rs:41`;
`from_type_node` unknown-name → `Any` `ty.rs:115`, unknown generic head → `Any` `ty.rs:150`; `assignable_depth`
`ty.rs:247`, rule 1 (`Any`) `:255`, rule 8 covariant `:398`–`431`, rule 11 default `Unknown` `:434`;
`TYPE_DEPTH_CAP=8` `ty.rs:23`, `UNION_WIDTH_CAP=8` `ty.rs:20`; `widen` `ty.rs:184`, `display` `ty.rs:482`,
`discriminant_order` `ty.rs:554`, `secondary_key` `ty.rs:582`, `normalize` `ty.rs:594`. `Table`/`ClassInfo`/
`EnumInfo` `table.rs:18`/`:29`/`:36`; `Table::build` two-pass `table.rs:47`; `class_id`/`enum_id`/
`method_return` `table.rs:126`/`:136`/`:192`. `RULE_CODES` (incl. the three type codes) `config.rs:48`–`50`;
`config.effective` `config.rs:92`. `tally` sets `any_error` on `Severity::Error` `main.rs:165`.
`hover_type_at` `infer/mod.rs:37`. AST `Type` enum `ast.rs:142` (no `Param`/`FnSig` today). Legacy
`parse_type_atom` `parser.rs:517`. CST `type_primary` `syntax/parser.rs:1240`, `GenericType` complete
`:1262`, `TypeArgs` kind `kind.rs:91`.
**New names (do not rename):** `CheckTy::{Var,FnSig,ClassApp,EnumApp,Interface}`, `VarId`, `InterfaceId`;
`InterfaceInfo` (table); `ast::Type::{Param,FnSig}`; `SyntaxKind::{TypeParams,TypeParam,TypeBound,FnType}`;
`parser::TypeParam{name, bound}`; module `src/check/infer/unify.rs`.
**Rebase note:** NUM has already added `CheckTy::Int`/`Float`, `ast::Type::Int`/`Float`, and desugared
`number → Union([Int,Float])`; ADT/IFACE have added their variants. Every exhaustive match TYPE touches
(`check_type` in `interp.rs`, `fmt.rs`, `ast.rs` `Display`, `ty.rs` `widen`/`display`/`assignable`/sort-keys)
must already cover those arms — add TYPE's arms on top (a missing arm is a compile error, by design).

## Conventions (every task)
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- `cargo test` AND `cargo test --no-default-features` green; clippy clean BOTH configs
  (`--all-targets` AND `--no-default-features --all-targets`).
- **No engine/`.aso` edit, no `vm_differential.rs` edit, no `ASO_FORMAT_VERSION` bump** — if a task needs
  one, the design is wrong. Reviewer greps `git diff` for `src/vm/`, `src/interp.rs` *behavior*, and
  `ASO_FORMAT_VERSION` and confirms they are absent (a `check_type` `Type::Param`/`Type::FnSig` arm is the
  ONLY permitted `interp.rs` touch, and it is accept-anything — no behavior change).
- The lattice silence discipline: every NEW `assignable` arm's fall-through is `Unknown`; only a provable
  concrete `No` ever emits.

---

## Task 1 — `emit` severity argument: the blocking chokepoint (soundness, no generics yet)
**Files:** `src/check/infer/pass.rs`. **Tests:** `tests/check.rs` (new fixtures, NOT in `examples/**`).
- [ ] Failing tests: `let x: number = "s"` → a `type-mismatch` at **`Severity::Error`** (fails the gate);
  `fn f(p: string) {} f(1)` → Error on arg 1; `fn f(): number { return "x" }` → Error on return;
  `class C { n: number = "x" }` → Error on field default. PLUS a paired-severity fixture: `let x: number
  = "s"` is **Error** but `let x = "s"; x - 1` style inferred misuse stays **Warning** (advisory). Assert
  via the `Analysis` diagnostics' `severity` field, not just presence.
- [ ] Change `emit` (`pass.rs:118`) to take `sev: Severity` and set `severity: sev` (replacing the
  hard-coded `Severity::Warning` at `:127`); de-dup (`legacy_spans`) and `suppress_emit` gating unchanged.
- [ ] Thread a `blocking: bool` through `check_against` (`pass.rs:569`) → passes `Error`/`Warning` to
  `emit`. `walk_let`'s annotated branch (`pass.rs:214`) and `walk_return`'s declared-return branch
  (`pass.rs:249`) pass `blocking=true`; all other `check_against` callers (inferred contexts) pass `false`.
- [ ] **Update the two INLINE sites directly** (they bypass `check_against`): `check_call_args` `emit`
  (`pass.rs:899`) and `check_field_default` `emit` (`pass.rs:561`) pass `Severity::Error` — the `expected`
  at both is `from_type_node(<annotated node>)` (`:891`/`:541`), always annotated. `possibly-nil` and
  every inferred-operand `type-error`/`type-mismatch` pass `Warning` (the conservative default when
  annotation provenance is unclear is advisory — never over-block, §3.1).
- [ ] Gate-5 spot check: `cargo run -- check` over a couple of `examples/**` files emits zero `type-*`
  (full tripwire is Task 14). Green both configs; clippy. Independent review (greps for any remaining
  hard-coded `Severity::Warning` in the emit path; confirms exactly four annotated sites pass `Error`).
  Commit.

## Task 2 — `config.rs`: annotated-slot default flip + downgrade knob documented
**Files:** `src/check/config.rs`. **Tests:** `config.rs`, `tests/check.rs`.
- [ ] Failing tests: the three codes stay in `RULE_CODES` (`:48`–`:50`); a project `ascript.toml`
  `[lint] type-mismatch = "warn"` **downgrades** a blocking annotated error back to a warning via
  `config.effective` (`:92`) — the explicit opt-out; the DEFAULT stays blocking (no override → the
  `Severity::Error` from Task 1 survives). `possibly-nil` default stays `Warning`.
- [ ] Confirm `effective`'s `default`-passthrough composes with the Task-1 severities (the emit severity is
  the `default` argument into `effective`); no code change may be needed beyond a doc comment + the test —
  ground it before editing. Green both configs; review; commit.

## Task 3 — AST: `Type::Param` + `Type::FnSig` (runtime-erased) + Display
**Files:** `src/ast.rs` (+ exhaustive arms in `interp.rs` `check_type`, `fmt.rs`, `ast.rs` `Display`).
**Tests:** `ast.rs`.
- [ ] Failing tests: `Type::Param("T")` `Display` renders `T`; `Type::FnSig(params, ret)` renders
  `fn(int) -> string`; both round-trip through the formatter. **`check_type` treats `Param` as
  accept-anything** (returns ok for any value — the erasure) and **discards the type-arg list on a generic
  `Named` head** (no runtime obligation) — assert a `T`-annotated value accepts any input at runtime.
- [ ] Add `Type::Param(String)` + `Type::FnSig(Vec<Type>, Box<Type>)` to the enum (`ast.rs:142`, on top of
  NUM's `Int`/`Float` + ADT/IFACE arms); add `Display` arms (`ast.rs` Display impl). The compiler flushes
  the missing arms in `interp.rs` (`check_type`) and `fmt.rs` — `check_type::Param` ⇒ accept-anything.
- [ ] **SAFETY-PROPERTY check:** the only `interp.rs` edit is the accept-anything `Param` arm (no behavior
  change for existing programs). Green both configs; review (confirms no engine behavior drift); commit.

## Task 4 — Legacy parser: type-param lists, `Type::Param`/`FnSig`/generic-app, NUM `>>`-split reuse
**Files:** `src/parser.rs`. **Tests:** `parser.rs`.
- [ ] Failing tests: `fn map<A, B>(...)` / `class Box<T>` / `enum Option<T>` / `interface Container<T>`
  parse a `decl.type_params: Vec<TypeParam{name, bound: Option<Type>}>`; `fn first<T, C: Container<T>>`
  parses the bound; in a *type position* an in-scope param name → `Type::Param`; `Box<int>` (known head) →
  `Type::Named("Box")` + parsed args; `fn(A) -> B` → `Type::FnSig`; nested `map<int, array<int>>` closes
  via the NUM `>>`-split helper.
- [ ] After a `fn`/`class`/`enum`/`interface` name, parse optional `< Ident (: Type)? (, ...)* >` into
  `type_params`. Extend `parse_type_atom` (`parser.rs:517`): an `Ident` matching an in-scope type param →
  `Type::Param`; a generic head + `<...>` → parsed-then-checked args (reuse NUM's `>>`-split when closing);
  a `fn(...) -> T` signature → `Type::FnSig`. This is **known type position** — no new disambiguation here.
- [ ] Green; review; commit.

## Task 5 — Legacy parser: expression-level explicit type args (`Box<int>(5)`) — the NEW disambiguation
**Files:** `src/parser.rs`. **Tests:** `parser.rs`.
- [ ] Failing tests (the paired battery): TYPE-ARG readings `Box<int>(5)`, `map<string, number>(xs, f)`
  parse as a call with explicit type args; COMPARISON readings `a < b > c`, `f(a < b, c > d)`, `a < b >
  (c)` parse as comparison chains — the **trailing `(` immediately after `>` is the only thing that
  selects the type-arg reading**.
- [ ] In expression position after a primary callee, on a `<`: **speculatively** parse `< Type (, Type)*
  >` and accept the type-arg reading ONLY if `>` is immediately followed by `(`; on any failure, rewind
  and parse a comparison expression (Rust/TypeScript technique). NUM's `>>`-split is reused *inside* the
  speculative parse; the *decision to enter* it is new. This must not regress NUM's `a >> b` shift in
  expression position.
- [ ] Green; review (probes `a < b > (c)` stays comparison, `Box<int>(5)` is a call); commit.

## Task 6 — CST parser + frontend conformance
**Files:** `src/syntax/parser.rs`, `src/syntax/kind.rs`. **Tests:** `tests/frontend_conformance.rs`,
`syntax/parser.rs` units.
- [ ] Add `SyntaxKind::{TypeParams, TypeParam, TypeBound, FnType}` (`kind.rs`, near `:86`). Add
  `type_params(p)` called from the fn/class/enum/interface decl parsers. Extend `type_primary`
  (`syntax/parser.rs:1240`): a head that is a type param → `NamedType` (lowering maps to `Var`); a generic
  head + nested `TypeArgs` (`:1262`) reuses the NUM `>>`-split; add `fn(A) -> B` → `FnType`. Mirror Task
  5's expression-level explicit-type-arg disambiguation in the CST Pratt parser (trailing-`(` decides).
- [ ] **Frontend conformance** proves the two parsers agree, INCLUDING the paired type-arg-vs-comparison
  battery from Task 5 (`Box<int>(5)`/`map<string,number>(xs,f)` vs `a<b>c`/`f(a<b,c>d)`/`a<b>(c)`).
- [ ] Green both configs; review; commit.

## Task 7 — Tree-sitter grammar + GLR conflict + publish
**Files:** `tree-sitter-ascript/grammar.js`, `queries/highlights.scm`, editors. **Tests:**
`tests/treesitter_conformance.rs`.
- [ ] Add `type_parameters`/`type_parameter`/`type_bound` rules and a `function_type` (`fn(A)->B`); allow a
  generic head + nested `type_arguments` (the `>>`-in-type handling is already in the grammar from NUM).
  **Declare a NEW GLR conflict for the expression-level explicit-type-arg case** (`expr < … > (…)` vs a
  comparison chain — NUM declared none for expression position); the GLR parser keeps both live and
  resolves on the trailing `(`.
- [ ] Regen `parser.c` (`tree-sitter generate --abi 14`) then `cargo build`. Update `queries/highlights.scm`
  (type params `@type.parameter`). **Publish:** `./scripts/sync-grammar.sh`; bump `editors/zed/extension.toml`
  `commit` + `editors/nvim/lua/ascript/treesitter.lua` `revision`; update VS Code TextMate + Zed/Neovim
  highlight copies.
- [ ] **Treesitter-conformance battery** (REQUIRED — the ambiguity is new): the same paired type-arg-vs-
  comparison set as Task 5/6 parses to the right trees. Green; review; commit.

## Task 8 — `CheckTy` variants + `from_type_node` lowering + widen/display/sort-keys
**Files:** `src/check/infer/ty.rs`. **Tests:** `ty.rs`.
- [ ] Failing tests: `CheckTy::Var(0, None)` displays as a param name (or `T`); `FnSig` displays
  `fn(int) -> string`; `ClassApp(c, [Int])` displays `Box<int>`; `widen` of a leftover `Var` → `Any`;
  `from_type_node` on a `NamedType` matching an in-scope type param → `Var`; a `GenericType` whose head is
  a user class/enum/interface → `ClassApp`/`EnumApp`/parameterized interface (today → `Any` at `ty.rs:150`
  — a strict, gradual-preserving upgrade); an unknown head still → `Any` (`ty.rs:115`/`:150`).
- [ ] Add `Var(VarId, Option<Box<CheckTy>>)`, `FnSig(Vec<CheckTy>, Box<CheckTy>)`, `ClassApp(ClassId,
  Vec<CheckTy>)`, `EnumApp(EnumId, Vec<CheckTy>)`, `Interface(InterfaceId)` to `CheckTy` (`ty.rs:41`, on
  top of NUM/ADT/IFACE arms). Wire each into `widen` (`:184`), `display` (`:482`), `discriminant_order`
  (`:554`), `secondary_key` (`:582`), `normalize` (`:594`). `from_type_node` learns the param/generic-head
  lowering (needs the in-scope type-param set + the `InterfaceInfo` table from Task 9 — stub the table hook,
  fill in Task 9; or sequence Task 9 first if cleaner).
- [ ] Green both configs; review; commit.

## Task 9 — `Table`: type params + bounds + `InterfaceInfo` + `conforms` predicate + instantiation
**Files:** `src/check/infer/table.rs`. **Tests:** `table.rs`.
- [ ] Failing tests: a `class Box<T>` records `type_params: vec![("T", None)]`; `fn first<T, C:
  Container<T>>` records `C`'s bound as `Interface(id)`; the IFACE-reserved `InterfaceInfo` table holds
  `Container`'s method-set signatures lowered to `CheckTy`; `method_return` for a generic method
  instantiates the param (`Box<int>.get()` → `int`); `conforms(Class providing every required method with
  assignable sigs, Interface)` → `Yes`, a class provably missing a method → `No`, a present-but-untyped
  method → `Unknown` (gradual, IFACE §6).
- [ ] `ClassInfo`/`EnumInfo` (`table.rs:18`/`:29`) gain `type_params: Vec<(String, Option<CheckTy>)>`;
  `Table::build` (`:47`) pass 2 records them + bounds; populate the **`InterfaceInfo`** vector (the name
  IFACE §6 reserves) with each interface's method signatures. Add the structural `conforms(t, InterfaceId)`
  predicate (Yes/No/Unknown per §4.5 — `No` only on a fully-concrete provable failure). Add generic
  field/method-return **instantiation** (substitute solved args).
- [ ] **Gate-5:** `conforms` returns `Unknown` for any partially-known `t` (never a corpus false positive).
  Green both configs; review; commit.

## Task 10 — `unify.rs`: occurs-checked union-find unifier + freshen + substitute
**Files:** `src/check/infer/unify.rs` (NEW), `src/check/infer/mod.rs` (declare the module). **Tests:**
`unify.rs` units.
- [ ] Failing tests (THE cardinal invariants): `unify(Var v, int)` binds `v:=int`; `unify(ClassApp(c,[a]),
  ClassApp(c,[b]))` unifies componentwise (same head/arity); **occurs-check:** `unify(T, array<T>)` is
  rejected → the whole inference degrades to `Unknown` results, NO hang; any side `Any` → succeeds
  vacuously; a concrete `t1 != t2` → a recorded constraint failure (NOT a panic). Depth-cap reuses
  `TYPE_DEPTH_CAP=8` and width via `UNION_WIDTH_CAP` (both already in `ty.rs`); past the cap → give up to
  `Unknown`.
- [ ] Implement `VarId` allocation, freshening (instantiate a decl's params to fresh `Var`s, substituted
  through param + return types), a union-find solver with occurs-check, and substitution. **An unsolved
  `Var` substitutes to `Any` for display and assignability** (gradual leaf). The unifier NEVER manufactures
  a `No` — a non-unification is a gradual give-up.
- [ ] Green both configs; clippy; review (probes the occurs-check + depth-cap termination); commit.

## Task 11 — `assignable` for the new variants: the INVARIANT arm + Var-bias + FnSig + Interface
**Files:** `src/check/infer/ty.rs`. **Tests:** `ty.rs`.
- [ ] Failing tests: **`assignable(Var unsolved/unbounded, anything)` and `assignable(anything, Var
  unbounded)` → `Unknown`, NEVER `No`** (the single most important invariant — assert both directions).
  `Box<int>` ↮ `Box<string>` → **`No`** (both type-arg dirs concrete-distinct); `Box<int>` ↔ `Box<any>` →
  gradual (`Yes`/`Unknown`, not `No`); **`Box<Dog>` NOT assignable to `Box<Animal>`** → `Unknown` (Dog→Animal
  Yes but Animal→Dog No ⇒ neither both-Yes nor both-No ⇒ silent — the documented v1 limitation, asserted);
  `FnSig` vs bare `Fn` → `Unknown`; `Interface(i)` destination → `conforms(src, i)`.
- [ ] Insert arms into `assignable_depth` (`ty.rs:247`), all `Unknown`-biased:
  - **`Var` (rule 1.5, before concretes):** unsolved/unbounded → `Unknown`; bounded `Var` as destination →
    check source `conforms` the bound (`No` only on a provable concrete failure).
  - **`FnSig` vs `FnSig`:** params contravariant, return covariant — but a `No` needs ALL components
    provable; bare-`fn` corpus lands on `Unknown`. `FnSig` vs `Fn` → `Unknown`.
  - **`ClassApp(c,sargs)` vs `ClassApp(d,dargs)` — NEW genuinely-invariant arm (§4.6, NOT a reuse of rule
    8):** `No` if heads provably unrelated or differing arity, OR same head with `invariant_args == No`
    (every pair concrete-and-distinct in BOTH directions); `Yes` if same head and `invariant_args == Yes`;
    else `Unknown` (any pair involving a `Var`/`Any` → `Unknown` via the Var-bias clause). `ClassApp(c,_)`
    vs `Object` → `Yes`; `Class(c)` (zero-arg) vs `ClassApp(c,_)` → `Unknown`. `invariant_args` calls
    `assignable` in BOTH directions per pair (the genuine invariance) — distinct from the covariant rule 8.
  - **`EnumApp`** mirrors `ClassApp` (same `invariant_args`).
  - **`Interface(i)` destination:** `conforms(src, i)`.
- [ ] **Leave rule 8 (`ty.rs:398`–`431`) UNTOUCHED** — the built-ins stay covariant; do NOT claim they are
  invariant. Default fall-through stays rule 11 `Unknown` (`ty.rs:434`). **NUM interplay test:** `T=int`
  flows into `: number` (`Yes`, union membership via existing rule 9) and NOT into `: string` (`No`).
- [ ] Green both configs; review (greps that no `Var`/`Any` path returns `No`); commit.

## Task 12 — `pass.rs` generic inference wiring: freshen → unify → substitute → check
**Files:** `src/check/infer/pass.rs`. **Tests:** `tests/check.rs`.
- [ ] Failing tests: `id(5)` (where `fn id<T>(x: T) -> T`) synthesizes `int`; `map(["a"], fn(s){return
  s.length})` solves `A=string, B=number` → result `array<number>`; `Box(5).get()` synthesizes `int`;
  **empty-array `map([], f)` leaves `A` unsolved → `array<any>` → ZERO diagnostics** (the pinned invariant);
  `map<string, number>([1,2], ...)` → blocking `type-mismatch` on the `int`-vs-`string` arg (explicit-vs-
  inferred conflict); `Box<int>(5)` constructor uses the explicit arg; a bounded `first<T,C: Container<T>>`
  called with a value provably lacking `at`/`len` → blocking conformance `type-mismatch`.
- [ ] Thread a `subst: HashMap<VarId, CheckTy>` instantiation context through `synth_call` (`pass.rs:785`),
  the constructor path (`:793`–`795`), and `check_call_args`: freshen the decl's params → unify
  `synth(arg)` against each freshened param (`fn(A)->B` callbacks unify structurally) → substitute → run
  the NORMAL `assignable` of each arg against its SOLVED param type (this is the step that can emit a
  blocking `type-mismatch` when the param was annotated — it always is for a generic decl, so the Task-1
  inline `check_call_args` `Error` severity applies). A param that stayed an unsolved `Var` → `Any` →
  gradual. `fn_return_type`/`method_return` (`pass.rs:1121`, `table.rs:192`) return the SUBSTITUTED return.
- [ ] Explicit type args (`Box<int>(5)`, `map<string,number>(...)`): lower directly to `CheckTy`s, no
  freshening; a conflict with an inferred constraint is a blocking `type-mismatch` on the annotated arg.
  Bound enforcement: after solving `T:=S`, `conforms(S, bound)`; provable `No` blocks (annotated origin).
- [ ] **Gate-5 invariants restated:** an un-annotated generic call (`xs: any`) stays `Unknown`;
  `array<any>` components stay gradual; a generic over an unknown type name → `Any`. Narrowing,
  `block_always_returns`, `infer_return`, hover collection — UNCHANGED. Green both configs; review (probes
  empty-array + occurs-check paths emit nothing); commit.

## Task 13 — Formatter: type-param lists + `fn(A)->B` + `Box<int>` round-trip
**Files:** `src/fmt.rs` (+ `ast.rs` `Display` already from Task 3). **Tests:** fmt idempotence goldens.
- [ ] Failing tests: `fn map<A, B>(...)`, `class Box<T>`, `fn first<T, C: Container<T>>` render canonically;
  `fn(A) -> B` and `Box<int>` round-trip-stable; idempotent (fmt∘fmt == fmt). `Type::Param`/`Type::FnSig`
  formatter arms (the `Display` arms landed in Task 3; the `fmt.rs` `write_*` arms land here).
- [ ] Green both configs; review; commit.

## Task 14 — Gate-5 CI tripwire + the zero-false-positive corpus assertion
**Files:** `tests/check.rs` (the tripwire test). **Tests:** `tests/check.rs`.
- [ ] Failing-then-green test: iterate EVERY `examples/**` `.as` file, run `analyze`/`analyze_with_config`,
  assert **zero** `type-mismatch`/`type-error` (and still zero `possibly-nil`) survive — run it in BOTH
  feature configs (the test compiles under `--no-default-features`). This is THE regression gate for the
  whole spec (§3.4): the severity flip adds no emit (proven), so the only way it trips is a generics
  `assignable`/unification bug returning `No` where it should be `Unknown` — fix the CHECKER, default to
  `Unknown`, NEVER relax the assertion.
- [ ] Property-style: a small generated battery of untyped + `any`-typed + partially-typed programs through
  generics emits no blocking diagnostic. Green both configs; review; commit.

## Task 15 — Runnable examples (Gate 9): `examples/generics.as` + an advanced one
**Files:** `examples/generics.as`, `examples/advanced/<name>.as`. **Tests:** conformance + the Task-14
tripwire + example-runner.
- [ ] `examples/generics.as` (introductory, CLEAN, runnable): a generic `fn map<A,B>(...)`, a `class
  Box<T>` and/or `Stack<T>`, a generic enum (`Option<T>` or `Result2<T,E>`), and at least one bounded
  `fn first<T, C: Container<T>>` call — zero `type-*`, runs under `target/release/ascript run`, exercised
  by the conformance tests.
- [ ] `examples/advanced/<name>.as` (production-shaped, fully error-handled): a realistic typed `Stack<T>`
  (`push`/`pop`) or a generic `Result` combinator pipeline — demonstrates inference, an explicit type arg,
  AND a bound; verified with `ascript run`. Both sit in `examples/**` so the Task-14 tripwire holds them to
  zero `type-*` automatically.
- [ ] **Verify runnable** in all engines as appropriate (`run` default VM); review; commit.

## Task 16 — LSP: hover/inlay instantiated generics + blocking diagnostics flow
**Files:** `src/lsp/`. **Tests:** `tests/lsp.rs`.
- [ ] Failing tests: hover over `Box(5)` shows `Box<int>`; hover over `map(...)`'s result shows
  `array<number>` (`hover_type_at`, `infer/mod.rs:37` shows INSTANTIATED generics); inlay hints surface
  solved type args (extend the hover-collection mode at `pass.rs:229`); the new blocking `Severity::Error`
  diagnostics flow through to the LSP; completion offers in-scope type-param names. (DX owns the inlay
  protocol surface; TYPE provides the inferred data.)
- [ ] Green both configs; review; commit.

## Task 17 — Docs + CLAUDE.md + roadmap + the static-only / ASO-unchanged note
**Files:** `docs/content/language/type-contracts.md`, `README.md`, the design spec's type section,
`CLAUDE.md`, `superpowers/roadmap.md`. **Tests:** docs serve check (manual).
- [ ] Append a "Generics" + "Sound typing" section to `docs/content/language/type-contracts.md`: the
  blocking-vs-gradual rule (annotated → Error, `any`/unannotated/`Unknown` → silent), generic syntax,
  bounds, **the invariance limitation + the `fn<T>` workaround** (no silent surprise, Gate 6), the
  `ascript.toml` downgrade opt-out. Update the README types table. NAV unchanged (appended to an existing
  page — no new slug, so no `docs/assets/app.js` NAV edit).
- [ ] Update the SP10 paragraph in `CLAUDE.md` (advisory → blocking-for-annotated + generics; **note the
  static-only safety property: no `.aso` bump, `vm_differential` untouched**); add a roadmap entry. A CI
  note asserts `ASO_FORMAT_VERSION` is unchanged by this branch (§9.4). Review; commit.

## Done when
Every task is checked behind an independent review; the soundness flip blocks at all FOUR annotated sites
(two via `check_against`, two inline) while `possibly-nil` and inferred slots stay advisory; generics
infer/unify/substitute with the cardinal **Var → Unknown, never No** invariant and the genuinely-invariant
`ClassApp`/`EnumApp`/interface arm (rule 8 left covariant); the empty-array `synth([])` stays `Array(Any)`;
the Gate-5 tripwire (Task 14) shows **zero `type-*` on `examples/**` in BOTH feature configs**; a runnable
`examples/generics.as` + an advanced example pass (Gate 9); both parsers + tree-sitter (regen + publish +
editor pins) + formatter + LSP are green (Gate 9 tooling parity); clippy + `cargo test` green in BOTH
configs; and **the safety property holds — no engine edit, `vm_differential.rs` unmodified and green,
`ASO_FORMAT_VERSION` NOT bumped, `src/vm/verify.rs` untouched**. `conforms` is owned here; IFACE emits
`implements-violation` consuming it. Merge `--no-ff` to `main`.
