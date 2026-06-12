# AScript Language Specification + Stability Policy (LSPEC) — Design

- **Status:** Draft for review
- **Date:** 2026-06-12
- **Code:** LSPEC (Flagship & ecosystem track, `goal-perf.md:321`)
- **Depends on:** nothing (documentation-and-governance work; owner-sequenced, independent of all
  engine specs). Coordinates with DOCS (`2026-06-12-docs-reconciliation-design.md`, NAV bijection
  ownership, §7.3) and SIG (`2026-06-12-lsp-stdlib-signatures-design.md`, stdlib signature
  ownership, §3.14) — both order-independent, neither a hard dependency.
- **Depended on by:** the 1.0 decision (§5.5); EMBED's stability contract slots into §5.2's
  INTERNAL tier when it lands.
- **Engines:** none touched. **The only code is two drift-guard test groups in a new
  `tests/spec_drift.rs`** (§7). No grammar change, no `.aso` bump (`ASO_FORMAT_VERSION` stays 27,
  `src/vm/aso.rs:167`), no `Value` change, `vm_differential` untouched.
- **Breaking:** no.

---

## 1. Summary & motivation

AScript today has an *authoritative design doc* (`superpowers/specs/2026-05-29-ascript-design.md`),
a *tutorial guide* (`docs/content/language/`), a *milestone record* (`superpowers/roadmap.md`), and
a *de-facto executable spec* (the tree-walker oracle + the four-mode differential over
`examples/**` + `tests/vm_goldens/**`). What it does not have is a **normative specification**: a
versioned document set that states, in MUST/SHOULD language, what the language *is* — independent
of which file in `superpowers/` happens to record which campaign decision — plus a **stability
policy** that tells a user which surface they can build on and a **process** for changing it.

The gap is real and verified: the 2026-05-29 design doc still carries superseded text as
load-bearing prose (§1 non-goals say "no tagged-union enums" — ADT is merged; §8.2 says "no
associated typed payloads"; §3's precedence list lacks the ternary/bitor/range tiers the shipped
grammar has, `tree-sitter-ascript/grammar.js:20-38`; §10.1 cites a `grammar/tree-sitter-ascript/`
path that does not exist; §15 lists the package manager and debugger as open questions — both
shipped). The doc is honest about its own drift (it patches itself with "superseded" callouts),
but a reader cannot extract "the language, as of today" from it without also reading eleven
campaign specs. LSPEC fixes that by making **one normative document set, verified against the
implementation as written**, with mechanical drift guards so it cannot silently rot.

Three deliverable groups:

1. **The specification document set** — 16 chapters under `docs/content/spec/` (§3), served by the
   existing docs site, each chapter citing the conformance tests/examples that pin its claims.
2. **Conformance-suite adoption + stability policy + RFC-lite process** (§4, §5, §6) — governance
   text in the spec's own stability chapter, `CONTRIBUTING.md`, and a `superpowers/rfcs/` template.
3. **Drift guardrails** (§7) — `tests/spec_drift.rs` (grammar-rule coverage + chapter manifest +
   citation existence, each with a deliberate-mutation self-test) and a new item on `CLAUDE.md`'s
   "Touching syntax" cross-cutting checklist.

**The cardinal rule of drafting (Gate 14, made operational):** every normative claim in a chapter
is **verified against the implementation as written** before the chapter is accepted — the plan's
per-chapter tasks each run the cited examples/tests. Any spec-vs-implementation contradiction
found is a **bug to triage**: either the spec text or the implementation is wrong, the owner
decides which, and the loser gets fixed with a regression test (or, where the fix is genuinely too
large to absorb, filed with an owner note AND a failing test — never silently spec'd around).

## 2. Decisions up front (each with its justification)

| # | Decision | Choice | Why (alternative rejected) |
|---|---|---|---|
| D1 | Spec location | **`docs/content/spec/`**, a new "Specification" NAV section | Reachability: the docs site is the one place users already read; a top-level `spec/` dir would be repo-only, invisible to the site, and would need a second publishing pipeline. The site is data-driven (`docs/assets/app.js` `NAV:11-62`) so a new section is pure data. Cost: NAV entries required (Gate 13) and coordination with DOCS's NAV⇄files bijection tripwire — handled in §7.3. |
| D2 | EBNF derivation | **Hand-written normative EBNF + a mechanical rule-name drift test** | A generator script over `grammar.js` was rejected: the grammar is a JS DSL with GLR `conflicts`, deliberately precedence-LESS rules (`propagate_expression`/`unwrap_expression`, `grammar.js:55-68`), and `prec`/`prec.right` annotations that do not map mechanically to *readable* EBNF — generated output would be a token-soup nobody can read, and the generator itself a second grammar to maintain. Instead: hand-written EBNF whose every production block is annotated with the tree-sitter rule names it covers, plus a drift test asserting **every named rule in `grammar.js` appears in the EBNF chapter** (§7.1). Honest limitation, stated in the chapter itself: the test proves *coverage of rule names*, NOT *equivalence of languages* — equivalence is pinned by the conformance suite (both parsers accept the whole corpus, `tests/treesitter_conformance.rs` + `tests/frontend_conformance.rs`), which is the strongest equivalence proof the project has. |
| D3 | Spec-pages-in-NAV enforcement | **Owned by DOCS (tripwire 4), not duplicated here** | DOCS's `tests/docs_drift.rs` tripwire 4 asserts the NAV ⇄ `docs/content` bijection both directions (DOCS spec §5.4). LSPEC adds pages + NAV entries and relies on that tripwire once DOCS merges; until then the `CLAUDE.md` NAV-orphan rule governs socially and LSPEC's plan verifies NAV + served-site rendering manually (Gate 13). `tests/spec_drift.rs` deliberately contains **no NAV logic** (coordinate, don't duplicate). |
| D4 | RFC home | **`superpowers/rfcs/`** (+ a `CONTRIBUTING.md` section) | The existing spec→review→lock cadence lives under `superpowers/`; an RFC is the lightweight front door to that cadence, so it belongs beside it. A `.github/` issue/PR template was rejected: the repo currently ships no issue templates, and an RFC that graduates becomes a `superpowers/specs/` doc — keeping the whole lifecycle in one tree beats splitting it across GitHub UI config. |
| D5 | Language version identity | **The language version IS the crate version** (`Cargo.toml` `version = "0.6.0"` today); the spec declares "AScript 0.x" current | One binary is the one implementation; a separate language-version counter would immediately drift. Pre-1.0 rule: a **minor** bump accompanies any breaking change to the STABLE surface (with corpus migration notes per Gate 7); patch releases are non-breaking. §5.3. |
| D6 | What "conforms" means | **Four-mode byte-identity over the adopted suite is THE conformance criterion** (§4) | It is already the project's strongest, continuously-enforced invariant (goal.md Gate 1); promoting it to the public conformance definition costs nothing and is honest: an alternative implementation conforms iff it matches the suite, exactly as the in-tree engines must. |

## 3. The specification document set (`docs/content/spec/`)

Sixteen Markdown chapters. The spec is **NORMATIVE**; `docs/content/language/` stays the
**tutorial** (the guide teaches, the spec defines — pages cross-link but never duplicate
authority; where they disagree, the spec wins and the disagreement is a bug). Chapters use
RFC-2119-style terms defined in chapter 1. Every chapter ends with a **"Conformance" section**
citing the test files / example programs that pin its claims — the citations are mechanically
checked to exist (§7.2).

Chapter inventory (slug → title → normative content sources → primary conformance citations):

| # | Slug | Chapter | Content is verified against | Pins (cited in-chapter) |
|---|---|---|---|---|
| 1 | `spec/intro` | Notation, terms & conformance | — (definitions) | `tests/vm_differential.rs` (the criterion, §4) |
| 2 | `spec/lexical` | Lexical structure | `src/lexer.rs`, `src/syntax/` lexer, design doc §2 | `tests/frontend_conformance.rs`, `examples/strings.as`, `examples/numbers.as` |
| 3 | `spec/grammar` | Grammar (normative EBNF) | `tree-sitter-ascript/grammar.js` (the derivation source) | `tests/treesitter_conformance.rs`, `tests/frontend_conformance.rs`, `tests/spec_drift.rs` |
| 4 | `spec/values` | Values & types | `src/value.rs`, NUM spec, CLAUDE.md value-model section | `examples/core_types.as`, `examples/integers.as`, `examples/num_int_float_edges.as`, `examples/numeric_tower.as`, `examples/map_literals.as` |
| 5 | `spec/expressions` | Expressions & operators | `grammar.js` PREC ladder, `src/parser.rs` tiers, CLAUDE.md `?`/`!` notes | `tests/vm_differential.rs` (ternary/propagate battery), `examples/all_features.as`, `examples/ranges.as`, `examples/range_step_default.as` |
| 6 | `spec/statements` | Statements & declarations | `src/ast.rs` `Stmt`, resolver semantics (globals, const, redeclaration) | `examples/functions.as`, `examples/default_params.as`, `examples/rest.as`, `examples/object_destructuring.as`, `examples/spread.as` |
| 7 | `spec/classes` | Classes, enums, interfaces & generics | design doc §8, ADT/IFACE/TYPE specs, `src/interp.rs` | `examples/oop.as`, `examples/records.as`, `examples/static_methods.as`, `examples/enums_adt.as`, `examples/interfaces.as`, `examples/generics.as`, `examples/advanced/interface_dispatch.as` |
| 8 | `spec/patterns` | Pattern matching & exhaustiveness | CLAUDE.md match notes, ADT spec, `src/check/infer/pass.rs` | `examples/pattern_matching.as`, `examples/match_or_patterns.as`, `examples/enums_adt.as`, `examples/advanced/state_machine.as` |
| 9 | `spec/errors` | Errors: the two-tier model | design doc §6, `Control` enum, SP3 recursion guard | `examples/result.as`, `examples/force_unwrap.as`, `examples/deep_recursion.as`, `examples/advanced/typed_errors.as` |
| 10 | `spec/modules` | Modules, imports & packages | design doc §9, SP6 spec, `classify_specifier` | `tests/modules.rs`, `tests/pkg.rs`, `examples/modules/` |
| 11 | `spec/concurrency` | Concurrency: async, generators, workers | design doc §7, workers specs A/B, `src/task.rs`, `src/coro.rs`, `src/worker/serialize.rs` | `examples/async.as`, `examples/concurrency.as`, `examples/structured_concurrency.as`, `examples/generators.as`, `examples/workers_parallel_map.as`, `examples/advanced/workers_actor_counter.as`, `tests/m17_structured_concurrency.rs` |
| 12 | `spec/capabilities` | Capabilities | FFI/caps spec, `src/stdlib/caps.rs`, `required_cap` | `tests/cap_audit.rs`, `examples/caps_sandbox.as` |
| 13 | `spec/types` | Gradual typing: contracts & the static checker | design doc §5, TYPE spec, `src/check/infer/` | `examples/typed.as`, `examples/typed_fields.as`, `examples/optional_types.as`, `examples/typed_parse.as`, `tests/check.rs` (`corpus::` Gate-5 tripwire) |
| 14 | `spec/stdlib` | Standard-library conformance (pointer) | `src/stdlib/mod.rs` `STD_MODULES`, docs reference | `examples/stdlib_completeness.as`, DOCS tripwire 3, SIG drift test |
| 15 | `spec/conformance` | The conformance suite | `tests/vm_differential.rs`, goldens, conformance tests | (it IS the mapping — §4) |
| 16 | `spec/stability` | Stability policy & versioning | §5 of this design | `tests/spec_drift.rs` (chapter manifest) |

Per-chapter content notes — what each chapter MUST state (the non-obvious, load-bearing
normatives; the plan's tasks carry the full outlines):

- **Ch.1 Notation & conformance.** Defines MUST/SHOULD/MAY (RFC-2119-style), plus the project's
  three honesty categories: **implementation-defined** (the implementation chooses and documents —
  e.g. the `std/intl` ICU subset boundary, HTTP trailers best-effort), **unspecified** (any of a
  set of behaviors, no documentation duty — e.g. `map` iteration order beyond what `Map` promises,
  OS-scheduler-dependent worker interleaving), and **forbidden-undefined** (AScript has NO
  undefined behavior: every error is a Tier-1 value, a Tier-2 panic, or a clean compile/verify
  error — silent wraparound/truncation are bugs by pillar 1). Declares conformance per §4 and the
  documented engine asymmetries (bytecode-capacity limits are VM-only; the tree-walker has no
  bytecode caps — design doc §6, `tests/vm_limits.rs`).
- **Ch.2 Lexical.** UTF-8 source; comments; ASI-lite (`;` an optional separator in
  statement lists AND class bodies, never a `,` substitute); reserved keywords vs CONTEXTUAL/soft
  keywords — the full honest split (`step`, `as`, `extends`-on-interfaces, `implements`, `from`
  are contextual; `interface` IS reserved; `self`/`super` are soft in the tree-sitter grammar) —
  verified against `src/lexer.rs` `Tok` and `grammar.js`'s "NOT reserved here" header note;
  literal forms (`0x`/`0b`/`0o`/`_`, float vs int discrimination, `0m` decimals, strings,
  templates with nested-template `${…}`).
- **Ch.3 Grammar.** The normative EBNF (D2). Organization mirrors `grammar.js` top-down
  (program/items → statements → expressions → patterns → types). Each production block carries a
  `covers: ts(rule_a, rule_b)` annotation line — the drift test's anchor (§7.1). States the three
  GLR ambiguity resolutions in prose beside the affected productions: ternary-vs-propagate `?`
  (a `?` is ternary only when a `:` follows at bracket-depth 0 before statement end), unwrap `!`
  (precedence-less, mirror of propagate), arrow-params vs parenthesized expr. States D2's
  limitation paragraph verbatim ("this test proves rule-name coverage, not language equivalence;
  equivalence is pinned by the two-parser conformance suite").
- **Ch.4 Values & types.** The authoritative user-facing kind enumeration **read from
  `src/value.rs` at drafting time** (nil, bool, int, float, decimal, string, array, object, map,
  set, bytes, regex, function, enum/variant, class/instance, interface, future, generator, native
  handles, shared/frozen) with each kind's mutability + sharing; the NUM numeric model (int/float
  subtypes, type-directed division, checked overflow + `+% -% *%`, int-only bitwise at Go
  precedence, `number = int | float`, float printing always shows a decimal); **truthiness = the
  NUM falsy set** (`nil false 0 0.0 -0.0 NaN 0m ""` — empty collections stay TRUTHY); **equality
  vs identity**: `==` structural for scalars/strings (exact across numeric subtypes, `1 == 1.0`),
  **pointer identity for containers** (`[1]==[1]` is `false`), identity for
  function/future/generator/interface, structural for constructed enum-variant payloads (ADT);
  `MapKey` canonicalization (−0.0→+0.0, NaN unified, integral float folds to int key).
- **Ch.5 Expressions & operators.** The full precedence table transcribed from the `grammar.js`
  PREC ladder (assign < ternary < `??` < `||` < `&&` < equality < comparison/`instanceof` <
  bitor `| ^` < range `..`/`..=` < add `+ - +% -%` < mul `* / % *% << >> &` < `**` (right) <
  [precedence-less postfix `?`/`!`] < unary `! - ~ await` < postfix call/member/index/`?.`) —
  including the two subtleties as normative rules: the `?`-disambiguation algorithm and the
  unwrap-tier placement (`?`/`!` bind LOOSER than `await`/prefix ops: `await x!` ≡ `(await x)!`,
  `f()? - 1` is propagate-then-subtract, `a ? -b : c` is ternary). Safe access `?.`/`??`
  short-circuiting incl. `a?.m(args)` not evaluating args on nil receiver; ranges + `step`
  semantics (direction-following, value-position materialization, `resolve_step` panics);
  spread strictness.
- **Ch.6 Statements & declarations.** `let`/`const` (+ destructuring incl. rename/rest,
  missing-key→nil), redeclaration + const-immutability as RUNTIME-timed errors (dead code never
  errors; RHS side effects run first), module-scope user-globals + late binding
  (forward-reference legality), `if`/`while`/`for of`/`for in`-range/`for await`,
  `break`/`continue`/`return`, function declarations (defaults: call-time, left-to-right, earlier
  params in scope, explicit `nil` suppresses; rest params last + element-checked).
- **Ch.7 Classes/enums/interfaces/generics.** Classes (init, `self`, single inheritance, `super`,
  statics in a separate namespace, generator methods, `async fn init` forbidden, `from` reserved,
  records auto-derived init, typed fields + `.from` validation); ADT enums (unit XOR payload,
  positional XOR named fields, first-class variant constructors, structural payload `==`,
  `.value`/`.name` reflection); interfaces (structural conformance = name + arity in v1,
  `instanceof` extension, lazy `extends` flattening + cycle guard, `implements` is
  checker-documentation only, interface-typed contracts via `conforms`); **generics are
  runtime-ERASED** — `fn<T>`/`class Box<T>` surface syntax type-checks statically
  (occurs-checked unification, invariant `ClassApp`, interface bounds) but a `T` slot is
  accept-anything at runtime; the chapter states erasure as a normative property (no runtime
  reification, no `.aso` encoding).
- **Ch.8 Patterns & exhaustiveness.** The pattern grammar (wildcard/ident/value/range/array/
  object/variant, or-patterns, guards); **Option C binding rule** (a bare ident already in scope
  compares with `==`; undefined binds; object shorthand `{key}` always binds) and its lint
  (`enum-variant-binding-shadow`); exhaustiveness is STATIC (`non-exhaustive-match` default
  Error, gradual-silent on unproven subjects) with the runtime `MatchNoArm` backstop; the
  qualified-variant guidance is normative SHOULD.
- **Ch.9 Errors.** No exceptions; Tier-1 `[value, err]` pairs + `Ok`/`Err`, `error` ≡
  `object | nil`, `Result<T>` sugar; `?` propagation (compile-time validity rule);
  `!` force-unwrap (recoverable panic with the original message); Tier-2 panics (contract
  failure, OOB `[]`, nil member, recursion-depth `maximum recursion depth exceeded` —
  byte-identical on both engines); `recover(fn)` as the single host boundary. The chapter
  documents the recorded carry-forward defect (CLAUDE.md): `recover(fn(){…})` with an
  anonymous-fn-EXPRESSION argument fails ("function declaration has no resolver binding");
  arrow form works — **owner triage at drafting time** (fix in-branch with regression test, or
  spec the limitation with the owner note; Gate-14 path, plan Task 2.9).
  **DEFER coordination (2026-06-12):** the `defer` statement's grammar flows into Ch.3 and Ch.6
  automatically from the tree-sitter grammar and `Stmt::Defer` AST addition, but the **Ch.9
  semantics chapter must absorb DEFER's §3 frame-exit matrix** — the complete table of when defers
  run (normal return, `?` propagation, panic unwind) and when they do not (`exit()`, task
  cancellation, `gen.close()`/last-drop) — plus the §3.6 merge rules (defer panic replaces
  return, supersedes propagation, appends-as-suppressed into existing panic) and the `defer await`
  rule (bare future-returning defer is a Tier-2 error). Conformance pins: `examples/defer.as`,
  `examples/advanced/defer_resources.as`, `tests/vm_differential.rs` defer battery.
- **Ch.10 Modules & packages.** ESM-style, no default exports, evaluate-once + cache, circular
  import semantics; specifier classification (`Std`/`Relative`/`Package`/`UnknownPackage`) and
  the SP6 resolution pipeline (MVS, content-addressed store, lockfile, `--locked` fail-closed);
  bare-version registry source = a RESERVED error (normative: reserved, not experimental).
- **Ch.11 Concurrency.** Eager scheduling (calling an `async fn` schedules immediately; `await`
  on a non-future is identity); **structured concurrency / cancel-on-drop** (task lifetime bound
  to the `future<T>` handle; discard = cancel; `task.spawn` detaches; `race` cancels losers;
  `timeout` cancels); exit-time drain; generators are consumer-driven bidirectional coroutines
  (`fn*`, `gen.next(v)`, `close`, `for await`); **workers**: the three forms over two lifecycles,
  parallelism-by-isolation, the **serializer airlock sendability rules** as a normative table
  (sendable: the data kinds + frozen `shared` values crossing by Arc; NON-sendable → recoverable
  field-path panic: closures, native handles, generator/actor handles, interface VALUES);
  actor mailbox FIFO + non-reentrancy; streaming backpressure; teardown on close/last-drop;
  the M17 architectural non-goals restated (no durable continuations, no deterministic task
  interleaving — `unspecified` per Ch.1).
- **Ch.12 Capabilities.** Opt-OUT, default-all-granted; the five caps; three subtraction scopes
  (CLI, manifest, irreversible in-code `caps.drop` — no grant); the single chokepoint contract
  (every OS-touching stdlib call is gated by construction; per-handle re-check); pooled-worker
  `caps.drop` refusal; `run_in_worker({caps})` as the sandbox.
- **Ch.13 Gradual typing.** Contracts fire at typed let/param/return + field assignment, eager
  full-depth checks; `any`/unannotated = gradual escape; the **soundness model** as normative:
  a provable `type-mismatch` on a *syntactically annotated* slot is a blocking Error;
  inferred-context findings stay advisory; **Compat3** semantics (only a provable `No` ever
  emits; unsolved type vars are `Unknown`, never `No` — the zero-false-positive guarantee over
  untyped code is a normative property, pinned by the Gate-5 corpus tripwire); generics erasure
  cross-referenced to Ch.7.
- **Ch.14 Stdlib conformance (pointer).** Deliberately one page: the stdlib reference
  (`docs/content/stdlib/*.md`) **is** the normative API documentation; module existence/claiming
  is enforced by DOCS tripwire 3, per-function signatures by SIG's drift-tested table. The
  chapter normatively states only the cross-cutting stdlib RULES (Tier-1 `[value, err]` for
  fallible fns; Tier-2 panic on argument misuse; native fns are ordinary `function` values;
  surplus args ignored by native fns; capability gating per Ch.12). No function-by-function
  duplication (§8 rejected).
- **Ch.15 Conformance suite.** §4 below, as user-facing text.
- **Ch.16 Stability.** §5 below, as user-facing text.

## 4. The conformance suite (formal adoption)

**Declaration (Ch.15, normative):** the AScript conformance suite v1 is, by definition:

1. **The example corpus** — every `.as` under `examples/`, `examples/advanced/`,
   `examples/modules/`, `examples/app/` (74 + 40 + module/app files at drafting time), minus the
   documented `EXAMPLE_SKIPS` (`tests/vm_differential.rs:977` — each skip individually justified
   and itself guarded by `vm_whole_corpus_skips_are_still_justified`).
2. **The golden outputs** — `tests/vm_goldens/*.out` (98 files), the byte-exact expected stdout.
3. **The differential battery** — the named-snippet tests in `tests/vm_differential.rs` (the
   IC/arithmetic/destructuring/short-circuit/edge batteries that don't fit the corpus shape).
4. **The two front-end conformance catalogs** — `tests/treesitter_conformance.rs` (grammar
   accepts the corpus + targeted constructs) and `tests/frontend_conformance.rs` (the
   `both_accept` construct catalog: both the legacy and CST parsers accept every cataloged form).

**THE conformance criterion (D6):** an implementation of AScript **conforms** iff, over the
entire suite, it produces **byte-identical observable behavior** — stdout, exit status, and
panic/diagnostic *messages* (caret columns may differ per the recorded SP1 ±1-column trade) — to
the suite's goldens and to the reference implementation. This is exactly the bar the in-tree
engines already meet continuously: `tree-walker == specialized-VM == generic-VM == .aso-compiled`
in both feature configs (goal.md Gate 1). The tree-walker is the permanent reference oracle; the
suite, not any prose, is the final arbiter — **where this spec's prose and the suite disagree, the
suite is presumed correct and the prose is a bug** (and if the suite itself is wrong, that is an
owner-triaged Gate-14 bug against the implementation).

**Chapter → suite mapping:** Ch.15 carries the full mapping table (the "Pins" column of §3,
expanded to per-claim granularity during drafting). Every chapter's "Conformance" section is the
chapter-local slice of that table; `tests/spec_drift.rs` asserts every cited path exists (§7.2).

**What the suite is NOT:** a feature-coverage promise. A feature with no example is a Gate-9
violation tracked by the campaign process, not a conformance loophole — the suite grows with the
language and the spec version records which suite snapshot it was verified against.

## 5. Stability policy (Ch.16 + `CONTRIBUTING.md` section)

### 5.1 Language version

AScript is **pre-1.0** (current: **0.6**, = the crate version, D5). Pre-1.0, breaking changes to
the STABLE surface are **allowed** with: (a) a minor version bump, (b) migration notes in the
release/roadmap record, and (c) the corpus migrated — never deleted — per goal.md Gate 7. The spec
set carries the language version + a per-chapter "verified against" date; a release that changes
spec'd behavior updates the affected chapter in the same PR (§7.4's checklist item enforces the
grammar half mechanically).

### 5.2 Stability tiers

- **STABLE — the spec'd surface.** Everything chapters 2–13 state normatively, plus the stdlib
  module APIs the reference documents (Ch.14's rules). Breaking a STABLE behavior requires an
  RFC-lite (§6) + the version bump + migration notes pre-1.0; post-1.0 it requires a major
  version.
- **EXPERIMENTAL — explicitly listed, may change without RFC.** Derived honestly from the
  shipped deferral/“v1 deferral” lists (owner-editable; the list below is the drafting-time
  proposal, finalized at chapter review):
  - `http3` (opt-in Cargo feature; upstream-unstable by its own deferral note).
  - The **DAP/debugger surface beyond what shipped** — DBG's documented v1 deferrals (transient
    single-line stepping, conditional breakpoints/logpoints) and the profiler output formats
    (speedscope/collapsed file shapes).
  - **Record/replay as a user-facing feature** — the `src/det.rs` seams are shipped-but-INERT;
    the REPLAY spec owns the user surface; until it lands, any exposed knob is experimental.
  - **Checker lint-code inventory & severities** — the *blocking* soundness behavior (Ch.13) is
    STABLE; the set of advisory lint codes, their names, and `ascript.toml [lint]` keys may
    grow/rename.
  - `std/ai` (tracks fast-moving upstream provider APIs) and `std/telemetry` wire formats.
  - The `ascript doc` output format and LSP capability set (DX-track, still growing per SIG).
  - Implementation-defined subsets called out as such: `std/intl` (ICU subset), `std/tui`
    (crossterm subset), HTTP response trailers (best-effort).
- **INTERNAL — versioned or private, no stability promise.** The `.aso` format (explicitly
  **versioned-but-internal**: `ASO_FORMAT_VERSION` guards it, an `.aso` is valid only for the
  binary version that produced it; rebuild from source across versions), the opcode set and
  bytecode layout, the worker structured-clone wire tags, the shape/IC machinery, the
  `Vm.instrument` seam, internal env vars (`ASCRIPT_NO_SPECIALIZE` is a diagnostic knob), the
  Rust API of the `ascript` crate (**until EMBED lands** — EMBED's contract will carve a STABLE
  embedding surface out of this tier; recorded as the explicit hand-off), and everything under
  `superpowers/` and `bench/`.

The `--tree-walker` engine flag and the four-mode identity are STABLE *as a guarantee* (the
oracle is permanent — owner memory: never delete), while the engines' internals stay INTERNAL.

### 5.3 Deprecation policy (pre-1.0)

No deprecation period is required pre-1.0: a breaking change ships in one minor release WITH the
corpus migration + migration notes (Gate 7's corpus-migration rule is the user-facing migration
guide's first draft — the diff of `examples/**` shows exactly what changes). A `SHOULD`: where a
compatible bridge is cheap (an old name aliased for one release), prefer it; where it is not
(NUM-style semantic breaks), break cleanly and loudly. Post-1.0 (recorded now, activated at 1.0):
deprecate-then-remove across a major version, with a deprecation diagnostic in between.

### 5.4 RFC-lite gate placement

A change is RFC-bearing iff it (a) changes STABLE spec'd behavior, (b) adds language surface
(grammar/AST/value kinds), or (c) promotes/demotes a stability tier. Stdlib additions inside
existing rules, bug fixes toward spec'd behavior, and INTERNAL changes are not RFC-bearing.

### 5.5 The 1.0 criteria checklist (owner-editable draft)

Proposed; the owner edits this list in Ch.16 — it is a living checklist, not a promise:

1. **Spec complete & green** — all 16 chapters published, every chapter verified against the
   implementation, `tests/spec_drift.rs` + (post-DOCS) NAV bijection green in CI.
2. **Stability soak** — **3 consecutive months** with no breaking change to the STABLE surface
   merged (clock resets on any such merge).
3. **Performance campaign closed** — `goal-perf.md` specs merged or explicitly parked; the
   Gate-12 floor (spec/tw geomean ≥2×) holds; headline numbers recorded in `bench/`.
4. **EMBED verdict recorded** — embedding API shipped-stable or explicitly post-1.0.
5. **WASM spike verdict recorded** — GO/NO-GO per its Phase-0 gate.
6. **Registry decision recorded** — REG ships or the bare-version source stays reserved at 1.0.
7. **Fuzzing clean** — the `aso_roundtrip` nightly streak at the BIN bar (≥7 consecutive ≥4 h
   crash-free) and zero differential-fuzzer divergences across the soak window.
8. **EXPERIMENTAL list resolved** — every §5.2 item promoted (spec'd) or explicitly stamped
   post-1.0.
9. **Zero recorded carry-forward bugs** (e.g. the `recover` anonymous-fn defect, if the owner
   chose to defer it at Ch.9 triage, must be closed by 1.0).
10. **Docs at parity** — DOCS + SIG drift suites green; README/landing repositioned per Gate 13.
11. **Process exercised** — at least one RFC-lite has run end-to-end (proposal → verdict →
    spec update).
12. **Conformance suite frozen** — the 1.0 suite snapshot tagged; four-mode identity green on it
    in both feature configs.

## 6. RFC-lite process (deliberately light)

The existing campaign cadence (**spec → independent review → lock → plan → implement → review →
merge**) IS the change-management process; RFC-lite formalizes only its FRONT DOOR for
language-surface changes. Three artifacts:

1. **`superpowers/rfcs/0000-template.md`** — a ONE-PAGE template: Title/Date/Champion; Problem
   (≤3 paragraphs); Proposal (surface sketch + one example); Impact checklist (grammar? both
   parsers? `.aso`? stdlib? breaking? which spec chapters?); Alternatives considered; Verdict
   block (owner fills: Accepted / Rejected / Deferred + date + rationale).
2. **`superpowers/rfcs/README.md`** — the process in one screen: open an RFC as a numbered file
   (`NNNN-slug.md`) via PR → owner review (the verdict block) → **on Accept, the RFC graduates
   into a full design spec** (`superpowers/specs/`) and the normal cadence takes over → on
   merge, the implementing PR updates the affected `docs/content/spec/` chapters (the staleness
   checklist + drift tests enforce the grammar half) → the RFC is the permanent decision record.
   Rejected/Deferred RFCs stay in the directory (the "Removed/parked" goal-perf.md section's
   role, now with a home).
3. **`CONTRIBUTING.md` § "Language changes & stability"** — points contributors at the RFC dir,
   states the §5 tiers + version rule, and reiterates: an RFC is required only per §5.4; bug
   fixes toward spec'd behavior never need one.

## 7. Drift guardrails (the only code)

One new dependency-free integration test file **`tests/spec_drift.rs`** (std-only string
scanning, repo-rooted via `env!("CARGO_MANIFEST_DIR")` — the `tests/docs_drift.rs` /
`srv_negative_space.rs` idiom; runs under BOTH feature configs). Each check's logic is a pure
helper exercised by a **deliberate-mutation self-test** (the DOCS anti-false-green rule: feed a
synthetically broken input and assert the helper reports the violation).

### 7.1 Grammar-rule coverage test

`grammar_rules_are_covered_by_spec()`: extract every named rule from
`tree-sitter-ascript/grammar.js` — the keys of the `rules:` object, matched as 4-space-indented
`name:` lines (105 rules at drafting time, including `_`-prefixed hidden rules) — and assert each
rule name appears **verbatim** in `docs/content/spec/grammar.md` (the chapter's `covers:
ts(rule_a, rule_b, …)` annotation lines are the intended anchors; a plain backticked mention also
satisfies). Failure message names the missing rule and says "a new grammar rule needs a
spec/grammar.md production (and a semantics chapter update if behavior changed)". **Mechanical
and honest:** this proves the EBNF *mentions* every rule, not that it *generates the same
language* (D2's stated limitation; equivalence is the two-parser conformance suite's job). The
extraction helper is pure (`fn grammar_rule_names(grammar_js: &str) -> Vec<String>`) and the
mutation self-test feeds a grammar source with one extra synthetic rule + a spec text without it.

### 7.2 Chapter manifest + citation-existence test

`spec_chapters_exist_and_cite_real_pins()`: a checked-in manifest of the 16 chapter slugs (§3's
table, transcribed as a `const`) — each `docs/content/spec/<slug>.md` must exist, be non-trivial
(> a floor of bytes), and contain a `## Conformance` section; every repo-relative path cited in
that section (recognized as `examples/…` / `tests/…` backticked tokens) must exist on disk. This
makes stale citations (a renamed example, a deleted test) a CI failure, which is what keeps the
chapter→suite mapping (§4) honest over time. Pure helper + mutation self-test (a chapter citing a
nonexistent path must be reported).

### 7.3 NAV reachability — owned by DOCS, not duplicated (D3)

DOCS tripwire 4 (NAV ⇄ `docs/content` bijection, `tests/docs_drift.rs`) covers the new
`spec/*` pages automatically in whichever merge order: if DOCS is in-tree first, LSPEC's pages
fail its tripwire until NAV is updated (LSPEC's plan updates NAV in the same task that creates
pages); if LSPEC lands first, the manual Gate-13 NAV check governs and DOCS's bijection picks the
pages up at its merge. `tests/spec_drift.rs` contains no NAV assertions.

### 7.4 The CLAUDE.md checklist item (social + mechanical)

`CLAUDE.md` "Touching syntax — the cross-cutting checklist" gains one bullet:

> **The spec is normative — update it.** Any grammar/AST/semantics change updates the matching
> `docs/content/spec/` chapter: a `grammar.js` change MUST update `spec/grammar.md`'s EBNF
> (`tests/spec_drift.rs` fails on an unmentioned rule), and a behavior change MUST update the
> owning semantics chapter + its `## Conformance` pins. The spec set is versioned with the
> language (`docs/content/spec/stability.md`); spec staleness is a campaign-blocking defect
> (Gate 13).

Grammar changes are enforced mechanically (7.1); pure-semantics changes are enforced socially by
this checklist + reviewer duty (same enforcement class as the rest of that checklist).

## 8. Scope & rejected alternatives

**In scope:** the 16 chapters + NAV section; the conformance-suite adoption chapter; the
stability chapter + `CONTRIBUTING.md` section; `superpowers/rfcs/` (template + README);
`tests/spec_drift.rs` (the only code); the `CLAUDE.md` checklist bullet; `goal-perf.md` /
`roadmap.md` bookkeeping; per-chapter verification against the implementation with Gate-14
triage of any contradiction found.

**Rejected (each recorded so it isn't re-litigated):**
- **A mechanized / executable specification** (Redex/K-framework-style). The differential oracle
  already plays that role — the tree-walker IS the executable semantics, continuously proven
  equal to the production engine over the whole corpus in both configs. A second mechanization
  would be a third implementation to keep in sync, with no consumer.
- **Full operational-semantics notation** (inference rules / small-step formalism) for v1.
  Precise prose + pinned tests deliver the verification value at a fraction of the cost;
  formalism is recorded as an **aspiration** in Ch.1 (a future spec may formalize chapters 4–5
  first, where the payoff is highest).
- **Spec'ing the stdlib function-by-function.** The docs reference (existence/claiming: DOCS)
  + SIG's drift-tested signature table own that surface; duplicating ~57 modules into the spec
  would create a third copy that drifts. Ch.14 is a pointer + the cross-cutting rules only.
- **A generated EBNF** (D2) and **a NAV assertion in spec_drift.rs** (D3) — rejected above.
- **A separate language-version counter** (D5) — rejected above.
- **A heavyweight RFC process** (multi-stage, shepherds, comment periods) — the project has one
  owner and an established review cadence; RFC-lite formalizes the front door, nothing more.

## 9. Gates & the verification model

- **Gate 13 (docs):** every new page is in `NAV` (same task), in-content links are relative per
  the house rule, and the served site renders (`cd docs && python3 -m http.server` spot-check;
  plan carries it).
- **Gate 14 (contradiction triage):** each chapter task runs its cited examples/tests
  (`target/release/ascript run <example>` and the named `cargo test` invocations) BEFORE the
  chapter is accepted. A mismatch between drafted normative text and observed behavior is
  triaged: implementation wrong → fix in-branch with failing-test-first regression (or
  owner-noted filing with a failing test if too large); spec text wrong → correct the text and
  record the delta. The known `recover`-anonymous-fn carry-forward (Ch.9) enters this triage
  explicitly.
- **Gates 2–3:** `tests/spec_drift.rs` compiles + passes under both feature configs; clippy
  clean both configs (the test is std-only, no new deps).
- **Gate 1/5/12 untouched by construction:** no engine, checker, or grammar code changes;
  `vm_differential` and the Gate-5 corpus tripwire are run once at the end as the
  no-code-surface proof.
- **Red-branch discipline:** `tests/spec_drift.rs` is written FIRST and is allowed to be red
  ONLY on its own assertions until the chapters land (the DOCS plan's rule, reused).

## 10. Grounding + contradictions found during drafting

**Grounding (verified 2026-06-12):** `goal-perf.md:321` (the LSPEC mandate);
`tree-sitter-ascript/grammar.js` (105 named rules; PREC ladder `:20-38`; GLR conflicts `:51-80`;
"NOT reserved here" header note); `docs/assets/app.js` `NAV:11-62` (40 slugs, data-driven);
`tests/vm_differential.rs` (`EXAMPLE_SKIPS:977`, whole-corpus gate `:1185`, skip-justification
guard `:1258`); `tests/vm_goldens/` (98 goldens); `tests/treesitter_conformance.rs` +
`tests/frontend_conformance.rs` (`both_accept` catalog); `src/vm/aso.rs:167`
(`ASO_FORMAT_VERSION = 27`); `Cargo.toml` `version = "0.6.0"`; DOCS spec §5.4 (NAV bijection
tripwire ownership) + §3 (SIG boundary); `CONTRIBUTING.md` (process-doc home, spec-authority
line `:115`); CLAUDE.md (the cross-cutting checklist; the `recover` carry-forward bug note; the
SP1 ±1-column trade).

**Contradiction/staleness findings surfaced while drafting (to be resolved by the chapters, all
doc-side so far — none change code):**
1. The 2026-05-29 design doc's §1/§8.2 "no tagged-union enums / no payloads" text is superseded
   by ADT but still reads as normative; §3's precedence sketch lacks the ternary/bitor/range
   tiers; §10.1's grammar path (`grammar/tree-sitter-ascript/`) is wrong (repo-root
   `tree-sitter-ascript/`); §15 lists shipped features as open questions. The normative spec
   supersedes these; the design doc gains a one-line header pointer ("normative spec:
   `docs/content/spec/` — this document is the historical design record").
2. CLAUDE.md cites `ASO_FORMAT_VERSION` as 18/25/26 in different (era-accurate) sections while
   the constant is 27 — consistent with its own "read the current constant, never hardcode"
   rule, but Ch.16/INTERNAL text must cite the constant by name, not number.
3. The `recover(fn(){…})` anonymous-fn-expression defect (recorded carry-forward) contradicts
   the design doc's `recover(fn)` contract — Gate-14 triage scheduled in the plan (Task 2.9).
