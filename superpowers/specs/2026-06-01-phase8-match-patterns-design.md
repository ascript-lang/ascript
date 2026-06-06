# Phase 8 — `match` Pattern Extensions Design (Option C: resolve-the-name)

- **Date:** 2026-06-01
- **Status:** Design — owner chose Option C (resolve-the-name) binding semantics.
- **Roadmap:** Phase 8 of `2026-05-31-batteries-completeness-roadmap.md`.
- **Owner:** Mahmoud Kayyali

## Goal

Extend the existing value-only `match` into real pattern matching: **binding**, **array/object
destructuring**, **range patterns**, and **guards** — with the `[value, err]` idiom as the
headline. This is the only **core-grammar** phase (parser + interp + fmt + tree-sitter + LSP).

## Binding semantics — Option C (resolve-the-name)

A **bare identifier** in a pattern is resolved **at match time** against the enclosing scope:
- **Defined name** (a `const`/`let`/param in scope) → **value pattern**: compare `subject == that
  value` (switch-like; identical to today's behavior).
- **Undefined name** (new) → **binding**: capture the subject value under that name in the arm's
  scope.

**Why this is non-breaking:** today a bare-identifier pattern is evaluated as an expression and
`==`-compared, which *requires* the name to be defined (an undefined name is an
"undefined variable" runtime error). Option C keeps "defined → compare" unchanged, and only
repurposes the previously-erroring "undefined name" into a useful binding. The existing
`match_with_variable_and_expression_patterns` test (a defined variable) still compares. **No
regression.**

**The const footgun is avoided:** `const target = 5; match n { target => ... }` still
**compares** `n == 5` (target is defined), exactly as a reader expects — unlike pure Rust-style
"always bind" which would silently shadow `target`.

Resolution is a **runtime decision in the matcher** (`env.get(name).is_some()`), so the parser
stays simple: it emits a generic `Ident` pattern node and the interpreter decides compare-vs-bind.

## Pattern forms

In match-arm position, a pattern is one of:
- **Wildcard** `_` — matches anything, binds nothing.
- **Ident** `name` — Option-C resolved (defined→compare, undefined→bind).
- **Value** `<expr>` — any expression that is NOT a lone identifier / range / `[`/`{` form (e.g.
  `0`, `"x"`, `true`, `nil`, `Color.Red`, `1 + 1`, `someFn()`): evaluated and compared by `==`
  (existing behavior).
- **Range** `a..b` (exclusive) / `a..=b` (inclusive) — `a`,`b` are expressions; matches a Number
  in range. (Reuses the existing `..`/`..=` range lexing/parsing; classified as a range *pattern*
  in pattern position.)
- **Array** `[p0, p1, ..., (...rest)?]` — subject must be an array; fixed-arity match unless a
  trailing `...rest` (binds the remainder array) or `...` (ignore remainder) is present; each
  element recursively matched against its sub-pattern (sub-patterns follow ALL these rules,
  including Option-C idents). Enables `[u, nil]` / `[nil, e]` / `[first, ...rest]`.
- **Object** `{key, key2: subpat, ..., (...rest)?}` — subject must be an Object or Instance with
  the named keys; `{key}` **shorthand always BINDS** `key` to that field (sugar; to match a field
  against a value use the explicit `{key: <pattern>}` or a guard — documented exception to
  Option C, since shorthand intent is capture); `{key: subpat}` matches the field against `subpat`
  (Option-C applies to subpat); `...rest` binds remaining keys as an Object.
- **Alternatives** `p1 | p2 | ...` — matches if any alternative matches (existing `|` support;
  bindings from alternatives must be consistent or are scoped per-arm — keep simple: alternatives
  are typically literals/values; if an alternative binds, document that only the matched
  alternative's bindings are in scope).
- **Guard** `pattern if <cond>` — the arm matches only if the pattern matches AND `cond` (evaluated
  in the arm scope WITH the pattern's bindings) is truthy. If the guard is false, matching
  continues to the next arm.

## AST changes (`src/ast.rs`)
- New `enum Pattern { Wildcard, Ident(Rc<str>), Value(Box<Expr>), Range{start,end,inclusive},
  Array(Vec<Pattern>, Option<RestPat>), Object(Vec<(Rc<str>, Pattern)>, Option<RestPat>) }`
  where `RestPat = Option<Rc<str>>` (None = `...` ignore, Some = `...name`).
- `MatchArm` changes from `patterns: Vec<Expr>` to `patterns: Vec<Pattern>` plus
  `guard: Option<Expr>`. (`Vec<Pattern>` preserves `|` alternatives.)
- `Pattern` gets a `Display` impl. `MatchArm`/`Match` Display updated.

## Parser (`src/parser.rs`)
- In the match-arm parser, replace "parse expression list separated by `|`" with "parse pattern
  list separated by `|`", then optional `if <expr>` guard, then `=> body`.
- `parse_pattern`:
  - `_` → Wildcard.
  - `[` → Array (parse comma-sep sub-patterns; trailing `...name`/`...` → rest).
  - `{` → Object (parse entries: `name` → (name, Ident-bind-shorthand); `name: subpat`; `...name`/
    `...` rest).
  - else parse a value-expression via the existing expression parser; then classify:
    - a `BinOp::Range` expr → Range pattern.
    - a lone `Ident` expr → Ident pattern.
    - anything else → Value pattern.
  - (Object shorthand `{name}` produces a special "bind this field" marker so the matcher always
    binds it, distinct from an Ident value-pattern.)
- No new tokens needed (`if`, `..`, `..=`, `[`, `{`, `...`, `|` all exist). The `?Send`/precedence
  rules unaffected (pattern parsing is its own sub-grammar inside match arms).

## Interpreter (`src/interp.rs`)
- A matcher: `fn match_pattern(&self, pat: &Pattern, subject: &Value, bindings: &mut Vec<(Rc<str>,
  Value)>, env: &Env) -> Result<bool, Control>` (async if Value patterns/ranges eval exprs —
  likely async via `#[async_recursion]`). Semantics per the pattern forms above; Option-C Ident
  resolution via `env.get(name)`.
- Match eval: for each arm, for each alternative pattern, run the matcher into a fresh bindings
  vec; if it matches, evaluate the guard (if any) in a child env containing the bindings; if the
  guard passes (or absent), evaluate the body in that child env and return. Else try next.
- No-arm-matched → the existing "match: no arm matched" recoverable panic (unchanged).
- Borrow discipline: no `RefCell` borrow across `.await` (clone subject parts as needed).

## fmt (`src/fmt.rs`)
- Render each `Pattern` form back to source (array `[a, b, ...rest]`, object `{a, b: p}`, range
  `a..=b`, ident `name`, value `<expr>`, `_`), the `|` alternatives, and the `if guard`.
  Idempotent. Add `PREC`/wrapping as needed so guards/ranges round-trip.

## tree-sitter (`docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js`)
- The match arm currently matches value expressions. Extend the match-arm pattern rule to accept:
  array_pattern / object_pattern (REUSE the existing `$.array_pattern`/`$.object_pattern` rules
  already defined for destructuring `let`), a range pattern, a bare identifier, a value
  expression, `_`, `|` alternatives, and an optional `if` guard. Resolve any GLR conflicts via
  declared conflicts (do NOT add `prec` that breaks the ternary/`?`). **Regenerate `parser.c` with
  `tree-sitter generate --abi 14`.** `tests/treesitter_conformance.rs` + `frontend_conformance.rs`
  must accept every `examples/*.as` (incl. the new Phase-8 example) under BOTH parsers.

## LSP (`src/lsp/analysis.rs`)
- Pattern bindings (the captured names from Ident/Array/Object/rest patterns) are LOCALS in the
  arm body — register them so they're not flagged as undefined and so completion sees them. The
  guard expression and body see the bindings. (Static analysis only — no interpreter.)

## Sub-phases
- **8a — AST + hand-parser + interp matcher** (the core; all existing match tests pass; new
  array/object/range/ident-resolve/guard patterns work via the hand parser + interpreter).
- **8b — tree-sitter grammar + regen + conformance** (both parsers accept the new syntax; the
  differential `frontend_conformance` guardrail passes).
- **8c — fmt + LSP** (formatter renders/round-trips patterns idempotently; LSP recognizes
  bindings).
- **8d — integration** (example exercising `[value,err]`/guards/ranges/array/object/
  const-compare-vs-bind; docs (`classes-enums.md` Match section); README; full gates; holistic
  review; merge `--no-ff`).

## Decisions (made; flagged)
1. **Option C** binding semantics (resolve-the-name): defined→compare, undefined→bind. Non-breaking.
   **Settled.**
2. Object shorthand `{key}` always BINDS (documented exception; explicit `{key: pat}` for matching
   a field). **Settled.**
3. Resolution is a runtime matcher decision (parser stays simple, emits `Ident`). **Settled.**
4. Range/array/object patterns reuse existing range + destructuring grammar; no new tokens.
   **Settled.**

## Open implementation choices (decide during impl, document)
- Alternatives `p1 | p2` where an alternative binds: keep alternatives to value/literal/ident-compare
  in practice; if a binding alternative is used, only the matched alternative's bindings exist —
  document; don't over-engineer cross-alternative binding consistency checks.
- Array fixed-arity vs rest: exact length unless `...`/`...name` present (then `>=`). Document.
- Whether guards can be `async` (await inside) — allow if cheap (matcher is async), else document.

## Blast radius
Core: ast.rs (Pattern enum + MatchArm), parser.rs (pattern sub-grammar), interp.rs (matcher +
arm eval), fmt.rs (pattern rendering), grammar.js + parser.c (regen), lsp/analysis.rs (bindings).
Backward compatibility preserved (Option C). Every exhaustive match on `ExprKind`/AST that
touches MatchArm must be updated (compiler-enforced). All existing `examples/*.as` and match tests
must still pass; conformance (both parsers) must stay green.
