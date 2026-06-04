# SP2 — New language features — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended)
> or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`)
> syntax for tracking.

**Goal:** Add six surface language features — `instanceof`, default parameters, `#{…}` map literals,
`object.freeze`/`isFrozen`, records (auto-derived `init`), and `..=` as a field default — each running
byte-identical on both engines (bytecode VM + `--tree-walker` oracle).

**Architecture:** Six phases (A–F), one per feature, ordered low-risk → high-risk. Each phase is TDD,
ends green on both feature configs + clippy + the whole-corpus three-way differential, and gets an
independent review before the next. Phase G is the closing invariant + docs + holistic gate. The
tree-walker is the byte-identical oracle (`ascript run --tree-walker`); never weaken it.

**Tech Stack:** Rust. CST front-end → resolver (`src/syntax/resolve`) → compiler (`src/compile/mod.rs`)
→ `Chunk` → VM (`src/vm/*`). Legacy front-end (`src/parser.rs`) → tree-walker (`src/interp.rs`).
Grammar: ungrammar (`src/syntax/ast/ascript.ungram`) + hand CST parser (`src/syntax/parser.rs`) +
tree-sitter (`docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js`; regen
`tree-sitter generate --abi 14`). `.aso` versioned bytecode (`src/vm/aso.rs`, currently v9).

**Spec:** `docs/superpowers/specs/2026-06-04-sp2-language-features-design.md`.

**Branch:** `feat/sp2-language-features` (create off `main` after SP1 merges; spec committed first).

---

## Conventions for every task

- **Differential test harness (`tests/vm_differential.rs`).** The file already exposes:
  `assert_vm_run_matches_treewalker(src)` (compares `ascript::run_source` vs `ascript::vm_run_source`,
  no exit), `assert_vm_run_error_matches_treewalker(src)` (panics/exit parity), and
  `assert_three_way_matches(src)` (tree-walker == specialized-VM == generic-VM). Add new cases with the
  existing `#[tokio::test]` per-snippet pattern (read a few neighbors first). "Byte-identical" =
  identical stdout + exit on all three engines. For new field-default cases reuse
  `assert_field_default_matches(prelude, ty, default, print_expr)` (`:3250`).
- **Per-engine manual smoke:** `cargo build` then `target/debug/ascript run X.as` (VM) vs
  `target/debug/ascript run --tree-walker X.as`.
- **Gate after each phase (paste tails):** `cargo test --test vm_differential 2>&1 | tail`;
  `cargo test 2>&1` (0 failures all binaries); `cargo test --no-default-features 2>&1` (0 failures);
  `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets` (clean);
  `grep await_holding_refcell_ref Cargo.toml` (still `deny`).
- **Grammar tasks:** after editing `grammar.js`, regen `cd docs/superpowers/specs/grammar/tree-sitter-ascript
  && tree-sitter generate --abi 14`; then `cargo test --test treesitter_conformance` +
  `cargo test --test frontend_conformance`.
- **`.aso`:** ONE `ASO_FORMAT_VERSION` bump for the whole sub-project (9 → 10), done in the FIRST phase
  that changes emitted bytecode (Phase A) and reused by every later phase. Do NOT double-bump.
- **Commit trailer:** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Never** edit a passing tree-walker test or weaken a differential assertion to make the VM pass. A
  divergence on valid code = fix the root cause. No `unsafe`/`#[allow]`/`#[ignore]`/stubs.

---

## Phase A — `instanceof` operator (reuses the dead `Op::InstanceOf`)

**Files:** `src/token.rs` + `src/lexer.rs` (legacy kw); `src/syntax/kind.rs` + `src/syntax/lexer.rs`
(`InstanceofKw`); `src/ast.rs` (`BinOp::InstanceOf` + Display); `src/parser.rs` (`comparison`);
`src/syntax/ast/ascript.ungram` + `grammar.js` + `src/syntax/parser.rs` (regen `parser.c`);
`src/compile/mod.rs` (emit + `cst_default_expr` arm); `src/vm/run.rs` (`Op::InstanceOf` exec);
`src/vm/aso.rs` (v9→v10); `src/interp.rs` + `src/value.rs` (`apply_binop` arm + `is_instance_of`);
`src/syntax/format/*`. Tests `tests/vm_differential.rs`, `tests/aso.rs`.

### Task A1: failing differential tests

- [ ] **Step 1 — Write failing tests** in `tests/vm_differential.rs` (match the neighboring `#[tokio::test]`
  snippet style), each `assert_vm_run_matches_treewalker` unless noted:

```rust
// instance is instanceof its own class
assert_vm_run_matches_treewalker(
    "class C {}\nlet c = C()\nprint(c instanceof C)\n").await;          // true
// subclass instance instanceof parent; parent instance NOT instanceof subclass
assert_vm_run_matches_treewalker(
    "class A {}\nclass B extends A {}\nprint(B() instanceof A)\nprint(A() instanceof B)\n").await;
// non-instances are always false, never panic
assert_vm_run_matches_treewalker(
    "class C {}\nprint(5 instanceof C)\nprint(\"x\" instanceof C)\nprint(nil instanceof C)\n").await;
// precedence: binds like a comparison (looser than &&, tighter than +? — comparison tier)
assert_vm_run_matches_treewalker(
    "class C {}\nlet c = C()\nprint(c instanceof C && true)\n").await;
// rhs not a class -> identical Tier-2 panic both engines
assert_vm_run_error_matches_treewalker(
    "let c = 1\nprint(c instanceof 5)\n").await;
```

- [ ] **Step 2 — Run, verify fail** (parse error today): `cargo test --test vm_differential instanceof 2>&1 | tail -20`.

### Task A2: lexer + AST + legacy parser (oracle accepts first)

- [ ] **Step 3 — Token/lex.** Add `Tok::Instanceof` (`src/token.rs`) + `"instanceof" => Tok::Instanceof`
  in the keyword match (`src/lexer.rs:493-516`). Add `InstanceofKw` to `src/syntax/kind.rs` (with
  `#[static_text("instanceof")]`) + `"instanceof" => InstanceofKw` in `keyword_kind` (`src/syntax/lexer.rs:352`).
- [ ] **Step 4 — AST.** Add `BinOp::InstanceOf` to the enum (`src/ast.rs:475`) + its `Display` arm
  (`write!(f, "instanceof")`).
- [ ] **Step 5 — Legacy parser.** In `Parser::comparison` (`src/parser.rs:1183`), match `Tok::Instanceof`
  → `BinOp::InstanceOf` alongside `Lt/Le/Gt/Ge`.
- [ ] **Step 6 — Shared helper.** Add `pub(crate) fn is_instance_of(v: &Value, class: &Rc<Class>) -> bool`
  (in `src/value.rs`) walking `superclass` by `Rc::as_ptr`. Tree-walker `apply_binop` (`src/interp.rs:3157`)
  gains the `BinOp::InstanceOf` arm: if rhs is `Value::Class` → `Bool(is_instance_of(lhs, cls))`, else
  Tier-2 panic `"instanceof requires a class on the right-hand side"` (anchored at rhs span).
- [ ] **Step 7 — Run** the tree-walker side: `cargo test --test vm_differential instanceof 2>&1 | tail`
  → the tree-walker now behaves (VM half still fails). Commit: `feat(lang): instanceof — lexer, AST, legacy parser + tree-walker`.

### Task A3: CST grammar + compiler + VM exec

- [ ] **Step 8 — Grammar.** ungrammar `BinaryExpr` op list (`:24`) += `'instanceof'`. tree-sitter
  `binary_expression` table (`grammar.js:329-340`) += `['instanceof', PREC.compare]`. Hand CST parser:
  recognize `InstanceofKw` at the comparison tier → `BinaryExpr` node. Regen `parser.c --abi 14`; run
  `treesitter_conformance` + `frontend_conformance`.
- [ ] **Step 9 — Compiler.** `compile_binary` maps `BinOp::InstanceOf` → operands + `Op::InstanceOf`.
  `cst_default_expr` (`src/compile/mod.rs:280`) += `SyntaxKind::InstanceofKw => BinOp::InstanceOf`.
- [ ] **Step 10 — VM.** Add the `Op::InstanceOf` arm in `src/vm/run.rs` (pop `cls`, pop `inst`; non-class
  rhs → the same panic message/span as the tree-walker; else push the bool). Remove the stale
  `instanceof` mention in the `compile/mod.rs:1499` doc comment.
- [ ] **Step 11 — `.aso` v10.** Bump `ASO_FORMAT_VERSION` 9→10 (`src/vm/aso.rs:74`). `Op::InstanceOf`
  is already in the round-trip opcode set/disasm/verify; confirm round-trip. Add `tests/aso.rs` build+run
  of an `instanceof` program; confirm a v9 `.aso` is rejected with the version-mismatch message.
- [ ] **Step 12 — Formatter.** Ensure `BinaryExpr` emits `a instanceof B` (single spaces); idempotence test.
- [ ] **Step 13 — Checker.** `instanceof` needs **no new lint and no rule change** — it is an ordinary
  binary expression, so `undefined`/`unused`/`shadowing`/etc. traverse its operands naturally, and
  `super-misuse`/`call-arity`/`duplicate-member` are unaffected (instanceof introduces no `super`, call,
  or class member). VERIFY this by re-running `ascript check examples/*.as examples/advanced/*.as`
  (0 diagnostics) plus a quick check on an `instanceof` program (no spurious diagnostic). State in the
  commit that no checker change was required.
- [ ] **Step 14 — Run all Phase-A tests** → byte-identical both engines.
- [ ] **Step 15 — Phase-A gate** (full gate set) + manual smoke.
- [ ] **Step 16 — Commit:** `feat(vm): instanceof — CST grammar, compiler emit, Op::InstanceOf exec, .aso v10`.

---

## Phase B — Default parameters

**Files:** `src/ast.rs` (`Param.default`); ungrammar + `grammar.js` + `src/syntax/parser.rs`
(regen `parser.c`); `src/parser.rs` (`param_list`); `src/syntax/resolve/*` (incremental param binding);
`src/interp.rs` (`check_call_args` min/max split + `run_body` default eval); `src/compile/mod.rs` +
`src/vm/run.rs` (default thunks at CALL); `src/check/rules/call_arity.rs` (min/max range);
`src/syntax/format/*`. Tests `tests/vm_differential.rs`, `tests/aso.rs`, `tests/check.rs`.

### Task B1: failing differential tests

- [ ] **Step 1 — Write failing tests:**

```rust
assert_vm_run_matches_treewalker(
    "fn f(a, b = 10) { return a + b }\nprint(f(1))\nprint(f(1, 2))\n").await;        // 11, 3
// default references an earlier param (left-to-right)
assert_vm_run_matches_treewalker(
    "fn f(a, b = a * 2) { return b }\nprint(f(5))\n").await;                          // 10
// arrow default
assert_vm_run_matches_treewalker(
    "let g = (x, y = 5) => x + y\nprint(g(2))\nprint(g(2, 3))\n").await;              // 7, 5
// composes with rest
assert_vm_run_matches_treewalker(
    "fn f(a, b = 2, ...xs) { return [a, b, xs] }\nprint(f(1))\nprint(f(1, 9, 8, 7))\n").await;
// explicit nil suppresses the default
assert_vm_run_matches_treewalker(
    "fn f(a, b = 10) { return b }\nprint(f(1, nil))\n").await;                        // nil
// typed default: value AND explicit both contract-checked
assert_vm_run_error_matches_treewalker(
    "fn f(a, b: number = 1) { return b }\nprint(f(1, \"x\"))\n").await;              // panic both
// required-after-default -> identical parse/compile error both engines
assert_vm_run_error_matches_treewalker(
    "fn f(a = 1, b) { return b }\nprint(f(1, 2))\n").await;
// arity messages
assert_vm_run_error_matches_treewalker("fn f(a, b = 1) {}\nf()\n").await;            // too few
assert_vm_run_error_matches_treewalker("fn f(a, b = 1) {}\nf(1, 2, 3)\n").await;     // too many
```

- [ ] **Step 2 — Run, verify fail:** `cargo test --test vm_differential default_param 2>&1 | tail`
  (name the tests accordingly). Most fail at parse today.

### Task B2: AST + parsers (oracle first)

- [ ] **Step 3 — AST.** Add `default: Option<Expr>` to `Param` (`src/ast.rs:150`); update its `Display`
  (emit ` = <expr>` when present) and any exhaustive constructors/matches.
- [ ] **Step 4 — Legacy parser.** `Parser::param_list` (`src/parser.rs:529`): after the optional `: type`,
  if `*peek == Tok::Eq`, advance + parse an expression into `Param.default`. Enforce required-after-default
  (a param with no default following one with a default) → error
  `"a required parameter cannot follow a defaulted parameter"`.
- [ ] **Step 5 — Grammar.** ungrammar `Param` (`:14`) → `Param = '...'? 'ident' (':' Type)? ('=' Expr)?`.
  tree-sitter `parameter` (`grammar.js:220`) += `optional(seq('=', field('default', $._expression)))`.
  Hand CST parser `param_list` (`src/syntax/parser.rs:544-562`): after the optional `: type`, consume
  `= <expr>` into the Param node. Regen `parser.c --abi 14`; verify the existing
  `[$.parameter, $._primary_expression]` GLR conflict still resolves (run `treesitter_conformance`).
- [ ] **Step 6 — Resolver.** In `src/syntax/resolve/*`, when resolving a function's params, introduce
  each param slot incrementally so a default expression resolves earlier params (e.g. `b = a` sees `a`).
  Add a resolver unit test asserting `fn f(a, b = a)` resolves `a` in `b`'s default as a param (not upvalue).
- [ ] **Step 7 — Run** parse-level: programs now parse on both front-ends; runtime still wrong (defaults
  not applied). Commit: `feat(lang): default-parameter syntax — AST, both parsers, resolver`.

### Task B3: runtime default application (both engines, shared gate)

- [ ] **Step 8 — `check_call_args` min/max split (`src/interp.rs:3552`).** Generalize to compute
  `min` (leading required run) and `max` (`params.len()` or ∞ with rest). Keep the EXACT current
  exact-arity message byte-for-byte when `min == max` and no param has a default (so existing
  tests/goldens stay green); new messages `"{what} expected at least {min} argument(s), got {n}"` /
  `"{what} expected at most {max} argument(s), got {n}"` only when defaults/range apply. Return the
  bound prefix + the set of missing trailing-default param indices (do NOT eval defaults here — keep it
  pure/sync).
- [ ] **Step 9 — Tree-walker default eval (`run_body`, `src/interp.rs:2302`).** After binding provided
  args, evaluate each missing default `Expr` left-to-right in the callee frame env (so it sees earlier
  params), `check_type` if the param is typed, and bind. Never hold a `RefCell` borrow across the eval.
- [ ] **Step 10 — VM default eval.** Compile each defaulted param's default expression to a thunk-like
  sequence (mirror field-default thunks, `src/vm/run.rs:3322`); the CALL path runs it in the new frame
  when the arg is missing, contract-checks, and binds. Verify it composes with rest.
- [ ] **Step 11 — Run all Phase-B differential tests** → byte-identical both engines.
- [ ] **Step 12 — `.aso`.** Default thunks serialize with the proto (reuse v10). Add `tests/aso.rs`
  build+run of a function with defaults (incl. a default referencing an earlier param).
- [ ] **Step 13 — Commit:** `feat(vm): default-parameter evaluation at call time — both engines`.

### Task B4: checker

- [ ] **Step 14 — Checker tests** (`tests/check.rs`): `call-arity` fires for `f()` (too few) and
  `f(1,2,3)` (too many, no rest) on `fn f(a, b=1)`, and is SILENT for `f(1)`/`f(1,2)` (in range);
  silent when the callee has a rest. Implement the min/max range in `src/check/rules/call_arity.rs`
  (replace the `arg_count != param_count` compare at `:85`).
- [ ] **Step 15 — Formatter.** `params`/`params_from_list` (`src/syntax/format/mod.rs:390/411`) emit
  `name: T = expr` (canonical spaces). Idempotence test.
- [ ] **Step 16 — Corpus zero-FP guard:** `ascript check examples/*.as examples/advanced/*.as` → 0 diagnostics.
- [ ] **Step 17 — Phase-B gate** + smoke + add `examples/default_params.as` (deterministic, ends
  `print("default_params ok")`; parses on all parsers; byte-identical VM/tree-walker; builds+runs `.aso`).
- [ ] **Step 18 — Commit:** `feat(check,fmt): default-param-aware call-arity + formatting + corpus example`.

---

## Phase C — `#{…}` map literals

**Files:** `src/token.rs` + `src/lexer.rs` (`Tok::HashBrace`); `src/syntax/kind.rs` + `src/syntax/lexer.rs`
(`HashLBrace`); `src/ast.rs` (`ExprKind::Map` + `MapEntry`); `src/parser.rs` (primary-expr `#{…}`);
ungrammar + `grammar.js` + `src/syntax/parser.rs` (regen `parser.c`); `src/syntax/resolve/*`;
`src/compile/mod.rs` + `src/vm/{run,opcode,disasm,verify,aso}.rs` (`Op::NewMap`/`MapEntry`);
`src/interp.rs` (`ExprKind::Map` eval); `src/syntax/format/*`. Tests `tests/vm_differential.rs`,
`tests/aso.rs`.

### Task C1: failing differential tests

- [ ] **Step 1 — Write failing tests:**

```rust
assert_vm_run_matches_treewalker("print(#{})\n").await;                              // empty map
assert_vm_run_matches_treewalker("print(#{ \"a\": 1, \"b\": 2 })\n").await;
// numeric/bool/nil keys
assert_vm_run_matches_treewalker("print(#{ 1: \"x\", true: \"y\", nil: \"z\" })\n").await;
// key is the VALUE of an expression (NOT the literal name) — the object/map distinction
assert_vm_run_matches_treewalker("let k = \"x\"\nprint(#{ k: 1 })\n").await;        // keyed by "x"
// later-key-wins
assert_vm_run_matches_treewalker("print(#{ 1: \"a\", 1: \"b\" })\n").await;          // "b"
// unhashable key -> identical Tier-2 panic both engines
assert_vm_run_error_matches_treewalker("print(#{ [1]: 2 })\n").await;
// interop with std/map
assert_vm_run_matches_treewalker(
    "import { map } from \"std/map\"\nlet m = #{ \"a\": 1 }\nprint(map.get(m, \"a\"))\n").await;
```
(Confirm the empty-map and key-render output against the tree-walker's `Value::Map` `to_string`.)

- [ ] **Step 2 — Run, verify fail** (lex/parse error today): `cargo test --test vm_differential map_lit 2>&1 | tail`.

### Task C2: lexer + AST + legacy parser (oracle first)

- [ ] **Step 3 — Token/lex.** `#{` is a NEW token and a NEW expression form, so this phase touches the
  **THREE parsers + the legacy oracle** (CST hand parser + ungrammar + tree-sitter, plus `src/parser.rs`
  for the `--tree-walker` oracle) — Task C2 lands the legacy oracle first, Task C3 lands the CST grammar
  (regen `parser.c --abi 14`) so `treesitter_conformance` + `frontend_conformance` stay green. Token
  work here: **Legacy** — add `Tok::HashBrace`; in `src/lexer.rs`, on `#` require the next char be `{`
  (emit ONE `#{` token), else lex error `"unexpected character '#'"`. **CST** — add
  `SyntaxKind::HashLBrace` (`#[static_text("#{")]`) recognized in `src/syntax/lexer.rs`. Lex `#{` as a
  single token so it cannot be confused with `#` + `{`. (Per decision D4, a `...` spread element inside
  `#{}` is a clean parse error — both parsers reject it; add a test in Step 12.)
- [ ] **Step 4 — AST.** Add `ExprKind::Map(Vec<MapEntry>)` near `Object` (`src/ast.rs:48`) and
  `struct MapEntry { key: Expr, value: Expr }`; add the `Display` arm (`#{k: v, …}`).
- [ ] **Step 5 — Legacy parser.** In the primary-expression parser (`src/parser.rs`), on `Tok::HashBrace`
  parse comma-separated `expr ':' expr` entries (trailing comma allowed) → `ExprKind::Map`.
- [ ] **Step 6 — Tree-walker.** `eval_expr` `ExprKind::Map` arm (`src/interp.rs`): eval each key+value
  left-to-right; `MapKey::from_value` (`src/value.rs:104`) — on `None` Tier-2 panic
  `"cannot use {type} as a map key"` at the key span; later-wins insert into a fresh `MapCell`.
- [ ] **Step 7 — Run** the tree-walker side green (VM still fails). Commit:
  `feat(lang): #{…} map literal — lexer, AST, legacy parser + tree-walker`.

### Task C3: CST grammar + compiler + VM

- [ ] **Step 8 — Grammar.** ungrammar: `MapExpr = '#{' MapEntry* '}'`, `MapEntry = key:Expr ':' value:Expr`,
  add `MapExpr` to the `Expr` alternation (`:16`). tree-sitter: `map_literal: $ => seq('#{',
  commaSep($.map_entry), optional(','), '}')`, `map_entry: $ => seq(field('key', $._expression), ':',
  field('value', $._expression))`, add `map_literal` to `_primary_expression`. Hand CST parser: on
  `HashLBrace` in primary position, parse a `MapExpr` node. Regen `parser.c --abi 14`; run conformance.
- [ ] **Step 9 — Compiler/VM opcodes.** Add `Op::NewMap` + `Op::MapEntry` (`src/vm/opcode.rs`, mirror
  `NewObject`/`AppendObject` at `:225/253`): `NEW_MAP` pushes an empty `Value::Map`; `MAP_ENTRY` pops
  value+key, `MapKey::from_value` (panic on `None`, same message/span), later-wins insert. Add disasm
  strings (`src/vm/disasm.rs`), verifier effects (`src/vm/verify.rs`), and the round-trip opcode-set
  entries (`src/vm/opcode.rs:~693`). Compiler `ExprKind::Map`/`MapExpr` → `NEW_MAP` + per-entry
  key/value eval + `MAP_ENTRY`. Reuse `.aso` v10.
- [ ] **Step 10 — Resolver.** Map key/value exprs are ordinary resolved expressions (no new binding) —
  ensure the resolver visits both children.
- [ ] **Step 11 — Formatter.** Emit `#{key: value, …}` / `#{}`; format the key as an EXPRESSION (not the
  object-key quoting). Idempotence test.
- [ ] **Step 12 — Run all Phase-C tests** → byte-identical both engines; `.aso` round-trip
  (`tests/aso.rs`); confirm `#` not followed by `{` is a lex error on BOTH lexers; confirm a spread
  inside a map literal (`#{ ...m }`, D4) is a clean parse error on BOTH front-ends (no panic, exit
  non-zero, identical-shape error) via a conformance/parse-error test.
- [ ] **Step 13 — Phase-C gate** + smoke + add `examples/map_literals.as` (deterministic, ends
  `print("map_literals ok")`).
- [ ] **Step 14 — Commit:** `feat(vm): #{…} map literals — CST grammar, Op::NewMap/MapEntry, formatter`.

---

## Phase D — `object.freeze` / `object.isFrozen`

**Files:** `src/value.rs` (frozen flags + `ArrayCell` wrapper + accessors); `src/stdlib/object.rs`
(freeze/isFrozen); mutation-site checks in `src/interp.rs` (`index_set`/`set_member`), `src/vm/run.rs`
(`SetIndex`/`vm_set_prop`/`AppendArray`/`AppendObject`/`SpreadObject`), `src/stdlib/array.rs`,
`src/stdlib/map.rs`. Tests `tests/vm_differential.rs`, `src/stdlib/object.rs` unit tests.

### Task D1: failing differential tests

- [ ] **Step 1 — Write failing tests:**

```rust
// freeze returns the value; isFrozen reflects it
assert_vm_run_matches_treewalker(
    "import { object } from \"std/object\"\nlet o = {a: 1}\nprint(object.isFrozen(o))\n\
     object.freeze(o)\nprint(object.isFrozen(o))\n").await;            // false, true
// mutating a frozen object/array/map/instance -> identical panic both engines
assert_vm_run_error_matches_treewalker(
    "import { object } from \"std/object\"\nlet o = {a: 1}\nobject.freeze(o)\no.a = 2\n").await;
assert_vm_run_error_matches_treewalker(
    "import { object } from \"std/object\"\nimport { array } from \"std/array\"\n\
     let a = [1]\nobject.freeze(a)\narray.push(a, 2)\n").await;
assert_vm_run_error_matches_treewalker(
    "import { object } from \"std/object\"\nimport { map } from \"std/map\"\n\
     let m = map.new()\nobject.freeze(m)\nmap.set(m, \"k\", 1)\n").await;
assert_vm_run_error_matches_treewalker(
    "import { object } from \"std/object\"\nclass C { x: number = 0 }\nlet c = C()\n\
     object.freeze(c)\nc.x = 9\n").await;
// shallow: element of a frozen array is still mutable
assert_vm_run_matches_treewalker(
    "import { object } from \"std/object\"\nlet a = [[1]]\nobject.freeze(a)\n\
     a[0][0] = 9\nprint(a)\n").await;
// non-container freeze is a no-op; deep-clone of frozen is unfrozen
assert_vm_run_matches_treewalker(
    "import { object } from \"std/object\"\nprint(object.isFrozen(5))\n\
     let o = {a: 1}\nobject.freeze(o)\nlet c = object.deepClone(o)\nprint(object.isFrozen(c))\n").await;
```

- [ ] **Step 2 — Run, verify fail** (`std/object has no function 'freeze'`): `cargo test --test vm_differential freeze 2>&1 | tail`.

### Task D2: representation — frozen flags + the `ArrayCell` migration (its own behavior-neutral task, D3-confirmed)

> This task is the ONE sanctioned `value.rs` representation change for SP2. The `Value::Array` →
> `Cc<ArrayCell>` migration MUST land **behavior-neutral and byte-identical** — exactly like the V11-T2
> `ObjectCell` migration did — BEFORE any freeze behavior (Task D3) is added. Do not mix freeze logic
> into this task; the only observable difference after D2 is zero.

- [ ] **Step 3 — Implement `src/value.rs`** (see spec §4 "Frozen-flag representation"): add
  `frozen: Cell<bool>` to `ObjectCell` (`:23`) and `Instance` (`:168`); add it to `MapCell`/`SetCell`
  (`:55/74`) keeping their `Deref`/`borrow` API; introduce `ArrayCell { vec: RefCell<Vec<Value>>,
  frozen: Cell<bool> }` with `borrow()/borrow_mut()`/`is_frozen()`/`freeze()` shims and change
  `Value::Array(Cc<RefCell<Vec>>)` → `Value::Array(Cc<ArrayCell>)`. All constructors default
  `frozen=false`. Add `is_frozen()`/`freeze()` accessors per kind, plus a free helper
  `pub(crate) fn frozen_kind(v: &Value) -> Option<&'static str>`. Confirm `Value::trace` is unaffected
  (`Cell<bool>` adds no traceable edge) and the whole-corpus differential + goldens stay byte-identical.
- [ ] **Step 4 — Run** `cargo build` + the FULL suite (the `Value::Array` wrapper is a wide refactor —
  use the `borrow()` shim to minimize churn; fix every access site). `cargo test 2>&1 | tail`
  (0 failures — no behavior change yet) + the whole-corpus three-way differential
  (`three_way_whole_corpus_*` + recorded goldens) **byte-identical** across the migration, the SAME bar
  the V11-T2 ObjectCell migration met. This is the riskiest single step; land it green BEFORE adding any
  freeze behavior (Task D3).
- [ ] **Step 5 — Commit:** `refactor(value): frozen flag on ObjectCell/MapCell/SetCell/Instance + ArrayCell wrapper (no behavior change)`.

### Task D3: freeze/isFrozen + mutation-site checks

- [ ] **Step 6 — stdlib.** Add `freeze`/`isFrozen` to `object::exports()` (`src/stdlib/object.rs:15`)
  and `object::call()` (`:215`): `freeze(x)` sets the flag (no-op for non-containers) and returns `x`;
  `isFrozen(x)` returns `Bool`. Update `deep_clone` (`:105`) so a clone starts unfrozen (it already
  builds fresh containers — verify). Update `docs/content/stdlib/object.md`.
- [ ] **Step 7 — Shared check.** Add `fn check_not_frozen(v: &Value, span) -> Result<(), Control>` →
  on `frozen_kind(v) == Some(k)` emit Tier-2 panic `"cannot mutate a frozen {k}"`.
- [ ] **Step 8 — Insert the check at EVERY mutation site (spec §4 list):** tree-walker `index_set`
  (`src/interp.rs:3376`, both arms) + `set_member` (`:2933`, both arms); VM `Op::SetIndex`
  (`src/vm/run.rs:1416`), `vm_set_prop` (`:3208`), `Op::AppendArray` (`:1297`), `Op::AppendObject`
  (`:1316`), `Op::SpreadObject`; stdlib `array.rs` in-place mutators (`push/pop/shift/unshift/splice/
  clear/sort/reverse/fill`) + `map.rs` (`set/delete/clear`). Check BEFORE the write.
- [ ] **Step 9 — Run all Phase-D tests** → byte-identical both engines; `object.rs` unit tests for the
  flag accessors.
- [ ] **Step 10 — Phase-D gate** (BOTH feature configs — `std/object` is core, so `--no-default-features`
  must pass) + the perf gate (`cargo test --release --test vm_bench -- --ignored --nocapture`: a single
  `Cell<bool>` read per mutation must not regress ≥2×) + smoke + `examples/frozen.as`.
- [ ] **Step 11 — Commit:** `feat: object.freeze/isFrozen + frozen-mutation guards on both engines`.

---

## Phase E — Records / auto-derived `init`

**Files:** `src/interp.rs` (`construct`, `:2423`), `src/vm/run.rs` (`vm_construct`, `:3283`),
`src/check/rules/call_arity.rs` (record construction arity). Tests `tests/vm_differential.rs`,
`tests/aso.rs`, `tests/check.rs`.

### Task E1: failing differential tests

- [ ] **Step 1 — Write failing tests:**

```rust
// positional auto-constructor in field-declaration order
assert_vm_run_matches_treewalker(
    "class Point { x: number\n y: number }\nlet p = Point(1, 2)\nprint(p.x)\nprint(p.y)\n").await;
// defaulted field -> optional trailing param
assert_vm_run_matches_treewalker(
    "class P { x: number\n y: number = 0 }\nprint(P(1).y)\nprint(P(1, 2).y)\n").await;     // 0, 2
// arity too few / too many -> identical message both engines
assert_vm_run_error_matches_treewalker("class Point { x: number\n y: number }\nPoint(1)\n").await;
assert_vm_run_error_matches_treewalker("class Point { x: number\n y: number }\nPoint(1,2,3)\n").await;
// contract mismatch on a positional arg -> identical panic
assert_vm_run_error_matches_treewalker(
    "class Point { x: number\n y: number }\nPoint(\"a\", 2)\n").await;
// class WITH explicit init is unchanged (auto-init NOT applied)
assert_vm_run_matches_treewalker(
    "class C { x: number = 0\n fn init(v) { self.x = v + 1 } }\nprint(C(5).x)\n").await;   // 6
// inheritance: base fields then subclass fields, positional
assert_vm_run_matches_treewalker(
    "class A { a: number }\nclass B extends A { b: number }\nlet x = B(1, 2)\nprint(x.a)\nprint(x.b)\n").await;
```

- [ ] **Step 2 — Run, verify fail** (`has no init but was given N argument(s)`):
  `cargo test --test vm_differential record 2>&1 | tail`.

### Task E2: auto-init synthesis (both engines)

- [ ] **Step 3 — Tree-walker (`construct`, `src/interp.rs:2423`).** Replace the `None => if !args.is_empty()
  { panic "has no init…" }` branch (`:2460-2472`) with: if the class has no `init`, treat args as the
  auto-derived positional constructor — compute the ordered field list (`merged_field_schema` order),
  split required (no default) / optional (has default), validate arity with the §2 min/max logic
  (identical too-few/too-many messages), then for each provided positional arg contract-check
  (`check_type`/`contract_panic`, span = construct site) and insert into the instance fields (OVERRIDING
  the default already applied). Omitted trailing defaulted fields keep their default.
- [ ] **Step 4 — VM (`vm_construct`, `src/vm/run.rs:3283`).** Mirror in the `else if !args.is_empty()`
  branch (`:3348-3357`): same ordered-field binding + arity + contract check, then
  `resync_instance_shape(&instance)` (`:3365`).
- [ ] **Step 5 — Run all Phase-E tests** → byte-identical both engines; `.aso` round-trip of a record
  class (`tests/aso.rs`).

### Task E3: checker + gate

- [ ] **Step 6 — Checker (`call-arity`).** Extend `src/check/rules/call_arity.rs` so a call to a class
  with no `init` validates against the field count (min = required fields, max = total; skip with rest
  semantics N/A for fields). `tests/check.rs`: `Point(1)` flagged too-few, `Point(1,2)` silent,
  `Point(1,2,3)` flagged too-many; a class WITH `init` validates against the init params, not fields.
- [ ] **Step 7 — Corpus zero-FP guard:** `ascript check examples/*.as examples/advanced/*.as` → 0.
- [ ] **Step 8 — Phase-E gate** + smoke + add `examples/records.as` (record + defaulted field +
  inheritance + `instanceof` + `object.freeze`, deterministic, ends `print("records ok")`).
- [ ] **Step 9 — Commit:** `feat: records — auto-derived positional init for field-only classes (both engines)`.

---

## Phase F — `..=` field default (regression-lock + spec correction)

**Files:** `tests/vm_differential.rs`, `tests/aso.rs`; doc corrections in
`docs/superpowers/specs/2026-06-04-sp1-engine-parity-class-model-design.md` + `docs/content`.
**No engine code change expected** — verified working (spec §6).

### Task F1: lock the behavior + correct the stale note

- [ ] **Step 1 — Confirm** (the audit already verified): `class C { xs: array<number> = 1..=3 }` runs
  `[1, 2, 3]` on `ascript run`, `ascript run --tree-walker`, `ascript check` (exit 0), and `ascript
  build`+`run` of the `.aso`. Re-run to be sure on the SP2 branch.
- [ ] **Step 2 — Add regression tests** using `assert_field_default_matches` (`tests/vm_differential.rs:3250`):
  inclusive `1..=3`, stepped inclusive `0..=10 step 2`, both via `C()` and `C.from({})`; assert
  byte-identical both engines. Add a `tests/aso.rs` build+run of a class with a `..=` field default.
- [ ] **Step 3 — Lock the `yield`-default rejection** (it MUST stay rejected):
  `assert_vm_run_error_matches_treewalker("class C { x: number = yield 5 }\nC()\n")` — both engines
  exit non-zero, no output, identical message.
- [ ] **Step 4 — Correct the stale SP1 note** in
  `docs/superpowers/specs/2026-06-04-sp1-engine-parity-class-model-design.md` (the non-goal line and §4
  text saying "..= field default stays rejected") with a one-line "superseded by SP2: `..=` field
  defaults are supported; `yield` defaults remain rejected." Document `..=` field defaults in
  `docs/content`.
- [ ] **Step 5 — Phase-F gate + commit:** `test+docs: lock ..= field default (both engines) + correct stale SP1 note`.

---

## Phase G — Invariant gate + docs + holistic review

**Files:** `tests/vm_differential.rs`, `docs/content/*`, `docs/content/stdlib/object.md`,
`docs/superpowers/specs/2026-05-29-ascript-design.md`.

### Task G1: feature-coverage gate

- [ ] **Step 1 — Add** `tests/vm_differential.rs::sp2_features_run_byte_identical`: a curated program set
  exercising each SP2 feature together (instanceof + records + map literals + default params + freeze),
  asserting `assert_three_way_matches` (tree-walker == specialized-VM == generic-VM). Ensure the
  whole-corpus `three_way_*` tests still pass with the new examples in `examples/`.

### Task G2: docs

- [ ] **Step 2 — Update** `docs/content` language guide (`instanceof`, default parameters, `#{…}` map
  literals, records / auto-init) + `docs/content/stdlib/object.md` (freeze/isFrozen) + the language spec
  (`docs/superpowers/specs/2026-05-29-ascript-design.md`). Verify every documented snippet against the
  built binary (`target/release/ascript run`).
- [ ] **Step 3 — Commit:** `docs: instanceof, default params, map literals, records, object.freeze`.

### Task G3: holistic gate + perf

- [ ] **Step 4 — Full gate set** both feature configs + clippy both + `cargo test --release --test
  vm_bench -- --ignored --nocapture` (geomean ≥2×, no spec-vs-generic regression). Pay special
  attention to the freeze-check (every mutation) and default-eval (every call) overheads.
- [ ] **Step 5 — Independent review** (re-read spec, re-run gates, adversarial divergence hunt over the
  new surface: instanceof precedence + non-class rhs, default-eval ordering with rest, map-key
  canonicalization + unhashable rejection, frozen shallow semantics across all mutation sites + GC
  soundness, record arity + contract + inheritance order). Fix any divergence at the root.
- [ ] **Step 6 — Final commit** if review surfaced fixes; otherwise the sub-project is complete.

---

## Resolved decisions (owner)

These were the open design questions; the owner has decided them and this plan is built to them
(see the spec's "Resolved decisions" for full rationale):

- **D1 — `..=` field default already works (confirmed).** Phase F is a regression-lock test + the SP1
  stale-note correction only; `yield` field default stays rejected (symmetric).
- **D2 — `instanceof` is a RESERVED (hard) keyword (confirmed)** at the comparison tier, reusing the
  dead `Op::InstanceOf`; corpus-safe (no identifier named `instanceof`). Phase A.
- **D3 — array freeze uses the `ArrayCell { vec, frozen: Cell<bool> }` wrapper (confirmed)** — the ONE
  sanctioned `value.rs` representation change for SP2, mirroring the V11-T2 `ObjectCell` migration.
  Phase D sequences it as **its own behavior-neutral task (D2)** that must stay byte-identical across
  the migration BEFORE any freeze behavior is added; the `gc::cc_addr` side-table is rejected.
- **D4 — `#{...m}` map spread is OUT of v1 (confirmed).** A spread element inside `#{}` is a clean parse
  error. Phase C.
- **D5 — auto-init synthesized at the construction hooks (confirmed).** Replace the "has no init but
  given N args" branch in `construct` (`src/interp.rs:2423`) and `vm_construct` (`src/vm/run.rs:3283`)
  with positional field binding in `merged_field_schema` order, reusing the §2 default/arity logic +
  field contracts. No synthetic compiled `init` proto. Phase E.

## Self-review (author)

**Spec coverage:** §1 instanceof → Phase A; §2 default params → Phase B; §3 map literals → Phase C;
§4 object.freeze → Phase D; §5 records → Phase E; §6 `..=` field default → Phase F; invariant gate +
docs → Phase G. All six features + the cross-feature gate are covered.

**Placeholder scan:** No "TBD/handle edge cases". Test programs are concrete AScript verified against
the current binary's behavior where possible. The differential helpers (`assert_vm_run_matches_treewalker`,
`assert_vm_run_error_matches_treewalker`, `assert_three_way_matches`, `assert_field_default_matches`) are
the FILE'S ACTUAL helpers (confirmed in `tests/vm_differential.rs`). Exact Rust signatures/opcode
internals are deferred to the implementer (who reads the cited `compile/mod.rs`/`vm/run.rs`/`value.rs`
lines — the spec gives the change sites with line numbers).

**Type/name consistency:** `BinOp::InstanceOf` (matches the existing dead `Op::InstanceOf`);
`Param.default: Option<Expr>` (mirrors `FieldDecl.default`); `ExprKind::Map(Vec<MapEntry>)` +
`MapEntry { key: Expr, value: Expr }`; `Tok::HashBrace`/`SyntaxKind::HashLBrace`; `Tok::Instanceof`/
`InstanceofKw`; `Op::NewMap`/`Op::MapEntry`; frozen flags as `Cell<bool>` + `frozen_kind`/`check_not_frozen`
+ `ArrayCell`; `is_instance_of` shared helper. `ASO_FORMAT_VERSION` bumped EXACTLY once (9 → 10, in
Phase A; B/C/E reuse it; D/F don't touch bytecode). Panic message strings ("cannot mutate a frozen
<kind>", "instanceof requires a class on the right-hand side", "cannot use <type> as a map key", "a
required parameter cannot follow a defaulted parameter") are stated identically in spec and plan.

**Risk ordering:** Phase A (smallest, reuses a dead opcode) → B → C → D (the `ArrayCell` wrapper is the
single widest refactor; landed behavior-neutral in D2 before any freeze behavior) → E → F (no code) → G.

**Stale-note correction:** Phase F explicitly corrects the SP1 spec's "..= field default stays rejected"
line, which the audit proved stale. Decision D1 (owner-confirmed, above) sets this reduced scope for §6.
