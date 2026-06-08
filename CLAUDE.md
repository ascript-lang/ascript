# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

AScript is a gradually-typed, multi-paradigm scripting language (`.as` files) with JavaScript-flavored
syntax, runtime-checked type contracts (plus an advisory static checker), first-class structured
concurrency, and a batteries-included standard library. It is implemented as a single Rust binary
`ascript`. The **default and production engine is an async bytecode VM** (CST front-end → resolver →
bytecode compiler → `Chunk` → VM, with inline caches, adaptive arithmetic, and a cycle-collecting GC).
The original async **tree-walking interpreter is retained** as a differential oracle and a
`--tree-walker` debugging engine — kept byte-for-byte behavior-identical to the VM, not a second dialect.

The design goal is **a focused core with a Go-class standard library**: the core stays approachable (a
small set of value kinds, gradual contracts, no hidden control flow) but is genuinely multi-paradigm —
object-oriented (classes, inheritance, `instanceof`), functional (closures, pattern matching, generators,
destructuring, ranges, lazy streams), and concurrent (`async`/`await`, structured concurrency, channels,
durable workflows, plus **shared-nothing workers** for multi-core parallelism — `worker fn` pools,
`worker class` actors, `worker fn*` streaming) — while the stdlib is deliberately rich. The authoritative design is
`superpowers/specs/2026-05-29-ascript-design.md` (the entire spec is implemented);
`superpowers/roadmap.md` is the milestone-by-milestone record.

## Documentation & examples

- **User-facing docs** live in `docs/` as a small static site: `docs/index.html` (landing),
  `docs/reader.html` (reader app), `docs/assets/{styles.css,app.js}`, and content as Markdown under
  `docs/content/` (language guide + per-domain stdlib reference). `app.js` `fetch`es the Markdown, so the
  site must be **served**, not opened from `file://` (`cd docs && python3 -m http.server`). The Markdown
  is also readable straight from the repo.
- The stdlib reference pages mirror the source modules; if you change a `std/*` API, update the matching
  `docs/content/stdlib/*.md` page. **Adding a NEW page** means adding its slug to the `NAV` array in
  `docs/assets/app.js` — the sidebar AND the cmd-K search both derive from `NAV`, so a page with no entry
  is unreachable (no link, no search hit). In-content links are resolved relative to the current page's
  directory (`](workflow)`, `](../language/syntax)`), not absolute-from-root. The language-guide pages are
  `docs/content/language/{syntax,values-types,classes-enums,type-contracts,errors,modules-async}.md`
  (note: `match`/generators/concurrency live inside those pages, not separate files).
- **`README.md`** is the repo front door (install, CLI, stdlib table, links into `docs/`).
- **Runnable examples:** `examples/*.as` (introductory; exercised by the conformance tests) and
  `examples/advanced/*.as` (production-shaped, fully error-handled). Verify with
  `target/release/ascript run <file>`.

> Note: under the CLI `run` command `print` **streams live to stdout** (`OutputSink::Live`) and is
> retained even if the program later panics; `run_source`/REPL/tests **capture** it
> (`OutputSink::Capture`). Template `${…}` interpolation fully supports nested string literals and
> nested templates (see the `template_interpolation_*` tests).

## Touching syntax — the cross-cutting checklist

Several rules recur for ANY change to the grammar/AST. Do all that apply:

- **Two parsers, regen the grammar.** A new/changed surface form touches BOTH the hand-written legacy
  `parser.rs` (precedence-climbing, the oracle front-end) and the CST parser, AND the tree-sitter grammar
  under `tree-sitter-ascript/` — after which you **regenerate `parser.c`** with `tree-sitter generate
  --abi 14`. `tests/treesitter_conformance.rs` + `tests/frontend_conformance.rs` are the guardrails.
- **Exhaustive `ExprKind`/`Pattern` matches.** A new `ExprKind`/`Pattern`/`Stmt` variant needs arms in
  `interp.rs` (eval), `fmt.rs` (`write_expr_inner`/`write_pattern`), and `ast.rs` (`Display`). Missing an
  arm is a compile error on purpose.
- **Both engines stay byte-identical.** Language *behavior* must match across tree-walker == specialized-VM
  == generic-VM over the whole corpus + goldens (`tests/vm_differential.rs`, both feature configs). Fix the
  engine, never relax the assertion.
- **`.aso` versioning.** Any opcode or serialization-layout change bumps `ASO_FORMAT_VERSION`
  (`src/vm/aso.rs`) and may need a `src/vm/verify.rs` update.
- **Publishing the grammar** (whenever you touch `tree-sitter-ascript/**`): run `./scripts/sync-grammar.sh`
  (subtree-splits + pushes to the `ascript-lang/tree-sitter-ascript` mirror, prints the new SHA), then bump
  that SHA in `editors/zed/extension.toml` (`commit`) and `editors/nvim/lua/ascript/treesitter.lua`
  (`revision`). CI `mirror-grammar.yml` also auto-mirrors, but the editor-pin bump is manual. See
  `CONTRIBUTING.md`.

## Language features — gotchas & where they live

Terse per-feature notes (the non-obvious bits; read the cited file for the rest):

- **`?` is overloaded** — postfix Result-propagation (`expr?`, `ExprKind::Try`) vs ternary `cond ? a : b`
  (`ternary()`, just above assignment). Disambiguated by `is_ternary_question()`: a `?` is ternary only
  when a `:` follows at bracket-depth 0 before the statement ends. So `a ? -b : c` is ternary but
  `f()? - 1` is propagate-then-subtract. Grammar mirrors this with a declared GLR conflict.
- **Unwrap `!`** (`ExprKind::Unwrap`) — force-unwrap of a `[value, err]` pair (yields `value`, or a
  *recoverable* panic with the original message). Both `?` and `!` live in `unwrap_tier()` — a precedence
  level between `exponent()` and `unary()`, i.e. **looser than `await` and prefix `!x`/`-x`** — so
  `await x!` parses as `(await x)!`. In the tree-sitter grammar `unwrap_expression`/`propagate_expression`
  are deliberately **precedence-LESS** (declared GLR conflicts; giving them a `prec` breaks the ternary).
- **`;` separators** — `;` is an optional statement separator (`skip_semicolons`) in top-level/block lists
  AND class bodies. Enums/match-arms/params/literals are comma-delimited and take no `;`. The formatter
  canonicalizes to newlines.
- **Nullable `T?` + typed class fields + `.from` validation + typed parse.** `T?` (sugar for `T | nil`,
  `Type::Optional`) is valid in every type position incl. `future<T>`. Class bodies allow `field_declaration`:
  required (`id: number`), optional (`name: T?` or the `name?: T` marker — same `Type::Optional`), and
  defaulted (`role: string = "guest"`); field types are checked on assignment incl. inside `init`
  (`FieldDecl`/`FieldSchema`). `ClassName.from(obj, strict=false)` (`Value::ClassMethod`) runs
  `validate_into` in `interp.rs` — recurses into nested class / `array<Class>` / `map<K,Class>`, applies
  defaults, recoverable field-path panic on mismatch, does NOT run `init`; includes an Object→Map boundary
  coercion. The same `validate_into` powers typed parse: `json.parse(text, Class)` / `resp.json(Class)`
  fuse decode + shape mismatch into ONE Tier-1 `[value, err]` pair (the class is an ordinary value arg, no
  generics).
- **`std/schema` fluent chaining** (call-site hook, additive). Refiners + `parse` chain as methods on a
  schema value in addition to free functions. Schemas stay tagged Objects (`{__kind:"string", …}`) — NO new
  `Value` variant. The hook lives ONLY in the `Call` evaluator: when the callee is `Member{object,name}`
  (not `OptMember`), if `schema::is_schema_value(recv) && schema::is_schema_method(name)` route to
  `call_schema`; else fall back to the behavior-identical `read_member → call_value` path. Call-position
  only (bare `s.minLength` still reads the stored field). Method set excludes the source constructors.
- **Ranges + `step`.** `..` exclusive, `..=` inclusive (`Tok::DotDotEq`); both are sequences whose direction
  follows the bounds (`10..1` counts down). A signed `step` (`a..b step k`) is valid in for-range, value
  position, and match patterns; `step` is a CONTEXTUAL keyword (not reserved). Value-position range
  materializes to `array<number>`; for-range stays lazy; match-range with step is strided membership. All
  validation flows through `interp::resolve_step(lo,hi,step,span)` (reused by `stream.range`) so step-0 /
  non-finite / direction-mismatch are byte-identical Tier-2 panics. VM opcodes: `RangeInclusive`,
  `RangeStepValue`, `RangeResolveStep`, `RangeHasNext`, `MatchRange` (operand = flags byte: bit0 inclusive,
  bit1 step-present). Lints: `range-step`, `invalid-propagate`, `unresolved-import` (default Warning).
- **Destructuring / spread / rest.** Object destructuring `let {a, b as local, "k" as v} = obj` binds by key
  from Object/Instance, missing keys → `nil`, rename with soft keyword `as`, trailing `...rest` collector.
  Spread `...` (`Tok::DotDotDot`) is valid in array/object literals and call args via typed-element AST
  (`ArrayElem`/`ObjEntry`/`CallArg`); spreading the wrong container is a Tier-2 panic; object-spread is
  later-value-wins. Rest params collect trailing args (`...name: array<T>`, per-element checked) via a
  `has_rest` fast path in `run_body`; array/object rest patterns collect the tail/leftover keys.
- **Match pattern extensions.** `MatchArm { patterns: Vec<Pattern>, guard: Option<Expr> }`. `Pattern` =
  `Wildcard`, `Ident`, `Value`, `Range{start,end,inclusive}`, `Array(_, rest)`, `Object(_, rest)` where rest
  is `None`/`Some(None)` (`...`)/`Some(Some(name))`. **Option C:** a bare `Ident` already defined in scope is
  compared (`==`); an undefined one binds the subject. Object shorthand `{key}` is always a bind.
- **`std/log`** — leveled (`debug/info/warn/error`) structured logging, `Interp`-stateful, routed via
  `self.call_log`; stderr (Live) or capture buffer (tests). Serializes via `json::to_json_lossy` (never
  panics). Object args merge as fields; a thunk first-arg defers message work past the level filter; default
  level from `ASCRIPT_LOG`.

## Larger subsystems (campaign work, condensed)

- **SP3 — runtime robustness.** (a) **Bytecode-capacity errors** (const pool/proto/import table > `u16::MAX`,
  jump displacement > 32 KB, `.aso` field > `u32::MAX`) are clean `CompileError`/`AsoError`, not panics, via
  a sticky `Chunk.overflow`/`Writer.overflow` (the `add_*`/`emit_*` sites record the first overflow + return
  a placeholder; compiler checks `take_overflow()`). VM-only by design — the tree-walker has no bytecode caps
  (documented asymmetry; `tests/vm_limits.rs` trips if a capacity `.expect`/`panic!` returns). (b)
  **Recursion-depth guard, byte-identical on both engines** via two `Cell<u32>` counters: `call_depth`
  (`MAX_CALL_DEPTH = 3000`, incremented EXACTLY once per call — tree-walker in `run_body`, VM at each
  `CallFrame` push; do NOT also bump it in `eval_expr` or you double-count on the tree-walker) and
  `expr_depth` (`EXPR_NEST_LIMIT`, expression nesting, reset per call body). Over either limit → the same
  `maximum recursion depth exceeded`, non-134 exit. Programs run on a `WORKER_STACK_SIZE = 512 MB` worker
  thread. SP9's `stacker::maybe_grow` (`src/vm/stack.rs`, `grow()`/`grow_future()`) lets deep recursion reach
  the cap cleanly instead of SIGABRTing. Truly unbounded recursion stays an architectural non-goal.
- **SP9 — determinism seams + durable workflows.** `src/det.rs`: a per-`Interp`
  `determinism: RefCell<Option<DeterminismContext>>` (Record/Replay, `VirtualClock`, `SeededRng`), INERT by
  default (the `None` branch is the exact pre-SP9 path → byte-identical). When `Some`, RNG (`math`/`uuid.v4`/
  `crypto.randomBytes`) and clock (`time`/`date`) route through it; never hold the cell across `.await`.
  `src/stdlib/workflow.rs` (`workflow` feature, default-on): event-sourced REPLAY (Temporal-style) with
  `activity`/`run`/`resume`/`ctx` as tagged Objects; `ctx.<method>` routed via the SAME call-site hook
  `std/schema` uses; append-only newline-JSON log; replay-mismatch detection; `workflow-determinism` lint.
- **SP4 — checker & tooling** (feature-independent, static-only). `ascript check --fix`/`--fix-dry-run` apply
  only the `FIXABLE_CODES` allowlist (`src/check/fix.rs`; v1 = `unused-import`); `apply_edits` is
  right-to-left overlap-safe and idempotent. `call-arity` (`src/check/rules/call_arity.rs`) covers fns,
  constructors, certain-receiver methods, and imported `std/*` fns (curated `std_arity.rs`, `max=None` since
  native fns ignore surplus args → only too-few is flagged). Cross-module span provenance via
  `AsError.span_source`. Cross-file LSP (`src/lsp/workspace.rs`, `Send+Sync`, interpreter-free
  `WorkspaceIndex`) powers go-to-def / workspace symbols / find-references / rename.
- **SP10 — advisory static gradual type checker** (`src/check/infer/`, static-only, NEVER runs code). One
  inference pass wired into `analyze_with_config` after `rules::ALL`. Emits three default-Warning codes:
  `type-mismatch` (value provably wrong for an ANNOTATED slot — subsumes `contract-mismatch`+
  `field-default-type`), `type-error` (arithmetic on a provable non-number), `possibly-nil` (provable `T?`
  deref without a guard). Files: `ty.rs` (the `CheckTy` lattice + `Compat3{Yes,No,Unknown}` — **only `No`
  ever emits**, everything uncertain is silent: the gradual escape that keeps the untyped corpus at ZERO
  false positives), `table.rs`, `env.rs`, `pass.rs` (bidirectional `synth`/`check_against`, in-file return
  inference, nil-guard/`match`/`instanceof` narrowing). **Invariants:** (1) `examples/**` emits 0 `type-*`
  diagnostics in BOTH feature configs (a new corpus `type-*` is a bug in `assignable`/`synth` — default to
  `Unknown`, never relax the gate); (2) it runs no code, so `vm_differential` is unchanged. LSP:
  `infer::hover_type_at` powers hover types.
- **SP6 — package manager** (`pkg` feature, default-on). An entirely CLI-side module set
  `src/pkg/{manifest,cache,hash,fetch,lock,resolve,commands}.rs` keeps TOML/IO out of the core. `ascript.toml`
  gains `[package]`+`[dependencies]` (value shape selects source kind: `{git,tag|rev}`/`{url}`/`{path}`/
  bare-version string → reserved-future registry error). Go-style **MVS** resolution over a `DepFetcher`
  trait; fetch staged into a content-addressed `store/<asum1>/` (`asum1` = sha256 over a normalized tree;
  cache root `$ASCRIPT_CACHE`). `ascript.lock` (own `version` counter) written by run/test/install;
  `--locked` is offline + re-hashed (fail-closed). **The one core change (byte-identical):** a
  dependency-free `PackageMap` on `Interp.package_resolver` + a shared `classify_specifier(source) ->
  SpecifierKind{Std|Relative|Package|UnknownPackage}`, wired into BOTH the tree-walker `Stmt::Import` and VM
  `Op::Import` (Std/Relative unchanged, Package → the same file loader at the store target, UnknownPackage →
  `unknown package '<k>' — add it with 'ascript add'`). Clone the resolver borrow out, never hold across the
  loader `.await`. CLI: `add`/`remove`/`install`/`update`/`lock`/`tree`/`verify`. Hermetic tests only
  (`tests/pkg.rs`). SP6 touches neither `.aso` nor `ASO_FORMAT_VERSION`.
- **Workers — shared-nothing parallelism** (`src/worker/`, **CORE / default-on, unconditional — NOT behind
  a Cargo feature**, like the GC; builds under `--no-default-features`). Two specs:
  `specs/2026-06-07-workers-foundation-stateless-design.md` (Spec A) +
  `specs/2026-06-07-workers-stateful-actors-streaming-design.md` (Spec B); plans of the same dates. The
  `worker` keyword fronts three forms over **two lifecycles**: (1) **pooled / stateless** — `worker fn` /
  `static worker fn` (Spec A), each call runs once on a lazy, demand-grown pool bounded to `num_cpus`
  (`$ASCRIPT_WORKERS`), returns `future<T>`; (2) **dedicated isolate** (Spec B) — `worker class` actors and
  `worker fn*` streaming generators, a long-lived isolate per handle. **Parallelism by ISOLATION:** each
  isolate is a full, independent `!Send` `Interp` on its own OS thread sharing no memory. The **serializer
  airlock** (`src/worker/serialize.rs`) is the ONLY thing that crosses — structured-clone of bytes; the
  runtime stays `!Send`, so non-sendable values (closures, native handles, generator/actor handles) raise a
  recoverable field-path panic. **Actor handle** = `Value::Native(NativeKind::WorkerActor)` backed by
  `ResourceState::WorkerActor` in `Interp.resources` (`src/worker/actor.rs`): a FIFO **one-message-at-a-time
  mailbox** over a `Send` channel + **non-reentrancy** guard; methods are async-only (each call → a
  `future<T>` message), NO cross-boundary field access; `spawn()` (not local construction) creates it,
  returns `future<handle>`. **Streaming handle** = `Value::Generator` over `GenImpl::Worker` (`src/coro.rs`),
  driven by **demand-driven pull** with a bounded buffer (backpressure across the boundary) and bidirectional
  `gen.next(v)`. Both actors and streams own in-isolate resources and are **torn down on `close()` /
  last-drop** (no zombie threads). **GC invariant:** the GC must NOT trace into either native handle (the
  native-resource rule — they reclaim via deterministic `Drop`). `task.pipe(gen, bus)` (`src/stdlib/task_mod.rs`)
  bridges a worker generator stream onto a local `std/events` bus (intra- vs inter-isolate layering).
  `.aso` bumped to **`ASO_FORMAT_VERSION = 18`**. All-modes byte-identical (tree-walker == specialized-VM ==
  generic-VM == `.aso`); examples in `examples/workers_*.as` + `examples/advanced/workers_*.as`; docs at
  `docs/content/language/workers.md`. (Carry-forward bug, OUT of workers scope: `recover(fn(){...})` — an
  anonymous-fn-expression call arg — fails with "function declaration has no resolver binding"; use the arrow
  form `recover(() => ...)`.)

## Commands

```bash
cargo build                              # build (default features = full stdlib)
cargo test                               # full suite (all features)
cargo test --no-default-features         # core language only
cargo test <name>                        # run a single test by name substring
cargo test --test cli                    # run one integration test file (tests/*.rs)
cargo clippy --all-targets               # lint — must be clean in BOTH feature configs

cargo run -- run examples/hello.as       # compile to bytecode + run on the VM (default engine)
cargo run -- run file.as --tree-walker   # run on the legacy tree-walker (oracle/debug; flag precedes file)
cargo run -- build file.as               # compile to bytecode → file.aso (-o to choose path)
cargo run -- run file.aso                # run compiled bytecode (no compile step; always VM)
cargo run -- repl                        # interactive REPL (VM; --tree-walker for the legacy engine)
cargo run -- fmt file.as                 # format in place
cargo run -- check file.as               # static check (syntax + lints + advisory types)
cargo run -- test file.as ...            # run a .as file's test(name, fn) registrations
cargo run -- lsp                         # language server over stdio (LSP)
```

Clippy must be clean under both `--all-targets` and `--no-default-features --all-targets`. Run both before
considering work done.

## Architecture

### Pipeline
**Two front-ends, two engines — same observable behavior.** The DEFAULT path is the **bytecode VM**: a
lossless **CST front-end** (`src/syntax/` — trivia-preserving lexer + parser → typed AST) → resolver
(scopes/upvalues/slots, classifies module top-level as user-globals) → bytecode compiler (`src/compile/`) →
a `Chunk` → the async VM (`src/vm/`). `ascript run file.as` compiles and runs on the VM; `ascript build`
serializes the `Chunk` to a versioned, verified `.aso` (`src/vm/aso.rs` + `src/vm/verify.rs`).

The LEGACY path is `lexer` → `parser` (precedence-climbing) → `interp` (async tree-walker), retained as the
differential oracle and `--tree-walker` debug engine (`ASCRIPT_ENGINE=tree-walker`; flag precedes the file,
ignored for `.aso`). The legacy front-end (`lexer`/`token`/`ast`/`parser`/`span`) is also consumed by `fmt`,
`repl`, and the `lsp` (static-analysis only — never instantiates the interpreter). Entry points in
`src/lib.rs`: `run_file`/`run_source`/`run_tests` (route to the VM by default); `vm_run_source` /
`vm_run_source_generic` are the VM test entry points. Every token/AST node carries a `Span` so `diagnostics`
(ariadne) points at exact source.

**REPL** buffers lines while `is_incomplete` (positive delimiter-TOKEN depth, or unterminated string/template)
on a `..` prompt, then execs the buffer against the persistent session `Vm`/`Interp` (state persists across
lines). Token-depth counting keeps `${…}` braces from skewing depth.

### The interpreter (`src/interp.rs`)
- `eval_expr`/`exec` are `async` (`#[async_recursion]`) and take **`&self`** — `Interp` state lives behind
  interior-mutability cells so multiple eval futures can be live at once (M17). The runtime is `Rc`/`RefCell`
  and therefore **`!Send`**: `#[tokio::main(flavor = "current_thread")]` + a `LocalSet`. Do not introduce
  `Send` bounds or a multi-thread runtime. **This single-threaded, `!Send` model is PER ISOLATE**, not a
  ceiling on parallelism: workers (see "Workers" below) run COMPLETE, independent `Interp` runtimes on
  separate OS threads that share NO memory — parallelism is achieved by ISOLATION, not shared memory, so
  there are still no data races. Only deep-copied bytes cross between isolates (the serializer airlock), and
  the "never hold a `RefCell` borrow across `.await`" invariant applies within each isolate.
- **M17 async model (spec §7):** calling a script `async fn` returns a `Value::Future` and **eagerly
  schedules** the body via `spawn_local` (`self.rc()` → owned `Rc<Interp>` via a self-`Weak`); `await` drives
  it (`await` on a non-future is identity). **Invariant: never hold a `RefCell` borrow across `.await`**
  (enforced by clippy `await_holding_refcell_ref = "deny"`); for native resources use the
  take-out-across-await pattern (`take_resource` → await → `return_resource`).
- **Structured concurrency / cancel-on-drop:** a task's lifetime is bound to its `Value::Future` handle —
  dropping the last handle aborts the task (an un-awaited, un-held async call is cancelled, not orphaned).
  `task.spawn` is the explicit detach; `race` cancels losers; `timeout` cancels timed-out work. `src/task.rs`
  `SharedFuture` splits into a `ResultCell` (held by the task) and a handle (owns the `AbortHandle`, aborts on
  `Drop`) — no cycle, so last-drop genuinely cancels. `std/task` (`src/stdlib/task_mod.rs`):
  `spawn`/`gather`/`race`/`timeout`/`retry` over `future<T>`.
- **Generators & coroutines** (`src/coro.rs`): `fn*`/`async fn*` return a `Value::Generator` that is
  **consumer-driven** — the body is a lazily-polled `Pin<Box<dyn Future>>` (NOT a spawned task), driven one
  step per `resume`/`gen.next(v)`/`for await`. `yield` parks via `poll_fn`; `gen.close()` drops the body.
- **Control flow uses two enums:** `Flow { Normal, Return, Break, Continue }` for statement-level control, and
  `Control { Panic(AsError), Propagate(Value) }` (`Clone`, rides cross-task futures) for errors — `Panic` is
  an unrecoverable Tier-2 bug (aborts unless caught by `recover`), `Propagate` is the `?` early return
  carrying a `[nil, err]` pair. `AsError` converts into `Control::Panic`.
- `global_env()` installs builtins; programs run in a `.child()` so they can shadow builtins (`let len = 5`).
- **Native resource handles:** OS resources (TCP, child processes, HTTP bodies/servers, SSE, WebSocket,
  terminals) are NOT embedded in `Value`. They live in `Interp.resources`
  (`RefCell<HashMap<u64, ResourceState>>`), referenced from script by a `Value::Native` id — keeps `Value`
  cheap and lets the runtime reclaim fds deterministically. Adding a stateful native API = a `ResourceState`
  variant + accessors; never hold a `resources` borrow across `.await`.

### Values (`src/value.rs`)
`Value` is the runtime tagged union — roughly 16 user-facing kinds: `Nil`, `Bool`, `Int(i64)`,
`Float(f64)`, `Decimal`, `Str(Rc<str>)`, `Builtin`/`Function`, `Array`, `Object` (insertion-ordered
`IndexMap`), `Map`, `Set`, `Bytes`, `Regex`, `Native`, `Enum`, `Class`/`Instance`, plus M17's `Future`
(identity-equal, backed by `SharedFuture`) and `Generator` (identity-equal, consumer-driven). A separate
hashable `MapKey` canonicalizes numbers (−0.0→+0.0, NaN unified; an integral in-range `Float` folds to the
equal `Int` key) for `Map` keys.

**Numeric model (NUM, `superpowers/specs/2026-06-08-numeric-model-design.md`).** Two numeric subtypes —
`Int(i64)` (default for integer literals; `0x`/`0b`/`0o`/underscores) and `Float(f64)` — plus exact
`Decimal`. Division is type-directed (`int/int` truncates); `+ - * **`/unary-`-` trap on i64 overflow
(explicit wrapping `+% -% *%`); bitwise/shift (`& | ^ << >> ~`) are int-only (Go precedence); code points
are `int`s (`string.codepoints`/`from_codepoints`/`code_at`). `number` is the annotation supertype
`int | float`. `x instanceof int|float|number|string|bool` is a runtime type guard (tree-walker intercepts
the reserved-name RHS before eval; the VM uses a dedicated `Op::InstanceOfType` with a type-name const —
byte-identical). Checker `CheckTy::Int`/`Float` (a `number`-typed value into an `int` slot stays gradual →
silent; only provably-concrete-distinct subtypes diagnose). **Float printing always shows a decimal**
(`print(5.0)` → `5.0`, `print(5)` → `5`) so the subtypes are visually distinct; `int`-valued `std/math`
results (e.g. `sqrt`/`sum`/`min`/`gcd`) still carry the `float` subtype and print `3.0` — only `abs`, the
rounding family (`floor`/`ceil`/`round`/`trunc`), the int-div helpers (`floordiv`/`ceildiv`/`divmod`), and
the bit helpers return `int`. **Truthiness is the NUM falsy set** (not just `nil`/`false`): `0`/`0.0`/`-0.0`/
`NaN`/`0m`/`""` are falsy, but **collections/objects/instances stay truthy even when empty** (query empties
with `len(x)`).

- **Cycle-collecting GC (`src/gc.rs`).** Adopts `gcmodule` (refcounting `Cc<T>` + Bacon–Rajan cycle
  collector) — an unconditional, default-on, CORE dependency (must build under `--no-default-features`).
  **Invariant:** native-resource handles and acyclic/immutable handles STAY on `Rc` with no-op `Trace` — the
  GC must never trace into a native resource (they rely on deterministic `Drop` to reclaim fds). When adding
  a cycle-capable `Value` container, mirror it in `Value::trace`.
- **Object/instance SHAPES (hidden classes).** `Value::Object` is `Rc<ObjectCell { map, shape: Cell<u32> }>`
  carrying a shape id beside the entry map; `Instance` has `shape_id`. A shape identifies an ordered
  key-layout; the per-`Vm` `ShapeRegistry` (`src/vm/shape.rs`) assigns ids via a memoized transition tree.
  The VM assigns shapes (literals, instances by class, `resync_object_shape` on a new key); the tree-walker
  never touches the registry (its objects stay shape 0). Additive/behavior-preserving — the inline caches
  rely on it.
- **VM module-scope user-globals.** A direct-child top-level `let`/`const`/`fn`/`class`/`enum`/`import` is a
  module-scope user-global (NOT a frame slot-local), mirroring the tree-walker's shared late-bound module
  `Environment`. Storage is on `Vm`: `user_globals: RefCell<IndexMap<Rc<str>, GlobalSlot{value, mutable}>>`
  (the `Vm` is the GC root, so plain owned `Value`s stay live — no `Cc` cell). `GET_GLOBAL` consults
  `user_globals` first, then builtins. This closes the forward-reference divergence (a fn/field-default
  referencing a later top-level binding late-binds, matching the tree-walker) and is the REPL's cross-line
  persistence. A read/write warms to a `GlobalCache::IndexBound` (stable IndexMap index, guarded by a
  `struct_gen` that bumps only on `DefineGlobal`), gated on `self.specialize`.
- **Redeclaration + const immutability (both RUNTIME-timed,** matching the tree-walker — so a redeclaration /
  const-reassignment in dead/uncalled code never errors and an RHS side-effect runs first). Redeclaration
  (`let x; let x`, `fn f; fn f`) → `Op::DefineGlobal` errors `'<name>' is already defined in this scope`.
  Const immutability at every scope → for globals the compiler always emits `SET_GLOBAL` (the single runtime
  source of truth: immutable → `cannot assign to immutable binding '<name>'`); `Op::ImmutableError` is kept
  for immutable LOCALS/upvalues. Each `Binding` carries `mutable` (`let`/`param` mutable; `const`/`fn`/
  `class`/`enum`/`import`/loop-var immutable).
- **Capture-by-value upvalues.** The resolver splits captures into `captured && mutated` (shared cell,
  by-reference) and `captured && !mutated` (by-value); `UpvalueDescriptor::ParentLocal{slot, by_value}`. The
  `by_value` decision depends on the binding's FINAL `mutated` flag, so it's resolved in a post-resolution
  pass (`finalize_capture_by_value`), NOT at capture time. A by-value slot emits plain `GET_LOCAL`/`SET_LOCAL`
  and `Op::Closure` copies it into a fresh private cell (per-iteration loop freshness is automatic).
  Byte-identical.
- **`--no-specialize` kill switch + three-way differential.** `Vm.specialize: bool` (default true;
  `Vm::new_generic`/`with_specialize(false)` → generic). When false, EVERY fast path is skipped (field/method
  inline caches, adaptive arithmetic, the global cache) and falls through to the generic lookup. The two modes
  MUST be byte-identical (only speed differs). The **three-way differential** asserts
  `tree-walker == specialized-VM == generic-VM` over the corpus + goldens + an IC/arithmetic-heavy set in both
  feature configs. If generic and specialized ever diverge, a specialization GUARD is wrong — fix the guard.

### Standard library (`src/stdlib/`)
Each `std/*` module is native Rust over `Value`. Two routing entry points in `src/stdlib/mod.rs`:
`std_module_exports("std/math")` → the `(name, Value)` bindings an `import` brings in; `call(module, func,
args, span)` → routes qualified builtin calls (`"math.abs"`). To add a module: create `src/stdlib/foo.rs`
exposing `exports()` and `call(...)`, register it in both match arms of `mod.rs`, declare the `pub mod`
(gated by the right `#[cfg(feature)]`), and add the example/test + the `docs/content/stdlib/*.md` page.
Native fns are ordinary `function` values; argument-type misuse is a Tier-2 panic.

### Feature flags (`Cargo.toml`)
The stdlib is split into Cargo features. The `default` set is `data` (json/regex/encoding/csv/toml/yaml/
uuid/url), `binary` (msgpack/cbor; depends on `data`), `datetime`, `intl`, `sys` (fs/process/env), `sysinfo`
(os metrics), `crypto`, `compress`, `sql` (sqlite), `postgres`, `redis`, `net` (tcp/udp/http/ws/server),
`log`, `workflow`, `tui`, `pkg` (package manager), `lsp`, `telemetry`, `ai`. **The only opt-in feature is
`http3`** (reqwest's HTTP/3 is still unstable — it also needs `RUSTFLAGS="--cfg reqwest_unstable"` and would
break a plain `cargo build`). Every module is `#[cfg]`-gated so `--no-default-features` builds the bare
language.

### Tree-sitter grammar
`build.rs` compiles the vendored parser from `tree-sitter-ascript/src/parser.c` via the `cc` crate. The
grammar lives at the repo-root `tree-sitter-ascript/` (conventional layout — splittable to a standalone repo;
its own empty `[workspace]` so `cargo build` doesn't absorb it). See "Touching syntax" above for the regen +
publish steps.

## Tests
- Unit tests inline (`#[test]` / `#[tokio::test]`) in `src/*.rs`.
- Integration tests in `tests/`: `cli.rs`, `modules.rs`, `lsp.rs`, `treesitter_conformance.rs`,
  `frontend_conformance.rs`, `vm_differential.rs`, `vm_limits.rs`, `check.rs`, `pkg.rs`. These spawn the built
  binary (`env!("CARGO_BIN_EXE_ascript")`).
- `examples/*.as` double as living documentation and are exercised by the conformance tests — keep them
  runnable.

## Conventions
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- Workflow per milestone (see roadmap): writing-plans → subagent-driven-development (a fresh implementer plus
  an *independent* reviewer that runs commands and probes edges) → holistic review → merge `--no-ff`. Plans
  live in `superpowers/plans/`.
- Any spec deferral must be a documented, owner-noted Cargo feature or Tier-1 error — never a silent drop.
  Current deferrals: `http3` (feature), HTTP trailers (best-effort), `icu`/crossterm subsets. M17 has three
  **architectural** non-goals (impossible under the approach-A async engine — documented in spec §7 and
  `superpowers/specs/adr/2026-05-30-async-generators.md`, not code TODOs): durable/serializable continuations,
  robust unbounded deep recursion, and deterministic/replayable task scheduling.
- **Accepted SP1 trade-offs** (recorded so they aren't mistaken for bugs): (1) a 1-column caret-span offset
  between the CST and legacy front-ends in diagnostics (message always correct, only the caret column can be
  off by one); (2) a perf trade (~2.9× → ~2.5× geomean) from routing top-level vars through `GET_GLOBAL` for
  tree-walker-parity late-binding (still ≥2×, meets the gate).
