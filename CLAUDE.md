# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

AScript is a small, dynamically-typed scripting language (`.as` files) with JavaScript-flavored
syntax, optional runtime-checked type contracts, and a batteries-included standard library. It is
implemented as a single Rust binary `ascript` containing an async tree-walking interpreter.

The design goal is **"Lua-simple language, Go/Deno-class standard library"**: the core stays tiny
(~8 value kinds, gradual contracts, no hidden control flow) while the stdlib is deliberately rich.
The authoritative design is `docs/superpowers/specs/2026-05-29-ascript-design.md` — the entire spec
(§§2–16) is implemented. `docs/superpowers/roadmap.md` is the milestone-by-milestone record.

## Documentation & examples

- **User-facing docs** live in `docs/` as a small static site: `docs/index.html` (landing),
  `docs/reader.html` (the reader app), `docs/assets/{styles.css,app.js}`, and the actual content as
  Markdown under `docs/content/` (language guide + per-domain stdlib reference). `app.js` `fetch`es
  the Markdown, so the site must be **served**, not opened from `file://` (`cd docs && python3 -m
  http.server`). The content Markdown is also readable straight from the repo.
- The stdlib reference pages are generated from the source modules; if you change a `std/*` API,
  update the matching `docs/content/stdlib/*.md` page.
- **`README.md`** is the repo front door (install, CLI, stdlib table, links into `docs/`).
- **Runnable examples:** `examples/*.as` (introductory; exercised by the conformance tests) and
  `examples/advanced/*.as` (production-shaped, fully error-handled — data pipeline, sqlite, crypto,
  fs, process, datetime, tui, plus HTTP/WebSocket/SSE client+server pairs). Verify with
  `target/release/ascript run <file>`.

> Language notes worth knowing when writing `.as` code or docs: `print` output is **buffered and
> flushed at program exit** (a forever-looping server won't stream logs live — use
> `serve({maxRequests:N})`). Template `${…}` interpolation fully supports nested string literals
> (incl. strings containing `}`/`{`/`${` and nested templates) — see the `template_interpolation_*`
> tests.
>
> **`?` is overloaded** and parsed in two places: postfix Result-propagation (`expr?`, `ExprKind::Try`)
> and the ternary `cond ? then : els` (in `ternary()`, just above assignment). They're
> disambiguated by `is_ternary_question()` — a `?` is a ternary only when a `:` follows at
> bracket-depth 0 before the statement ends; otherwise it's a `Try`. So `a ? -b : c` is a ternary
> but `f()? - 1` is propagate-then-subtract. The tree-sitter grammar mirrors this via a declared GLR
> conflict (`[$._expression, $.propagate_expression]`); **regenerate `parser.c` with
> `tree-sitter generate --abi 14`** after any grammar change. When touching `ExprKind`, the
> exhaustive matches in `interp.rs` (eval), `fmt.rs` (`write_expr_inner`), and `ast.rs` (`Display`)
> must each get an arm.
>
> **Unwrap `!` and the `?`/`!` precedence tier (class-shape-validation feature).** Postfix `!` is
> `ExprKind::Unwrap(Box<Expr>)` (force-unwrap of a `[value, err]` pair — yields `value`, or a
> *recoverable* panic carrying the original error message). Both `?` (`Try`) and `!` (`Unwrap`) live
> in `unwrap_tier()` in `parser.rs` — a precedence level **between `exponent()` and `unary()`**, i.e.
> **looser than `await` and prefix unary `!x`/`-x`** — so `await x!` parses as `(await x)!` and
> `await x?` as `(await x)?` with no parens. Mirror this tier in `fmt.rs` (`expr_prec`: a `PREC_TRY`
> level so the formatter does NOT wrongly parenthesize `await x?`/`await x!`). In the **tree-sitter
> grammar**, `unwrap_expression`/`propagate_expression` are deliberately **precedence-LESS** and
> resolved via declared GLR conflicts — **do NOT give them a `prec`, it breaks the ternary**.
> `ExprKind::Unwrap` needs arms in `interp.rs` (eval), `fmt.rs` (`write_expr_inner`), and `ast.rs`
> (`Display`).
>
> **Nullable suffix + typed class fields + `.from` validation (same feature).** `T?` is the nullable
> suffix (sugar for `T | nil`), parsed as `Type::Optional(Box<Type>)`, valid in EVERY type position
> (let/const/param/return/field) and rendered canonically as `T?`. A class body now allows
> `field_declaration` (grammar) — required (`id: number`), optional (BOTH `name: T?` and the marker
> `name?: T`, which lower to the SAME `Type::Optional` node; the formatter normalizes to `name: T?`,
> fields-before-methods), and defaulted (`role: string = "guest"`). Declared field types are checked
> on assignment (incl. inside `init`); see `Stmt::Class.fields` (`FieldDecl`) / runtime
> `FieldSchema`. `ClassName.from(obj, strict=false)` is a `Value::ClassMethod` that calls
> `validate_into` in `interp.rs` (recurses into nested class / `array<Class>` / `map<K,Class>` fields,
> applies defaults, recoverable field-path panic on mismatch, does NOT run `init`); it includes an
> **Object→Map boundary coercion** so a raw JSON dictionary `{...}` validates into a `map<K,Class>`
> field. The same `validate_into` core powers typed parse: `json.parse(text, Class)` and
> `resp.json(Class)` fuse a parse/decode failure and a shape mismatch into ONE Tier-1 `[value, err]`
> pair (no panic); the class is an ordinary value argument (no generics).
>
> **Object destructuring**: `let {a, b as local, "k" as v} = obj` binds by key from an `Object` or
> `Instance` (`Stmt::LetDestructureObject`); missing keys bind `nil`. Keys are `Ident | Str` (quote
> non-identifier keys); rename with the soft keyword `as`. A trailing `...rest` collector is active
> (see the rest note below).
>
> **Spread `...`** (`Tok::DotDotDot`): valid in array literals, object literals, and call args via
> typed-element AST (`ArrayElem`/`ObjEntry`/`CallArg`), so spread is unrepresentable elsewhere.
> Strict: spreading the wrong container is a Tier-2 panic; object-spread is later-value-wins with
> `IndexMap` keeping a key's first-seen position. After grammar changes, regen `parser.c` with
> `tree-sitter generate --abi 14`.
>
> **Rest `...name`**: rest params collect trailing args into an array (typed `...name: array<T>`,
> per-element checked) via a `has_rest` fast-path branch in `run_body` (non-rest calls byte-identical).
> Array/object rest patterns (`let [a, ...r]`, `let {a, ...r}`) collect the tail / leftover keys;
> object-rest excludes already-bound SOURCE keys. For async/`fn*`, arity/contract errors surface
> lazily when the future is driven.

## Commands

```bash
cargo build                              # build (default features = full stdlib)
cargo test                               # full suite (~540 tests, all features)
cargo test --no-default-features         # core language only (~245 tests)
cargo test <name>                        # run a single test by name substring
cargo test --test cli                    # run one integration test file (tests/*.rs)
cargo clippy --all-targets               # lint — must be clean in BOTH feature configs

cargo run -- run examples/hello.as       # run a .as program
cargo run -- repl                        # interactive REPL
cargo run -- fmt file.as                 # format in place
cargo run -- test file.as ...            # run a .as file's test(name, fn) registrations
cargo run -- lsp                         # language server over stdio (LSP)
```

Clippy must be clean under both `--all-targets` and `--no-default-features --all-targets`. Run both
before considering work done.

## Architecture

### Pipeline
`lexer` → `parser` (precedence-climbing) → `interp` (async tree-walker). The shared front-end
(`lexer`, `token`, `ast`, `parser`, `span`) is also consumed by `fmt`, `repl`, and the `lsp` (which
is static-analysis only and never instantiates the interpreter).

Source flows as: `lexer::lex(src)` → `parser::parse(&tokens)` → `Interp::exec`/`load_module`. Every
token and AST node carries a `Span` (byte offsets + line/col) so `diagnostics` (ariadne-backed) can
point at exact source. Entry points live in `src/lib.rs`: `run_file`, `run_source`, `run_tests`.

### The interpreter (`src/interp.rs`)
- `eval_expr`/`exec` are `async` (`#[async_recursion]`) and take **`&self`** (not `&mut self`) —
  `Interp` state lives behind interior-mutability cells (`RefCell`/`Cell`) so multiple eval futures can
  be live at once (M17). The whole runtime is `Rc`/`RefCell`-based and therefore **`!Send`** — the
  binary uses `#[tokio::main(flavor = "current_thread")]` and runs each program inside a
  `tokio::task::LocalSet`. Do not introduce `Send` bounds or a multi-thread runtime.
- **M17 async model (spec §7):** calling a script `async fn` returns a `Value::Future` and **eagerly
  schedules** the body via `tokio::task::spawn_local` (`self.rc()` yields an owned `Rc<Interp>` via a
  self-`Weak` installed by `install_self`); `await` drives a future to completion (`await` on a
  non-future is identity). Entry points (`run_file`/`run_source`/`run_tests` in `src/lib.rs`, repl) do
  `local.run_until(root).await; local.await;`. **Invariant: never hold a `RefCell` borrow across an
  `.await`** (enforced by clippy `await_holding_refcell_ref = "deny"` in `Cargo.toml`); for native
  resources use the take-out-across-await pattern (`take_resource` → await on the owned value →
  `return_resource`).
- **Structured concurrency / cancel-on-drop:** an async task's lifetime is bound to its
  `Value::Future` handle — dropping the last handle aborts the task (so an un-awaited, un-held async
  call is cancelled, not orphaned). `task.spawn` is the explicit detach (fire-and-forget); `race`
  cancels losers; `timeout` cancels the timed-out work. A cooperative yield above `INFLIGHT_YIELD_CAP`
  reaps finished/cancelled tasks so a tight un-awaited loop stays bounded (no memory growth).
- **Concurrency primitives:** `src/task.rs` has `SharedFuture` — split into a `ResultCell` (held by the
  spawned task, which resolves it) and a handle (behind `Value::Future`) that owns the task's
  `AbortHandle` and aborts on `Drop`; the task never holds the handle, so there is no reference cycle
  and the handle's last-drop genuinely cancels. It carries `Result<Value, Control>` so panics cross the
  task boundary. `std/task` (`src/stdlib/task_mod.rs`) provides `spawn`/`gather`/`race`/`timeout` over
  `future<T>`.
- **Generators & coroutines** (`src/coro.rs`): `fn*`/`async fn*` return a `Value::Generator` that is
  **consumer-driven** — the body is a lazily-polled `Pin<Box<dyn Future>>` (NOT a spawned task), driven
  one step per `resume`/`gen.next(v)`/`for await`. `yield` parks via `poll_fn`; a thread-local stack
  tracks the current generator for nested composition; `gen.close()` drops the body. Abandoning a
  generator just drops the future (no task → no exit hang). Surface syntax (`yield`, `fn*`, `async fn*`,
  `for await`, `future<T>`) touches the lexer, parser, tree-sitter grammar (regen `parser.c` with
  `tree-sitter generate --abi 14`), `fmt.rs`, and the LSP keyword list — and `ExprKind::Yield` needs
  arms in interp (eval), `fmt.rs` (`write_expr_inner`), and `ast.rs` (`Display`).
- **Control flow uses two enums, not `Result<_, io::Error>`:**
  - `Flow { Normal, Return(Value), Break, Continue }` — normal statement-level control flow.
  - `Control { Panic(AsError), Propagate(Value) }` (`derive(Clone)` so it can ride cross-task futures)
    — the error channel. `Panic` is an unrecoverable Tier-2 programmer error (aborts unless caught by
    `recover`); `Propagate` is the `?`-operator early return carrying a `[nil, err]` Result pair.
    `AsError` converts into `Control::Panic`.
- `global_env()` builds a fresh environment with builtins installed; programs run in a `.child()` of
  it so they can shadow builtins (`let len = 5`).
- **Native resource handles:** OS resources (TCP streams, child processes, HTTP bodies/servers, SSE,
  WebSocket connections, terminals) are NOT embedded in `Value`. They live in `Interp.resources`
  (a `RefCell<HashMap<u64, ResourceState>>`) and are referenced from script by a `Value::Native` handle
  id. This keeps `Value` cheap/cloneable and lets the interpreter reclaim fds deterministically. When
  adding a stateful native API, add a `ResourceState` variant + accessor methods on `Interp`, and never
  hold a `resources` borrow across an `.await` (take the state out first). The HTTP server handles each
  connection on its own `spawn_local` task with a `Semaphore` concurrency cap (M17).

### Values (`src/value.rs`)
`Value` is the ~14-variant runtime tagged union: `Bool`, `Number(f64)`, `Str(Rc<str>)`, `Builtin`,
`Function`, `Array`, `Object` (insertion-ordered `IndexMap`), `Map`, `Bytes`, `Regex`, `Native`,
`Enum`, `Class`, `Instance`, `Super`, plus M17's `Future` (identity-equal, backed by `SharedFuture`)
and `Generator` (identity-equal, a consumer-driven `GeneratorHandle`). Mutable containers are
`Rc<RefCell<...>>`. There is a separate
hashable `MapKey` (numbers canonicalized: −0.0→+0.0, NaN unified) for `Map` keys.

### Standard library (`src/stdlib/`)
Each `std/*` module is native Rust over the `Value` model. Two routing entry points in
`src/stdlib/mod.rs`:
- `std_module_exports("std/math")` → the `(name, Value)` bindings an `import` brings in.
- `call(module, func, args, span)` → routes qualified builtin calls (`"math.abs"`) to e.g.
  `math::call`.

To add a stdlib module: create `src/stdlib/foo.rs` exposing `exports()` and `call(...)`, register it
in both match arms of `src/stdlib/mod.rs`, declare the `pub mod` (gated by the right `#[cfg(feature)]`),
and add the example/test. Native functions are ordinary `function` values; argument-type misuse is a
Tier-2 panic.

### Feature flags (`Cargo.toml`)
The stdlib is split into Cargo features, all on by `default`: `data` (json/regex/encoding/csv/toml/
yaml/uuid/bytes), `datetime`, `intl`, `sys` (fs/process/env), `crypto`, `compress`, `sql` (sqlite),
`net` (tcp/http/ws/server), `tui` (crossterm), `lsp` (tower-lsp). Every module is `#[cfg]`-gated so
`--no-default-features` builds the bare language. `http3` is opt-in and additionally requires
`RUSTFLAGS="--cfg reqwest_unstable"` (reqwest's http3 is unstable).

### Tree-sitter grammar
`build.rs` compiles a vendored Tree-sitter parser from
`docs/superpowers/specs/grammar/tree-sitter-ascript/src/parser.c` via the `cc` crate.
`tests/treesitter_conformance.rs` asserts BOTH the grammar and the hand-written parser accept every
`examples/*.as` file with no errors. `tests/frontend_conformance.rs` is a differential parser
guardrail. If you change syntax, update both parsers and keep the examples passing.

## Tests
- Unit tests live inline (`#[test]` / `#[tokio::test]`) in `src/*.rs`.
- Integration tests in `tests/`: `cli.rs`, `modules.rs`, `lsp.rs`, `treesitter_conformance.rs`,
  `frontend_conformance.rs`. These spawn the built binary (`env!("CARGO_BIN_EXE_ascript")`).
- `examples/*.as` double as living documentation and are exercised by the conformance tests — keep
  them runnable.

## Conventions
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- Workflow per milestone (see roadmap): writing-plans → subagent-driven-development (a fresh
  implementer plus an *independent* reviewer that runs commands and probes edges) → holistic review →
  merge `--no-ff`. Plans live in `docs/superpowers/plans/`.
- Any spec deferral must be a documented, owner-noted Cargo feature or Tier-1 error — never a silent
  drop. Current deferrals: `http3` (feature), HTTP trailers (best-effort), `icu`/crossterm subsets,
  cross-file LSP features. M17 has three **architectural** non-goals (impossible under the approach-A
  async engine — documented in spec §7 and `docs/superpowers/specs/adr/2026-05-30-async-generators.md`,
  not code TODOs): durable/serializable continuations (needs an explicit-stack VM), robust unbounded
  deep recursion (needs stackful coroutines), and deterministic/replayable task scheduling.
