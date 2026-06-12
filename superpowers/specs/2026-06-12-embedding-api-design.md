# AScript Embedding API — Rust crate + C API — Design (EMBED)

- **Status:** Draft for review
- **Date:** 2026-06-12
- **Code:** EMBED (Deployment & reach track of the PERF campaign — see `goal-perf.md` §"Deployment
  & reach track"; this spec adds **no language surface** and **no engine change**)
- **Depends on:** nothing in-flight. Builds entirely on shipped machinery: the `!Send` per-isolate
  runtime (CLAUDE.md §"The interpreter"), the REPL's persistent-`Vm` session model (`src/repl.rs`),
  the FFI capability model (`src/stdlib/caps.rs` + the `call_stdlib` gate), the SP6
  `classify_specifier` import seam (`src/interp.rs:2882`), and the worker spawn paths
  (`src/worker/isolate.rs`). Independent of every engine spec (LANE/CALL/SHAPE/…).
- **Depended on by:** no spec, hard. WASM v1 integrates directly with `compile_source`/the VM
  (its own decision); the `embed` facade is a possible future consumer there, not a dependent.
  The real consumers are host applications (game scripting, plugin systems, edge hosts —
  Lua's niche).
- **Engines:** the embedded engine is the **VM** (the default/production engine). The tree-walker
  stays the differential oracle; host-module behavior is implemented at the shared
  `Interp`/stdlib layer so both engines stay byte-identical (proven by tests, §11).
- **Breaking:** none. Additive `embed` Cargo feature (default-on) + a new sibling `capi/` crate.
  **No grammar change, no opcode, no `ASO_FORMAT_VERSION` bump** (27 at writing,
  `src/vm/aso.rs:167` — pinned by a negative-space test), **no `Value` variant**, no change to any
  existing entry point's behavior.

---

## 1. Summary & motivation

AScript can be *driven* from Rust today — `tests/` and the CLI do it constantly — but the surface
they use was never designed for hosts. `src/lib.rs` exposes ~30 `pub mod`s of internals plus a set
of ad-hoc entry points (`run_file`, `run_source`, `run_tests`, `vm_run_source`, …) that are
explicitly *not* stable (most are `#[doc(hidden)]` test seams; the rest return CLI-shaped results
like exit codes and captured-stdout strings, not values). There is no way to: hold a long-lived
engine and call script functions repeatedly (a game loop), hand the script host-defined functions
(a plugin API), convert values across the boundary without printing them, or embed from C.

EMBED ships that missing tier as **two stable surfaces**:

1. **`ascript::embed`** — a small, semver-contracted Rust facade: an `Isolate` (builder-constructed,
   **`!Send`, one per host thread**), `eval`/`call`/`load_archive` (blocking and async variants), an
   `AsValue` value bridge, host modules under a collision-proof `host:` namespace, and
   **host-decided capabilities** (default **deny-all** — the loud inversion of the CLI's
   all-granted default, §7).
2. **`ascript-capi`** (`capi/` crate) — a `cdylib`/`staticlib` with a hand-written, checked-in
   `ascript.h`: handle-based, panic-safe (every `extern "C"` fn catches unwind → status code),
   UTF-8 with explicit lengths, manual free fns, thread-affinity *checked* (cheap thread-id
   compare → error code, never UB), versioned with an ABI guard.

**The headline is the model, not the API.** AScript's runtime is `!Send` per isolate — `Rc`/`RefCell`
state on a current-thread reactor, parallelism by *more isolates*, never by sharing (CLAUDE.md;
`src/value.rs:1203` `assert_not_impl_any!(Value: Send, Sync)`). For embedding this is a *strength*,
not a limitation: **one isolate per host thread, zero global VM lock, zero cross-isolate
interference** — the exact property Lua's `lua_State`-per-thread idiom approximates by convention,
AScript enforces by type. The design states this honestly and builds the threading contract around
it (§4) instead of papering over it with channels.

This spec is the **inverse of FFI** (`2026-06-08-ffi-capabilities-design.md`): FFI marshals
AScript values *out* to a C ABI the runtime doesn't control; EMBED marshals host values *in* to a
runtime the host doesn't control. The marshalling vocabulary deliberately mirrors FFI's: scalars
cross **by value**, opaque things cross **by handle**, the error model is the same two-tier split
(Tier-1 `[value, err]` for environment facts, Tier-2 panic for misuse), and "no new `Value` kind"
is a tenet here exactly as it was there.

### Design tenets locked up front

1. **An `Isolate` is `!Send`. Period.** No `Send` wrapper, no mutex'd handle, no cross-thread
   command channel in v1 (rejected, §10). A host that wants N threads creates N isolates.
2. **The embedded engine is the VM**, mirroring the CLI default. The REPL's persistent-`Vm`
   session model (`src/repl.rs:95` — `user_globals` *is* the session scope, fresh `LocalSet` per
   input, structured drain) is reused as the eval substrate, not reinvented.
3. **Containers cross by handle, scalars by value** (§5). An `AsValue` over an `Array`/`Object` is
   an `Rc`/`Cc` clone of the *same* cell the script sees — aliasing and identity preserved (the
   Lua-table model), zero per-crossing deep copies. Deep conversion is an *explicit* JSON/serde
   bridge, never an implicit walk.
4. **Embedded capabilities default to deny-all** (§7). A CLI program is the trusted artifact the
   user chose to run (opt-out caps, FFI §4.2); an embedded script is typically a *plugin* —
   someone else's code inside the host's process. The host grants, explicitly, at build time.
5. **Zero cost when unused.** No hot-path branch is added for hosts that register nothing: the
   host-module dispatch hook lives on the *already-error* fall-through arm of `call_stdlib`'s
   module match, and the `host:` import check is one prefix test at the cold import site (§6.4).
   The CLI binary never constructs the embed types at all.

## 2. Today's de-facto surface (verified — what EMBED replaces and what it reuses)

| Today (`src/lib.rs`) | Takes / returns | Why it can't be the contract |
|---|---|---|
| `run_file(path, args) -> Result<i32, AsError>` (`:119`) | path → process exit code; print streams live | CLI-shaped: no value out, no persistent state, tree-walker engine |
| `run_source(src) -> Result<String, AsError>` (`:632`) | source → captured stdout | output-as-string is a test harness, not a value bridge |
| `vm_run_source` / `vm_run_source_generic` (`:791`, `:804`) | source → `(stdout, Option<exit>)` | `#[doc(hidden)]` differential seams; one-shot (fresh `Vm` per call) |
| `vm_eval_source(src) -> Result<Value, AsError>` (`:739`) | source → raw `Value` | `#[doc(hidden)]`; leaks the unstable `Value` enum; one-shot |
| `run_tests*`, `run_archive`, `run_embedded_aso`, … | CLI plumbing | exit-code shaped |
| ~30 `pub mod`s (`ast`, `vm`, `interp`, `value`, …) | everything | pub-for-bin/tests; zero stability intent |

What EMBED **reuses** (all verified in-tree):

- **Runtime construction:** the CLI runs everything on a 512 MB-stack worker thread hosting a
  `current_thread` runtime (`src/main.rs:475-488`); `run_on_worker_stack` (`src/lib.rs:85`) is the
  in-process equivalent. The worker isolate bootstrap (`src/worker/isolate.rs:287`
  `run_isolate_thread`: runtime + `LocalSet` + fresh `Interp::new()` + `Vm::new`) is the proof the
  model constructs cleanly outside `main`.
- **The session substrate:** `run_repl_vm` (`src/repl.rs:95`) — one persistent `Vm` over a live
  `Interp`; each completed input compiled by `compile_source`, run on a fresh `Fiber` under a fresh
  per-input `LocalSet` (`eval_line_vm`, `:199`), trailing-expression value returned via
  `RunOutcome::Done`, panics reported without killing the session, `user_globals` persisting.
  `Isolate::eval` is this loop, minus the readline.
- **Global read/call:** `Vm::user_global(name)` (`src/vm/run.rs:5154` — doc comment already says
  "the natural read hook for the REPL/embedders") + `Vm::call_value` (`src/vm/run.rs:4462`).
- **Caps:** `Interp::set_caps` (`src/interp.rs:1037`), `CapSet` (`src/stdlib/caps.rs:353`),
  installed before any code runs (the `run_file_with_packages` pattern, `src/lib.rs:139`).
- **Imports:** `classify_specifier` (`src/interp.rs:2882`) — the single seam both engines'
  import paths consult (SP6) — gains one `host:` arm; `load_std_module` (`src/interp.rs:2819`) is
  the template for `load_host_module`.
- **Builtin dispatch:** `Value::Builtin("math.abs")` falls through `call_builtin` to
  `split_once('.')` → `call_stdlib(module, func)` (`src/interp.rs:6419`) — host fns ride the same
  rail with `"host:app.greet"` names (`split_once('.')` splits at the *first* dot → module
  `"host:app"`, func `"greet"`).
- **Archives:** `run_archive` (`src/lib.rs:2416`) — `ModuleArchive` decode → `from_bytes_verified`
  → `set_module_archive` → run entry. `load_archive` is this, on the persistent `Vm`.

## 3. The Rust API (`ascript::embed`, feature `embed`, default-on, additive)

### 3.1 The facade module — and the stability policy it anchors (§9)

All of EMBED's Rust surface lives in **one new module**, `src/embed/` (`mod.rs`, `value.rs`,
`host.rs`, `error.rs`), re-exported as `ascript::embed::*` behind `#[cfg(feature = "embed")]`.
Nothing else in the crate becomes more public; everything else becomes *documentedly* exempt
from semver (§9). The feature is in `default` and is pure library code — `--no-default-features`
builds without it; the CLI binary compiles identically with or without it (no `main.rs` change).

### 3.2 `Isolate` + builder

```rust
use ascript::embed::{Isolate, AsValue, Caps, HostError, OutputMode};

let iso = Isolate::builder()
    .caps(Caps::deny_all())                  // the DEFAULT — shown for emphasis (§7)
    .output(OutputMode::Capture)             // print → buffer (default: Inherit → stdout)
    .host_module("host:app", |m| {
        m.value("version", AsValue::from("1.2.0"));
        m.func("greet", |_ctx, args| {
            let name = args.first().and_then(AsValue::as_str).unwrap_or("world");
            Ok(AsValue::from(format!("hello, {name}")))
        });
    })?
    .build()?;

iso.eval(r#"
    import * as app from "host:app"
    fn on_tick(n) { return app.greet("tick " + n) }
"#)?;
let out = iso.call("on_tick", &[AsValue::from(7)])?;   // → "hello, tick 7"
```

```rust
pub struct Isolate { /* Rc<Vm>, owned runtime, session source, output mode — all private */ }
// Isolate is !Send by construction (holds Rc<Vm>); a unit test pins it:
// static_assertions::assert_not_impl_any!(Isolate: Send, Sync);

impl Isolate {
    pub fn builder() -> IsolateBuilder;

    /// Compile + run `src` on this isolate's persistent Vm, BLOCKING the calling
    /// thread until the program (and everything it spawned) is quiescent.
    /// Returns the trailing-expression value (Nil for a statement-terminated input).
    pub fn eval(&self, src: &str) -> Result<AsValue, EmbedError>;

    /// The async variant: a `!Send` future the HOST drives (§4.2 for exactly
    /// which host configurations can).
    pub async fn eval_async(&self, src: &str) -> Result<AsValue, EmbedError>;

    /// Call a module-scope global by name. If the callee is `async fn`, the
    /// returned Future is driven to completion and its value returned (§3.4).
    pub fn call(&self, name: &str, args: &[AsValue]) -> Result<AsValue, EmbedError>;
    pub async fn call_async(&self, name: &str, args: &[AsValue]) -> Result<AsValue, EmbedError>;

    /// Call a callable AsValue (a function handle previously read out).
    pub fn call_value(&self, callee: &AsValue, args: &[AsValue]) -> Result<AsValue, EmbedError>;
    pub async fn call_value_async(&self, callee: &AsValue, args: &[AsValue])
        -> Result<AsValue, EmbedError>;

    /// Load + run a compiled module archive (`ascript build` output bytes) as the
    /// entry program on this isolate (verified through the same `.aso` trust
    /// boundary the CLI uses: `Chunk::from_bytes_verified`).
    pub fn load_archive(&self, bytes: &[u8]) -> Result<AsValue, EmbedError>;
    pub async fn load_archive_async(&self, bytes: &[u8]) -> Result<AsValue, EmbedError>;

    /// Read a module-scope global (None if undefined). Define/overwrite a global
    /// (defined mutable, like a top-level `let`).
    pub fn global(&self, name: &str) -> Option<AsValue>;
    pub fn set_global(&self, name: &str, value: AsValue) -> Result<(), EmbedError>;

    /// Drain the capture buffer (OutputMode::Capture; empty string under Inherit).
    pub fn take_output(&self) -> String;
}
```

`IsolateBuilder` (all methods additive; `build()` validates and constructs):

```rust
impl IsolateBuilder {
    pub fn caps(self, caps: Caps) -> Self;                 // default: Caps::deny_all() (§7)
    pub fn stdlib(self, filter: StdlibFilter) -> Self;     // default: StdlibFilter::Full (§7.3)
    pub fn output(self, mode: OutputMode) -> Self;         // default: OutputMode::Inherit
    pub fn args(self, args: &[&str]) -> Self;              // script's cli.args (default empty)
    pub fn host_module(self, name: &str, f: impl FnOnce(&mut HostModuleBuilder))
        -> Result<Self, EmbedError>;                       // name validated (§6.2)
    /// Per-isolate factory: ALSO installs this module into every worker isolate
    /// this Isolate spawns (§6.5). The closure is Send+Sync because it runs
    /// inside freshly-spawned isolate threads.
    pub fn host_module_factory(self, name: &str,
        f: std::sync::Arc<dyn Fn(&mut HostModuleBuilder) + Send + Sync>)
        -> Result<Self, EmbedError>;
    pub fn build(self) -> Result<Isolate, EmbedError>;
}
```

`build()` does what `run_isolate_thread` does, on the **calling** thread: construct
`Interp::new()`/`new_live()` per `OutputMode`, `set_caps`, install host modules, `install_self()`,
`Vm::new(interp)`, plus an **owned** `tokio::runtime::Builder::new_current_thread().enable_all()`
runtime (§4.1). No thread is spawned — the isolate *is* the calling thread's.

### 3.3 `eval` semantics (the REPL contract, made precise)

`eval` mirrors `eval_line_vm` (`src/repl.rs:199`) exactly, because that loop already solved every
sub-problem (verified):

1. **Compile** via `compile_source` — a lex/parse/compile error returns
   `EmbedError::Compile { diagnostics }` with NO session mutation (REPL rule).
2. **Accumulate session source** + `set_worker_source` (so a `worker fn` defined in an earlier
   `eval` is sliceable later — the REPL's `session_src` discipline, `repl.rs:224-228`).
3. **Run** the chunk on a fresh `Fiber` against the persistent `Vm`, inside a fresh `LocalSet`
   driven by the owned runtime; after the root future completes, **drain** the `LocalSet`
   (structured join — the `local.await` step every shipped entry point performs).
4. **Map the outcome:** `RunOutcome::Done(v)` → `Ok(AsValue(v))` (the trailing-expression value;
   `Nil` for statement inputs). `Control::Panic(e)` → `EmbedError::Panic` carrying the message,
   span, and an ariadne-rendered report string; **the session survives** (per-eval fiber
   discarded; `user_globals` persist — REPL rule, `repl.rs:265-267`). `Control::Propagate(_)` →
   `Ok(AsValue::nil())` (a top-level `?` ends the program; CLI parity, `lib.rs:665`).
   `Control::Exit(code)` → `EmbedError::Exit(code)`; the isolate stays usable (the *host* decides
   what exit means — documented difference from the CLI, where it ends the process).
5. `gc::collect()` runs on `Isolate::drop` (the end-of-session sweep every entry point performs),
   not per-eval.

`call(name, args)`: `Vm::user_global(name)` → `EmbedError::Undefined` if absent →
`Vm::call_value(v, args, Span::new(0,0))` under the same per-call `LocalSet` + drain →
**auto-await**: if the result is `Value::Future` (an `async fn` callee — eager-scheduled per M17),
it is driven to completion and the resolved value returned. Documented: an embedder who wants the
future itself uses `eval` with an expression that returns it un-awaited (and accepts cancel-on-drop
semantics). `call_value` is the same minus the lookup; non-callables map the engine's own
"value is not callable" Tier-2 panic.

### 3.4 `EmbedError` (the error bridge)

```rust
#[non_exhaustive]
pub enum EmbedError {
    /// Lex/parse/compile diagnostics (message + span + rendered report each).
    Compile(Vec<EmbedDiagnostic>),
    /// A Tier-2 runtime panic: message, optional span, ariadne-rendered report.
    Panic(EmbedPanic),
    /// The script called exit(n).
    Exit(i32),
    /// Blocking eval/call invoked from inside an async runtime context (§4.1).
    NestedRuntime,
    /// `call` target not defined / not callable / wrong-shaped argument.
    Undefined(String),
    /// Builder/registration misuse (bad host-module name, duplicate, …).
    Config(String),
    /// Archive decode/verify failure (the `.aso` trust boundary, verbatim message).
    Archive(String),
}
```

`#[non_exhaustive]` keeps the enum extensible under the semver contract. The *strings* of
diagnostics are NOT contract (wording may improve); the variants and their structured fields are.

## 4. The threading & async contract (the hard part, designed honestly)

### 4.1 An `Isolate` OWNS a current-thread runtime; blocking `eval` drives it

The runtime needs a reactor + `LocalSet` because every engine future is `!Send` and the stdlib's
I/O/timers are tokio-backed. Decision: **(a) the `Isolate` owns its runtime** —
`build()` constructs a private `new_current_thread().enable_all()` runtime; `eval`/`call` run
`LocalSet::block_on(&rt, fut)` (the exact REPL/CLI shape). This is the simplest correct contract
for the majority host (a sync game loop / plugin host with no tokio of its own): **`eval` blocks
the calling thread until the script is quiescent; timers and I/O inside the script work because
`block_on` parks on the owned reactor.**

**Nested-runtime hazard, detected not documented-away:** calling blocking `eval` from inside an
async context panics in tokio ("cannot start a runtime from within a runtime" / blocking on a
worker thread). Every blocking entry first checks `tokio::runtime::Handle::try_current()` — if a
runtime is ambient, return `EmbedError::NestedRuntime` with a message naming `eval_async` as the
fix. Cheap (a TLS read), and it converts a panic into a typed error.

### 4.2 `eval_async` — for hosts that already have tokio (what is actually required)

The `!Send` machinery requires, precisely: the engine future must be **polled on one thread**, with
(i) a tokio reactor reachable for I/O/timers and (ii) a `LocalSet` context for the engine's
`spawn_local` calls (M17 eager async scheduling — `spawn_local` panics outside a `LocalSet`).
`eval_async` returns the engine future (compile + run + drain composed); it never touches the
owned runtime. Therefore, **supported v1**:

- **(b1) Host with a `current_thread` runtime:** await `eval_async` inside
  `LocalSet::run_until` / `LocalSet::block_on` on that runtime. ✔
- **(b2) Host with a multi-thread runtime, driving from a non-worker thread:**
  `local.block_on(&rt, iso.eval_async(..))` — `LocalSet::block_on` accepts either flavor; the
  future runs on the *calling* thread while the multi-thread reactor serves I/O. ✔

**Rejected v1 (typed, documented — not silently broken):**

- **(c) Awaiting from a `tokio::spawn`ed task on a multi-thread runtime.** Impossible by
  construction: `tokio::spawn` requires `Send` futures and `eval_async`'s future is `!Send` — this
  is a **compile error** at the host's call site, which is the correct rejection (the type system
  states the model). The documented escape is the same one the CLI uses: dedicate a thread to the
  isolate (`std::thread::spawn` + blocking `eval`, bridging results over channels). v1 ships that
  pattern as a documented example, NOT as an API — a built-in actor wrapper would re-introduce a
  command-channel API this spec rejects (§10).

`call_async`/`load_archive_async` follow identically. The async variants also work on an isolate
that owns a runtime (the owned reactor simply isn't used for that call — the ambient one is);
there is no mode flag to get wrong.

### 4.3 Deep recursion: the host-stack reality

The CLI's headroom comes from a **512 MB worker-thread stack** (`src/main.rs:478`,
`WORKER_STACK_SIZE`, `src/interp.rs:734`) — that trick belongs to the CLI's `main`, and an
embedded isolate runs on whatever stack the host thread has (8 MB default main thread; commonly
512 KB–2 MB for host-spawned threads). What actually protects an embedded isolate (both shipped,
both engine-level, both verified): (1) the logical `MAX_CALL_DEPTH = 3000` / `EXPR_NEST_LIMIT`
guards convert runaway recursion into the clean catchable `maximum recursion depth exceeded`
panic; (2) SP9's `stacker::maybe_grow` (`src/vm/stack.rs` `grow`/`grow_future`) allocates
heap-backed stack segments at the native re-entry points when the remaining stack falls below the
red zone — so deep-but-legal recursion reaches the logical cap cleanly **even on a small host
stack**. Documented in `docs/content/embedding.md` with the explicit recommendation: a host that
wants CLI-identical headroom runs the isolate on its own `std::thread::Builder::new()
.stack_size(...)` thread (one line; the §4.2(c) pattern). `run_on_worker_stack` stays a
`#[doc(hidden)]` internal — the embed docs show the raw pattern instead of contracting a helper
whose shape (spawn-per-call) is wrong for persistent isolates.

### 4.4 Host functions run synchronously on the isolate thread (the FFI §3.5 analog)

A host fn is invoked synchronously inside the engine's dispatch — **a blocking host fn stalls the
entire isolate** (no preemption; same rule as a blocking C call under FFI §3.5). Cancel/timeout
cannot interrupt it (no `.await` point inside). Documented verbatim alongside FFI's rule. Async
host fns are deferred (§10) — v1 host fns are plain `Fn`, and the deferral is recorded, not silent.

## 5. `AsValue` — the value bridge

### 5.1 Representation: a `!Send` newtype over `Value`; containers ARE handles

```rust
pub struct AsValue(pub(crate) Value);   // field crate-private; !Send by construction
```

Because every AScript container is already `Rc`/`Cc`-backed interior-mutable shared state, **a
clone of the `Value` IS a handle**: an `AsValue` over an `Object` aliases the same `ObjectCell`
the script holds — host writes are visible to the script and vice versa, identity (`==` for
identity-equal kinds) is preserved, and crossing the boundary costs one refcount bump. This is the
recommended container strategy (per the brief), justified against the alternative:

- **Deep conversion per crossing — rejected.** O(n) walks on every boundary touch, loses identity
  (a script mutating an object after the host read it would diverge), loses non-JSON kinds
  (Map keys, Bytes, instances), and double-represents cycles the GC already handles. The hosts
  that *want* a detached deep copy get it explicitly: `to_json`/`from_json` (and the serde bridge)
  — the same "explicit airlock, never implicit" discipline the worker serializer established.
- **Scalars + strings by value** — `nil`/`bool`/`int`/`float` are `Copy`-cheap; strings expose
  `as_str(&self) -> Option<&str>` (borrowing the `Rc<str>`) and `From<String>/From<&str>`
  construction. No handle ceremony for the 90% case.

### 5.2 The kind table (all runtime kinds classified — the Gate-10 "which kinds" answer)

Constructors: `AsValue::nil() / from(bool|i64|f64|&str|String)`, `AsValue::array(Vec<AsValue>)`,
`AsValue::object(Vec<(String, AsValue)>)`, `AsValue::bytes(Vec<u8>)`. Accessors: `kind() ->
AsKind`, `type_name() -> &str` (the engine's `type_name`, stable), `as_int/as_float/as_bool/
as_str/as_bytes`, `len()`, `get(usize)`, `get_key(&str)`, `set(usize, AsValue)`,
`set_key(&str, AsValue)`, `items() -> Vec<AsValue>` (snapshot), `entries()`, `is_callable()`.

| `Value` kind(s) | Crossing class | Host operations |
|---|---|---|
| `Nil`, `Bool`, `Int`, `Float` | **by value** | full construct + read |
| `Str` | **by value** (construct) / borrow (read) | `from`, `as_str` |
| `Decimal` | **by value via string** | `as_str` of display form + `AsValue::decimal(&str)`; lossless |
| `Array`, `Object`, `Map`, `Set` | **live handle** (aliasing clone) | `len/get/get_key/set/set_key/items/entries`; construct (`array`/`object`); `Map`/`Set` read via `entries`/`items`, constructed only script-side (their `MapKey` canonicalization stays engine-owned) |
| `Bytes` | **live handle** | `as_bytes` (copy out), `bytes(vec)` construct, `len` |
| `Function`, `Closure`, `Builtin`, `BoundMethod`, `ClassMethod`, `EnumVariant` (ctor) | **callable handle** | `is_callable`, `Isolate::call_value`; otherwise opaque |
| `Future` | **opaque handle** | `Isolate::call*` auto-awaits (§3.3); a held Future handle keeps the task alive (cancel-on-drop preserved) |
| `Generator`, `GeneratorMethod`, `Native`, `NativeMethod`, `Class`, `Enum`, `Interface`, `Super`, `Regex`, `Shared` | **opaque handle** | `kind`/`type_name`, pass-back-in unchanged; `Shared` additionally reads like its underlying kind (the SRV read dispatch) |

**Nothing errors on crossing** — every kind is at least an opaque, pass-back-able handle (the
inverse of the worker airlock, which must serialize and therefore rejects; the embed boundary is
same-thread, so a handle is always sound). `to_json` errors (Tier-1-shaped
`Result<String, EmbedError>`) on non-serializable kinds, with the field path — reusing
`json::to_json` semantics, not a second serializer.

### 5.3 The explicit deep bridge

`to_json(&self, iso) -> Result<String, EmbedError>` / `Isolate::json_parse(&str) -> Result<AsValue,
EmbedError>` route through `std/json`'s shipped total serializer/parser (feature `data`; under
`--no-default-features` these return a typed `Config` error naming the feature — documented, not
compiled away silently). A `serde` bridge (`AsValue: Serialize` via the JSON model) is included in
the same task; both are *copies*, stated loudly in docs.

## 6. Host modules

### 6.1 Namespace: `host:` — the collision-proof scheme (decision + justification)

Host modules import as `import * as app from "host:app"`. The `host:` URI-style scheme (Deno's
`node:`/`npm:` precedent) is chosen over a path-style `"host/app"` because of how
`classify_specifier` (`src/interp.rs:2882`) already carves the namespace:

- `std/...` is claimed by prefix; **a future std module can never collide** with `host:`.
- Everything else non-relative is a **bare package specifier** — `"host/app"` would parse as
  package key `host` (`split_package_key` splits on `/`), silently shadowable by a real installed
  package named `host` (legal today!). `host:` contains `:`, which no package key can carry
  (path-mapped to the store, and `:` is reserved by this spec — `classify_specifier` checks
  `host:` FIRST, before package classification, making the reservation structural).
- The scheme *reads* as what it is: not loadable from disk, not publishable, host-process-bound.

Module names are validated at registration: `host:` + `[a-z][a-z0-9_]*(/[a-z][a-z0-9_]*)*`, **no
dots** (the builtin dispatch splits the qualified fn name at the first `.` — `src/interp.rs:6419`
— so a dotted module name would mis-split; rejected at `host_module()` with `EmbedError::Config`).

### 6.2 Registration model

```rust
pub struct HostModuleBuilder { /* name, entries */ }
impl HostModuleBuilder {
    /// A constant export (any AsValue).
    pub fn value(&mut self, name: &str, v: AsValue);
    /// A plain host function: Ok(v) returns v; Err(e) follows §6.3 tiering.
    pub fn func(&mut self, name: &str,
        f: impl Fn(&mut HostCtx, &[AsValue]) -> Result<AsValue, HostError> + 'static);
    /// A Tier-1 fallible function: ALWAYS returns the [value, err] pair —
    /// Ok(v) → [v, nil]; Err(HostError::Recoverable(e)) → [nil, {message: e}].
    pub fn fallible_func(&mut self, name: &str,
        f: impl Fn(&mut HostCtx, &[AsValue]) -> Result<AsValue, HostError> + 'static);
}
pub enum HostError {
    /// Recoverable, data-shaped (the FFI "dlopen failed" class). In `fallible_func`
    /// it becomes the Tier-1 err half; in plain `func` it is upgraded to Tier-2
    /// (a plain fn has no err channel — upgrading beats silently swallowing).
    Recoverable(String),
    /// Programmer-misuse / invariant violation (the FFI marshalling-misuse class):
    /// a Tier-2 recoverable panic with the message, catchable by `recover`.
    Panic(String),
}
pub struct HostCtx<'a> { /* &Interp accessor surface: args span, output push */ }
```

The tiering mirrors FFI §3.1's split exactly and **the host chooses per function** (the brief's
requirement): environment facts → `fallible_func` (script handles `[v, err]` with `?`); misuse →
`HostError::Panic` from either form. `HostCtx` v1 exposes `print`-equivalent output and the call
span; it deliberately does NOT expose isolate re-entry (`eval` from inside a host fn) — re-entrant
eval during a borrow-live dispatch is the classic embedding footgun; rejected v1, recorded (§10).

### 6.3 Storage + dispatch (both engines, one chokepoint, zero hot-path cost)

- `Interp` gains `host_modules: RefCell<HashMap<Rc<str>, Rc<HostModuleDef>>>` (`HostModuleDef` =
  ordered exports: values + `Rc<dyn Fn…>` fns). Installed by `build()` before any code runs.
- **Import:** `classify_specifier` gains `SpecifierKind::Host(name)` (checked first, one
  `starts_with("host:")` on the cold import path). Both the tree-walker `Stmt::Import` arm
  (`src/interp.rs:3430`) and the VM `Op::Import` (which routes through the same classify/loader
  seam — the SP6 wiring) get the arm: `load_host_module(source)` builds a `ModuleEntry` exactly
  like `load_std_module` (`:2819`) — env child of `global_env`, each fn export bound as
  `Value::Builtin("host:app.greet")`, memoized in `modules` under `<host>/app`. A miss (module not
  registered — the CLI always misses) is the Tier-2 panic
  `host module 'host:app' is not registered in this isolate` (recoverable, `recover`-able).
- **Call:** `call_stdlib`'s terminal `match module` keeps every existing arm untouched; the
  **fall-through arm** (today's "unknown module" error path) first tests
  `module.starts_with("host:")` → registry lookup → invoke the host fn (clone the `Rc<dyn Fn>`
  out of the borrow before calling — never hold the `RefCell` borrow across the call, the
  standing invariant). **Gate-12 by construction:** the prefix test sits on a path that was
  already an error; no existing program's dispatch touches it. `required_cap(module, func)`
  already returns `None` for unknown module strings (`src/stdlib/mod.rs:325` catch-all) — host
  fns are **un-capability-gated by design**: a host fn is the *host's own trusted code*; if it
  proxies a dangerous effect, gating is the host's job (documented LOUDLY in `embedding.md` and
  on `func`'s rustdoc; the §8 cap-completeness drift test is scoped to `STD_MODULES` and is
  unaffected — asserted by a test).
- Engine parity: because registration, import, and dispatch all live on the shared `Interp`,
  tree-walker == VM byte-identity holds by construction; §11 proves it with a both-engines test.

### 6.4 Workers: main-isolate-only by default; the factory opt-in (the caps precedent)

Host fns are `Rc<dyn Fn>` — `!Send`, structurally unable to cross the airlock. But a
`Value::Builtin("host:app.greet")` is just a *name* (sendable!), and worker isolates re-run
imports from shipped source — so without a rule, a worker would dispatch `host:app` against its
own fresh `Interp::new()` and miss. The rule, v1:

- **Default: host modules are main-isolate-only.** In a worker isolate, `import "host:app"` (and
  a smuggled `Builtin` name) hits the registry-miss panic with the worker-specific message:
  `host module 'host:app' is not available in a worker isolate (register it with
  host_module_factory to install it per-isolate)`. Loud, typed, documented — never a hang or a
  silent nil.
- **Opt-in: `host_module_factory`** (§3.2) — an `Arc<dyn Fn(&mut HostModuleBuilder) + Send +
  Sync>` carried exactly the way the FFI `CapSet` rides the spawn paths (FFI §4.5a, verified
  mechanism): **dedicated** isolates capture the `Arc` list directly in the `Send` `make_loop`
  closure (`spawn_isolate`, `src/worker/isolate.rs:211`); **pooled** requests carry it as a
  side-channel field on the `Send` `WorkerRequest` (like `caps`), installed **fresh at the top of
  each request** (the caps-floor discipline — no cross-request leak between two Isolates sharing
  a thread's pool, which is real: the pool is `thread_local!`, `src/worker/pool.rs:26-28`).
  Factory-built host fns are constructed *inside* the worker thread, so they may close over
  `Send + Sync` host state only — the type signature enforces it.

### 6.5 `StdlibFilter` is availability, caps are security (don't confuse the two)

`StdlibFilter::Full` (default — every compiled-in module) / `Core` (the no-OS subset: everything
`required_cap` maps to `None`, minus `std/ffi`) / `Allow(&["std/math", …])`. Enforced at the
import chokepoints only (`load_std_module` + the checker-visible miss), where a filtered module
reports `module 'std/fs' is not available in this isolate`. Documented loudly: **the filter is an
availability knob, not a security boundary** (an allowlisted module's transitively-reachable
builtins are not re-walked); **capabilities are the security boundary** (the FFI gate at
`call_stdlib` covers every effect path by construction). Cargo features remain the compile-time
availability layer underneath both.

## 7. Capabilities for embedded isolates — DEFAULT DENY-ALL (the loud inversion)

`Isolate::builder()` defaults to **`Caps::deny_all()`** — the embedded inverse of the CLI's
default-all-granted. Rationale, stated as the contract: the CLI's opt-out default exists because a
CLI program is the artifact the *user* chose to run (FFI §4.2 — "batteries included"); an embedded
script is characteristically *someone else's plugin inside the host's process*, and the host — not
the script's author — owns the blast radius. So the host *grants*:

```rust
pub struct Caps(/* CapSet */);
impl Caps {
    pub fn deny_all() -> Self;                  // the default: fs/net/process/ffi/env all denied
    pub fn all_granted() -> Self;               // CLI-equivalent (trusted scripts)
    pub fn granting(caps: &[Cap]) -> Self;      // deny-all + carve-ins, decided AT CONSTRUCTION
}
```

Granting at *construction* does not violate cap monotonicity: the FFI invariant is "no isolate
ever re-gains a capability it **dropped**" (FFI §4.5a) — construction precedes all drops. After
`build()`, the in-script `caps.drop` and the whole FFI §4 machinery work unchanged (the embedded
isolate is a top-level-program isolate in the caps model's terms; drops are irreversible for the
isolate's life). Granular fs/net carve-outs reuse `FsScope`/`NetScope` verbatim. Documentation
carries a boxed warning on the difference from the CLI default, in both `embedding.md` and the
`Caps` rustdoc, plus the rule that **host fns bypass caps** (§6.3 — gate your own proxies).

## 8. The C API (`capi/` crate → `cdylib` + `staticlib` + `include/ascript.h`)

### 8.1 Crate layout (deviation from the brief, justified)

The brief sketches "`capi` feature → cdylib". Cargo cannot feature-gate a crate-type, so an
in-crate feature would force `crate-type = ["rlib", "cdylib"]` *unconditionally* — every
`cargo build`/`test` would link a full extra shared object of this 40+ MB-class crate (a permanent
build-time tax on the default path), or require non-standard `cargo rustc --crate-type`
invocations. Decision: a **sibling crate `capi/`** (`ascript-capi`, `crate-type = ["cdylib",
"staticlib"]`, depending on `ascript = { path = "..", features = ["embed"] }`), with its own empty
`[workspace]` — the exact precedent `tree-sitter-ascript/` set so the root build doesn't absorb
it. The `extern "C"` fns are *defined in* the capi crate (never re-exported across an rlib
boundary, where unreferenced `#[no_mangle]` symbols can be stripped). Main-crate builds are
untouched; CI builds capi explicitly (`cargo test --manifest-path capi/Cargo.toml`).

### 8.2 Surface (handle-based, panic-safe, length-explicit — header excerpt)

```c
/* ascript.h — hand-written, checked in at capi/include/ascript.h.
 * ABI: every handle is opaque; every string is UTF-8 with explicit length;
 * every fn returns as_status; nothing is thread-safe — see THREADING below. */
#define ASCRIPT_CAPI_ABI 1
typedef struct as_isolate as_isolate;
typedef struct as_value   as_value;
typedef enum {
  AS_OK = 0, AS_ERR_COMPILE = 1, AS_ERR_PANIC = 2, AS_ERR_EXIT = 3,
  AS_ERR_UTF8 = 4, AS_ERR_TYPE = 5, AS_ERR_UNDEFINED = 6, AS_ERR_CONFIG = 7,
  AS_ERR_WRONG_THREAD = 8, AS_ERR_NESTED_RUNTIME = 9, AS_ERR_POISONED = 10,
  AS_ERR_INTERNAL = 127
} as_status;

uint32_t ascript_version(void);      /* packed crate semver: major<<16|minor<<8|patch */
uint32_t ascript_abi_version(void);  /* ASCRIPT_CAPI_ABI of the loaded library */

as_isolate *as_isolate_new(void);                   /* deny-all caps, captured output */
as_isolate *as_isolate_new_with_caps(const char *const *grant, size_t n);
void        as_isolate_free(as_isolate *);

as_status as_eval(as_isolate *, const char *src, size_t src_len, as_value **out);
as_status as_call(as_isolate *, const char *name, size_t name_len,
                  const as_value *const *args, size_t nargs, as_value **out);
/* Last error message for this isolate (borrowed; valid until the next call). */
as_status as_last_error(const as_isolate *, const char **msg, size_t *msg_len);
as_status as_take_output(as_isolate *, char **out, size_t *out_len); /* free w/ as_string_free */

/* Values: make (owned by caller until passed/freed) + read. */
as_value *as_nil(void);  as_value *as_bool(bool);  as_value *as_int(int64_t);
as_value *as_float(double);
as_value *as_string(const char *utf8, size_t len);            /* NULL on invalid UTF-8 */
as_status as_value_kind(const as_value *, int *out);          /* AS_KIND_* enum */
as_status as_value_int(const as_value *, int64_t *out);       /* AS_ERR_TYPE on mismatch */
as_status as_value_float(const as_value *, double *out);
as_status as_value_bool(const as_value *, bool *out);
as_status as_value_string(const as_value *, const char **ptr, size_t *len); /* borrowed */
as_status as_value_to_json(const as_isolate *, const as_value *, char **out, size_t *len);
as_status as_json_parse(as_isolate *, const char *json, size_t len, as_value **out);
void      as_value_free(as_value *);
void      as_string_free(char *);

/* Host functions: C callback + userdata. tier: 0 = plain, 1 = fallible (Tier-1). */
typedef as_status (*as_host_fn)(void *userdata, as_isolate *iso,
                                const as_value *const *args, size_t nargs,
                                as_value **out, char **err_utf8 /* as_string_free'd */);
as_status as_register_host_fn(as_isolate *, const char *module, size_t module_len,
                              const char *name, size_t name_len,
                              as_host_fn fn, void *userdata, int tier);
```

Design rules (each is a test in §11):

- **Panic safety:** every `extern "C"` body is `catch_unwind(AssertUnwindSafe(..))`; a caught Rust
  panic stores the message, **poisons the isolate** (subsequent calls → `AS_ERR_POISONED` except
  `as_isolate_free`/`as_last_error`), and returns `AS_ERR_INTERNAL`. Unwinding never crosses the
  boundary. Host callbacks are called *from* Rust; a callback that itself unwinds C++ exceptions is
  the host's UB (documented), but a callback returning an error status is mapped to
  `HostError` cleanly.
- **Thread affinity is CHECKED, not UB** (the honest decision): `as_isolate` records its creating
  `ThreadId`; **every** entry compares (`std::thread::current().id()` — a cheap TLS read +
  integer compare) and returns `AS_ERR_WRONG_THREAD` instead of touching any `Rc`. `as_value`
  handles carry the owning isolate's thread id too; the one unfixable case — `as_value_free` from
  the wrong thread — **leaks the box and returns** (an `Rc` refcount decrement off-thread is a
  data race; a documented leak beats UB). Stated in the header's THREADING block.
- **Ownership:** every `out` value is caller-owned (`as_value_free`); every `char* out` is
  `as_string_free`'d; borrowed pointers (`as_value_string`, `as_last_error`) are valid until the
  next call on the same isolate/value — documented per-fn in the header.
- **Versioning:** `ascript_abi_version()` is the load-time guard (a host asserts
  `== ASCRIPT_CAPI_ABI` from its header); `ascript_version()` reports the crate semver. The ABI
  constant bumps only on a breaking C-surface change — the C ABI is the *only* stable ABI (§10).

### 8.3 Header drift + C smoke test (realistic CI design)

`ascript.h` is hand-written and checked in. Two guards, both ordinary `cargo test`s in the capi
crate (no bespoke CI machinery): (1) **drift test** — parse the header's `as_*`/`ascript_*`
declarations (regex over the checked-in file) and the crate's `#[no_mangle] pub extern "C" fn`
list (a `src/lib.rs` include-introspection via a generated symbol inventory module), assert
set-equality both directions; (2) **smoke test** — `tests/c_smoke.rs` uses the `cc` crate (dev-dep)
at *test time* to compile `tests/smoke.c` (eval, call, host fn, error paths, free discipline) and
link it against the freshly built cdylib (`CARGO_*` env locates `target/`), then runs it and
asserts exit 0. Both run in CI via `cargo test --manifest-path capi/Cargo.toml`; macOS + Linux are
covered by the existing matrix, Windows linkage is best-effort v1 (documented; the cdylib builds,
the smoke test is `#[cfg(unix)]` until a Windows runner exists — an owner-noted deferral, not
silent).

## 9. Stability policy (what is contract, what is exempt)

- **Stable under semver (source-level):** `ascript::embed::*` (every pub item in the facade) and
  the C ABI of `ascript.h` (guarded by `ASCRIPT_CAPI_ABI`). Pre-1.0 rules: breaking changes bump
  the minor version and are CHANGELOG'd; the embed module's rustdoc carries the contract section.
- **Exempt, with a documented exemption (the facade recommendation, adopted):** everything else
  `pub` in the crate. Investigation (verified): `src/lib.rs` declares ~30 `pub mod`s because the
  bin target, the 30+ integration-test binaries, the fuzz targets, and `tree-sitter` conformance
  all link the lib — shrinking that to `pub(crate)` is a multi-thousand-line churn across
  `tests/` with zero user benefit. Decision: **do not shrink; demarcate.** The crate root docs
  gain a "Stability" section declaring `ascript::embed` (and the existing doc'd CLI behavior) the
  only contract, everything else "internal, pub for the bin/tests, no stability promise"; a
  `#[doc(hidden)]` sweep is applied to the *root-level* fn seams that aren't already hidden
  (verified: most already are — `vm_run_source`, `vm_eval_source`, `run_source_deterministic`,
  etc. carry `#[doc(hidden)]`; the sweep audits stragglers like `run_source`/`run_file` which stay
  visible but get "CLI entry, not the embedding contract — use `ascript::embed`" doc pointers).
  docs.rs therefore renders `embed` as the API; internals don't appear.

## 10. Scope & rejected alternatives (recorded so they aren't re-litigated)

- **`Send` isolates / cross-thread isolate sharing — FORBIDDEN.** The model is the product
  (`assert_not_impl_any!(Value: Send, Sync)` is load-bearing); a mutex'd `Isolate` would
  deadlock on re-entrancy and serialize all hosts. One isolate per thread; N threads → N isolates.
- **Stable Rust ABI — rejected.** Semver source-level only; `#[repr(C)]`-ifying the Rust surface
  buys nothing (Rust hosts compile against the crate). The C ABI is the stable ABI.
- **Async C API — rejected v1.** Blocking eval/call only. A C-consumable completion model
  (callbacks or polling) drags an executor contract across the FFI boundary; the C tier targets
  plugin/scripting hosts that call synchronously. A `as_poll`-style API is recorded as future work
  gated on demand.
- **Embedding the toolchain (fmt/check/lsp as API) — recorded future**, not v1. The facade leaves
  room (`embed::tooling` namespace reserved in docs).
- **Host-fn isolate re-entrancy (`ctx.eval(...)`) — rejected v1** (§6.2): re-entrant dispatch
  under live borrows is the classic embedding crash; revisit with a designed re-entry protocol.
- **Async host fns — deferred** (recorded in `embedding.md`): requires suspending the calling
  fiber on a host future; the seam exists (the engine awaits natives elsewhere) but the API design
  (cancellation, `!Send` host futures) deserves its own pass.
- **Determinism seams in the builder (`.deterministic(seed)`) — deferred, recorded.**
  `Interp::enter_deterministic` exists (`src/lib.rs:687`); exposing it is trivially additive later.
- **A built-in cross-thread actor wrapper over Isolate — rejected v1** (§4.2c): shipped as a
  documented example instead; an API would re-import the command-channel model.

## 11. Testing (every gate, mapped)

- **Negative space (Gate 1 / "no language surface"):** `tests/embed_negative_space.rs` pins
  `ASO_FORMAT_VERSION == 27`, no new `Op` (opcode count pin), no grammar file diffs (this spec
  touches neither parser), and `vm_differential.rs` runs **unchanged** — the four-mode identity
  over the corpus is structurally untouched because EMBED adds no engine behavior. The two core
  touches (classify_specifier `host:` arm; call_stdlib fall-through arm) get *both-engines* parity
  tests: the same host-module program run on tree-walker (`Interp` + registration directly) and VM
  (via `Isolate`) asserts byte-identical output incl. the miss-panic message.
- **Unit + integration, happy AND edge, BOTH feature configs** (Gates 3/10): every `embed` API fn —
  eval (value/statement/compile-error/panic-survives-session/exit/propagate), call
  (undefined/not-callable/async-auto-await/wrong-arity panics passthrough), globals
  (get/set/immutability of `const`), load_archive (valid/corrupt/wrong-version bytes →
  `Archive` error verbatim from the verifier), output modes, builder validation
  (bad host names, dup registration), `NestedRuntime` detection (call `eval` inside
  `#[tokio::test]`), eval_async under b1/b2 configurations.
- **Value bridge:** a 25-kind round-trip table test — construct-or-produce each `Value` kind in
  script, read via `AsValue`, assert its crossing class (§5.2): scalars round-trip by value,
  containers alias (mutate via host, observe in script, and vice versa), opaque kinds pass back
  `==`-identical, `to_json` errors carry field paths. Decimal lossless string round-trip.
- **Host modules:** tiering (plain `func` Err→Tier-2 recoverable-by-`recover`; `fallible_func`
  `[v, err]` shapes), `HostError::Panic`, host-fn-bypasses-caps proof (deny-all isolate still runs
  host fns — asserted + documented), worker miss-panic message, `host_module_factory` in a
  dedicated AND a pooled worker (and the pooled no-leak test: two Isolates on one thread's pool
  don't see each other's modules — the caps-floor discipline test, FFI §8 precedent).
- **Caps:** deny-all default proof (`fs.read` → `capability 'fs' denied` on a fresh
  builder-default isolate), `granting`, in-script `caps.drop` still monotone, CLI-vs-embed default
  difference asserted in a doc-test.
- **C API:** the §8.3 smoke + drift tests; panic-through-boundary (a deliberately-panicking host
  callback path + an internal-panic injection seam) → `AS_ERR_INTERNAL` then `AS_ERR_POISONED`,
  never abort; wrong-thread (spawn a thread, call `as_eval` → `AS_ERR_WRONG_THREAD`; free →
  documented leak + error); invalid UTF-8 in/out; every make/read pair; double-free safety
  (handles are boxes — document single-free, test the error paths we can detect).
- **Gate 12 (zero perf regression):** the touched dispatch sites are cold-path by construction
  (§1 tenet 5); `vm_bench` geomean re-run pre/post in the same session, recorded in
  `bench/EMBED_RESULTS.md` (expected ≈1.0×; the gate is the proof, not the expectation). Peak RSS
  on the corpus re-recorded (Gate 18 of `goal-perf.md`).
- **Examples are tested** (Gate 9): the rust-host example builds + runs in CI (its stdout
  asserted), the c-host smoke runs in CI (§8.3), and the `.as` scripts they load are valid
  standalone (run via `ascript run` in the example test for four-mode sanity; they live under
  `examples/embed/` which the corpus discovery — `examples/*.as` + `examples/advanced/*.as`,
  verified non-recursive — does not auto-claim).

## 12. Examples, docs, cross-cutting checklist

- **`examples/embed/rust-host/`** — a cargo example (`[[example]] name = "embed-rust-host",
  path = "examples/embed/rust-host/main.rs", required-features = ["embed"]`): a game-loop host —
  builder with `host:game` module (`log`, `rand_seeded`), loads `game.as` defining `on_tick`,
  calls it per frame, reads a script-side state object by handle, demonstrates deny-all caps and
  a `recover`-handled host panic. **`examples/embed/c-host/`** — `main.c` + `Makefile` (compile
  against `capi/include` + the built cdylib): eval, call, host fn via callback+userdata, error
  handling, frees. Both CI-exercised (§11).
- **Docs:** new `docs/content/embedding.md` (the contract: model headline, builder, threading
  table b1/b2/c, caps inversion warning box, host modules + worker rules, AsValue kind table, C
  API + thread-affinity rules, deep-recursion reality) — **added to `NAV`** in
  `docs/assets/app.js` (Introduction section, after `runtime`; the orphan-page gotcha).
  `README.md` gains an "Embedding" section (Rust + C snippets). Rustdoc on `ascript::embed` is
  the normative Rust reference.
- **Cross-cutting (CLAUDE.md checklist):** no grammar/fmt/LSP/REPL surface (no syntax). `.aso`
  untouched. CLAUDE.md gains an EMBED subsection; `superpowers/roadmap.md` + `goal-perf.md` status
  flipped at merge. The `unresolved-import` checker treats `host:` specifiers as
  satisfied-by-construction at *runtime registration* time — statically unknowable, so the checker
  **skips** `host:`-prefixed imports (a one-arm addition to the import rule, tested; never a false
  positive on embed-targeted scripts).

## 13. Grounding (verified sources)

`src/lib.rs:85,119,632,739,791,2416` (entry points; `run_on_worker_stack`); `src/main.rs:475-488`
(CLI runtime construction); `src/repl.rs:95-281` (the session substrate); `src/vm/run.rs:4462`
(`Vm::call_value`), `:5154` (`user_global`), `:165` (`user_globals`); `src/interp.rs:461+`
(`Interp` fields: `resources:471`, `caps:603`, `determinism:558`), `:974/:980` (`new`/`new_live`),
`:1037` (`set_caps`), `:2819` (`load_std_module`), `:2882` (`classify_specifier`), `:3430`
(import arms), `:6419` (builtin `split_once('.')` → `call_stdlib`), `:734` (`WORKER_STACK_SIZE`);
`src/stdlib/mod.rs:114` (`std_module_exports`), `:221` (`STD_MODULES`), `:325` (`required_cap`);
`src/stdlib/caps.rs:353-403` (`CapSet`, `deny`, `deny_all_dangerous`); `src/value.rs:1101-1203`
(the kind inventory + the `!Send` assert); `src/worker/isolate.rs:211,287` (spawn paths),
`src/worker/pool.rs:26` (thread-local pool); `src/vm/aso.rs:167` (`ASO_FORMAT_VERSION = 27`);
`Cargo.toml` (lib+bin layout, feature table); `tests/vm_differential.rs:885` (corpus discovery is
non-recursive); FFI spec §3/§4 (tiering + caps vocabulary); `docs/assets/app.js:11` (`NAV`).
