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
- ✅ **M8 — Modules.** `export` decls, named + namespace (`* as`) imports, relative
  `.as` path resolution, once-only cached eval, circular-import partial-init,
  `run_file` entry point + module loader. 120 lib + 9 cli + 4 module tests. Merged.
  Plan: `plans/2026-05-29-ascript-m8-modules.md`.
- ✅ **M9 — Tooling.** clap multi-command CLI; ariadne source-pointing diagnostics;
  REPL (rustyline, TTY/non-TTY, persistence, panic isolation); `ascript fmt`
  (idempotent); `ascript test` (`test()` builtin + runner); Tree-sitter grammar
  reconciled + generated + conformance test; **async/await surface syntax (§7)**.
  128 lib + 14 cli + 4 module + 2 conformance tests. Merged.
  Plan: `plans/2026-05-29-ascript-m9-tooling.md`.

## ✅✅ PHASE 1 COMPLETE — language core + modules + tooling (spec §§2–10). ✅✅
Everything below is **Phase 2+** (the standard library + LSP), deferred to the next
goal. A fresh conversation starts here; see "Phase 2 starting point" notes at the end.

## Phase 2 — Standard library: data & text

- ✅ **M10 — Core collections.** `core` globals (`len`/`type`/`range`), `std/string`,
  `std/array`, `std/object`, `std/map`, `std/math`, `std/convert`; the **`Map` value
  kind + `MapKey` + `map<K,V>` contract type** (the one §4/§5 item deferred from the
  language); and **array destructuring `let [a, b] = expr`** (spec §6 — a Phase-1 gap
  closed here, since the whole stdlib returns `[value, err]` pairs). Native dispatch via
  qualified `Value::Builtin("module.fn")` routed through `call_stdlib`; `std/*` imports
  resolve from a static registry (`src/stdlib/mod.rs`) cached under a synthetic `<std>/`
  key. 183 lib + 15 cli + 5 module + 2 conformance tests. Merged.
  Plan: `plans/2026-05-29-ascript-m10-core-collections.md`.
- ✅ **M11 — Serialization & encoding.** `std/json`, `std/regex`, `std/encoding`,
  `std/bytes`, `std/uuid`, `std/csv`, `std/toml`, `std/yaml`. Introduced the
  **`Value::Bytes`** (always-on) and **`Value::Regex`** (feature-gated) value kinds, a
  default-on **`data` Cargo feature** (§12.4) gating the crate-backed modules, and one
  shared `Value`↔`serde_json::Value` converter reused by json/toml/yaml (with
  `preserve_order` so all three keep source key order). **Also closed three Phase-1
  front-end conformance gaps** discovered en route: (1) hex/binary/scientific/underscore
  number literals (`0xFF`/`0b1010`/`1e9`/`1_000`); (2) single-quoted strings + escape
  sequences (`\n`/`\"`/`\\`/…); (3) range `..` as a general operator + `let` without
  initializer; plus a builtin-shadowing fix (programs/modules run in a child of the
  builtins scope, so user code and imports can shadow `len`/`type`/`test`/…). Added
  `tests/frontend_conformance.rs` (a ~150-snippet differential guardrail). 241 lib + 16
  cli + 2 frontend + 5 module + 2 conformance tests (266 default; 236 `--no-default`).
  Merged. Plan: `plans/2026-05-29-ascript-m11-serialization.md`.
- ✅ **M12 — Time & locale.** `std/time` (now/monotonic/sleep⚡/durations — **the first
  async stdlib fn**, awaits `tokio::time::sleep` via `call_time`, suspension verified),
  `std/date` (chrono; instants as plain objects, parse/format/arithmetic with month-clamping,
  offset timezones), `std/intl` (pragmatic `icu` subset — real CLDR number/case/collation;
  documented currency/date fallbacks per the spec's "trimmed icu4x"). Features `datetime`
  (chrono) + `intl` (icu) added to default; tokio gained `time`. NO new value kind (dates are
  objects). 268 lib + 17 cli + 2 frontend + 5 module + 2 conformance (294 default; 244
  `--no-default`). Merged. Plan: `plans/2026-05-29-ascript-m12-time-locale.md`.

## Phase 3 — Standard library: system & async

- ✅ **M13 — System.** `std/fs` (read/write/append/stat/mkdir/remove/readDir/walk/path
  helpers + recursive **`grep`** §11.3), `std/process` (async `run`+`spawn`, §11.4 — non-zero
  exit≠err, spawn-fail=err, check/timeout/capture/shell/env, child handle with async
  reader/writer), `std/env`, `std/crypto` (sha256/512/md5/hmac/randomBytes/argon2/bcrypt —
  vectors cross-checked, constant-time verify), `std/compress` (gzip/deflate/zip), `std/sqlite`
  (connections/statements/transactions). Introduced the **native resource-handle mechanism**
  (`Value::Native` + `Value::NativeMethod` + an interp `resources` table; non-Clone OS resources
  live in the table, the value is a cheap id-handle; `read_member`→method binding→async
  `call_native_method`) — sqlite + process are its first consumers; **reused by M14** (http
  streaming/sse/sockets). Resource lifecycle verified leak-free (spawn+drain+wait → table
  returns to baseline). Features `sys`/`crypto`/`compress`/`sql` (default-on). 346 lib + 19 cli
  + 2 frontend + 5 module + 2 conformance (374 default; 245 `--no-default`). Merged.
  Plan: `plans/2026-05-29-ascript-m13-system.md`.
- ✅ **M14 — Async I/O.** `std/net/tcp` (listener+stream), `std/net/http` (the full §11.5
  modern client — verbs/headers/query/auth, body json/form/multipart/streamed, timeouts/
  redirects/retries/decompress/tls/cookies/proxy/httpVersion/errorOnStatus/cancel, streaming
  response bodies, and first-class `http.sse` SSE with auto-reconnect), `std/http/server`
  (hand-rolled HTTP/1 over hyper deps, routes/handlers/middleware/params, sequential — handler
  panic→500 with the loop surviving, header/body limits + read timeout), `std/net/ws`
  (WebSocket client+server, opts headers/auth). **Design note: NO new future/awaitable kind was
  needed** — §7 + §11.5 await every async API at its own call site (no hold-a-future /
  concurrent-spawn primitive), so the existing inline-async-dispatch model (`await` identity;
  async builtins/NativeMethods await inside their dispatch) already satisfies §7. Network handles
  are `Value::Native` (M13 mechanism); the §11.4 reader idiom is reused for http streaming bodies.
  Feature `net` (default-on); `http3` nested (default-off, needs `RUSTFLAGS=--cfg reqwest_unstable`).
  §11.5 deferrals: HTTP/3 (feature-gated), response trailers (best-effort empty object), SOCKS
  (shipped via reqwest feature). All tests use in-process `127.0.0.1:0` fixtures (no external
  network). 426 lib + 20 cli + 2 frontend + 5 module + 2 conformance (455 default; 245
  `--no-default`). Merged. Plan: `plans/2026-05-29-ascript-m14-async-io.md`.
  **`std/net/http` client must be modern — full spec in §11.5:**
  - **SSE:** first-class `http.sse(url, opts)` (dedicated entry, not a request flag) —
    parses `event:`/`data:`/`id:`/`retry:`, dispatches on blank-line boundaries,
    multi-line `data:`, `lastEventId`, auto-reconnect honoring the server `retry:`.
  - **Streaming:** streaming response bodies via the `std/process` reader idiom
    (`read(n?)`/`readLine()`/`readToEnd()`, string-or-bytes per `bodyMode`, `nil` at EOF)
    and streamed request bodies (bytes/reader/async generator). Backpressure-aware on
    the Tokio loop.
  - **Protocol versions:** HTTP/1.1 + HTTP/2 + HTTP/3(QUIC), ALPN negotiation,
    `httpVersion` pin + `resp.version` report. Backing: **`reqwest`** (h1/h2 baseline,
    h3 via `quinn`/`h3` behind a Cargo feature).
  - **Modern features:** keep-alive/pooling, redirects+policy, gzip/deflate/brotli/zstd,
    connect/read/total timeouts, retries+backoff, cancellation, TLS config, cookies,
    multipart/form-data, JSON/form helpers, custom headers, proxy.
  - **Deferred/best-effort (justified in §11.5):** HTTP/3 (feature-gated, opt-in),
    response trailers (best-effort; hyper-level for first-class), SOCKS proxy (feature).
- ✅ **M15 — Terminal UI.** `std/tui` (feature `tui`, default-on): `crossterm`-backed Terminal
  handle (`Value::Native`) + a hand-rolled double-buffered screen (Cell/Color/Attrs); raw mode,
  alt screen, cursor control; drawing primitives (setCell/text/hline/vline/box/fill + `dump()`
  snapshot) with styling (color names / `[r,g,b]` / 0-255 + bold/underline/italic/reverse); flush
  (per-cell diff render); key/mouse/resize events (`pollEvent`/`readEvent`, filtered to
  Press/Repeat — no Windows dup keys); an off-screen `buffer(w,h)` constructor. Pragmatic
  crossterm-over-ratatui (hand-rolled buffer = "basic widgets & drawing", per the spec's
  "crossterm/ratatui" license — documented). The testable core (buffer ops, diff, event→object,
  style parsing) is unit-tested without a tty; coordinate inputs validate-then-cast (huge/negative/
  fractional → clean Tier-2; in-range OOB clips). 471 lib + 21 cli + 2 frontend + 5 module + 2
  conformance (501 default; 245 `--no-default`). Merged. Plan: `plans/2026-05-29-ascript-m15-tui.md`.

## Phase 4 — Tooling completion

- ✅ **M16 — Language Server.** `ascript lsp` (tower-lsp, feature `lsp`, default-on) over the
  SHARED lexer/parser/AST (no second front-end): inline diagnostics (lex/parse errors with
  UTF-16-correct ranges), document symbols (fn/class+methods/enum+variants/const/let), completion
  (keywords + builtins + `std/*` module paths in import context + namespace-import exports after
  `alias.`), hover (keyword/builtin/decl markdown), go-to-definition (params → local lets →
  top-level decls). **Static-analysis only** (never runs the interpreter) → `Send+Sync` Backend
  (`Mutex<HashMap<Url,String>>`, no `Rc`/`Value`), runs on the current-thread runtime. Added
  `span`/`name_span` to AST declaration nodes + `Param.name_span` (purely additive — every match
  site uses `..`, zero behavior change). A real end-to-end protocol smoke test spawns `ascript lsp`
  and drives initialize→didOpen→publishDiagnostics→documentSymbol→hover→shutdown over framed
  JSON-RPC (bounded against hangs). 509 lib + 21 cli + 1 lsp + 2 frontend + 5 module + 2 conformance
  (540 default; 245 `--no-default`). Merged. Plan: `plans/2026-05-29-ascript-m16-lsp.md`.

---

## Phase 5 — Concurrency & coroutines (post-spec extension)

- ✅ **M17 — Async Concurrency + Generators/Coroutines (Architecture A).** Turn the async
  model from "sequential inline, `await` is identity" into real cooperative concurrency on the
  single-threaded tokio runtime, then expose the interpreter's *existing* stackless-coroutine
  nature as script-level generators/coroutines — one engine, no `unsafe`, no CPS rewrite.
  Calling an `async fn` returns an eagerly-scheduled `Value::Future`; `await` actually drives it
  (identity on non-futures, for back-compat); `std/task` adds `spawn`/`gather`/`race`/`timeout`
  over `tokio::task::LocalSet` + `spawn_local` (accepts `!Send`, so `Rc`/`RefCell` is preserved).
  `yield` is a real `.await` on an internal single-consumer rendezvous → generators (`fn*` /
  `async fn*`), bidirectional resume (`gen.next(v)`), and `for await`. Runtime joins all spawned
  tasks before exit (structured drain). New interpreter invariant: **never hold a `RefCell` borrow
  across an `.await`** (clippy `await_holding_refcell_ref = deny`). New value kinds `Value::Future`
  + `Value::Generator`; new `future<T>` contract type. **Documented deferrals** (deliberate
  Architecture-A boundaries, each needs a different engine): durable/serializable continuations
  (needs explicit-stack VM "B2"); robust unbounded deep script recursion (needs stackful "B1" or
  "B2"); deterministic/replayable scheduling (needs "B2"). Spec §7 rewritten; ADR at
  `specs/adr/2026-05-30-async-generators.md`. Plan:
  `plans/2026-05-30-async-generators-coroutines.md`.
  **Update 2026-06-04 (#147 resolved):** end-of-program task-drain parity for a *held*
  un-awaited future is confirmed — both the tree-walker and the VM run
  `local.run_until(..).await; local.await;` (`src/lib.rs`), so a held future's body
  (`let f = work()`) drains and runs identically on both engines. Asserted by
  `vm_held_future_drains_identically_to_treewalker` in `tests/vm_differential.rs`.

## Phase 6 — Ergonomics & observability (post-spec extension)

- ✅ **M18 — Destructuring, spread/rest, print streaming, `std/log`.** A batch of
  JS-flavored ergonomics plus live output and structured logging:
  - **Object destructuring** `let {a, b as local, "k" as v} = obj` — binds by key from
    an `object` or class `Instance`; missing key → `nil`; keys are `Ident | Str` (quote
    non-identifier keys); rename via the soft keyword `as` (`Stmt::LetDestructureObject`).
  - **Spread `...`** (`Tok::DotDotDot`) in array literals `[...a, b]`, object literals
    `{...o, k: v}`, and call args `f(...args)` — typed-element AST
    (`ArrayElem`/`ObjEntry`/`CallArg`). Strict: spreading the wrong container kind is a
    Tier-2 panic; no array↔object coercion. Object-spread is later-value-wins with
    `IndexMap` first-seen key position.
  - **Rest collectors** — rest params `fn f(a, ...rest: array<T>)` (array-type spelling,
    per-element checked; bare `...rest` untyped; must be last) via a fast-path branch in
    `run_body` (non-rest calls unchanged); array-rest `let [a, ...rest] = arr`;
    object-rest `let {a, ...rest} = obj` (excludes already-bound SOURCE keys). Empty
    rest = `[]`/`{}`. For `async`/`fn*`, arity/contract errors surface lazily.
  - **Print streaming** — `print` streams live to stdout under the CLI `run` command
    (`OutputSink::Live`) and output survives a later panic; `run_source`/REPL/tests
    capture it (`OutputSink::Capture`). `run_file` now returns `Result<(), AsError>`.
  - **`std/log`** — leveled (`debug/info/warn/error`, default `info`, `ASCRIPT_LOG`
    env) structured logging; `setLevel`/`setFormat` (human/json); first non-object args
    → `msg`, object args merge as fields, reserved `level`/`msg` always win; thunk
    first-arg defers work past the level filter; total serialization (cycles →
    `"[Circular]"`, functions → `"<function>"`, NaN → null, never panics) via
    `json::to_json_lossy`; emits to stderr (or capture in tests). `log` Cargo feature
    (default-on, depends on `data`).
  - **Front-end + tooling kept in lockstep:** Tree-sitter grammar (regenerated),
    LSP keyword/symbol surface, `fmt` round-trips, spec (§3 spread, §5 rest params,
    §6 destructuring, §11.6 `std/log`, §12.1 `OutputSink`) and docs all updated; 4 new
    examples (`object_destructuring.as`, `spread.as`, `rest.as`, `logging.as`).
  - **Test posture:** 682 passing (default features) / 370 (`--no-default-features`);
    `cargo clippy --all-targets` clean in both configs. Merged.

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

### M9 design guidance (from M8 holistic review — read before planning M9)

- **`std/*` resolution hook:** `resolve_import`/`load_module` (`src/interp.rs`) are the
  seam — add a `source.starts_with("std/")` check BEFORE `module_dir.join` to dispatch
  to a registry of built-in modules (native bindings or embedded source), bypassing the
  filesystem, with a non-path cache key. (Used by the Phase-2 stdlib milestones.)
- **Multi-file diagnostics:** `AsError` carries only `message` + `span` (offsets relative
  to each module's own source). For ariadne to point at the right FILE, thread the
  originating module path/id into `AsError`/`Control::Panic` during M9. This is the main
  diagnostics-layer change.
- **Live bindings deferred:** imports snapshot values at import time (named copies the
  Value; namespace builds a fixed IndexMap object). Spec-adequate. Full ESM live bindings
  would need the namespace to reference the module env instead of a copied map.
- **REPL note (from M8):** a module whose body panics still leaves a cached entry; for a
  long-lived REPL, consider evicting failed modules so re-import re-runs them.

### M11 design guidance (from M10 holistic review — read before planning M11)

- **The stdlib seam is proven and replicable.** Each module is `src/stdlib/<name>.rs` with
  `exports() -> Vec<(&'static str, Value)>` (binding name → `bi("name.fn")` or a constant
  `Value`) + a dispatcher. Pure modules use a free `pub fn call(func, args, span)`;
  callback/`self`-needing modules use `impl Interp { async fn call_<name>(...) }` (see
  `std/array`). Register in `std_module_exports` (the `import` registry) AND `call_stdlib`
  (the runtime router), both in `src/stdlib/mod.rs`. New modules: add `pub mod <name>;` there.
- **Shared helpers** in `mod.rs`: `bi`, `arg`, `want_number/string/array/object`, `clamp_index`.
  Add `want_*` helpers as new value kinds need them (keep the `"{ctx} expects a {type}, got
  {actual}"` message shape). `ctx = |f| format!("module.{}", f)` per module.
- **Tier-1 vs Tier-2 (spec §11.3):** fallible-on-data ops return `[value, err]` via
  `make_pair`/`make_error` (`pub(crate)` in interp.rs); wrong-arg-type is a Tier-2 panic via
  the `want_*` helpers. `std/convert` is the worked example (parseNumber = Tier-1, toNumber =
  Tier-2 coercion).
- **M11 introduces `Value::Bytes`** (recommended): a dedicated `Rc<RefCell<Vec<u8>>>` kind is
  cleaner than an array-of-byte-numbers and is also needed by `std/process` (M13) and
  `std/net/http` (M14). Adding it repeats the M10 `Map` discipline: new arms in `value.rs`
  (PartialEq/Debug/Display/`is_truthy` is automatic via the catch-all/`type_name`) and
  `interp.rs` (`type_name`, `check_type` if a `bytes` contract type is added, `len`). The
  compiler's exhaustiveness checker lists every match to update — add explicit arms, no `_`.
- **`std/json`** round-trips `Value` (object/array/number/string/bool/nil → JSON); `Map` has no
  JSON form, so JSON objects parse to `Value::Object`. New crates (`serde_json`, `regex`,
  `base64`, `hex`, `uuid`, `csv`, `toml`, `serde_yaml`) land here; gate under a `data` Cargo
  feature per spec §12.4.
- **Conformance/fmt:** any NEW surface syntax needs a Tree-sitter grammar update + an example
  exercising it (the conformance test parses every `examples/*.as` under both parsers). M10
  added no new syntax beyond what the grammar already had (`array_pattern`, `map_type`).
- **`run`/`run_err` async test helpers** now exist in `src/interp.rs` tests (lex+parse+exec a
  string → output / expected-panic `AsError`). Reuse them for stdlib e2e tests.

### M12 design guidance (from M11 holistic review — read before planning M12)

- **Feature groups:** M11 established the `data` Cargo feature (default-on) gating crate-backed
  modules, with cfg-gated `pub mod`/`std_module_exports`/`call_stdlib` arms AND cfg-gated value
  kinds (`Value::Regex`). M12 (time/date/intl) should add a `time` (or `intl`) feature the same
  way. **Critical:** if a milestone adds a feature-gated `Value` variant, gate EVERY exhaustive
  match arm and verify BOTH `cargo build` and `cargo build --no-default-features` compile (the
  `tests/frontend_conformance.rs` + the dual-config test runs are the guardrail).
- **`std/time.sleep` is the first ASYNC stdlib fn.** The async seam already exists (eval is async
  on current-thread tokio). Model it on `std/array`'s `call_array`: an `impl Interp { async fn
  call_time(...) }` that `await`s `tokio::time::sleep`. A real future/awaitable `Value` kind is
  NOT needed until M14 — `sleep` can await directly inside dispatch. Register the async arm in
  `call_stdlib` like array's (`"time" => self.call_time(...).await`).
- **Durations:** likely plain numbers (ms) or a small `{secs, nanos}` object — decide when
  planning. `now()`→unix ms (number), `monotonic()`→a monotonic number.
- **`std/date`:** `chrono` or `time` crate; civil dates parse/format/arithmetic/timezones. Map
  to AScript values (an object `{year, month, day, ...}` or a dedicated kind — prefer object to
  avoid a new value kind unless arithmetic ergonomics demand it).
- **`std/intl`:** trimmed `icu4x` (pragmatic subset) — locale-aware number/currency/date
  formatting, case folding, basic collation. This is the heaviest dep; keep the surface small
  and the feature optional.
- **Reuse the M11 patterns:** the shared `want_*` helpers, the `ctx` error closure, Tier-1
  `make_pair`/`make_error` for fallible parse (date parsing!), Tier-2 panic for arg-type misuse,
  and the cfg-gated module registration. `run`/`run_err` test helpers + per-module unit tests +
  an interp e2e + a capstone example + the holistic review remain the process.
- **Front-end is now solid** (numbers/strings/range/let/shadowing all conform to the grammar;
  the differential guardrail catches grammar-vs-parser drift). No known remaining Phase-1 gaps.

### M13 design guidance (from M12 holistic review — read before planning M13)

- **`std/process` + `std/fs` are the next async area** (after `time.sleep`). The async dispatch
  pattern is set: an `impl Interp { async fn call_<module> }` registered as
  `"<module>" => self.call_<module>(...).await` in `call_stdlib` (see `call_time`/`call_array`).
  `std/process` uses `tokio::process`; `std/fs` can be sync (std::fs) or async (tokio::fs) — the
  spec lists fs as non-async (no ⚡), so std::fs is fine; process is async.
- **The §11.4 reader idiom is shared with M14 http streaming.** `spawn`'s child handle exposes
  `stdout`/`stderr` as async readers with `read(n?)`/`readLine()`/`readToEnd()` (string-or-bytes
  per `capture`, `nil` at EOF), and `stdin` as a writer. This SAME reader/writer shape is reused
  by M14's `std/net/http` streaming bodies. **Strongly consider introducing a reusable async
  reader/writer representation now** (e.g. an object carrying a handle + native methods, or a
  small dedicated value kind) so M14 inherits it. A child handle / reader is the first stdlib
  object that owns OS resources + async methods — decide its representation carefully (object
  with `Value::Builtin` methods bound to an interp-side handle table, vs a new `Value` kind).
- **`std/fs.grep`** (spec §11.3) returns `[matches, err]` with `{path,line,column,text}`; reuse
  `std/regex` + a `walkdir`/`ignore` directory walker (respect `.gitignore` by default). Don't
  build a new search stack.
- **Features (§12.4):** add `sys`/`fs` (fs/process/env), `crypto` (RustCrypto: sha256/512, md5,
  hmac, random bytes, argon2/bcrypt), `compress` (flate2 + zip), `sql` (rusqlite). Gate each;
  add to default so tests run; keep `--no-default-features` building (no new ungated value kind,
  or cfg-gate it like `Value::Regex`).
- **`std/env`:** get/set env vars + dotenv loading (`dotenvy`). **`std/process`** cross-platform
  per §11.4 (run = one-shot capture, spawn = streaming handle; argv-by-default, shell opt-in;
  capture string/bytes/inherit/null; timeout; signals; exit code/signal). Read §11.4 carefully —
  it's the most detailed spec section after §11.5 (http).
- **Patterns to reuse:** shared `want_*` helpers, `ctx` closure, Tier-1 `make_pair`/`make_error`
  for fallible I/O (a non-zero process exit is NOT an err — it's a normal result with
  `success==false`; spawn FAILURE is the err — see §11.4), Tier-2 panic for arg-type misuse,
  cfg-gated registration, `run`/`run_err` test helpers, per-module unit + interp e2e + capstone
  example + holistic review.

### M14 design guidance (from M13 holistic review — read before planning M14)

- **M14 is the big async I/O milestone + the §11.5 modern HTTP client (the most detailed spec
  section after §11.4).** Modules: `std/net/tcp`, `std/net/http` (client, §11.5), `std/http/server`
  (hyper), `std/net/ws` (tokio-tungstenite). Backing: `reqwest` (http client), `hyper` (server),
  `tokio-tungstenite` (ws), all under a `net` feature; HTTP/3 behind a nested `http3` feature
  (default-OFF per §11.5 deferrals). SOCKS proxy + response trailers are §11.5-documented
  best-effort/feature-gated deferrals — carry them forward.
- **The native resource-handle mechanism (M13) is the foundation — REUSE it.** `Value::Native` +
  `Value::NativeMethod` + the interp `resources` table + `read_member`→method-binding→
  `call_native_method` dispatch are in place and proven (sqlite/process). HTTP streaming bodies,
  the SSE stream's `next()`, and TCP/WS sockets are all new `NativeKind`s with async methods —
  add variants to `NativeKind` + `ResourceState` and a `call_<module>_method` handler + a cfg arm
  in `call_native_method`, exactly like process did. **The §11.4 reader idiom IS the §11.5
  streaming-body idiom — keep `read(n?)`/`readLine()`/`readToEnd()` identical** (the `ProcReader`/
  resource-finalize-on-EOF patterns transfer directly; remember to finalize stream resources to
  avoid the fd leak M13 fixed).
- **M14 introduces a REAL future/awaitable `Value` kind** so `await` actually suspends on a value
  (today `ExprKind::Await(inner)` in interp.rs is identity — it just evaluates inner; `time.sleep`
  and `process` suspend *inside the call* via async dispatch, which works for fire-and-await but
  NOT for "hold a future, await later"). Sockets/http need `await someFuture` to suspend. Decide:
  (a) keep the "async builtin awaits inline" model where it suffices (simplest — http `get` can
  await inline like process.run), and (b) add a real awaitable kind only where the spec's API
  hands back a future the user awaits separately. §11.5's `await get(...)` / `await resp.json()`
  can all be inline-await (the call itself is async) — so a full future kind may STILL be deferrable
  if every async API awaits at its own call site. Re-examine §11.5/§7 carefully when planning;
  the async-dispatch precedent (call_time/call_array/call_process) likely covers http/ws too,
  with handles (connections/streams) as `Value::Native` and their methods async. Document the
  decision.
- **§11.5 is large** — SSE first-class (`http.sse`, not a request flag), streaming request+response
  bodies, HTTP/1.1+2+3, retries/backoff, redirects+policy, decompression, timeouts, cancellation,
  TLS config, cookies, multipart, proxy, auth helpers. Plan it as MANY tasks (likely split the http
  client across several). reqwest bundles most of this — lean on it. `std/http/server` (hyper) and
  `std/net/ws` are separate sub-areas.
- **Patterns persist:** shared `want_*`/`ctx`/`make_pair`/`make_error`; Tier-1 for network failures
  (connect/TLS/timeout/DNS = err; non-2xx = normal resp with `ok=false` unless `errorOnStatus`);
  Tier-2 for arg-type misuse; cfg-gated registration; dual-config builds; `run`/`run_err` helpers;
  per-module unit + interp e2e (network tests need a local test server or mocking — consider a
  `hyper`/`tokio` in-process test server, or gate live-network tests); capstone example; holistic.

### M15/M16 design guidance (from M14 holistic review — read before planning M15)

- **M15 — `std/tui`** (`crossterm`/`ratatui`, under a `tui` feature, default-on): raw mode, alt
  screen, screen buffer, key/mouse events, basic widgets & drawing. Mostly synchronous terminal
  I/O + an event loop. crossterm's event read is blocking/pollable — model an event source as a
  `Value::Native` handle with `pollEvent(timeoutMs?)`/`readEvent()` (async via `tokio` or a
  blocking read on a spawned task — but the interp is single-threaded; crossterm `event::poll`
  with a timeout on the current thread is simplest). A terminal/screen is a `Value::Native` handle
  (raw-mode guard, alt-screen enter/leave, draw cells, flush). ratatui for widgets is optional —
  the spec says "basic widgets & drawing", so a minimal buffer + a few widgets (text/box/list) may
  suffice; lean on ratatui if it integrates cleanly, else hand-draw via crossterm. TUI tests are
  hard (no real terminal in CI) — test the buffer/diff logic + event PARSING in isolation; gate
  any real-terminal test or skip it (document). The resource-handle + cfg-gating + Tier-1/2
  patterns all transfer.
- **M16 — `ascript lsp`** (`tower-lsp`, the LAST milestone): `ascript lsp` CLI subcommand running a
  language server over stdio. Reuse the shared front-end (lexer/parser → the conformance-tested
  grammar) + the `SourceInfo`/ariadne diagnostics groundwork from M9. Capabilities: diagnostics
  (lex/parse/runtime-contract errors via the existing `AsError`+`SourceInfo`), hover, completion
  (keywords + stdlib module/function names — you have the full stdlib registry in
  `std_module_exports`), goto-definition (within a file/module graph), document symbols. The
  cross-module deferred-error diagnostics limitation (M9 note: AST spans lack module provenance) is
  best addressed HERE — thread a module id into spans/`AsError` so multi-file diagnostics point at
  the right file. tower-lsp + tokio; the LSP is its own binary path (`ascript lsp`), tested via
  the lsp protocol (send initialize/didOpen/etc. and assert responses) or by unit-testing the
  analysis functions directly. This completes the spec (§10 tooling + §16) — after M16, EVERYTHING
  in the spec is implemented.
- **Patterns that persist:** writing-plans → subagent-driven-development (Opus implementer + independent
  reviewer per task + holistic) → merge --no-ff; cfg-gated features + dual-config builds; Value::Native
  for handles; Tier-1/Tier-2; `run`/`run_err` + in-process fixtures; capstone example + conformance.

## Roadmap status: M1–M16 ✅ ALL MERGED. **🎉 THE ENTIRE SPEC (§§2–16) IS IMPLEMENTED. 🎉**

AScript is **complete** per `specs/2026-05-29-ascript-design.md`:
- **Language (§§2–9):** lexer (incl. hex/binary/scientific/underscore number literals, single+double+
  template strings with escapes), precedence-climbing parser, async tree-walking interpreter; full
  operator set incl. range `..`; `let`/`const` (with destructuring + uninitialized); control flow;
  functions/closures/arrows; arrays/objects/maps/bytes; optional chaining + `??`; the two-tier
  error model (`Ok`/`Err`/`?`/`assert`/panic/`recover`); gradual runtime-checked type contracts
  (incl. `map<K,V>`); classes+inheritance+`super`/`self`; enums; `match`; ESM modules.
- **Tooling (§10):** `run`/`repl`/`fmt`/`test`/`lsp` CLI; ariadne source-pointing diagnostics; the
  conformance-tested Tree-sitter grammar; a differential front-end-conformance guardrail.
- **Standard library (§11):** all 28 `std/*` modules across data/text, serialization/encoding,
  time/locale, system (incl. the `Value::Native` resource-handle mechanism), async I/O (the full
  §11.5 modern HTTP client + server + TCP + WebSocket), and terminal UI.

**540 tests pass** (default features); **245** with `--no-default-features`; `cargo clippy
--all-targets` clean in both configs. Everything is unit- and example/integration-tested,
production quality, spec-compliant, merged to `main`. **Documented deferrals** (the only non-default
items, each justified + owner-noted): HTTP/3 (Cargo feature `http3`, default-off; needs
`RUSTFLAGS=--cfg reqwest_unstable`); HTTP response trailers (best-effort — reqwest high-level API);
SOCKS proxy (reqwest `socks` feature, shipped); pragmatic subsets of `icu` (intl) and `crossterm`-
over-`ratatui` (tui); LSP cross-file goto-def/rename/incremental-sync (per-document analysis ships).
Nothing else remains.

## Previous status: M1–M15 ✅ merged. PHASE 1 COMPLETE; PHASE 2 (M10–M15) done.
The AScript language (spec §§2–9) + tooling (§10: diagnostics, REPL, fmt, test,
Tree-sitter conformance) + async/await surface (§7) are fully implemented, unit- and
example-tested, clippy-clean, and merged to `main`. Remaining: Phase 2+ standard
library (M10–M15) + the LSP (M16).

---

## Phase 2 starting point (read this first in a fresh conversation)

**Where things are.** `main` is green: ~148 tests (128 lib + 14 cli + 4 module + 2
tree-sitter conformance), `cargo clippy --all-targets` clean. Single crate `ascript`
(lib + bin), single-threaded `Rc`/`RefCell`, current-thread tokio. The CLI has
`run`/`repl`/`fmt`/`test` subcommands (clap). Examples live in `examples/`; each is
covered by an integration test and the fmt-idempotence + tree-sitter conformance suites.

**Process (unchanged).** Per milestone: writing-plans → subagent-driven TDD (Opus
implementer + an independent Opus reviewer that checks spec-compliance AND code quality
AND runs the tests) → a final holistic review → merge `--no-ff`. One milestone per
feature branch off `main`. See [[prefers-opus-for-subagents]] and
[[ascript-full-build-goal]] in agent memory.

**The `std/*` resolution hook (the key Phase-2 seam).** `resolve_import`/`load_module`
in `src/interp.rs` currently canonicalize against the filesystem. Phase 2 must add a
`source.starts_with("std/")` branch BEFORE that, dispatching to a registry of built-in
modules. Two viable designs: (a) embed `.as` source for std modules written in AScript;
(b) native modules whose exports are Rust-backed builtins (`Value::Builtin` names the
interp dispatches in `call_builtin`). Most stdlib (string/array/math/json/regex/fs/…) is
native (b); use a non-path cache key (e.g. a synthetic `PathBuf` like `<std>/string`).

**`Value::Builtin` is the stdlib dispatch mechanism.** `call_builtin(name, args, span)`
in `src/interp.rs` already dispatches builtins by name (`print`, `Ok`, `Err`, `assert`,
`recover`, `test`). Stdlib functions are added as more arms (or a registry). Module-scoped
stdlib functions (`string.split`) resolve via the std module's exports → `Value::Builtin`.

**M10 (first stdlib milestone) introduces the `Map` value kind.** Add `Value::Map`
(`Rc<RefCell<…>>` with a hashable key — define a `MapKey` for number/string/bool/nil) and
new arms in `PartialEq`/`Debug`/`Display`/`is_truthy`/`type_name` (value.rs) and
`check_type` (interp.rs). Wire the `map<K,V>` type: `parse_type_atom` (parser.rs) currently
errors on `map` with "arrive in Milestone 8" — replace with real parsing; `check_type`
gains a `Map` arm. There is NO map literal syntax — maps are constructed by `std/map` (e.g.
`Map.new()`), which is why the kind waited for the stdlib.

**M14 (async I/O) introduces a real future/awaitable `Value` kind** so `await` actually
suspends on it (today `ExprKind::Await` is identity — see interp.rs; `is_async` flags are
carried but inert). Timers/sockets/http return awaitables; `await` drives them on the
existing current-thread tokio runtime. The async surface (parse/AST/fmt/grammar) is
already done and conformance-tested (`examples/async.as`).

### Known Phase-1 limitations to carry forward (none block Phase 1; address in noted phases)
- **Cross-module deferred-error diagnostics (M9 review):** a function defined in module A
  but called from B whose body panics renders B's source, not A's, because AST spans lack
  module provenance. The error MESSAGE is always correct; only the caret's file can be
  wrong for that case. Single-file + during-import diagnostics are perfect. Fix needs span
  provenance (tag spans/`AsError` with a module id) — do it in **M16 (LSP)** where
  multi-file diagnostics matter most. `SourceInfo` on `AsError` is the groundwork.
- **REPL is single-line:** multi-line blocks typed across lines aren't accumulated (single-
  line blocks work). Sanctioned v1 limitation; accumulate-on-incomplete is a nice follow-up.
  **Update 2026-06-04:** shipped — multi-line accumulation lands via the `is_incomplete`
  token-depth buffer on a `..` prompt (CLAUDE.md "REPL multi-line input"; `src/repl.rs`).
- **fmt drops comments** (AST pretty-printer) and re-emits string literals with `"` + raw
  contents (round-trips for current corpus). Future fmt hardening.
  **Update 2026-06-04:** superseded — the lossless CST formatter (`src/syntax/format`) is now
  the shipped formatter; it preserves comments and round-trips, replacing the AST pretty-printer.
- **Live module bindings:** imports snapshot values at import time (spec-adequate).
- **Spec authority:** `docs/superpowers/specs/2026-05-29-ascript-design.md`. The
  Tree-sitter grammar is the syntax source of truth, conformance-tested; keep it in
  lockstep when adding any new surface syntax (add an example exercising it).
