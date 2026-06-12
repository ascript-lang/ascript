# Language Specification + Stability Policy (LSPEC) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. At the end of each
> phase, a **holistic per-phase review subagent** reviews the phase's combined changes before
> the next phase starts. A task/phase is closed only when every box under it is ticked.

**Goal:** Publish the normative AScript specification — 16 chapters under `docs/content/spec/`
served by the docs site — with every chapter's claims VERIFIED against the implementation as
written (cited examples/tests actually run, contradictions Gate-14-triaged); formally adopt
`examples/**` + goldens + the differential battery + the two front-end catalogs as the
conformance suite with four-mode byte-identity as THE conformance criterion; ship the stability
policy (tiers, versioning, deprecation, owner-editable 1.0 checklist), the RFC-lite process
(`superpowers/rfcs/`), and the drift guardrails (`tests/spec_drift.rs`, written FIRST, observed
red; the CLAUDE.md spec-staleness checklist bullet).

**Spec:** `superpowers/specs/2026-06-12-language-spec-stability-design.md` (LSPEC). Read it
first; § references below are into it. The spec is self-contained.

**Architecture:** Documentation-and-governance work; the ONLY code is one new dependency-free
integration test file `tests/spec_drift.rs` (std-only string scanning, repo-rooted via
`env!("CARGO_MANIFEST_DIR")`, the `tests/docs_drift.rs`/`srv_negative_space.rs` idiom; must
compile and pass under BOTH feature configs). Two check groups: (7.1) grammar-rule coverage —
every named `grammar.js` rule appears verbatim in `docs/content/spec/grammar.md`; (7.2) chapter
manifest + citation existence — all 16 chapter files exist, each has a `## Conformance` section,
and every `examples/…`/`tests/…` path cited there exists on disk. Each check is a pure helper
exercised by a deliberate-mutation self-test (anti-false-green). The spec is NORMATIVE; the
`docs/content/language/` guide stays tutorial — chapters cross-link, never duplicate authority.

**Key decisions (locked in the spec, §2):** D1 spec lives at `docs/content/spec/` + a new
"Specification" NAV section; D2 hand-written EBNF + mechanical rule-name drift test (a generator
is rejected; the test proves rule-name COVERAGE, not language equivalence — equivalence is the
two-parser conformance suite's job, and the chapter says so); D3 NAV reachability is OWNED by
DOCS tripwire 4 — `tests/spec_drift.rs` contains NO NAV logic; D4 RFCs live in
`superpowers/rfcs/`; D5 language version = crate version (0.6 today); D6 four-mode identity over
the adopted suite is THE conformance criterion.

**Red-branch discipline (explicit):** Task 1.1 commits `tests/spec_drift.rs` deliberately RED
(the spec pages don't exist yet). From then until the end of Phase 3 the branch may be red ONLY
on `tests/spec_drift.rs` — everything else (full suite, clippy, both configs) stays green at
every commit. Record each observed-red output verbatim in the task log. The merge gate (Phase 4)
requires everything green.

**Gate-14 triage protocol (binding for every chapter task):** each chapter task RUNS its cited
examples/tests before acceptance. Drafted-normative-text vs observed-behavior mismatch is a BUG:
the implementer STOPS, records the reproduction, and escalates to the owner — owner decides
whether the spec text or the implementation is wrong. Implementation-wrong → fix in-branch with
a failing-test-first regression guard (or, if genuinely too large, file with an owner note AND a
failing test — never silently spec'd around). Spec-text-wrong → correct the text, record the
delta in the task log. Doc-side staleness in OTHER documents (the 2026-05-29 design doc, guide
pages) is fixed where cheap (one-liners) or recorded in the Task 4.2 sweep.

**Binding execution standards (production-grade mandate):** any bug found while working — ours
or pre-existing, direct or incidental — is fixed in-branch with a failing-test-first regression
guard, never stepped around (goal.md Gate 14). No placeholders, no silent deferrals, no stub
chapters. Commit per task with the house trailer:
`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

## File structure

**New files:**
- `tests/spec_drift.rs` — the two drift-check groups + pure helpers + mutation self-tests +
  the checked-in 16-slug chapter manifest.
- `docs/content/spec/{intro,lexical,grammar,values,expressions,statements,classes,patterns,
  errors,modules,concurrency,capabilities,types,stdlib,conformance,stability}.md` — the 16
  chapters.
- `superpowers/rfcs/README.md` — the RFC-lite process (one screen).
- `superpowers/rfcs/0000-template.md` — the one-page RFC template.

**Modified files:**
- `docs/assets/app.js` — the `NAV` array gains a "Specification" section (16 entries).
- `CLAUDE.md` — the "Touching syntax" checklist gains the spec-staleness bullet (§7.4); the
  docs section mentions `docs/content/spec/` as normative.
- `CONTRIBUTING.md` — new "Language changes & stability" section (tiers, version rule, RFC
  pointer); the spec-authority line updated to name `docs/content/spec/` as normative and the
  2026-05-29 doc as the historical design record.
- `superpowers/specs/2026-05-29-ascript-design.md` — ONE header line added pointing at the
  normative spec (the doc itself is preserved as the historical record).
- `docs/content/language/*.md` — only where a chapter task finds guide-vs-implementation
  staleness cheap to fix (recorded per task).
- `README.md` — one "Specification" link line.
- `goal-perf.md` (LSPEC status → ✅ at merge), `superpowers/roadmap.md` (the LSPEC record) —
  Phase 4.

---

## Phase 0 — Preflight: grounding re-verification + baseline

### Task 0.1: re-verify the spec's grounding and baseline the tree

**Files:** none (verification only; findings recorded in the task log).

- [ ] **Step 1 — citation re-grep** (record ACTUAL values; use them throughout):
  `grep -c -E '^\s{4}[a-z_0-9]+:' tree-sitter-ascript/grammar.js` (expect ~105 — the rule
  count); `grep -n 'const NAV' docs/assets/app.js` (~`:11`);
  `grep -n 'ASO_FORMAT_VERSION' src/vm/aso.rs | head -1` (expect 27 — cite by NAME in
  chapters); `grep -n '^version' Cargo.toml` (expect 0.6.0 — the declared language version);
  `grep -n 'EXAMPLE_SKIPS' tests/vm_differential.rs | head -3`;
  `ls tests/vm_goldens | wc -l` (~98); `ls examples/*.as examples/advanced/*.as | wc -l`;
  `grep -n 'recover(fn(){' CLAUDE.md` (the carry-forward bug note — confirm still recorded).
- [ ] **Step 2 — confirm the negative space:** `ls docs/content/spec` errors (no dir yet);
  `ls superpowers/rfcs` errors; `grep -rn 'spec_drift' tests/` empty. Confirm whether
  `tests/docs_drift.rs` exists yet (DOCS merge state) — record it; if it EXISTS, Task 3.2's
  NAV update must keep its tripwire 4 green (run it); if not, D3's manual-check path applies.
- [ ] **Step 3 — baseline runs:** `cargo test` green; `cargo test --no-default-features` green;
  `cargo clippy --all-targets` + `cargo clippy --no-default-features --all-targets` clean;
  `cargo build --release` (the chapter tasks run `target/release/ascript`). Record counts.
- [ ] **Step 4:** create the feature branch `feat/language-spec` off `main`.

### Task 0.2: Phase 0 review

- [ ] **Step 1:** Independent reviewer re-runs Steps 1–3, confirms recorded values match the
  tree and the DOCS-merge-state determination is correct. Mismatches corrected in the task log
  before Phase 1.

---

## Phase 1 — Drift guardrails first (TDD: written, observed RED)

### Task 1.1: `tests/spec_drift.rs` — both check groups + mutation self-tests

**Files:** new `tests/spec_drift.rs`.

- [ ] **Step 1 — pure helpers (std-only, no new deps):**
  ```rust
  /// Extract every named rule from grammar.js: lines matching exactly
  /// four spaces + an identifier + ':' inside the file (the rules-object
  /// entries; includes `_`-prefixed hidden rules).
  fn grammar_rule_names(grammar_js: &str) -> Vec<String> { /* line scan:
      starts_with("    "), 5th char is [a-z_], take chars [a-z0-9_]+ until ':',
      reject lines whose name is empty or that don't end the ident at ':' */ }

  /// Every rule name must appear verbatim in the spec grammar chapter
  /// (anchored by `covers: ts(...)` lines or any backticked mention).
  fn uncovered_rules(rules: &[String], grammar_md: &str) -> Vec<String>

  /// Chapter manifest checks: file exists, len >= MIN_CHAPTER_BYTES (e.g. 1500),
  /// contains "## Conformance"; returns violations with reasons.
  fn chapter_violations(spec_dir: &Path, manifest: &[&str]) -> Vec<String>

  /// Scan a chapter's "## Conformance" section for backticked repo-relative
  /// `examples/...` / `tests/...` tokens; return those that don't exist on disk.
  fn dead_citations(chapter_md: &str, repo_root: &Path) -> Vec<String>
  ```
  Plus `const SPEC_CHAPTERS: [&str; 16] = ["intro","lexical","grammar","values",
  "expressions","statements","classes","patterns","errors","modules","concurrency",
  "capabilities","types","stdlib","conformance","stability"];`
- [ ] **Step 2 — the four `#[test]`s:** `grammar_rules_are_covered_by_spec` (reads
  `tree-sitter-ascript/grammar.js` + `docs/content/spec/grammar.md`; sanity-asserts
  `rules.len() >= 100` so a broken extractor can't false-green; failure message: the missing
  rule names + "a new grammar rule needs a spec/grammar.md production — and a semantics chapter
  update if behavior changed (CLAUDE.md 'Touching syntax')");
  `spec_chapters_exist_with_conformance_sections`; `spec_citations_resolve`;
  and `spec_drift_helpers_catch_mutations` — the self-test: (a) a synthetic grammar source
  with one extra rule vs a spec text lacking it → `uncovered_rules` reports exactly it;
  (b) a synthetic chapter citing `examples/does_not_exist.as` → `dead_citations` reports it;
  (c) a manifest naming a missing chapter → `chapter_violations` reports it. The self-test
  must pass even while the real tests are red.
- [ ] **Step 3 — observe RED honestly:** `cargo test --test spec_drift` → the three real tests
  FAIL (no `docs/content/spec/` yet), the mutation self-test PASSES. Record the verbatim
  failure output in the task log. `cargo clippy --all-targets` stays clean;
  `cargo test --no-default-features --test spec_drift` shows the same red (proves
  feature-config independence).
- [ ] **Step 4:** commit (red allowed on `spec_drift` only, per the discipline above).

### Task 1.2: Phase 1 review

- [ ] **Step 1:** Independent reviewer runs the suite, confirms: red is confined to
  `spec_drift`'s three real tests; the mutation self-test genuinely exercises all three helpers
  (reviewer hand-breaks one helper — e.g. makes `uncovered_rules` return empty — and confirms
  the self-test catches it, then restores); the rule extractor's output, printed via the test's
  failure message or a temporary `--nocapture` dump, matches a manual
  `grep -oE '^\s{4}[a-z_0-9]+:' tree-sitter-ascript/grammar.js` (same count, same names).

---

## Phase 2 — The chapters (each task: outline + drafted normatives + verification)

Every chapter task follows the same protocol: write the chapter from the outline below (the
headings and normative statements in the task are the chapter's spine — flesh out, do not
deviate without recording why), then RUN the verification commands, then add the
`## Conformance` section citing exactly the files the verification ran. Gate-14 triage applies
to every mismatch. Style: normative MUST/SHOULD per Ch.1, present tense, one concept per
section, code blocks runnable, cross-links relative (`](grammar)`, `](../language/syntax)`).

> Chapter ordering note: `spec/intro` (Task 2.1) is written first because every other chapter
> uses its terms; `spec/grammar` (Task 2.3) before the semantics chapters so they can cite its
> productions; `spec/conformance` + `spec/stability` close the set (Tasks 2.15, 3.1).

### Task 2.1: `spec/intro.md` — Notation, terms & conformance

- [ ] **Step 1 — write the chapter.** Headings: `# AScript Specification — Notation & Conformance`
  · `## Status & versioning` (language version = crate version, currently 0.6; per-chapter
  "verified against" dates; the spec is NORMATIVE, the language guide is tutorial; where they
  disagree the spec wins and the disagreement is a bug) · `## Requirement words`
  (MUST/MUST NOT/SHOULD/MAY, RFC-2119-style) · `## Behavior categories` — draft these
  normatives verbatim:
  - "**Implementation-defined** behavior is chosen by the implementation and documented (e.g.
    the `std/intl` locale-data subset, best-effort HTTP response trailers)."
  - "**Unspecified** behavior may be any of an identified set, with no documentation duty
    (e.g. the interleaving of concurrently scheduled tasks, OS scheduling of worker isolates)."
  - "AScript has **no undefined behavior**: every erroneous condition is a Tier-1 error value,
    a Tier-2 panic, or a clean compile/verification error. Silent wraparound, truncation, or
    coercion is a conformance bug, never latitude."
  · `## Conformance` (forward-pointer to `spec/conformance`: the four-mode criterion; the
  documented engine asymmetry — bytecode-capacity limits are VM-only) · `## Aspirations`
  (formal operational semantics recorded as a future possibility, not v1 — the differential
  oracle is the executable semantics today).
- [ ] **Step 2 — verify:** `cargo test --test vm_limits` (the documented asymmetry is real);
  confirm `grep -n 'version' Cargo.toml | head -1` still says 0.6.x and the chapter matches.
- [ ] **Step 3 — Conformance section** cites `tests/vm_differential.rs`, `tests/vm_limits.rs`.
- [ ] **Step 4:** independent review (reviewer reads the chapter against the spec §3 inventory,
  re-runs Step 2); commit.

### Task 2.2: `spec/lexical.md` — Lexical structure

- [ ] **Step 1 — write.** Headings: `## Source text` (UTF-8) · `## Comments` · `## Statement
  separation` (ASI-lite: newline-terminated; `;` optional in statement lists AND class bodies;
  never substitutes for `,`; formatter canonicalizes) · `## Identifiers` ·
  `## Keywords` — the honest three-way split, drafted:
  - reserved: the `Tok` keyword set verified against `src/lexer.rs` (MUST list it exactly —
    includes `interface`; enumerate by reading the lexer, not the 2026-05-29 doc).
  - contextual: `step`, `as` (rename), `implements`, `extends` (in interface position), `from`
    (import), `static`, `worker` — usable as identifiers elsewhere.
  - soft-in-grammar: names the tree-sitter header treats as plain identifiers (`self`, `super`,
    primitive type names) with the note that semantic restrictions are enforced later.
  · `## Literals` (int forms `0x/0b/0o/_`; the float discriminator — a `.` or exponent;
  `0m` decimal; strings/escapes; template strings — nested literals and nested templates inside
  `${…}` are valid) · `## Operators & punctuation` (token inventory incl. `+% -% *%`,
  `..`/`..=`, `...`, `?.`, `??`, `#{`).
- [ ] **Step 2 — verify:** run `target/release/ascript run examples/strings.as`,
  `examples/numbers.as`, `examples/integers.as` (outputs match goldens); spot-check the keyword
  claims: `echo 'let step = 5\nprint(step)' > /tmp/kw.as && target/release/ascript run /tmp/kw.as`
  (contextual `step` binds) and `echo 'let interface = 1' > /tmp/kw2.as && target/release/ascript
  run /tmp/kw2.as` (reserved → error). `cargo test --test frontend_conformance` green.
- [ ] **Step 3 — Conformance** cites `tests/frontend_conformance.rs`, `examples/strings.as`,
  `examples/numbers.as`, `examples/integers.as`.
- [ ] **Step 4:** independent review (reviewer probes one more keyword from each bucket);
  commit.

### Task 2.3: `spec/grammar.md` — the normative EBNF (the big one)

- [ ] **Step 1 — write the EBNF.** Organization mirrors `grammar.js` top-down; ISO-style EBNF
  notation declared at the top (`=`, `|`, `[opt]`, `{rep}`, terminals quoted). EVERY production
  block is followed by an anchor line of the form `covers: ts(source_file, _item)` naming the
  tree-sitter rules it formalizes — the union of all `covers:` lines MUST equal the full rule
  inventory (the Task 1.1 test enforces it; 105 rules at plan time, re-counted in Task 0.1).
  Sections: `## Notation` · `## Program & items` · `## Declarations` (let/const/fn/class/enum/
  interface incl. type-parameters/bounds, field declarations, implements) · `## Statements` ·
  `## Expressions` (the precedence ladder transcribed from `PREC`, lowest→highest, as a table:
  assign 1 · ternary 2 · `??` 3 · `||` 4 · `&&` 5 · equality 6 · comparison+`instanceof` 7 ·
  bitor `| ^` 8 · range 9 · add `+ - +% -%` 10 · mul `* / % *% << >> &` 11 · `**` right-assoc
  12 · postfix `?`/`!` precedence-LESS (GLR) · unary 14 · postfix-chain 15) ·
  `## Patterns` (match patterns, or-patterns, guards) · `## Types` (the type grammar incl.
  `T?`, unions, `future<T>`, tuples, fn types, generic applications) · `## Ambiguities resolved
  by GLR` — draft the three prose rules beside their productions:
  - "`expr ?` is a ternary condition iff a `:` follows at bracket-depth 0 before the statement
    ends; otherwise it is Result-propagation. `a ? -b : c` is ternary; `f()? - 1` is
    propagate-then-subtract."
  - "`?` and `!` postfix forms are deliberately precedence-less and bind LOOSER than `await`
    and prefix `!`/`-`: `await x!` parses as `(await x)!`."
  - "`(x)` is held ambiguous between a parameter list and a parenthesized expression until
    `=>` (or its absence) decides."
  · `## Limitation (read this)` — D2's honesty paragraph: "the drift test proves every
  tree-sitter rule is COVERED by a production here; it does not prove the two grammars generate
  the same language. Language equivalence is pinned empirically by the conformance suite: both
  parsers (and the legacy front-end) accept the entire corpus
  (`tests/treesitter_conformance.rs`, `tests/frontend_conformance.rs`)."
- [ ] **Step 2 — make the drift test GREEN:** `cargo test --test spec_drift
  grammar_rules_are_covered_by_spec` passes (first green of the red branch). If any rule is
  awkward to place (e.g. helper rules like `template_chars`), it still gets a `covers:` mention
  in the section that owns it — no rule is silently dropped.
- [ ] **Step 3 — verify semantics-bearing claims:** `cargo test --test treesitter_conformance`
  + `--test frontend_conformance` green; run the ternary/propagate differential battery:
  `cargo test --test vm_differential ternary` and `cargo test --test vm_differential propagate`
  (the named tests at `tests/vm_differential.rs:645-677`).
- [ ] **Step 4 — Conformance** cites `tests/treesitter_conformance.rs`,
  `tests/frontend_conformance.rs`, `tests/spec_drift.rs`, `tests/vm_differential.rs`.
- [ ] **Step 5:** independent review — reviewer picks 8 random grammar.js rules across
  declarations/expressions/patterns/types and checks each `covers:` claim is honest (the EBNF
  near it actually formalizes that construct, not just name-drops it); reviewer adds a
  synthetic rule to a scratch copy of grammar.js and confirms the test would fail. Commit.

### Task 2.4: `spec/values.md` — Values & types

- [ ] **Step 1 — write.** Headings: `## Value kinds` (the authoritative table READ FROM
  `src/value.rs` at drafting time — name, runtime `type(x)` string, mutability, sharing;
  includes future/generator/interface/native/shared) · `## Numbers` (the NUM model — draft:
  "integer literals are `int` (i64); a `.` or exponent makes a `float` (f64); `number` is the
  annotation union `int | float`; `int/int` truncates toward zero (`7/2 == 3`, `-7/2 == -3`);
  any float operand promotes; `+ - * **` and unary `-` MUST trap on i64 overflow with a
  recoverable Tier-2 panic; `+% -% *%` wrap; bitwise/shift are int-only; a float always prints
  with a fractional digit (`5.0`), an int never does (`5`)") · `## Truthiness` (draft: "the
  falsy set is exactly `nil`, `false`, `0`, `0.0`/`-0.0`/`NaN`, `0m`, `\"\"`; **all containers
  are truthy, including empty ones**") · `## Equality & identity` (draft: "`==` is structural
  for nil/bool/int/float/decimal/string and EXACT across numeric subtypes (`1 == 1.0` is
  `true`, no lossy promotion past 2^53); `==` is **pointer identity** for array/object/map/set/
  bytes/function/future/generator/instance (two structurally equal arrays are NOT `==`);
  constructed enum-variant payloads compare structurally; there is no cross-kind coercion
  (`1 == \"1\"` is `false`)") · `## Map keys` (MapKey canonicalization: −0.0→+0.0, NaN unified,
  integral in-range float folds to the equal int key) · `## Reference vs value semantics` ·
  `## Frozen values` (`object.freeze` shallow in-place; `shared.freeze` deep immutable
  Send-able — forward-link to concurrency).
- [ ] **Step 2 — verify:** run `examples/core_types.as`, `examples/numbers.as`,
  `examples/integers.as`, `examples/num_int_float_edges.as`, `examples/numeric_tower.as`,
  `examples/map_literals.as`, `examples/frozen.as` (outputs match goldens); probe identity:
  `printf 'print([1] == [1])\nprint(1 == 1.0)\nprint(len([]))\nif ([]) { print("truthy") }' >
  /tmp/eq.as && target/release/ascript run /tmp/eq.as` → expect `false true 0 truthy`.
  Cross-check the kind table against `grep -n 'pub fn type_name' src/value.rs` output.
- [ ] **Step 3 — Conformance** cites the run examples + `tests/vm_differential.rs`
  (equality battery `vm_equality_matches_treewalker`).
- [ ] **Step 4:** independent review (reviewer probes 2^53 boundary + `-0.0` map-key fold in
  the REPL); commit.

### Task 2.5: `spec/expressions.md` — Expressions & operators

- [ ] **Step 1 — write.** Headings: `## Evaluation order` (left-to-right; short-circuit `&&`/
  `||`/`??`; `?.` receiver-nil skips args) · `## The precedence table` (reference the grammar
  chapter's table; semantics per tier) · `## Arithmetic` (cross-link values; div/mod by zero
  panics; shift-amount rules) · `## Comparison & instanceof` (draft: "`x instanceof RHS`
  requires a class OR interface on the right — class → nominal chain walk, interface →
  structural conformance (name+arity), reserved type names `int|float|number|string|bool` →
  runtime type guard; any other RHS is a Tier-2 panic; a non-instance LHS yields `false`,
  never panics") · `## Result propagation ?` and `## Force-unwrap !` (draft: "`expr!` evaluates
  a `[value, err]` pair to `value`, or raises a recoverable panic carrying the original error
  message; `?`/`!` occupy one tier between `**` and unary, so they bind looser than `await`") ·
  `## Ternary` (the `:`-at-depth-0 rule restated semantically) · `## Ranges` (draft: "`a..b`
  exclusive, `a..=b` inclusive; direction follows the bounds (`10..1` counts down); `step k`
  is signed; step 0 / non-finite / direction-mismatch is a Tier-2 panic identical in for-range,
  value position (materializes `array<number>`), and match patterns (strided membership)") ·
  `## Spread` (strict: wrong-container spread panics; object spread later-value-wins,
  first-seen position) · `## Safe access` (`?.`, `??`) · `## await` (identity on non-futures —
  cross-link concurrency) · `## Template strings`.
- [ ] **Step 2 — verify:** run `examples/all_features.as`, `examples/ranges.as`,
  `examples/range_step_default.as`, `examples/instanceof.as`, `examples/force_unwrap.as`,
  `examples/spread.as` (match goldens); `cargo test --test vm_differential short_circuit`;
  probe the unwrap-tier claim: `printf 'async fn f() { return [7, nil] }\nasync fn m() {
  print(await f()!) }\nawait m()' > /tmp/uw.as && target/release/ascript run /tmp/uw.as` → `7`.
- [ ] **Step 3 — Conformance** cites the run examples + the `vm_differential` ternary/
  propagate/short-circuit tests.
- [ ] **Step 4:** independent review (reviewer probes `f()? - 1` and `a ? -b : c` on both
  engines via `--tree-walker`); commit.

### Task 2.6: `spec/statements.md` — Statements & declarations

- [ ] **Step 1 — write.** Headings: `## Bindings` (let/const; draft: "redeclaration in one
  scope and assignment to an immutable binding are RUNTIME errors raised when the declaration/
  assignment executes — dead code never errors, and the RHS evaluates first";
  loop variables and fn/class/enum/import bindings are immutable) · `## Destructuring`
  (array positional + trailing rest; object by-key with `as` rename, quoted keys, missing key
  binds `nil`, `...rest` collects leftovers preserving insertion order; non-matching container
  → Tier-2 panic, no coercion) · `## Module-scope globals & late binding` (draft: "a direct
  top-level declaration is a module-scope global; a function body or field default MAY
  reference a binding declared later in the module — resolution happens at use time") ·
  `## Control flow` (if/while/for-of/for-in-range/for-await/break/continue/return) ·
  `## Functions` (declarations vs arrows; default params — call-time, left-to-right, earlier
  params in scope, explicit `nil` suppresses, required-after-defaulted is an error; rest param
  last, `array<T>`-typed rest element-checked) · `## Statement separators` (cross-link
  lexical).
- [ ] **Step 2 — verify:** run `examples/functions.as`, `examples/default_params.as`,
  `examples/rest.as`, `examples/object_destructuring.as` (match goldens); probe runtime-timed
  redeclaration: `printf 'fn f() { let x = 1\n let x = 2 }\nprint("ok")' > /tmp/rd.as &&
  target/release/ascript run /tmp/rd.as` → prints `ok` (uncalled), then a variant calling `f()`
  → the redeclaration error; both engines (`--tree-walker`) agree.
- [ ] **Step 3 — Conformance** cites the run examples + `tests/vm_differential.rs`
  (destructure batteries).
- [ ] **Step 4:** independent review; commit.

### Task 2.7: `spec/classes.md` — Classes, enums, interfaces & generics

- [ ] **Step 1 — write.** Headings: `## Classes` (callable class = construction, `init`,
  `self`, single inheritance, `super`, method resolution; `async fn init`/`fn* init` are
  compile errors; `static fn from` reserved) · `## Statics` (separate namespace, inherited,
  no `super`) · `## Typed fields & records` (field schemas checked on assignment incl. inside
  `init`; auto-derived positional init for init-less classes, defaults → optional trailing
  params) · `## ClassName.from` (validate_into: recursive, applies defaults, field-path panic,
  does NOT run init; powers `json.parse(text, Class)`) · `## Enums` (unit variants — interned,
  `.value`/`.name`; payload variants — positional XOR named, first-class constructors,
  arity+field-type validation at call, structural payload `==`, named-field direct reads) ·
  `## Interfaces` (draft: "an interface is a named method-set descriptor; `v instanceof I` is
  a STRUCTURAL check — v1 conformance = method name + arity compatibility, and only class
  instances can conform; `extends` composes lazily with a cycle guard; `implements` on a class
  is checker documentation and never affects runtime conformance; an interface-typed annotation
  is a runtime contract via the same conformance predicate; an interface VALUE is not sendable
  across workers") · `## Generics & erasure` (draft: "type parameters on fn/class/enum/
  interface are checked statically and ERASED at runtime: a `T`-typed slot performs no runtime
  check, generic instantiation creates no distinct runtime type, and bytecode carries no type
  arguments — `Box<int>` and `Box<string>` are one runtime class").
- [ ] **Step 2 — verify:** run `examples/oop.as`, `examples/records.as`,
  `examples/static_methods.as`, `examples/typed_fields.as`, `examples/enums_adt.as`,
  `examples/enums_negative_backing.as`, `examples/interfaces.as`, `examples/generics.as`,
  `examples/advanced/interface_dispatch.as`, `examples/typed_parse.as` (match goldens);
  probe erasure: `printf 'class Box<T> { v: T }\nlet b = Box(1)\nb.v = "s"\nprint(b.v)' >
  /tmp/er.as && target/release/ascript run /tmp/er.as` → runs (T erased; record observed
  output verbatim — if it panics, the erasure claim is wrong → Gate-14 triage).
- [ ] **Step 3 — Conformance** cites the run examples.
- [ ] **Step 4:** independent review (reviewer probes interface arity-mismatch non-conformance
  + `implements`-lying class still conforming structurally); commit.

### Task 2.8: `spec/patterns.md` — Pattern matching & exhaustiveness

- [ ] **Step 1 — write.** Headings: `## match` (expression form, first-arm-wins, or-patterns,
  guards) · `## Pattern forms` (wildcard/ident/value/range(+step)/array(+rest)/object(+rest)/
  variant positional+named) · `## The binding rule` (draft Option C verbatim: "a bare
  identifier pattern that names an EXISTING in-scope binding is a comparison (`==`); an
  identifier with no in-scope binding BINDS the subject; object-shorthand `{key}` always
  binds. Unit enum variants used unqualified therefore shadow-bind — write them QUALIFIED
  (`Shape.Point`) in exhaustiveness-relevant matches; the checker warns
  (`enum-variant-binding-shadow`)") · `## Exhaustiveness` (draft: "exhaustiveness over an
  enum-typed subject is checked STATICALLY (`non-exhaustive-match`, default severity Error);
  on a subject the checker cannot prove, the check is gradually silent; at runtime a subject
  matching no arm is a Tier-2 panic (`MatchNoArm` backstop) on every engine") ·
  `## Range & strided patterns`.
- [ ] **Step 2 — verify:** run `examples/pattern_matching.as`, `examples/match_or_patterns.as`,
  `examples/enums_adt.as`, `examples/advanced/state_machine.as` (match goldens); probe
  exhaustiveness: write a two-variant enum match missing one arm to `/tmp/ex.as`, run
  `target/release/ascript check /tmp/ex.as` → expect `non-exhaustive-match` Error; run the
  no-arm runtime backstop on both engines.
- [ ] **Step 3 — Conformance** cites the run examples + `tests/check.rs`.
- [ ] **Step 4:** independent review (reviewer probes Option C both directions); commit.

### Task 2.9: `spec/errors.md` — the two-tier model (+ the recover triage)

- [ ] **Step 1 — write.** Headings: `## No exceptions` · `## Tier 1 — errors are values`
  (`[value, err]`, `Ok`/`Err`, error objects ≥ `{message}`, `error` ≡ `object | nil`,
  `Result<T>` ≡ `[T, error]`) · `## ? propagation` (early-returns `[nil, err]`; using `?`
  where the enclosing fn cannot return a pair is a compile-time error) · `## ! force-unwrap`
  (recoverable panic, original message) · `## Tier 2 — panics` (the enumerated panic sources;
  draft: "a panic unwinds to the host, prints a source-pointed diagnostic + stack trace, and
  exits non-zero; panics are not catchable in normal code") · `## Recursion limits` (draft:
  "call depth and expression-nesting depth are capped; exceeding either raises the recoverable
  panic `maximum recursion depth exceeded` — a clean panic, never a process abort, identical
  on every engine") · `## recover` (the single host boundary: runs a zero-arg fn, converts a
  panic to `[nil, err]`; for REPL/tests/embedding, not control flow).
- [ ] **Step 2 — Gate-14 triage (the recorded carry-forward):** reproduce
  `recover(fn(){ assert(false, "boom") })` (anonymous fn-EXPRESSION arg) on both engines —
  CLAUDE.md records it failing with "function declaration has no resolver binding" while the
  arrow form works. **STOP and present to the owner:** (a) fix in-branch (failing-test-first;
  likely a resolver binding for fn-expressions in call-arg position) — preferred if tractable,
  or (b) spec the limitation explicitly ("v0.6 restriction: `recover` takes an arrow or a
  named function; anonymous `fn` expressions are rejected — tracked defect") + keep the
  CLAUDE.md owner note + add the failing test marked `#[ignore]` with the owner note. Either
  way the chapter text matches observed behavior and a test pins it.
- [ ] **Step 3 — verify:** run `examples/result.as`, `examples/force_unwrap.as`,
  `examples/deep_recursion.as`, `examples/advanced/typed_errors.as` (match goldens); probe
  the recursion panic exit code is non-134 (`target/release/ascript run
  examples/deep_recursion.as; echo $?`).
- [ ] **Step 4 — Conformance** cites the run examples.
- [ ] **Step 5:** independent review (reviewer re-runs the triage reproduction and confirms
  the chapter text matches reality); commit.

### Task 2.10: `spec/modules.md` — Modules, imports & packages

- [ ] **Step 1 — write.** Headings: `## Modules` (one file = one module; named + namespace
  imports; NO default exports; evaluate-once + cache; circular imports resolve to the
  partially-initialized module, use-before-init is a load error) · `## Specifier
  classification` (draft the four-way split: "`std/…` → built-in module; `./`/`../` →
  relative file; bare specifier → package (first segment = package, rest = subpath);
  an unresolvable bare specifier is the clean error `unknown package '<k>' — add it with
  'ascript add'`") · `## Package resolution` (manifest dependency shapes git/url/path;
  MVS version selection; content-addressed store keyed by `asum1`; `ascript.lock` +
  `--locked` is offline and re-hashed, fail-closed; **the bare-version registry source is
  RESERVED** — a clean error today, a future additive source kind) · `## Capability note`
  (imports of `std/*` are not cap-gated; calls are — cross-link capabilities).
- [ ] **Step 2 — verify:** `cargo test --test modules` + `cargo test --test pkg` green; run an
  `examples/modules/`-importing example; probe the unknown-package error text:
  `printf 'import { x } from "nosuchpkg"' > /tmp/up.as && target/release/ascript run
  /tmp/up.as` and match the chapter's quoted message verbatim.
- [ ] **Step 3 — Conformance** cites `tests/modules.rs`, `tests/pkg.rs`,
  `examples/modules/`.
- [ ] **Step 4:** independent review; commit.

### Task 2.11: `spec/concurrency.md` — async, generators, workers (the longest chapter)

- [ ] **Step 1 — write.** Headings: `## Tasks & eager scheduling` (draft: "calling an
  `async fn` returns a `future<T>` and the body is scheduled IMMEDIATELY; `await` drives a
  future to completion; `await` on a non-future is the identity") · `## Structured
  concurrency — cancel-on-drop` (draft: "a task's lifetime is bound to its `future<T>`
  handle: dropping the last handle cancels the task; a discarded un-awaited call therefore
  does not run to completion; `task.spawn` is the explicit detach; `race` cancels losers;
  `timeout` cancels timed-out work; at program exit the runtime drains all still-owned
  tasks") · `## std/task combinators` (spawn/gather/race/timeout/retry contracts) ·
  `## Generators` (consumer-driven, lazily polled, bidirectional `gen.next(v)`, `close()`,
  `for await`; `async fn*` may await between yields) · `## Workers — parallelism by
  isolation` (the three forms over two lifecycles: pooled `worker fn` → `future<T>` per call;
  `worker class` actors — `spawn()` returns `future<handle>`, FIFO one-message-at-a-time
  mailbox, non-reentrant, async-only methods, no cross-boundary field access; `worker fn*`
  streams — demand-driven pull, bounded buffer backpressure, bidirectional; teardown on
  `close()`/last-drop) · `## The sendability rules (the airlock)` — draft the normative
  table: "values cross worker boundaries by structured deep copy; SENDABLE: nil/bool/int/
  float/decimal/string/bytes/array/object/map/set/regex/enum variants/class instances (with
  shipped class code)/frozen `shared` values (which cross by reference, not copy);
  NON-SENDABLE (recoverable field-path panic at the boundary): closures/functions, native
  resource handles, futures, generator handles, actor handles, interface values" ·
  `## Determinism` (task interleaving and worker scheduling are UNSPECIFIED per intro;
  the M17 architectural non-goals restated) · `## Frozen shared values`
  (`shared.freeze`: deep, acyclic — an on-stack cycle is rejected; diamonds preserved;
  reads behave as the underlying kind; mutation panics `cannot mutate a frozen <kind>`).
- [ ] **Step 2 — verify:** run `examples/async.as`, `examples/concurrency.as`,
  `examples/structured_concurrency.as`, `examples/generators.as`,
  `examples/workers_parallel_map.as`, `examples/workers_errors.as`,
  `examples/shared_config.as`, `examples/advanced/workers_actor_counter.as`,
  `examples/advanced/workers_stream_bidirectional.as` (match goldens);
  `cargo test --test m17_structured_concurrency` + `--test m17_generator_regressions` green;
  probe a non-sendable boundary panic (closure into a `worker fn`) and match the chapter's
  field-path-panic claim.
- [ ] **Step 3 — Conformance** cites the run examples + the two m17 test files +
  `tests/workers_stateful.rs`.
- [ ] **Step 4:** independent review (reviewer probes cancel-on-drop: an un-awaited async call
  with a side effect that must NOT appear); commit.

### Task 2.12: `spec/capabilities.md`

- [ ] **Step 1 — write.** Headings: `## The model` (draft: "capabilities are OPT-OUT: every
  capability is granted by default, so a program that never subtracts runs identically to a
  capability-unaware runtime; there is no grant operation") · `## The five capabilities`
  (fs/net/process/ffi/env — what each governs, incl. by-construction coverage: DNS under net,
  stdin under fs-era io gating as implemented, OS topology under env/process per
  `required_cap`) · `## Subtraction scopes` (CLI `--deny`/`--sandbox`/`--deny-net`/
  `--deny-fs`; manifest `[capabilities]`; in-code `caps.drop` — IRREVERSIBLE) ·
  `## Enforcement points` (the single stdlib chokepoint + the per-open-handle re-check —
  normative: "an already-open handle does not outlive a drop") · `## Workers` (draft:
  "`run_in_worker(fn, input, {caps:{deny}})` runs on a DEDICATED isolate with the reduced set
  — a memory-isolated sandbox; `caps.drop` inside a POOLED `worker fn` is REFUSED") ·
  `## Denial behavior` (the panic shape on a denied call).
- [ ] **Step 2 — verify:** `cargo test --test cap_audit` green (19 denial paths); run
  `examples/caps_sandbox.as` (matches golden); probe one CLI denial:
  `target/release/ascript run --deny-fs examples/system.as` → records the denial diagnostic
  quoted in the chapter.
- [ ] **Step 3 — Conformance** cites `tests/cap_audit.rs`, `examples/caps_sandbox.as`.
- [ ] **Step 4:** independent review (reviewer probes the open-handle re-check claim); commit.

### Task 2.13: `spec/types.md` — gradual typing & the soundness model

- [ ] **Step 1 — write.** Headings: `## Contracts (runtime)` (fire at typed let/const, param
  entry, typed return, field assignment; eager full-declared-depth checks; `any`/unannotated
  = gradual escape; failed contract = Tier-2 panic) · `## The type grammar` (cross-link
  grammar chapter: primitives, `number`, `T?` ≡ `T | nil`, unions, `array<T>`/`map<K,V>`,
  tuples, `Result<T>`, `future<T>`, fn types, class/enum/interface names, type parameters) ·
  `## The static checker` (advisory by default; NEVER runs code) · `## The soundness model`
  — draft verbatim: "a provable `type-mismatch` on a SYNTACTICALLY ANNOTATED slot (typed
  let/return/param/field-default) is a blocking Error; `possibly-nil`, `type-error`, and
  mismatches in inferred context are advisory Warnings; `ascript.toml [lint]` may downgrade
  the block. The checker emits only on a provable `No` — an unsolved or unbounded type
  variable is `Unknown`, never `No` — so fully untyped programs receive ZERO type
  diagnostics (a false positive on untyped code is a conformance bug, pinned by the corpus
  gate)" · `## Generic inference` (argument-driven; interface bounds via structural
  conformance; invariance of parameterized class/enum applications) · `## Erasure`
  (cross-link classes chapter) · `## Lint stability` (the lint-code inventory is
  EXPERIMENTAL per the stability chapter; the blocking model above is STABLE).
- [ ] **Step 2 — verify:** run `examples/typed.as`, `examples/typed_fields.as`,
  `examples/optional_types.as`, `examples/typed_config.as` (match goldens);
  `cargo test --test check corpus` green in BOTH configs (the zero-FP pin); probe the block:
  a `/tmp/bad.as` with `let x: int = "s"` → `target/release/ascript check /tmp/bad.as`
  exits non-zero with `type-mismatch`; the same file with no annotation → zero diagnostics.
- [ ] **Step 3 — Conformance** cites the run examples + `tests/check.rs`.
- [ ] **Step 4:** independent review; commit.

### Task 2.14: `spec/stdlib.md` — the conformance pointer (one page)

- [ ] **Step 1 — write.** Headings: `## The stdlib reference is normative` (draft: "the
  per-module reference under `docs/content/stdlib/` IS the normative API documentation for
  the standard library; this specification does not duplicate it. Module existence/claiming
  is mechanically enforced (DOCS drift tripwires); per-function signatures by the SIG
  signature table when it lands — until then the reference prose governs") · `## Cross-cutting
  rules (normative here)` — draft: "fallible stdlib functions return Tier-1 `[value, err]`;
  argument-type misuse is a Tier-2 panic; native functions are ordinary `function` values and
  ignore surplus positional arguments; OS-touching functions are capability-gated per the
  capabilities chapter; async stdlib functions return `future`s riding the event loop" ·
  `## Feature flags` (a module absent from a build is an unknown-import error, not silent) ·
  `## Always-global core` (print/len/type/assert/range/Ok/Err/recover + the truthiness/len
  contract).
- [ ] **Step 2 — verify:** run `examples/stdlib_completeness.as` (matches golden); spot-check
  surplus-arg tolerance and a wrong-type Tier-2 panic on one native fn
  (`math.abs("x")`).
- [ ] **Step 3 — Conformance** cites `examples/stdlib_completeness.as`, `examples/stdlib.as`.
- [ ] **Step 4:** independent review; commit.

### Task 2.15: `spec/conformance.md` — the suite, formally adopted

- [ ] **Step 1 — write.** Headings: `## The conformance suite v1` (the four-part definition
  from spec §4: corpus minus documented `EXAMPLE_SKIPS` (each skip itself test-guarded) +
  `tests/vm_goldens/` + the `vm_differential` battery + the two front-end catalogs) ·
  `## The criterion` — draft verbatim: "an implementation of AScript CONFORMS iff, over the
  entire suite, it produces byte-identical observable behavior — stdout, exit status, and
  panic/diagnostic messages (caret columns may differ by the recorded ±1 column) — to the
  suite's goldens and the reference implementation. The in-tree engines meet this bar
  continuously: tree-walker == specialized VM == generic VM == `.aso`-compiled, in both
  feature configurations. Where this specification's prose and the suite disagree, the suite
  is presumed correct and the prose is a defect" · `## Chapter → suite map` (the full table:
  each chapter's pins, expanded from the per-chapter Conformance sections — kept honest by
  `tests/spec_drift.rs` citation checking) · `## What the suite is not` (not a
  feature-coverage promise; it grows with the language; the spec version records the suite
  snapshot verified against) · `## Running it` (the exact commands:
  `cargo test --test vm_differential` both configs, `--test treesitter_conformance`,
  `--test frontend_conformance`).
- [ ] **Step 2 — verify:** `cargo test --test vm_differential` AND
  `cargo test --no-default-features --test vm_differential` green (record counts);
  `cargo test --test treesitter_conformance --test frontend_conformance` green. Cross-check
  the chapter's `EXAMPLE_SKIPS` description against the live list.
- [ ] **Step 3 — Conformance** cites all four suite components.
- [ ] **Step 4:** independent review; commit.

### Task 2.16: Phase 2 holistic review

- [ ] **Step 1:** Holistic reviewer reads all 15 chapters end-to-end for: contradiction with
  each other, duplicated authority with `docs/content/language/` (the guide may overlap in
  topic, never in normative voice), requirement-word discipline (MUST/SHOULD used per Ch.1),
  and unverified claims (every normative statement traceable to a Step-2 verification or a
  cited pin). Reviewer re-runs `cargo test --test spec_drift` — the chapter-manifest test
  must now fail ONLY on the missing `stability.md` (15/16 present) — record it.
- [ ] **Step 2:** Reviewer picks 3 chapters at random and re-executes their Step-2
  verification commands verbatim. Any mismatch reopens the chapter task.
- [ ] **Step 3:** Gate-14 ledger check: every triage event in Phase 2 (incl. Task 2.9's
  `recover` decision) has its resolution recorded (fix + regression test in-branch, or
  owner-noted filing + failing/ignored test). No silent spec-arounds.

---

## Phase 3 — Governance: stability chapter, process docs, NAV, checklist

### Task 3.1: `spec/stability.md` + `CONTRIBUTING.md` + `superpowers/rfcs/`

- [ ] **Step 1 — write `spec/stability.md`.** Headings: `## Language version` (D5: version =
  crate version, 0.6 now; pre-1.0 a STABLE-surface breaking change requires a minor bump +
  migration notes + corpus migration) · `## Stability tiers` — the spec §5.2 lists transcribed:
  STABLE (chapters 2–13 + the stdlib reference surface), EXPERIMENTAL (the honest list:
  `http3`; DAP stepping/conditional-breakpoints + profiler file formats; record/replay user
  surface; the advisory lint-code inventory + `[lint]` keys; `std/ai` + telemetry wire
  formats; `ascript doc` output + LSP capability set; the implementation-defined subsets
  noted as such), INTERNAL (`.aso` — versioned-but-internal, valid only for the producing
  binary version, cite `ASO_FORMAT_VERSION` by NAME; opcodes/bytecode; worker wire tags;
  shape/IC machinery; `ASCRIPT_NO_SPECIALIZE` and diagnostic knobs; the Rust crate API until
  EMBED lands — explicit hand-off note; `superpowers/` + `bench/`) — each tier list marked
  **owner-editable** · `## Deprecation policy` (pre-1.0 per spec §5.3; the post-1.0 rule
  recorded now, activated at 1.0) · `## The road to 1.0` — the 12-item owner-editable
  checklist from spec §5.5, as literal checkboxes · `## Changing the language` (RFC-lite
  pointer; §5.4's is-it-RFC-bearing rule).
- [ ] **Step 2 — `superpowers/rfcs/0000-template.md`:** the one-page template (Title/Date/
  Champion; Problem ≤3 paragraphs; Proposal + one example; Impact checklist: grammar? both
  parsers + regen? `.aso`? stdlib? breaking? which spec chapters?; Alternatives; Verdict
  block: Accepted/Rejected/Deferred + date + rationale). `superpowers/rfcs/README.md`: the
  process in one screen (numbered file via PR → owner verdict → on Accept graduates to a
  `superpowers/specs/` design + the normal cadence → implementing PR updates the affected
  spec chapters → the RFC is the permanent record; Rejected/Deferred stay filed).
- [ ] **Step 3 — `CONTRIBUTING.md`:** add "Language changes & stability" (tiers summary +
  version rule + RFC pointer + "bug fixes toward spec'd behavior never need an RFC"); update
  the Conventions line `CONTRIBUTING.md:115` — the normative spec is `docs/content/spec/`;
  `superpowers/specs/2026-05-29-ascript-design.md` is the historical design record.
- [ ] **Step 4 — the design-doc pointer:** add ONE line under the 2026-05-29 doc's header:
  "> **Normative spec:** `docs/content/spec/` (LSPEC, 2026-06-12). This document is the
  historical design record; where they differ, the spec governs." No other edits to it.
- [ ] **Step 5 — verify:** `cargo test --test spec_drift` — ALL tests now GREEN (16/16
  chapters, citations resolve, grammar covered). Record the first all-green output. Full
  `cargo test` + `--no-default-features` green; clippy clean both configs.
- [ ] **Step 6:** independent review (reviewer audits the EXPERIMENTAL list against the
  shipped deferral notes — nothing invented, nothing missing from the recorded deferrals;
  reviewer confirms the 1.0 checklist matches spec §5.5); commit.

### Task 3.2: NAV + README + CLAUDE.md checklist

- [ ] **Step 1 — NAV:** add a "Specification" section to `docs/assets/app.js` `NAV` (after
  "Language", before "Standard library") with the 16 entries in chapter order
  (`['spec/intro', 'Notation & conformance']` … `['spec/stability', 'Stability & 1.0']`).
- [ ] **Step 2 — served-site sanity (Gate 13):** `cd docs && python3 -m http.server` +
  load the reader; click through ≥4 spec pages (intro, grammar, concurrency, stability);
  verify sidebar + cmd-K search find them and in-content relative links resolve
  (`](grammar)`, `](../language/syntax)`). Record the check.
- [ ] **Step 3 — DOCS coordination:** if `tests/docs_drift.rs` exists (per Task 0.1 Step 2),
  run it — the NAV bijection tripwire must be green with the new pages. If not, record "D3
  manual path: NAV verified by hand; DOCS tripwire will cover at its merge".
- [ ] **Step 4 — CLAUDE.md:** append the spec-staleness bullet to "Touching syntax — the
  cross-cutting checklist" (the §7.4 text verbatim); update the docs guidance paragraph to
  name `docs/content/spec/` as the normative set beside the tutorial guide. **README.md:**
  one line in the docs links: "**Specification** — `docs/content/spec/` (normative;
  stability policy + 1.0 criteria in `spec/stability`)".
- [ ] **Step 5:** independent review (reviewer greps NAV vs the 16 files both directions by
  hand; re-renders the site); commit.

### Task 3.3: Phase 3 holistic review

- [ ] **Step 1:** Holistic reviewer verifies the whole governance loop closes: a hypothetical
  grammar change (add a scratch rule to a COPY of grammar.js) → `spec_drift` red with an
  actionable message naming the CLAUDE.md checklist; a hypothetical chapter-citation rot
  (rename an example in a scratch copy) → caught. Confirms `superpowers/rfcs/` README +
  template are self-consistent with `spec/stability.md` and CONTRIBUTING (one process, three
  views, no contradiction).

---

## Phase 4 — Final gates, bookkeeping, merge

### Task 4.1: full-gate run

- [ ] **Step 1:** `cargo test` green; `cargo test --no-default-features` green;
  `cargo clippy --all-targets` + `cargo clippy --no-default-features --all-targets` clean.
- [ ] **Step 2:** `cargo test --test vm_differential` both configs green and
  `cargo test --test check corpus` both configs green — the no-code-surface proof (LSPEC
  changed no engine/checker code; counts match Task 0.1's baseline or the in-branch
  regression fixes explain the delta).
- [ ] **Step 3:** `cargo test --test spec_drift` green both configs; the mutation self-tests
  pass.
- [ ] **Step 4:** fmt-idempotence untouched-areas spot check: `cargo run -- fmt` on any
  example modified by a Gate-14 fix (if none were, record N/A).

### Task 4.2: bookkeeping + contradiction ledger

- [ ] **Step 1 — `goal-perf.md`:** flip the LSPEC entry (line ~321) to ✅ with a two-line
  result summary (chapters published, suite adopted, tiers + RFC process live, drift tests
  green).
- [ ] **Step 2 — `superpowers/roadmap.md`:** append the LSPEC record: deliverables, the D1–D6
  decisions, the Gate-14 contradiction ledger (every triage event + resolution, incl. the
  `recover` outcome and the design-doc staleness items from spec §10), and the standing rule
  "touching grammar = touching `spec/grammar.md` (mechanically enforced)".
- [ ] **Step 3 — memory hygiene:** if the `recover` defect was FIXED in-branch, update the
  CLAUDE.md carry-forward note (remove it); if deferred with owner note, leave it and
  cross-reference the spec chapter's recorded limitation.

### Task 4.3: final holistic review + merge

- [ ] **Step 1:** Independent holistic reviewer: re-runs Task 4.1 entirely; reads the diff
  end-to-end; spot-verifies 3 normative claims per the chapter protocol (run the cited
  pins); confirms the red-branch discipline held (git log: no commit between Task 1.1 and
  Task 3.1 Step 5 red outside `spec_drift`); confirms no placeholder text (`grep -rn
  "TODO\|TBD\|XXX" docs/content/spec/` empty).
- [ ] **Step 2:** Merge `feat/language-spec` to `main` with `--no-ff`; confirm CI green.
