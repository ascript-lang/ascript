# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

AScript is a small, dynamically-typed scripting language (`.as` files) with JavaScript-flavored
syntax, optional runtime-checked type contracts, and a batteries-included standard library. It is
implemented as a single Rust binary `ascript`. The **default and production engine is an async
bytecode VM** (CST front-end → resolver → bytecode compiler → `Chunk` → VM, with inline caches,
adaptive arithmetic, and a cycle-collecting GC). The original async **tree-walking interpreter is
retained** as a differential oracle and a `--tree-walker` debugging engine — kept byte-for-byte
behavior-identical to the VM, not a second dialect.

The design goal is **"Lua-simple language, Go/Deno-class standard library"**: the core stays tiny
(~10 value kinds, gradual contracts, no hidden control flow) while the stdlib is deliberately rich.
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

> Language notes worth knowing when writing `.as` code or docs: under the CLI `run` command, `print`
> **streams live to stdout** (`OutputSink::Live`) and output is retained even if the program later
> panics; `run_source`/REPL/tests **capture** it instead (`OutputSink::Capture`), and async tasks in
> tests buffer via that capture path. `serve({maxRequests:N})` still gives a forever-looping server a
> graceful shutdown but is no longer needed just to *see* its `print` output. Template `${…}`
> interpolation fully supports nested string literals
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
> **`;` separators**: `;` is an optional statement separator (`skip_semicolons`) honored in
> top-level/block statement lists AND class bodies (members are self-delimiting). Enums/match-arms/
> params/literals are comma-delimited and do NOT take `;`. The formatter always canonicalizes to
> newlines.
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
> **`std/schema` fluent method chaining (call-site hook, ADDITIVE).** Refiners + `parse` are callable
> as methods on a schema value (`schema.string().minLength(3).pattern(p).parse(input)`) in addition to
> the free functions. Schemas stay tagged Objects (`{__kind:"string", minLength:3, ...}`) — NO new
> `Value` variant, NO representation change. The hook lives ONLY in the `Call` evaluator
> (`eval_chain`, `ExprKind::Call`): when the callee is a `Member { object, name }` (NOT `OptMember`),
> eval `object` and the args ONCE, and if `schema::is_schema_value(&recv) && schema::is_schema_method(name)`
> route to `self.call_schema(name, [recv, ...args])` (the SAME ops as the free fns); ELSE fall back to
> the **behavior-identical** `read_member(recv, name) → call_value` path (factored shared arg eval into
> `eval_call_args`). It's **call-position only**: bare `s.minLength` (member access) still reads the
> STORED constraint field — this avoids the field/method collision and is the deliberate limitation.
> `is_schema_value` is NARROW (Object whose `__kind` is one of the known schema kinds — never a module
> namespace or arbitrary user object). Method set = `call_schema` ops whose first arg is the receiver
> schema: `minLength/maxLength/pattern/min/max/refine/default/optional/strict/parse` (EXCLUDES the
> source constructors `string/number/bool/nilType/any/literal/object/array/union/oneOf/map/fromClass`).
>
> **Ranges + `step` (ranges-step-analyzer feature).** `..` is exclusive, `..=` inclusive, and both
> are **sequences**: direction is inferred from the bounds (`10..1` counts DOWN). A signed `step`
> (`a..b step k`) is allowed in for-range, value position, AND match patterns; its sign sets the
> direction, and when omitted the direction comes from the bounds. `step` is a **CONTEXTUAL keyword**
> (only special in range position — `let step = 1` still works; it is NOT a reserved word). A range in
> value position **materializes to `array<number>`**; for-range stays lazy. Match-range patterns with a
> step are **strided membership** (anchor = `start`: `x` matches iff in-bounds AND `(x−start)` is a whole
> multiple of `k`). All validation flows through the single shared validator
> **`interp::resolve_step(lo, hi, step, span)`** — reused by the for-range, value-range, and match-range
> paths in `interp.rs` AND by `stream.range` (`src/stdlib/stream.rs`) — so a `step 0`/non-finite step
> (*"step must be a finite, non-zero number"*) and a direction mismatch (`sign(step) != sign(end−start)`
> with `start != end`: *"step <k> moves away from end (<end>); range can never progress"*) are Tier-2
> panics that are **byte-identical across both engines**. VM side: opcodes `Op::RangeInclusive`,
> `RangeStepValue`, `RangeResolveStep`, `RangeHasNext` (`src/vm/opcode.rs`), and `Op::MatchRange` whose
> u8 operand is a **flags byte** (bit0 = inclusive, bit1 = step present) with stack shape
> `subject lo hi step`. These shifts mean **`ASO_FORMAT_VERSION` is now 9** (`src/vm/aso.rs`) — bump it
> on any opcode/serialization change. Three new `ascript check` lints ship alongside: `range-step`,
> `invalid-propagate`, `unresolved-import` (all default Warning, configurable).
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
>
> **`std/log`**: leveled (`debug/info/warn/error`) structured logging; `Interp`-stateful
> (`log_level`/`log_format`), routed via `self.call_log`. Emits to stderr (Live) or a capture buffer
> (tests, `log_output()`). Total serialization via `json::to_json_lossy` (cycles→`"[Circular]"`,
> functions→`"<function>"`, NaN→null — never panics). Non-object args join into `msg`; object args
> merge as fields; reserved `level`/`msg` always win; a thunk first-arg (incl. `async fn`, awaited)
> defers message work past the level filter. Default level from `ASCRIPT_LOG`.
>
> **Phase 8 — match pattern extensions.** `MatchArm` (in `ast.rs`) holds `patterns: Vec<Pattern>` (the
> `|`-alternatives) and `guard: Option<Expr>` (the `if` guard). The `Pattern` enum has these variants:
> `Wildcard` (`_`), `Ident(Rc<str>)`, `Value(Box<Expr>)`, `Range { start, end, inclusive }`,
> `Array(Vec<Pattern>, Option<Option<Rc<str>>>)`, and `Object(Vec<ObjPatEntry>, Option<Option<Rc<str>>>)`.
> The rest field is `None` = no rest, `Some(None)` = `...` (discard), `Some(Some(name))` = `...name`
> (bind). `ObjPatEntry { key, pat: Option<Pattern> }` — `pat: None` is the shorthand `{key}` form.
>
> **Option C runtime resolution (bare identifiers in patterns):** at match time, `Ident(name)` is
> looked up in the current scope: if **defined** → compare subject `== value` (switch-like); if
> **undefined** → bind/capture the subject into `name` for the arm body. This is non-breaking because
> all pre-Phase-8 patterns used value expressions (not bare identifiers). **Object shorthand `{key}`
> is always a bind** (documented exception to Option C — `pat: None` in `ObjPatEntry`); shorthand is
> unambiguously destructuring.
>
> **`..=` token**: `Tok::DotDotEq` — lexed as the inclusive-range operator, used ONLY in match
> `Pattern::Range { inclusive: true }`. It is distinct from `Tok::DotDot` (`..`, exclusive).
>
> **Changes touching match/pattern:** `ast.rs` (`MatchArm`, `Pattern`, `ObjPatEntry`, `Display`
> impl), `parser.rs` (`parse_match_arm`, `parse_pattern`), `interp.rs` (the pattern matcher in
> `match_pattern`), `fmt.rs` (`write_pattern`, `write_match_arm`), `token.rs` (`DotDotEq`),
> `lexer.rs` (lex `..=`), tree-sitter grammar + `parser.c` (regen with
> `tree-sitter generate --abi 14`), LSP (recognizes pattern bindings as definitions).

## Commands

```bash
cargo build                              # build (default features = full stdlib)
cargo test                               # full suite (~540 tests, all features)
cargo test --no-default-features         # core language only (~245 tests)
cargo test <name>                        # run a single test by name substring
cargo test --test cli                    # run one integration test file (tests/*.rs)
cargo clippy --all-targets               # lint — must be clean in BOTH feature configs

cargo run -- run examples/hello.as       # compile to bytecode + run on the VM (default engine)
cargo run -- run file.as --tree-walker   # run on the legacy tree-walker (oracle/debug; flag precedes file)
cargo run -- build file.as               # compile to bytecode → file.aso (-o to choose path)
cargo run -- run file.aso                # run compiled bytecode (no compile step; always VM)
cargo run -- repl                        # interactive REPL (VM; --tree-walker for the legacy engine)
cargo run -- fmt file.as                 # format in place
cargo run -- check file.as               # static check (syntax + lints)
cargo run -- test file.as ...            # run a .as file's test(name, fn) registrations
cargo run -- lsp                         # language server over stdio (LSP)
```

Clippy must be clean under both `--all-targets` and `--no-default-features --all-targets`. Run both
before considering work done.

## Architecture

### Pipeline
**Two front-ends, two engines — same observable behavior.** The DEFAULT production path is the
**bytecode VM**: a lossless **CST front-end** (`src/cst/` — trivia-preserving lexer + parser → typed
AST) → resolver (scopes/upvalues/slots, classifies module top-level as user-globals) → bytecode
compiler (`src/compile/`) → a `Chunk` → the async VM (`src/vm/`). `ascript run file.as` compiles and
runs on the VM; `ascript build file.as` serializes the `Chunk` to a versioned, verified `.aso`
(`src/vm/aso.rs` + `src/vm/verify.rs`) that `ascript run file.aso` loads with no compile step.

The LEGACY path is `lexer` → `parser` (precedence-climbing) → `interp` (async tree-walker). It is
**retained as a differential oracle** (the VM is checked byte-for-byte against it over the whole
corpus + recorded goldens, in both feature configs) and as a `--tree-walker` debug engine
(`ascript run --tree-walker` / `ASCRIPT_ENGINE=tree-walker`; the flag must precede the file and is
ignored for `.aso`). The legacy front-end (`lexer`, `token`, `ast`, `parser`, `span`) is also
consumed by `fmt`, `repl`, and the `lsp` (which is static-analysis only and never instantiates the
interpreter). When changing language *behavior*, both engines must stay byte-identical or the
three-way/whole-corpus differential fails — fix the engine, don't relax the assertion.

Source flows (legacy) as: `lexer::lex(src)` → `parser::parse(&tokens)` → `Interp::exec`/`load_module`.
Every token and AST node carries a `Span` (byte offsets + line/col) so `diagnostics` (ariadne-backed)
can point at exact source. Entry points live in `src/lib.rs`: `run_file`, `run_source`, `run_tests`
(these route to the VM by default; `vm_run_source` / `vm_run_source_generic` are the VM test entry
points).

**REPL multi-line input**: `repl.rs` buffers lines while `is_incomplete` (positive delimiter-TOKEN
depth, or unterminated string/template at EOF) on a `..` prompt, then execs the whole buffer against
the persistent session `Interp`+`Environment` (state already persists across lines). Token-depth
(not raw-brace) counting keeps `${…}` template braces from skewing the depth.

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

**Cycle-collecting GC (`src/gc.rs`, V13).** The spec adopts [`gcmodule`] (a refcounting `Cc<T>` +
Bacon–Rajan trial-deletion cycle collector) to reclaim reference cycles (`a.push(a)`). `gcmodule` is
an **unconditional, default-on, CORE dependency** (NOT a stdlib feature — it must build under
`--no-default-features`). The migration is phased: **V13-T1 (done)** adds the dep + `gcmodule::Trace`
impls for the cycle-capable types (`Value`, `ObjectCell`, `Instance`, `Closure`, `MapKey`, plus the
`indexmap` collections via free `trace_index_map`/`trace_index_set` helpers — `indexmap` is foreign
so no blanket impl is possible) while **keeping everything on `Rc` — NO migration**; the `Trace`
impls compile and are exercised by `gc::tests` but are not yet load-bearing. **V13-T2** is the one-pass
`Rc→Cc` migration of the cycle-capable variants (`Array`/`Object`/`Map`/`Set`/`Instance`/`Closure` +
the closure upvalue cell). **Invariant:** native-resource handles (`Native`/`NativeMethod`) and the
acyclic/immutable handles (`Str`/`Builtin`/`Regex`/`Enum`/`Class`/`Function`'s captured
`Environment`/`Future`/`Generator`) **STAY on `Rc`** and have **no-op `Trace`** — the GC must never
trace into a native resource, because those rely on deterministic `Drop` to reclaim fds. When touching
`Value` variants, mirror any new cycle-capable container in `Value::trace`.

**Object/instance SHAPES (hidden classes, V11-T2).** `Value::Object` is `Rc<ObjectCell>` where
`ObjectCell { map: RefCell<IndexMap<String,Value>>, shape: Cell<u32> }` — the wrapper exists ONLY to
carry a `shape` id beside the entry map. `ObjectCell` has `borrow()`/`borrow_mut()` helpers that
forward to `map`, so the ~150 read/write sites (`o.borrow()`/`o.borrow_mut()`) are unchanged;
construction uses `ObjectCell::new(map)` (shape defaults to `0`). `Instance` gained
`shape_id: Cell<u32>` (also default `0`). A *shape* identifies an object's ordered key-LAYOUT; the
per-VM `ShapeRegistry` (`src/vm/shape.rs`, lives on `Vm` behind a `RefCell`) assigns ids via a
transition tree (`add_key(shape,key)→child`, memoized). Two objects with the same insertion-ordered
keys share a shape; different keys OR order differ; shape `0` is the empty layout. The **VM** assigns
shapes (object literals → `object_shape_for` on the final keys; instances → `class_base_shape` cached
per class by `Rc::as_ptr`; adding a NEW key via `SET_PROP`/`SET_INDEX`/`APPEND_OBJECT`/`SPREAD_OBJECT`
calls `resync_object_shape`, reassigning an existing key keeps the shape — V11-T3 inline caches rely
on this). The **tree-walker never touches the registry** — its objects/instances stay shape `0`. The
change is additive/behavior-preserving (the whole-corpus differential + goldens stay byte-identical).

**VM module-scope user-globals (WS1).** A DIRECT-child top-level `let`/`const`/`fn`/`class`/`enum`/
`import` of the `SourceFile` is a **module-scope user-global**, NOT a file-frame slot-local — mirroring
the tree-walker's single shared late-bound module `Environment`. The resolver (`resolve_file`) collects
those names into `module_globals` and records each as a `Binding { is_global: true, slot: u32::MAX }`
(kept in `result.bindings` for the checker, with use-counts tracked by NAME via `global_uses`); their
references resolve `resolve_local → resolve_upvalue → Resolution::Global(name)` (so an inner `let` still
shadows). The compiler lowers a global define-site to `Op::DefineGlobal <name>` (in SOURCE ORDER — no
eager pre-pass) and a top-level reassignment to `Op::SetGlobal`; reads are the existing `Global`→
`GET_GLOBAL`. Storage is on **`Vm`** (NOT `Interp`): `user_globals: RefCell<IndexMap<Rc<str>,Value>>`
(the `Vm` is the GC root, so plain owned `Value`s stay live — do NOT wrap each in a `Cc` cell) +
`global_version: Cell<u64>`. `GET_GLOBAL` consults `user_globals` FIRST, then `BUILTIN_NAMES` (so a user
name shadows a builtin), else `undefined variable '<n>'`. This closes the forward-reference divergence
(a fn/thunk/field-default referencing a top-level binding declared LATER late-binds at run time, matching
the tree-walker); use-before-init stays a SYMMETRIC error on both engines. The `GET_GLOBAL` inline cache
records ONLY immutable builtins — a user-global read is an uncached IndexMap lookup and `SET_GLOBAL`
updates in place WITHOUT bumping the version (so a hot reassigned-top-level-`let` loop does not thrash the
cache; `DefineGlobal`, which can shadow a builtin, DOES bump). `.from`/typed-parse field defaults resolve
through the lazily-built `class_env` (`def_env`), which is seeded from `user_globals` and kept in sync by
`define_user_global`/`update_user_global`. The checker treats an `is_global` binding name as defined
(`undefined-variable` exempt) and as a file-declared callee (`dropped_local_call`/contract rules).
`ImportDesc` carries `is_global` (named) / `alias`+`is_global` (namespace); a top-level import binds into
`user_globals`. This is also the REPL's cross-line persistence (one `Vm` kept alive). `.aso`
`ASO_FORMAT_VERSION` bumped 3→4 (new opcode + ImportDesc layout). The whole-corpus three-way differential
stays byte-identical.

**Redeclaration + const immutability (WS1 follow-ups, both RUNTIME-timed).** The tree-walker enforces
both via `Environment::define`/`assign` at RUNTIME — so a redeclaration / const-reassignment in
dead/un-entered/uncalled code never errors, and an RHS side-effect runs before the error. The VM matches
exactly. (a) **Redeclaration** (`let x; let x`, `let x; const x`, `fn f; fn f`, `fn f; let f`): the
resolver records EVERY top-level define-site range in `ResolveResult::global_decl_ranges` (deduping only
the checker `bindings` + a `duplicate-binding` resolve diagnostic surfaced by `check/analyze.rs`); the
compiler lowers every such site to `DEFINE_GLOBAL` (keyed on the RANGE, NOT the name — so a same-named
BLOCK/fn-body `let`, which has its own range NOT in the set, stays a slot-local exactly as the resolver
classified it); `Op::DefineGlobal` errors `'<name>' is already defined in this scope` (span `None`, via
`AsError::new` — NOT `panic_at` — to match the tree-walker's span-less error) when the name is already in
`user_globals`. (b) **Const immutability** at EVERY scope: each `Binding` carries `mutable` (a `let`/
`param` is mutable; `const`/`fn`/`class`/`enum`/`import`/loop-var, and a const-DESTRUCTURE pattern bind,
are immutable — pattern-bind mutability is threaded from the enclosing `let`/`const`). The resolver's
`mark_mutated_target` records an assignment whose target resolves to an immutable binding into
`ResolveResult::immutable_assign_targets` (consulting `module_global_mutable`, collected UP FRONT, so a
const reassignment inside a function body that textually PRECEDES the const's declaration is still caught);
the compiler emits `Op::ImmutableError <name>` (a new opcode, `is_unconditional_terminator`) at the STORE
position — i.e. AFTER the RHS is compiled — so it raises `cannot assign to immutable binding '<name>'`
(anchored at the target span) with the tree-walker's exact runtime TIMING (RHS first, dead stores never
fire) without any runtime const-flag tracking. `.aso` `ASO_FORMAT_VERSION` bumped 4→5 (new opcode). Both
fixes keep the whole-corpus three-way differential byte-identical and add no perf-gate regression.

> **Cross-chunk immutable globals (WS2 follow-up).** `Op::ImmutableError` only sees a SAME-chunk
> assignment, so an immutable global (`const`/`fn`/`class`/`enum`/`import`) reassigned from a LATER,
> separately-compiled chunk (REPL line-to-line, or a main module reassigning an import) escaped it.
> Fix: `user_globals` now stores `GlobalSlot { value, mutable }` (still a plain `Value`, NO `Cc` cell —
> the `Vm` is the GC root). `Op::DefineGlobal` carries a `u8` mutability flag (1 = `let`, 0 = immutable;
> the compiler reads it off the resolver `Binding.mutable`) → `DefineGlobal` is now a `u16 name + u8 mut`
> op (3 bytes). For a GLOBAL assignment target the compiler ALWAYS emits `SET_GLOBAL` (never the
> compile-time `ImmutableError`); `Op::SetGlobal` is the SINGLE runtime source of truth: immutable global
> → `cannot assign to immutable binding '<name>'` (target span), absent → `cannot assign to undefined
> variable`, mutable → in-place update. Runtime-timed (dead `if false { k = 2 }` never executes the op);
> imports define immutable globals (`define_user_global(.., false)`). `Op::ImmutableError` is KEPT for
> immutable LOCALS/upvalues (a `const` inside a function — that path is unchanged, byte-identical).
> `.aso` `ASO_FORMAT_VERSION` bumped 5→6 (DefineGlobal operand layout).

**`--no-specialize` KILL SWITCH + three-way differential (V11-T5).** The `Vm` carries a
`specialize: bool` (default `true`; `Vm::new` → specializing, `Vm::new_generic` /
`Vm::with_specialize(interp, false)` → generic). When `false`, EVERY specialization fast path is
skipped: the field/method inline caches (`GET_PROP`/`SET_PROP`/`CALL_METHOD`, via `ic_get_field` /
`ic_resolve_method` / `vm_set_prop`), the PEP-659 adaptive arithmetic (`eval_binop_adaptive`), and
the `GET_GLOBAL` cache all gate their consult AND record behind `if self.specialize`, falling through
to the generic lookup with no warmup/specialize/deopt. The two modes MUST be byte-identical (both
correct) — the only difference is speed. `vm_run_source` (specialize) and `vm_run_source_generic`
(generic) are the test entry points; the eventual CLI `--no-specialize` maps to the generic one. The
**THREE-WAY DIFFERENTIAL** (`tests/vm_differential.rs`, `three_way_*`) asserts
`tree-walker == specialized-VM == generic-VM` byte-for-byte over the whole corpus + recorded goldens +
an IC/adaptive/property/method/arithmetic-heavy program set, in BOTH feature configs. If generic and
specialized EVER diverge, a specialization GUARD is wrong — do NOT relax the assertion, fix the guard.

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
`net` (tcp/http/ws/server), `log` (std/log — default-on, depends on `data` for JSON serialization),
`tui` (crossterm), `lsp` (tower-lsp). Every module is `#[cfg]`-gated so
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
  **Accepted SP1 trade-offs** (post-cutover, recorded so they are not mistaken for bugs): (1) a
  **1-column caret-span offset** between the CST and legacy front-ends in error diagnostics — the
  error *message* is always correct, only the caret column can be off by one (cosmetic, accepted);
  (2) a **perf trade** (~2.9× → ~2.5× geomean) from routing top-level vars through `GET_GLOBAL` for
  tree-walker-parity late-binding — still ≥2× (meets the perf gate); SP8 may recover it; (3)
  **`Op::InstanceOf` is reserved for SP2** (declared at `src/vm/opcode.rs:290`, not yet emitted) —
  do NOT remove it as "dead code"; SP2 reuses it for the `instanceof` operator.
