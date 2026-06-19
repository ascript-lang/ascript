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
- The stdlib reference pages are **domain-grouped** (22 pages covering 57 modules — e.g.
  `collections.md` owns `std/string`, `std/array`, `std/object`, `std/map`, `std/set`, `std/math`,
  `std/convert`, and `std/bytes`). The authoritative module→page mapping is `MODULE_PAGES` in
  `tests/docs_drift.rs` (tripwire-validated both directions, now enforced by `tests/docs_drift.rs`).
  If you change a `std/*` API, update the module's **owning page** per that mapping. A **NEW std
  module** needs a reference section on its owning page (or a new page) PLUS a `MODULE_PAGES` entry
  — CI fails if either is missing. **Adding a NEW page** means adding its slug to the `NAV` array in
  `docs/assets/app.js` (now enforced by `tests/docs_drift.rs`) — the sidebar AND the cmd-K search
  both derive from `NAV`, so a page with no entry is unreachable (no link, no search hit).
  In-content links are resolved relative to the current page's directory (`](workflow)`,
  `](../language/syntax)`), not absolute-from-root. The language-guide pages are
  `docs/content/language/{syntax,values-types,classes-enums,type-contracts,errors,modules-async}.md`
  (note: `match`/generators/concurrency live inside those pages, not separate files).
  `tests/docs_drift.rs` is the campaign-gate enforcement (gate 19) for ALL doc-currency invariants:
  CLI-surface⊆cli.md, env-var coverage, module→page mapping, NAV⇄files bijection, in-content links,
  and editor-pin consistency — if any of these six tripwires is RED, CI fails.
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
  that SHA in `editors/zed/extension.toml` (`rev` — Zed's grammar table requires `rev`, not `commit`) and
  `editors/nvim/lua/ascript/treesitter.lua` (`revision`). CI `mirror-grammar.yml` also auto-mirrors, but the editor-pin bump is manual. See
  `CONTRIBUTING.md`. **After a sync, verify BOTH editor pins were bumped to the new mirror SHA** — pin
  currency against the mirror is a manual check (network/another repo; not CI-testable in-repo); pin
  mutual consistency (Zed == Nvim) IS enforced by `tests/docs_drift.rs` (tripwire 6).

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
  is `None`/`Some(None)` (`...`)/`Some(Some(name))`, plus ADT's `Variant{enum_name: Option<Rc<str>>, variant,
  fields: VariantPatFields::{Positional(Vec<Pattern>) | Named(Vec<(Rc<str>, Option<Pattern>)>)}}`. **Option C:**
  a bare `Ident` already defined in scope is compared (`==`); an undefined one binds the subject. Object
  shorthand `{key}` is always a bind.
- **Algebraic enums (ADT).** A variant is unit (`Point` / `Red = 2`), positional-payload (`Pair(int, int)`),
  or named-payload (`Circle(radius: float)`) — uniformly named XOR positional, field type required, never both
  a `= backing` and a `(…)` payload. A payload variant is a first-class **constructor** (`Shape.Circle`,
  `ctor:true`); calling it validates arity + field types via `validate_into` → a constructed `EnumVariant`
  (`payload: Some`, structural `==`). `.value` reflects the payload (Object named / stable Array positional);
  named fields read directly (`c.radius`). **Exhaustiveness is STATIC** (`src/check/infer/pass.rs`,
  `non-exhaustive-match` default **Error**, gradual-silent on an unproven subject) — the runtime `MatchNoArm`
  backstop is unchanged. Bare unit patterns shadow-bind (Option C); the checker emits
  `enum-variant-binding-shadow` (Warning) — write unit variants QUALIFIED (`Shape.Point`) in
  exhaustiveness-relevant matches. Examples: `examples/enums_adt.as`, `examples/advanced/{json_adt,
  state_machine,typed_errors}.as`.
- **Structural interfaces (IFACE — runtime half).** `interface Name [extends A, B] { fn m(params): T }`
  declares a named method SET (signatures, NO bodies; `async`/`fn*`/`static`/`worker` modifiers rejected).
  `interface` is a RESERVED keyword (`Tok::Interface`/`InterfaceKw`); `extends` (interface composition,
  transitive-union method set) and `implements` (on a class) are CONTEXTUAL. An interface name resolves to
  `Value::Interface(Rc<InterfaceDef>)` — an immutable, acyclic, no-op-`Trace` descriptor (the weight class of
  `Value::Class`, never a receiver). `v instanceof I` is a STRUCTURAL conformance check (`Interp::conforms`,
  the single SoT both engines reach via the shared `apply_binop` `InstanceOf` arm — branch on RHS:
  `Value::Class` → unchanged nominal `is_instance_of`, `Value::Interface` → `conforms`, else the Tier-2
  `instanceof requires a class **or interface** on the right-hand side`). v1 conformance = **name + arity**
  (`arity_compatible` over `min_required`/`declared_max`); only `Value::Instance` can conform. `extends` is
  flattened **lazily** (forward-referenceable module-globals) with a runtime cycle guard; verdict memoized
  per `(class, iface)` pointer pair on `Interp`/`Vm` (pure memo, active in `--no-specialize`, holds no `Rc`).
  `implements` is documentation only — stored on `Stmt::Class.implements` for the checker, NOT on the runtime
  `Class` (conformance stays structural). An interface-typed annotation is a runtime CONTRACT via the
  env-aware `check_type_env` (a `Named` resolving to an interface runs `conforms`; class/unresolved
  unchanged). Interfaces ride the worker **code-shipping** closure (`TopDef::Interface`, transitive `extends`
  deps), never the value serializer (an interface VALUE is non-sendable → field-path panic). `.aso` serializes
  the UNFLATTENED descriptor (own methods + `extends` names) + the class `implements` list. **Static interface
  type-checking (`CheckTy::Interface`, `assignable`, `implements-violation`) is TYPE-era — NOT shipped yet.**
  Examples: `examples/interfaces.as`, `examples/advanced/interface_dispatch.as`; docs at
  `docs/content/language/classes-enums.md#interfaces`.
- **`std/log`** — leveled (`debug/info/warn/error`) structured logging, `Interp`-stateful, routed via
  `self.call_log`; stderr (Live) or capture buffer (tests). Serializes via `json::to_json_lossy` (never
  panics). Object args merge as fields; a thunk first-arg defers message work past the level filter; default
  level from `ASCRIPT_LOG`.
- **`defer [await] <call>`** — **RESERVED keyword**. Call-only (enforced at parse time — `defer x` is a parse
  error; a no-effect deferred expression is a silent bug). The callee and args are evaluated **AT the `defer`
  statement** (Go semantics — `defer f(x)` snapshots `x`; `defer (() => f(x))()` does not if `x` is mutated).
  Per-**function** scope (not block — a `defer` inside an `if`/loop runs at function exit). Drains **LIFO**.
  Frame-exit matrix: runs on normal return / `?`-propagation / panic-unwind; does **NOT** run on `exit()`,
  task cancellation (cancel-on-drop is unsound to interrupt), or `gen.close()`/last-drop (`close()` is
  sync). Cancellation non-run is loud + documented: cleanup that must survive cancellation belongs on the
  resource's deterministic Drop. **`defer await f()`** drives the returned future before the next older defer;
  a bare `defer f()` whose call returns a future is a Tier-2 error:
  `deferred call returned a future that would be cancelled on drop — use 'defer await f()' or do async cleanup before exit`.
  §3.6 merge rules (both engines share `merge_defer_panic`): (1) defer panic into a live normal/return →
  defer panic **replaces** the return; (2) defer panic into a live `?`-propagate → defer panic
  **supersedes** the pair; (3) defer panic into an existing panic → **ORIGINAL wins**, new message
  appended as `<orig> (suppressed panic in deferred call: <new>)`; (4) remaining defers still run.
  Method-callee entries store `(recv, name, args)` and re-enter the member-call evaluator (not a
  pre-bound value) to preserve schema/shared/workflow call-site hooks byte-identically with normal calls.
  Two VM opcodes: `Op::DeferPush` (flags byte: bit0 `awaited`, bit1 spread; width 2) and
  `Op::DeferPushMethod` (name const-idx u16 + flags u8 + argc u8; width 4). **`.aso` bumps
  `ASO_FORMAT_VERSION` 27 → 28**. Lints: `defer-in-loop` (Warning — accumulates per iteration),
  `defer-async-call` (Warning — bare defer of a known `async fn`). Both engines byte-identical.

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
- **SP10 / TYPE — static gradual type checker, sound-for-annotated + generics** (`src/check/infer/`,
  static-only, NEVER runs code). One inference pass wired into `analyze_with_config` after `rules::ALL`.
  Emits `type-mismatch` (value provably wrong for an ANNOTATED slot — subsumes `contract-mismatch`+
  `field-default-type`), `type-error` (arithmetic on a provable non-number), `possibly-nil` (provable `T?`
  deref without a guard). **Severity is the soundness model (TYPE):** a `type-mismatch` on a *syntactically
  annotated* slot is a **blocking `Severity::Error`** (the single chokepoint = a `sev` arg on `emit`, threaded
  via a `blocking` flag through `check_against` for `walk_let`/`walk_return` + passed `Error` directly at the
  two INLINE sites `check_call_args`/`check_field_default`); `possibly-nil`, `type-error`, and inferred-context
  mismatches stay **Warning**. `ascript.toml [lint] type-mismatch = "warn"` downgrades the block.
  **Generics (TYPE, runtime-ERASED):** `CheckTy::{Var,FnSig,ClassApp,EnumApp,Interface}` + occurs-checked
  union-find (`unify.rs`) + argument-driven inference (freshen→unify→substitute→check) + a genuinely-INVARIANT
  `ClassApp`/`EnumApp`/parameterized-interface `assignable` arm (rule 8 left covariant) + interface bounds via
  the structural `conforms` predicate. Generics surface in all front-ends but are ERASED: a `T`-slot checks as
  accept-anything at runtime → **no `.aso` bump (`ASO_FORMAT_VERSION` unchanged), `vm_differential` untouched,
  all four modes byte-identical**. Files: `ty.rs` (the `CheckTy` lattice + `Compat3{Yes,No,Unknown}` — **only
  `No` ever emits**, an unsolved/unbounded `Var` → **`Unknown`, NEVER `No`** — the gradual escape that keeps the
  untyped corpus at ZERO false positives), `unify.rs`, `table.rs` (`type_params`/bounds/`InterfaceInfo`/
  `field_order` for positional construction inference), `env.rs`, `pass.rs` (bidirectional `synth`/`check_against`,
  generic call/construction inference + arrow callback-return inference, in-file return inference, narrowing).
  **Invariants:** (1) `examples/**` emits 0 `type-*`/exhaustiveness diagnostics in BOTH feature configs — the
  Gate-5 tripwire (`tests/check.rs` `corpus::`); a new corpus diagnostic is a bug in `assignable`/`synth`/`unify`
  (default to `Unknown`, never relax the gate); (2) it runs no code → `vm_differential` and `.aso` unchanged.
  LSP: `infer::hover_type_at` powers hover + inlay, surfacing INSTANTIATED generics (`Box<int>`, `array<int>`).
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
- **FFI + opt-out capabilities** (`src/stdlib/{ffi,caps}.rs`; spec
  `specs/2026-06-08-ffi-capabilities-design.md`). **`std/caps` is CORE** (no feature; works under
  `--no-default-features`); **`std/ffi` is the default-on `ffi` feature** (`libloading`+`libffi`). NO `.aso`
  bump, NO grammar change — pure stdlib + an `Interp` field + CLI/manifest. (a) **Capabilities — opt-OUT,
  default-all-granted** (so every existing program is byte-identical). Five caps (`fs`/`net`/`process`/`ffi`/
  `env`) on a `CapSet` bitset (`Interp.caps`), subtracted at three scopes (CLI `--deny`/`--sandbox`/
  `--deny-net`/`--deny-fs`, `ascript.toml [capabilities]`, in-code IRREVERSIBLE `caps.drop` — there is NO
  `grant`). The gate is **ONE central chokepoint** in `Interp::call_stdlib` immediately before
  `match module`, keyed by `required_cap(module, func)` — so DNS (`net.lookup`, NOT a connect site), `io`
  stdin, and `os`-topology are gated **by construction**; a per-handle re-check in `call_native_method`
  (`NativeKind::governing_cap`) holds a drop for already-open handles. Gate-12 hot path: a single `Copy`
  bitset `all_granted()` flag short-circuits when nothing is dropped (zero-cost default). The KEYSTONE:
  `run_in_worker(fn, input, {caps:{deny}})` spawns a DEDICATED isolate with a reduced `CapSet` (a real
  memory-isolated sandbox); `caps.drop` is REFUSED in a POOLED `worker fn` (shared-`Interp` reuse leak,
  §4.5a). Audit: `tests/cap_audit.rs` (Gate 10 — every OS path denied). (b) **FFI** — `ffi.open`/`lib.symbol`/
  `sym.call` over libffi; sized C ints (`i8…u64`/`size`) marshal **over `int`** (no new `Value` kind, NUM
  §10); three `NativeKind` Foreign handles (GC-untraced, non-sendable). **libffi return-width rule**
  (load-bearing): a sub-register-width int return MUST be read at register width (`cif.call::<i64>` then
  narrow) — libffi writes a full `ffi_arg`, so `call::<i32>` overflows the 4-byte slot (a stack smash). (c)
  **SP9 FFI seam** (`src/det.rs` `DetEvent::FfiCall`/`FfiRet`): inside Record/Replay, a `sym.call` records the
  marshalled return + post-call `Bytes` out-param contents and replays them WITHOUT re-invoking C; a
  pointer-return / `ForeignPtr` out-param is a LOUD Tier-2 refusal (never a silent wrong replay). INERT by
  default (`determinism_mode()==None` → byte-identical). `ffi-nondeterminism` lint (default Warning, 0 FP on
  `examples/**`) flags `ffi.*` inside a workflow body. Examples: `examples/{ffi_libm,caps_sandbox}.as` +
  `examples/advanced/ffi_struct.as`; docs `docs/content/stdlib/{ffi,caps}.md`.
- **SRV — server tier: shared read-only heap + multi-isolate serve** (spec
  `specs/2026-06-08-server-tier-shared-heap-design.md`). **NO grammar change, NO `.aso` bump** (`freeze` is a
  runtime call; the `TAG_SHARED` serializer tag is worker-wire only — `tests/srv_negative_space.rs` enforces
  both, incl. `ASO_FORMAT_VERSION` unchanged at 25). (a) **`std/shared` — the first `Send` value.**
  `shared.freeze(v)` deep-converts a value into an immutable, `Arc`-backed `Value::Shared(Arc<SharedNode>)` —
  AScript's ONLY `Send`-carrying `Value` variant (the union as a whole STAYS `!Send`; guarded by
  `static_assertions::assert_not_impl_any!(Value: Send)` + a positive `assert_send_sync::<SharedNode>`). The
  variant + `SharedNode` + read-only dispatch are **CORE** (build under `--no-default-features`); only the
  `shared.*` fns are behind the `shared` feature (in `default`). `SharedNode` carries NUM's `Int`/`Float` and
  ADT's payload `EnumVariant`. The freeze walk (`src/stdlib/shared.rs`) uses **two identity tables**, both keyed
  by `gc::cc_addr`/`Rc::as_ptr`: `in_progress: HashSet` (on-stack cycle → REJECT, checked FIRST) and
  `completed: HashMap` (finished-node DIAMOND → reuse the `Arc`) — so a frozen graph is an acyclic `Arc` DAG
  with preserved sharing. **GC:** `Value::trace`'s `Shared` arm is a **no-op** (a different ownership domain,
  acyclic by construction — refcounting reclaims it; never trace into it). **Reads** (`index_get`/`read_member`
  + a `call_shared` call-site hook mirroring the `std/schema` hook) make a frozen value read exactly like its
  underlying kind (scalar materialized, sub-container → a `Shared` view, zero-copy iteration); the VM read fast
  paths **deopt** a `Shared` receiver to the generic reader (specialized == generic). **Mutation** reuses the
  shipped `frozen_kind` `cannot mutate a frozen {kind}` panic (no bespoke string); a frozen-INSTANCE
  user-method call gets a DISTINCT diagnostic (`method '<name>' is not available on a frozen instance …`). (b)
  **Multi-isolate serve.** `server.serve({ workers: N, setup, args })` (`workers` absent/1 = today's
  single-isolate path, unchanged) spreads the accept loop across N shared-nothing isolates that each bind the
  same port via **`SO_REUSEPORT`** (kernel-balanced; `socket2` is now a DIRECT `net`-gated dep). The single
  `&self` loop is refactored into `accept_loop(listener, id, …)` (takes the listener BY VALUE); each isolate
  runs `setup(...args)` at boot to build its OWN handle + open its OWN per-isolate `Native` resources (never
  cross the airlock). Global `maxRequests` is a shared `Arc<AtomicUsize>` budget + a coordinated `Notify` stop
  (only the TOTAL is asserted, never the per-isolate split — OS scheduling). **Windows** has no `SO_REUSEPORT`
  → single-isolate fallback + a one-time `warn`. **Airlock crossing is an `Arc` bump, not a copy:** path-a
  (accept-loop boot) captures the raw `Arc<SharedNode>`s directly in the `Send` `make_loop` closure; path-b
  (pooled per-request) uses a `TAG_SHARED` wire tag + a `Writer.shared`/`WorkerRequest.shared`
  `Vec<Arc<SharedNode>>` side-vector. The shared-heap DATA examples (`examples/shared_config.as`,
  `examples/advanced/shared_routing_table.as`) are four-mode byte-identical; the server example
  (`examples/advanced/server_multicore.as`) binds a port + blocks, so it is EXCLUDED from the run-to-completion
  corpus (`EXAMPLE_SKIPS` `LongRunningServer`) and covered by `tests/server_multicore.rs`. Bench:
  `bench/shared_heap_bench.as` + `run_shared_heap_bench.sh` (the zero-copy-vs-deep-clone headline + Gate-12
  no-tax). Docs: `docs/content/stdlib/shared.md` + the "Multi-core servers" section in
  `docs/content/language/workers.md`.
- **DBG — source debugger (DAP) + CPU sampling profiler** (VM-only; spec/plan
  `specs|plans/2026-06-08-debugger-profiler*`). Everything hangs off ONE **zero-cost-when-off** seam:
  `Vm.instrument: RefCell<Option<Box<Instrumentation{breakpoints,profiler,coverage}>>>` (`src/vm/instrument.rs`).
  The per-instruction dispatch loop is UNTOUCHED — breakpoints are reached ONLY via a runtime-patched
  `Op::Break` byte (never compiler-emitted; the verifier REJECTS a serialized one). **`Chunk.code` is a
  `Code` newtype = `UnsafeCell<Vec<u8>>` (derefs to `Vec<u8>`)** so `patch_byte(&self)` is sound (Miri-clean;
  a `*const→*mut` cast would be UB). **Airlock:** the debugger ships only PLAIN OWNED `String`/`u32` across a
  `Send` mpsc channel (`DebugCommand`/`DebugEvent`/`FrameSnapshot`) — NO `Value`/`Rc`/`Cc` crosses
  (`_assert_send` proves it). The **debuggee runs on its OWN thread** (it parks by BLOCKING on `recv` in
  `debug_stop`); the **DAP server** (`src/dap/`, `dap` feature, hand-rolled serde types, sync stdio loop +
  event-pump thread) is on another thread. `ascript run --inspect <file>` (caps honored) / `ascript dap`
  (program from `launch`). Stop-on-entry; line breakpoints (real verdict via a `breakpoint` event);
  stackTrace/scopes/variables from the cached stop snapshot; `evaluate` reuses the tree-walker
  (`self.interp().eval_expr` over an env built from the paused frame's locals + `user_globals`). v1 stepping =
  resume-to-next-breakpoint (transient line-stepping deferred). **Profiler** (`src/profile/`, `profile`
  feature): publishes the frame-name stack at frame push/pop ONLY (a single None-check when off), a sampler
  aggregates a function-level call tree → speedscope JSON / collapsed folded-stacks; `ascript run --profile
  cpu -o … [--profile-hz N] [--profile-format …]` (observation-only — stdout byte-identical). `.aso` gains an
  OPTIONAL strippable debug section (module source + per-proto line/var tables; `build --strip` omits it);
  **`ASO_FORMAT_VERSION = 26`**. **PRIMARY GATE** (`tests/vm_bench.rs` `dbg_zero_cost_gate`, `#[ignore]`,
  release): instrument==None ≈ armed-idle (geomean 0.998×) AND spec/tw geomean ≥ 2× (2.95× ≥ pre-DBG 2.88×).
  Docs: `docs/content/tooling/debugging-profiling.md`.
- **LANE — two-lane fiber engine** (VM-only; spec `superpowers/specs/2026-06-12-two-lane-engine-design.md`).
  The VM runs two drivers over the SAME `Fiber` state (which externalizes ALL execution — frames/ip/stack
  — so lane-switching is just choosing which driver polls). **`run_loop_sync`** is a plain non-async fn that
  executes the suspension-free opcode subset in a tight loop; the async **`run_loop` is demoted to an
  orchestrator** that bursts into the sync driver and takes over only at ops that can actually suspend —
  non-plain callees (`Op::Call` non-`Closure`, all of `Op::CallMethod`/`CallMethodSpread` in v1),
  `Op::Import`, `Op::Await` on a pending future, `Op::IterNext`, `Op::Break` (DBG). Per-op runtime
  escalation: `NeedsAsync` returned with `ip` still pointing AT the escalating byte (the async driver
  re-decodes it). **`Op::DeferPush`/`Op::DeferPushMethod`** are in-subset (they are pure stack pushes),
  but a frame exit with a non-empty defer list escalates to the async driver (defer drain is async). **`Op::Await`
  on an already-resolved future** is handled inline via `SharedFuture::try_get` — no reactor round-trip, no
  leaving the sync lane. Kill switch: `Vm.sync_lane` (`bool`, default true; env `ASCRIPT_NO_SYNC_LANE=1`),
  mirroring `--no-specialize`; when off, every burst falls through to the async driver. **The orchestrator
  (`run_loop`) is the ONLY caller of `run_loop_sync`** — no other call site. Four-way differential identity:
  tree-walker == specialized-lane-on == specialized-lane-off == generic-lane-on (the generic×lane combination
  is covered by the differential). Fuzz axis and corpus coverage assertion (`lane_corpus_coverage_check`)
  added in the same PR. **No tree-walker change, no `.aso` change, `ASO_FORMAT_VERSION` unchanged.** Headline
  (`bench/LANE_RESULTS.md`): spec/tw geomean 3.59×; A/B geomean 1.045× (dispatch-bound +15–21%); RSS no
  regression; `dbg_zero_cost_gate` 1.006×.
- **CALL — call-path allocation diet + higher-order callback trampoline** (VM-only; spec
  `superpowers/specs/2026-06-12-call-path-diet-design.md`). Three allocation units (A1/A2/A3) plus
  a callback trampoline (Unit B), all VM-only — tree-walker untouched, no `.aso` change
  (`ASO_FORMAT_VERSION` 28 unchanged), no semantics change. **A1:** `alloc_cells` returns
  `Vec::new()` when `cell_slots` is empty (capture-free frames allocate no cells vector — always-on,
  not gated on `call_fast`). **A2:** in-place argument binding over the operand-stack window for the
  qualifying `Op::Call` plain-Closure arm (`call_fast=true`, `!has_rest`): `check_call_args_in_place`
  borrows the existing stack window, eliminating the `vec![Value::Nil; argc]` and `BoundArgs.values`
  allocations — the qualifying call shape reaches **0 allocs/call**. The shared arity +
  contract logic is extracted into `check_call_arity`/`check_param_contract` cores consumed by both
  paths — wording byte-identical by construction. **A3:** fiber pooling at three re-entrant call
  funnels (`call_value` plain-Closure arm, `invoke_compiled_method`, `invoke_compiled_static`):
  `fiber_pool: RefCell<Vec<Fiber>>` capped at `FIBER_POOL_MAX = 8`; `take_pooled_fiber` pops and
  resets (fresh cells per element — capture identity preserved); `return_pooled_fiber` parks back
  only on `RunOutcome::Done`; on `Err` the fiber is dropped, never pooled. Generator fibers, the
  module fiber, and the program root are never pooled. **Unit B (trampoline):** higher-order builtins
  (`array.{map,filter,reduce,sort,find,findIndex,some,every,flatMap,groupBy,partition}`,
  `object.mapValues`, stream pipeline + terminals) detect a `Value::Closure` callee and drive it
  through ONE reused fiber on LANE's sync lane with per-element escalation to the async driver when a
  callback suspends — never re-executing the element. Arming requires a `Value::Closure` (VM-only);
  `Value::Function` (tree-walker) callbacks take the unchanged generic path. **Kill switch:**
  `Vm.call_fast` (`bool`, default true; env `ASCRIPT_NO_CALL_FAST=1`); `Vm::new_generic` disables it
  — the generic path is the complete semantic floor. **Fifth differential mode:**
  `vm_run_source_no_call_fast` joins `vm_differential.rs` (both feature configs); alloc-count slope
  harness in `tests/alloc_count.rs`. **No user-facing docs change** (no API/syntax/opcode surface —
  Gate 13 satisfied by bench + repo docs). Headline (`bench/CALL_RESULTS.md`): A1+A2 → 0
  allocs/qualifying call; A3 → 31→15 allocs/element; spec/tw geomean **4.05×**; A/B geomean
  **1.000×** (func_pipeline +1.1%, call_heavy +1.6%); `dbg_zero_cost_gate` **1.005×**; RSS no
  regression.
- **DECODE — pre-decoded instruction stream + superinstruction fusion + the byte-patch invalidation
  contract** (VM-only; spec `superpowers/specs/2026-06-12-decoded-dispatch-design.md`). A per-`FnProto`,
  **lazily-built decoded side representation** (`DecodedChunk` in `src/vm/decode.rs`: fixed-width op
  records, operands widened, jump targets pre-resolved) that the LANE sync driver executes from instead of
  re-decoding `Chunk.code` bytes. **`Chunk.code` is byte-identical** to before (disassembler / goldens /
  `.aso` / `ASO_FORMAT_VERSION` 28 all unchanged — DECODE never mutates or reformats bytes). **Unit B —
  superinstruction fusion:** a peephole over the decoded records rewrites hot adjacent op runs into ONE
  fused record; the `FUSION_CANDIDATES` set is **data-driven from the committed op-pair census**
  (`bench/DECODE_PAIR_CENSUS.md`) — it changes ONLY with refreshed committed census data, never guessed
  (current forms: `GetLocal;GetProp`, `GetLocal;GetProp;Add`, `GetProp;Add`, `GetLocal;GetLocal`,
  `GetLocal;Const`, `Const;GetLocal`). **§4 invalidation contract (the load-bearing deliverable — the JIT
  prerequisite):** the ONLY thing that mutates bytes is `Op::Break` byte-patching (DBG); it routes through
  the `Chunk.patch_epoch` chokepoint which **drops the decoded cache (never edits decoded records)** so a
  live decoded stream is rebuilt from the patched bytes on the next decode — "drop, never edit." The
  deps-epoch machinery is the same invalidation story a future JIT needs, built and tested here first
  (`tests/vm_decode.rs` battery; the breakpoint-during-hot-loop edge lives there, not in an example).
  **Kill switch:** `Vm.decode` (`bool`, default true; env `ASCRIPT_NO_DECODE=1`) — sits beside
  `--no-specialize` / `ASCRIPT_NO_SYNC_LANE` / `ASCRIPT_NO_CALL_FAST` as the pure-byte-path floor;
  `Vm::new_generic` does NOT disable decode (it's lane-side). **Seven-way differential identity:**
  tree-walker == specialized == generic == lane-off == no-call-fast == decoded-forced (threshold 0) ==
  no-decode, both feature configs (`vm_differential.rs`). **Owner-accepted trade:** Units A+B ship
  **default-on accepting a ~2.3% whole-program regression** (decode-on geomean 0.977× vs decode-off; worst
  `func_pipeline` 0.933×) — the invalidation contract is the value, the kill switch is the escape hatch
  (recorded in `goal-perf.md` + `bench/DECODE_RESULTS.md`). **Units C (speculative global-fn inlining) and
  D (top-of-stack register cache) were EVIDENCE-DROPPED** (reverted, not shipped — inline +0.45% < 2%
  gate; TOS −1.6%, object_churn −3.2%); their `ASCRIPT_NO_DECODE_INLINE`/`ASCRIPT_NO_DECODE_TOS` env reads
  + flags remain as INERT no-ops (documented inert in `docs/content/cli.md`). spec/tw geomean ≥ 2×;
  `dbg_zero_cost_gate` ≈ 1.0×; no `.aso`/grammar/LSP/fmt change.

- **PAR — data-parallel primitives** (stdlib-only; spec `superpowers/specs/2026-06-12-data-parallel-design.md`).
  `task.pmap(data, f, opts?) -> future<array>` + `task.preduce(data, f, init, opts?) -> future<T>`: chunk an
  array across the existing worker pool, run `f` per chunk in isolates, merge results. **No syntax, no opcode,
  no `.aso` change (`ASO_FORMAT_VERSION` stays 29); no new worker-wire tag** — `ChunkJob` rides `WorkerRequest`
  as plain `Send` fields; a native `run_chunk_job` driver runs inside each isolate. **Input:** frozen array →
  `Arc`-bump (TAG_SHARED, O(1)); plain array → per-chunk structured-clone copy. **No auto-freeze** — the
  shipped decision is freeze-or-copy, explicit; non-array → Tier-2 panic. **Callback** = named top-level
  `worker fn` only; a `static worker fn` is rejected at the gate (§2.2 message). Chunk plan CONTRACTUAL:
  `chunk_size = max(minChunk, ceil(len/cap))`; opts `{chunks?, minChunk?}`. `preduce`: each chunk folds seeded
  by its own first element; `init` participates EXACTLY once (final combine). Input-order merge; first-by-input-order
  errors; venue-invariant nesting; cancel-on-drop. **Worker `?`** yields the `[nil,err]` pair (not nil). Plain
  instances cross the airlock field-only (methods not shipped — Spec A limitation). **Headline
  (`bench/DATA_PARALLEL_RESULTS.md`):** scaling **4.28× @ 8 workers**; frozen input 2.01× faster than plain at
  1M; break-even ~1000 LCG-iters/element; Gate-12 spec/tw 3.85×.

- **ELIDE — contract elision via static proof** (both engines; spec
  `superpowers/specs/2026-06-12-contract-elision-design.md`). When the TYPE checker PROVES a call
  site's arguments / an annotated `let` / a fn's returns satisfy their contracts under the strict
  **(E)(Y)(A)** predicate — elide-safe destination type ∧ `assignable==Yes` ∧ argument anchored —
  the runtime check is dropped. **Both engines elide identically:** the VM via `Op::CallElided` +
  skipped `Op::CheckLocal` + `proto.ret=None`; the tree-walker via a per-module AST marking pass
  that sets `Call.elide_args` / `Stmt::Fn.ret=None` before execution. **"Raw `Yes` is not a
  proof"** — anchoring (A) is what makes a `Compat3::Yes` a runtime guarantee; a speculative
  narrowing or loop-carried update without anchoring is never elided, and ELIDE fixed a rule-6
  `Class→Object` checker unsoundness discovered by the adversarial suite. **Default-OFF, opt-in
  via `--elide` / `ASCRIPT_ELIDE=1`** (the collector pass adds ≥1ms / ≥7% cold-start at corpus
  scale, exceeding the §5.1 budget; `ascript build --elide` is the natural surface — the artifact
  carries the win). Kill switch `--no-elide` / `ASCRIPT_NO_ELIDE=1` force-off wins over the
  opt-in. Paranoid mode `ASCRIPT_ELIDE_PARANOID=1` runs elide-OFF and escalates a violated proof
  to `ELIDE proof violated (checker soundness bug):`. **Headline:** typed call-heavy **−6.0%**
  (`--elide` vs `--no-elide`), 66.7% elision rate; default path unchanged (Gate-12/17 spec/tw
  3.92× ≥ 2×). **Cross-axis gate:** elide-on == elide-off over the full corpus + fuzz +
  paranoid-corpus zero-escalations (`vm_differential.rs` elide axis, both feature configs). **ASO
  28→29** (`Op::CallElided`). No grammar/LSP/fmt/tree-walker-behavior change.
- **WARM — warm starts & durable-log throughput** (spec
  `superpowers/specs/2026-06-12-warm-starts-design.md`; three independent units, all
  behaviour-invisible). **Unit A — compile cache** (CLI-side, `src/cache/`): `ascript run <f>.as`
  consults a content-addressed cache under `$ASCRIPT_CACHE/compiled/` BEFORE parsing. The
  `CompileCacheKey` (`ck1-` prefix) hashes source + the **transitive module graph**
  (`collect_module_graph` — a parallel re-derivation of `compile_path_module_set`, kept ≡ by the
  §2.5 walk-drift tripwire) + effective flags + lockfile. **Fail-open + verify-on-hit**: a hit is
  re-verified before use, any mismatch/corruption degrades to a normal compile — a hostile cache
  entry can never produce a wrong run. Applies ONLY to the plain `.as`-on-VM path (`.aso` /
  `--tree-walker` / `--inspect` / `--profile` / `--elide` never cached). `--no-cache` /
  `ASCRIPT_NO_COMPILE_CACHE`; `ascript cache clean|dir`. CLI-only → `vm_differential` untouched.
  Bench N=500 → **8.0× warm** (+60ms cold tax). **Unit B — PGO** (`src/vm/pgo.rs` + `src/vm/run.rs`
  harvest/seed + `src/vm/shape.rs` `keys_of_pgo`): `ascript build --pgo` runs the program as a REAL
  training workload, harvests warmed inline-cache/adaptive-arith state from LIVE `FnProto`s, and
  appends an `ASPGO` section that rides **OUTSIDE** the `ModuleArchive` encode/decode (count-bomb /
  hostile-byte safe). A later run's `seed_chunk` pre-installs the profile **behind every
  specialization guard** (DERIVED field index, digest-checked) — byte-INVISIBLE: a build without
  `--pgo` is byte-identical, a seeded run is byte-identical to unseeded across all engines, and a
  stale/hostile section deopts on first use. `ASCRIPT_NO_PGO` kill switch. Seeded-PGO joins
  `vm_differential` as the **445/0** axis (both configs). Steady-state ≈1.0× (moves *when* caches
  warm, not warm-code speed). **No `.aso` bump** (rides outside the archive; `ASO_FORMAT_VERSION`
  unchanged, `ARCHIVE_VERSION` 1 unchanged). **Unit C — workflow durability** (`workflow`-gated;
  `src/stdlib/workflow.rs` + the CORE `src/det.rs` chokepoint): `Durability::{Fsync (default,
  unchanged), Group{window_ms,max_events}, Buffered}` routed through ONE `record_event` chokepoint;
  a crc-framed group appender with torn-tail **prefix repair** (a partial trailing record is
  truncated, never replayed). At-least-once activity contract. The `det.rs` chokepoint is core and
  compiles under `--no-default-features`; `log_to_events` reconstruction keeps its local vec (NOT
  pumped). Group ≈98.85× faster than fsync on per-commit shapes; `"fsync"` ≈ baseline. Tree-walker
  untouched. Recorded follow-ups (roadmap, none silent): cache auto-GC, PGO profile merging,
  method-IC seeding, group-mode background flusher.
- **RT — runtime-only native stubs** (spec `superpowers/specs/2026-06-12-native-runtime-stubs-design.md`;
  CLI/link-level — **no engine change**, `ASO_FORMAT_VERSION` 29 + `ARCHIVE_VERSION` 1 unchanged). `build
  --native` no longer ships the full ~43 MB toolchain as the runtime. A second bin target **`ascript-rt`**
  (`src/bin/ascript-rt.rs`, no clap) is the runtime-only stub — VM/GC/`Interp`-kernel/stdlib/workers/caps/
  `.aso`+archive loaders/panic-diagnostics/embedded-shim, with the entire front-end (parsers, compiler,
  checker, LSP/DAP/fmt/REPL/pkg, tree-sitter) compiled OUT via a **build-time cfg `ascript_rt`** (NOT a
  Cargo feature — features are additive and `--no-default-features` must keep building the parsers; the cfg
  is emitted by `build.rs` from `ASCRIPT_RT=1`, the `fuzzing`-cfg precedent). Normal builds are byte-identical
  (the cfg is never set under `cargo test`). The §2.3 audit proves EXACTLY two runtime paths can reach the
  compiler — `Vm::compile_module_file`'s `.as`-disk fallback and `src/worker/dispatch.rs`'s `compile_source`
  arms — both cfg-gated to a loud refusal; a stub has **0 `compile_source`/tree-sitter symbols** (`nm`
  tripwire). **4-tier prebuilt matrix** (rt-core ⊂ rt-local ⊂ rt-net ⊂ rt-full; rt-core **5.75 MB = 13% of
  43 MB**) selected by the archive's own import facts through a drift-tested module→feature table
  (`src/rtstub/std_features.rs`). **Distribution is fail-closed:** an ed25519-signed, version-locked release
  manifest (compiled-in pubkey, no insecure env knob), a content-addressed stub cache that **re-hashes on
  load** (never trusts by path), and a 5-rung resolution ladder (`--stub` → cache → fetch → dev-sibling →
  `current_exe`) where **integrity failures ABORT and only availability failures fall through** (a tampered
  stub never "recovers" to a weaker rung). The signing half rides a default-OFF `rt-release` feature — never
  in a stub. **Footer flags** (`reserved`→`flags`, `FLAG_ZSTD`; `BUNDLE_FOOTER_VERSION` 1→2 only for
  compressed; `flags=0` byte-identical to pre-RT) power **`--compress`** (zstd, bounded decompress). Plus
  un-rejected **`--target`** cross builds (platform-independent payload; macOS sign-before-append means
  prebuilt darwin stubs append cleanly with no host signing), **`--exact`** (local-cargo precise-feature
  stub), **`--oci`** (a deterministic, Docker-less OCI image tarball — hand-rolled USTAR + the two-digest
  rule; musl-only/scratch), reproducible outputs, and **`--report-json`** (a schema-locked build report).
  Engines untouched; tests in `tests/{native,rt_stub,rt_supply_chain,rt_oci,rt_select,rt_repro}.rs`; docs in
  `docs/content/language/bundles.md`. **Stubs are version-locked to the toolchain** (a stub verifies payloads
  with the same `ASO_FORMAT_VERSION` the builder emits). Recorded follow-ups: SBOM for `--oci`,
  registry-push (`--push`), the tree-walker-eval carve-out if Phase-0-material, musl-matrix narrowing if a CI
  leg fails.
- **RESIL — `std/resilience` stdlib** (spec `superpowers/specs/2026-06-12-resilience-stdlib-design.md`; `resilience`
  feature, default-on; **no `.aso`/opcode/grammar change**, `ASO_FORMAT_VERSION` 29 unchanged). Six policy kinds
  (`breaker`/`limiter`/`keyedLimiter`/`bulkhead`/`retry`/`memoize`) + module fns (`fallback`/`singleflight`/
  `deadline`/`withTrace`/`metricsHandler`/`health`/`handler`). Policies are **tagged Objects** (`{__resil:"<kind>",
  …, __local: Native(NativeKind::Resilience)}` — NO new `Value` variant); method calls route through a
  **call-position-only hook** mirroring `std/schema` (one `call_resilience_method`, both engines, `OptMember`
  excluded). **Hook ladder order: schema FIRST, then resilience** (disjoint `__kind`/`__resil` tags + disjoint
  method sets; pinned in `tests/resil_negative_space.rs`). Rejections are Tier-1 `[nil, {message, code}]` (§2.4
  codes: `rate-limited`/`bulkhead-full`/`breaker-open`/`deadline-exceeded`/…); misuse is Tier-2. Substrates reused,
  not rebuilt: breaker ring + `sync.semaphore` (bulkhead) + `std/lru` (keyed-limiter buckets, memoize entries) +
  `SharedFuture` (singleflight). Time via the det-routed `clock_monotonic_ms` seam → SP9 Record/Replay verdicts
  replay byte-identically; the enforcement **sleep** is timing-only (the documented exemption). **THE engine seam
  — `TASK_LOCALS`** (`src/interp.rs`, CORE `tokio::task_local!`, NOT feature-gated): `Cell<Option<Rc<TaskLocals
  {deadline_at_ms, trace_id}>>>`, **copy-on-spawn** (capture an `Rc` clone before the spawn, `task_locals_scope`
  the body) at the **five user-code async spawn sites** (tree-walker ×3 + VM ×2), the `ambient_root_scope` root
  (renamed from `telemetry_root_scope`, wraps every entry point INCL. the CLI tree-walker load AND each http-server
  connection task so `deadline`/`withTrace` work there). Zero-cost when unused: every consult is one TLS `try_with`
  → `None` fast (`deadline_remaining_ms`/`task_locals_capture`); the §5.4 deadline-aware I/O sites (http clamp,
  pg/redis budget-wrap, sqlite pre-check) and the limiter/bulkhead parks all take the `None` branch unchanged when
  no deadline is set — so `vm_differential` stays **445/0** (deadline-bearing code is unreachable in the corpus).
  Per-isolate honesty (§7): every policy's state is per-isolate; under `serve({workers:N})` there are N independent
  copies (the per-replica model); the `__local` marker makes shipping a policy to a worker a LOUD field-path panic;
  global state = the `worker class` actor pattern. Minimal always-on per-isolate metrics **registry** (`ResilState`
  on `Interp`, counters+gauges, `ascript_resilience_*`) with a `#[cfg(telemetry)]` mirror (no-op until init) + a
  `#[cfg(log)]` breaker-transition breadcrumb; `metricsHandler()`/`health()`/`handler()` are `NativeKind::Resilience`
  `NativeMethod`s the server mounts directly (Prometheus 0.0.4 text; 429/503/504 status mapping; `required_cap` =
  `None` so they serve under `--sandbox`). **Two Gate-14 carry-forward fixes landed here:** the VM async-spawn sites
  previously LACKED the `telemetry_scope` wrap the tree-walker had (span lineage now matches; regression test in
  `tests/telemetry.rs`), and a stale telemetry doc-comment. `task.retry` gained v2 keys (additive, v1 bit-identical).
  Zero-cost gate (`bench/RESILIENCE_RESULTS.md`): cross-binary async-spawn 1.024× wall (within the 1.05× DBG bound),
  RSS flat; compute floor geomean 5.32× ≥ 2×. Examples: `examples/resilience.as`, `examples/advanced/
  resilient_gateway.as`; docs `docs/content/stdlib/resilience.md`. (Note: the sleep fn is `time.sleep`, NOT a
  non-existent `task.sleep`.)
- **CNTR — container-native runtime + `std/docker`** (spec `superpowers/specs/2026-06-12-containers-docker-design.md`;
  `docker` feature = `["net","data"]`, default-on; **NO `.aso`/opcode/grammar change**, `ASO_FORMAT_VERSION` 29
  unchanged; pure stdlib + cap-gate generalization + a server-drain seam). **The cap chokepoint is now a CONJUNCTION:**
  `required_cap`/`NativeKind::governing_caps` returns a `CapReq` (a `Copy(u8)` bitset newtype in `src/stdlib/caps.rs`:
  `NONE`/`one`/`and`/`is_empty`/`iter`-in-`Cap::ALL`-order) instead of `Option<Cap>`, so `docker` can require **net ∧
  process** (the first-denied-in-`Cap::ALL`-order names the error). The gate at `call_stdlib` + the per-handle re-check
  at `call_native_method` iterate `required_cap(...).iter()` / `governing_caps().iter()` behind the unchanged
  `!all_granted()` short-circuit → the default all-granted path is byte-identical (single-cap = one iteration =
  old behavior; `cap_audit` 100% green = verdict-preservation proof). **`std/net/unix`** (UDS connect/listen handles,
  the structural mirror of `net_tcp.rs`, `NativeKind::{UnixStream,UnixListener}`, Drop-unlinks-the-bound-path) + a
  stage-2 carve-out `Interp::check_unix_path` (`None` net-scope → zero-cost Ok; else allow iff `unix:<canonical>` is
  allow-listed). **HTTP over UDS, NOT reqwest:** `src/stdlib/http1.rs` — a small HARDENED HTTP/1.1 client codec
  (generic over the transport; bounds `MAX_HEADER_BLOCK=64KiB`/`MAX_HEADERS=256`/`MAX_CHUNK_SIZE=16MiB`; hostile
  input → clean Tier-1, never panic/hang/OOM; `read_to_end` never pre-`reserve`s an attacker length; `101`→`Upgraded
  {transport,leftover}`). `std/net/http` routes `{socketPath}` requests through it (`call_http_send_uds`,
  surface-identical to the reqwest TCP path incl. `errorOnStatus`/stream/json). **`std/docker`** (`src/stdlib/
  docker.rs`): `docker.connect` (socket resolution opts→`$DOCKER_HOST`→`/var/run/docker.sock`; `tcp://`→Tier-1) +
  version negotiation (clamp `[1.24,1.43]`, floor-reject) + the unary table + `logs`/`events`/`pull`/exec streams
  (`NativeKind::DockerStream`, `native_stream_method=>Some("next")` makes `for await` work on BOTH engines) over the
  **8-byte multiplexed demux** (stdout/stderr type + big-endian size, Multiplexed/Tty auto-detect on the first 8
  bytes, JsonLines for events/pull; oversize-no-alloc + partial-frame reassembly + truncation→Tier-1). exec via the
  `Upgraded` hijack. Return shapes per spec §4.2 (`ping`→`[true]`, `wait`→`[StatusCode int]`). `DockerClient`/
  `DockerStream` `governing_caps` = net∧process; all four new handles GC-untraced + non-sendable (worker airlock
  field-path panic). Hermetic: a recorded-fixture **mock Engine daemon** (`tests/docker.rs`) makes the whole module
  testable with NO Docker installed; live tests env-gated on `ASCRIPT_DOCKER_LIVE=1`. **Inbound signals + graceful
  drain** (§6–§7): `process.on`/`process.off` (`tokio::signal`, MAIN-ISOLATE only — worker→Tier-2 via `in_isolate()`;
  `off` → emulated default-kill `exit(128+signo)` on next receipt; the signal listeners are daemon tasks ABORTED at
  program end via `Interp::abort_signal_listeners()` before the `local.await` drain — else a registered handler hangs
  the process). `srv.shutdown()` + `serve({onShutdown,drainTimeout})` graceful drain: the accept_loop predicate
  generalized `budget==0` → `budget==0 || shutdown.is_armed()` with the lost-wakeup register→`enable()`→recheck
  sequence **PRESERVED** (the existing server battery byte-identical = the proof); `onShutdown` once
  (`AtomicBool::swap`); drain awaits in-flight raced vs `drainTimeout`, aborts losers. **cgroup-aware sizing** (§8):
  `effective_parallelism()` = `$ASCRIPT_WORKERS || min(num_cpus, cgroup_quota)` (cgroup v2 `cpu.max` / v1
  `cfs_quota`, Linux-only → `None` elsewhere so non-Linux is byte-identical to `main`) swapped into the 4 pool/worker
  sizing sites; `os.inContainer()` (ungated). `ascript init --template server` scaffolds a container-ready server.
  Examples `examples/docker_info.as` + `examples/advanced/docker_supervisor.as` are four-mode-PROVEN against the mock
  (in `EXAMPLE_SKIPS` `DaemonDependent` — they need a daemon, so they're out of the run-to-completion corpus but
  byte-identical across modes via the mock). Docs `docs/content/{deploying,stdlib/docker}.md`. **Recorded
  ENABLED follow-ups** (RT+RESIL both merged): a `scratch`/`--oci` rt-stub image base for the Dockerfiles; the
  template `/proxy` upgrade from `task.retry` to `std/resilience` policies.

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

- **BIN — native single-binary** (`src/bundle.rs` + `build_native`/`run_embedded_aso` in `lib.rs`,
  `try_run_embedded` shim in `main.rs`; `tests/native.rs`). `ascript build --native app.as -o app`
  appends the **verified `.aso`** + a trailing magic-tagged footer (`ASCRIPTB`, bounds-checked) to a copy
  of the running runtime (`current_exe()`); on macOS the clean stub is ad-hoc signed FIRST, then the
  payload is appended AFTER the signature (the loader validates `[0, codeLimit)` and ignores the overlay
  — append-then-sign would relocate it). Startup's `try_run_embedded` runs BEFORE `Cli::parse()`: it reads
  only the 32-byte footer tail of `current_exe()` (~10µs, never the whole image) and, if present, runs the
  payload via the SAME `from_bytes_verified` path as `run file.aso` — **bundling, not AOT** (the embedded
  VM still interprets). Worker-in-bundle is free (`set_worker_aso_bytes`). `--target` is host-only
  (parsed-but-rejected). **NO `.aso` format change, NO `ASO_FORMAT_VERSION` bump** (the embedded payload is
  a byte-identical `build` artifact → four-mode parity stays free).

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
  terminals, **FFI `ForeignLib`/`ForeignSymbol`/`ForeignPtr`**) are NOT embedded in `Value`. They live in
  `Interp.resources` (`RefCell<HashMap<u64, ResourceState>>`), referenced from script by a `Value::Native` id
  — keeps `Value` cheap and lets the runtime reclaim fds deterministically (a `ForeignLib` `dlclose`s on
  drop). Adding a stateful native API = a `ResourceState` variant + accessors; never hold a `resources`
  borrow across `.await`. All three FFI handles stay **GC-untraced** (a raw foreign pointer is opaque memory).

### Values (`src/value.rs`)
`Value` is the runtime tagged union — roughly 16 user-facing kinds: `Nil`, `Bool`, `Int(i64)`,
`Float(f64)`, `Decimal`, `Str(Rc<str>)`, `Builtin`/`Function`, `Array`, `Object` (insertion-ordered
`IndexMap`), `Map`, `Set`, `Bytes`, `Regex`, `Native`, `Enum`, `Class`/`Instance`, IFACE's
`Interface(Rc<InterfaceDef>)` (identity-equal, immutable, no-op `Trace` — a conformance descriptor, never a
receiver), plus M17's `Future` (identity-equal, backed by `SharedFuture`) and `Generator` (identity-equal,
consumer-driven). A separate
hashable `MapKey` canonicalizes numbers (−0.0→+0.0, NaN unified; an integral in-range `Float` folds to the
equal `Int` key) for `Map` keys.

**`Value` is a sealed `pub struct Value(ValueRepr)`** (the `enum ValueRepr` is module-private; NANB Phase 1)
— construct/inspect only through the total constructors + the `ValueKind`/`OwnedKind` borrowing/owning view
accessors, never by matching the enum. The seam is proven zero-cost (the view inlines away). `Value` is
**`size_of` = 24 bytes and FINAL at 24** — the 16-byte two-word repr (`value16`/`ThinStr`) was built, proven
behavior-invisible, and **evidence-REJECTED** against the NANB §8.1 SHIP criteria (no measured time or RSS
win; `bench/NANB_RESULTS.md` "Phase 4", frozen on `feat/value16`). Do not re-litigate the 16-byte (or
8-byte NaN-box) representation without NEW profiling evidence; `Value: !Send + !Sync` stays asserted.

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
  a cycle-capable `Value` container, mirror it in `Value::trace`. **ADT exception:** the
  `Value::EnumVariant(Rc<EnumVariant>)` WRAPPER stays on `Rc` (unit variants are interned, registration-free),
  but a `Some(payload)` IS traced — its `Payload::Positional(Vec<Value>)` / `Payload::Named(Cc<ObjectCell>)`
  can hold cycle-capable containers (e.g. a recursive `Json.Arr(items)`), so `EnumVariant` is in the
  `Value::trace` set and the `gc.rs` doc-comment no longer lists it under "immutable/acyclic … stay on Rc".
- **Object/instance SHAPES (hidden classes) — SHAPE spec, `feat/shape-storage`.** `ObjectCell` and
  `Instance.fields` now hold an `ObjectStorage` enum: **`Slab { keys: Rc<[Rc<str>]>, values: Vec<Value> }`**
  (the common case — key list shared per shape via the registry, values inline, zero per-object key alloc)
  **| `Dict(IndexMap<String, Value>)`** (fallback; always shape 0). All access goes through sealed accessors;
  the legacy `borrow()` shim **panics on Slab** — use accessors only. **The VM builds slabs**; the
  **tree-walker builds Dict (shape 0)** — the oracle is unchanged, as the four-mode differential proves.
  The per-`Vm` `ShapeRegistry` (`src/vm/shape.rs`, `FxHashMap`-backed) interns key-lists → shape ids via a
  memoized transition tree; caps: `SLAB_MAX_KEYS = 64`, `SHAPE_FANOUT_MAX = 128`. A slab that grows past
  either cap **demotes to Dict (one-way, shape 0)**. The per-site **`lit_shapes` cache** (`Chunk.lit_shapes`,
  specialize-gated, runtime-only — NOT serialized into `.aso`) lets a warm `NewObject` site skip the registry
  probe; `--no-specialize` still builds slabs (representation is not toggleable). Construction/mutation go
  through `vm_object_insert`/`vm_instance_insert` (precise per-key registry transitions); the old
  `resync_object_shape`/`resync_instance_shape` full-re-derive functions were **deleted**. Delete-bug lesson
  (Phase 0): `object.delete` demotes slab → Dict and resets shape to 0 before the `shift_remove`, so stale
  inline caches cannot read wrong slot offsets. **Hashing boundary (security):** VM interior tables
  (`class_methods`/`class_static_methods`/`class_defaults`/`user_globals` + shape registry) use **FxHash**
  (bounded, non-adversarial keys); **`Map`/`Set`/dict-mode objects/decode paths keep SipHash** (hash-flooding
  DoS resistance — do not "optimize"). **No `.aso`/opcode change** — SHAPE is purely runtime
  (`ASO_FORMAT_VERSION` unchanged at 28; guarded by `tests/shape_negative_space.rs`).
  Performance: `object_churn` **1.77×** speedup; per-object alloc slope **13 → 2 (6.5× reduction)**;
  `json_roundtrip` flat by design (decode-born objects stay Dict/SipHash per spec §9).
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
- **Fuzzing (FUZZ — CONTINUOUS infra).** Two layers: (1) **in-tree property tests** (`tests/property.rs`,
  `proptest` + the `src/fuzzgen` grammar-aware generator) run in the normal `cargo test` — they guard the
  three-way differential, the `.aso`/worker-clone round-trips, and the GC; (2) **libFuzzer targets** in the
  **isolated `fuzz/` cargo workspace member** (its own `[workspace]` so `libfuzzer-sys`/`cargo-fuzz` NEVER
  enter the root build graph — verify `cargo tree -e normal`): `aso_roundtrip`, `worker_serialize`,
  `differential`, `parser`. Any fuzz-support seam added to production code (e.g. `lib.rs`
  `aso_runnable_accept`) is `#[cfg(any(test, feature = "fuzzgen", fuzzing))]`-gated so it never ships. Only
  curated `ex_*`/`bad_*` seeds under `fuzz/corpus/<target>/` are committed (the grown corpus is gitignored).
  **Continuous obligation:** a syntax/numeric/`.aso`/worker-serialization change must EXTEND the generator +
  corpus + a normal-suite regression guard; a fuzz crash is fixed with a permanent `bad_*` seed + a
  `property.rs` test BEFORE the fix (Gate 0). CI: `ci.yml` `fuzz-smoke` (per-PR corpus replay) +
  `fuzz-nightly.yml` (deep campaign; the `aso_roundtrip` 4 h run is BIN's sustained-clean gate — see
  `CONTRIBUTING.md` "Fuzzing & property tests"). `ASO_FORMAT_VERSION` bump → `./fuzz/regenerate_aso_corpus.sh`.

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
- **Accepted SP1 trade-offs** (recorded so they aren't mistaken for bugs): (1) ~~a 1-column caret-span offset
  between the CST and legacy front-ends~~ — **RESOLVED** (DX diag-polish): the offset was the visible tip of a
  span-UNIT inconsistency — the CST front-end built `Span`s from cstree BYTE `text_range()` offsets while the
  whole `Span`/AsError/DBG/ariadne(char-mode) machinery (and the legacy oracle) assumes CHAR offsets. ASCII hid
  it (byte==char); any multibyte char before a span desynced them (dropped/blanked caret frames on the VM,
  parse errors, check lints, and DBG line/col). Fixed by converting at the CST→`Span` boundary
  (`src/compile/mod.rs`: a thread-local byte→char map installed per `compile_source`, zero-cost for ASCII;
  `collect_parse_errors`/`diagnostics::report`/`check::render` made char-canonical). The field-default reparse
  subsystem + `Param.name_span` stay byte (internal, never rendered → the `*_bytes` helpers). No `.aso` bump
  (offsets are `usize` regardless of unit); four-mode byte-identity unchanged. The LEGACY front-end is
  untouched (it was already CHAR-correct). (2) a perf trade (~2.9× → ~2.5× geomean) from routing top-level vars
  through `GET_GLOBAL` for tree-walker-parity late-binding (still ≥2×, meets the gate).

## Diagnostic message style guide (DX D4-T18)

DX owns this guide; **every construct/spec writes its OWN error strings and MUST follow it.** It is grounded
in the de-facto corpus (`src/interp.rs` ~166 `AsError::at` sites, `src/value.rs`, `src/vm/run.rs`,
`src/stdlib/*.rs`, the checker `src/check/**`). A new message that violates these rules is a review nit, not a
style preference. Checklist:

- **Know the tier, name it correctly.** Three error tiers (`src/interp.rs` `Control`): a **Tier-1 recoverable**
  result is the `[value, err]` pair (the err is a *value*, not a panic — fused decode/IO/parse); **`?`-propagate**
  (`Control::Propagate`) early-returns that pair; a **Tier-2 panic** (`Control::Panic(AsError)`) is an
  unrecoverable bug (bad type, arity, undefined name) caught only by `recover`. Most messages in this guide are
  Tier-2 panic strings. Don't phrase a Tier-2 bug as if it were a recoverable result, and vice-versa.
- **Lowercase, no trailing period.** The entire corpus is lowercase-leading and period-less
  (`undefined variable '{}'`, `value is not callable`, `operator requires two numbers …`). A proper noun /
  quoted identifier may be mixed-case, but the sentence starts lowercase and never ends in `.`.
- **Single-quote identifiers, keywords, and operators.** `'{}' is not a function`, `undefined variable '{}'`,
  `cannot assign to immutable binding '{}'`, `unexpected key '{}' for {} (strict)`. Type *names* in a
  `got {type}` tail are NOT quoted (they come from `type_name()`, a closed vocabulary).
- **When the cause is a type mismatch, include the offending `type_name()` with a `, got {type}` tail.** This
  is the single most common shape (`array index must be an int, got float`; `bitwise op requires int operands,
  got float`; `len() expects … , got {}`; `{}: expected {}, got {}`). Use `crate::interp::type_name(&v)` (or
  `value.type_name()`) — never hand-spell the type. *Newly unified (T18): the generic arithmetic/comparison
  fallback (`apply_binop`) and the negate/`~` fallbacks (`apply_unop`) now carry the `, got {type}` tail like
  their siblings.*
- **Preferred shape templates** (pick the one that fits, don't invent a fourth):
  1. `expected <X>, got <Y>` — arity/shape/type contracts (`{}.{} expects {} field{}, got {}`,
     `type contract violated at {}: expected {}, got {}`).
  2. `cannot <verb> a <type>` / `cannot <verb> … , got {type}` — operations on a wrong-kind receiver
     (`cannot destructure a non-array value of type {}`, `cannot negate a non-number, got {}`,
     `cannot mutate a frozen {}`).
  3. `'<name>' is …` / `<thing> requires <X>` — naming/requirement statements (`'{}' is not a function`,
     `instanceof requires a class or interface on the right-hand side`).
- **Attach a `help:` line for an actionable next step, did-you-mean, or the blessed pattern — not for
  restating the error.** Reserve it for: a closest-name suggestion (`src/check/suggest.rs` `closest()` powers
  the T16 did-you-mean), a fix hint (range-step lints, the `recover(() => …)` arrow-form note), or the one
  correct spelling. If there is no concrete next action, omit it.
- **Field-path panics** (`validate_into`, `src/interp.rs`): report the failing path as a dotted/indexed
  selector so the user can locate the field (e.g. `user.roles[2]`); these are recoverable Tier-2.
- **BYTE-IDENTITY RULE (load-bearing).** A Tier-2 panic message is observable output under the four-mode gate
  (tree-walker == specialized-VM == generic-VM == `.aso`). **Raise the message from SHARED code both engines
  reach** (`apply_binop`/`apply_unop`/`value.rs`/`validate_into`) so the string is identical by construction;
  the VM's specialized fast paths must deopt to that shared site, never re-spell the message. If you change a
  message: (1) prove it stays identical via `cargo test --test vm_differential` (377/0, BOTH feature configs),
  and (2) update EVERY asserting test — `grep` the string across `tests/` and `src/` (prefer `.contains(prefix)`
  assertions, which survive an appended `, got {type}` tail). If a change cascades into many goldens for
  marginal benefit, document the convention and leave the message — churn is not an improvement.
