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
- ⬜ **M5 — Result & error model.** `Ok`/`Err`, the `?` propagation operator,
  Result tier vs panic tier, `recover` boundary (spec §6).
- ⬜ **M6 — Gradual type contracts.** Annotation grammar; runtime contract checks
  at bindings/params/returns; `error`/`Result<T>` types; `array<T>`/`map<K,V>`
  depth checks; contract failures panic.
- ⬜ **M7 — Classes & enums + match.** `class`/`extends`/`super`/`self`/`init`;
  simple enums; `match` expression with patterns.
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
