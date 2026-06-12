# Embedding API — Rust crate + C API (EMBED) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. At the end of each
> phase, a **holistic per-phase review subagent** reviews the phase's combined changes before
> the next phase starts. A task/phase is closed only when every box under it is ticked.

**Goal:** Ship the stable embedding tier: `ascript::embed` (feature `embed`, default-on) — a
`!Send` `Isolate` (builder: caps default **deny-all**, stdlib filter, host modules under the
`host:` namespace, output mode) with blocking `eval`/`call`/`load_archive` + `!Send` async
variants, an `AsValue` bridge (scalars by value, containers as live aliasing handles, explicit
JSON deep bridge) — plus the sibling `capi/` crate (`ascript-capi`: cdylib/staticlib +
hand-written `ascript.h`, panic-safe, thread-affinity-checked, ABI-guarded) — with **no grammar
change, no opcode, no `ASO_FORMAT_VERSION` bump (pin 27), no `Value` variant**, and zero hot-path
cost (the only core touches are the cold `host:` import arm and the already-error `call_stdlib`
fall-through arm).

**Spec:** `superpowers/specs/2026-06-12-embedding-api-design.md` (EMBED). **Read it first and in
full** — §2 (the verified machinery being reused), §4 (the threading contract: what is supported
v1 vs rejected — implement EXACTLY that), §5.2 (the kind table — every row becomes a test), §6
(host modules: namespace validation, tiering, the worker factory riding the caps precedent), §7
(deny-all default), §8 (C API rules: poisoning, thread checks, ownership), §9 (stability
demarcation), §11 (the test matrix). Section references (§) below are into it.

**Before writing any code, read these files end to end** (line numbers verified 2026-06-12 —
**re-grep every symbol before editing**, names are the anchors):
- `src/repl.rs` (`run_repl_vm` + `eval_line_vm` — the eval substrate being lifted)
- `src/lib.rs:60-160, 632-900, 2416+` (entry-point patterns; `run_on_worker_stack`; `run_archive`)
- `src/interp.rs` (`Interp::new`/`new_live` ~`:974`; `set_caps` ~`:1037`; `load_std_module`
  ~`:2819`; `classify_specifier` ~`:2882`; the import arms ~`:3430`; `call_builtin`'s
  `split_once('.')` fall-through ~`:6419`)
- `src/stdlib/mod.rs` (`std_module_exports` `:114`; `required_cap` `:325`; the `call` dispatch +
  its terminal match)
- `src/stdlib/caps.rs` (`CapSet`, `deny`, `deny_all_dangerous`, `FsScope`/`NetScope`)
- `src/vm/run.rs` (`user_global` ~`:5154`; `define_user_global` ~`:5231`; `call_value` ~`:4462`)
- `src/worker/isolate.rs` (spawn paths + `WorkerRequest`; how `caps` rides — the factory mirrors
  it) + `src/worker/pool.rs` (thread-local pool `:26`)
- `src/value.rs:1101-1203` (the kind inventory + `assert_not_impl_any`)
- `tree-sitter-ascript/Cargo.toml` (the own-`[workspace]` precedent `capi/` copies)

**Architecture:** Phase 1 (Unit A — the facade core): `src/embed/` (error, builder, Isolate,
eval/call/globals/archive), feature `embed`. Phase 2 (Unit B — AsValue + JSON bridge + kind
table). Phase 3 (Unit C — host modules: registry on `Interp`, `host:` specifier arm in BOTH
engines, dispatch fall-through, worker rules + factory, stdlib filter, checker skip). Phase 4
(Unit D — `capi/` crate + header + drift + C smoke). Phase 5 (Unit E — examples rust-host/c-host
+ CI wiring). Phase 6 (Unit F — docs/NAV/README/CLAUDE/roadmap/goal-perf, stability sweep, bench
A/B + RSS, negative-space pins, holistic review).

**Tech stack:** Rust; the `!Send` per-isolate runtime (never add `Send` bounds; never hold a
`RefCell` borrow across `.await` — clippy denies it); tokio `current_thread` + `LocalSet`;
`cc` as a dev-dependency of `capi/` only; tests green in BOTH feature configs
(`cargo test` and `cargo test --no-default-features`); clippy clean in both.

**Hard rules carried from the spec:**
- **No grammar / opcode / `.aso` / `Value` change.** `ASO_FORMAT_VERSION` stays 27
  (`src/vm/aso.rs:167`) — the negative-space test pins it. `vm_differential.rs` is untouched.
- **The only `Interp`/dispatch touches:** `SpecifierKind::Host` checked FIRST in
  `classify_specifier` (cold import path); the host-registry lookup on `call_stdlib`'s
  **fall-through** (already-error) arm; `host_modules` + `stdlib_filter` fields on `Interp`;
  the factory side-channel on the worker spawn paths (exactly where `caps` rides).
- **Caps default for embedded = deny-all** (`Caps::deny_all()`); CLI behavior unchanged
  (all-granted). Host fns bypass caps — documented loudly + tested.
- **Blocking entries detect ambient runtimes** (`Handle::try_current()` → `NestedRuntime`).
- **C API:** every `extern "C"` is `catch_unwind`-wrapped; thread-affinity checked on EVERY
  entry; wrong-thread free LEAKS + errors (never an off-thread `Rc` decrement).
- `embed` is default-on but everything compiles cleanly with it OFF
  (`--no-default-features` has no `src/embed/`); `capi/` is a separate crate, root builds
  untouched.

**Binding execution standards (production-grade mandate):** any bug found while working — ours
or pre-existing, direct or incidental — is fixed in-branch with a failing-test-first regression
guard, never stepped around (goal.md Gate 14). No placeholders, no silent deferrals. Branch:
`feat/embedding-api` off `main`. Commit per task with the house trailer:
`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

## File structure

**New files:**
- `src/embed/mod.rs`, `src/embed/error.rs`, `src/embed/value.rs`, `src/embed/host.rs`
  (all `#[cfg(feature = "embed")]` via the single `pub mod embed` gate in `lib.rs`)
- `capi/Cargo.toml` (own `[workspace]`), `capi/src/lib.rs`, `capi/include/ascript.h`,
  `capi/tests/c_smoke.rs`, `capi/tests/smoke.c`, `capi/tests/header_drift.rs`
- `tests/embed.rs` (integration: builder/eval/call/caps/host/worker matrix)
- `tests/embed_negative_space.rs` (ASO pin, opcode-count pin, no-grammar-diff pin)
- `examples/embed/rust-host/main.rs`, `examples/embed/rust-host/game.as`
- `examples/embed/c-host/main.c`, `examples/embed/c-host/Makefile`,
  `examples/embed/c-host/plugin.as`
- `docs/content/embedding.md`
- `bench/EMBED_RESULTS.md`

**Modified files:**
- `Cargo.toml` — `embed` feature (default; no new deps), the `[[example]] embed-rust-host` entry.
- `src/lib.rs` — `#[cfg(feature = "embed")] pub mod embed;` + the root-doc Stability section +
  the `#[doc(hidden)]`/doc-pointer sweep (§9).
- `src/interp.rs` — `host_modules` + `stdlib_filter` fields; `SpecifierKind::Host`;
  `load_host_module`; the tree-walker import arm; the stdlib-filter check in `load_std_module`.
- `src/stdlib/mod.rs` — the host fall-through arm in the `call` dispatch.
- `src/vm/run.rs` — ONLY if the VM `Op::Import` arm needs its own `SpecifierKind::Host` match arm
  (it routes through the shared classify/loader seam — verify; exhaustive-match additions only).
- `src/worker/isolate.rs` / `src/worker/mod.rs` / `src/worker/pool.rs` — the
  `host_factories` side-channel (mirror `caps` exactly).
- `src/check/` (the unresolved-import rule) — skip `host:`-prefixed specifiers.
- `docs/assets/app.js` (`NAV`), `README.md`, `CLAUDE.md`, `superpowers/roadmap.md`,
  `goal-perf.md` — final phase.

---

## Phase 0 — Preflight: branch + semantic pins

### Task 0.1: branch + pin the shipped substrate EMBED composes

**Files:** `tests/embed_negative_space.rs` (new — starts as the pin home).

- [ ] **Step 1:** `git checkout -b feat/embedding-api main`. `cargo build` clean.
- [ ] **Step 2:** Write PASSING pins (document today's ground truth; if any fails, STOP — the
  spec's substrate moved):

```rust
//! EMBED negative space + substrate pins (spec §11). EMBED adds NO language surface:
//! these pins prove the engine envelope is untouched for the life of the branch.

/// The `.aso` format does not move (spec: no opcode, no serialization change).
#[test]
fn aso_format_version_pinned() {
    assert_eq!(ascript::vm::aso::ASO_FORMAT_VERSION, 27,
        "EMBED must not bump the .aso format — a bump means an engine change leaked in");
}

/// The REPL substrate behaviors Isolate::eval lifts (spec §3.3): trailing-expression
/// value, session persistence across inputs, panic survives the session.
#[tokio::test]
async fn pin_vm_session_substrate() {
    // vm_run_source is one-shot, so pin via two sequential evals on ONE Vm the way
    // eval_line_vm does it (compile_source → Fiber → vm.run), asserting:
    //   eval("let x = 2") → Done(Nil); eval("x + 1") → Done(Int(3)).
    // (Implement with the same ~20 lines repl.rs uses; this test BECOMES the shape
    //  of Isolate::eval in Task 1.2.)
    ...
}

/// user_global is the read hook (vm/run.rs doc comment says "REPL/embedders").
#[tokio::test]
async fn pin_user_global_read_and_call_value() { /* define fn f; user_global("f") → call_value → value */ }
```

- [ ] **Step 3:** Also pin (same file): `classify_specifier` today classifies `"host:app"` as a
  bare package (`UnknownPackage`) — the test asserts the CURRENT behavior with a comment that
  Task 3.2 flips it to `Host` (failing-first discipline for the flip).
- [ ] **Step 4:** `cargo test --test embed_negative_space` green in BOTH configs. Commit —
  `test(embed): phase-0 substrate pins + negative-space ASO pin (spec §11)`.

### Task 0.2: Phase 0 review

- [ ] Independent reviewer: run the pins; confirm `ASO_FORMAT_VERSION` read from source (not
  hardcoded twice); confirm no non-test files changed; confirm the `host:`-classification pin
  documents the intended flip.

---

## Phase 1 — Unit A: the `embed` facade core

### Task 1.1: feature + module skeleton + `EmbedError`

**Files:** `Cargo.toml`, `src/lib.rs`, `src/embed/mod.rs`, `src/embed/error.rs`.

- [ ] **Step 1 (failing test):** in `tests/embed.rs` (new, `#![cfg(feature = "embed")]`):

```rust
use ascript::embed::{Isolate, EmbedError};

#[test]
fn builder_constructs_and_isolate_is_not_send() {
    static_assertions::assert_not_impl_any!(ascript::embed::Isolate: Send, Sync);
    let iso = Isolate::builder().build().expect("default build");
    drop(iso);
}
```

- [ ] **Step 2:** `Cargo.toml`: add `embed = []` to `[features]` and to `default`. NO new
  dependencies (tokio/static_assertions/serde_json are already present; the facade uses only
  in-tree machinery). `src/lib.rs`: `#[cfg(feature = "embed")] pub mod embed;`.
- [ ] **Step 3:** `src/embed/error.rs` — the spec §3.4 enum verbatim (`#[non_exhaustive]`,
  `EmbedDiagnostic { message, start, end, rendered }`, `EmbedPanic { message, span, rendered }`),
  `Display + std::error::Error` impls, `From<AsError>` (renders via the ariadne path
  `diagnostics` already exposes for strings — grep `report` for the capture-rendering helper;
  if only stderr-printing exists, add a `render_to_string` sibling, used by DAP/tests too).
- [ ] **Step 4:** `src/embed/mod.rs` — `IsolateBuilder` (fields: `caps: CapSet` default
  `deny_all` per §7, `stdlib: StdlibFilter::Full`, `output: OutputMode::Inherit`,
  `args: Vec<String>`, `host_modules`, `host_factories`) and `Isolate { vm: Rc<Vm>,
  rt: tokio::runtime::Runtime, session_src: RefCell<String>, output: OutputMode }`. `build()`:
  `Interp::new()`/`new_live()` per output mode → `set_caps` → `set_cli_args` → `install_self` →
  `Vm::new` → owned `new_current_thread().enable_all()` runtime. `Caps` wrapper:
  `deny_all()` (CapSet with all five denied — compose via `deny_all_dangerous`), `all_granted()`,
  `granting(&[Cap])`.
- [ ] **Step 5:** `cargo test --test embed` green; `cargo build --no-default-features` green
  (embed off compiles). `cargo clippy --all-targets` + `--no-default-features --all-targets`
  clean. Commit — `feat(embed): the embed feature + Isolate builder skeleton + EmbedError (§3)`.

### Task 1.2: blocking `eval` (the REPL substrate, lifted) + nested-runtime detection

**Files:** `src/embed/mod.rs`, `tests/embed.rs`.

- [ ] **Step 1 (failing tests):**

```rust
#[test]
fn eval_trailing_expression_and_session_persistence() {
    let iso = Isolate::builder().build().unwrap();
    assert!(iso.eval("let x = 2").unwrap().is_nil());
    assert_eq!(iso.eval("x + 1").unwrap().as_int(), Some(3));     // session persists
}

#[test]
fn eval_panic_survives_session_and_compile_error_mutates_nothing() {
    let iso = Isolate::builder().build().unwrap();
    iso.eval("let a = 1").unwrap();
    let e = iso.eval("nosuch()").unwrap_err();
    assert!(matches!(e, EmbedError::Panic(_)));
    let e = iso.eval("let oops = ").unwrap_err();                 // compile error
    assert!(matches!(e, EmbedError::Compile(_)));
    assert_eq!(iso.eval("a").unwrap().as_int(), Some(1));         // session intact
}

#[test]
fn eval_exit_is_typed_and_isolate_survives() { /* exit(3) → EmbedError::Exit(3); then eval("1") works */ }

#[tokio::test]
async fn blocking_eval_inside_runtime_is_a_typed_error() {
    let iso = Isolate::builder().build().unwrap();
    assert!(matches!(iso.eval("1").unwrap_err(), EmbedError::NestedRuntime));
}
```

- [ ] **Step 2:** Implement `eval` as `eval_line_vm` minus readline (spec §3.3 steps 1–5
  exactly): `Handle::try_current()` guard FIRST → `compile_source` → session-src accumulate +
  `set_worker_source` → `FnProto`/`Closure`/`Fiber` (copy the exact proto literal `repl.rs:229`
  uses) → fresh `LocalSet`, `local.block_on(&self.rt, telemetry_root_scope(vm.run(&mut fiber)))`
  → drain (`local.block_on(&self.rt, local)` — verify the drain idiom compiles outside an
  ambient runtime; if not, run `run_until` + awaiting the LocalSet inside ONE `block_on` future,
  the `lib.rs:651-655` shape) → outcome mapping (Done/Panic/Propagate/Exit per §3.3.4).
- [ ] **Step 3:** `take_output()` (Capture mode → `interp.output()` drained; document Inherit →
  empty). `Drop for Isolate` → `gc::collect()`.
- [ ] **Step 4:** AsValue doesn't exist yet — have `eval` return the crate-internal `Value`
  behind a thin pre-AsValue struct in this task, OR sequence Task 2.1's minimal `AsValue`
  scalars first if cleaner; either way the tests above compile by the end of the task
  (implementer's choice, reviewer checks no placeholder `todo!`).
- [ ] **Step 5:** green both configs + clippy. Commit —
  `feat(embed): blocking eval on a persistent Vm (REPL substrate) + NestedRuntime guard (§3.3, §4.1)`.

### Task 1.3: `call` / `call_value` / globals / `load_archive` + async variants

**Files:** `src/embed/mod.rs`, `tests/embed.rs`.

- [ ] **Step 1 (failing tests):** `call` happy (defined `fn add(a,b)` → call with ints → 3);
  `call` on `async fn` auto-awaits (§3.3); `EmbedError::Undefined` for a missing name; calling a
  non-callable global maps the engine's Tier-2 "value is not callable"; `global`/`set_global`
  (set then read from script; `set_global` over a `const`-defined name follows the engine's
  redeclare/immutability semantics — assert the actual behavior, document it); `call_value` on a
  function handle read out of `global()`; `load_archive` happy (bytes from
  `compile_archive`-built fixture) + corrupt bytes → `EmbedError::Archive` carrying the
  verifier's message; async variants: `eval_async`/`call_async` awaited under
  `LocalSet::run_until` on a current-thread runtime (the §4.2 b1 config) and under
  `LocalSet::block_on` of a multi-thread runtime from the test thread (b2).
- [ ] **Step 2:** Implement: `call` = guard → `vm.user_global(name)` → per-call LocalSet →
  `vm.call_value(...)` → if `Value::Future(f)` → `f.get().await` (grep the await helper the VM's
  Await op uses — `run.rs:4456` shape) → map. `set_global` → expose a `pub` define hook on `Vm`
  delegating to `define_user_global` (mutable=true) — keep it minimal and documented as the
  embed hook. `load_archive` = `ModuleArchive::decode` + the `run_archive` body (`lib.rs:2416`)
  refactored to run against the ISOLATE's persistent `Vm`/`Interp` instead of fresh ones —
  factor a shared helper rather than copying (DRY; `run_archive` keeps its behavior, proven by
  the existing archive tests staying green).
- [ ] **Step 3:** The async variants share one internal `async fn eval_inner` with the blocking
  wrappers (`block_on(eval_inner)`) — no duplicated logic.
- [ ] **Step 4:** green both configs + clippy. Commit —
  `feat(embed): call/globals/load_archive + !Send async variants (§3, §4.2)`.

### Task 1.4: Phase 1 review

- [ ] Independent reviewer: run every test; probe edges — eval that spawns-and-detaches
  (`task.spawn` then return: does the drain hang? assert the documented structured-drain
  behavior with a bounded test using a completing task); two isolates on one thread (state
  isolation); an isolate on a non-main spawned thread (works — affinity is per-construction);
  `Isolate` dropped mid-session with live Future handles (no leak/abort, gc::collect clean);
  re-verify `NestedRuntime` from a multi-thread `#[tokio::test(flavor = "multi_thread")]`.
- [ ] Holistic phase review subagent: facade-only diff (no engine files touched yet), no
  `unwrap()` on reachable paths, rustdoc on every pub item.

---

## Phase 2 — Unit B: `AsValue` + the kind table + the JSON bridge

### Task 2.1: `AsValue` scalars + constructors + accessors

**Files:** `src/embed/value.rs`, `tests/embed.rs`.

- [ ] **Step 1 (failing tests):** scalar round-trips (`from(7i64)` into script via `set_global`,
  script `x * 2` back, `as_int`); `as_str` borrow; float/bool/nil; `Decimal` lossless via
  `decimal("1.50")` → script `d + 0.25m`-style op → display string back; `AsKind` exhaustive
  over scalars; `static_assertions::assert_not_impl_any!(AsValue: Send, Sync)`.
- [ ] **Step 2:** Implement `AsValue(pub(crate) Value)`, `AsKind`, `type_name()` (delegate to the
  engine's `type_name` — single source of truth), constructors + accessors per spec §5.2 list.
- [ ] **Step 3:** green both configs. Commit — `feat(embed): AsValue scalars + kinds (§5.1-5.2)`.

### Task 2.2: container handles (aliasing semantics) + the 25-kind table test

**Files:** `src/embed/value.rs`, `tests/embed.rs`.

- [ ] **Step 1 (failing tests):**

```rust
#[test]
fn containers_are_live_aliasing_handles() {
    let iso = Isolate::builder().build().unwrap();
    iso.eval("let state = { hp: 10 }").unwrap();
    let state = iso.global("state").unwrap();
    state.set_key("hp", AsValue::from(7)).unwrap();              // host write...
    assert_eq!(iso.eval("state.hp").unwrap().as_int(), Some(7)); // ...script sees it
    iso.eval("state.hp = 3").unwrap();                            // script write...
    assert_eq!(state.get_key("hp").unwrap().as_int(), Some(3));  // ...host sees it
}

/// Spec §5.2: EVERY runtime kind crosses (value / live handle / callable / opaque) —
/// one row per Value variant, produced in script, classified + round-tripped.
#[test]
fn kind_table_every_value_kind_crosses() { /* table-driven over §5.2's rows, incl.
    Future (auto-await via call), Generator/Native/Class/Enum/Interface/Regex/Shared
    as pass-back-identical opaques (script-side `g == g2` identity assertions). */ }
```

- [ ] **Step 2:** Implement `len/get/get_key/set/set_key/items/entries/is_callable` over
  Array/Object/Map/Set/Bytes (Map/Set read-only per §5.2 — constructing them host-side is
  script's job; `set` on a frozen `Shared` receiver surfaces the engine's `cannot mutate a
  frozen …` panic as `EmbedError::Panic`, test it). Mutators route through the SAME engine paths
  (`index_set`/member-write helpers) so type/frozen checks aren't bypassed — grep for the
  `Interp` helpers the stdlib uses rather than poking cells directly.
- [ ] **Step 3:** green both configs. Commit —
  `feat(embed): container handles + the full kind-table crossing test (§5.2)`.

### Task 2.3: the explicit deep bridge (`to_json` / `json_parse` / serde)

**Files:** `src/embed/value.rs`, `tests/embed.rs`.

- [ ] **Step 1 (failing tests):** object → `to_json` string; `json_parse` → handle → script
  reads it; non-serializable (a function) → typed error with field path; under
  `--no-default-features` + `embed` only: `to_json` returns the documented `Config` error
  (cfg-gated test).
- [ ] **Step 2:** Route through `std/json`'s existing serializer/parser fns (grep
  `json::to_json` / `parse` entry signatures in `src/stdlib/json.rs`; call the Rust fns
  directly, not the script-level dispatch). `#[cfg(feature = "data")]` arms with the typed
  fallback otherwise. Serde bridge: `impl serde::Serialize for AsValue` via the JSON model
  (`#[cfg(all(feature = "embed", feature = "data"))]`).
- [ ] **Step 3:** green both configs. Commit — `feat(embed): explicit JSON/serde deep bridge (§5.3)`.

### Task 2.4: Phase 2 review

- [ ] Independent reviewer: run the kind table against the spec table row-by-row; probe: deeply
  cyclic object via handle `to_json` (must error or terminate — match `to_json_lossy`'s
  documented cycle behavior, never hang); huge-array `items()` snapshot cost documented;
  Bytes round-trip; `MapKey` canonicalization not violated by any host write path.
- [ ] Holistic phase review.

---

## Phase 3 — Unit C: host modules (the only core-runtime touches)

### Task 3.1: registry on `Interp` + registration API + name validation

**Files:** `src/interp.rs` (fields + accessors), `src/embed/host.rs`, `tests/embed.rs`.

- [ ] **Step 1 (failing tests):** `host_module` registration with a bad name (`"app"` — missing
  prefix; `"host:My.App"` — dot; `"host:"` — empty) → `EmbedError::Config`; duplicate module →
  `Config`; valid registration builds.
- [ ] **Step 2:** `Interp` gains `host_modules: RefCell<HashMap<Rc<str>, Rc<HostModuleDef>>>`
  (`HostModuleDef { values: Vec<(String, Value)>, fns: HashMap<String, HostFnEntry> }`,
  `HostFnEntry { f: Rc<dyn Fn(&mut HostCtx, &[AsValue]) -> Result<AsValue, HostError>>,
  fallible: bool }`). CORE field (not feature-gated — it must exist for the dispatch arm to
  compile under `--no-default-features`; it is just always empty there). `HostCtx` v1: span +
  an output-push hook (`interp.push_output` via a callback — no `Interp` exposure).
  `HostError`/`HostModuleBuilder` per spec §6.2 verbatim. Name regex per §6.1 (hand-rolled
  matcher, no regex dep in core).
- [ ] **Step 3:** green both configs (incl. `--no-default-features` — the field + types compile
  without `embed`; only the builder surface is embed-gated). Commit —
  `feat(embed): host-module registry on Interp + registration/validation (§6.2)`.

### Task 3.2: `host:` specifier + import on BOTH engines + dispatch fall-through

**Files:** `src/interp.rs`, `src/stdlib/mod.rs`, `src/vm/run.rs` (if its import match is
separate — verify), `tests/embed.rs`, `tests/embed_negative_space.rs`.

- [ ] **Step 1 (failing tests; flip the Phase-0 classification pin):**

```rust
#[test]
fn host_module_import_and_call_both_tiers() {
    let iso = Isolate::builder()
        .host_module("host:app", |m| {
            m.value("version", AsValue::from("1.0"));
            m.func("double", |_c, a| Ok(AsValue::from(a[0].as_int().unwrap_or(0) * 2)));
            m.func("boom", |_c, _a| Err(HostError::Panic("bad call".into())));
            m.fallible_func("lookup", |_c, a| match a[0].as_str() {
                Some("k") => Ok(AsValue::from(42)),
                _ => Err(HostError::Recoverable("no such key".into())),
            });
        }).unwrap()
        .build().unwrap();
    let out = iso.eval(r#"
        import * as app from "host:app"
        print(app.version, app.double(21))
        let [v, e1] = app.lookup("k");  let [n, e2] = app.lookup("x")
        let [r, e3] = recover(() => app.boom())
        print(v, e1 == nil, n, e2.message, e3.message)
    "#);
    // capture-mode output assertions: "1.0 42", "42 true nil no such key bad call"
}

#[test]
fn unregistered_host_module_is_a_clean_recoverable_panic() { /* import "host:nope" → Panic
    with the EXACT §6.3 message; recover()-able when probed via recover in script */ }
```

- [ ] **Step 2:** `classify_specifier`: `if let Some(name) = source.strip_prefix("host:")` →
  `SpecifierKind::Host(...)` FIRST (before `std/`? order irrelevant for correctness — `std/`
  can't start with `host:` — put it after `std/` to keep the common path first; comment why).
  Add `load_host_module` (mirror `load_std_module` — env child, values bound directly, fns
  bound as `Value::Builtin(format!("{module}.{fname}"))`, memoized under
  `PathBuf::from(format!("<host>/{name}"))`). Wire the tree-walker import arm
  (`interp.rs:3430` match) and the VM's import path (follow `import_std`'s caller in
  `vm/run.rs` — add the `Host` arm beside it).
- [ ] **Step 3:** dispatch — in `src/stdlib/mod.rs`'s `call` terminal match, the EXISTING
  fall-through/unknown arm gains: `m if m.starts_with("host:") => interp.call_host_fn(m, func,
  args, span).await` (clone the `Rc<dyn Fn>` out of the borrow BEFORE invoking — the standing
  borrow rule; the host fn itself is sync, no await under a borrow). `call_host_fn` maps
  `Ok`/`Recoverable`/`Panic` per the §6.2 tier table (`make_pair` for fallible — grep its
  location).
- [ ] **Step 4 (engine parity test):** one test runs the SAME host-module program on the
  tree-walker (construct `Interp`, register via the crate-internal API, `exec`) and via
  `Isolate` (VM), asserting byte-identical captured output including the miss-panic message.
- [ ] **Step 5:** assert zero hot-path cost structurally: a unit test greps... no — the reviewer
  verifies by READING that the prefix test sits on the previously-error arm only; the Gate-12
  bench in Phase 6 is the measured proof.
- [ ] **Step 6:** green both configs + clippy. Commit —
  `feat(embed): host: imports + dispatch on both engines, FFI-mirrored tiering (§6.1-6.3)`.

### Task 3.3: worker rules — the miss panic + `host_module_factory`

**Files:** `src/worker/isolate.rs`, `src/worker/mod.rs`, `src/worker/pool.rs`,
`src/embed/host.rs`, `tests/embed.rs`.

- [ ] **Step 1 (failing tests):** (a) a `worker fn` whose body imports `host:app` (registered
  main-isolate only) → the §6.4 worker-specific panic message; (b) with
  `host_module_factory("host:app", Arc::new(|m| { m.func("double", ..) }))` the same program
  succeeds in BOTH a pooled `worker fn` call and a dedicated `run_in_worker` call; (c) the
  pooled no-leak test: Isolate A (factory) and Isolate B (none) dispatching on the same host
  thread's pool — B's worker still misses (per-request install, the caps-floor discipline).
- [ ] **Step 2:** `HostModuleFactory = (Rc<str> name, Arc<dyn Fn(&mut HostModuleBuilder) + Send
  + Sync>)`. Carry: `WorkerRequest` gains `host_factories: Vec<(String, HostFactoryFn)>`
  (plain `Send` field beside `caps` — NOT serialized, NO wire tag — mirror exactly how `caps`
  rides per FFI §4.5a/§6); installed fresh at the top of each pooled request; dedicated spawns
  capture the list in the `Send` `make_loop` closure. The worker-side miss message switches on
  "am I a worker isolate" — reuse however the isolate marks itself (grep `is_worker`/worker
  marker on `Interp`; if none exists, the install path sets a flag — minimal, documented).
- [ ] **Step 3:** green both configs (worker tests run under default features; the carry fields
  compile under `--no-default-features`). Commit —
  `feat(embed): worker host-module rules — miss panic + Send factories riding the caps path (§6.4)`.

### Task 3.4: caps default + `StdlibFilter` + checker `host:` skip

**Files:** `src/embed/mod.rs`, `src/interp.rs`, `src/check/` (unresolved-import rule),
`tests/embed.rs`.

- [ ] **Step 1 (failing tests):** deny-all default (`fs.read` on a default-built isolate →
  `capability 'fs' denied` panic; same program under `.caps(Caps::all_granted())` succeeds —
  tempdir); `granting(&[Cap::Fs])`; in-script `caps.drop` remains monotone post-grant;
  `StdlibFilter::Allow(&["std/math"])` → `import "std/json"` → the §6.5 availability message,
  `import "std/math"` works; `Core` excludes every `required_cap`-mapped module + `std/ffi`
  (table-driven against `required_cap` itself — drift-proof); `ascript check` on a file with
  `import "host:app"` emits NO `unresolved-import` (checker test).
- [ ] **Step 2:** Implement `Caps` (wrap `CapSet`; `deny_all` composes the five denials via the
  existing `deny`/`deny_all_dangerous`), the `stdlib_filter: Option<...>` field on `Interp`
  checked in `load_std_module` only (one chokepoint; comment: availability knob, NOT a security
  boundary — §6.5), and the checker skip (one arm in the unresolved-import rule:
  specifier starts with `host:` → resolved-by-host, skip).
- [ ] **Step 3:** green both configs. Commit —
  `feat(embed): deny-all default caps + StdlibFilter + checker host: skip (§6.5, §7)`.

### Task 3.5: Phase 3 holistic review (the security-shaped phase)

- [ ] Independent reviewer, explicitly adversarial: (1) can a worker smuggle a
  `Builtin("host:app.f")` value and reach a host fn it shouldn't? (assert: registry miss in the
  worker's Interp → the clean panic, test it); (2) does any host-fn invocation hold a `RefCell`
  borrow across the call? (read the code; host fns can call back into nothing, but they CAN
  allocate `AsValue`s — fine); (3) `required_cap` catch-all really returns `None` for
  `host:`-strings and the cap-completeness test still passes; (4) `vm_differential.rs` full run
  both configs — must be 100% unchanged; (5) the §6.3 LOUD docs exist on `func`/`fallible_func`
  (host fns bypass caps).
- [ ] Holistic phase review of Phases 1–3 combined.

---

## Phase 4 — Unit D: the `capi/` crate

### Task 4.1: crate scaffold + isolate/eval/value scalars + panic safety + thread affinity

**Files:** `capi/Cargo.toml`, `capi/src/lib.rs`, `capi/include/ascript.h`.

- [ ] **Step 1:** `capi/Cargo.toml`: name `ascript-capi`, `crate-type = ["cdylib",
  "staticlib"]`, `ascript = { path = "..", features = ["embed"] }`, own empty `[workspace]`
  (the tree-sitter-ascript precedent), dev-deps `cc`, `tempfile`.
- [ ] **Step 2 (failing tests, Rust-side first):** in `capi/src/lib.rs` `#[cfg(test)]`:
  `as_isolate_new` → `as_eval("1 + 2")` → `as_value_int` == 3 → frees; eval error →
  `AS_ERR_PANIC` + `as_last_error` non-empty; wrong-thread: spawn a thread, call `as_eval` on
  the main thread's isolate → `AS_ERR_WRONG_THREAD`; wrong-thread `as_value_free` → leak +
  error (assert no crash; the leak is the documented contract); poisoning: an injected
  internal panic (a `#[cfg(test)]`-only `as__test_panic` entry) → `AS_ERR_INTERNAL` then
  `AS_ERR_POISONED` on the next call, `as_isolate_free` still works.
- [ ] **Step 3:** Implement per spec §8.2: `CIsolate { iso: Isolate, thread: ThreadId,
  last_error: RefCell<CString-ish>, poisoned: Cell<bool> }`; every `extern "C"` body =
  thread check → poison check → `catch_unwind(AssertUnwindSafe(..))` → status map. `as_value`
  = `Box<CValue { thread: ThreadId, v: AsValue }>`. NULL-pointer args → `AS_ERR_CONFIG` (checked,
  never deref'd). Invalid UTF-8 → `AS_ERR_UTF8`.
- [ ] **Step 4:** `ascript.h` hand-written covering exactly what's implemented (the spec §8.2
  excerpt, completed). `cargo test --manifest-path capi/Cargo.toml` green;
  `cargo clippy --manifest-path capi/Cargo.toml --all-targets` clean.
  Commit — `feat(capi): ascript-capi crate — panic-safe, thread-checked core (§8.1-8.2)`.

### Task 4.2: host-fn registration + JSON bridge + remaining surface

- [ ] **Step 1 (failing tests):** register a C-callback host fn (a Rust `extern "C"` fn in the
  test acting as the C side, userdata round-trip) → script calls it; callback returns error
  status + message → tier mapping (tier 0 → Tier-2 panic; tier 1 → `[nil, err]`);
  `as_value_to_json`/`as_json_parse` round-trip; `as_take_output`.
- [ ] **Step 2:** Implement `as_register_host_fn` (wrap the C fn pointer + userdata in a
  closure; userdata is a raw pointer the HOST promises is thread-affine — documented in the
  header; the closure is `!Send` so the promise is structural on the Rust side). NOTE:
  registration is on the isolate post-construction — add the crate-internal hook
  `Isolate::register_host_module_late` (embed-internal, `#[doc(hidden)]` or pub-in-embed as
  `register_host_module`; decide with the reviewer: late registration is useful in Rust too —
  if exposed, document that an already-imported module is memoized and late fns won't appear in
  it; simplest honest rule: late registration REPLACES the registry entry but a memoized
  `ModuleEntry` is not retro-patched → registration before first import, error after — pick,
  test, document).
- [ ] **Step 3:** green. Commit — `feat(capi): host fns + json bridge + output (§8.2)`.

### Task 4.3: header drift test + C smoke test

**Files:** `capi/tests/header_drift.rs`, `capi/tests/c_smoke.rs`, `capi/tests/smoke.c`.

- [ ] **Step 1 (failing-first):** drift test — extract `ascript_*`/`as_*` decl names from
  `include/ascript.h` (line regex) and from `src/lib.rs` (`#[no_mangle]\s*pub extern "C" fn
  (\w+)` scan of the source file at test time via `include_str!`), assert set equality with a
  diff message. Temporarily remove one decl to watch it fail, restore.
- [ ] **Step 2:** smoke test — `smoke.c` exercises: version/ABI guard, new/eval/call, host fn
  with userdata, error + last_error, json bridge, frees; `c_smoke.rs` compiles it with the
  system compiler via `cc::Build` (test-time; `#[cfg(unix)]` per the §8.3 owner-noted Windows
  deferral), links against the cdylib found via
  `env!("CARGO_MANIFEST_DIR")`-relative `target` probing (handle both
  `target/debug` and workspace-style paths; fail with a clear message if the cdylib isn't
  built — `cargo test` builds it first by dependency, verify), runs it, asserts exit 0 and
  expected stdout.
- [ ] **Step 3:** green: `cargo test --manifest-path capi/Cargo.toml`. Commit —
  `test(capi): header drift guard + compiled C smoke test (§8.3)`.

### Task 4.4: Phase 4 review

- [ ] Independent reviewer: run the smoke test; hand-write a 10-line C misuse program (double
  free is documented-undefined — skip; NULL everything, wrong-thread everything, read-int-from-
  string) and verify status codes; `nm`/`objdump` the cdylib for the exported symbol set ==
  header; valgrind/leaks the smoke binary if available (no leaks beyond the documented
  wrong-thread case); confirm root `cargo build` is byte-identically unaffected (capi not in
  the root graph).
- [ ] Holistic phase review.

---

## Phase 5 — Unit E: examples

### Task 5.1: `examples/embed/rust-host` (cargo example) + `examples/embed/c-host`

**Files:** `Cargo.toml` (`[[example]]`), `examples/embed/rust-host/{main.rs,game.as}`,
`examples/embed/c-host/{main.c,Makefile,plugin.as}`, `tests/embed.rs` (runner test).

- [ ] **Step 1:** `[[example]] name = "embed-rust-host", path = "examples/embed/rust-host/main.rs",
  required-features = ["embed"]`. The host (spec §12): deny-all caps + capture output;
  `host:game` module (`log` func, `rand_seeded` fallible func); loads `game.as`
  (`fn on_tick(n)` returning a state delta, an `async fn on_save` exercising auto-await, a
  `recover`-handled host panic, a `caps`-denial probe — happy AND edge per Gate 9); 5-tick
  game loop printing state read via container handles. Exit non-zero on any assertion failure
  (the example IS its own check).
- [ ] **Step 2 (test):** an integration test (`#[ignore]`-free, but marked `// slow` and placed
  in `tests/embed.rs::examples`) runs `cargo build --example embed-rust-host --features embed`
  (incremental — cheap after first build) then executes
  `target/debug/examples/embed-rust-host`, asserting exit 0 + a stdout sentinel line.
- [ ] **Step 3:** c-host: `main.c` (the §12 surface; ~120 lines, fully error-checked — every
  status tested, C-side `die()` on mismatch) + `Makefile` (`CFLAGS += -I../../../capi/include`,
  links `libascript_capi`). The capi smoke test (Task 4.3) ALREADY proves compilation; add a
  capi test that compiles & runs `examples/embed/c-host/main.c` the same way (the Makefile is
  for humans, the test is the CI truth — both kept in sync by using the same source file).
- [ ] **Step 4:** `game.as`/`plugin.as` are valid standalone where possible; they import
  `host:` modules so `ascript run` rejects them at the miss panic — assert THAT exact behavior
  in a CLI test (the corpus discovery is non-recursive over `examples/embed/`, verified — they
  are NOT corpus members; the negative-space test asserts discovery still excludes them).
- [ ] **Step 5:** green both configs + capi. Commit —
  `feat(embed): rust-host + c-host examples, CI-executed (§12, Gate 9)`.

### Task 5.2: Phase 5 review

- [ ] Independent reviewer: run both examples by hand (`cargo run --example embed-rust-host`;
  `make -C examples/embed/c-host` with the cdylib prebuilt); confirm the examples demonstrate
  edge cases (denial, panic recovery, fallible tier) not just happy path; confirm README-ready
  output.

---

## Phase 6 — Unit F: docs, stability sweep, bench, finish

### Task 6.1: docs (`embedding.md` + NAV + README) + rustdoc contract

- [ ] **Step 1:** `docs/content/embedding.md` per spec §12 (model headline; builder; the §4
  threading table — supported b1/b2, rejected c with the dedicated-thread pattern shown;
  deep-recursion reality §4.3; caps inversion WARNING box; host modules + tiering + worker
  factory; the §5.2 kind table; stdlib filter ≠ security; C API section w/ thread-affinity +
  ownership rules; stability policy). Add `['embedding', 'Embedding (Rust & C)']` to `NAV`
  (Introduction section, after `runtime`) — the orphan-page gotcha. Verify served
  (`cd docs && python3 -m http.server`, load the page, check sidebar + cmd-K).
- [ ] **Step 2:** `README.md` "Embedding" section (Rust + C snippets, link to docs).
- [ ] **Step 3:** `src/lib.rs` root-doc Stability section + the §9 sweep: audit every root-level
  `pub fn` — already-hidden stay hidden; `run_source`/`run_file`-class entries gain the
  "CLI entry, not the embedding contract" doc pointer; `run_on_worker_stack` gains
  `#[doc(hidden)]` IF nothing external needs it (grep tests first — if tests use it, keep
  visible with the internal-seam doc note; never break a consumer to satisfy a doc sweep).
  Rustdoc on `ascript::embed` carries the semver-contract section verbatim from spec §9.
- [ ] **Step 4:** `cargo doc --no-deps --features embed` builds warning-free; the embed module
  is the visible API story. Commit — `docs(embed): embedding.md + NAV + README + stability contract (§9, §12)`.

### Task 6.2: negative space, Gate-12 bench A/B, RSS

- [ ] **Step 1:** finalize `tests/embed_negative_space.rs`: ASO pin (27); opcode-count pin
  (read the `Op` count via the existing disassembler/opcode table export — grep how
  `vm_limits`/fuzz tests count ops; pin the number); corpus-discovery exclusion pin
  (`examples/embed/**` not in the differential corpus); `Value` variant-count pin if a cheap
  introspection exists (else omit — never add runtime reflection for a test).
- [ ] **Step 2:** full `tests/vm_differential.rs` BOTH configs — must pass with zero corpus
  changes (Gate 1 proof: EMBED ran no engine change).
- [ ] **Step 3:** Gate-12 same-session A/B: `tests/vm_bench.rs` (or the bench harness's
  documented runner) baseline at the merge-base vs branch HEAD in ONE session; spec/tw geomean
  ≥2× holds and branch≈baseline (expected ≈1.0×). Peak RSS over the corpus via
  `/usr/bin/time -l` recorded. Write `bench/EMBED_RESULTS.md` (numbers, machine, command lines).
- [ ] **Step 4:** Commit — `test+bench(embed): negative-space pins + Gate-12 A/B + RSS (§11)`.

### Task 6.3: cross-cutting docs-of-record + final gates checklist

- [ ] **Step 1:** Update `CLAUDE.md` (an EMBED subsection under the campaign-work list: the
  facade, deny-all default, `host:` namespace, the two cold-path core touches, capi crate
  layout, the wrong-thread-free leak rule), `superpowers/roadmap.md`, `goal-perf.md` (status
  flip on the EMBED row).
- [ ] **Step 2:** Full final verification — every box re-run, evidence pasted into the PR/branch
  notes:
  - [ ] `cargo test` green (all binaries)
  - [ ] `cargo test --no-default-features` green
  - [ ] `cargo test --manifest-path capi/Cargo.toml` green (drift + C smoke)
  - [ ] `cargo clippy --all-targets` AND `--no-default-features --all-targets` clean
  - [ ] `cargo clippy --manifest-path capi/Cargo.toml --all-targets` clean
  - [ ] `tests/vm_differential.rs` both configs, zero changes (Gate 1)
  - [ ] Gate 5: `examples/**` zero `type-*` diagnostics both configs (untouched corpus — re-run)
  - [ ] Gate 12: bench A/B ≈1.0×, geomean floor ≥2× holds; RSS recorded (Gate 18)
  - [ ] Gates 9/10: examples happy+edge run in CI; every API fn unit-tested happy+edge
  - [ ] Gate 13: docs page served + NAV + README; rustdoc clean
  - [ ] No `unwrap()/expect()/todo!()/unreachable!()` reachable from any embed/capi input
        (reviewer greps the new code)
- [ ] **Step 3:** Commit — `docs(embed): CLAUDE/roadmap/goal-perf — EMBED complete`.

### Task 6.4: final holistic review + merge

- [ ] **Holistic review subagent** over the whole branch: re-probe the §11 matrix end to end;
  hunt cross-subsystem leaks (the FFI-holistic lesson: look where per-unit reviews structurally
  can't — e.g. does `load_archive` on an isolate with a `StdlibFilter` still filter imports
  from archive modules? does a host-module `value(...)` holding a container leak across
  `Isolate` drop? does poisoning interact with `as_take_output`?); run both examples; verdict
  recorded.
- [ ] Address every finding in-branch (failing test first), re-run the Task 6.3 checklist.
- [ ] Merge `feat/embedding-api` → `main` with `--no-ff`; update goal-perf status to ✅.
