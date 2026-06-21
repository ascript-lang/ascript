# AScript wasm32 Target + Browser Playground — Design (WASM)

- **Status:** Implemented — MERGED to `main` (`--no-ff`, `2120476d`). The Phase-0 feasibility
  spike (the gate) returned **GO** (evidence on `spike/wasm-target` `ae9d0f99`, kept unmerged;
  §10 has the matrix); Phases 1-3 were then re-done TDD off `main`, native byte-identical (Gate
  W-1, proven whole-branch in both feature configs). **Deltas from this draft, all owner-noted:**
  (1) the spike recorded the gcmodule wasm32 issue as a "debug-only `debug_assert`" caveat, but
  Phase 2 found it was a REAL heap-corruption bug (a `GcHeader` 3-pointer alignment defect) and
  fixed it by VENDORING gcmodule with `#[repr(C, align(8))]` (a proven no-op on 64-bit native);
  (2) `stacker` needed NO wasm patch (compiles + degrades `maybe_grow` to run-on-current-stack);
  (3) the Run▶-on-docs-code-blocks stretch (§5.5/Task 3.3) was DEFERRED to v2 (roadmap-noted).
  The original spike-gate posture (a NO-GO would have been an honored outcome, the
  `superpowers/specs/2026-06-08-baseline-jit-design.md` §0 stance) is preserved in §0/§4.6.
- **Date:** 2026-06-12
- **Code:** WASM (the "Deployment & reach track" item of `goal-perf.md`)
- **Depends on:** nothing in the engine waves (independent of LANE/CALL/SHAPE/NANB/DECODE).
  Composes shipped subsystems only: `OutputSink::Capture`, the `std/caps` deny model, the
  `det.rs` determinism seams, the feature-gated stdlib graph.
- **Depended on by:** nothing. v2 (WASI/edge) is sketched in §8 with its own gate.
- **Engines:** **no new engine, no new differential mode.** wasm32 is a *build target* of the
  existing CST → resolver → compiler → VM pipeline. The four-mode differential
  (tree-walker == specialized-VM == generic-VM == `.aso`) is **not** extended to wasm in v1
  (recorded non-goal, §7.1); instead a **mini-differential CI smoke** compares the wasm
  playground's captured output against native output over a curated pure-compute example
  corpus (§5.6).
- **Breaking:** **no.** No grammar change, no opcode change, no `.aso` change
  (`ASO_FORMAT_VERSION` untouched), no surface-semantics change on native. Every main-crate
  change is `cfg(target_family = "wasm")`-shaped or a target-gated dependency: the native
  build's dependency graph, codegen, and behavior are byte-identical (§5.3, Gate W-1).

---

## 0. Read this first — the gate is the design

This spec exists to capture the design *and its feasibility risks* before code is written, so
the go/no-go is decided against evidence, not optimism. Three things are true up front:

1. **Phase 0 is the deliverable until it says otherwise.** The spike (§4) is a fixed command
   matrix — dependency builds, the tokio-on-wasm executor question, tree-sitter linkage,
   `gcmodule`, binary size — each cell with both outcomes specified. The result is committed
   to §10 verbatim (failing compiler output included on NO-GO), and `goal-perf.md`'s WASM
   entry is updated either way.
2. **GO means "playground-viable", not "everything works".** The GO bar (§4.6) is: the
   curated-feature lib compiles to `wasm32-unknown-unknown`; a hello-world + an async example
   run under Node with captured output matching native; size after `wasm-opt -Oz` + gzip is
   *recorded* (a budget is measured, never promised — §5.7). Workers, timers-v1 fallbacks,
   and stdlib breadth are design decisions, not gate criteria.
3. **Nothing is silently dropped.** Every platform gap on wasm is either compiled out by the
   existing feature graph (an `import "std/fs"` is the *existing* unknown-module error) or a
   **clean Tier-2 panic** `"<thing> is not available on this platform (wasm)"` (§5.4) —
   never a hang, never a wrong answer, never a stub. This is goal.md Gate 6 applied to a
   platform port.

## 1. Summary & motivation

**v1 goal:** an in-browser playground on the existing docs site (`docs/`) that compiles and
runs AScript **entirely client-side** — the full front-end (CST parser → resolver → bytecode
compiler) and the async VM, built for `wasm32-unknown-unknown`, wrapped by a thin
`wasm-bindgen` crate exporting `run(source) → Promise<{ok, output, error, diagnostics}>`.

Why this is cheap relative to its reach:

- **The runtime model already fits the platform.** The interpreter is `!Send`,
  single-threaded, `Rc`/`RefCell`, driven by a current-thread executor (`CLAUDE.md`,
  "The interpreter") — exactly the shape a browser's single-threaded wasm instance wants.
  Parallelism-by-isolation (workers) is the only piece that assumes OS threads, and it is
  cleanly severable (§5.4).
- **Output capture already exists.** `OutputSink::Capture` (`src/interp.rs:375`;
  `Interp::new()` → `with_sink(OutputSink::Capture(..))` at `src/interp.rs:975`) is how
  every test and `run_source` already collects `print` output. The playground is another
  `Capture` consumer — zero new output machinery.
- **Sandboxing already exists.** `std/caps` is CORE, default-all-granted with
  **deny-only** mutators (`src/stdlib/caps.rs` — "there is deliberately no grant"), gated at
  the single `Interp::call_stdlib` chokepoint via `required_cap(module, func)`
  (`src/stdlib/mod.rs:325`). The playground constructs its `Interp` with all five caps
  (`fs`/`net`/`process`/`ffi`/`env`) denied: any OS-touching call that survives feature
  pruning yields the *existing, tested* capability-denied error. Defense in depth: the wasm
  build's feature set excludes the OS modules entirely (§5.2), so most such imports are the
  existing unknown-module error before caps are even consulted.
- **Nondeterminism is already seamed.** `src/det.rs` routes RNG and clock through a
  per-`Interp` determinism context when armed; the *ambient* sources underneath
  (`SystemTime::now` in `src/stdlib/time.rs:44`, the time-seeded xorshift in
  `src/stdlib/math.rs:501-535`) are the only raw OS touchpoints, and they funnel through a
  handful of functions. WASM adds a tiny platform layer *below* the det seams (§5.3.3):
  `Date.now()`/`performance.now()` for the clock, `crypto.getRandomValues` for entropy —
  the det seams themselves are untouched and Record/Replay still works in-browser.
- **The stdlib feature graph does the pruning.** Every stdlib module is `#[cfg(feature)]`-
  gated and `--no-default-features` builds the bare language (`Cargo.toml`). The wasm build
  is `default-features = false` plus a curated list of pure-compute features (§5.2) — **no
  new feature flag is required for v1**; the existing graph is the mechanism.

**What v1 is not:** a server runtime (WASI/workerd is v2, §8), a fifth differential mode
(§7.1), a multi-core target (no workers, §5.4), or a `.aso` distribution channel (the
playground compiles in-browser; `.aso` loading is recorded future work, §8).

## 2. Verified ground truth (code findings, 2026-06-12 — the spike re-verifies the dynamic ones)

Everything below was verified against the working tree at spec-writing time. Items marked
**[SPIKE]** are expectations the Phase-0 spike must confirm or refute with committed output.

### 2.1 tree-sitter linkage — compiled into the lib unconditionally, consumed only by tests

`build.rs:1-11` runs `cc::Build` over the vendored `tree-sitter-ascript/src/parser.c` for
**every** build of the crate (lib and bin, all targets, unconditionally). However, a full-tree
grep shows the exported symbol `tree_sitter_ascript` has **zero consumers under `src/`** —
the only consumer is the dev-side integration test `tests/treesitter_conformance.rs:11`
(`extern "C" { fn tree_sitter_ascript() -> tree_sitter::Language; }`), linked against the dev
`tree-sitter = "0.25"` crate. The CST front-end (`src/syntax/`) is a hand-written Rust
parser; it does not call the C parser.

**Decision:** on wasm targets `build.rs` **skips the `cc` step** (a `TARGET` env check). This
is *safe by construction* — no `src/` code references the symbol, so the lib is complete
without it — and avoids the entire "compile C to wasm32-unknown-unknown without a libc"
problem (tree-sitter's runtime C is wasm-clean upstream, but `wasm32-unknown-unknown` has no
sysroot headers for `cc` to find; targeting it from `cc` requires a wasi-sysroot detour we do
not need). The native test suite is unaffected (the cc step still runs for native targets).
The brief's alternative — `clang --target=wasm32` — is **rejected as unnecessary** (§7.2).

### 2.2 Dependency inventory for `wasm32-unknown-unknown`

Non-optional deps and their expected wasm posture (the spike's checklist, §4.1):

| Dep | Posture | Action |
|---|---|---|
| `rustyline` (non-optional; `src/repl.rs` is an **unconditional** `pub mod`, `src/lib.rs:44`) | **Blocker** — termios/native-console crate | Move to a `cfg(not(target_family = "wasm"))` target-dep table; `#[cfg]`-gate `pub mod repl` (and `main.rs` is never built for wasm — the wrapper crate is the entry) |
| `tokio` with `rt-multi-thread` | **Blocker** — tokio supports only `sync`/`macros`/`io-util`/`rt`/`time` on `wasm32-unknown-unknown`; `rt-multi-thread` does not build there. **Code finding: `rt-multi-thread` is enabled in `Cargo.toml:32` but a grep for `multi_thread` finds ZERO uses in `src/`** — `main.rs:480` builds `new_current_thread`, workers build per-thread current-thread runtimes. It is a vestigial feature. | Split into target-dep tables: non-wasm keeps the current list (no behavior change, identical resolution); wasm gets `rt`, `macros`, `sync` (+ `time` per **[SPIKE]** — kept if it compiles, since core types reference it; its *runtime* use is shimmed, §5.3.4) |
| `stacker` (`src/vm/stack.rs` `grow`/`grow_future`) | **[SPIKE]** — `psm` has no wasm stack-switching; expectation: compiles with `maybe_grow` degrading to "run on current stack" (probe returns unknown). If it does not compile, target-gate the dep and `cfg` the two functions to pass-throughs | Either way: wasm cannot grow segments → recursion depth is bounded by the linker-set shadow stack. §5.3.5: raise the wasm stack via `-C link-arg=-zstack-size`, lower `MAX_CALL_DEPTH` on wasm (cfg const), keep the **same** `maximum recursion depth exceeded` message — a documented platform asymmetry, same class as SP3's VM-only capacity errors |
| `num_cpus` | OK (returns 1 on wasm) | none |
| `gcmodule` 0.3 | **[SPIKE]** expected OK — pure Rust; per-thread `thread_local!` `THREAD_OBJECT_SPACE` (`collect.rs:248`, verified in the vendored registry source); `std::thread::current()` only in debug logging. `thread_local!` works on single-threaded wasm | none expected |
| `rust_decimal`, `indexmap`, `cstree`, `clap`, `ariadne`, `toml`, `sha2`, `static_assertions`, `async-recursion` | pure Rust — expected OK | none |
| `uuid` (v4, in `data`) → `getrandom` | **Blocker on wasm without the js backend** | Target-dep `getrandom = { features = ["js"] }` + `uuid = { features = ["js"] }` for wasm (the ecosystem-standard fix); **[SPIKE]** confirms |
| `socket2`, `libloading`, `libffi`, `crossterm`, `rusqlite`, `reqwest`/`hyper`, `sysinfo`, `tower-lsp`, `icu`, postgres/redis | Never built — all optional, excluded by the wasm feature set (§5.2) | none |
| `apple-codesign` | macOS target-dep only | none |

### 2.3 The entry point and executor

- `src/main.rs:475-480`: native entry spawns a dedicated thread with
  `WORKER_STACK_SIZE` (512 MB, `src/interp.rs:698+`) and a
  `tokio::runtime::Builder::new_current_thread()` runtime + `LocalSet`. **None of this is
  reachable on wasm** — there are no threads, and a browser cannot block. The wasm entry is
  the wrapper crate (§5.1), not `main.rs` (the `[[bin]]` is simply never built for wasm).
- `tokio::task::spawn_local` is the structured-concurrency substrate — 20+ call sites
  (`src/interp.rs`, `src/task.rs`, `src/vm/run.rs`, `src/stdlib/*`), and
  `src/task.rs` `SharedFuture` owns the **`AbortHandle`** that makes cancel-on-drop real.
  **Decision (the brief's executor question): keep tokio on wasm.** Replacing
  `spawn_local`/`JoinHandle`/`abort` with `wasm_bindgen_futures::spawn_local` (which has no
  handle and no abort) would force re-deriving cancel-on-drop, `race` loser-cancellation, and
  `timeout` semantics — the riskiest invariants in the runtime. Instead, the wrapper drives
  `tokio::task::LocalSet::run_until(<program future>)` **as a plain future on the browser
  microtask loop** via `wasm-bindgen-futures` (§5.3.6). No `Runtime::block_on` ever runs on
  wasm (blocking is impossible *and unnecessary* — `run_until` provides the `spawn_local`
  context while polled). **[SPIKE]** task (b) proves this end-to-end in Node: spawn_local +
  await + JoinHandle-drop-aborts inside `run_until` driven by `wasm_bindgen_futures`,
  including whether a `Runtime`/`EnterGuard` must additionally be entered (both outcomes
  handled: if an enter guard is required, the wrapper builds a current-thread runtime it
  never blocks on and holds the guard across the run — single-threaded wasm makes this
  benign).
- `tokio::time` is used by CORE modules: `task_mod.rs:186,308` (`timeout`, `retry` backoff),
  `time_timers.rs` (`sleep`, `interval`), and `src/interp.rs:272`
  (`ResourceState::Interval(Box<tokio::time::Interval>)`). On wasm the time *driver* never
  runs (no parking), so these would hang, not error. §5.3.4 routes the sleep-shaped uses
  through a platform seam (JS `setTimeout` future on wasm) and makes interval/timer
  *resources* a clean Tier-2 platform error in v1.

### 2.4 Raw OS touchpoints below the det seams

- `src/stdlib/time.rs:8,29,44`: `SystemTime::now` + a `LazyLock<Instant>` — **both panic at
  runtime on `wasm32-unknown-unknown`** ("time not implemented on this platform").
- `src/stdlib/math.rs:501-505`: the ambient xorshift64* RNG is **"seeded once from the"
  system time** — first `math.random()` call would panic on wasm.
- `src/det.rs` (`VirtualClock`, `SeededRng`, `clock_now_ms`, `next_seeded_f64`) sits *above*
  these: when a determinism context is armed, the raw sources are never consulted. The
  platform shim (§5.3.3) therefore slots in cleanly **below** det with no seam change.

### 2.5 Workers

`src/worker/isolate.rs:273` (`spawn_isolate`) uses `std::thread::Builder::spawn` — compiles
on wasm, **fails at runtime** (wasm32-unknown-unknown has no threads). All three worker forms
(`worker fn` pools, `worker class` actors, `worker fn*` streams) plus `run_in_worker` and
PAR's `task.pmap`/`preduce` funnel through `spawn_isolate`/`dispatch_worker`
(`src/worker/mod.rs:87,342`). §5.4 puts ONE guard at this funnel.

## 3. Goals / non-goals

**Goals (v1, on GO):**
1. The `ascript` lib (curated features) builds for `wasm32-unknown-unknown` with zero impact
   on native builds (graph, codegen, behavior — Gate W-1).
2. A wrapper crate `ascript-wasm/` exporting `run(source, opts?) → Promise<RunResult>`:
   full compile (CST → resolver → compiler) + VM execution in-browser, output captured,
   caps denied-all, async programs work (eager scheduling, `await`, generators, channels,
   `task.gather`/`race`/`timeout`).
3. A minimal playground page on the existing docs site (textarea + Run/Stop + output pane +
   example picker), executed inside a **browser Web Worker** so the docs UI never jankes and
   Stop genuinely terminates runaway programs (§5.5).
4. A CI job: wasm build + `cargo clippy --target wasm32-unknown-unknown` (curated features)
   + a Node-based smoke that runs the wasm-compatible example corpus and **byte-compares**
   captured output against the native binary (§5.6) + a recorded size/load measurement.
5. Docs: a `tooling/playground.md` content page (NAV entry — the NAV-orphan rule), links
   from `index.html`/the reader topbar, and honest platform-limits documentation.

**Non-goals (v1, recorded):**
- Workers / threads / SharedArrayBuffer on wasm (§7.1) — `worker fn` is a clean Tier-2 error.
- WASI / edge runtimes (v2, §8). JIT on wasm (meaningless — no codegen target). `.aso`
  loading in the browser (§8). The full stdlib (curated subset only, §5.2). A fifth
  differential mode (§7.1; the mini-differential smoke is the v1 guard). Streaming
  (non-captured) output; live stdin; `time.every`/interval resources (Tier-2 v1, §5.3.4).

## 4. Phase 0 — the feasibility spike (the gate)

A fixed matrix, run on a spike branch with **probe patches** (minimal, throwaway-quality
allowed *in the spike branch only*; the real implementation re-does them TDD on GO). Every
cell's raw output is committed into §10. Exact commands live in the plan (Tasks 0.1–0.7);
the matrix and decision rules are normative here.

### 4.1 (a) Dependency build inventory

`rustup target add wasm32-unknown-unknown`, then:

```
cargo check --target wasm32-unknown-unknown --no-default-features
```

Expected first failure: `cc`/parser.c (no wasm sysroot). Apply probe patch 1 (build.rs TARGET
skip), re-run, and iterate, recording **every** failing dep in order (expected: tokio
`rt-multi-thread`, `rustyline`; possibly `stacker`). Then the curated set:

```
cargo check --target wasm32-unknown-unknown --no-default-features \
  --features data,binary,log,shared
```

(plus `crypto` and `datetime` as *candidate* probes — each individually, recorded
pass/fail; they are subset candidates, not gate criteria).

### 4.2 (b) tokio-on-wasm + the executor model

A throwaway `#[wasm_bindgen_test]` (run under `wasm-pack test --node`) in the spike wrapper
skeleton proving, inside `LocalSet::run_until` driven by the wasm-bindgen executor:
(1) `tokio::task::spawn_local` works and its `JoinHandle` resolves; (2) dropping a
`JoinHandle`+`abort()` cancels (the `SharedFuture` pattern); (3) `tokio::sync::mpsc`/
`oneshot` work; (4) whether an explicit `Runtime`/`EnterGuard` is required (try without
first; if `spawn_local`/sync types panic wanting a runtime context, retry with a never-blocked
`new_current_thread` runtime's `enter()` guard held). **Decision rule:** if neither
configuration can run `spawn_local`-based code to completion under Node, the executor cell is
NO-GO and the fallback design (a cfg'd `crate::rt` facade re-exporting a hand-rolled `!Send`
spawn with abort support) is *costed but not built* — that finding alone may flip the overall
GO/NO-GO since it touches 20+ call sites.

### 4.3 (c) tree-sitter linkage

Already resolved statically (§2.1): zero `src/` consumers (the grep command + output is
committed as evidence). The spike confirms the build.rs skip compiles for wasm AND that the
native test suite (`cargo test --test treesitter_conformance`) is untouched.

### 4.4 (d) gcmodule + stacker + getrandom

Covered by (a)'s curated-features check; record each crate's pass/fail individually. For
`gcmodule`, additionally run a cycle-collection smoke **at runtime** under Node (a program
that builds and drops a cyclic object graph, then asserts liveness/output) — compiling is
not the same as collecting.

### 4.5 (e) Size & load

Build the spike wrapper with `wasm-pack build --release`, then:

```
wasm-opt -Oz -o pkg/ascript_wasm_bg.opt.wasm pkg/ascript_wasm_bg.wasm
gzip -9 -k pkg/ascript_wasm_bg.opt.wasm && ls -l pkg/
```

Record raw / `-Oz` / gzip bytes and Node cold-instantiate time. **The <5 MB-gzipped figure
from the roadmap brief is a *target to measure against*, not a promise** — if measured size
exceeds it, the recorded number plus a pruning plan (e.g. dropping `regex`/`csv`/`yaml` from
the subset) goes in §10 and the GO decision weighs it; size alone is a soft criterion.

### 4.6 GO / NO-GO

**GO requires all of:**
1. The curated-feature lib + wrapper compile for `wasm32-unknown-unknown` (with only the
   sanctioned probe patches of §5.3).
2. The executor cell (§4.2) passes — spawn_local + abort + await semantics work under Node.
3. A hello-world AND an async example (`examples/async.as`-class) run under Node with
   captured output **byte-equal** to `target/release/ascript run` on native.
4. The `gcmodule` runtime smoke passes.
5. Size/load is recorded (any number — measurement, not threshold).

**NO-GO protocol:** each failing cell gets its failing output committed to §10, the blocker
named (crate + version + error), and any plausible unblock path sketched with a cost note;
`goal-perf.md`'s WASM entry is updated to NO-GO with a pointer here. The spike branch is
kept (not merged). This is an honored outcome — the JIT-spec posture.

## 5. Design (v1, executed only on GO)

### 5.1 Build shape: a thin workspace-excluded wrapper crate

**`ascript-wasm/`** at the repo root — its own `Cargo.toml` with an **empty `[workspace]`**
table so the root `cargo build` does not absorb it (the exact precedent of
`tree-sitter-ascript/`, which already uses this layout for the same reason):

```toml
[package]
name = "ascript-wasm"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
ascript = { path = "..", default-features = false, features = ["data", "binary", "log", "shared"] }
wasm-bindgen = "0.2"
wasm-bindgen-futures = "0.4"
js-sys = "0.3"
serde = { version = "1", features = ["derive"] }
serde-wasm-bindgen = "0.6"
tokio = { version = "1", features = ["rt", "macros", "sync"] }

[dev-dependencies]
wasm-bindgen-test = "0.3"

[workspace]
```

**Why a separate crate, not a cfg'd module in the main crate (the brief's decide-point):**
(1) `wasm-bindgen`/`js-sys`/`serde-wasm-bindgen` never enter the main crate's graph — the
main crate's only wasm-conditional additions are the tiny platform-shim deps (§5.3.3), kept
to the minimum that *core* code needs; (2) the export surface (`run()`, the JS API contract)
versions independently of the language crate; (3) `wasm-pack` wants to own a crate root
(packaging, `pkg/` output, test runner) — pointing it at a leaf crate keeps the main build
system untouched; (4) it mirrors RT's direction (separate runtime artifacts, main crate
clean). The curated feature list lives ONCE, here.

### 5.2 The curated wasm stdlib subset (derived from the feature graph)

No new Cargo feature. The wasm build is `default-features = false` + the list below. The
table is the documentation contract (`docs/content/tooling/playground.md` reproduces it).

| Status | Modules | Mechanism |
|---|---|---|
| **In (CORE — always compiled)** | `math`, `string`, `array`, `object`, `collections` (map/set), `convert`, `task`, `sync` (channels), `stream`, `time` (clock via shim §5.3.3; timers per §5.3.4), `schema`, `caps`, `assert`, `bench`, events/LRU/template utilities | un-gated core |
| **In (features)** | `data` → json, regex, encoding (base64/hex/url/percent), csv, yaml, uuid, url; `binary` → msgpack, cbor; `log` (capture-buffer routing already exists); `shared` (`freeze` — pure in-memory) | `--features data,binary,log,shared` |
| **Candidate (spike-verified, in if green)** | `crypto` (md5/sha/hmac/argon2/bcrypt/crc32/xxhash — pure compute; `rand`→`getrandom(js)`); `datetime` (`chrono` — needs its `wasmbind` clock on wasm) | added to the list only on a green spike cell; otherwise recorded out |
| **Out (OS surface — excluded by features)** | `sys` (fs/env/process/io/os), `net`/`http`/`ws`/server, `sql`/`postgres`/`redis`, `compress` (zstd/brotli link C), `tui`, `workflow` (writes a log file), `ffi`, `intl` (icu size), `sysinfo`, `telemetry`, `ai`, `pkg`, `lsp`, `dap`, `doc`, `profile` | feature absent → `import "std/fs"` is the **existing** unknown-module error |
| **Out (platform)** | workers (all three forms + `run_in_worker` + `task.pmap`/`preduce`), interval/timer resources (`time.every`), REPL | runtime Tier-2 platform error (§5.4); repl module cfg'd out |

Caps are denied-all on top (§5.5) — defense in depth for anything core-and-gated (e.g. a
future core module with an OS touchpoint hits `required_cap` and gets the shipped denial).

### 5.3 Main-crate changes (all wasm-conditional; native byte-identical)

The discipline: every change is (a) a `cfg(target_family = "wasm")` arm whose `not(wasm)`
side is *textually today's code*, or (b) a target-dep table whose non-wasm row resolves to
*today's exact feature set*. Gate W-1 (§7.3) makes this testable: native `cargo tree`,
`cargo test` (both configs), and the differential corpus are unchanged.

**5.3.1 `build.rs`** — skip the `cc` step when `std::env::var("TARGET")` starts with
`wasm32` (keep the `rerun-if-changed` lines unconditional so dep-tracking is stable). The
ungrammar codegen half is pure Rust and runs everywhere.

**5.3.2 `Cargo.toml`** — target-dep tables:

```toml
[target.'cfg(not(target_family = "wasm"))'.dependencies]
tokio = { version = "1", features = ["rt", "rt-multi-thread", "macros", "time", "sync"] }
rustyline = "14"
stacker = "0.1"            # iff the spike finds stacker non-wasm-buildable; else stays in [dependencies]

[target.'cfg(target_family = "wasm")'.dependencies]
tokio = { version = "1", features = ["rt", "macros", "sync"] }   # + "time" iff spike (b) shows it compiles
wasm-bindgen = "0.2"
js-sys = "0.3"
getrandom = { version = "0.2", features = ["js"] }
uuid = { version = "1", features = ["v4", "v7", "js"], optional = true }
```

(`uuid` must be declared in both tables since it's optional-under-`data`; the wasm row adds
`js`. `rt-multi-thread` is dropped from the wasm row — it is *unused in `src/` anyway*,
§2.2.) `src/lib.rs`: `#[cfg(not(target_family = "wasm"))] pub mod repl;`.

**5.3.3 `src/platform.rs` (new, core, ~80 lines)** — the ONE home for raw ambient sources,
*below* the det seams:

```rust
/// WASM §5.3.3 — platform clock/entropy/sleep. Native arms are byte-for-byte the
/// previous inline implementations; det.rs seams sit ABOVE this and are unchanged.
pub fn now_unix_ms() -> f64 { /* native: SystemTime; wasm: js_sys::Date::now() */ }
pub fn monotonic_ms() -> f64 { /* native: Instant since LazyLock start; wasm: performance.now() */ }
pub fn entropy_seed() -> u64 { /* native: SystemTime-derived (today's math.rs seed()); wasm: getrandom */ }
pub async fn sleep_ms(ms: u64) { /* native: tokio::time::sleep; wasm: a JS setTimeout future */ }
```

Consumers re-routed (behavior-identical on native): `stdlib/time.rs` (`now`/monotonic),
`stdlib/math.rs` `seed()`, `stdlib/date.rs` if `datetime` lands in the subset,
`task_mod.rs` `timeout`/`retry` sleeps, `time_timers.rs` `sleep`. The wasm `sleep_ms` is a
hand-rolled `wasm-bindgen` `setTimeout` future (~25 lines, no `gloo` dep): it wakes via the
JS callback, which works precisely because the whole program future is polled by the browser
microtask loop (§5.3.6).

**5.3.4 Timers that are resources** — `time.every`/interval (`ResourceState::Interval`,
`src/interp.rs:272`) and any `tokio::time::Interval` construction get a
`cfg(target_family = "wasm")` arm raising Tier-2:
`time.every is not available on this platform (wasm)`. v1 keeps the *type* compiling
(tokio `time` feature on wasm per spike) or, if `time` doesn't compile on wasm, the
`Interval` variant itself is cfg'd with a constructor-side guard — the spike output decides
which arm; both are specified in the plan. `time.sleep`, `task.timeout`, `task.retry` —
the common cases — **work** via `sleep_ms`.

**5.3.5 Recursion depth without stacker** — `src/vm/stack.rs` gains
`#[cfg(target_family = "wasm")]` pass-through bodies for `grow`/`grow_future` (run the
closure / pin the future, no probe) — *iff* the spike shows stacker non-buildable; if it
builds-but-degrades, it stays and the pass-through is what `maybe_grow` already does there.
Either way, wasm cannot grow stack segments, so:
- the wrapper crate sets `-C link-arg=-zstack-size=8388608` (8 MiB shadow stack) via
  `.cargo/config.toml` in `ascript-wasm/`;
- `src/interp.rs` gains `#[cfg(target_family = "wasm")] pub const MAX_CALL_DEPTH: u32 = …`
  (initial value 1000; **calibrated by a spike measurement** — a deep-recursion probe under
  Node binary-searches the real native-frame budget and the constant is set to ≤50% of it),
  with the native constant untouched at 3000;
- the panic message stays exactly `maximum recursion depth exceeded` — same error, lower
  ceiling, documented in the playground docs. This is a **documented platform asymmetry**
  (the SP3/VM-capacity precedent: documented, tested, never silent).

**5.3.6 The wasm executor model** — decided in §2.3: tokio stays; nothing blocks. The
wrapper's whole run is:

```rust
let local = tokio::task::LocalSet::new();
// (+ a never-blocked current_thread Runtime enter-guard iff spike (b) shows it's required)
let result = local.run_until(async { ascript::run_source_exit(&source).await }).await;
```

awaited inside a `#[wasm_bindgen]` `pub async fn` — wasm-bindgen-futures turns it into a
Promise and polls it on the microtask queue. Single-threaded, `!Send`, cooperative — the
M17 model unmodified. `maybe_yield_for_inflight` and eager `spawn_local` scheduling work
unchanged because `run_until` provides the LocalSet context for the entire run.

**5.3.7 Workers guard** — one chokepoint: `spawn_isolate` (`src/worker/isolate.rs:211`)
gets a `cfg(target_family = "wasm")` arm returning the error that surfaces as Tier-2
`workers are not available on this platform (wasm)` through the existing
`dispatch_worker` error path — covering `worker fn`, `worker class.spawn()`, `worker fn*`,
`run_in_worker`, and `task.pmap`/`preduce` **by construction** (they all funnel here).
Never silent, never a hang. (A `cfg`-time compile-out of `src/worker/` was rejected: the
module compiles fine on wasm, and a runtime guard at the funnel is one arm instead of a
cfg-lattice across `interp.rs`/`run.rs` dispatch sites.)

### 5.4 The wrapper API

```rust
// ascript-wasm/src/lib.rs
#[wasm_bindgen]
pub async fn run_program(source: String) -> JsValue  // → RunResult, via serde-wasm-bindgen
```

```ts
type RunResult = {
  ok: boolean,
  output: string,          // OutputSink::Capture contents (print + captured log)
  error: string | null,    // panic / Tier-1 error rendering, plain text (no ANSI)
  diagnostics: string[],   // compile/check diagnostics, ariadne rendered with color OFF
  exitCode: number | null, // exit(n) if called
  durationMs: number,
}
```

Behavior: build the program the same way `run_source_exit` does (VM engine, capture sink) —
the wrapper calls into a new thin `ascript` entry
`pub async fn wasm_run_source(src: &str, caps: CapSet) -> …` (lib-side, cfg-free; it is just
`run_source_exit` + an explicit `CapSet` parameter so the deny-all is applied at `Interp`
construction — native tests exercise it too, keeping it on the tested path). The wrapper
passes `CapSet::all_granted().deny_all_dangerous()`-equivalent (all five denied).
Diagnostics render through ariadne with color disabled (plain text for the `<pre>` pane).
No panic ever crosses the FFI boundary un-caught: the wrapper converts every `Err` into
`RunResult{ok:false, …}`, and a Rust panic is caught by `console_error_panic_hook` +
documented as a playground bug (Gate 14 class).

### 5.5 Playground UI (minimal by design — the wasm is the feature)

- **`docs/playground.html`** — a standalone app page (sibling of `reader.html`), same
  topbar/branding/styles (`docs/assets/styles.css`).
- **`docs/assets/playground.js`** — vanilla JS following `app.js` conventions (no framework,
  no build step). Layout: `<textarea>` editor (monospace; v1 — CodeMirror is recorded future
  work), Run / Stop buttons, an output `<pre>`, an examples `<select>` populated from a
  small embedded manifest of wasm-compatible examples, and a share link
  (`playground.html#code=<base64url>` read on load).
- **Execution in a browser Web Worker** (`docs/assets/playground-worker.js`): the worker
  imports the wasm-bindgen `pkg/` and runs `run_program`; the page `postMessage`s source in
  and results out. **Stop = `worker.terminate()` + lazy re-instantiate** — the only reliable
  kill for an infinite loop in wasm, and it keeps the docs UI thread jank-free. (This is a
  browser Web Worker — JS-side plumbing only; it is NOT the AScript worker subsystem and
  shares nothing with §5.3.7.)
- Artifact location: `docs/assets/playground/pkg/` (the `wasm-pack build --target web`
  output, `wasm-opt`'d), built by `scripts/build-wasm.sh` and committed (the docs site is
  static and must keep working from a plain checkout + `python3 -m http.server`, per
  CLAUDE.md). The CI smoke rebuilds and diff-checks it so the committed artifact can't go
  stale silently.
- **NAV / linking (the NAV-orphan rule):** the playground *app page* is not Markdown, so it
  cannot be a NAV slug itself. Instead: (1) a new content page
  `docs/content/tooling/playground.md` (what runs, the subset table from §5.2, the limits —
  recursion ceiling, no workers/timers caveat, deny-all caps) **with its NAV entry** under
  Tooling; (2) direct links to `playground.html` from `index.html`'s topnav + hero and from
  the reader topbar (`reader.html:16`); (3) `docs/content/examples.md` gains a "run these in
  the playground" pointer. **Stretch (optional-in-v1, specced):** `app.js` adds a "Run ▶"
  button to fenced `ascript` code blocks on docs pages that opens
  `playground.html#code=<base64url(block)>` — read-side support (the hash loader) ships in
  v1 regardless, so the stretch is purely the button injection in `renderMarkdown`.

### 5.6 The mini-differential CI gate (wasm output == native output)

Not a fifth differential mode — a corpus smoke:

- `ascript-wasm/tests/` carries `wasm-bindgen-test` unit smokes (hello, async/await, panic
  rendering, cap-denied error, worker-unavailable error, recursion-ceiling error, gc cycle
  drop) run via `wasm-pack test --node`.
- `scripts/wasm_smoke.mjs` (Node ≥18): loads the built pkg, runs every example in
  `EXAMPLES_WASM` (a curated, committed list in the script — the subset of `examples/*.as`
  whose imports ∩ excluded-modules = ∅ and which don't use workers/timers/intervals;
  initial sweep expected ≳25 of 71: `hello`, `factorial`, `functions`, `oop`,
  `pattern_matching`, `enums_adt`, `generators`, `async`, `interfaces`, `generics`,
  `numbers`/`integers`/`numeric_tower`, `ranges`, `data`, `serialization`, …), captures
  `RunResult.output`, and **byte-compares** against `target/release/ascript run <file>`
  stdout captured in the same CI job. Any diff fails CI. The list is a committed artifact;
  shrinking it requires a recorded reason in the file (anti-rot).
- CI (`.github/workflows/ci.yml`, new `wasm` job): rustup target add → main-crate
  `cargo check --target wasm32-unknown-unknown --no-default-features --features data,binary,log,shared`
  → `cargo clippy --target wasm32-unknown-unknown` (same features, `-D warnings`) →
  `wasm-pack build && wasm-pack test --node` in `ascript-wasm/` → `node scripts/wasm_smoke.mjs`
  → size report (raw/-Oz/gz bytes) emitted to the job summary.

### 5.7 Size & load-time (measured, not promised)

Recorded at spike time (§4.5) and re-recorded at merge in `bench/WASM_SIZE.md`: raw,
`wasm-opt -Oz`, gzip, brotli, plus Node instantiate time and a browser cold-load number
(devtools, recorded manually once). Expectations to validate, not commitments: the dominant
contributors should be the compiler+VM core, `regex`, `serde_json`/`yaml`, and
`rust_decimal`; `icu` (the usual wasm-size disaster) is excluded by the subset. If gzip size
exceeds 5 MB, the recorded follow-ups are feature-list pruning (drop `csv`/`yaml`/`binary`
first) and `wasm-opt` flag tuning — applied only with the measurement in hand.

## 6. Failure modes

| Failure | Handling |
|---|---|
| `import "std/fs"` (any excluded module) | existing unknown-module error (feature absent) — same text as `--no-default-features` native |
| A core-module OS call that survives pruning | `required_cap` chokepoint → existing capability-denied error (caps denied-all) |
| `worker fn` / actor `spawn()` / `worker fn*` / `run_in_worker` / `pmap` | Tier-2 `workers are not available on this platform (wasm)` at the `spawn_isolate` funnel — never a thread-spawn panic |
| `time.every` / interval resources | Tier-2 `… not available on this platform (wasm)` (§5.3.4) |
| Deep recursion | same `maximum recursion depth exceeded`, wasm-calibrated ceiling (§5.3.5) — never a wasm stack overflow trap (the calibration margin is the guard; the trap, if ever observed, is a bug to fix by lowering the constant, Gate 14) |
| Infinite loop / runaway program | playground Stop = `worker.terminate()`; the wasm instance is discarded and re-created |
| Rust panic inside the engine | `console_error_panic_hook` → surfaced in `RunResult.error` path as a playground bug; never a silent dead button |
| `math.random` / `uuid.v4` | entropy via `getrandom(js)`/the platform seed — works; det Record/Replay unaffected (seams above the shim) |
| Output ordering | `OutputSink::Capture` is the single buffer — same ordering guarantees as `run_source` tests |

## 7. Scope decisions, rejected alternatives, gates

### 7.1 Rejected for v1 (recorded so they aren't re-litigated)

- **Workers on wasm** — needs wasm threads + SharedArrayBuffer + COOP/COEP headers, a
  `Send`-able airlock variant, and a thread-pool story inside the browser; enormous
  complexity for a playground. Rejected v1; the Tier-2 error is the honest surface. (A
  future wasm-threads design would be its own spec.)
- **JIT on wasm** — meaningless (no native codegen target; wasm *is* the codegen target and
  that's a different spec class entirely).
- **Shipping `.aso` to the browser** — the playground compiles in-browser (that's the demo);
  `.aso` loading + verify-on-load in the wrapper is recorded future work (§8).
- **A fifth differential mode** — would require running the full corpus + goldens under a
  wasm host in the differential harness; v1's mini-differential smoke (§5.6) buys most of
  the safety at a fraction of the cost. Promoting it to a real mode is v2 work, gated on the
  WASI entry (§8) where a host with fs access makes the harness natural.
- **Replacing tokio with a bespoke wasm executor** — loses `JoinHandle`/`AbortHandle`/
  cancel-on-drop, the runtime's riskiest invariants (§2.3). Re-evaluated only if spike (b)
  fails. (EXEC's future bespoke executor, if its gate ever opens, must keep these semantics
  too — its seam would slot in here identically; v1 takes the simplest working path.)
- **Compiling parser.c with `clang --target=wasm32`** — unnecessary: zero `src/` consumers
  (§2.1). Revisit only if the CST front-end ever grows a tree-sitter dependency.
- **CodeMirror/Monaco** — v1 is a textarea; the wasm is the feature. Recorded future polish.

### 7.2 Documented platform asymmetries (v1, honest and tested)

1. Lower recursion ceiling (wasm-calibrated `MAX_CALL_DEPTH`), same error text.
2. No workers / intervals / OS stdlib — Tier-2 / unknown-module errors as tabled.
3. Captured-only output (no live streaming v1).
All three are documented on `tooling/playground.md` and asserted by wrapper tests.

### 7.3 Gates

Gates 1–14 of `goal.md` + the `goal-perf.md` additions apply; WASM's specific instantiation:

- **W-1 (native invariance):** `cargo tree` (default + `--no-default-features`), `cargo test`
  (both configs), clippy (both configs), and the four-mode differential are **unchanged** on
  native after every main-crate edit — the target-dep tables must resolve the non-wasm graph
  identically (checked by diffing `cargo tree` output before/after in the plan).
- **W-2 (wasm CI):** the §5.6 job — wasm clippy clean (`-D warnings`), wrapper tests green
  under Node, mini-differential byte-equal, committed-artifact freshness check, size
  recorded.
- **W-3 (no silent gaps):** every excluded capability has a test asserting its clean error
  (worker, interval, fs-import, cap-denied) — the negative-space discipline.
- **W-4 (docs):** `tooling/playground.md` + NAV entry (the orphan rule), `index.html`/reader
  links, README pointer, `CLAUDE.md` + `superpowers/roadmap.md` + `goal-perf.md` status
  updates.
- **No `.aso` bump, no grammar change, no new `Op`** — pinned by a negative-space test
  (`tests/wasm_negative_space.rs`, native-side: `ASO_FORMAT_VERSION` unchanged, no
  `target_family = "wasm"` cfg leaks into `Chunk`/serializer code paths).

## 8. v2 — recorded, not built (each with its own gate)

- **WASI / edge runtimes (wasmtime, workerd):** a `wasm32-wasip1` (or p2) build with a real
  entry (`main`-shaped, not wasm-bindgen), a stdlib-over-WASI mapping (fs/clock/random via
  WASI; net via WASI-sockets or host bindings), caps mapped onto WASI's own capability
  handles (a natural fit), and the differential harness runnable under wasmtime → the
  **fifth differential mode** lands here. **Gate:** demonstrated demand (an embedding or
  deployment user) + the v1 playground proving the core port stable for a full release
  cycle. Sketch only; its own spec when gated open.
- **`.aso` in the browser** (compile once, run many — docs examples could ship precompiled),
  **streaming output** (a JS callback sink variant beside `Capture`), **CodeMirror**, and
  **wasm-threads workers** — each recorded with the v1 seam it would extend.

## 9. Cross-cutting checklist (CLAUDE.md discipline)

- Two parsers / grammar regen: **not touched** (no syntax change).
- `ExprKind`/`Pattern` exhaustive matches: **not touched**.
- Engines byte-identical: native unaffected (W-1); wasm guarded by the mini-differential.
- `.aso`: **unchanged** (negative-space pinned).
- Grammar publish: n/a.
- Docs: §7.3 W-4. CLAUDE.md gains a short WASM section (build command, wrapper crate, the
  platform-asymmetry list, the "wasm row must mirror native deps" rule for future dep edits).

## 10. Evidence appendix (filled by the Phase-0 spike — committed verbatim)

> Populated by plan Tasks 0.2–0.7. Until then, the static evidence stands:
>
> - `build.rs:1-11` — unconditional `cc` compile of `tree-sitter-ascript/src/parser.c`.
> - `grep -rn "tree_sitter_ascript" src/ tests/` → sole consumer
>   `tests/treesitter_conformance.rs:11` (dev-only). Zero `src/` references.
> - `grep -rn "multi_thread" src/` → empty; `Cargo.toml:32` enables `rt-multi-thread`
>   (vestigial); `src/main.rs:480` uses `new_current_thread`.
> - `src/interp.rs:375` `OutputSink`; `:975` capture default.
> - `src/stdlib/caps.rs` deny-only `CapSet`; `src/stdlib/mod.rs:325` `required_cap`.
> - `src/stdlib/time.rs:8,29,44` SystemTime/Instant; `src/stdlib/math.rs:501-535`
>   time-seeded xorshift + `next_seeded_f64` det hook.
> - `gcmodule-0.3.3/src/collect.rs:248` `thread_local!` object space (vendored source).
> - `src/worker/isolate.rs:273` `std::thread::Builder` (the single spawn funnel at `:211`).
>
> ### 10.1 (a) dependency inventory — `cargo check` transcripts        [SPIKE — to fill]
> ### 10.2 (b) executor probe — wasm-pack test output                  [SPIKE — to fill]
> ### 10.3 (c) tree-sitter skip — native conformance green            [SPIKE — to fill]
> ### 10.4 (d) gcmodule runtime smoke + stacker/getrandom cells        [SPIKE — to fill]
> ### 10.5 (e) size/load numbers                                       [SPIKE — to fill]
> ### 10.6 GO/NO-GO record (decision, date, signer)                    [SPIKE — to fill]
