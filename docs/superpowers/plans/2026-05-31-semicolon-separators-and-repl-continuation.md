# `;` Separators in Class Bodies + REPL Multi-line Continuation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `;` a uniform optional statement separator inside class bodies (closing a half-kept promise), and add multi-line continuation to the REPL so multi-line constructs (classes, multi-line functions/objects) can be entered interactively.

**Architecture:** Two small, independent phases. Phase 1 adds one `skip_semicolons()` call to the class-member loop in the hand parser plus the tree-sitter grammar. Phase 2 adds a token-delimiter-depth "is this input incomplete?" check to the REPL so it buffers physical lines (on a `..` prompt) until a statement parses, then execs the whole buffer against the already-persistent session env.

**Tech Stack:** Rust hand-written parser (`src/parser.rs`), tree-sitter grammar (regen `--abi 14`), `rustyline` REPL (`src/repl.rs`), the existing lexer (`src/lexer.rs`).

**Decisions locked (from design discussion):**
- `;` scope: **declaration/statement lists only** — `;` ≡ newline, and **never replaces `,`**. Top-level and block statement lists already honor `;` (`skip_semicolons`, `parser.rs:60`). The only self-delimiting member loop missing it is the **class body** (`parser.rs:269`). **Enums are EXCLUDED**: enum variants are comma-delimited (`parser.rs:237`), so by the "never replaces `,`" rule `;` does not apply there. Match arms, params, array/object literals are comma-delimited and likewise excluded.
- REPL incomplete-input detection: **unclosed delimiters / EOF only.** Continue (buffer more lines) when the lexed buffer has positive delimiter-token depth (`{`/`(`/`[` minus closers) OR an unterminated string/template at EOF. A genuine mid-line syntax error (balanced delimiters) is reported immediately. We deliberately do NOT continue on trailing operators.
- Why token-delimiter counting (not brace-char counting): AScript has `${…}` template interpolation whose braces live inside Str/Template tokens, not `LBrace`/`RBrace` tokens — so counting delimiter *tokens* is immune to the template-string wrinkle.

---

## Conventions (both phases)

- **Commit trailer (required):** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`
- **Tree-sitter regen:** after any `grammar.js` change: `cd docs/superpowers/specs/grammar/tree-sitter-ascript && tree-sitter generate --abi 14`, then `cargo build`.
- **Two-config rule:** `cargo test` and `cargo test --no-default-features` both pass; `cargo clippy --all-targets` and `cargo clippy --no-default-features --all-targets` both clean.
- Baseline at plan start: **686 tests passing (default) / 373 (no-default)**, clippy clean both. Nothing may regress.

---

## Phase 1 — `;` as an optional separator in class bodies

**Goal:** `class P { x: number; y: number }` and `class C { fn a() {}; fn b() {} }` parse (one-line class defs, useful in the REPL); the formatter still canonicalizes to newlines.

**Files:**
- Modify: `src/parser.rs` — class member loop (~line 269): consume stray `;` between members.
- Modify: `docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js` — allow optional `;` between class members; regen `parser.c`.
- Modify: docs (language guide) + `CLAUDE.md` note.

### Task 1.1: Parser — consume `;` between class members

**Files:**
- Modify: `src/parser.rs` (class member loop, the `while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof` at ~line 269)
- Test: inline `#[test]` in `src/parser.rs`

- [ ] **Step 1: Write the failing test.** In `src/parser.rs` tests:

```rust
#[test]
fn class_body_allows_semicolon_separators() {
    // one-line class with `;` between fields
    let p = parse(&lex("class P { x: number; y: number }").unwrap()).unwrap();
    match &p[0] {
        Stmt::Class { fields, .. } => assert_eq!(fields.len(), 2),
        other => panic!("expected Class, got {other:?}"),
    }
    // `;` between methods, and a trailing `;`
    assert!(parse(&lex("class C { fn a() {}; fn b() {}; }").unwrap()).is_ok());
    // mixed field + method on one line
    assert!(parse(&lex("class M { n: number; fn get() { return self.n } }").unwrap()).is_ok());
    // newline-separated still works (no regression)
    assert!(parse(&lex("class N {\n  a: number\n  b: number\n}").unwrap()).is_ok());
}
```

> Confirm the real field/variant accessor name on `Stmt::Class` (it may be `fields`/`methods`); adjust the match to the actual struct shape (read the `Stmt::Class` definition in `src/ast.rs`).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test class_body_allows_semicolon_separators`
Expected: FAIL — `expected a field name or method, found Semicolon`.

- [ ] **Step 3: Implement.** In `src/parser.rs`, in the class-member `while` loop body (the one at ~line 269 that dispatches method-vs-field), call `self.skip_semicolons();` at the TOP of the loop, and also once before the loop's `eat(&Tok::RBrace)` is reached. The cleanest single-point fix is to skip leading semicolons at the start of each iteration AND allow a trailing run before `}`:

```rust
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            self.skip_semicolons();
            if *self.peek() == Tok::RBrace { break; } // trailing `;` before `}`
            // ... existing member (method-or-field) parsing unchanged ...
        }
```

> `skip_semicolons(&mut self)` already exists (`parser.rs:60`). Do NOT change comma handling anywhere. Do NOT touch the enum loop (enums are comma-delimited — out of scope by decision).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test class_body_allows_semicolon_separators`
Expected: PASS.

- [ ] **Step 5: Confirm the formatter still canonicalizes to newlines** (no fmt change needed — verify it). Add:

```rust
#[test]
fn class_with_semicolons_formats_to_newlines() {
    // the formatter emits one member per line regardless of `;` input
    let out = format_source("class P { x: number; y: number }").unwrap();
    assert!(out.contains("x: number\n"), "got: {out}");
    assert!(!out.contains(';'), "formatter should not emit semicolons: {out}");
}
```

> Use the real fmt helper name (`format_source`/`format_str` — check neighbouring fmt tests). If the formatter already prints class fields one-per-line, this passes with no fmt change. If it does NOT, STOP and report — the plan assumed canonical newline output.

Run: `cargo test class_with_semicolons_formats_to_newlines`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/parser.rs
git commit -m "feat(parser): honor ; as an optional separator in class bodies

Closes the half-kept promise: ; already separates top-level/block statements
(skip_semicolons); class members are self-delimiting too. Enums stay comma-
delimited (out of scope). Formatter still canonicalizes to newlines.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 1.2: Tree-sitter grammar — optional `;` between class members

**Files:**
- Modify: `docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js`
- Regen: `parser.c`

- [ ] **Step 1: Inspect the grammar's class body rule.** Find the `class_declaration` / `class_body` rule and how it lists members (likely `repeat($._class_member)` or similar). Determine whether `;` is already tolerated (some grammars use a `_statement_separator`).

- [ ] **Step 2: Allow optional `;` between/after members.** Add an optional semicolon to the member repetition, e.g. wrap members as `repeat(seq($._class_member, optional(';')))` (match the rule's actual structure). If the grammar has a shared optional-`;` helper used by statements, reuse it.

- [ ] **Step 3: Regenerate and build**

```bash
cd docs/superpowers/specs/grammar/tree-sitter-ascript && tree-sitter generate --abi 14 && cd /Users/mahmoud/ascript
cargo build
```

- [ ] **Step 4: Verify the grammar accepts `;` class bodies (zero ERROR nodes)**

```bash
cd docs/superpowers/specs/grammar/tree-sitter-ascript
printf 'class P { x: number; y: number }\n' | tree-sitter parse /dev/stdin 2>&1 | grep -c ERROR   # expect 0
printf 'class C {\n  a: number\n  b: number\n}\n' | tree-sitter parse /dev/stdin 2>&1 | grep -c ERROR  # expect 0 (no regression)
cd /Users/mahmoud/ascript
```

If `tree-sitter` CLI is unavailable, report BLOCKED on this task (do not hand-edit parser.c). If a GLR conflict appears, declare it in the grammar's `conflicts` array with a comment.

- [ ] **Step 5: Conformance**

Run: `cargo test --test treesitter_conformance && cargo test --test frontend_conformance`
Expected: PASS (existing examples unaffected; both parsers still agree).

- [ ] **Step 6: Commit**

```bash
git add docs/superpowers/specs/grammar/tree-sitter-ascript/
git commit -m "feat(grammar): optional ; between class members; regen parser.c

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 1.3: Conformance snippet + docs + Phase-1 gate

**Files:**
- Modify: `tests/frontend_conformance.rs` (snippet catalog)
- Modify: docs language guide, `CLAUDE.md`

- [ ] **Step 1: Add a snippet to the catalog.** In `tests/frontend_conformance.rs` `interpreter_parses_each_grammar_construct`, add `"class P { x: number; y: number }"` to the `snippets` array. Run `cargo test --test frontend_conformance` → PASS.

- [ ] **Step 2: Docs.** In the language-guide page that documents statements/`;` (search `docs/content/` for where `;` "optional" is described, or the classes page), add a one-line note: `;` is an optional separator between statements AND between class members (newlines are the canonical form; the formatter normalizes to newlines). Note enums use commas.

- [ ] **Step 3: CLAUDE.md.** In the "Language notes" blockquote, add:
  > **`;` separators**: `;` is an optional statement separator (`skip_semicolons`) honored in top-level/block statement lists AND class bodies (members are self-delimiting). Enums/match-arms/params/literals are comma-delimited and do NOT take `;`. The formatter always canonicalizes to newlines.

- [ ] **Step 4: PHASE-1 GATE**

```bash
cargo test
cargo test --no-default-features
cargo clippy --all-targets
cargo clippy --no-default-features --all-targets
cargo test --test treesitter_conformance
cargo test --test frontend_conformance
```
All green/clean; counts only grow.

- [ ] **Step 5: Commit**

```bash
git add tests/frontend_conformance.rs docs/ CLAUDE.md
git commit -m "test+docs: ; in class bodies — conformance snippet, guide, CLAUDE; phase-1 gate green

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Phase 2 — REPL multi-line continuation

**Goal:** Entering an incomplete statement (e.g. `class P {`) makes the REPL show a `..` continuation prompt and buffer lines until the statement is complete, then exec the whole buffer against the persistent session env. State persistence already works (one shared `Interp`+`Environment`); this only adds front-end buffering.

**Files:**
- Modify: `src/repl.rs` — add an `is_incomplete(src)` helper; buffer in both `run_tty` and `run_piped`; `..` prompt; Ctrl-C clears the buffer.
- Test: `tests/cli.rs` (piped REPL multi-line) and/or inline `#[test]` for `is_incomplete`.

### Task 2.1: `is_incomplete` helper (delimiter-token depth + unterminated string/template)

**Files:**
- Modify: `src/repl.rs`
- Test: inline `#[test]` in `src/repl.rs`

- [ ] **Step 1: Inspect the lexer's unterminated-string/template error.** Read `src/lexer.rs` to find how it signals an unterminated `"`/`'`/`` ` `` (the error message text and/or whether its span reaches end-of-input). You need to distinguish "unterminated string at EOF" (→ incomplete) from a genuine bad-character lex error (→ report).

- [ ] **Step 2: Write the failing test.** In `src/repl.rs` tests:

```rust
#[test]
fn detects_incomplete_input() {
    // open brace → incomplete
    assert!(is_incomplete("class P {"));
    assert!(is_incomplete("fn f() {"));
    assert!(is_incomplete("let o = {"));
    assert!(is_incomplete("let a = [1,"));
    assert!(is_incomplete("print("));
    // balanced → complete (let the parser judge correctness)
    assert!(!is_incomplete("let x = 1"));
    assert!(!is_incomplete("class P { x: number }"));
    assert!(!is_incomplete("print(1 + 2)"));
    // too many closers → NOT incomplete (a real error, report it)
    assert!(!is_incomplete("}"));
    // unterminated template string → incomplete
    assert!(is_incomplete("let s = `hello"));
    // a complete template with braces inside must NOT count as unbalanced
    assert!(!is_incomplete("let s = `a ${x} b`"));
}
```

- [ ] **Step 3: Implement `is_incomplete`.** Add to `src/repl.rs`:

```rust
/// Is this REPL input an incomplete statement that should buffer more lines?
/// True only for unclosed delimiters or an unterminated string/template at EOF —
/// NOT for genuine mid-line syntax errors (those are reported immediately).
/// Counts delimiter TOKENS (not raw braces) so template `${...}` braces, which
/// live inside string/template tokens, never skew the depth.
fn is_incomplete(src: &str) -> bool {
    match crate::lexer::lex(src) {
        Ok(tokens) => {
            let mut depth: i32 = 0;
            for t in &tokens {
                match t.tok {
                    Tok::LBrace | Tok::LParen | Tok::LBracket => depth += 1,
                    Tok::RBrace | Tok::RParen | Tok::RBracket => depth -= 1,
                    _ => {}
                }
            }
            depth > 0 // positive = unclosed; negative/zero = let the parser decide
        }
        Err(e) => is_unterminated_at_eof(&e, src),
    }
}
```

Implement `is_unterminated_at_eof(&AsError, &str) -> bool` using whatever signal the lexer gives (Step 1): e.g. the error message indicates an unterminated string/template, OR the error span reaches the end of `src`. Keep it conservative — when unsure, return `false` (report the error rather than hang). Import `Tok` (`use crate::token::Tok;`).

> If `lexer::lex`/`Tok` are not already reachable from `repl.rs`, add the `use`. If `is_unterminated_at_eof` can't be implemented cleanly from the lexer's current error (e.g. it doesn't distinguish unterminated), fall back to: treat ALL lex errors whose span end `>= src.trim_end().len()` as incomplete — and note this approximation in a code comment.

- [ ] **Step 4: Run**

Run: `cargo test detects_incomplete_input`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/repl.rs
git commit -m "feat(repl): is_incomplete — delimiter-token depth + unterminated-at-EOF

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 2.2: Buffer multi-line input (piped path + tty path)

**Files:**
- Modify: `src/repl.rs` (`run_piped` and `run_tty`)
- Test: `tests/cli.rs`

- [ ] **Step 1: Write the failing integration test.** In `tests/cli.rs` (match the file's binary-invocation style), prove a multi-line class works via the piped REPL:

```rust
#[test]
fn repl_buffers_multiline_class() {
    let input = "class P {\n  x: number\n  y: number\n}\nlet p = P.from({x: 3, y: 4})\nprint(p.x + p.y)\n";
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ascript"))
        .arg("repl")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.take().unwrap().write_all(input.as_bytes())?;
            c.wait_with_output()
        })
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains('7'), "expected 7 from the multi-line class; stdout: {stdout}");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test cli repl_buffers_multiline_class`
Expected: FAIL (today each line is parsed independently; the class never defines).

- [ ] **Step 3: Implement buffering in `run_piped`.** Replace the line-at-a-time loop with a buffer that accumulates while `is_incomplete`:

```rust
async fn run_piped(interp: &Interp, env: &Environment) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut buf = String::new();
    for line in stdin.lock().lines() {
        let line = line?;
        if !buf.is_empty() { buf.push('\n'); }
        buf.push_str(&line);
        if is_incomplete(&buf) {
            continue; // need more lines
        }
        eval_line(interp, env, &buf).await;
        buf.clear();
    }
    // EOF with a leftover buffer: exec it so any error surfaces (don't swallow).
    if !buf.trim().is_empty() {
        eval_line(interp, env, &buf).await;
    }
    Ok(())
}
```

- [ ] **Step 4: Implement buffering in `run_tty`.** Accumulate across `readline` calls, switching the prompt to `..` while buffering; Ctrl-C clears the buffer (return to `>> ` rather than exit when a partial buffer exists):

```rust
async fn run_tty(interp: &Interp, env: &Environment) -> std::io::Result<()> {
    use rustyline::error::ReadlineError;
    use rustyline::DefaultEditor;
    let mut rl = DefaultEditor::new().map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut buf = String::new();
    loop {
        let prompt = if buf.is_empty() { ">> " } else { ".. " };
        match rl.readline(prompt) {
            Ok(line) => {
                if !buf.is_empty() { buf.push('\n'); }
                buf.push_str(&line);
                if is_incomplete(&buf) { continue; }
                let _ = rl.add_history_entry(buf.as_str());
                eval_line(interp, env, &buf).await;
                buf.clear();
            }
            // Ctrl-C: if mid-buffer, cancel the buffer and return to the main prompt;
            // otherwise exit.
            Err(ReadlineError::Interrupted) => {
                if buf.is_empty() { break; } else { buf.clear(); continue; }
            }
            Err(ReadlineError::Eof) => break,
            Err(e) => return Err(std::io::Error::other(e.to_string())),
        }
    }
    Ok(())
}
```

> Match the existing `run_tty` signature/imports; reuse the file's existing `eval_line`. Keep `add_history_entry` behavior (history the whole buffered statement).

- [ ] **Step 5: Run the integration test + a no-regression single-line check**

Run: `cargo test --test cli repl_buffers_multiline_class`
Expected: PASS.

Also confirm single-line REPL still works (state persistence + bare-expression echo) — run the existing repl test(s): `cargo test repl`.

- [ ] **Step 6: Commit**

```bash
git add src/repl.rs tests/cli.rs
git commit -m "feat(repl): buffer incomplete input across lines (.. prompt; Ctrl-C cancels)

State already persists via the shared session env; this adds front-end buffering
so multi-line constructs (class/fn/object) can be entered interactively.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 2.3: Docs + Phase-2 gate

- [ ] **Step 1: Docs.** In the docs page (or README section) that mentions the REPL, add: the REPL keeps session state across lines (variables/functions/imports persist) and now supports multi-line input — an incomplete statement (unclosed `{([` or an open template) continues on a `..` prompt until complete; Ctrl-C cancels a partial entry.

- [ ] **Step 2: CLAUDE.md.** Add to the REPL/architecture notes:
  > **REPL multi-line input**: `repl.rs` buffers lines while `is_incomplete` (positive delimiter-TOKEN depth, or unterminated string/template at EOF) on a `..` prompt, then execs the whole buffer against the persistent session `Interp`+`Environment` (state already persisted across lines). Token-depth (not raw-brace) counting keeps `${…}` template braces from skewing the depth.

- [ ] **Step 3: Manual smoke test** (tty path can't be fully automated, but verify the piped path + a manual note):

```bash
cargo build --release
printf 'class P {\n  x: number\n  y: number\n}\nlet p = P.from({x:10, y:20})\nprint(p.x + p.y)\n' | ./target/release/ascript repl
# expect: 30
printf 'fn add(a, b) {\n  return a + b\n}\nprint(add(2, 3))\n' | ./target/release/ascript repl
# expect: 5
```

- [ ] **Step 4: PHASE-2 GATE**

```bash
cargo test
cargo test --no-default-features
cargo clippy --all-targets
cargo clippy --no-default-features --all-targets
cargo test --test cli
```
All green/clean; counts only grow.

- [ ] **Step 5: Commit**

```bash
git add docs/ CLAUDE.md README.md
git commit -m "docs: REPL multi-line continuation + state persistence; phase-2 gate green

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Final integration pass

- [ ] **Step 1: Spec update.** In `docs/superpowers/specs/2026-05-29-ascript-design.md`, add a short note where statement separators / the REPL are specified: `;` is an optional separator in statement lists and class bodies (not comma-lists); the REPL persists session state and supports multi-line continuation.
- [ ] **Step 2: Whole-suite + lints, both configs** (`cargo test`, `cargo test --no-default-features`, `cargo clippy --all-targets`, `cargo clippy --no-default-features --all-targets`) — all green/clean.
- [ ] **Step 3: Manual REPL confirmation** of a multi-line class and a one-line `;` class (piped).
- [ ] **Step 4: Finish the branch** via `superpowers:finishing-a-development-branch` (merge `--no-ff` per the established workflow).

---

## Self-Review

**Spec coverage:** `;` in class bodies (Task 1.1 parser, 1.2 grammar, 1.3 conformance+docs); enum exclusion documented (1.3, justified by comma-delimited variants); REPL incomplete-detection (2.1), buffering both tty+piped (2.2), docs (2.3). REPL state persistence is pre-existing — documented, not re-implemented. ✅

**Placeholder scan:** every step has concrete code/commands; the one approximation (lexer unterminated-error detection) has an explicit documented fallback, not a TODO. ✅

**Type/name consistency:** `is_incomplete(src: &str) -> bool`, `is_unterminated_at_eof(&AsError, &str) -> bool`, `skip_semicolons`, `eval_line`, `run_tty`/`run_piped`, buffer var `buf` — used identically across tasks. ✅
