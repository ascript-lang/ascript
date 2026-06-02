# VM Plan V10 — Match, destructuring, spread (language-complete → whole-corpus differential gate ON)

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development.

**Goal:** Implement `match` with all pattern kinds (Option-C runtime resolution), array/object destructuring `let` (incl. rest), and spread in array/object literals + call args. After this slice the VM covers the ENTIRE language, so the differential gate flips from an allow-list to the **whole `examples/` corpus + full `cargo test` suite** in both feature configs (oracle #1) — the central correctness proof of the VM.

**Architecture:** Compiler lowers patterns to a sequence of test/bind ops; `match` is a cascade of pattern tests with jumps + guards. Destructuring `let` lowers to index/key reads into the binding slots. Spread uses the `SPREAD` op when building arrays/objects/arg-lists. Reuse the tree-walker's pattern-match semantics (`match_pattern`) and destructuring semantics EXACTLY. **Depends on V9.**

---

## Ground truth (mirror EXACTLY)
- `MatchArm { patterns: Vec<Pattern>, guard: Option<Expr> }`; `Pattern::{Wildcard, Ident(name), Value(expr), Range{start,end,inclusive}, Array(pats, rest), Object(entries, rest)}`; `ObjPatEntry{key, pat:Option}` (`pat:None` = shorthand bind). Option-C: a bare `Ident(name)` compares `subject==value` if `name` is DEFINED in scope, else BINDS the subject. Object shorthand `{key}` is ALWAYS a bind. `..=` inclusive range only in patterns. (CLAUDE.md Phase 8.)
- Tree-walker `match_pattern` (`src/interp.rs`) — the authoritative matcher; replicate its order, binding scoping (each arm a scope), guard evaluation (after bind, before body), and fall-through. The resolver already does match-arm scopes + Option-C (P3-T7).
- Destructuring: `let [a, ...r] = xs` (array, with rest collecting the tail), `let {a, b as local, "k" as v, ...rest} = obj` (object, rest = leftover keys excluding bound source keys); missing keys → nil. `Stmt::LetDestructure`/`LetDestructureObject`. Match the exact semantics + error cases.
- Spread: `[...a, x]`, `{...o, k:v}` (later-value-wins, IndexMap first-seen position), `f(...args)`. `Tok::DotDotDot`; typed-element AST. Spreading a wrong container → Tier-2 panic (identical message).

---

## Tasks
- [ ] **T1 — destructuring `let` (array + object + rest).** Lower `LetDestructureObject` to: eval RHS once (a temp slot); for each binding, `GET_PROP`/key-read into the binding's slot (missing → Nil); rest collects leftover keys into an Object excluding bound source keys. Lower array `LetDestructure` to indexed reads (`GET_INDEX` by position) + rest collecting the tail array. Errors (destructuring a non-object/array) identical to tree-walker. Tests: `object_destructuring.as`-style + array destructure + rest, missing keys, quoted keys, `as` rename. Commit.
- [ ] **T2 — spread in literals + calls.** `NEW_ARRAY`/`NEW_OBJECT` with spread elements: emit element/spread markers; `SPREAD` op flattens an iterable onto the under-construction array/object (object-spread later-value-wins, IndexMap order). `CALL` with spread args: build the arg vector with spread expansion before the call (a `SPREAD` into an arg accumulator, or a `CALL_SPREAD` variant — choose; building an args array then a spread-aware call is simplest). Wrong-container spread → identical panic. Tests: `spread.as`, `rest.as` (rest params already V4; rest patterns T1). Commit.
- [ ] **T3 — match: literal/value/wildcard/range patterns + guards.** `MatchExpr { subject, arms }`: eval subject into a temp; for each arm, for each alternative pattern, emit tests: `Wildcard`→always; `Value(expr)`→eval+`EQ`; `Ident` defined→`EQ` with its value, undefined→bind (push subject into the arm's binding slot); `Range`→`>=start && <(=)end`. If any alternative matches AND the guard (if present) is true → run the arm body, `JUMP end`; else next arm. No arm matches → the tree-walker's behavior (panic? nil? verify and mirror). Each arm is a scope (bindings in arm slots). Tests: literals, ranges (excl/incl), guards, Option-C bind-vs-compare, `|` alternatives. Commit.
- [ ] **T4 — match: array/object patterns + rest + nesting.** `Pattern::Array(pats, rest)` → test subject is an array of compatible length (rest allows tail), recursively test/bind each element; `Pattern::Object(entries, rest)` → test keys present, recursively test/bind, rest binds leftover. Nested patterns recurse. Shorthand `{key}` binds. Match the tree-walker's `match_pattern` for length/rest/missing-key behavior exactly. Tests: array/object patterns, nesting, rest binds, shorthand. Commit.
- [ ] **T5 — FLIP THE DIFFERENTIAL GATE TO THE WHOLE CORPUS.** Replace the allow-list in `tests/vm_differential.rs` with a runner over ALL `examples/*.as` + `examples/advanced/*.as` (skipping only those needing a network peer/TTY, with an explicit documented skip-list — same set the existing conformance tests skip), asserting byte-identical stdout + exit code vs the tree-walker. ALSO add a `cargo test`-suite differential: a representative set of the language tests run through the VM == tree-walker. Any divergence is a real bug → fix the compiler/VM (NEVER relax the gate). This is oracle #1, the central VM correctness proof. Iterate until the whole corpus is byte-identical. Commit.
- [ ] **T6 — recorded goldens (oracle #2).** If `assert.snapshot`/Phase 9 goldens exist, run them through the VM; if they DON'T exist yet (survey said no `std/bench`/snapshot), record stdout goldens from the CURRENT tree-walker for the whole corpus into `tests/vm_goldens/` and assert the VM reproduces them (these survive the tree-walker's eventual deletion at cutover). Commit.
- [ ] **T7 — full suite + clippy.** `cargo test` (both feature configs) green; clippy clean both configs. Commit.

## Done criteria (V10) — the VM is language-complete
- [ ] match (all patterns + guards + Option-C), destructuring (array/object/rest), spread (literals + calls) identical to the tree-walker.
- [ ] **Whole-corpus differential gate ON: VM == tree-walker byte-identical on all examples + the test suite, both feature configs.**
- [ ] Recorded goldens captured (oracle #2). `cargo test` green; clippy clean both configs.

**Next:** V11 — shapes + inline caches + PEP-659 specialization (the performance layer), built AFTER parity, semantics-preserving (guards + deopt), gated by the three-way differential (generic-VM == specialized-VM == tree-walker) + the perf gate.
