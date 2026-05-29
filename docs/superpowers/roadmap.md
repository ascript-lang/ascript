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
- ⬜ **M3 — Functions & data.** `fn` declarations + closures + `return`; arrays
  `[…]`, objects `{…}`, maps; member access `.`, indexing `[]`, optional
  chaining `?.`; `for (x of iterable)`; template strings; the `?` Result operator
  + Result/panic tiers; generalize builtin dispatch to callable `Value`.
- ⬜ **M4 — Gradual type contracts.** Annotation grammar; runtime contract checks
  at bindings/params/returns; `error`/`Result<T>` types; `array<T>`/`map<K,V>`
  depth checks; contract failures panic.
- ⬜ **M5 — Classes & enums + match.** `class`/`extends`/`super`/`self`/`init`;
  simple enums; `match` expression with patterns.
- ⬜ **M6 — Modules.** ESM `import`/`export`, namespace import, module graph +
  once-only evaluation + cache.
- ⬜ **M7 — Tooling.** Rich diagnostics (ariadne/miette); REPL; `ascript fmt`;
  `ascript test` runner; Tree-sitter grammar conformance test.

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
