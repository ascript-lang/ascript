# Destructuring, Spread, Rest, Print Streaming & Structured Logging — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add object destructuring (`let {a as b} = obj`), the spread operator (`...` in arrays/objects/calls), rest collectors (`...rest` params + array/object rest patterns), live-streaming `print`, and a structured `std/log` module — each landing production-grade with no regressions.

**Architecture:** Five dependency-ordered phases. Each touches the shared front-end (`lexer`→`parser`→`ast`) plus the mirrors that consume it (`interp`, `fmt`, `lsp`, the vendored tree-sitter grammar) and ships docs + a runnable `.as` example. The `...` token is introduced in Phase 2 and reused by Phase 3; the `OutputSink` from Phase 4 is reused by Phase 5.

**Tech Stack:** Rust (async tree-walking interpreter, `Rc`/`RefCell`, `#[async_recursion]`, tokio current-thread + `LocalSet`), tree-sitter (`--abi 14`), `serde_json` (for the logging serializer), `indexmap`.

**Phase order (dependency-correct):**
1. Object destructuring (independent; no `...` needed)
2. `...` token + spread (lands the lexer token; typed-element AST)
3. Rest collectors (params + array-rest + object-rest; reuse `...` token & Phase-1 AST)
4. Print streaming (`OutputSink`; also fixes panic-drops-output bug)
5. Structured logging (`std/log`; built on Phase-4 sink)

**Decisions locked (from design session):**
- Object destructuring: `as` rename (soft keyword), keys are `Ident | Str` (quote to escape keyword/non-ident keys), missing key → `nil`, works on `Value::Object` **and** `Value::Instance`.
- Rest-exclusion in object patterns uses **source keys**; empty rest is `[]`/`{}`, never `nil`.
- Spread: typed-element AST (unrepresentable where invalid); **strict** — no array↔object coercion; object-spread is later-value-wins with `IndexMap` first-seen position.
- Rest params: typed as `...rest: array<T>` (array-type spelling, per-element checked); bare `...rest` ⇒ `array<any>`. Fast-path branch keeps non-rest calls byte-identical.
- Logging: `Interp`-stateful (`log_level` + sink), levels `debug/info/warn/error`, record = first string → `msg` + object args merged as fields + auto `level`/`ts`, format selectable (default human, JSON-lines opt-in), total lossy serializer reusing `json::from_ascript`, thunk support, stderr via shared sink.
- Print: `OutputSink::{Live, Capture}` selected per entry point (`run_file`→Live, `run_source`→Capture); fixes the existing bug where output before a panic is dropped.

---

## Conventions used in every phase

- **Commit trailer (required):** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`
- **Tree-sitter regen:** after any `grammar.js` change, run from the grammar dir:
  `cd docs/superpowers/specs/grammar/tree-sitter-ascript && tree-sitter generate --abi 14`
  then `cargo build` recompiles the vendored `parser.c` via `build.rs`.
- **ExprKind/Stmt/Type rule:** any new/changed `ExprKind` (or `Stmt`/`Type`) variant needs arms in **`interp.rs` (eval/exec), `fmt.rs` (`write_expr_inner`/`write_stmt`), and `ast.rs` (`Display`)**. The compiler's exhaustiveness check is the safety net — a missing arm fails to build.
- **Two-config rule:** `cargo test` and `cargo test --no-default-features` must both pass; `cargo clippy --all-targets` and `cargo clippy --no-default-features --all-targets` must both be clean.

---

## Phase 1 — Object Destructuring

**Goal:** `let {a, b as local, "weird key" as wk} = obj` binds names from an `Object` or `Instance`; missing keys bind `nil`.

**Files:**
- Modify: `src/ast.rs` — add `Stmt::LetDestructureObject` + `ObjBinding`; add `Display`.
- Modify: `src/parser.rs` — `let_stmt`: add `Tok::LBrace` branch; add `obj_pattern` helper.
- Modify: `src/interp.rs` — `exec`: add `Stmt::LetDestructureObject` arm; `declared_names` helper at line ~2068.
- Modify: `src/fmt.rs` — `write_stmt`: add arm.
- Modify: `src/lsp/analysis.rs` — three `LetDestructure` sites: add object arm.
- Modify: `docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js` — `object_pattern` in `_binding_target`.
- Create: `examples/object_destructuring.as`
- Modify: `docs/content/language/*.md` (destructuring section), `README.md`, `CLAUDE.md`.

### Task 1.1: AST node for object destructuring

- [ ] **Step 1: Write the failing test** — add to `src/parser.rs` `#[cfg(test)]` module (near `parses_array_destructuring_let`):

```rust
#[test]
fn parses_object_destructuring_shorthand_and_rename() {
    let p = parse(&lex("let {a, b as local} = obj").unwrap()).unwrap();
    match &p[0] {
        Stmt::LetDestructureObject { bindings, mutable, .. } => {
            assert!(*mutable);
            assert_eq!(bindings[0].key, "a");
            assert_eq!(bindings[0].binding, "a");
            assert_eq!(bindings[1].key, "b");
            assert_eq!(bindings[1].binding, "local");
        }
        other => panic!("expected LetDestructureObject, got {other:?}"),
    }
}

#[test]
fn parses_object_destructuring_quoted_key() {
    let p = parse(&lex(r#"let {"weird key" as wk} = obj"#).unwrap()).unwrap();
    match &p[0] {
        Stmt::LetDestructureObject { bindings, .. } => {
            assert_eq!(bindings[0].key, "weird key");
            assert_eq!(bindings[0].binding, "wk");
        }
        other => panic!("expected LetDestructureObject, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test parses_object_destructuring -- --nocapture`
Expected: FAIL — `no variant LetDestructureObject` (compile error).

- [ ] **Step 3: Add the AST node.** In `src/ast.rs`, add the struct next to `Param`:

```rust
/// One `{key as binding}` entry in an object-destructuring pattern. `key` is the
/// SOURCE key looked up in the value; `binding` is the local name introduced
/// (equal to `key` for the shorthand `{key}`). `key_span` covers the key token,
/// `binding_span` the local name (they coincide for shorthand).
#[derive(Debug, Clone, PartialEq)]
pub struct ObjBinding {
    pub key: String,
    pub binding: String,
    pub key_span: Span,
    pub binding_span: Span,
}
```

Add the `Stmt` variant next to `LetDestructure`:

```rust
    /// `let {a, b as local} = expr` — object destructuring (binds by key name).
    /// `rest` is the optional `...name` collector (Phase 3); `None` in Phase 1.
    LetDestructureObject {
        bindings: Vec<ObjBinding>,
        rest: Option<(String, Span)>,
        value: Expr,
        mutable: bool,
        span: Span,
    },
```

- [ ] **Step 4: Run to verify the test now reaches the parser (still fails, parser unchanged)**

Run: `cargo test parses_object_destructuring -- --nocapture`
Expected: FAIL — parser still produces a different `Stmt` (the test panics in its match arm). Good: the type now exists.

- [ ] **Step 5: Commit**

```bash
git add src/ast.rs src/parser.rs
git commit -m "feat(ast): LetDestructureObject + ObjBinding for object destructuring

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 1.2: Parse the object pattern

- [ ] **Step 1: Implement the parser branch.** In `src/parser.rs` `let_stmt`, immediately after the existing `if *self.peek() == Tok::LBracket { ... }` array-destructuring block (which ends with `return Ok(Stmt::LetDestructure { ... });`), add:

```rust
        // `let {a, b as local} = expr` — object destructuring binding.
        if *self.peek() == Tok::LBrace {
            self.advance(); // consume '{'
            let mut bindings = Vec::new();
            let rest: Option<(String, Span)> = None; // populated in Phase 3
            if *self.peek() != Tok::RBrace {
                loop {
                    // key := Ident | Str  (mirrors object-literal keys)
                    let key_span = self.span();
                    let key = match self.advance() {
                        Tok::Ident(n) => n,
                        Tok::Str(s) => s,
                        other => return Err(AsError::at(
                            format!("expected a key in object pattern, found {:?}", other),
                            self.tokens[self.pos - 1].span,
                        )),
                    };
                    // optional `as binding`
                    let (binding, binding_span) =
                        if matches!(self.peek(), Tok::Ident(s) if s == "as") {
                            self.advance(); // consume soft `as`
                            let bspan = self.span();
                            match self.advance() {
                                Tok::Ident(b) => (b, bspan),
                                other => return Err(AsError::at(
                                    format!("expected a local name after 'as', found {:?}", other),
                                    self.tokens[self.pos - 1].span,
                                )),
                            }
                        } else {
                            // shorthand: key must be a valid identifier to bind directly
                            if !is_ident_like(&key) {
                                return Err(AsError::at(
                                    format!("key {:?} is not a valid binding name; use `as`", key),
                                    key_span,
                                ));
                            }
                            (key.clone(), key_span)
                        };
                    bindings.push(crate::ast::ObjBinding { key, binding, key_span, binding_span });
                    if *self.peek() == Tok::Comma {
                        self.advance();
                        if *self.peek() == Tok::RBrace { break; }
                    } else {
                        break;
                    }
                }
            }
            self.eat(&Tok::RBrace)?;
            self.eat(&Tok::Eq)?;
            let value = self.expr()?;
            let span = Span::new(start, self.prev_end());
            return Ok(Stmt::LetDestructureObject { bindings, rest, value, mutable, span });
        }
```

Add this free helper near the bottom of `src/parser.rs` (module scope, not inside `impl`):

```rust
/// A string is "identifier-like" if it can be a bare binding name: starts with a
/// letter or `_`, rest are alphanumeric or `_`. Used to reject `{"weird key"}`
/// shorthand (which has no valid local name) while allowing `{a}`.
fn is_ident_like(s: &str) -> bool {
    let mut cs = s.chars();
    match cs.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    cs.all(|c| c.is_alphanumeric() || c == '_')
}
```

- [ ] **Step 2: Run the parse tests**

Run: `cargo test parses_object_destructuring`
Expected: PASS (both tests).

- [ ] **Step 3: Add a guard test for invalid shorthand and run it**

```rust
#[test]
fn object_destructuring_quoted_shorthand_is_error() {
    let r = parse(&lex(r#"let {"weird key"} = obj"#).unwrap());
    assert!(r.is_err(), "quoted non-ident key needs `as`");
}
```

Run: `cargo test object_destructuring_quoted_shorthand_is_error`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/parser.rs
git commit -m "feat(parser): object destructuring pattern (Ident|Str keys, as-rename)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 1.3: Evaluate object destructuring (Object + Instance, missing → nil)

- [ ] **Step 1: Write the failing interp test.** In `src/interp.rs` tests, add:

```rust
#[tokio::test]
async fn object_destructuring_binds_from_object_and_instance() {
    let out = eval_program(
        r#"
let {a, b as local, missing} = {a: 1, b: 2}
print(a)
print(local)
print(missing)

class P { x: number; y: number }
let p = P.from({x: 10, y: 20})
let {x, y} = p
print(x)
print(y)
"#,
    ).await;
    assert_eq!(out, "1\n2\nnil\n10\n20\n");
}
```

> Use the file's existing program-eval test helper (search for an existing `eval_program`/`run_program`/`eval_to_output` helper used by neighbouring `#[tokio::test]`s; reuse the exact name).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test object_destructuring_binds_from_object_and_instance`
Expected: FAIL — non-exhaustive match in `exec` (compile error: `Stmt::LetDestructureObject` not covered).

- [ ] **Step 3: Implement the exec arm.** In `src/interp.rs` `exec`, right after the existing `Stmt::LetDestructure { .. } => { ... }` arm, add:

```rust
            Stmt::LetDestructureObject { bindings, rest, value, mutable, .. } => {
                let v = self.eval_expr(value, env).await?;
                // Read a key from either an Object or a class Instance; missing → nil.
                let get = |key: &str| -> Value {
                    match &v {
                        Value::Object(o) => o.borrow().get(key).cloned().unwrap_or(Value::Nil),
                        Value::Instance(i) => i.borrow().fields.get(key).cloned().unwrap_or(Value::Nil),
                        _ => Value::Nil,
                    }
                };
                if !matches!(v, Value::Object(_) | Value::Instance(_)) {
                    return Err(AsError::at(
                        format!("cannot destructure a non-object value of type {}", type_name(&v)),
                        value.span,
                    ).into());
                }
                for b in bindings {
                    env.define(&b.binding, get(&b.key), *mutable).map_err(AsError::new)?;
                }
                // Phase 3 populates `rest`; nil here.
                if let Some((rest_name, _)) = rest {
                    let bound: std::collections::HashSet<&str> =
                        bindings.iter().map(|b| b.key.as_str()).collect();
                    let mut remaining = indexmap::IndexMap::new();
                    match &v {
                        Value::Object(o) => for (k, val) in o.borrow().iter() {
                            if !bound.contains(k.as_str()) { remaining.insert(k.clone(), val.clone()); }
                        },
                        Value::Instance(i) => for (k, val) in i.borrow().fields.iter() {
                            if !bound.contains(k.as_str()) { remaining.insert(k.clone(), val.clone()); }
                        },
                        _ => {}
                    }
                    let obj = Value::Object(std::rc::Rc::new(std::cell::RefCell::new(remaining)));
                    env.define(rest_name, obj, *mutable).map_err(AsError::new)?;
                }
                Ok(Flow::Normal)
            }
```

> The `rest` block is written now (so Phase 3 only flips the parser) but is dead until Phase 3 sets `rest = Some(..)`. It is fully correct, not a stub.

- [ ] **Step 4: Cover the `declared_names` helper.** At `src/interp.rs:2068` the `Stmt::LetDestructure { names, .. } => names.clone()` arm lives in a helper that lists names a statement declares (used for hoisting/scoping). Add immediately after it:

```rust
        Stmt::LetDestructureObject { bindings, rest, .. } => {
            let mut v: Vec<String> = bindings.iter().map(|b| b.binding.clone()).collect();
            if let Some((r, _)) = rest { v.push(r.clone()); }
            v
        }
```

- [ ] **Step 5: Run the interp test**

Run: `cargo test object_destructuring_binds_from_object_and_instance`
Expected: PASS.

- [ ] **Step 6: Add the non-object error test and run it**

```rust
#[tokio::test]
async fn object_destructuring_on_non_object_panics() {
    let err = eval_program_expect_panic(r#"let {a} = 5"#).await;
    assert!(err.contains("cannot destructure a non-object"));
}
```

> Reuse the neighbouring panic-asserting helper (search for one that returns the panic message string).

Run: `cargo test object_destructuring_on_non_object_panics`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/interp.rs
git commit -m "feat(interp): eval object destructuring over Object/Instance, missing->nil

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 1.4: Formatter

- [ ] **Step 1: Write the failing fmt round-trip test.** In `src/fmt.rs` tests:

```rust
#[test]
fn fmt_object_destructuring_roundtrips() {
    let src = "let {a, b as local} = obj\n";
    assert_eq!(format_str(src), src);
    let src2 = "const {\"weird key\" as wk} = obj\n";
    assert_eq!(format_str(src2), src2);
}
```

> Use the file's existing format helper name (search for `format_str`/`fmt_str`/`format_source` used by neighbouring tests).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test fmt_object_destructuring_roundtrips`
Expected: FAIL — non-exhaustive `write_stmt` (compile error).

- [ ] **Step 3: Implement the fmt arm.** In `src/fmt.rs` `write_stmt`, after the `Stmt::LetDestructure { .. }` arm:

```rust
        Stmt::LetDestructureObject { bindings, rest, value, mutable, .. } => {
            indent(out, level);
            out.push_str(if *mutable { "let " } else { "const " });
            out.push('{');
            for (i, b) in bindings.iter().enumerate() {
                if i > 0 { out.push_str(", "); }
                // quote keys that are not identifier-like (mirror the parser rule)
                if b.key.chars().next().map_or(false, |c| c.is_alphabetic() || c == '_')
                    && b.key.chars().all(|c| c.is_alphanumeric() || c == '_')
                {
                    out.push_str(&b.key);
                } else {
                    out.push('"');
                    out.push_str(&b.key);
                    out.push('"');
                }
                if b.binding != b.key {
                    out.push_str(" as ");
                    out.push_str(&b.binding);
                }
            }
            if let Some((r, _)) = rest {
                if !bindings.is_empty() { out.push_str(", "); }
                out.push_str("...");
                out.push_str(r);
            }
            out.push_str("} = ");
            write_expr(out, value, 0);
            out.push('\n');
        }
```

- [ ] **Step 4: Run the fmt test**

Run: `cargo test fmt_object_destructuring_roundtrips`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/fmt.rs
git commit -m "feat(fmt): format object destructuring patterns

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 1.5: LSP symbols

- [ ] **Step 1: Add the LSP arms.** `src/lsp/analysis.rs` has three `Stmt::LetDestructure` sites (symbol collection, find-decl, top-level decls). After each, add the object analogue. Symbol-collection site:

```rust
        Stmt::LetDestructureObject { bindings, rest, mutable, span, .. } => {
            let kind = if *mutable { SymbolKind::VARIABLE } else { SymbolKind::CONSTANT };
            for b in bindings {
                out.push(symbol(b.binding.clone(), kind, span_range(*span, index), span_range(b.binding_span, index), None));
            }
            if let Some((r, rspan)) = rest {
                out.push(symbol(r.clone(), kind, span_range(*span, index), span_range(*rspan, index), None));
            }
        }
```

Find-decl-name-span site (mirrors the array version's loop):

```rust
        Stmt::LetDestructureObject { bindings, rest, .. } => {
            for b in bindings {
                if b.binding == name && b.binding_span.start <= offset { found = Some(b.binding_span); }
            }
            if let Some((r, rspan)) = rest {
                if r == name && rspan.start <= offset { found = Some(*rspan); }
            }
        }
```

Top-level-decl site:

```rust
        Stmt::LetDestructureObject { bindings, rest, .. } => {
            for b in bindings {
                if b.binding == name { *out = Some(b.binding_span); return; }
            }
            if let Some((r, rspan)) = rest {
                if r == name { *out = Some(*rspan); return; }
            }
        }
```

> Match the exact variable names (`found`, `out`, `offset`, `index`) used by the adjacent array arms; copy their shape.

- [ ] **Step 2: Build the lsp feature**

Run: `cargo build --features lsp` then `cargo test --features lsp lsp`
Expected: builds clean; existing lsp tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/lsp/analysis.rs
git commit -m "feat(lsp): symbols + go-to-def for object destructuring bindings

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 1.6: Tree-sitter grammar

- [ ] **Step 1: Edit `grammar.js`.** Find `_binding_target` and `array_pattern`; add `object_pattern`:

```javascript
    _binding_target: $ => choice($.identifier, $.array_pattern, $.object_pattern),
    object_pattern: $ => seq(
      '{',
      commaSep($.object_pattern_entry),
      optional(','),
      '}',
    ),
    object_pattern_entry: $ => seq(
      field('key', choice($.identifier, $.string)),
      optional(seq('as', field('binding', $.identifier))),
    ),
```

- [ ] **Step 2: Regenerate and build**

Run:
```bash
cd docs/superpowers/specs/grammar/tree-sitter-ascript && tree-sitter generate --abi 14 && cd -
cargo build
```
Expected: generates without conflict errors; build succeeds.

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/grammar/tree-sitter-ascript/
git commit -m "feat(grammar): object_pattern in binding targets; regen parser.c

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 1.7: Example + docs + Phase-1 verification gate

- [ ] **Step 1: Create `examples/object_destructuring.as`:**

```javascript
// Object destructuring: shorthand, rename with `as`, quoted keys, missing → nil.
let user = {name: "Ada", role: "admin", "login count": 42}

let {name, role as r} = user
print(name)           // Ada
print(r)              // admin

let {"login count" as logins, missing} = user
print(logins)         // 42
print(missing)        // nil

// Works on class instances too.
class Point { x: number; y: number }
let p = Point.from({x: 3, y: 4})
let {x, y} = p
print(x + y)          // 7
```

- [ ] **Step 2: Run the example**

Run: `cargo build --release && ./target/release/ascript run examples/object_destructuring.as`
Expected output:
```
Ada
admin
42
nil
7
```

- [ ] **Step 3: Conformance — both parsers accept the example**

Run: `cargo test --test treesitter_conformance && cargo test --test frontend_conformance`
Expected: PASS (the new example is picked up automatically).

- [ ] **Step 4: Update docs.** In the language-guide destructuring page under `docs/content/language/` (the page that documents `let [a, b] = ...`), add an "Object destructuring" subsection: shorthand `{a}`, rename `{a as b}`, quoted keys `{"k" as v}`, missing→nil, valid on objects and instances. Add a one-line mention in `README.md`'s language-features list. Remove no existing content.

- [ ] **Step 5: Update `CLAUDE.md`.** In the "Language notes worth knowing" blockquote, add one line:
  > **Object destructuring**: `let {a, b as local, "k" as v} = obj` binds by key from an `Object` or `Instance` (`Stmt::LetDestructureObject`); missing keys bind `nil`. Keys are `Ident | Str` (quote non-identifier keys); rename with the soft keyword `as`.

- [ ] **Step 6: PHASE-1 VERIFICATION GATE — run every check and confirm output:**

```bash
cargo test                                   # full suite green
cargo test --no-default-features             # core-only green
cargo clippy --all-targets                   # clean
cargo clippy --no-default-features --all-targets  # clean
cargo run -- fmt examples/object_destructuring.as && git diff --exit-code examples/object_destructuring.as  # formatter is idempotent on the example
cargo test --test treesitter_conformance
cargo test --test frontend_conformance
./target/release/ascript run examples/object_destructuring.as   # matches expected output above
```

- [ ] **Step 7: Blast-radius / regression review (record findings in the commit body).** Confirm: (a) `Stmt` is an enum so every `match` over it was forced to compile (no silent fall-through); search `grep -rn "Stmt::LetDestructure\b" src/` and confirm every site that handles the array form now also handles the object form or explicitly doesn't need to. (b) No change to the array-destructuring path → its tests unchanged. (c) `let {` was previously a hard parse error → purely additive.

- [ ] **Step 8: Commit the gate**

```bash
git add examples/object_destructuring.as docs/ README.md CLAUDE.md
git commit -m "docs+test: object destructuring example, guide, README, CLAUDE; phase-1 gate green

Blast radius: additive only (let { was a parse error pre-change). Array
destructuring path untouched. Both feature configs + clippy + conformance green.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Phase 2 — `...` Token + Spread Operator

**Goal:** `[...a, b]`, `{...o, k: v}`, `f(...args)` — spread in array literals, object literals, and call arguments. Strict (no cross-type coercion).

**Files:**
- Modify: `src/token.rs` — add `Tok::DotDotDot`.
- Modify: `src/lexer.rs` — emit `DotDotDot` (3-dot, before the 2-dot `DotDot`).
- Modify: `src/ast.rs` — change `ExprKind::Array`/`Object`/`Call` element types to typed-element enums; add `Display`.
- Modify: `src/parser.rs` — array/object/call parsing to accept spread elements.
- Modify: `src/interp.rs` — eval the three contexts (flatten/merge/expand).
- Modify: `src/fmt.rs` — `write_expr_inner` for the three contexts.
- Modify: `grammar.js` — spread in array/object/arguments.
- Create: `examples/spread.as`; docs/README/CLAUDE updates.

### Task 2.1: Lexer token

- [ ] **Step 1: Write the failing lexer test.** In `src/lexer.rs` tests:

```rust
#[test]
fn lexes_triple_dot_as_spread_not_two_dots() {
    let toks = lex("...x").unwrap();
    assert_eq!(toks[0].tok, Tok::DotDotDot);
    assert!(matches!(toks[1].tok, Tok::Ident(ref s) if s == "x"));
    // ensure `..` still lexes as DotDot (range), unaffected
    let r = lex("0..5").unwrap();
    assert_eq!(r[1].tok, Tok::DotDot);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test lexes_triple_dot_as_spread_not_two_dots`
Expected: FAIL — `Tok::DotDotDot` undefined (compile error).

- [ ] **Step 3: Add the token.** In `src/token.rs`, add `DotDotDot,` next to `DotDot`. In `src/lexer.rs`, update the `'.'` arm to check three dots first (maximal munch):

```rust
                '.' => {
                    if i + 2 < chars.len() && chars[i + 1] == '.' && chars[i + 2] == '.' {
                        tokens.push(Token { tok: Tok::DotDotDot, span: Span::new(start, start + 3) });
                        i += 3;
                    } else if i + 1 < chars.len() && chars[i + 1] == '.' {
                        tokens.push(Token { tok: Tok::DotDot, span: Span::new(start, start + 2) });
                        i += 2;
                    } else {
                        tokens.push(Token { tok: Tok::Dot, span: Span::new(start, start + 1) });
                        i += 1;
                    }
                }
```

- [ ] **Step 4: Run**

Run: `cargo test lexes_triple_dot_as_spread_not_two_dots`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/token.rs src/lexer.rs
git commit -m "feat(lexer): DotDotDot token (... before ..)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 2.2: Typed-element AST (the refactor)

- [ ] **Step 1: Add the element enums to `src/ast.rs`:**

```rust
/// An element of an array literal: a plain item or a `...spread`.
#[derive(Debug, Clone, PartialEq)]
pub enum ArrayElem {
    Item(Expr),
    Spread(Expr),
}

/// An entry of an object literal: a `key: value` pair or a `...spread`.
#[derive(Debug, Clone, PartialEq)]
pub enum ObjEntry {
    KV(String, Expr),
    Spread(Expr),
}

/// A call argument: positional or a `...spread`.
#[derive(Debug, Clone, PartialEq)]
pub enum CallArg {
    Pos(Expr),
    Spread(Expr),
}
```

Change the three `ExprKind` variants:

```rust
    Call { callee: Box<Expr>, args: Vec<CallArg> },
    Array(Vec<ArrayElem>),
    Object(Vec<ObjEntry>),
```

- [ ] **Step 2: Update `ast.rs` `Display`.** In the `ExprKind` `Display` impl, replace the `Array`, `Object`, `Call` arms:

```rust
            ExprKind::Array(elems) => {
                write!(f, "[")?;
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    match e {
                        ArrayElem::Item(x) => write!(f, "{}", x)?,
                        ArrayElem::Spread(x) => write!(f, "...{}", x)?,
                    }
                }
                write!(f, "]")
            }
            ExprKind::Object(entries) => {
                write!(f, "{{")?;
                for (i, e) in entries.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    match e {
                        ObjEntry::KV(k, v) => write!(f, "{}: {}", k, v)?,
                        ObjEntry::Spread(x) => write!(f, "...{}", x)?,
                    }
                }
                write!(f, "}}")
            }
            ExprKind::Call { callee, args } => {
                write!(f, "{}(", callee)?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    match a {
                        CallArg::Pos(x) => write!(f, "{}", x)?,
                        CallArg::Spread(x) => write!(f, "...{}", x)?,
                    }
                }
                write!(f, ")")
            }
```

- [ ] **Step 3: Build to find every consumer (compiler-driven blast radius).**

Run: `cargo build 2>&1 | grep -E "error|src/" | head -40`
Expected: errors at each `ExprKind::Array(_)` / `Object(_)` / `Call{..}` construction & match — in `parser.rs`, `interp.rs`, `fmt.rs`, possibly `repl.rs`/`lsp`. This list IS the blast radius; the next tasks fix each. Do not commit yet (broken build).

> This step intentionally leaves the build red; Tasks 2.3–2.5 make it green. Keep them in one working session.

### Task 2.3: Parse spread in the three contexts

- [ ] **Step 1: Array literal.** In `src/parser.rs` `primary`'s `Tok::LBracket` arm, replace `items.push(self.expr()?);` loop body so each element checks for spread:

```rust
                        if *self.peek() == Tok::DotDotDot {
                            self.advance();
                            items.push(crate::ast::ArrayElem::Spread(self.expr()?));
                        } else {
                            items.push(crate::ast::ArrayElem::Item(self.expr()?));
                        }
```
and change the constructor to `ExprKind::Array(items)` (now `Vec<ArrayElem>`).

- [ ] **Step 2: Object literal.** In the `Tok::LBrace` arm, before reading a key, check for spread:

```rust
                        if *self.peek() == Tok::DotDotDot {
                            self.advance();
                            entries.push(crate::ast::ObjEntry::Spread(self.expr()?));
                        } else {
                            let key = match self.advance() {
                                Tok::Ident(name) => name,
                                Tok::Str(s) => s,
                                other => return Err(AsError::at(
                                    format!("expected object key, found {:?}", other),
                                    self.tokens[self.pos - 1].span)),
                            };
                            self.eat(&Tok::Colon)?;
                            let value = self.expr()?;
                            entries.push(crate::ast::ObjEntry::KV(key, value));
                        }
```
Constructor becomes `ExprKind::Object(entries)` (now `Vec<ObjEntry>`).

- [ ] **Step 3: Call arguments.** In the call-arguments parsing (`Tok::LParen` postfix args), replace `args.push(self.expr()?);`:

```rust
                        if *self.peek() == Tok::DotDotDot {
                            self.advance();
                            args.push(crate::ast::CallArg::Spread(self.expr()?));
                        } else {
                            args.push(crate::ast::CallArg::Pos(self.expr()?));
                        }
```

- [ ] **Step 4: Add parse tests** in `src/parser.rs`:

```rust
#[test]
fn parses_spread_in_array_object_call() {
    assert!(parse(&lex("let a = [...x, 1]").unwrap()).is_ok());
    assert!(parse(&lex("let o = {...x, k: 1}").unwrap()).is_ok());
    assert!(parse(&lex("f(...args, 2)").unwrap()).is_ok());
}
```

(Will compile-pass only after Task 2.4/2.5 fix interp/fmt; run in Step of 2.5.)

- [ ] **Step 5: Commit (after build is green at end of 2.5).**

### Task 2.4: Evaluate spread (strict)

- [ ] **Step 1: Array eval.** In `src/interp.rs` `ExprKind::Array` arm:

```rust
            ExprKind::Array(elems) => {
                let mut values = Vec::with_capacity(elems.len());
                for e in elems {
                    match e {
                        crate::ast::ArrayElem::Item(x) => values.push(self.eval_expr(x, env).await?),
                        crate::ast::ArrayElem::Spread(x) => {
                            let v = self.eval_expr(x, env).await?;
                            match v {
                                Value::Array(a) => values.extend(a.borrow().iter().cloned()),
                                other => return Err(AsError::at(
                                    format!("can only spread an array into an array, got {}", type_name(&other)),
                                    x.span).into()),
                            }
                        }
                    }
                }
                Ok(Value::Array(std::rc::Rc::new(std::cell::RefCell::new(values))))
            }
```

- [ ] **Step 2: Object eval (later-wins, IndexMap keeps first-seen position).**

```rust
            ExprKind::Object(entries) => {
                let mut map = indexmap::IndexMap::with_capacity(entries.len());
                for e in entries {
                    match e {
                        crate::ast::ObjEntry::KV(k, v) => {
                            let value = self.eval_expr(v, env).await?;
                            map.insert(k.clone(), value); // IndexMap::insert keeps original position, updates value
                        }
                        crate::ast::ObjEntry::Spread(x) => {
                            let v = self.eval_expr(x, env).await?;
                            match v {
                                Value::Object(o) => for (k, val) in o.borrow().iter() {
                                    map.insert(k.clone(), val.clone());
                                },
                                other => return Err(AsError::at(
                                    format!("can only spread an object into an object, got {}", type_name(&other)),
                                    x.span).into()),
                            }
                        }
                    }
                }
                Ok(Value::Object(std::rc::Rc::new(std::cell::RefCell::new(map))))
            }
```

- [ ] **Step 3: Call eval.** In `ExprKind::Call`, replace the arg-collection loop:

```rust
                let mut values = Vec::new();
                for a in args {
                    match a {
                        crate::ast::CallArg::Pos(x) => values.push(self.eval_expr(x, env).await?),
                        crate::ast::CallArg::Spread(x) => {
                            let v = self.eval_expr(x, env).await?;
                            match v {
                                Value::Array(arr) => values.extend(arr.borrow().iter().cloned()),
                                other => return Err(AsError::at(
                                    format!("can only spread an array as call arguments, got {}", type_name(&other)),
                                    x.span).into()),
                            }
                        }
                    }
                }
```

### Task 2.5: Formatter + make build green

- [ ] **Step 1: fmt arms.** In `src/fmt.rs` `write_expr_inner`, update the `Array`, `Object`, `Call` arms to the typed-element forms (mirror the `Display` shape from Task 2.2, using `write_expr` for sub-exprs and a `"..."` prefix on spreads). Keep existing spacing/precedence handling.

- [ ] **Step 2: Build green**

Run: `cargo build`
Expected: SUCCESS (all blast-radius sites fixed).

- [ ] **Step 3: Run parse + add eval/fmt tests:**

```rust
#[tokio::test]
async fn spread_array_object_call_eval() {
    let out = eval_program(r#"
let a = [1, 2]
let b = [...a, 3]
print(b)
let o = {x: 1}
let p = {...o, y: 2, x: 9}
print(p)
fn add(a, b, c) { return a + b + c }
print(add(...[1, 2, 3]))
"#).await;
    assert_eq!(out, "[1, 2, 3]\n{x: 9, y: 2}\n6\n");
}

#[tokio::test]
async fn spread_wrong_type_panics() {
    assert!(eval_program_expect_panic("let a = [...5]").await.contains("can only spread an array"));
    assert!(eval_program_expect_panic("let o = {...5}").await.contains("can only spread an object"));
}

#[test]
fn fmt_spread_roundtrips() {
    for src in ["let a = [...x, 1]\n", "let o = {...x, k: 1}\n", "f(...args, 2)\n"] {
        assert_eq!(format_str(src), src);
    }
}
```

Run: `cargo test spread`
Expected: PASS (eval, panic, parse, fmt).

- [ ] **Step 4: Commit the whole spread refactor**

```bash
git add src/ast.rs src/parser.rs src/interp.rs src/fmt.rs
git commit -m "feat: spread operator in arrays, objects, calls (typed-element AST, strict)

Blast radius (compiler-enforced): ExprKind::Array/Object/Call element types
changed; all construction/match sites in parser, interp, fmt updated.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 2.6: Tree-sitter + example + docs + Phase-2 gate

- [ ] **Step 1: grammar.js.** Add a `spread_element: $ => seq('...', $._expression)` and allow it as an alternative in `array`, `object`, and `arguments` rules (wherever elements/entries/args are listed). Add `'...'` to the token set if the grammar lists literal tokens. Regenerate (`tree-sitter generate --abi 14`) and `cargo build`. If a GLR conflict surfaces, declare it in the `conflicts` array (the grammar already declares two — follow that pattern); document why in a comment.

- [ ] **Step 2: Create `examples/spread.as`:**

```javascript
// Spread: arrays, objects, and call arguments.
let base = [1, 2, 3]
let more = [0, ...base, 4]
print(more)                       // [0, 1, 2, 3, 4]

let defaults = {host: "local", port: 80}
let config = {...defaults, port: 443}
print(config)                     // {host: local, port: 443}

fn sum3(a, b, c) { return a + b + c }
let nums = [10, 20, 30]
print(sum3(...nums))              // 60

// Forwarding idiom (pairs with rest in Phase 3).
fn maxOf(...xs) { return xs }     // collects (Phase 3); here just echo
print(maxOf(...base))             // [1, 2, 3]
```

> The last two lines depend on Phase 3 rest params. If executing Phase 2 standalone, drop the `maxOf` lines and re-add them in Phase 3's example update. (Note this in the commit.)

- [ ] **Step 3: Run + conformance**

Run:
```bash
cargo build --release && ./target/release/ascript run examples/spread.as
cargo test --test treesitter_conformance && cargo test --test frontend_conformance
```
Expected: example prints the commented values; conformance green.

- [ ] **Step 4: Docs.** Add a "Spread" subsection to the language guide (`docs/content/language/`): the three contexts, strictness (no array↔object coercion → Tier-2 panic), object later-wins semantics. README one-liner. CLAUDE.md note:
  > **Spread `...`** (`Tok::DotDotDot`): valid in array literals, object literals, and call args via typed-element AST (`ArrayElem`/`ObjEntry`/`CallArg`). Strict: spreading the wrong container is a Tier-2 panic; object-spread is later-value-wins with `IndexMap` first-seen position.

- [ ] **Step 5: PHASE-2 GATE** — run the full gate block (same eight commands as Phase 1, swapping the example path) and confirm green; record blast-radius note (the `ExprKind` element-type change was compiler-enforced exhaustive; `..` range operator unaffected — verified by `lexes_triple_dot...` test).

- [ ] **Step 6: Commit gate**

```bash
git add grammar dir, examples/spread.as docs/ README.md CLAUDE.md
git commit -m "docs+grammar+test: spread example, guide, grammar; phase-2 gate green

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Phase 3 — Rest Collectors

**Goal:** `fn f(a, ...rest: array<number>)` (typed, per-element), `let [a, ...rest] = arr`, `let {a, ...rest} = obj`. Non-rest function calls remain byte-identical.

**Files:**
- Modify: `src/ast.rs` — `Param` gains `rest: bool`; `LetDestructure` gains a rest slot.
- Modify: `src/parser.rs` — `param_list` rest; array-pattern rest; object-pattern rest (flip Phase-1 `rest`).
- Modify: `src/interp.rs` — `run_body` rest branch (per-element contract); `LetDestructure` array-rest eval.
- Modify: `src/fmt.rs`, `src/lsp/analysis.rs`, `grammar.js`.
- Create/extend examples; docs.

### Task 3.1: Rest parameters — AST + parser

- [ ] **Step 1: AST.** In `src/ast.rs` `Param`, add `pub rest: bool,`. Update the two `Param { .. }` constructions in `src/parser.rs` (single-ident arrow at ~705, and any other literal construction) to include `rest: false`.

- [ ] **Step 2: Failing parser test:**

```rust
#[test]
fn parses_rest_param_typed_and_untyped() {
    match &parse(&lex("fn f(a, ...rest: array<number>) {}").unwrap())[0] {
        Stmt::Fn { params, .. } => {
            assert_eq!(params.len(), 2);
            assert!(!params[0].rest);
            assert!(params[1].rest);
            assert_eq!(params[1].ty.as_ref().unwrap().to_string(), "array<number>");
        }
        o => panic!("got {o:?}"),
    }
    match &parse(&lex("fn g(...rest) {}").unwrap())[0] {
        Stmt::Fn { params, .. } => assert!(params[0].rest && params[0].ty.is_none()),
        o => panic!("got {o:?}"),
    }
}

#[test]
fn rest_param_must_be_last() {
    assert!(parse(&lex("fn f(...rest, a) {}").unwrap()).is_err());
}
```

- [ ] **Step 3: Run → fail**

Run: `cargo test parses_rest_param_typed_and_untyped rest_param_must_be_last`
Expected: FAIL (no `rest` field / no enforcement).

- [ ] **Step 4: Implement in `param_list`.** Replace the per-parameter head of the loop:

```rust
                let is_rest = if *self.peek() == Tok::DotDotDot {
                    self.advance();
                    true
                } else {
                    false
                };
                let name = match self.advance() {
                    Tok::Ident(name) => name,
                    other => return Err(AsError::at(
                        format!("expected a parameter name, found {:?}", other),
                        self.tokens[self.pos - 1].span)),
                };
                let name_span = self.tokens[self.pos - 1].span;
                let ty = if *self.peek() == Tok::Colon {
                    self.advance();
                    Some(self.parse_type()?)
                } else { None };
                let rest = is_rest;
                params.push(crate::ast::Param { name, ty, name_span, rest });
                if rest {
                    // rest must be the final parameter
                    if *self.peek() == Tok::Comma {
                        return Err(AsError::at("a rest parameter must be last", name_span));
                    }
                    break;
                }
```

- [ ] **Step 5: Run → pass**

Run: `cargo test parses_rest_param_typed_and_untyped rest_param_must_be_last`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/ast.rs src/parser.rs
git commit -m "feat(parser): rest parameters (...name[: array<T>]), must be last

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 3.2: Rest parameter binding (fast-path-preserving)

- [ ] **Step 1: Characterization tests FIRST (pin current strict-arity wording for non-rest):**

```rust
#[tokio::test]
async fn non_rest_arity_error_message_unchanged() {
    let e = eval_program_expect_panic("fn f(a, b) {} f(1)").await;
    assert!(e.contains("expected 2 argument(s), got 1"), "got: {e}");
}
```

Run: `cargo test non_rest_arity_error_message_unchanged`
Expected: PASS (pins existing behavior before refactor).

- [ ] **Step 2: Failing rest-binding tests:**

```rust
#[tokio::test]
async fn rest_param_collects_trailing_args_as_array() {
    let out = eval_program(r#"
fn f(a, ...rest) { print(a); print(rest) }
f(1)
f(1, 2, 3)
"#).await;
    assert_eq!(out, "1\n[]\n1\n[2, 3]\n");
}

#[tokio::test]
async fn rest_param_too_few_fixed_args_panics() {
    let e = eval_program_expect_panic("fn f(a, b, ...r) {} f(1)").await;
    assert!(e.contains("at least 2"), "got: {e}");
}

#[tokio::test]
async fn typed_rest_checks_each_element() {
    let e = eval_program_expect_panic(r#"fn f(...rest: array<number>) {} f(1, "x", 3)"#).await;
    assert!(e.to_lowercase().contains("number"), "got: {e}");
    // happy path
    let out = eval_program(r#"fn f(...rest: array<number>) { print(rest) } f(1, 2)"#).await;
    assert_eq!(out, "[1, 2]\n");
}
```

- [ ] **Step 3: Run → fail**

Run: `cargo test rest_param_collects rest_param_too_few typed_rest_checks`
Expected: FAIL (rest not yet bound; arity equality rejects extra args).

- [ ] **Step 4: Implement the rest branch in `run_body`.** Replace the arity check + bind loop (the `if args.len() != params.len() { .. }` block and the `for (p, a) in params.iter().zip(...)` loop) with a fast-path/rest split:

```rust
        let has_rest = params.last().map_or(false, |p| p.rest);
        if !has_rest {
            // ── UNCHANGED fast path: exact arity, byte-identical to pre-rest ──
            if args.len() != params.len() {
                return Err(AsError::at(
                    format!("{} expected {} argument(s), got {}", what, params.len(), args.len()),
                    span,
                ).into());
            }
            for (p, a) in params.iter().zip(args.into_iter()) {
                if let Some(ty) = &p.ty {
                    if !check_type(&a, ty) { return Err(contract_panic(ty, &a, span)); }
                }
                call_env.define(&p.name, a, true).map_err(AsError::new)?;
            }
        } else {
            let n_fixed = params.len() - 1; // rest is last
            if args.len() < n_fixed {
                return Err(AsError::at(
                    format!("{} expected at least {} argument(s), got {}", what, n_fixed, args.len()),
                    span,
                ).into());
            }
            let mut it = args.into_iter();
            // bind fixed params
            for p in &params[..n_fixed] {
                let a = it.next().unwrap();
                if let Some(ty) = &p.ty {
                    if !check_type(&a, ty) { return Err(contract_panic(ty, &a, span)); }
                }
                call_env.define(&p.name, a, true).map_err(AsError::new)?;
            }
            // collect the tail into an array; per-element contract if `: array<T>`
            let rest_p = &params[n_fixed];
            let elem_ty = match &rest_p.ty {
                Some(crate::ast::Type::Array(inner)) => Some(inner.as_ref()),
                Some(other) => {
                    return Err(AsError::at(
                        format!("a rest parameter type must be an array type (array<T>), got {}", other),
                        span,
                    ).into());
                }
                None => None,
            };
            let mut rest_vals = Vec::new();
            for a in it {
                if let Some(t) = elem_ty {
                    if !check_type(&a, t) { return Err(contract_panic(t, &a, span)); }
                }
                rest_vals.push(a);
            }
            let arr = Value::Array(std::rc::Rc::new(std::cell::RefCell::new(rest_vals)));
            call_env.define(&rest_p.name, arr, true).map_err(AsError::new)?;
        }
```

- [ ] **Step 5: Run all rest + characterization tests**

Run: `cargo test rest_param non_rest_arity typed_rest`
Expected: PASS (fast-path message unchanged; rest collects; typed rest checks elements).

- [ ] **Step 6: Commit**

```bash
git add src/interp.rs
git commit -m "feat(interp): rest-param binding via fast-path branch in run_body

Non-rest calls keep exact-arity equality + identical error wording. Rest
collects the tail into an array; array<T> rest is per-element checked.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 3.3: Array-rest destructuring

- [ ] **Step 1: AST.** Add `pub rest: Option<(String, Span)>` to `Stmt::LetDestructure`; update its construction in `let_stmt` and all match sites (interp, fmt, lsp) — compiler will flag them.

- [ ] **Step 2: Failing test:**

```rust
#[tokio::test]
async fn array_rest_destructuring() {
    let out = eval_program(r#"
let [first, ...others] = [1, 2, 3, 4]
print(first)
print(others)
let [only, ...none] = [9]
print(none)
"#).await;
    assert_eq!(out, "1\n[2, 3, 4]\n[]\n");
}
```

- [ ] **Step 3: Parser.** In the `Tok::LBracket` destructuring loop in `let_stmt`, detect a leading `...` on the final name:

```rust
                    if *self.peek() == Tok::DotDotDot {
                        self.advance();
                        let rspan = self.span();
                        let rname = match self.advance() {
                            Tok::Ident(n) => n,
                            other => return Err(AsError::at(
                                format!("expected a name after '...', found {:?}", other),
                                self.tokens[self.pos - 1].span)),
                        };
                        rest = Some((rname, rspan));
                        if *self.peek() == Tok::Comma {
                            return Err(AsError::at("a rest element must be last", rspan));
                        }
                        break;
                    }
```
(Declare `let mut rest: Option<(String, Span)> = None;` at the top of the array branch and include it in the `Stmt::LetDestructure { .., rest }` constructor.)

- [ ] **Step 4: Interp.** In the `Stmt::LetDestructure` exec arm, after binding the positional names, collect the tail:

```rust
                if let Some((rest_name, _)) = rest {
                    let tail: Vec<Value> = items.iter().skip(names.len()).cloned().collect();
                    let arr = Value::Array(std::rc::Rc::new(std::cell::RefCell::new(tail)));
                    env.define(rest_name, arr, *mutable).map_err(AsError::new)?;
                }
```

- [ ] **Step 5: fmt + lsp.** Update `Stmt::LetDestructure` fmt arm to emit `, ...name` before `]` when `rest` is `Some`; update the three lsp arms to also emit the rest symbol (mirror Phase-1 object-rest handling).

- [ ] **Step 6: Run**

Run: `cargo test array_rest_destructuring && cargo test --features lsp lsp`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/ast.rs src/parser.rs src/interp.rs src/fmt.rs src/lsp/analysis.rs
git commit -m "feat: array rest destructuring (let [a, ...rest] = arr)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 3.4: Object-rest destructuring (flip Phase-1 rest)

- [ ] **Step 1: Failing test:**

```rust
#[tokio::test]
async fn object_rest_destructuring_excludes_source_keys() {
    let out = eval_program(r#"
let {a, b as local, ...rest} = {a: 1, b: 2, c: 3, d: 4}
print(a); print(local); print(rest)
"#).await;
    assert_eq!(out, "1\n2\n{c: 3, d: 4}\n");
}
```

- [ ] **Step 2: Parser.** In the Phase-1 object-pattern loop, detect `...` as the final entry and set the `rest` slot (which Phase 1 already plumbed through the AST/interp/fmt):

```rust
                    if *self.peek() == Tok::DotDotDot {
                        self.advance();
                        let rspan = self.span();
                        let rname = match self.advance() {
                            Tok::Ident(n) => n,
                            other => return Err(AsError::at(
                                format!("expected a name after '...', found {:?}", other),
                                self.tokens[self.pos - 1].span)),
                        };
                        rest = Some((rname, rspan));
                        if *self.peek() == Tok::Comma {
                            return Err(AsError::at("a rest element must be last", rspan));
                        }
                        break;
                    }
```
(Change Phase-1's `let rest: Option<(String, Span)> = None;` to `let mut rest = None;`.)

- [ ] **Step 3: Run** — the interp arm (written in Phase 1, Task 1.3 Step 3) already excludes source keys, so no interp change is needed.

Run: `cargo test object_rest_destructuring_excludes_source_keys`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/parser.rs
git commit -m "feat(parser): object rest destructuring (let {a, ...rest} = obj)

Rest excludes source keys; interp + fmt path was plumbed in phase 1.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 3.5: Tree-sitter + examples + docs + Phase-3 gate

- [ ] **Step 1: grammar.js.** Add `rest_parameter: $ => seq('...', field('name', $.identifier), optional(seq(':', field('type', $._type))))` to the `parameters` rule (as the optional final entry). Add `rest_element: $ => seq('...', $.identifier)` to `array_pattern` and `object_pattern` (final entry). Regenerate + build; resolve/declare any conflict.

- [ ] **Step 2: Create `examples/rest.as`:**

```javascript
// Rest parameters — collect trailing args into an array.
fn sum(...nums: array<number>) {
  let total = 0
  for n in nums { total = total + n }
  return total
}
print(sum(1, 2, 3, 4))            // 10
print(sum())                      // 0

fn tagged(label, ...rest) {
  print(label)
  print(rest)
}
tagged("nums", 1, 2)              // nums \n [1, 2]

// Rest in destructuring.
let [head, ...tail] = [10, 20, 30]
print(head)                       // 10
print(tail)                       // [20, 30]

let {id, ...meta} = {id: 7, role: "admin", active: true}
print(id)                         // 7
print(meta)                       // {role: admin, active: true}

// Spread + rest forwarding round-trip.
fn wrap(...args) { return sum(...args) }
print(wrap(5, 6, 7))              // 18
```

- [ ] **Step 3: Run + conformance**

Run:
```bash
cargo build --release && ./target/release/ascript run examples/rest.as
cargo test --test treesitter_conformance && cargo test --test frontend_conformance
```
Expected: prints `10`,`0`,`nums`,`[1, 2]`,`10`,`[20, 30]`,`7`,`{role: admin, active: true}`,`18`.

- [ ] **Step 4: Update `examples/spread.as`** — restore the `maxOf`/forwarding lines deferred in Phase 2 (now that rest exists) and re-run it.

- [ ] **Step 5: Docs.** Language guide: "Rest parameters" + "Rest in destructuring" subsections (array<T> typing, per-element check, must-be-last, empty = `[]`/`{}`). Note the async/generator lazy-arity timing. README one-liner. CLAUDE.md:
  > **Rest `...name`**: rest params collect trailing args into an array (typed `...name: array<T>`, per-element checked) via a `has_rest` fast-path branch in `run_body` (non-rest calls byte-identical); array/object rest patterns collect the tail/leftover keys. Object-rest excludes **source** keys. For async/`fn*`, arity/contract errors surface lazily (when the future is driven).

- [ ] **Step 6: PHASE-3 GATE** — full gate block on `examples/rest.as` and `examples/spread.as`; confirm `non_rest_arity_error_message_unchanged` still passes (proves fast path untouched). Blast-radius note: `run_body` change is gated behind `has_rest`; all script call kinds (sync/async/`fn*`/method/arrow) reach it via `run_function_body` → so rest works uniformly and no path was missed.

- [ ] **Step 7: Commit gate**

```bash
git add grammar dir, examples/rest.as examples/spread.as docs/ README.md CLAUDE.md
git commit -m "docs+grammar+test: rest example, guide, grammar; phase-3 gate green

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Phase 4 — Print Streaming (`OutputSink`) + Panic-Output Fix

**Goal:** `print` streams live to stdout under the CLI while staying captured for tests/REPL/embedders; output before a panic is no longer dropped.

**Files:**
- Modify: `src/interp.rs` — `OutputSink` enum, field, `push_output`, accessor.
- Modify: `src/lib.rs` — `run_file` (Live), `run_source` (Capture), panic arms return/stream output.
- Modify: `src/main.rs` — drop the end-of-run re-print for `run_file`.
- Modify: `src/repl.rs` — keep Capture (per-line flush) unchanged in behavior.
- Modify: `CLAUDE.md`/docs — remove the "buffered until exit" caveat.

### Task 4.1: OutputSink

- [ ] **Step 1: Failing test** (capture still works; live streams):

```rust
#[tokio::test]
async fn capture_sink_buffers_and_live_sink_streams() {
    // Capture mode (default for run_source): output() returns the text.
    let out = run_source("print(1)\nprint(2)").await.unwrap();
    assert_eq!(out, "1\n2\n");
}

#[tokio::test]
async fn output_survives_a_panic_in_capture_mode() {
    // Before the fix, output before a panic was dropped.
    let interp = Interp::new();
    interp.install_self();
    let _ = run_source_on(&interp, "print(\"before\")\nlen(1, 2, 3)").await; // panics
    assert!(interp.output().contains("before"), "output before panic must be retained");
}
```

> If a `run_source_on(&interp, src)` helper does not exist, add a thin test-only one that execs into a provided interp and returns the `Result`, so the test can read `interp.output()` after a panic.

- [ ] **Step 2: Implement `OutputSink`.** In `src/interp.rs`, replace the `output: RefCell<String>` field with:

```rust
/// Where `print` output goes. `Capture` buffers it (tests, REPL, embedders read
/// it back via `output()`); `Live` streams to stdout as it is produced (CLI
/// `run`) so a long-running program shows output immediately and output is not
/// lost if the program later panics.
pub enum OutputSink {
    Capture(RefCell<String>),
    Live,
}
```

Field: `output: OutputSink,` — default `OutputSink::Capture(RefCell::new(String::new()))` in `Interp::new()`. Add a setter `pub fn set_live_output(&mut self)` OR construct via `Interp::new_live()` (whichever fits how `run_file` builds the interp — it currently does `Rc::new(Interp::new())`, so add `Interp::new_live()` that returns an `Interp` with `output: OutputSink::Live`).

Update the accessors:

```rust
    pub(crate) fn push_output(&self, s: &str) {
        match &self.output {
            OutputSink::Capture(buf) => buf.borrow_mut().push_str(s),
            OutputSink::Live => {
                use std::io::Write;
                let mut so = std::io::stdout().lock();
                let _ = so.write_all(s.as_bytes());
                let _ = so.flush(); // stream immediately even when piped
            }
        }
    }
    pub fn output(&self) -> String {
        match &self.output {
            OutputSink::Capture(buf) => buf.borrow().clone(),
            OutputSink::Live => String::new(),
        }
    }
    pub(crate) fn output_is_empty(&self) -> bool {
        match &self.output {
            OutputSink::Capture(buf) => buf.borrow().is_empty(),
            OutputSink::Live => true,
        }
    }
    pub(crate) fn clear_output(&self) {
        if let OutputSink::Capture(buf) = &self.output { buf.borrow_mut().clear(); }
    }
```

- [ ] **Step 3: Run capture tests**

Run: `cargo test capture_sink_buffers output_survives_a_panic`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/interp.rs
git commit -m "feat(interp): OutputSink (Capture|Live) for print; retain output on panic

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 4.2: Wire entry points

- [ ] **Step 1: `src/lib.rs`.** In `run_file`, build a Live interp and stream the panic path's nothing (output already streamed): replace `Rc::new(Interp::new())` with `Rc::new(Interp::new_live())`, and in the result match, the `Ok(_)` arm returns `Ok(interp.output())` (now empty string under Live — fine), and the `Panic(e)` arm returns `Err(e)` unchanged (output already streamed). `run_source` keeps `Interp::new()` (Capture) — unchanged behavior.

- [ ] **Step 2: `src/main.rs`.** The `Command::Run` `Ok(output) => { print!("{}", output); .. }` now prints an empty string under Live (output already streamed). Simplify to not re-print:

```rust
        Command::Run { file } => match ascript::run_file(std::path::Path::new(&file)).await {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => { ascript::diagnostics::report(&e); ExitCode::from(1) }
        },
```

- [ ] **Step 3: Integration test** (the binary streams, and a panicking program still shows its prior output). Add to `tests/cli.rs`:

```rust
#[test]
fn run_streams_output_and_keeps_it_before_panic() {
    // a program that prints then panics: stdout must contain "before".
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("p.as");
    std::fs::write(&f, "print(\"before\")\nlen(1,2,3)\n").unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ascript"))
        .args(["run", f.to_str().unwrap()]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("before"));
}
```

> Match the existing `tests/cli.rs` style for building a temp `.as` file (reuse its helper if present).

- [ ] **Step 4: Run**

Run: `cargo test --test cli run_streams_output_and_keeps_it_before_panic`
Expected: PASS.

- [ ] **Step 5: REPL sanity.** `src/repl.rs` uses Capture (`Interp::new()`); its `flush_output` (print buffer + clear) is unchanged and correct. Confirm: `cargo test repl` passes; manually `cargo run -- repl`, type `print(1)`, see `1` once (no double-print).

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/main.rs tests/cli.rs
git commit -m "feat: CLI run streams print live; REPL/embedders stay captured

Fixes output-before-panic being dropped. run_file=Live, run_source=Capture.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 4.3: Docs + Phase-4 gate

- [ ] **Step 1: CLAUDE.md.** Update the blockquote that currently says `print` output is "buffered and flushed at program exit" to reflect: under the CLI `run`, `print` streams live (`OutputSink::Live`); `run_source`/REPL/tests capture it (`OutputSink::Capture`); output before a panic is retained. Remove the now-obsolete `serve({maxRequests:N})` "won't stream logs live" implication for `print` (the server doc can keep `maxRequests` for graceful shutdown).

- [ ] **Step 2: Docs.** Update any `docs/content` page that documents `print` buffering semantics. Update the server example note if it referenced print buffering.

- [ ] **Step 3: PHASE-4 GATE** — full gate block. Special attention: run the **entire** suite (`cargo test` and `--no-default-features`) — ~540 tests read `output()` and must all stay green (Capture path unchanged). Confirm no double-printing in REPL.

- [ ] **Step 4: Commit gate**

```bash
git add CLAUDE.md docs/
git commit -m "docs: print now streams live under CLI; phase-4 gate green

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Phase 5 — Structured Logging (`std/log`)

**Goal:** `log.info("saved", {userId: 5})` → leveled, field-structured records to stderr (live), format selectable (default human, JSON-lines opt-in), never crashes on un-serializable values, level-filtered with thunk support.

**Files:**
- Modify: `src/stdlib/json.rs` — add a lossy mode to `from_ascript` (total serializer).
- Create: `src/stdlib/log.rs` — `exports()` + `Interp::call_log(...)`.
- Modify: `src/stdlib/mod.rs` — register module (both routing arms), `pub mod log`.
- Modify: `src/interp.rs` — `log_level: Cell<Level>` + a log sink that respects `OutputSink`-style capture for tests; route `"log"` calls.
- Modify: `Cargo.toml` — `log` feature (in `default`).
- Create: `examples/logging.as`; docs (`docs/content/stdlib/`), README, CLAUDE.

### Task 5.1: Total (lossy) serializer

- [ ] **Step 1: Failing test** in `src/stdlib/json.rs` tests:

```rust
#[test]
fn lossy_serializer_never_errors() {
    // cycle
    let a = Value::Array(Rc::new(RefCell::new(vec![])));
    if let Value::Array(inner) = &a { inner.borrow_mut().push(a.clone()); }
    let jv = to_json_lossy(&a, &mut Vec::new());
    assert_eq!(jv.to_string(), "[\"[Circular]\"]");
    // function
    assert_eq!(to_json_lossy(&Value::Builtin("print".into()), &mut Vec::new()).to_string(), "\"<function>\"");
    // NaN
    assert_eq!(to_json_lossy(&Value::Number(f64::NAN), &mut Vec::new()), serde_json::Value::Null);
}
```

- [ ] **Step 2: Run → fail**

Run: `cargo test lossy_serializer_never_errors`
Expected: FAIL — `to_json_lossy` undefined.

- [ ] **Step 3: Implement `to_json_lossy`** in `src/stdlib/json.rs` (mirror `from_ascript`, but substitute placeholders instead of erroring). Make it `pub(crate)`:

```rust
/// AScript Value -> serde_json::Value, TOTAL: never errors. Cycles become
/// "[Circular]", non-finite numbers become null, functions/native/regex become
/// "<function>"/"<native>". Used by std/log so a logging call never crashes.
pub(crate) fn to_json_lossy(v: &Value, seen: &mut Vec<usize>) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        Value::Nil => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Number(n) => {
            if !n.is_finite() { return J::Null; }
            if n.fract() == 0.0 && *n >= i64::MIN as f64 && *n <= i64::MAX as f64 {
                J::Number(serde_json::Number::from(*n as i64))
            } else {
                serde_json::Number::from_f64(*n).map(J::Number).unwrap_or(J::Null)
            }
        }
        Value::Str(s) => J::String(s.to_string()),
        Value::Array(a) => {
            let ptr = Rc::as_ptr(a) as usize;
            if seen.contains(&ptr) { return J::String("[Circular]".into()); }
            seen.push(ptr);
            let out = a.borrow().iter().map(|x| to_json_lossy(x, seen)).collect();
            seen.pop();
            J::Array(out)
        }
        Value::Object(o) => {
            let ptr = Rc::as_ptr(o) as usize;
            if seen.contains(&ptr) { return J::String("[Circular]".into()); }
            seen.push(ptr);
            let mut m = serde_json::Map::new();
            for (k, val) in o.borrow().iter() { m.insert(k.clone(), to_json_lossy(val, seen)); }
            seen.pop();
            J::Object(m)
        }
        Value::Instance(i) => {
            let ptr = Rc::as_ptr(i) as usize;
            if seen.contains(&ptr) { return J::String("[Circular]".into()); }
            seen.push(ptr);
            let mut m = serde_json::Map::new();
            for (k, val) in i.borrow().fields.iter() { m.insert(k.clone(), to_json_lossy(val, seen)); }
            seen.pop();
            J::Object(m)
        }
        Value::Map(mp) => {
            let ptr = Rc::as_ptr(mp) as usize;
            if seen.contains(&ptr) { return J::String("[Circular]".into()); }
            seen.push(ptr);
            let mut m = serde_json::Map::new();
            for (k, val) in mp.borrow().iter() {
                let key = match k.to_value() { Value::Str(s) => s.to_string(), other => other.to_string() };
                m.insert(key, to_json_lossy(val, seen));
            }
            seen.pop();
            J::Object(m)
        }
        Value::Function(_) | Value::Builtin(_) => J::String("<function>".into()),
        other => J::String(format!("<{}>", crate::interp::type_name(other))),
    }
}
```

- [ ] **Step 4: Run → pass**

Run: `cargo test lossy_serializer_never_errors`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/stdlib/json.rs
git commit -m "feat(json): to_json_lossy — total Value->JSON for structured logging

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 5.2: Log level state + sink on Interp

- [ ] **Step 1: Add level + log sink to `Interp`.** In `src/interp.rs`:

```rust
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel { Debug = 0, Info = 1, Warn = 2, Error = 3 }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LogFormat { Human, Json }
```

Fields on `Interp`: `log_level: Cell<LogLevel>` (default `Info`), `log_format: Cell<LogFormat>` (default `Human`), and a `log_capture: RefCell<String>` used **only** when the interp is in Capture mode (tests) — in Live mode, logs go to stderr. Provide:

```rust
    pub(crate) fn emit_log(&self, line: &str) {
        match &self.output {
            OutputSink::Capture(_) => { self.log_capture.borrow_mut().push_str(line); self.log_capture.borrow_mut().push('\n'); }
            OutputSink::Live => { use std::io::Write; let mut e = std::io::stderr().lock(); let _ = writeln!(e, "{}", line); }
        }
    }
    pub fn log_output(&self) -> String { self.log_capture.borrow().clone() }
    pub(crate) fn set_log_level(&self, l: LogLevel) { self.log_level.set(l); }
    pub(crate) fn set_log_format(&self, f: LogFormat) { self.log_format.set(f); }
```

- [ ] **Step 2: Commit (scaffolding)**

```bash
git add src/interp.rs
git commit -m "feat(interp): log level/format state + capturable log sink

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 5.3: The `std/log` module

- [ ] **Step 1: Failing test** in a new `src/stdlib/log.rs` test module (and an `.as`-level test via `run_source`-with-log-capture helper). Start with the record-shaping unit:

```rust
#[cfg(test)]
mod tests {
    // Tested end-to-end via interp in src/interp.rs tests (see log_records_*).
}
```

End-to-end test in `src/interp.rs` tests:

```rust
#[tokio::test]
async fn log_records_human_and_filtering() {
    let interp = Interp::new(); // Capture mode
    interp.install_self();
    run_source_on(&interp, r#"
import * as log from "std/log"
log.setLevel("warn")
log.info("ignored", {a: 1})
log.warn("disk low", {pct: 92})
log.error("boom")
"#).await.unwrap();
    let logs = interp.log_output();
    assert!(!logs.contains("ignored"));          // filtered by level
    assert!(logs.contains("[WARN]") && logs.contains("disk low") && logs.contains("pct=92"));
    assert!(logs.contains("[ERROR]") && logs.contains("boom"));
}

#[tokio::test]
async fn log_json_format_and_thunk() {
    let interp = Interp::new();
    interp.install_self();
    run_source_on(&interp, r#"
import * as log from "std/log"
log.setFormat("json")
log.info("saved", {userId: 5})
log.debug(() => "expensive")   // filtered at info → thunk NOT called
"#).await.unwrap();
    let logs = interp.log_output();
    assert!(logs.contains("\"level\":\"info\"") && logs.contains("\"msg\":\"saved\"") && logs.contains("\"userId\":5"));
    assert!(!logs.contains("expensive"));
}
```

- [ ] **Step 2: Create `src/stdlib/log.rs`:**

```rust
//! `std/log` — leveled, structured logging. Records carry a level, a message,
//! and merged object fields; emitted to stderr (live) or a capture buffer (tests).
//! Serialization is total (never panics) via `json::to_json_lossy`.

use crate::value::Value;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("debug", super::bi("log.debug")),
        ("info", super::bi("log.info")),
        ("warn", super::bi("log.warn")),
        ("error", super::bi("log.error")),
        ("setLevel", super::bi("log.setLevel")),
        ("setFormat", super::bi("log.setFormat")),
    ]
}
```

- [ ] **Step 3: Route `"log"` calls through the interp** (it needs `&self` for level/format/sink). In `src/stdlib/mod.rs` `call` routing, add `"log" => self.call_log(func, args, span).await,` (mirror how `time` async calls route through `self.`). Implement `call_log` in `src/interp.rs` (or `src/stdlib/log.rs` as an `impl Interp` block, gated by `#[cfg(feature = "log")]`... but `log` should be core-on; gate consistent with feature decision below):

```rust
    pub(crate) async fn call_log(&self, func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
        use crate::interp::{LogLevel, LogFormat};
        let level_of = |f: &str| match f { "debug" => Some(LogLevel::Debug), "info" => Some(LogLevel::Info), "warn" => Some(LogLevel::Warn), "error" => Some(LogLevel::Error), _ => None };
        match func {
            "setLevel" => {
                let s = match args.first() { Some(Value::Str(s)) => s.to_string(), _ => return Err(AsError::at("log.setLevel expects a level string", span).into()) };
                match level_of(&s) { Some(l) => { self.set_log_level(l); Ok(Value::Nil) } None => Err(AsError::at(format!("unknown log level {:?}", s), span).into()) }
            }
            "setFormat" => {
                let s = match args.first() { Some(Value::Str(s)) => s.to_string(), _ => return Err(AsError::at("log.setFormat expects \"human\" or \"json\"", span).into()) };
                match s.as_str() { "human" => { self.set_log_format(LogFormat::Human); Ok(Value::Nil) } "json" => { self.set_log_format(LogFormat::Json); Ok(Value::Nil) } o => Err(AsError::at(format!("unknown log format {:?}", o), span).into()) }
            }
            "debug" | "info" | "warn" | "error" => {
                let lvl = level_of(func).unwrap();
                if lvl < self.log_level.get() { return Ok(Value::Nil); } // filtered: skip thunk + serialization
                // first arg: a string message, OR a thunk (function) returning the message
                let (msg, field_args) = match args.first() {
                    Some(Value::Function(_)) | Some(Value::Builtin(_)) => {
                        let r = self.call_value(args[0].clone(), vec![], span).await?;
                        (r.to_string(), &args[1..])
                    }
                    Some(v) if matches!(v, Value::Str(_)) => (v.to_string(), &args[1..]),
                    Some(v) => (v.to_string(), &args[1..]),
                    None => (String::new(), &args[..0]),
                };
                // merge any object args into fields
                let mut fields = serde_json::Map::new();
                for a in field_args {
                    if let Value::Object(o) = a {
                        for (k, val) in o.borrow().iter() {
                            fields.insert(k.clone(), crate::stdlib::json::to_json_lossy(val, &mut Vec::new()));
                        }
                    }
                }
                let level_str = func;
                let line = match self.log_format.get() {
                    LogFormat::Json => {
                        let mut rec = serde_json::Map::new();
                        rec.insert("level".into(), serde_json::Value::String(level_str.into()));
                        rec.insert("msg".into(), serde_json::Value::String(msg));
                        for (k, v) in fields { rec.insert(k, v); }
                        serde_json::Value::Object(rec).to_string()
                    }
                    LogFormat::Human => {
                        let mut s = format!("[{}] {}", level_str.to_uppercase(), msg);
                        for (k, v) in &fields {
                            let vs = match v { serde_json::Value::String(s) => s.clone(), other => other.to_string() };
                            s.push_str(&format!(" {}={}", k, vs));
                        }
                        s
                    }
                };
                self.emit_log(&line);
                Ok(Value::Nil)
            }
            other => Err(AsError::at(format!("std/log has no function '{}'", other), span).into()),
        }
    }
```

Also add `"std/log" => log::exports(),` to the `std_module_exports` match arm in `src/stdlib/mod.rs`, and `pub mod log;` (feature-gated per below).

- [ ] **Step 4: Run end-to-end log tests**

Run: `cargo test log_records_human_and_filtering log_json_format_and_thunk`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/stdlib/log.rs src/stdlib/mod.rs src/interp.rs
git commit -m "feat(stdlib): std/log — leveled structured logging (human/json, thunks)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 5.4: Feature flag + default level from env

- [ ] **Step 1: Cargo feature.** In `Cargo.toml`, add `log = []` to `[features]` and to the `default` list. Gate `pub mod log;` with `#[cfg(feature = "log")]`, the `"std/log"`/`"log"` routing arms with `#[cfg(feature = "log")]`, and `call_log`/log state likewise (so `--no-default-features` builds without it).

- [ ] **Step 2: Default level from `ASCRIPT_LOG`.** In `Interp::new()` (or `new_live()`), read `std::env::var("ASCRIPT_LOG")` and map `debug/info/warn/error` → initial `log_level` (default `Info` if unset/invalid). Add a test:

```rust
#[test]
fn ascript_log_env_sets_default_level() {
    // tested via a helper that constructs Interp with the env var set;
    // assert log_level.get() == LogLevel::Warn when ASCRIPT_LOG=warn.
}
```

> Use a process-safe approach (set/reset the var inside the test, or parse a passed-in string in a pure helper `LogLevel::from_env_str(Option<&str>)` and unit-test that helper instead to avoid env races).

- [ ] **Step 3: Run both configs**

Run: `cargo test --features log && cargo test --no-default-features`
Expected: both green; `--no-default-features` compiles without `std/log`.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml src/stdlib/mod.rs src/interp.rs
git commit -m "feat(log): gate std/log behind default feature; ASCRIPT_LOG default level

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 5.5: Example + docs + Phase-5 gate

- [ ] **Step 1: Create `examples/logging.as`:**

```javascript
import * as log from "std/log"

log.setLevel("debug")
log.debug("starting", {pid: 42})
log.info("request", {method: "GET", path: "/users", ms: 12})
log.warn("slow query", {ms: 540})
log.error("upstream failed", {code: 502})

// Switch to JSON-lines for ingestion.
log.setFormat("json")
log.info("saved", {userId: 7, ok: true})

// Thunk: only evaluated if the level passes (debug is on here).
log.debug(() => "computed detail")
```

- [ ] **Step 2: Run** (logs go to stderr live):

Run: `cargo build --release && ./target/release/ascript run examples/logging.as 2>&1 1>/dev/null`
Expected (stderr): human lines `[DEBUG] starting pid=42`, `[INFO] request method=GET path=/users ms=12`, `[WARN] ...`, `[ERROR] ...`, then a JSON line `{"level":"info","msg":"saved","userId":7,"ok":true}`, then `[DEBUG] computed detail` (still human? No — format is json by now → JSON line). Confirm the actual interleaving and update the comment to match observed output.

- [ ] **Step 3: Conformance** — the example imports `std/log`; ensure both parsers accept it:

Run: `cargo test --test treesitter_conformance && cargo test --test frontend_conformance`
Expected: PASS.

- [ ] **Step 4: Docs.** Create/extend `docs/content/stdlib/` with a `log` reference page (functions, levels, record shape, format, thunks, `ASCRIPT_LOG`, total-serialization guarantee). Add `std/log` to the README stdlib table and to the stdlib table in `CLAUDE.md`'s feature-flags section. CLAUDE.md note:
  > **`std/log`** (`log` feature, default-on): leveled (`debug/info/warn/error`) structured logging. `Interp`-stateful (`log_level`/`log_format`); routes via `self.call_log`. Emits to stderr (Live) or a capture buffer (tests). Serialization is total via `json::to_json_lossy` (cycles→`"[Circular]"`, functions→`"<function>"`, NaN→null). First string arg → `msg`; object args merge as fields; auto `level`. A thunk first-arg defers message construction past the level filter.

- [ ] **Step 5: PHASE-5 GATE** — full gate block on `examples/logging.as`, both feature configs, both clippy configs, conformance, fmt idempotency. Blast-radius note: `std/log` is additive + feature-gated; `to_json_lossy` is a new function (existing `from_ascript`/`json.stringify` unchanged); routing arms are new.

- [ ] **Step 6: Commit gate**

```bash
git add examples/logging.as docs/ README.md CLAUDE.md
git commit -m "docs+test: std/log example, reference, README, CLAUDE; phase-5 gate green

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Final integration pass (after all phases)

- [ ] **Step 1: Update the design spec.** In `docs/superpowers/specs/2026-05-29-ascript-design.md`, add/extend: §6 (object + rest destructuring), §5 (rest param `array<T>` typing), the I/O section (print streaming + `OutputSink`), and a new stdlib subsection for `std/log`. Each addition mirrors the implemented behavior (the spec must match code). Add a roadmap entry in `docs/superpowers/roadmap.md`.

- [ ] **Step 2: Whole-suite + lints, both configs:**

```bash
cargo test
cargo test --no-default-features
cargo clippy --all-targets
cargo clippy --no-default-features --all-targets
```
Expected: all green/clean.

- [ ] **Step 3: Run every example** to confirm the language works end-to-end:

```bash
cargo build --release
for f in examples/object_destructuring.as examples/spread.as examples/rest.as examples/logging.as; do
  echo "== $f =="; ./target/release/ascript run "$f"; done
```
Expected: each prints its documented output with no errors.

- [ ] **Step 4: Formatter idempotency across all new examples:**

```bash
for f in examples/object_destructuring.as examples/spread.as examples/rest.as examples/logging.as; do
  cargo run -- fmt "$f"; done
git diff --exit-code examples/   # no formatting drift
```

- [ ] **Step 5: Final commit + branch finish.** Use `superpowers:finishing-a-development-branch` to merge `--no-ff` (per repo workflow).

---

## Self-Review (run before handing off)

**Spec coverage:** every locked decision maps to a task — object destructuring (1.1–1.7), `as` rename + quoted keys (1.2/1.4), missing→nil + Object/Instance (1.3), spread three contexts + strict + later-wins (2.3–2.5), `...` token (2.1), typed-element AST (2.2), rest params typed array<T> per-element + fast path (3.1–3.2), array-rest (3.3), object-rest source-key exclusion (3.4), print Live/Capture + panic-output fix (4.1–4.3), logging stateful + levels + selectable format + total serializer + thunks + env (5.1–5.5). ✅

**Placeholder scan:** no "TBD"/"add error handling"/"similar to Task N" — every code step shows the code; the one deliberately-red build state (Task 2.2 Step 3) is labelled and resolved within the same phase. The Phase-1 object-rest interp block is written complete (not a stub) and activated by Phase-3 parser flip. ✅

**Type consistency:** `ObjBinding{key,binding,key_span,binding_span}`, `Stmt::LetDestructureObject{bindings,rest,value,mutable,span}`, `LetDestructure` gains `rest: Option<(String,Span)>`, `Param.rest: bool`, `ArrayElem`/`ObjEntry`/`CallArg`, `OutputSink::{Capture,Live}`, `LogLevel`/`LogFormat`, `to_json_lossy`, `call_log` — names are used identically across tasks. ✅
