:::eyebrow Introduction

# Embedding (Rust & C)

AScript ships a stable embedding tier so a host application can hold a long-lived engine, call
script functions repeatedly (a game loop, a plugin hook), hand the script host-defined functions,
move values across the boundary without printing them, and do all of this from **Rust** or **C**.

There are two surfaces:

- **`ascript::embed`** — a small, semver-contracted Rust facade: a builder-constructed
  [`Isolate`](#the-rust-api), an `AsValue` value bridge, host modules under a collision-proof
  `host:` namespace, and host-decided capabilities.
- **`ascript-capi`** — a `cdylib`/`staticlib` with a hand-written, checked-in `ascript.h`:
  handle-based, panic-safe, length-explicit UTF-8, manual `free`, thread-affinity *checked*.

> [!NOTE] To build the bare language for embedding, use `--no-default-features` and add back only
> the stdlib features you need. The `embed` feature is in `default`; the CLI binary compiles
> identically whether or not it is enabled.

## The model: one isolate per thread

**The headline is the model, not the API.** AScript's runtime is `!Send` per isolate — `Rc`/`RefCell`
state on a current-thread reactor; parallelism comes from *more isolates*, never from sharing memory.
For embedding this is a strength, not a limitation: **one isolate per host thread, zero global VM
lock, zero cross-isolate interference** — the property Lua's `lua_State`-per-thread idiom approximates
by convention, AScript enforces by type. An `Isolate` is `!Send + !Sync` by construction. A host that
wants N threads creates N isolates.

This page is the inverse of [FFI](stdlib/ffi): FFI marshals AScript values *out* to a C ABI the runtime
doesn't control; embedding marshals host values *in* to a runtime the host doesn't control. The error
model is the same two-tier split — Tier-1 `[value, err]` pairs for environment facts, Tier-2 panics
for misuse — and "no new value kind" is a tenet of both.

## The Rust API

```rust
use ascript::embed::{Isolate, AsValue, Caps, HostError, OutputMode};

let iso = Isolate::builder()
    .caps(Caps::deny_all())          // the DEFAULT — shown for emphasis (see Capabilities below)
    .output(OutputMode::Capture)     // print → buffer (default: Inherit → stdout)
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
# Ok::<(), ascript::embed::EmbedError>(())
```

### `Isolate`

`Isolate::builder()` returns an `IsolateBuilder`; `build()` validates and constructs the isolate on
the **calling thread** (it spawns no thread — the isolate *is* the caller's thread). The builder is
additive:

| Builder method | Default | Effect |
|---|---|---|
| `.caps(Caps)` | `Caps::deny_all()` | the isolate's capability set (see below) |
| `.stdlib(StdlibFilter)` | `StdlibFilter::Full` | which compiled-in stdlib modules are *available* |
| `.output(OutputMode)` | `OutputMode::Inherit` | `print` → stdout (`Inherit`) or a buffer (`Capture`) |
| `.args(&[&str])` | empty | the script's `cli.args` |
| `.host_module(name, f)` | — | install a `host:` module (this isolate only) |
| `.host_module_factory(name, arc_f)` | — | install a `host:` module into this isolate **and** every worker isolate it spawns |

The constructed `Isolate` exposes:

- **`eval(src) -> Result<AsValue, EmbedError>`** — compile + run `src` on the persistent VM,
  **blocking** the calling thread until the program (and everything it spawned) is quiescent. Returns
  the trailing-expression value (`Nil` for a statement-terminated input). State persists across
  `eval` calls (the REPL session model): a binding from an earlier `eval` is visible to a later one. A
  Tier-2 panic returns `EmbedError::Panic` but **the session survives** — the isolate stays usable.
- **`call(name, args)`** — call a module-scope global by name. If the callee is an `async fn`, the
  returned future is driven to completion and its resolved value returned.
- **`call_value(callee, args)`** — call a callable `AsValue` previously read out (a function handle).
- **`global(name)` / `set_global(name, value)`** — read / define-or-overwrite a module-scope global.
- **`load_archive(bytes)`** — load + run a compiled `.aso` archive (`ascript build` output) through
  the same trust boundary the CLI uses (`Chunk::from_bytes_verified`).
- **`take_output()`** — drain the capture buffer (empty under `Inherit`).
- **`json_parse(text)`** — the explicit deep bridge (see [AsValue](#the-asvalue-bridge)).

Each method has an `*_async` sibling (`eval_async`, `call_async`, …) that returns the engine future
for hosts that already drive their own tokio runtime (see the threading table below).

### `EmbedError`

`EmbedError` is `#[non_exhaustive]`. The variants and their structured fields are semver contract;
the diagnostic *strings* are not (wording may improve). The variants: `Compile(Vec<EmbedDiagnostic>)`,
`Panic(EmbedPanic)`, `Exit(i32)`, `NestedRuntime`, `Undefined(String)`, `Config(String)`,
`Archive(String)`.

## Threading & async

An `Isolate` **owns a current-thread tokio runtime**. Blocking `eval`/`call` run the engine future on
it via `LocalSet::block_on` — the engine needs a reactor (for the stdlib's tokio-backed I/O and
timers) and a `LocalSet` (for the engine's `spawn_local` async scheduling). The `*_async` variants
instead return the `!Send` engine future for the host to drive.

| Configuration | Supported | How |
|---|---|---|
| **b1** — host with a `current_thread` runtime | ✔ | `await` `eval_async` inside `LocalSet::run_until` / `LocalSet::block_on` on that runtime |
| **b2** — host with a multi-thread runtime, driving from a non-worker thread | ✔ | `local.block_on(&rt, iso.eval_async(..))` — runs the `!Send` future on the *calling* thread while the multi-thread reactor serves I/O |
| **plain sync host** (game loop / plugin host, no tokio of its own) | ✔ | the **blocking** `eval`/`call` — the isolate's owned runtime parks the calling thread |
| **c** — awaiting from a `tokio::spawn`ed task on a multi-thread runtime | ✗ | impossible by construction — `tokio::spawn` requires `Send` and the engine future is `!Send`; this is a **compile error** at your call site (the type system stating the model) |

> [!WARN] **Nested-runtime hazard, detected not papered over.** Calling a *blocking* `eval`/`call`
> from inside an async context (an ambient tokio runtime) would panic in tokio. Every blocking entry
> first checks `Handle::try_current()` and returns `EmbedError::NestedRuntime` instead — a typed
> error naming `eval_async` as the fix. The escape for configuration (c) is the same one the CLI
> uses: dedicate a thread to the isolate (`std::thread::spawn` + blocking `eval`, bridging results
> over channels). v1 ships that pattern as a documented example, not as an API.

### Host functions run synchronously

A host function is invoked synchronously inside the engine's dispatch — **a blocking host fn stalls
the entire isolate** (no preemption; the same rule as a blocking C call under FFI). Cancel/timeout
cannot interrupt it (there is no `.await` point inside it). Async host functions are a recorded
deferral, not a silent gap.

### Deep recursion: the host-stack reality

The CLI's deep-recursion headroom comes from a **512 MB worker-thread stack** in its `main` — that
trick belongs to the CLI, and an embedded isolate runs on whatever stack the host thread has (8 MB on
the default main thread; commonly 512 KB–2 MB for host-spawned threads). Two engine-level protections
still apply on *any* stack: (1) the logical `MAX_CALL_DEPTH` / expression-nesting guards convert
runaway recursion into the clean, catchable `maximum recursion depth exceeded` panic; (2) the VM
allocates heap-backed stack segments at native re-entry points when the remaining stack falls below a
red zone, so deep-but-legal recursion reaches the logical cap cleanly even on a small host stack.

> [!NOTE] A host that wants CLI-identical headroom runs the isolate on its own thread with an
> enlarged stack — `std::thread::Builder::new().stack_size(512 * 1024 * 1024).spawn(..)` — and drives
> the isolate's blocking API from inside it. One line; it is the configuration-(c) dedicated-thread
> pattern.

## Capabilities — DEFAULT DENY-ALL

> [!WARN] **The embedded default is the inverse of the CLI's.** An embedded isolate defaults to
> `Caps::deny_all()` — fs, net, process, ffi, and env all **denied**. This is the loud inversion of
> the CLI, where capabilities default to all-granted. The rationale: a CLI program is the artifact the
> *user* chose to run; an embedded script is characteristically *someone else's plugin inside the
> host's process*, and the host — not the script's author — owns the blast radius. **The host grants,
> explicitly, at build time.**

```rust
use ascript::embed::{Caps, Cap};

Caps::deny_all();                 // the default: every capability denied
Caps::all_granted();              // CLI-equivalent (trusted scripts only)
Caps::granting(&[Cap::Net]);      // deny-all + explicit carve-ins
```

Granting at construction does not violate capability monotonicity: the invariant is "no isolate ever
re-gains a capability it *dropped*", and construction precedes all drops. After `build()`, the
in-script `caps.drop` and the whole [capability machinery](stdlib/caps) work unchanged — the embedded isolate
is a top-level-program isolate in the capability model's terms. Granular fs/net carve-outs reuse the
same `FsScope`/`NetScope` the CLI uses.

> [!WARN] **Host functions bypass capabilities.** A host fn is the *host's own trusted code*; it is
> not capability-gated. If a host fn proxies a dangerous effect (reads a file, opens a socket), gating
> that effect is **your** job — do it inside the host fn. The cap gate covers every `std/*` effect
> path by construction; it does not and cannot reach inside your callbacks.

## Host modules

Host modules import under the `host:` URI scheme: `import * as app from "host:app"`. The scheme is
collision-proof — `std/…` is claimed by prefix, and a `:` can never appear in a package key, so a
real installed package can never shadow a host module (the reservation is structural in
`classify_specifier`). Module names are validated at registration: `host:` followed by
`[a-z][a-z0-9_]*(/[a-z][a-z0-9_]*)*`, **no dots** (the builtin dispatch splits the qualified function
name at the first `.`, so a dotted module name would mis-split — rejected with `EmbedError::Config`).

A module is built with two function tiers, mirroring [FFI](stdlib/ffi)'s split exactly — **the host chooses
per function**:

- **`func(name, f)`** — a plain host function. `Ok(v)` returns `v`. `Err(HostError::Recoverable(e))`
  has no err channel in a plain fn, so it is *upgraded* to a Tier-2 recoverable panic (upgrading beats
  silently swallowing). `Err(HostError::Panic(e))` is a Tier-2 recoverable panic (catchable by
  `recover`).
- **`fallible_func(name, f)`** — a Tier-1 fallible function that **always** returns the `[value, err]`
  pair: `Ok(v)` → `[v, nil]`; `Err(HostError::Recoverable(e))` → `[nil, {message: e}]`. The script
  handles it with `?`. Use this for environment facts the script should recover from.

A constant export is `value(name, v)`. A missing host module (the CLI always misses — it registers
none) is a typed Tier-2 panic, `host module 'host:app' is not registered in this isolate`
(recoverable).

### Workers

Host functions are `!Send` `Rc<dyn Fn>` and structurally cannot cross the worker airlock. So:

- **Default: host modules are main-isolate-only.** In a worker isolate, `import "host:app"` hits a
  worker-specific miss panic: `host module 'host:app' is not available in a worker isolate (register
  it with host_module_factory to install it per-isolate)`. Loud, typed, documented — never a hang or a
  silent nil.
- **Opt-in: `host_module_factory(name, arc_f)`** — an `Arc<dyn Fn(&mut HostModuleBuilder) + Send +
  Sync>` that *also* installs the module into every [worker isolate](language/workers) this isolate spawns.
  Because the factory runs inside the freshly-spawned worker thread, its closure may close over
  `Send + Sync` host state only — the type signature enforces it.

### `StdlibFilter` is availability, not security

`StdlibFilter::Full` (default — every compiled-in module) / `Core` (the no-OS subset: everything that
maps to no capability, minus `std/ffi`) / `Allow(&["std/math", …])`. It is enforced at the import
chokepoint; a filtered module reports `module 'std/fs' is not available in this isolate`. **The filter
is an availability knob, not a security boundary** — an allowlisted module's transitively-reachable
builtins are not re-walked. **Capabilities are the security boundary.** To sandbox an untrusted
script, set deny-all caps (the default); the filter only narrows what is *importable*.

## The `AsValue` bridge

`AsValue` is a `!Send` newtype over the engine's `Value`. Because every AScript container is already
`Rc`/`Cc`-backed shared interior-mutable state, **a clone of a container value IS a handle**: an
`AsValue` over an `Object` aliases the *same* cell the script holds. Host writes are visible to the
script and vice versa, identity is preserved, and crossing the boundary costs one refcount bump — the
Lua-table model. Scalars and strings cross by value.

Deep conversion is never implicit — it is an explicit airlock: `AsValue::to_json` / `Isolate::json_parse`
(and a `serde` bridge under the `data` feature) produce detached *copies*, stated loudly.

| `Value` kind(s) | Crossing class | Host operations |
|---|---|---|
| `Nil`, `Bool`, `Int`, `Float` | **by value** | full construct + read |
| `Str` | **by value** (construct) / borrow (read) | `from`, `as_str` |
| `Decimal` | **by value via string** (lossless) | `decimal(&str)`, `as_str` of the display form |
| `Array`, `Object`, `Map`, `Set` | **live handle** (aliasing clone) | `len`/`get`/`get_key`/`set`/`set_key`/`items`/`entries`; `array`/`object` construct |
| `Bytes` | **live handle** | `as_bytes` (copy out), `bytes(vec)` construct, `len` |
| callables (`Function`, `Closure`, `Builtin`, `BoundMethod`, `ClassMethod`, `EnumVariant` ctor) | **callable handle** | `is_callable`, `Isolate::call_value` |
| `Future` | **opaque handle** | `call*` auto-awaits; a held `Future` keeps its task alive (cancel-on-drop preserved) |
| `Generator`, `Native`, `Class`, `Enum`, `Interface`, `Regex`, `Shared`, … | **opaque handle** | `kind`/`type_name`, pass back in unchanged |

**Nothing errors on crossing** — every kind is at least an opaque, pass-back-able handle. This is the
inverse of the worker airlock (which must serialize, and therefore rejects non-sendable values): the
embed boundary is same-thread, so a handle is always sound. `to_json` is the one operation that errors
on a non-serializable kind, with the field path, reusing `std/json`'s shipped serializer.

## The C API (`ascript-capi`)

`ascript-capi` is a sibling crate (`cdylib` + `staticlib`) with a hand-written, checked-in
`include/ascript.h`. Every handle is opaque; every string is UTF-8 with an explicit length; every
function returns an `as_status`; nothing is thread-safe.

```c
#include "ascript.h"

as_isolate *iso = as_isolate_new();          /* deny-all caps, captured output */
as_value *out = NULL;
if (as_eval(iso, "fn add(a,b){return a+b}", 22, &out) == AS_OK) {
    as_value_free(out);
    as_value *a = as_int(2), *b = as_int(40);
    const as_value *args[] = { a, b };
    if (as_call(iso, "add", 3, args, 2, &out) == AS_OK) {
        int64_t n; as_value_int(out, &n);     /* 42 */
        as_value_free(out);
    }
    as_value_free(a); as_value_free(b);
}
as_isolate_free(iso);
```

Design rules (each is a test):

- **Panic safety.** Every `extern "C"` body is `catch_unwind`-wrapped. A caught Rust panic stores the
  message, **poisons the isolate** (subsequent calls return `AS_ERR_POISONED` except `as_isolate_free`
  / `as_last_error`), and returns `AS_ERR_INTERNAL`. Unwinding never crosses the boundary.
- **Thread affinity is CHECKED, not UB.** An `as_isolate` records its creating `ThreadId`; *every*
  entry compares the current thread (a cheap TLS read) and returns `AS_ERR_WRONG_THREAD` instead of
  touching any `Rc`. The one unfixable case — `as_value_free` from the wrong thread — **leaks the box
  and returns** (an off-thread `Rc` decrement is a data race; a documented leak beats UB).
- **Ownership.** Every `out` value is caller-owned (`as_value_free`); every `char* out` is
  `as_string_free`'d; borrowed pointers (`as_value_string`, `as_last_error`) are valid until the next
  call on the same isolate/value.
- **Host functions bypass caps**, exactly as in Rust — a C host callback (`as_register_host_fn` with a
  callback + `userdata`) is the host's trusted code; gate its own effects.
- **Versioning.** `ascript_abi_version()` is the load-time guard (assert it equals `ASCRIPT_CAPI_ABI`
  from the header); `ascript_version()` reports the crate semver. The C ABI is the *only* stable ABI.

## Stability policy

- **Stable under semver (source-level):** `ascript::embed::*` — every public item in the facade — and
  the C ABI of `ascript.h` (guarded by `ASCRIPT_CAPI_ABI`). Pre-1.0, a breaking change bumps the minor
  version and is recorded in the changelog. The `ascript::embed` rustdoc carries the contract.
- **Exempt, documentedly:** everything else `pub` in the crate. The crate's ~30 `pub mod`s exist for
  the bin target, the integration tests, and the fuzz/conformance harnesses — they carry **no**
  stability promise. The crate root docs declare `ascript::embed` (and the existing documented CLI
  behavior) the only contract; the root-level CLI entry fns (`run_file`, `run_source`) note "CLI
  entry, not the embedding contract — embed via `ascript::embed`". docs.rs renders `embed` as the API;
  internals do not appear.

## See also

- [Capabilities & sandboxing](stdlib/caps) — the capability model the embedded deny-all default sits on.
- [FFI (C interop)](stdlib/ffi) — the inverse boundary (AScript values *out* to C).
- [Workers & parallelism](language/workers) — the isolate model host-module factories install into.
- [Compilation & runtime](runtime) — `.aso` archives (`load_archive` input) and the VM.
