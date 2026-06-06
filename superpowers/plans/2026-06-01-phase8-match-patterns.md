# Phase 8 — match Pattern Extensions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** Real pattern matching on `match` — Option C binding (defined→compare, undefined→bind), array/object/range patterns, guards, `[value,err]` idiom. Core-grammar phase. Full spec: `docs/superpowers/specs/2026-06-01-phase8-match-patterns-design.md`.

**Architecture:** New `Pattern` AST enum; hand-parser emits patterns (Option-C resolution deferred to runtime); interp matcher resolves Ident (compare-if-defined, bind-if-new), recurses arrays/objects, checks ranges, runs guards. fmt renders patterns; tree-sitter accepts them (reuses existing array_pattern/object_pattern); LSP registers bindings. NON-BREAKING (existing match tests pass). Touches ast/parser/interp/fmt + grammar.js+parser.c + lsp.

**Conventions:** When touching `ExprKind`/AST, the exhaustive matches in interp.rs (eval), fmt.rs, ast.rs (Display) each need updating (compiler-enforced). Regen `parser.c` with `tree-sitter generate --abi 14` after grammar changes. No `RefCell` borrow across `.await`. clippy clean both configs; RUN both test configs; conformance (treesitter+frontend) must pass; docs+README+example.

Sub-phases: 8a AST+parser+matcher → 8b tree-sitter+conformance → 8c fmt+LSP → 8d integration. 8a is the large core.

---

## Sub-phase 8a: Pattern AST + hand-parser + interp matcher

**Files:** `src/ast.rs` (Pattern enum + MatchArm change + Display), `src/parser.rs` (pattern parsing + guard), `src/interp.rs` (matcher + arm eval), plus any exhaustive-match sites the compiler flags. Tests inline in interp.rs.

- [ ] **Step 1 — failing tests** (interp.rs `#[tokio::test]` via run_source; cover the NEW behavior AND regression):
  - REGRESSION: existing `match n { 0 => "z", 1|2 => "s", _ => "m" }` still works; `match c { Color.Red => true, _ => false }`; `match_with_variable_and_expression_patterns` (a defined `let k=2; match n { k => ... }` compares) still passes.
  - Option C bind: `match getPair() { x => x }` where `x` is undefined → binds (returns the value). A defined name still compares.
  - `[value,err]` idiom: `let [u,e] = [{name:"a"}, nil]; match [u, e] { [user, nil] => user.name, [nil, err] => "e" }` → "a". And the error branch with a non-nil err → the err branch.
  - array rest: `match [1,2,3] { [first, ...rest] => first + len(rest) }` → 1 + 2 = 3.
  - object: `match {method:"GET", path:"/x"} { {method: "GET", path} => path } ` → "/x" (path binds; method compares literal "GET"); object shorthand `match {a:1,b:2} { {a, b} => a + b }` → 3 (a,b bind).
  - range: `match 5 { 1..=9 => "digit", _ => "big" }` → "digit"; `match 12 { 1..=9 => "d", _ => "big" }` → "big"; exclusive `0..10`.
  - guard: `match n { _ if n < 0 => "neg", 0 => "zero", _ => "pos" }`; guard with binding `match [v,e] { [x, nil] if x > 10 => "big", [x, nil] => "small", _ => "err" }`.
  - const-compare (Option C footgun avoidance): `const target = 5; match 5 { target => "matched", _ => "no" }` → "matched" (compares, NOT binds).
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:**
  - `ast.rs`: add `Pattern` enum (Wildcard, Ident(Rc<str>), Value(Box<Expr>), Range{start:Box<Expr>,end:Box<Expr>,inclusive:bool}, Array(Vec<Pattern>, Option<Option<Rc<str>>>), Object(Vec<ObjPatEntry>, Option<Option<Rc<str>>>)) where ObjPatEntry distinguishes shorthand-bind from `key: subpat`. Change `MatchArm` to `{ patterns: Vec<Pattern>, guard: Option<Expr>, body: ... }`. Add `Display` for Pattern; update MatchArm/Match Display.
  - `parser.rs`: in the match-arm parser, parse `|`-separated patterns then optional `if <expr>` guard then `=> body`. `parse_pattern`: `_`→Wildcard; `[`→Array (sub-patterns + `...name`/`...` rest); `{`→Object (entries + rest); else parse an expression and classify: Range binop→Range, lone Ident→Ident, else→Value.
  - `interp.rs`: matcher `async fn match_pattern(&self, pat, subject, bindings: &mut Vec<(Rc<str>,Value)>, env) -> Result<bool, Control>`. Wildcard→true. Ident→ if `env.get(name).is_some()` compare subject==that value, else push binding (true). Value→eval expr in env, compare ==. Range→eval start/end, subject is Number in range. Array→subject is array; arity (exact, or >= with rest); recurse each element; rest binds remainder. Object→subject is Object/Instance with keys; shorthand entry binds field; `key:subpat` recurses; rest binds remaining keys. Match eval: per arm, per alternative pattern, fresh bindings; on match, child env with bindings, eval guard (truthy?), then body; else next. No match → existing recoverable panic.
  - Fix all compiler-flagged exhaustive AST/ExprKind matches (interp eval, fmt, ast Display) for the MatchArm/Pattern change.
- [ ] **Step 4 — verify:** `cargo test` AND `cargo test --no-default-features` (RUN both), `cargo clippy --all-targets`, `cargo clippy --no-default-features --all-targets`. Green, 0 warnings. ALL existing match tests pass.
- [ ] **Step 5 — commit:** `feat(match): pattern matching - binding(Option C)/array/object/range/guard`

> CRITICAL: this is non-breaking — existing match arms (literals, enum refs, defined-variable compares, `|` alternatives, `_`) MUST behave identically. The Option-C Ident resolution is the linchpin: `env.get(name).is_some()` → compare, else bind. Run the FULL existing test suite, not just new tests.

---

## Sub-phase 8b: tree-sitter grammar + regen + conformance

**Files:** `docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js`, regenerated `.../src/parser.c`, conformance tests.

- [ ] **Step 1 — failing check:** add a Phase-8 pattern snippet to the conformance corpus (or rely on the Step-3 example); run `cargo test --test treesitter_conformance` + `cargo test --test frontend_conformance` — they must accept array/object/range/guard match patterns. Initially the tree-sitter grammar will REJECT the new syntax (match arms only allow value expressions) → conformance fails.
- [ ] **Step 2 — verify fail** (tree-sitter rejects the new pattern syntax).
- [ ] **Step 3 — implement:** extend the `match_expression` arm rule in grammar.js to accept patterns: reuse `$.array_pattern`/`$.object_pattern` (already defined for destructuring), add a range pattern + bare identifier + value expression + `_` + `|` alternatives + optional `'if' $._expression` guard. Resolve GLR conflicts via declared `conflicts` (do NOT add `prec` that breaks the ternary/`?`/range — per CLAUDE.md). Regenerate: `tree-sitter generate --abi 14` in the grammar dir to rebuild `parser.c` (the `cc`-compiled vendored parser). 
- [ ] **Step 4 — verify:** `cargo test --test treesitter_conformance` + `cargo test --test frontend_conformance` pass (both parsers accept all examples incl. the new patterns); `cargo test` still green; clippy clean.
- [ ] **Step 5 — commit:** `feat(grammar): tree-sitter match patterns (array/object/range/guard) + regen parser.c`

---

## Sub-phase 8c: fmt + LSP

**Files:** `src/fmt.rs` (pattern rendering), `src/lsp/analysis.rs` (pattern bindings as locals), tests.

- [ ] **Step 1 — failing tests:** fmt idempotence — a `.as` file with array/object/range/guard match patterns, formatted twice, is unchanged AND still parses+runs. (Add to the fmt test suite; the existing fmt tests show the pattern.) LSP — a match arm binding (`[x, nil] => use(x)`) does NOT flag `x` as undefined in the arm body (add/extend an lsp test).
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:** `fmt.rs` `write_match_arm` (and a `write_pattern`): render Wildcard `_`, Ident, Value (via existing expr writer), Range `a..=b`/`a..b`, Array `[p, ...rest]`, Object `{k, k2: p, ...rest}`, `|` alternatives, and `if guard`. Match the formatter's spacing conventions; ensure round-trip. `lsp/analysis.rs`: when analyzing a match arm, collect the pattern's bound names (Ident-that-binds, array/object/rest bindings) and treat them as defined in the guard + body scope.
- [ ] **Step 4 — verify:** `cargo test` (incl fmt + lsp tests) both configs; `cargo run -- fmt <example>` idempotent; clippy clean.
- [ ] **Step 5 — commit:** `feat(fmt,lsp): format match patterns + recognize pattern bindings`

---

## Sub-phase 8d: integration

- [ ] `examples/pattern_matching.as`: demonstrate `[value,err]` matching (the headline), guards, ranges, array rest, object patterns, AND the Option-C const-compare-vs-bind distinction (a `const` name compares; a new name binds). Assertions; prints success; terminates.
- [ ] Run it; verify it parses under BOTH parsers (treesitter+frontend conformance) and `cargo run -- fmt` is idempotent on it.
- [ ] Docs: `docs/content/language/classes-enums.md` (the "Match" section) — document the new pattern forms, Option-C binding (defined→compare/undefined→bind), object shorthand binds, ranges, guards, `[value,err]`. `docs/content/language/syntax.md` if it lists match grammar. README if it showcases match.
- [ ] Update CLAUDE.md's match note (the `?`/ternary + ExprKind arms note already exists; add that MatchArm now holds `Vec<Pattern>` + guard and the matcher's Option-C rule).
- [ ] FULL gates: both `cargo test` configs, both clippy `--all-targets`, `fmt --check`, the example, both conformance tests, `cargo run -- lsp`-adjacent lsp tests.
- [ ] Holistic review (focus: NO regression to existing match/destructuring/ranges; Option-C resolution correct incl. const-compare; matcher borrow-safe; tree-sitter+hand-parser agree (frontend_conformance); fmt idempotent; LSP bindings; no TODOs). Merge `--no-ff`.

## Self-review notes
- Riskiest: 8a parser pattern-vs-expression classification (lone Ident vs member-access/call/range) + the matcher's Option-C resolution + keeping ALL existing match behavior. And 8b GLR conflicts in tree-sitter (don't break ternary/`?`/range).
- This is the only phase touching the grammar — frontend_conformance (differential hand-parser vs tree-sitter) is the key guardrail; both must accept identical inputs.
- Non-breaking is paramount: run the entire suite after 8a, not just new tests.
