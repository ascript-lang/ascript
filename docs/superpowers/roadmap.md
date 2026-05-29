# AScript Implementation Roadmap

Durable tracker for the full build of AScript per `specs/2026-05-29-ascript-design.md`.
**Goal:** the entire language + standard library implemented, fully unit- and
example-tested, production quality, spec-compliant, **nothing left deferred**.

Execution model: one milestone at a time, each via writing-plans →
subagent-driven-development (implementer + independent reviewer per task + a
final per-milestone holistic review) → merge to `main`. Each milestone produces
working, tested software on its own.

**Status legend:** ✅ done · 🟡 in progress · ⬜ not started

## Phase 1 — Language core

- ✅ **M1 — Walking skeleton.** lexer, AST, precedence-climbing parser, async
  tree-walking interpreter, `print`, `ascript run` CLI. Merged.
- ✅ **M2 — Variables & control flow.** AST spans; `Environment`; full operator
  set (`+ - * / % **`, comparisons, equality, `&& || !`, `??`); `let`/`const`;
  assignment + compound assignment; optional `;`; blocks; `if/else`; `while`;
  `for (i in a..b)`. 44 lib + 3 integration tests. Merged.
  Plan: `plans/2026-05-29-ascript-phase1-m2-variables-control-flow.md`.
- ✅ **M3 — Functions & control-flow completion.** Flow signal
  (`Normal/Return/Break/Continue`); `fn` declarations + closures + `return`;
  `break`/`continue`; arrow functions; callable `Value::Builtin`/`Value::Function`
  with uniform call dispatch; recursion + arity checks. 62 lib + 4 integration
  tests. Merged. Plan: `plans/2026-05-29-ascript-m3-functions.md`.
- ✅ **M4 — Data structures.** Arrays `[…]`, objects `{…}` (insertion-ordered),
  member access `.`, indexing `[]`, optional chaining `?.` (full-chain
  short-circuit, spec §4), l-value member/index assignment, `for (x of …)` over
  arrays/strings, template strings, string `+` concat, trailing commas, `Paren`
  node. 86 lib + 5 integration tests. Merged. (Map kind → M8: no literal syntax.)
  Plan: `plans/2026-05-29-ascript-m4-data-structures.md`.
- ✅ **M5 — Result & error model.** `Control { Panic, Propagate }` error channel;
  `Ok`/`Err` + error objects; the `?` propagation operator; `assert`; panic tier
  (unrecoverable abort); `recover` (panic→Result). 94 lib + 6 integration tests.
  Merged. Plan: `plans/2026-05-29-ascript-m5-result-error-model.md`.
- ✅ **M6 — Gradual type contracts.** Optional annotations on let/const/params/
  returns; recursive `check_type` enforced at runtime (failure → recover-able
  panic); `number/string/bool/nil/any/fn/object/error`, `array<T>`, `Result<T>`
  (accepts Ok+Err), tuple, union. Also fixed: `//` + `/* */` comments (were
  missing). 107 lib + 7 integration tests. Merged. (map types → M8; class/enum
  types → M7.) Plan: `plans/2026-05-29-ascript-m6-type-contracts.md`.
- ✅ **M7 — Classes & enums + match.** Classes (construct/fields/methods/`self`),
  single inheritance (`extends`/`super`, defining-class-based resolution), simple
  enums (interned variants, `.name`/`.value`), `match` (literal/enum/wildcard/
  or-patterns, parsed below arrow precedence), `Type::Named` contracts
  (subclass-aware). 120 lib + 8 integration tests. Merged.
  Plan: `plans/2026-05-29-ascript-m7-classes-enums-match.md`.
- ⬜ **M8 — Modules.** ESM `import`/`export`, namespace import, module graph +
  once-only evaluation + cache.
- ⬜ **M9 — Tooling.** Rich diagnostics (ariadne/miette); REPL; `ascript fmt`;
  `ascript test` runner; Tree-sitter grammar conformance test.

(Phase 2+ stdlib milestones below shift accordingly; renumber when reached.)

## Phase 2 — Standard library: data & text

- ⬜ **M8 — Core collections.** `core` globals, `std/string`, `std/array`,
  `std/object`, `std/map`, `std/math`, `std/convert`.
- ⬜ **M9 — Serialization & encoding.** `std/json`, `std/regex`, `std/encoding`,
  `std/bytes`, `std/uuid`, `std/csv`, `std/toml`, `std/yaml`.
- ⬜ **M10 — Time & locale.** `std/time`, `std/date`, `std/intl` (pragmatic icu4x).

## Phase 3 — Standard library: system & async

- ⬜ **M11 — System.** `std/fs` (incl. `grep`), `std/process` (subprocess
  run/spawn), `std/env`, `std/crypto`, `std/compress`, `std/sqlite`.
- ⬜ **M12 — Async I/O.** `std/net/tcp`, `std/net/http` (client),
  `std/http/server`, `std/net/ws`.
- ⬜ **M13 — Terminal UI.** `std/tui`.

## Phase 4 — Tooling completion

- ⬜ **M14 — Language Server.** `ascript lsp` (tower-lsp) over the shared front-end.

---

## Working notes (carry forward across compaction)

- Single crate `ascript` (lib + bin); modules mirror future crate split (deferred
  until it earns its keep). Single-threaded; `Rc`/`RefCell`, never `Arc`.
- Spans are CHAR offsets (byte-offset precision lands with M7 diagnostics).
- Statements delimited structurally; `;` optional.
- IEEE-754 numerics intentional (`1/0` → inf), matching JS.
- async eval seam exists (`eval_expr` is `async`, `#[async_recursion(?Send)]`,
  current_thread tokio) — M12 async stdlib builds on it.
- Each milestone: new feature branch off `main`, subagent-driven TDD, merge `--no-ff`.
- Update this file's status markers as milestones complete.

### M3 design guidance (from M2 holistic review — read before planning M3)

- **Control-flow signal:** before adding `fn`/`return`, give `exec`/`exec_stmt` a
  flow signal (e.g. return `Result<Flow, AsError>` where `Flow` is
  `Normal | Return(Value) | Break | Continue`) so `return`/`break`/`continue` work
  uniformly inside `if`/`while`/`for`. Design this first.
- **Callable dispatch:** generalize `call_builtin`'s name-`match` into evaluating
  the callee to a `Value::Function` (closure capturing an `Environment`) or a
  builtin; dispatch on the value. `Environment` is already `Rc<RefCell<Scope>>` +
  `Clone`, so closures capture it directly — no structural change needed.
- **l-values:** `ExprKind::Assign` currently takes `name: String`. Member/index
  assignment (`obj.x = …`, `arr[i] = …`) needs a structured target
  (`target: Box<Expr>` resolved to a place); revisit `assignment()` desugaring.
- **`postfix` is the slot** for `.` member access, `[]` indexing, and `?.` (lexer
  already reserves bare `.`/`?` with M3-pointing errors).
- **`for-of`:** add a sibling `Stmt::ForOf { var, iter, body }`; `for_stmt` branches
  on `in` vs `of` after reading the loop var.
- Known acceptable edge (not a bug): for-range with non-integer/`inf` bounds follows
  IEEE semantics (`0.5..3.5` steps by 1.0; `0..(1/0)` loops forever).

### M4 design guidance (from M3 holistic review — read before planning M4)

- **Member access slots into `postfix()`** (parser): it currently loops only on
  `Tok::LParen` (call). Add `.`-member, `[]`-index, and `?.`-optional-chaining as
  sibling suffix arms. Because `Call` dispatches on an evaluated callee `Value`,
  method calls (`obj.f()`) compose for free once member access yields the callee.
- **l-value assignment:** `ExprKind::Assign` takes `name: String`. Member/index
  targets (`obj.x = …`, `arr[i] = …`) need a structured place; revisit
  `assignment()` and the `Assign` shape (likely `target: Box<Expr>`).
- **`for-of`:** add `Stmt::ForOf { var, iter, body }`; `for_stmt` branches on `in`
  (range) vs `of` (iterable) after reading the loop var.
- **Equality:** keep `Function` identity-compared (`Rc::ptr_eq`); arrays/objects get
  structural equality. `Value`'s manual `PartialEq`/`Debug` already anticipate this.
- **Lexer reservations updated:** lone `.` now points to M4, lone `?` points to
  M4 (`?.`) / M5 (`?` operator).
- **Watch (not a bug):** `return`/statement boundaries are newline-insensitive
  (optional `;`); revisit newline-significant termination before the surface grows
  much larger (templates, multiline literals).

### M5 design guidance (from M4 holistic review — read before planning M5)

- **Reclassify into Tier-2 panics (spec §6):** out-of-bounds index reads/writes
  (`interp.rs` `Index` arm + `assign_to`) and member-of-nil (`read_member`) are
  currently plain `AsError`s; M5 makes them panics. Safe accessors (`?.`, `??`,
  and a future `arr.get(i)`) stay nil-returning.
- **`AsError` likely needs a tier/severity** (Error vs Panic) so the `?` operator
  propagates recoverable Results distinctly from fatal panics; add `recover`
  boundary for the REPL/test-runner/host.
- **`Ok`/`Err` + `?`:** `Ok(v)`→`[v,nil]`, `Err(msg)`→`[nil,errObj]`; `?` postfix
  early-returns `[nil,err]` from the enclosing fn. Lexer `?` arm already reserved.
- **Known pre-existing (not M4):** very deep nesting (~450 levels of `[`/`(`/`.`/`${`)
  overflows the native stack (recursive parser+evaluator). A parser depth-guard
  returning an `AsError` would close it across the board — future hardening.
- **`(x) = 5`** (parenthesized assignment target) is rejected as "invalid
  assignment target" (Paren not assignable). Acceptable; revisit only if needed.

### M6 design guidance (from M5 holistic review — read before planning M6)

- **Contracts reuse the Panic tier:** a failed type contract is just
  `Control::Panic(AsError::at(...))`, exactly like `assert` (`interp.rs` assert arm).
  No new control mechanism needed; `recover` catches contract failures for free.
- **Annotation grammar:** the `Colon` token (M4) already exists for `name: Type`.
  Add type parsing for `let x: T = …`, `fn f(p: T): R { }`. Check contracts at
  bind/param/return sites; failure → panic.
- **`Result<T>` / `error` types** reference the pair shape: `Ok`→`[v,nil]`,
  `Err`→`[nil,{message}]`, `len()==2` invariant. Extract shared predicates
  (`is_result_pair`, `is_error_object`) — currently the structural check lives
  inline in the `Try` arm; share it so M6's `Result<T>` validation can't drift.
  Route construction through `make_pair`/`make_error` (the canonical builders).
- **Parametric depth (spec §5):** `array<T>`/`map<K,V>` contracts check eagerly to
  full declared depth at the check site; `any`/unparameterized opt out.

### M7 design guidance (from M6 holistic review — read before planning M7)

- **`Type::Named(String)`** is the new type variant for class/enum names. The
  parser's `parse_type_atom` unknown-ident arm (currently errors "Milestone 7")
  becomes `Tok::Ident(name) => Type::Named(name)` AFTER the known-primitive
  matches. The `map` arm stays deferred to M8.
- **`check_type` gains a `Named` arm:** inspect the value's class/enum tag. Needs
  class instances + enum values to carry their declared name. Enum types "accept
  any variant" (name-membership check, not structural — spec §5).
- **Classes:** `class`/`extends`/`super`/`self`/`init`; instances are tagged
  objects (reuse `Value::Object` + a class tag, or a dedicated instance value).
  Method resolution walks the class chain. `Type::Display`/`contract_panic` already
  handle a `Named` variant with `write!("{}", name)`.
- **Enums:** simple named variants (spec §8.2), optional backing value; interned
  tagged values; usable in `match` and as a `Named` contract type.
- **`match` expression:** patterns over literals, enum variants, `_` wildcard,
  or-patterns. Reuses the `match` keyword/tokens already lexed (M2 added `match`?
  check — if not, add the keyword).
- **Carried-over (not new):** `Ok(nil)` is structurally indistinguishable from an
  Err's nil success slot under Result checking — inherent to Result-as-[T,error],
  matches spec; do not try to "fix".

### M8 design guidance (from M7 holistic review — read before planning M8)

- **`run_source` (lib.rs) is the module-loader seam:** grow it into a loader keyed
  by resolved path with once-only evaluation + a module cache. Each module gets its
  own top-level scope; `std/*` paths resolve to built-in modules.
- **Exports are easy:** classes/enums/fns/consts are ordinary `env.define` bindings;
  `export` captures a module's top-level scope and exposes selected names. The value
  model needs no change — `Value::Class.def_env` and `Function.closure` already
  capture the defining scope, so cross-module resolution sees the right lexical env.
- **`map<K,V>` + `Map` value kind land together in M8** (parser already reserves
  `map`→error). Adding `Map` needs new arms in `PartialEq`/`Debug`/`Display`/
  `is_truthy`/`type_name`/`check_type` — same exhaustive-match discipline.
- **NOTE on ordering:** M8 in this roadmap = "Modules". The original Phase-2 stdlib
  numbering shifts; after M8 (modules) + M9 (tooling) come the stdlib milestones.
  The `Map` kind is needed by `std/map`, so it can be introduced either in the
  modules milestone or the first stdlib-collections milestone — decide when planning.
