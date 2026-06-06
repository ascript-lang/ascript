# SP2 — New language features — Design

> **Status:** approved design, ready for implementation planning (superpowers:writing-plans).
> **Sub-project of** the post-cutover gap program (SP1 done; SP3–SP10 follow). Builds on SP1
> (`docs/superpowers/specs/2026-06-04-sp1-engine-parity-class-model-design.md`).

**Goal:** Add six surface-level language features that the SP1 spec explicitly deferred — `instanceof`,
default parameters, `#{…}` map literals, `object.freeze`/`isFrozen`, records (auto-derived `init`),
and `..=` as a field default — each implemented so it **runs byte-identical on both engines** (the
bytecode VM, default for `ascript run x.as` + REPL, and the `--tree-walker` reference oracle).

**Architecture:** Each feature is a focused change spanning some subset of: the lexer(s)
(`src/lexer.rs` legacy + `src/syntax/lexer.rs` CST), the three parsers (hand CST parser
`src/syntax/parser.rs` + ungrammar `src/syntax/ast/ascript.ungram` + tree-sitter
`docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js`, regen `parser.c --abi 14`) and the
legacy oracle parser (`src/parser.rs`), the AST (`src/ast.rs`), the resolver
(`src/syntax/resolve`), the compiler (`src/compile/mod.rs`), the VM (`src/vm/{run,opcode,disasm,verify,aso}.rs`),
the tree-walker (`src/interp.rs`), the value model (`src/value.rs`), the formatter
(`src/syntax/format`), the checker (`src/check/rules`), and the stdlib (`src/stdlib`). Every feature
is gated by the whole-corpus three-way differential (tree-walker == specialized-VM == generic-VM)
staying byte-identical, plus new per-feature differential tests.

**Tech stack:** Rust. CST front-end → resolver → compiler → `Chunk` → VM (default); legacy front-end
→ tree-walker (reference oracle). gcmodule GC (`Object` = `Cc<ObjectCell>`, `Map` = `Cc<MapCell>`,
`Array` = `Cc<RefCell<Vec>>`, `Instance` = `Cc<RefCell<Instance>>`). `.aso` versioned bytecode
(currently v9, `src/vm/aso.rs:74`).

---

## Critical project invariants (apply to EVERY feature here)

1. **Two engines, byte-identical.** The bytecode VM is the default for `ascript run x.as` and the
   REPL; the tree-walker is the reference oracle (`ascript run --tree-walker x.as`). Every feature
   must produce identical stdout + exit code on both. The whole-corpus three-way differential
   (`tests/vm_differential.rs`: `three_way_*`, tree-walker == specialized-VM == generic-VM) must stay
   green. New per-feature tests compare `ascript::vm_run_source` / `ascript::vm_run_source_generic`
   against `ascript::run_source_exit` (tree-walker).
2. **Syntax changes touch THREE parsers + the legacy oracle.** Any new surface syntax must be added
   to the hand CST parser (`src/syntax/parser.rs`), the ungrammar grammar (`src/syntax/ast/ascript.ungram`),
   and the tree-sitter grammar (`docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js`;
   regen with `tree-sitter generate --abi 14`), kept passing by `tests/treesitter_conformance.rs` +
   `tests/frontend_conformance.rs`. The **legacy parser** (`src/parser.rs`, used only by
   `--tree-walker`) ALSO needs the feature so the oracle accepts it.
3. **Both feature configs green** (`cargo test` + `cargo test --no-default-features`); **clippy clean**
   under `--all-targets` AND `--no-default-features --all-targets`; `await_holding_refcell_ref` stays
   `deny`. **Perf gate ≥2×** (`tests/vm_bench.rs`), no spec-vs-generic regression. No `unsafe`, no
   `#[allow]`, no `#[ignore]`, no stubs.
4. **GC discipline.** Don't break the gcmodule `Cc` + Bacon–Rajan cycle collector. Any new flag on a
   container must not introduce a new cycle-traceable edge into a native resource and must keep the
   `Value::trace` impl correct. Mutable containers stay `Cc<RefCell<…>>` / `Cc<…Cell>`.
5. **Per-task commit** with trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

## Codebase findings that shaped these decisions (verified)

- **`Op::InstanceOf` already exists but is DEAD.** The opcode is declared (`src/vm/opcode.rs:289-290`),
  has a disasm string (`src/vm/disasm.rs:250` → `"INSTANCE_OF"`), and a verifier stack effect
  (`src/vm/verify.rs:301` → `Effect::new(2, 1)`, pops `inst`+`cls`, pushes `bool`), and is listed in
  the round-trip opcode set (`src/vm/opcode.rs:708`). It is **never emitted by the compiler and never
  handled in `src/vm/run.rs`** — a leftover reservation from V9-T2. The only other `instanceof`
  reference is a stale doc-comment (`src/compile/mod.rs:1499`) that says "Superclass (`extends`),
  `super`, and `instanceof` are V9-T2" — `extends`/`super` shipped, `instanceof` did not. There is NO
  `instanceof` token, keyword, or parse rule in any parser. SP2 reuses the opcode and removes the stale
  comment.
- **`#` is entirely free.** A grep across `src/`, the tree-sitter grammar, and the lexers finds `#`
  only inside string literals (`src/stdlib/regex.rs`, `src/stdlib/tui.rs`) — never as a token.
  Comments are `//` and `/* */` (`src/lexer.rs:266`). `#{` can be a brand-new token with no collision.
- **`std/object` exists and is CORE (not feature-gated).** `src/stdlib/object.rs` already exposes
  `keys/values/entries/has/delete/merge/fromEntries/pick/omit/deepClone/deepEqual/mapValues`, is
  registered unconditionally (`pub mod object;` at `src/stdlib/mod.rs:51`, exports at `:102`, dispatch
  at `:320`), and builds under `--no-default-features`. `freeze`/`isFrozen` are added here. There is
  NO `freeze` today.
- **`..=` as a field default ALREADY WORKS on both engines.** `cst_default_expr` lowers a
  `RangeExpr` and accepts BOTH `DotDot` and `DotDotEq` (`src/compile/mod.rs:322-324`), and the
  tree-walker evaluates `ExprKind::Range { inclusive: true, .. }` the same way. Verified end-to-end:
  `class C { xs: array<number> = 1..=3 }` runs `[1, 2, 3]` on `ascript run`, `ascript run --tree-walker`,
  `ascript check` (exit 0), and `ascript build`+`run` of the `.aso`. **The SP1 spec's "..= field
  default stays rejected" note is STALE** — the inclusive range was completed as part of the SP1
  range-step work. SP2's feature #6 is therefore primarily a **test + documentation + spec-correction**
  task (regression-lock it; correct the stale notes). `yield` as a field default IS still rejected
  symmetrically (`src/compile/mod.rs:519-525`, verified: both engines exit non-zero, no output) — that
  stays.
- **`check_call_args` is the single shared arity/contract gate** (`src/interp.rs:3552`), used by both
  the tree-walker (`run_body`) and the VM CALL path. It currently does exact-arity or rest-arity. Both
  `Param` (`src/ast.rs:150`) and the CST/legacy param parsers (`src/syntax/parser.rs:544`,
  `src/parser.rs:529`) and both grammars have **no notion of a parameter default**. The `FieldDecl`
  AST already carries `default: Option<Expr>` (`src/ast.rs:321`) — a model to mirror for `Param`.
- **`construct` (tree-walker, `src/interp.rs:2423`) and `vm_construct` (VM, `src/vm/run.rs:3283`)** both
  apply merged field defaults base-class-first, then run `init` if present, and BOTH emit the identical
  error `"{class} has no init but was given {n} argument(s)"` when there is no `init` and args are
  passed (`src/interp.rs:2461-2470`, `src/vm/run.rs:3348-3357`). This is exactly the hook for
  records/auto-init. `Class` has no explicit `init` field — `init` is a `methods["init"]` entry.
- **Mutation sites for `object.freeze`:** tree-walker writes go through `index_set`
  (`src/interp.rs:3376` — `Array[i]=`, `Object[key]=`) and `set_member` (`src/interp.rs:2933` —
  `obj.k=`, `inst.f=`); the VM writes go through `Op::SetIndex` (`src/vm/run.rs:1416`), `Op::SetProp`
  → `vm_set_prop` (`src/vm/run.rs:2282/3208`), `Op::AppendArray` (`:1297`), and `Op::AppendObject`
  (`:1316`). Stdlib mutators: `array.push`/`pop`/etc. (`src/stdlib/array.rs:87`), `map.set`
  (`src/stdlib/map.rs:95`). All mutate via `.borrow_mut()`.
- **`MapKey::from_value`** (`src/value.rs:104`) canonicalizes number keys (−0→+0, all NaN unified),
  accepts `nil`/`bool`/`number`/`string`/`decimal`, and returns `None` for unhashable kinds
  (containers, functions, etc.) — the exact rejection set `#{…}` reuses.

---

## §1 — `instanceof` operator

### Current behavior (verified)
No surface syntax exists. The dead `Op::InstanceOf` opcode is defined (`src/vm/opcode.rs:290`,
`disasm.rs:250`, `verify.rs:301`) but never emitted or executed. `x instanceof C` is currently a
parse error in every parser.

### Target semantics
`x instanceof C` is a **binary operator** yielding a `bool`:
- `true` iff `x` is a `Value::Instance` whose class is `C` **or a subclass of `C`** (walk the
  `Class.superclass` chain via `Rc::as_ptr` identity, the same identity used by `find_method`).
- `false` for any non-`Instance` `x` (number, string, object, nil, enum, etc.) — never a panic.
- The right operand `C` must evaluate to a `Value::Class`. If it is not a class, that is a **Tier-2
  panic** `instanceof requires a class on the right-hand side` (anchored at the rhs span), symmetric on
  both engines. (`x instanceof 5` panics; `x instanceof nil` panics.)
- **Precedence: a comparison operator** — same tier as `< <= > >= == !=` (binds looser than `+`/`-`,
  tighter than `&&`/`||`). `a instanceof B && c` parses as `(a instanceof B) && c`. Left-associative;
  not chainable in a way that differs from other comparisons.

### Implementation
- **Token / lexing.** `instanceof` becomes a reserved keyword in both lexers: `src/lexer.rs` (add
  `"instanceof" => Tok::Instanceof` to the keyword match at `:493-516`) + `src/token.rs` (add
  `Tok::Instanceof`), and `src/syntax/lexer.rs::keyword_kind` (add `"instanceof" => InstanceofKw`) +
  `src/syntax/kind.rs` (add `InstanceofKw`, with `#[static_text("instanceof")]`). It is a HARD keyword
  (like `in`/`of`), NOT a soft one — `instanceof` as an identifier is extremely unlikely and reserving
  it keeps the comparison-tier parse unambiguous. (Corpus grep confirms no example uses `instanceof`
  as an identifier.)
- **AST.** Add `BinOp::InstanceOf` to `src/ast.rs:475` (the `BinOp` enum) + its `Display` arm
  (renders `instanceof`).
- **Legacy parser (oracle).** In `src/parser.rs::comparison` (`:1183`), recognize `Tok::Instanceof`
  alongside `Lt/Le/Gt/Ge` → `BinOp::InstanceOf`.
- **Grammar (CST).** ungrammar `BinaryExpr` op alternation (`src/syntax/ast/ascript.ungram:24`) gains
  `'instanceof'`. tree-sitter `binary_expression` table (`grammar.js:328-340`) gains
  `['instanceof', PREC.compare]`. Hand CST parser: add `InstanceofKw` to the comparison-tier operator
  set so it lowers to a `BinaryExpr` node with the `instanceof` operator. Regen `parser.c --abi 14`.
- **Compiler.** `compile_binary` maps `BinOp::InstanceOf` → emit operands then `Op::InstanceOf`.
  `cst_default_expr` (`src/compile/mod.rs:280`) gains the `InstanceofKw → BinOp::InstanceOf` arm so it
  is usable as a field default (consistent with the other comparison operators already there).
- **VM (`src/vm/run.rs`).** Add the `Op::InstanceOf` arm: pop `cls`, pop `inst`; if `cls` is not a
  `Value::Class` → the Tier-2 panic above; else push `Bool(is_instance_of(inst, cls))`.
- **Tree-walker (`src/interp.rs`).** `apply_binop` (`:3157`) gains a `BinOp::InstanceOf` arm calling the
  **same** `is_instance_of` helper, so message + span are byte-identical.
- **Shared helper.** Add `pub(crate) fn is_instance_of(v: &Value, class: &Rc<Class>) -> bool` (in
  `value.rs` or `interp.rs`) that walks the superclass chain by `Rc::as_ptr`. Single source of truth.
- **Formatter.** `BinaryExpr` formatting already renders the operator token; ensure `instanceof` emits
  surrounded by single spaces (`a instanceof B`). Add an idempotence test.
- **Checker.** No new lint required. The `super-misuse`/`call-arity`/etc. rules are unaffected. (A
  future "rhs-of-instanceof-is-not-a-class" static lint is explicitly out of scope.)
- **`.aso`.** `Op::InstanceOf` is already in the round-trip opcode set; once emitted it serializes with
  no format change. **Bump `ASO_FORMAT_VERSION` once for SP2** (9 → 10) because the bytecode now
  legitimately contains the opcode where old chunks never could and so older readers must reject —
  coordinate one bump across all SP2 features that change emitted bytecode (§1, §2, §3, §5).

### Tests
Differential, byte-identical both engines: instance `instanceof` its own class → true; subclass
instance `instanceof` parent → true; parent instance `instanceof` subclass → false; non-instance
(`5 instanceof C`, `"x" instanceof C`, `nil instanceof C`, an enum value) → false; `x instanceof 5`
→ identical Tier-2 panic both engines; precedence (`a instanceof B && c`, `1 + 0 instanceof C`);
`instanceof` as a field default. `.aso` round-trip of a program using `instanceof`.

---

## §2 — Default parameters

### Current behavior (verified)
`fn f(a, b)` and `(a, b) => …` accept a fixed param list with optional per-param type annotations and a
trailing `...rest`. `Param` (`src/ast.rs:150`) has `name`/`ty`/`name_span`/`rest` and **no default**.
`check_call_args` (`src/interp.rs:3552`) enforces exact arity (or `≥ n_fixed` with a rest). The
`call-arity` checker (`src/check/rules/call_arity.rs:85`) flags `arg_count != param_count` for
non-rest, non-spread calls.

### Target semantics
A parameter may carry a default: `fn f(a, b = expr)` and `(a, b = expr) => …`.
- **Min-arity = count of leading params with no default.** Defaulted params are optional. A default
  may NOT precede a non-default param (`fn f(a = 1, b)` is a **compile/parse error** —
  `a required parameter cannot follow a defaulted parameter`), symmetric on both engines.
- **Defaults evaluate at CALL time, LEFT-TO-RIGHT, only for omitted trailing args.** A default
  expression may reference **earlier already-bound params** (`fn f(a, b = a + 1)`) and any outer
  scope / global. An explicitly-passed arg (even `nil`) suppresses the default — only a MISSING
  trailing arg triggers it.
- **Composes with rest.** `fn f(a, b = 2, ...xs)` — `a` required, `b` defaulted, `xs` collects the
  rest. A defaulted param may carry a type annotation (`b: number = 2`); the default value AND any
  explicitly-passed value are both contract-checked (existing `check_type`).
- **Defaults are allowed on `fn`, arrows, methods, `init`, `async fn`, and `fn*`/`async fn*`** (for
  async/generators the arity/default work happens when the body is driven, consistent with the
  existing lazy arity surfacing noted in CLAUDE.md).

### Implementation
- **AST.** Add `default: Option<Expr>` to `Param` (`src/ast.rs:150`); update its `Display`/any
  pattern-matches. (One field, mirrors `FieldDecl.default`.)
- **Grammar.** ungrammar `Param` (`:14`) → `Param = '...'? 'ident' (':' Type)? ('=' Expr)?` (capture
  the default). tree-sitter `parameter` (`grammar.js:220`) → add `optional(seq('=', field('default',
  $._expression)))`. Hand CST parser `param_list` (`src/syntax/parser.rs:544-562`) → after the optional
  `: type`, consume `= <expr>` into the Param node. Regen `parser.c --abi 14`. (Note the existing GLR
  conflict around `(x) => …` vs parenthesized expr — a default inside the param list is unambiguous
  because it appears after a param name; verify the tree-sitter conflict list still resolves.)
- **Legacy parser (oracle).** `src/parser.rs::param_list` (`:529`) → after the optional `: type`,
  parse `Tok::Eq` + an expression into `Param.default`.
- **Resolver.** A param default expression is resolved in a scope where the **earlier params of the
  same function are already bound** (left-to-right), plus the enclosing scope. The resolver must
  introduce the param slots incrementally so `fn f(a, b = a)` resolves `a` as a param, not an upvalue.
- **`check_call_args` (`src/interp.rs:3552`) — the shared gate.** Generalize from exact-arity to
  **min/max arity**:
  - `min = params.iter().take_while(no rest).filter(|p| p.default.is_none()).count()` (leading required
    run); with no rest, `max = params.len()`; with a rest, `max = ∞`.
  - Error messages: too few → `"{what} expected at least {min} argument(s), got {n}"`; too many
    (no rest) → `"{what} expected at most {max} argument(s), got {n}"`. (When `min == max` and no
    default, keep the EXISTING exact-arity message `"expected {N}"` byte-for-byte so all current tests
    and goldens stay green.)
  - For each omitted trailing defaulted param, evaluate its default. **Because defaults can reference
    earlier params and run arbitrary code, default evaluation must be done by the engine (async,
    `&Interp`/`&Vm`), NOT inside the pure `check_call_args`.** Therefore `check_call_args` returns
    enough info (the bound prefix + which trailing defaults are missing) and the engine fills defaults.
    Concretely: split into a pure arity-validation that returns `BoundArgs { fixed: Vec<Value>,
    missing_defaults: Range }`, and have `run_body` (tree-walker) and the VM CALL path each evaluate the
    missing defaults left-to-right in the callee frame (so `b = a` sees the bound `a`), contract-check,
    and bind. The two engines share the param list and the default `Expr`/compiled-thunk, so they
    cannot diverge.
  - **VM lowering of defaults.** A defaulted param's default expression compiles to a thunk-like
    sequence the CALL path runs in the new frame when the arg is missing (mirrors how field-default
    thunks already work in `vm_construct`, `src/vm/run.rs:3322`). The tree-walker evals the `Expr`
    directly in the frame env.
- **Checker (`call-arity`, `src/check/rules/call_arity.rs`).** Replace the exact `param_count` compare
  with a **range**: compute `min`/`max` from the callee's params (count leading non-defaulted; `max`
  = total or ∞ with rest), and flag only when `arg_count < min` or (`no rest` and `arg_count > max`).
  Skip when a default references something the checker can't see (it doesn't need to — arity is purely
  structural). Keep the zero-false-positive corpus guard.
- **Formatter.** `params`/`params_from_list` (`src/syntax/format/mod.rs:390/411`) emit `name: T = expr`
  (canonical single spaces around `=`). Idempotence test.

### Tests
Differential, byte-identical both engines: `fn f(a, b = 10) { return a + b }` called `f(1)` and
`f(1, 2)`; default referencing earlier param `fn f(a, b = a * 2)`; default calling a global; default
with a type annotation (value + explicit both contract-checked, mismatch panics identically); arrow
default `(x, y = 5) => x + y`; default composing with rest `fn f(a, b = 2, ...xs)`; `nil` explicitly
passed suppresses the default; required-after-default → identical parse/compile error both engines;
too-few / too-many arity messages identical both engines; `call-arity` lint fires for `< min` and
`> max` (no rest) and is silent in range. `.aso` round-trip of a function with defaults.

---

## §3 — `#{…}` map literals

### Current behavior (verified)
There is **no map literal syntax**. `Value::Map` (`src/value.rs`, `Cc<MapCell>`) is only produced by
`std/map` (`map.new(...)`, `src/stdlib/map.rs`) and a few stdlib paths. `{…}` is always an object
literal (string keys). `#` is an unused character.

### Target semantics
`#{ k: v, 1: x, expr: y }` evaluates to a `Value::Map` with **arbitrary evaluated keys**:
- `#{}` is an empty map.
- Each entry is `<keyExpr>: <valueExpr>`; the key expression is **evaluated** (NOT a bare identifier
  name like object literals) and converted via `MapKey::from_value`. So `#{ a: 1 }` uses the VALUE of
  `a` as the key; to key by the string `"a"` write `#{ "a": 1 }`. (This is the deliberate distinction
  from `{a: 1}` object literals, where `a` is the literal key name.)
- **Key canonicalization** follows existing `Map` rules: numbers canonicalized (−0.0→+0.0, all NaN
  unified), `decimal`/`number`/`string`/`bool`/`nil` allowed.
- **Unhashable key** (a container, function, instance, etc. — anything `MapKey::from_value` returns
  `None` for) is a **Tier-2 panic** `cannot use <type> as a map key` (anchored at the key span),
  symmetric both engines — the same rejection `map.set` already applies.
- **Later-key-wins:** duplicate keys keep the LAST value (an `IndexMap` insert overwrites the value,
  keeping first-seen position). Insertion order = first-seen key order.
- Spread is **out of scope** for `#{…}` in SP2 (object/array/call spread is unchanged; a `...` inside
  `#{…}` is a parse error). This keeps the typed-element AST minimal; revisit if needed.

### Implementation
- **Token / lexing.** Add a `#{` token: legacy `Tok::HashBrace` (`src/token.rs` + `src/lexer.rs` —
  on seeing `#`, require the next char to be `{`, else lex error `unexpected character '#'`); CST
  `SyntaxKind::HashLBrace` with `#[static_text("#{")]` (`src/syntax/kind.rs`) recognized in
  `src/syntax/lexer.rs`. Lex `#{` as ONE token so it cannot be confused with `#` + `{`.
- **AST.** Add `ExprKind::Map(Vec<MapEntry>)` (`src/ast.rs:14`, near `Object`) where
  `MapEntry { key: Expr, value: Expr }` (a typed-element entry, mirroring `ObjEntry::KV` but with an
  **expression** key) — so a map literal is unrepresentable with a bare-name key. Add the `Display`
  arm (renders `#{k: v, …}`). Add the eval/compile/fmt arms (exhaustive matches in `interp.rs`,
  `compile/mod.rs`, `fmt.rs`, `ast.rs Display` per CLAUDE.md).
- **Grammar.** ungrammar: `MapExpr = '#{' (MapEntry)* '}'`, `MapEntry = key:Expr ':' value:Expr`; add
  `MapExpr` to the `Expr` alternation (`:16`). tree-sitter: `map_literal: $ => seq('#{',
  commaSep($.map_entry), optional(','), '}')`, `map_entry: $ => seq(field('key', $._expression), ':',
  field('value', $._expression))`, add to `_primary_expression`. Hand CST parser: in the primary-expr
  position, on `HashLBrace` parse a `MapExpr` node. Regen `parser.c --abi 14`. (Note: `map_entry`'s key
  is an `_expression`, unlike `object_entry`'s `choice($.identifier, $.string)` — this is what makes
  keys evaluated.)
- **Legacy parser (oracle).** In the primary-expression parser (`src/parser.rs`), on `Tok::HashBrace`
  parse comma-separated `expr : expr` entries into `ExprKind::Map`.
- **Resolver.** Both key and value expressions are ordinary resolved expressions (no new binding).
- **Compiler / VM.** Add `Op::NewMap` (+ an append/`MapEntry` op, mirroring `NewObject`/`AppendObject`
  at `src/vm/opcode.rs:225/253`): `NEW_MAP` pushes an empty `Value::Map`; per entry, eval key+value
  then `MAP_ENTRY` does `MapKey::from_value` (panic on `None`) + later-wins insert. Add disasm strings,
  verifier stack effects, and the round-trip opcode set entry. Bump `.aso` version (the single SP2
  bump, see §1).
- **Tree-walker.** `eval_expr` `ExprKind::Map` arm: eval each key+value left-to-right, `MapKey::from_value`
  (panic on `None`, identical message/span), later-wins insert into a fresh `MapCell`.
- **Formatter.** Emit `#{key: value, …}`; `#{}` for empty. The map-key is an arbitrary expression, so
  format it as an expression (NOT the object-key quoting logic). Idempotence test.
- **Checker.** No new lint. Existing expression lints traverse key/value naturally.

### Tests
Differential, byte-identical both engines: `#{}` empty; `#{ "a": 1, "b": 2 }`; numeric/bool/nil keys;
key from a variable's VALUE (`let k = "x"\n#{ k: 1 }` → keyed by `"x"`); later-key-wins
(`#{ 1: "a", 1: "b" }` → `"b"`); −0/NaN canonicalization; unhashable key (`#{ [1]: 2 }`) → identical
Tier-2 panic both engines; iteration / `map.*` interop on a `#{…}` literal; `#{…}` inside a function /
field default. `.aso` round-trip. Confirm `#` not followed by `{` is a lex error both lexers.

---

## §4 — `object.freeze` / `object.isFrozen`

### Current behavior (verified)
No freeze concept exists. `std/object` is core (`src/stdlib/object.rs`) with no `freeze`/`isFrozen`.
Mutation flows through `index_set`/`set_member` (tree-walker), `Op::SetIndex`/`SetProp`/`AppendArray`/
`AppendObject` (VM), and stdlib `array.push`/`map.set`/etc.

### Target semantics
`object.freeze(x)` **shallow-freezes** a mutable container and **returns `x`** (for chaining).
`object.isFrozen(x)` returns a `bool`.
- Freezable kinds: `Object`, `Map`, `Array`, `Instance`. Freezing any other value is a no-op that
  returns it unchanged (and `isFrozen` of a non-container is `false`). (Decision: be permissive on
  non-containers rather than panic, matching JS `Object.freeze` ergonomics.)
- **Shallow:** freezing a container does NOT freeze its element values.
- A subsequent **mutation of a frozen container is a Tier-2 panic** with message
  `cannot mutate a frozen <kind>` (`<kind>` ∈ object/map/array/instance), anchored at the mutation
  site span, **byte-identical on both engines.** Mutations covered: index-assign (`a[i]=`,
  `o[k]=`), member/field assign (`o.k=`, `inst.f=`), `array.push`/`pop`/`shift`/`unshift`/`splice`/
  `clear`/`sort`/`reverse` (any in-place array mutator), `map.set`/`delete`/`clear`, and the VM's
  `AppendArray`/`AppendObject`/`SpreadObject` when targeting a frozen container.
- Freezing is **idempotent and one-way** (no `unfreeze`).

### Frozen-flag representation (DECISION + analysis)
The flag lives **on the container payload**, NOT in a side-table, to keep lookups O(1) and avoid a
global identity map that the GC would have to reason about:
- `ObjectCell` (`src/value.rs:23`) → add `frozen: Cell<bool>` (defaults `false`). The cell already
  carries a `shape: Cell<u32>`; adding a second `Cell` is a minimal, non-cloning, non-traced field.
- `MapCell` / `SetCell` (`src/value.rs:55/74`) → change from a newtype tuple to a small struct
  `{ map: RefCell<…>, frozen: Cell<bool> }` (keep the `Deref` to the inner `RefCell` so the ~existing
  `m.borrow()/borrow_mut()` sites are unchanged) **OR** keep the tuple and add a second field
  `SetCell(RefCell<…>, Cell<bool>)`. Mirror `ObjectCell`'s pattern for consistency.
- `Array` is `Cc<RefCell<Vec<Value>>>` (`src/value.rs:371`) — it has no wrapper to hang a flag on.
  **Decision (D3, owner-confirmed): wrap arrays the same way as objects** — introduce
  `ArrayCell { vec: RefCell<Vec<Value>>, frozen: Cell<bool> }` with `borrow()/borrow_mut()` helpers, and
  change `Value::Array(Cc<RefCell<Vec>>)` → `Value::Array(Cc<ArrayCell>)`. **This is the ONE sanctioned
  `value.rs` representation change for SP2** — a wide-but-mechanical refactor that **mirrors the V11-T2
  `ObjectCell` migration**: the `borrow()/borrow_mut()` shim keeps most array access sites textually
  unchanged, and it is the only representation that keeps the flag local and the differential
  byte-identical without a side-table. It is sequenced as **its own task** in the plan and landed
  **behavior-neutral** (whole-corpus three-way differential + goldens byte-identical across the
  migration, exactly as ObjectCell was) BEFORE any freeze behavior is added. The side-table alternative
  (keyed by `gc::cc_addr`) is rejected — it adds a global map the GC must not retain plus an
  identity-lifetime concern.
- `Instance` (`src/value.rs:168`) → add `frozen: Cell<bool>` (defaults `false`), beside the existing
  `shape_id: Cell<u32>`.
- **`Cell<bool>` is `Copy`/no-op-`Trace`-safe:** it adds no new traceable edge, so `Value::trace` is
  unaffected and the GC is untouched. `Cell` (not `RefCell`) so a `&self` engine can set/read it
  without a borrow conflict and without any await-holding-borrow risk.

### Mutation-site instrumentation (every site, both engines)
A shared helper `fn frozen_kind(v: &Value) -> Option<&'static str>` returns `Some("array"|"object"|
"map"|"instance")` iff the value is frozen, else `None`; and `fn check_not_frozen(v, span) -> Result<(),
Control>` emits the panic. Insert the check at the START of each mutation, BEFORE the write:
- Tree-walker: `index_set` (`src/interp.rs:3376` — Array + Object arms), `set_member`
  (`src/interp.rs:2933` — Object + Instance arms).
- VM: `Op::SetIndex` (`src/vm/run.rs:1416`), `vm_set_prop` (`:3208`, used by `Op::SetProp`),
  `Op::AppendArray` (`:1297`), `Op::AppendObject` (`:1316`), `Op::SpreadObject`.
- Stdlib: `array` in-place mutators (`src/stdlib/array.rs` — `push/pop/shift/unshift/splice/clear/
  sort/reverse/fill`), `map` mutators (`src/stdlib/map.rs` — `set/delete/clear`). These receive the
  receiver value, so they check it directly.
NOTE: object/array/map LITERAL construction and `vm_construct`/`construct` field population build a
FRESH (unfrozen) container, so the check is only on user-visible mutation paths — freezing happens
after construction.

### Implementation
- `src/value.rs`: the four flag additions above (+ the `ArrayCell` wrapper + `borrow` shims +
  `is_frozen()`/`freeze()` accessors per kind), and any `ObjectCell::new`/`MapCell::new` constructors
  default `frozen=false`. Update `deep_clone` (`src/stdlib/object.rs:105`) and any place that
  reconstructs containers to start unfrozen (a clone of a frozen object is NOT frozen — JS semantics).
- `src/stdlib/object.rs`: add `freeze`/`isFrozen` to `exports()` (`:15`) and `call()` (`:215`); these
  are pure (no callback), so they live in the top-level `call`, not `call_object`'s callback path.
- Insert `check_not_frozen` at every mutation site listed above.
- `docs/content/stdlib/object.md`: document `freeze`/`isFrozen`.

### Tests
Differential, byte-identical both engines: freeze an object/array/map/instance then mutate via each
path (`a[0]=`, `o.k=`, `inst.f=`, `arr.push`, `map.set`) → identical `cannot mutate a frozen <kind>`
panic; `freeze` returns the same value (chaining); `isFrozen` true after freeze, false before/for
non-containers; shallow (element of a frozen array is still mutable); freeze of a non-container is a
no-op returning it; deep-clone of a frozen object is unfrozen; freeze is idempotent. Whole-corpus
differential + goldens stay byte-identical (the flag defaults false; unfrozen behavior is unchanged).
`.aso` not affected (freeze is a runtime call, not bytecode).

---

## §5 — Records / auto-derived `init`

### Current behavior (verified)
A class with declared fields but no `init` can ONLY be constructed with zero args; `C(1, 2)` panics
`"{class} has no init but was given {n} argument(s)"` (`src/interp.rs:2461`, `src/vm/run.rs:3348`).
Field defaults are applied first, then `init` runs if present.

### Target semantics
A class that **declares fields and has NO explicit `init`** gets an **auto-derived positional
constructor**:
- Parameters are the class's declared fields **in field-declaration order** (merged base-class-first,
  consistent with `merged_field_schema`). A **defaulted field becomes an optional trailing param**
  (so `class Point { x: number; y: number = 0 }` → `Point(x)` or `Point(x, y)`); the existing
  default-param min/max arity rules from §2 apply (required fields are the leading run; a required
  field after a defaulted field in declaration order is the same error as §2's
  required-after-default, surfaced symmetrically).
- Each positional arg is **assigned to its field and contract-checked** with the existing field
  type-contract (`check_type` / `contract_panic`), identical to a hand-written `init` that assigns
  `self.f = arg`.
- A class **WITH an explicit `init` is completely unchanged** (no auto-init).
- A class with **no fields and no init** keeps today's zero-arg behavior.
- No new keyword — "record-ness" is implicit (fields + no init).

### Implementation
- **Synthesis point.** The auto-init is synthesized where `construct`/`vm_construct` currently hit the
  "has no init but was given args" branch. Replace that branch (in BOTH engines) with: if the class has
  no `init` method, treat the call as the auto-derived constructor — after applying field defaults
  (already done above the branch), bind the positional args to the still-fields in declaration order:
  - Compute the ordered field list (`merged_field_schema` order). The required/optional split = fields
    without/with a default. Validate arity against that (reuse the §2 min/max arity logic so messages
    match: too few / too many).
  - For each provided positional arg, contract-check against the field's `ty` (`contract_panic` on
    mismatch, span = construct site) and insert into the instance fields, OVERRIDING any default
    already applied for that field. Omitted trailing (defaulted) fields keep their default.
  - Re-sync the VM instance shape (`resync_instance_shape`, `src/vm/run.rs:3365`) after population, as
    `vm_construct` already does.
- **Resolver/compiler.** Prefer synthesizing at construction time (runtime) over emitting a synthetic
  `init` method, to avoid grammar/AST changes — the field schema + defaults are already available to
  both engines via `Class.fields` / `class_defaults`. (If the implementer finds a synthetic compiled
  `init` proto cleaner for the VM, it must produce byte-identical behavior — the differential decides.)
- **Checker (`call-arity`).** The `call-arity` lint currently keys on functions with a param list; a
  record construction `C(args)` is a class call. Extend the arity rule (or add a small companion) so a
  call to a class with no `init` validates against the field count (min = required fields, max = total,
  or skip if any field default is non-trivial — arity is structural, so the field count suffices).
  This keeps `ascript check` honest about record construction arity. Zero-false-positive corpus guard.
- **`.aso`.** No new structure if synthesized at runtime (the field schema + default thunks already
  serialize). Covered by the single SP2 version bump if the implementer chooses a synthetic proto.

### Tests
Differential, byte-identical both engines: `class Point { x: number; y: number }` → `Point(1, 2)`
sets fields; `class P { x: number; y: number = 0 }` → `P(1)` uses the default, `P(1, 2)` overrides;
arity too-few / too-many → identical messages; contract mismatch (`Point("a", 2)`) → identical panic;
a class WITH `init` is unchanged (auto-init NOT applied); zero-field class unchanged; inheritance
(subclass fields appended after base fields in the positional order); record interop with `instanceof`
(§1) and `object.freeze` (§4). `.aso` round-trip of a record class.

---

## §6 — `..=` (inclusive range) as a field default

### Current behavior (verified — ALREADY WORKS)
`cst_default_expr` (`src/compile/mod.rs:315-345`) lowers a `RangeExpr` and accepts BOTH `DotDot`
(exclusive) and `DotDotEq` (inclusive) into `ExprKind::Range { inclusive, .. }`; the tree-walker
materializes it identically. Verified end-to-end on both engines + `check` + `.aso`:
`class C { xs: array<number> = 1..=3 }` → `[1, 2, 3]`. The SP1 spec's "..= field default stays
rejected" line is **stale** (the inclusive range shipped with the SP1 range-step work).

### Target
This feature is therefore primarily a **regression-lock + documentation correction**, NOT new code:
- Add explicit differential tests (`class C { xs: array<number> = 1..=N }` via `C()` and `C.from({})`)
  asserting both engines + `.aso` produce identical output, so the behavior is permanently locked.
- **Correct the stale SP1 spec note.** Add a one-line note in the SP1 spec §4 / non-goals that `..=`
  field defaults are now supported (superseded by SP2), and document `..=` field defaults in
  `docs/content`.
- **`yield` as a field default STAYS REJECTED** (`src/compile/mod.rs:519-525`) — verified symmetric
  (both engines exit non-zero, no output). No change; add a test asserting the symmetric rejection so
  it is locked.
- If, contrary to the verification, any path (e.g. a checker lint or the legacy oracle) rejects
  `..=` defaults, that path is fixed to match — but the audit found none.

### Tests
Differential: `..=` field default via `C()` and `C.from({})`, byte-identical both engines + built
`.aso`; stepped inclusive default `0..=10 step 2`; `yield` default still rejected symmetrically.

---

## Testing strategy (whole sub-project)

- **Differential oracle never relaxed.** Whole-corpus three-way (`tests/vm_differential.rs`,
  tree-walker == specialized-VM == generic-VM) byte-identical, plus recorded goldens, plus the new
  per-feature tests. Any divergence on valid code = fix the root cause; never weaken an assertion or
  edit a tree-walker test to match the VM.
- **Per-feature differential tests** added to `tests/vm_differential.rs` using the file's existing
  snippet helper, each comparing `vm_run_source` + `vm_run_source_generic` vs `run_source_exit`.
- **Both feature configs** (`cargo test` + `--no-default-features`). `std/object` (freeze) is core, so
  §4 must pass under `--no-default-features`.
- **Clippy clean** both `--all-targets` configs; `await_holding_refcell_ref` stays `deny`.
- **Perf gate ≥2×** (`tests/vm_bench.rs`), no spec-vs-generic regression — especially watch §4 (a
  freeze check on every mutation) and §2 (default-eval on every call): the frozen check is a single
  `Cell<bool>` read on the already-borrowed container, and default eval only runs when args are
  omitted, so neither should regress; the bench gate verifies.
- **Grammar:** regen `parser.c --abi 14` after §1/§2/§3 grammar changes; `treesitter_conformance` +
  `frontend_conformance` green.
- **`.aso`:** ONE `ASO_FORMAT_VERSION` bump for SP2 (9 → 10), shared by §1/§2/§3/§5; `tests/aso.rs`
  build+run round-trip for a program exercising all bytecode-affecting features; confirm a v9 `.aso`
  is rejected with the version-mismatch message.
- **`is_instance_of`, `frozen_kind`/`check_not_frozen`, the min/max arity split, and the auto-init
  field-binding loop are SHARED helpers** so the two engines cannot diverge.
- **Per-task commit** with the trailer. Independent per-phase review (re-read spec, re-run gates,
  adversarial divergence hunt) before sign-off.
- **Docs:** update `docs/content` (language guide: `instanceof`, default params, `#{…}` map literals,
  records; stdlib `object.md`: freeze/isFrozen) and the language spec
  (`docs/superpowers/specs/2026-05-29-ascript-design.md`).

## File-touch map (for the plan)

| Area | Files |
|---|---|
| Lexer | `src/lexer.rs` + `src/token.rs` (instanceof kw, `#{`); `src/syntax/lexer.rs` + `src/syntax/kind.rs` (InstanceofKw, HashLBrace) |
| AST | `src/ast.rs` (`BinOp::InstanceOf`, `Param.default`, `ExprKind::Map`+`MapEntry`, Display arms) |
| Legacy parser (oracle) | `src/parser.rs` (instanceof in `comparison`, param defaults in `param_list`, `#{…}` in primary, `ExprKind::Map`) |
| Grammar | `src/syntax/ast/ascript.ungram` + `grammar.js` (+ regen `parser.c`) + hand CST parser `src/syntax/parser.rs` |
| Resolver | `src/syntax/resolve/*` (param-default incremental binding, map key/value exprs) |
| Compiler | `src/compile/mod.rs` (`Op::InstanceOf` emit, param-default thunks, `Op::NewMap`/`MapEntry`, `cst_default_expr` instanceof arm, auto-init synthesis) |
| VM | `src/vm/{run,opcode,disasm,verify,aso}.rs` (InstanceOf exec, NewMap/MapEntry, frozen checks, auto-init in `vm_construct`, `.aso` v10) |
| Tree-walker | `src/interp.rs` (apply_binop InstanceOf, default-eval in `run_body`, `ExprKind::Map` eval, frozen checks in `index_set`/`set_member`, auto-init in `construct`, `check_call_args` min/max) |
| Value | `src/value.rs` (`is_instance_of`; frozen flags on ObjectCell/MapCell/SetCell/Instance + `ArrayCell` wrapper) |
| Stdlib | `src/stdlib/object.rs` (freeze/isFrozen), `src/stdlib/array.rs` + `src/stdlib/map.rs` (frozen mutator checks) |
| Formatter | `src/syntax/format/*` (instanceof spacing, param defaults, `#{…}` emission) |
| Checker | `src/check/rules/call_arity.rs` (min/max range + record construction arity) |
| Tests | `tests/vm_differential.rs`, `tests/aso.rs`, `tests/treesitter_conformance.rs`, `tests/frontend_conformance.rs`, examples |
| Docs | `docs/content/*`, `docs/content/stdlib/object.md`, language spec |

## Resolved decisions (owner)

The design questions raised during planning are RESOLVED as follows (owner sign-off); the body of
this spec already reflects them, recorded here for traceability.

- **D1 — `..=` field default is already shipped (confirmed).** Feature #6 is done in code (verified on
  both engines + `check` + `.aso`). SP2 §6 reduces to a regression-lock test + correcting the stale SP1
  spec note. `yield` as a field default STAYS rejected (symmetric on both engines). See §6.
- **D2 — `instanceof` is a RESERVED (hard) keyword (confirmed).** It is reserved like `in`/`of` (NOT a
  soft/contextual keyword) at the comparison-precedence tier, reusing the dead `Op::InstanceOf` opcode.
  Corpus-safe: no existing example uses `instanceof` as an identifier. See §1.
- **D3 — `object.freeze` array representation: the `ArrayCell` wrapper (confirmed).** `Value::Array`
  becomes `Cc<ArrayCell>` where `ArrayCell { vec: RefCell<Vec<Value>>, frozen: Cell<bool> }`, with
  `borrow()`/`borrow_mut()` shims to minimize churn — **mirroring the V11-T2 `ObjectCell` migration**.
  This is **the single sanctioned `value.rs` representation change for SP2**: one wide-but-mechanical
  refactor. It is sequenced as **its own task** in the plan's freeze phase, landed **behavior-neutral**
  (the whole-corpus three-way differential + goldens stay byte-identical across the migration, exactly
  as the ObjectCell migration was) BEFORE any freeze behavior is added. The side-table alternative
  (keyed by `gc::cc_addr`) is rejected — it adds a global map the GC must not retain and an
  identity-lifetime concern. See §4.
- **D4 — `#{...m}` map spread is OUT of v1 (confirmed, deferred).** A spread element (`...`) inside a
  `#{…}` map literal is a clean parse error. Object/array/call spread is unchanged. See §3.
- **D5 — auto-init via runtime synthesis (confirmed).** The auto-derived constructor is synthesized at
  the existing construction hooks — `construct` (tree-walker, `src/interp.rs:2423`) and `vm_construct`
  (VM, `src/vm/run.rs:3283`) — by REPLACING the "has no init but given N args" branch with positional
  field binding in `merged_field_schema` order, reusing the §2 default/arity logic + the field
  contracts. **No synthetic compiled `init` proto** is emitted; no AST/grammar change. See §5.
