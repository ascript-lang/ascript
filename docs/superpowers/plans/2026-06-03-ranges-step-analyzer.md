# Ranges (inclusive + `step`) and Analyzer Batch — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `..` a sequence (direction from bounds), add inclusive `..=` everywhere and a signed `step` modifier, migrate `stream.range` to the same model, and add three `ascript check` rules — all across both engines, byte-identical.

**Architecture:** Land each behavior across the legacy tree-walker (`src/interp.rs`) and the default bytecode VM (`src/compile/` + `src/vm/`) together so the byte-identical differential gate stays green. New syntax (`..=`, `step`) can be built incrementally (the corpus doesn't use it yet); the one *changed* existing behavior (`10..1` now counts down) and the `stream.range` migration must land in both engines in the same commit.

**Tech Stack:** Rust (single binary), two front-ends (legacy `lexer/parser/ast` + `cstree` CST in `src/syntax/`), tree-sitter grammar, `cargo test`, the `ascript` CLI.

**Spec:** `docs/superpowers/specs/2026-06-03-ranges-step-analyzer-design.md`

---

## Scope note (possible split)

This plan is one coherent feature but has a clean seam: **Phases 1–8 are the range feature + the `range-step` lint** (tightly coupled — the lint needs the new range AST). **Phase 9 (the `invalid-propagate` and `unresolved-import` lint rules) is fully independent** of ranges and could be executed as a separate effort at any time. Keep them here per the batched spec, but they have no ordering dependency on Phases 1–8.

---

## Conventions used in every task

- **Build:** `cargo build` (full features). **Lint gate:** `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets` must be clean.
- **Engine selection when testing `.as`:** default is the VM. Force the tree-walker with `target/debug/ascript --tree-walker run FILE` or `ASCRIPT_ENGINE=tree-walker`.
- **Parity is sacred:** never weaken the byte-identical assertion in `tests/vm_differential.rs`. If the two engines disagree, fix the engine.
- **Exhaustive-match obligation:** when you add/extend an AST node, the matches in `src/interp.rs` (eval), `src/fmt.rs` (`write_expr_inner`), and `src/ast.rs` (`Display`) must each get an arm. For the CST, the compiler `src/compile/mod.rs` must handle it.
- **Commit trailer:** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

## File Structure (what gets created / modified)

**Legacy front-end (tree-walker):**
- `src/token.rs` — tokens already exist (`DotDot`, `DotDotEq`); no change.
- `src/lexer.rs` — `step` recognized as a contextual keyword token (or kept as ident; see Task 2).
- `src/ast.rs` — `Stmt::ForRange` gains `inclusive`/`step`; new `ExprKind::Range{start,end,inclusive,step}`; `Pattern::Range` gains `step`; `Display` arms.
- `src/parser.rs` — accept `..=` and `step` in for-range and value positions; build the new `Range` expr node.
- `src/interp.rs` — unified range iteration (direction, step, validation), value materialization, strided pattern membership.
- `src/fmt.rs` — render `..=` and `step`.

**CST front-end (VM/checker):**
- `src/syntax/lexer.rs` — `step` contextual keyword.
- `src/syntax/ast/` (+ `src/syntax/kind.rs`, `src/syntax/parser.rs`, `src/syntax/tree_builder.rs`) — `RangeExpr` gains a `step()` child accessor; `step` recognized in for/value positions.
- `src/compile/mod.rs` — remove the two V2 rejections; codegen unified iteration + value materialization + pattern membership.
- `src/vm/` — range/loop opcodes for direction + step; `ASO_FORMAT_VERSION` bump (`src/vm/aso.rs`); verifier `stack_effect` (`src/vm/verify.rs`).

**Shared:**
- `src/stdlib/stream.rs` + `docs/content/stdlib/stream.md` — `stream.range` migration.
- `src/check/rules/range_step.rs`, `src/check/rules/invalid_propagate.rs`, `src/check/rules/unresolved_import.rs` (new) + `src/check/rules/mod.rs` (`ALL`) + `src/check/config.rs` (`RULE_CODES`).
- `docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js` (+ regenerated `parser.c`).
- `examples/ranges.as` (new), language-guide docs.
- `tests/` — new unit tests + differential corpus entries.

---

## Phase 1 — Foundation: AST + keyword (additive, no behavior change)

Goal: `..=` and `step` *parse* into AST/CST on both front-ends and round-trip through `fmt`, with runtime semantics unchanged (exclusive, no step) until later phases wire them. Keep all existing tests green.

### Task 1: Legacy AST — dedicated `Range` expr node + `ForRange`/`Pattern::Range` fields

**Files:**
- Modify: `src/ast.rs` (`Stmt::ForRange` ~248, `Pattern::Range` ~348, `ExprKind`, `Display` ~494)
- Test: `src/ast.rs` (inline `#[test]`)

- [ ] **Step 1: Extend the node definitions.**

In `src/ast.rs`, change `Stmt::ForRange`:

```rust
ForRange {
    var: String,
    start: Expr,
    end: Expr,
    inclusive: bool,
    step: Option<Expr>,
    body: Vec<Stmt>,
},
```

Add a dedicated value-range expression variant to `ExprKind` (value-position ranges currently lower through `BinOp::Range`; give them a real node so they can carry `inclusive`/`step`):

```rust
/// `a..b`, `a..=b`, optionally `… step k` — value position. Materializes to
/// `array<number>` at eval. Replaces the old `Binary{op: BinOp::Range,..}` path.
Range {
    start: Box<Expr>,
    end: Box<Expr>,
    inclusive: bool,
    step: Option<Box<Expr>>,
},
```

Extend `Pattern::Range` with `step`:

```rust
Range {
    start: Box<Expr>,
    end: Box<Expr>,
    inclusive: bool,
    step: Option<Box<Expr>>,
},
```

- [ ] **Step 2: Add `Display` arms** for the new `ExprKind::Range` (and the new `Pattern::Range.step`) in `src/ast.rs`. Format `start`, then `..` or `..=` from `inclusive`, then `end`, then ` step {k}` if `step.is_some()`. Keep `BinOp::Range` Display only if the variant still exists; if you remove `BinOp::Range`, delete its arm.

- [ ] **Step 3: Compile.** Expect errors in `src/interp.rs`, `src/parser.rs`, `src/fmt.rs` for the new/changed nodes — those are wired in later tasks. To keep Phase 1 compiling, add **temporary** arms that preserve *current* behavior: `ExprKind::Range` evaluates exclusive, no-step (mirror the existing `BinOp::Range` eval at `src/interp.rs:2936`); `ForRange.inclusive=false, step=None` behaves as today. Mark these `// PHASE 1 placeholder — replaced in Phase 3/4`.

- [ ] **Step 4: Unit test** the Display round-trip:

```rust
#[test]
fn range_display_inclusive_and_step() {
    // Build ExprKind::Range { 1, 10, inclusive: true, step: Some(2) } and assert
    // its Display renders "1..=10 step 2". (Construct via parser in Task 5's test if
    // easier; here assert the Display formatting directly.)
}
```

Run: `cargo test range_display -- --nocolor` → PASS.

- [ ] **Step 5: Commit.** `git commit -m "feat(ast): Range expr node + ForRange/Pattern step+inclusive fields (placeholder eval)"`

### Task 2: `step` as a contextual keyword (both lexers)

**Files:**
- Modify: `src/lexer.rs`, `src/syntax/lexer.rs`
- Test: `src/lexer.rs` inline, `src/syntax/lexer.rs` inline

Decision: keep `step` lexing as a normal identifier token and recognize it **contextually in the parser** (cleaner than a reserved word; preserves `let step = …`). So lexers need **no change** — verify and lock it with a test.

- [ ] **Step 1: Test** that `step` still lexes as an identifier in both lexers (so `let step = 1` is unaffected):

```rust
#[test]
fn step_is_a_plain_identifier() {
    let toks = lex("let step = 1").unwrap();
    assert!(matches!(toks[1].tok, Tok::Ident(ref s) if s == "step"));
}
```

Run: `cargo test step_is_a_plain_identifier` → PASS (no lexer change needed).

- [ ] **Step 2: Commit.** `git commit -m "test(lexer): pin step as a contextual (non-reserved) identifier"`

### Task 3: Legacy parser — accept `..=` and `step` in for-range and value position

**Files:**
- Modify: `src/parser.rs` (range parsing in `for` header + the binary `..` site)
- Test: `tests/` or `src/parser.rs` inline

- [ ] **Step 1: Write failing parse tests** (`.as` source → no parse error), e.g. add to `src/parser.rs` inline tests:

```rust
#[test]
fn parses_inclusive_and_step_ranges() {
    for src in [
        "for (i in 1..=5) {}",
        "for (i in 1..10 step 2) {}",
        "for (i in 10..1 step -2) {}",
        "let xs = 1..=5",
        "let ys = 1..10 step 2",
    ] {
        let toks = lex(src).unwrap();
        assert!(parse(&toks).is_ok(), "failed to parse: {src}");
    }
}
```

Run: `cargo test parses_inclusive_and_step_ranges` → FAIL (parser rejects `..=`/`step` outside patterns).

- [ ] **Step 2: Implement parsing.** In `src/parser.rs`:
  - Where the value-position `..` range is parsed (currently producing `Binary{op: BinOp::Range}`), produce `ExprKind::Range`. Accept `Tok::DotDot` → `inclusive=false` and `Tok::DotDotEq` → `inclusive=true`. After the end expr, if the next token is the identifier `step`, consume it and parse a `step` expression into `step: Some(..)`.
  - In the `for` header range parse, accept `DotDotEq` (set `inclusive`) and a trailing `step <expr>`, populating the new `ForRange` fields.
  - `step` binds looser than the range: parse it as `(range) step (expr)` — the step expr is a full expression at the range's precedence boundary (it ends the for-header at `)` or the let-initializer at statement end).

- [ ] **Step 3: Run** `cargo test parses_inclusive_and_step_ranges` → PASS.

- [ ] **Step 4: Regression** `cargo test` (legacy parser/frontend suites) → all green; `cargo clippy --all-targets` clean.

- [ ] **Step 5: Commit.** `git commit -m "feat(parser): accept ..= and step in for-range and value position (legacy)"`

### Task 4: CST front-end — `step()` accessor + parse `..=`/`step` in for/value

**Files:**
- Modify: `src/syntax/kind.rs`, `src/syntax/parser.rs`, `src/syntax/tree_builder.rs`, `src/syntax/ast/` (the `RangeExpr`/`ForStmt` typed accessors)
- Test: `tests/frontend_conformance.rs` or inline

- [ ] **Step 1: Failing test** — the CST parser accepts the same sources as Task 3. Add to `tests/frontend_conformance.rs` (differential parser guardrail) cases for `1..=5`, `1..10 step 2` in for and value position, asserting both the legacy and CST parsers accept them with no error node.

Run the relevant test → FAIL.

- [ ] **Step 2: Implement.** In the CST:
  - The `RangeExpr` node already carries the `op` (`DotDot`/`DotDotEq`) and `start`/`end` children (compiler reads `range.op()/start()/end()` at `src/compile/mod.rs`). Add the `step` sub-expression as a child of `RangeExpr` and expose a `step()` accessor on the typed node in `src/syntax/ast/`.
  - In `src/syntax/parser.rs`/`tree_builder.rs`, parse a trailing contextual `step <expr>` after a range in for-header and value position, attaching it as the `RangeExpr` step child. Recognize `step` only when it directly follows a range end (contextual).

- [ ] **Step 3: Run** the conformance test → PASS. `cargo test --test frontend_conformance` green.

- [ ] **Step 4: Commit.** `git commit -m "feat(syntax): RangeExpr.step accessor + parse ..=/step in for/value (CST)"`

---

## Phase 2 — Inclusive `..=` everywhere (both engines)

Goal: `..=` works in for-range and value position with **inclusive** boundary; still exclusive-direction-ascending for now (sequence direction comes in Phase 4). New syntax, so safe to land per-engine.

### Task 5: Tree-walker — inclusive boundary in for-range and value position

**Files:**
- Modify: `src/interp.rs` (`Stmt::ForRange` ~1125, `ExprKind::Range` eval)
- Test: `tests/cli.rs` (run `.as`, assert stdout)

- [ ] **Step 1: Failing test.** Add to `tests/cli.rs`:

```rust
#[test]
fn inclusive_range_tree_walker() {
    // for-range inclusive
    let out = run_as_tree_walker("for (i in 1..=4) { print(i) }");
    assert_eq!(out.trim(), "1\n2\n3\n4");
    // value position inclusive
    let out = run_as_tree_walker("print(1..=5)");
    assert_eq!(out.trim(), "[1, 2, 3, 4, 5]");
}
```

(Use the existing `.as`-running test helper; add a `--tree-walker` variant helper if none exists — mirror the existing run helpers in `tests/cli.rs`.)

Run → FAIL (inclusive currently rejected / treated exclusive).

- [ ] **Step 2: Implement** inclusive iteration in `src/interp.rs`. In `Stmt::ForRange`, loop condition becomes `if inclusive { i <= hi } else { i < hi }` (ascending; direction added Phase 4). In `ExprKind::Range` eval, materialize with the same inclusive boundary.

- [ ] **Step 3: Run** → PASS.

- [ ] **Step 4: Commit.** `git commit -m "feat(interp): inclusive ..= in for-range and value position"`

### Task 6: VM/compiler — inclusive boundary; remove the two V2 rejections

**Files:**
- Modify: `src/compile/mod.rs` (rejections at ~322 and ~2461; `compile_for`; value-range materialization)
- Modify: `src/vm/` (loop bound comparison opcode if needed), `src/vm/aso.rs` (`ASO_FORMAT_VERSION`), `src/vm/verify.rs` (`stack_effect` if a new op is added)
- Test: `tests/cli.rs` (default VM), `tests/vm_differential.rs`

- [ ] **Step 1: Failing test.** Add VM-default versions:

```rust
#[test]
fn inclusive_range_vm() {
    let out = run_as("for (i in 1..=4) { print(i) }");      // default engine = VM
    assert_eq!(out.trim(), "1\n2\n3\n4");
    let out = run_as("print(1..=5)");
    assert_eq!(out.trim(), "[1, 2, 3, 4, 5]");
}
```

Run → FAIL (compiler rejects `..=` at `src/compile/mod.rs:322,2461`).

- [ ] **Step 2: Implement.** Read `compile_for` and the value-range compile path in `src/compile/mod.rs`, and the loop opcodes in `src/vm/` (disassemble an exclusive for-range with `ascript build` + the disassembler to see the emission). Then:
  - Delete the two `CompileError` rejections (`src/compile/mod.rs:322-323` value default, `:2461-2466` inclusive for-range).
  - Emit an inclusive bound comparison when `range.op() == DotDotEq` (either a new `RANGE_INCLUSIVE` loop opcode or reuse the existing loop op with an inclusive flag operand — choose whichever matches the existing emission style; mirror how exclusive is currently emitted).
  - Value-position `..=` materializes inclusively.
  - If you add/alter an opcode: bump `ASO_FORMAT_VERSION` in `src/vm/aso.rs` and update `stack_effect` in `src/vm/verify.rs`.

- [ ] **Step 3: Run** `cargo test inclusive_range_vm` → PASS. Then `cargo test --test vm_differential` and `cargo test --test aso` → green (VM == tree-walker on the new behavior).

- [ ] **Step 4: Commit.** `git commit -m "feat(vm): inclusive ..= in for-range and value position; drop V2 rejections; ASO bump"`

---

## Phase 3 — `step` iteration + validation (both engines)

Goal: `step` works in for-range and value position with sign-honored direction and the validation panics. Direction *when step is omitted* still ascending-only until Phase 4 (so `10..1` stays empty here; `10..1 step -1` already works because step sign drives direction).

### Task 7: Tree-walker — `step` + validation panics

**Files:**
- Modify: `src/interp.rs` (`ForRange`, `ExprKind::Range`)
- Test: `tests/cli.rs`

- [ ] **Step 1: Failing tests.**

```rust
#[test]
fn step_iteration_tree_walker() {
    assert_eq!(run_as_tree_walker("for (i in 1..10 step 2){print(i)}").trim(), "1\n3\n5\n7\n9");
    assert_eq!(run_as_tree_walker("for (i in 10..1 step -2){print(i)}").trim(), "10\n8\n6\n4\n2");
    assert_eq!(run_as_tree_walker("print(1..=10 step 2)").trim(), "[1, 3, 5, 7, 9]");
    assert_eq!(run_as_tree_walker("print(0..=1 step 0.25)").trim(), "[0, 0.25, 0.5, 0.75, 1]");
}

#[test]
fn step_validation_panics_tree_walker() {
    assert!(run_as_tree_walker_err("for (i in 1..10 step 0){}").contains("finite, non-zero"));
    assert!(run_as_tree_walker_err("for (i in 1..10 step -2){}").contains("moves away from end"));
    assert!(run_as_tree_walker_err("for (i in 10..1 step 2){}").contains("moves away from end"));
}
```

(`run_as_tree_walker_err` captures stderr + asserts non-zero exit; mirror existing error-asserting helpers in `tests/cli.rs`.)

Run → FAIL.

- [ ] **Step 2: Implement** a shared validator + iterator in `src/interp.rs`. Add a helper:

```rust
/// Resolve (lo, hi, step) for a range. `step_v` is None when omitted.
/// Returns Err(panic) on zero/non-finite step or a direction mismatch.
fn range_step(lo: f64, hi: f64, step_v: Option<f64>, span: Span) -> Result<f64, Control> {
    let step = match step_v {
        Some(s) => {
            if s == 0.0 || !s.is_finite() {
                return Err(AsError::at("step must be a finite, non-zero number", span).into());
            }
            // mismatch: nonempty range whose step sign disagrees with the bounds
            if lo != hi && (s > 0.0) != (hi > lo) {
                return Err(AsError::at(
                    &format!("step {s} moves away from end ({hi}); range can never progress"),
                    span,
                ).into());
            }
            s
        }
        None => if hi >= lo { 1.0 } else { -1.0 }, // direction from bounds
    };
    Ok(step)
}
```

Iterate with `while (step > 0.0 && cond_lt) || (step < 0.0 && cond_gt)` where `cond_lt`/`cond_gt` honor `inclusive` (`<=`/`>=`). Use this in both `ForRange` (lazy) and `ExprKind::Range` (materialize).

NOTE: in Phase 3 the `None` branch already returns `-1.0` for `hi < lo`, which would make bare `10..1` count down — but Phase 4's differential test is what *certifies* that change across engines. To keep this task's commit green against the still-old VM, **temporarily** keep the `None` branch ascending-only (`1.0`) here, and flip it to the snippet above in Task 9 (Phase 4) together with the VM. Add a `// Phase 4 flips this` comment.

- [ ] **Step 3: Run** `cargo test step_iteration_tree_walker step_validation_panics_tree_walker` → PASS. `cargo test --test vm_differential` still green (bare-range direction unchanged this task).

- [ ] **Step 4: Commit.** `git commit -m "feat(interp): step iteration + zero/mismatch validation panics"`

### Task 8: VM/compiler — `step` + validation panics

**Files:**
- Modify: `src/compile/mod.rs` (for-range + value-range codegen), `src/vm/` (range loop op with a step operand; emit the validation panic), `src/vm/verify.rs`, `src/vm/aso.rs` (bump if op changes)
- Test: `tests/cli.rs` (VM), `tests/vm_differential.rs`

- [ ] **Step 1: Failing tests** — VM-default copies of Task 7's iteration + panic tests (`run_as` / `run_as_err`).

Run → FAIL.

- [ ] **Step 2: Implement** in the VM path. Read the current for-range emission (disassemble one) and extend:
  - Compile the optional `step()` child; default to the literal `1`/`-1` decision deferred to runtime (the VM must compute direction the same way the tree-walker does — emit the bounds and step and let a range-setup opcode resolve direction + validate, OR compute in codegen when operands are constant; prefer a runtime range-setup op so computed steps behave identically).
  - Emit the same panic messages on zero/non-finite/mismatch (reuse the strings verbatim so diagnostics match the tree-walker).
  - Keep bare-range direction ascending-only this task (Phase 4 flips both engines together).
  - Bump `ASO_FORMAT_VERSION`; update `stack_effect`.

- [ ] **Step 3: Run** VM tests + `cargo test --test vm_differential --test aso` → green.

- [ ] **Step 4: Commit.** `git commit -m "feat(vm): step iteration + validation panics (byte-identical to tree-walker)"`

---

## Phase 4 — Sequence direction (the one changed behavior; both engines, atomic)

Goal: bare descending ranges count down. `10..1` → `10,9,…,2`; `10..=1` → `10,…,1`. This **changes existing `..` behavior**, so tree-walker + VM must flip in the **same commit**.

### Task 9: Flip bare-range direction in both engines together

**Files:**
- Modify: `src/interp.rs` (the `None` branch from Task 7), `src/compile/mod.rs` + `src/vm/` (the matching bare-range setup)
- Test: `tests/cli.rs` (both engines), `tests/vm_differential.rs`

- [ ] **Step 1: Failing tests** (both engines must agree):

```rust
#[test]
fn descending_bare_range_counts_down() {
    for run in [run_as, run_as_tree_walker] {
        assert_eq!(run("for (i in 5..1){print(i)}").trim(), "5\n4\n3\n2");
        assert_eq!(run("print(10..=1)").trim(), "[10, 9, 8, 7, 6, 5, 4, 3, 2, 1]");
        assert_eq!(run("print(5..5)").trim(), "[]");
        assert_eq!(run("print(5..=5)").trim(), "[5]");
    }
}
```

Run → FAIL on both.

- [ ] **Step 2: Implement.** In `src/interp.rs`, change the Task 7 `None` branch to `if hi >= lo { 1.0 } else { -1.0 }` (remove the temporary). In the VM, make the bare-range setup compute direction from bounds identically. Do both before committing.

- [ ] **Step 3: Run** `cargo test descending_bare_range_counts_down` → PASS. Then **the full differential**: `cargo test --test vm_differential --test aso` → green (no corpus example uses descending bare ranges, so goldens are unaffected; if any golden shifts, regenerate it and justify).

- [ ] **Step 4: Commit.** `git commit -m "feat: bare descending ranges count down (sequence direction), both engines"`

---

## Phase 5 — Match-pattern `step` (strided membership, both engines)

### Task 10: Tree-walker — strided pattern membership

**Files:**
- Modify: `src/interp.rs` (`Pattern::Range` match ~1635)
- Test: `tests/cli.rs`

- [ ] **Step 1: Failing test.**

```rust
#[test]
fn stepped_range_pattern_tree_walker() {
    let prog = r#"
        fn cls(n) { match n { 1..=10 step 2 => "odd-in", 0..=10 step 2 => "even-in", _ => "out" } }
        print(cls(3)); print(cls(4)); print(cls(11))
    "#;
    assert_eq!(run_as_tree_walker(prog).trim(), "odd-in\neven-in\nout");
}
```

Run → FAIL.

- [ ] **Step 2: Implement.** In `Pattern::Range` matching, after the bounds test (honoring `inclusive` and direction), if `step.is_some()` evaluate it, run the **same validator** (`range_step`) for zero/mismatch panic, then require `((x - start) / step)` to be a whole number (`.fract() == 0.0`) — strided membership. Anchor is `start`.

- [ ] **Step 3: Run** → PASS.

- [ ] **Step 4: Commit.** `git commit -m "feat(interp): step in match range patterns (strided membership)"`

### Task 11: VM/compiler — strided pattern membership

**Files:**
- Modify: `src/compile/mod.rs` (the `MatchRange` pattern codegen), `src/vm/` (extend the range-pattern op with a step operand), `src/vm/verify.rs`, `src/vm/aso.rs`
- Test: `tests/cli.rs` (VM), `tests/vm_differential.rs`

- [ ] **Step 1: Failing test** — VM-default copy of Task 10.

- [ ] **Step 2: Implement** the strided membership in the `MatchRange` path, byte-identical to the tree-walker (same panic strings). Bump `ASO_FORMAT_VERSION` if the op changes; update `stack_effect`.

- [ ] **Step 3: Run** VM test + differential → green.

- [ ] **Step 4: Commit.** `git commit -m "feat(vm): step in match range patterns (byte-identical)"`

---

## Phase 6 — `stream.range` migration

### Task 12: Migrate `stream.range` to the unified model

**Files:**
- Modify: `src/stdlib/stream.rs` (`stream_range` ~285, advance ~506-514, tests ~871/921)
- Modify: `docs/content/stdlib/stream.md` (~67-74)
- Test: `src/stdlib/stream.rs` inline `#[tokio::test]`

- [ ] **Step 1: Failing tests.** Add/extend in `src/stdlib/stream.rs`:

```rust
#[tokio::test]
async fn range_infers_direction_and_panics_on_mismatch() {
    assert_eq!(collect("stream.range(10, 1)"), vec![10.0,9.0,8.0,7.0,6.0,5.0,4.0,3.0,2.0]); // was []
    assert_eq!(collect("stream.range(0, 5)"), vec![0.0,1.0,2.0,3.0,4.0]);                    // unchanged
    assert_eq!(collect("stream.range(10, 0, -3)"), vec![10.0,7.0,4.0,1.0]);                  // unchanged
    assert!(range_err("stream.range(1, 10, -2)").contains("moves away from end"));           // was []
    assert!(range_err("stream.range(1, 10, 0)").contains("finite, non-zero"));
}
```

(Use the module's existing stream-collection test harness; `range_err` asserts the call panics.)

Run → FAIL.

- [ ] **Step 2: Implement.** In `stream_range`: when `step` is omitted, infer sign from bounds (`if end >= start { 1.0 } else { -1.0 }`). When present and `start != end`, panic on sign mismatch with the **same string** used by the engines (`"step {s} moves away from end ({end}); range can never progress"`). Keep the zero-step panic (reword to match: `"step must be a finite, non-zero number"`). The advance logic at `:506-514` already branches on step sign — it stays correct once direction/validation are set at construction.

- [ ] **Step 3: Update docs** `docs/content/stdlib/stream.md`: note default step direction is inferred from bounds; `range(10,1)` counts down; a step whose sign disagrees with the bounds panics. Keep the three existing examples (still valid).

- [ ] **Step 4: Run** `cargo test --lib range_infers_direction` and the existing stream tests → green. Update `range_with_step`/`range_negative_step_counts_down` if their assertions changed.

- [ ] **Step 5: Commit.** `git commit -m "feat(stdlib): stream.range — infer direction from bounds + mismatch panic (unified model)"`

---

## Phase 7 — `range-step` lint rule

### Task 13: `range-step` checker rule

**Files:**
- Create: `src/check/rules/range_step.rs`
- Modify: `src/check/rules/mod.rs` (`ALL`), `src/check/config.rs` (`RULE_CODES`)
- Test: `src/check/rules/range_step.rs` inline (mirror `contract.rs` tests) + `tests/cli.rs` (`ascript check`)

- [ ] **Step 1: Failing test.** Mirror the test style in `src/check/rules/contract.rs`. Assert that `analyze("for (i in 1..10 step 0){}")` yields a `range-step` diagnostic; `1..10 step -2` (literal mismatch) yields one; `1..10 step 2` yields none; and a *float* step in a *match pattern* (`match n { 0..=1 step 0.25 => 1, _ => 0 }`) yields the advisory.

Run → FAIL (rule doesn't exist / not registered).

- [ ] **Step 2: Implement** `src/check/rules/range_step.rs` following the `contract.rs` shape:

```rust
//! `range-step`: flag a statically-bad stepped range (literal step 0 / non-finite,
//! literal direction mismatch) — matching the guaranteed runtime panic — plus an
//! advisory for a float step inside a match pattern (fragile membership).
use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::code_range;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(tree: &ResolvedNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    let mut out = Vec::new();
    for n in tree.descendants().filter(|n| matches!(n.kind(), SyntaxKind::RangeExpr | SyntaxKind::RangePat)) {
        // read literal start/end/step from n's children; skip if any is non-literal.
        // step == 0 / NaN / Inf  -> Severity::Warning "step must be a finite, non-zero number"
        // start != end && sign(step) != sign(end-start) -> Warning "... moves away from end ..."
        // n.kind()==RangePat && step is a non-integer literal -> Warning float-membership advisory
        // push AsDiagnostic { range: code_range(&n), severity, code: "range-step", message, fix: None }
    }
    out
}
```

Fill in the literal extraction (mirror how `contract.rs` reads literal arg kinds) and the three checks. Register it: add `pub mod range_step;` and `range_step::check,` to `ALL` (`src/check/rules/mod.rs:20`), and add `"range-step"` to `RULE_CODES` (`src/check/config.rs:27`).

- [ ] **Step 3: Run** the rule tests + `cargo test --test cli` check-command tests → PASS.

- [ ] **Step 4: Commit.** `git commit -m "feat(check): range-step rule (literal step0/mismatch + float-step-in-pattern advisory)"`

---

## Phase 8 — Grammar, formatter, examples, docs

### Task 14: tree-sitter grammar + `fmt`

**Files:**
- Modify: `docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js` (+ regenerate `parser.c`), `src/fmt.rs`
- Test: `tests/treesitter_conformance.rs`, `src/fmt.rs` inline

- [ ] **Step 1: Failing tests.** (a) `tests/treesitter_conformance.rs` includes a `.as` snippet using `..=` and `step` in for/value/pattern positions and asserts the tree-sitter grammar parses it with no error. (b) `fmt` idempotence: formatting `1..=10 step 2` round-trips unchanged.

Run → FAIL.

- [ ] **Step 2: Implement.** Add `..=`/`step` to the grammar's range/for productions; `tree-sitter generate --abi 14`. In `src/fmt.rs`, render `ExprKind::Range`/`ForRange`/`Pattern::Range` with `..`/`..=` and ` step k`, with precedence so `a..b step c` is not wrongly parenthesized.

- [ ] **Step 3: Run** `cargo test --test treesitter_conformance` + fmt tests → PASS.

- [ ] **Step 4: Commit.** `git commit -m "feat(grammar,fmt): ..= and step productions + formatting"`

### Task 15: Example program + docs + differential corpus entry

**Files:**
- Create: `examples/ranges.as`
- Modify: language-guide range docs under `docs/content/`, `tests/vm_differential.rs` (add `examples/ranges.as` to the corpus, not the skip list)
- Test: conformance + differential

- [ ] **Step 1: Write `examples/ranges.as`** exercising: ascending/descending bare ranges, `..=`, positive/negative `step`, float step, value-position materialization, and a stepped match pattern — each with `assert(...)` against the §3.5 truth table.

- [ ] **Step 2: Verify** `target/debug/ascript run examples/ranges.as` (VM) and `--tree-walker` both exit 0 with identical output. Ensure `examples/ranges.as` is covered by `tests/treesitter_conformance.rs` and is NOT on the `tests/vm_differential.rs` skip list.

- [ ] **Step 3: Update docs** — the language-guide range section: sequence direction, `..` vs `..=`, `step` (signed; default direction from bounds; zero/mismatch panic), value materialization, pattern membership + the float caveat.

- [ ] **Step 4: Run** `cargo test` (whole suite incl. differential + conformance) → green; both clippy configs clean.

- [ ] **Step 5: Commit.** `git commit -m "docs,examples: ranges.as + language-guide range section; corpus parity"`

---

## Phase 9 — Independent analyzer rules (no dependency on Phases 1–8)

### Task 16: `invalid-propagate` rule (`?` in a non-Result fn)

**Files:**
- Create: `src/check/rules/invalid_propagate.rs`
- Modify: `src/check/rules/mod.rs` (`ALL`), `src/check/config.rs` (`RULE_CODES`)
- Test: inline + `tests/cli.rs`

- [ ] **Step 1: Failing test.** `analyze("fn f(): number { g()? }")` → one `invalid-propagate` diagnostic; `fn f(): Result<number> { g()? }` → none; `fn f() { g()? }` (no annotation) → none.

Run → FAIL.

- [ ] **Step 2: Implement** following `contract.rs`/`missing_return.rs` shape: walk `FnDecl`/`MethodDecl` with a return-type annotation that is not a `Result`/pair type; if its body contains a `Try` (`?`) node not nested inside another fn, emit a `invalid-propagate` Warning on the `?`. Register in `ALL` + `RULE_CODES` (`"invalid-propagate"`).

- [ ] **Step 3: Run** → PASS.

- [ ] **Step 4: Commit.** `git commit -m "feat(check): invalid-propagate rule (? in non-Result fn) — closes spec §257"`

### Task 17: `unresolved-import` rule

**Files:**
- Create: `src/check/rules/unresolved_import.rs`
- Modify: `src/check/rules/mod.rs`, `src/check/config.rs`
- Test: inline + `tests/cli.rs`

- [ ] **Step 1: Failing test.** `analyze("import { abs } from \"std/maths\"")` → one `unresolved-import` (typo'd module); `"std/math"` → none. For a relative path, a non-existent file → diagnostic. (For file paths, the rule needs the source path; if `analyze` is path-less, restrict V1 to `std/*` resolvability and note file-path checking as a follow-up — do not silently skip without the note.)

Run → FAIL.

- [ ] **Step 2: Implement.** Walk `ImportStmt` nodes; for a `std/*` specifier, check membership against the static module list (the set backing `std_module_exports`); for a named import, optionally check the name is exported. Emit `unresolved-import` Warning. Register in `ALL` + `RULE_CODES` (`"unresolved-import"`).

- [ ] **Step 3: Run** → PASS.

- [ ] **Step 4: Commit.** `git commit -m "feat(check): unresolved-import rule (std/* path + name resolvability)"`

### Task 18: Checker docs + final gate

**Files:**
- Modify: `docs/content/cli.md` (or the checker reference) — document `range-step`, `invalid-propagate`, `unresolved-import` + their default severities and config.
- Test: full suite

- [ ] **Step 1:** Add the three new codes to the checker docs table with one-line descriptions, default severities, and an `ascript.toml [lint]` override example.

- [ ] **Step 2: Final gate.** `cargo test` (all features) AND `cargo test --no-default-features` → green; `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets` → clean; `cargo run -- run examples/ranges.as` → exit 0.

- [ ] **Step 3: Commit.** `git commit -m "docs(check): document range-step, invalid-propagate, unresolved-import"`

---

## Self-review checklist (done while writing)

- **Spec coverage:** §3 model → Tasks 5–11; §3.6 value position → Tasks 5/6; §3.7 patterns → Tasks 10/11/13; §3.8 float → Task 7/13; §4 stream.range → Task 12; §5.1 range-step → Task 13; §5.2 invalid-propagate → Task 16; §5.3 unresolved-import → Task 17; §5.4 severities → Tasks 13/16/17 + 18; §7 surface → all phases; §8 breakage (no descending corpus ranges) → Task 9 note; §9 testing → per-task tests + Task 15.
- **Parity:** every behavior change ships in both engines before its commit; the one changed existing behavior (descending direction) is atomic in Task 9; `ASO_FORMAT_VERSION` bumps called out in Tasks 6/8/11.
- **Type consistency:** the `range_step(lo,hi,step,span)` validator (Task 7) is reused in Tasks 8/10/11/12 with identical panic strings; `RULE_CODES` strings (`range-step`, `invalid-propagate`, `unresolved-import`) match their registrations and docs.
- **Known soft spots (read before implementing):** VM opcode emission in Tasks 6/8/11 requires reading `compile_for` + the `src/vm/` loop/range ops and disassembling an existing range (the plan specifies behavior + tests + sites; the exact opcode shape is learned from the code, mirroring existing emission). `unresolved-import` file-path checking depends on whether `analyze` has the source path (Task 17 restricts V1 to `std/*` with a noted follow-up rather than silently skipping).
