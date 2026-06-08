# FFI — Foreign Function Interface & Opt-Out Capabilities — Implementation Plan

> REQUIRED SUB-SKILL: superpowers:subagent-driven-development (fresh implementer + independent reviewer
> per task; the reviewer runs the commands and probes edges). Steps use `- [ ]`. Two co-designed
> subsystems: CAPS (the safety substrate) lands **before** FFI (the dangerous capability it guards).

**Spec:** `superpowers/specs/2026-06-08-ffi-capabilities-design.md`. **Branch:** `feat/ffi-capabilities`
off `main`. **Depends on:** **NUM** merged (sized C ints marshal over `Value::Int(i64)`; there is no FFI
without exact integers) + Workers Spec A (the per-isolate `!Send` model + the structured-clone airlock).
**Not breaking:** capabilities are **default-all-granted** (every existing program runs unchanged); FFI
is an additive `std/ffi` module. **No grammar/parser/tree-sitter/formatter change** (pure stdlib + an
`Interp` field + CLI/manifest — the two-parsers/tree-sitter/formatter rows are N/A; VERIFY this holds).
**No `.aso` bump** (FFI/caps add no opcode and no serialized layout).

**Architecture (two subsystems):**
- **CAPS first** — a per-`Interp` `CapSet` (default = all granted) with **ONE central gate** at the
  `&self` dispatch site `Interp::call_stdlib` (`src/stdlib/mod.rs:276`), inserted immediately before the
  terminal `match module { … }` (`src/stdlib/mod.rs:364`), keyed by **module string** via
  `required_cap(module, func) -> Option<Cap>`. Gating by module-at-the-dispatch-root captures DNS
  (`net.lookup` → `call_net`/`net_host`) **by construction** — there is no per-connect bypass. Five
  capabilities: `fs`/`net`/`process`/`ffi`/`env`. Opt-OUT at three scopes (CLI `--deny`/`--sandbox`,
  `ascript.toml` `[capabilities]`, in-code irreversible `caps.drop`). Cap-reduced/untrusted work runs on
  a **DEDICATED** isolate (`run_in_worker({caps})` → `spawn_isolate`, the `Send` `CapSet` captured in the
  `make_loop` closure); `caps.drop` is **REFUSED** in a pooled `worker fn` (shared-`Interp` reuse hazard).
- **FFI then** — `std/ffi` over `libloading` (`dlopen`/`dlsym`) + `libffi` (the C-ABI trampoline), a new
  default-ON `ffi` feature + deps. Sized C ints (`i8…u64`/`size`) marshal **over `int`** (no new `Value`
  kind). Three new `NativeKind` variants (`ForeignLib`/`ForeignSymbol`/`ForeignPtr`) backed by
  `ResourceState`; `ForeignSymbol` stores a raw `*mut c_void` + **keeps the `Library` alive** (a borrowed
  `Symbol<'lib>` cannot be `'static`); all three stay **GC-untraced** (caught by `Value::trace`'s
  `_ => {}` over `Native`). SP9 record/replay records the marshalled return AND **post-call `Bytes`
  out-param contents**, and **refuses** (loud Tier-2) a `ForeignPtr` out-param / pointer return.

**Tech stack:** Rust; `src/stdlib/{ffi,caps,mod}.rs`; `src/value.rs`; `src/interp.rs`; `src/worker/`;
`src/main.rs`; `src/pkg/manifest.rs`; `src/det.rs`; `src/check/{std_arity,rules}.rs`; `Cargo.toml`.

---

## Shared API Contract (pinned to current code — VERIFIED)
**Existing (verified):** `Interp::call_stdlib` `src/stdlib/mod.rs:276` (the `&self` async funnel; terminal
`match module` `:364`); `std_module_exports` `mod.rs:109`; `STD_MODULES` `mod.rs:211`; `is_std_module`
`mod.rs:271`. `struct Interp` `interp.rs:437`; `resources: RefCell<HashMap<u64, ResourceState>>`
`interp.rs:447`; `Interp::new` `interp.rs:788` (resources init `:805`); `register_resource` `interp.rs:1789`;
`enum ResourceState` `interp.rs:168`. `enum NativeKind` `value.rs:366`; `type_name` `value.rs:483`;
`Value::Native` print arms `value.rs:763,882`. `Value::trace` `Native` catch-all `gc.rs:177` (`_ => {}`).
`call_net` `net_host.rs:51` (`lookup`/`lookupOne` — NOT connect/bind); `call_io` `io.rs:32`
(`readLine`/`readAll`/`readLines`); `os` topology `os.rs:171` (`networkInterfaces`)/`:198`-ish (`localIp`)/
`:88` (`hostname`); `fs::call` free fn `fs.rs:70` (no `&self`); `env::call` free fn `env.rs:28` (no `&self`).
Worker: `WorkerRequest` `isolate.rs:37` (all-`Send` fields); `spawn_isolate` `isolate.rs:140` (inbound
`Vec<u8>`; `Send + 'static` `make_loop` `:142`); `run_isolate_thread` builds `Interp::new()` `isolate.rs:230`;
`isolate_loop` reuses one `Interp` across requests `isolate.rs:240`; pool `dispatch` `pool.rs:68`. `det.rs`:
`Mode::{Record,Replay}` `:39,42`; `enum DetEvent` `:50`; `enum BoundaryOutcome` `:94`; `DeterminismContext`
`:199`. `main.rs`: `Command::Run` `:17` (`tree_walker` `:23`; dispatch `:213`). `manifest.rs`:
`struct Manifest` `:21` (`package` `:25`, `dependencies` `:29` — `[capabilities]` is a NEW owned table).
`std_arity.rs`: `std_fn_arity` `:31`, `required_args` `:40`, drift-guard `:104`. `rules::ALL`
`src/check/rules/mod.rs:30`. `Cargo.toml` `default` set `:107`.

**New names (do not rename):** `Value` is **unchanged** (no new variant). `NativeKind::{ForeignLib,
ForeignSymbol,ForeignPtr}`; `ResourceState::{ForeignLib(libloading::Library), ForeignSymbol(...),
ForeignPtr(usize)}`. `struct CapSet` (`Send + Clone + Copy`-bitset + `Option<FsScope>`/`Option<NetScope>`);
`enum Cap { Fs, Net, Process, Ffi, Env }`; `Interp.caps: RefCell<CapSet>`; `required_cap(module, func) ->
Option<Cap>`; `Interp::require_cap(Cap, module, func, args, Span) -> Result<(), Control>`. Cargo `ffi`
feature (`ffi = ["dep:libloading", "dep:libffi"]`), added to `default`. Lint `ffi-nondeterminism`.

## Conventions (every task)
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- `cargo test` AND `cargo test --no-default-features` green; clippy clean BOTH configs (`--all-targets`
  AND `--no-default-features --all-targets`). `std/caps` is **core** (no feature gate) — it must build and
  function under `--no-default-features`; `std/ffi` is `#[cfg(feature = "ffi")]`.
- Both engines byte-identical (`vm_differential.rs`, both feature configs) — the whole feature lives at the
  shared `Value`/`Interp`/stdlib layer, so this holds by construction; fix the engine, never the assertion.
- **No `await` across a `RefCell`/resource borrow** (clippy `await_holding_refcell_ref = "deny"`): read a
  `Copy` snapshot of `caps`; take-out-across-await for the `Library`/symbol resources.
- Tests are **hermetic** (libc/libm; no fixtures to compile). All `caps.*` and script-exposed `ffi.*` fns
  register in `std_arity.rs` (drift-guard `:104` cross-checks against real exports).

---

## Task 1 — `CapSet` + `Cap` value type (core, no feature gate)
**Files:** new `src/stdlib/caps.rs` (the `CapSet`/`Cap`/`FsScope`/`NetScope` types + helpers only — the
`std/caps` module routing is Task 4). **Tests:** unit in `caps.rs`.
- [ ] Failing tests: `CapSet::all_granted()` has every cap; `.deny(Cap::Ffi)` clears only `ffi`;
  `.has(Cap::Net)` reflects it; `deny` is **monotone** (deny then deny-again is idempotent; there is no
  grant method — assert the type exposes none); `CapSet` is `Send + Clone + Copy` (bitset) with the two
  `Option<FsScope>`/`Option<NetScope>` carve-out fields defaulting to `None`; `from_deny_list(["ffi",
  "process"])` parses cap names and errors on an unknown name; `--sandbox`-equivalent `deny_all_dangerous()`
  clears all five.
- [ ] Implement `enum Cap { Fs, Net, Process, Ffi, Env }`, `struct CapSet { bits: u8, fs_scope:
  Option<FsScope>, net_scope: Option<NetScope> }` (a closed five-bit set), `FsScope`/`NetScope`
  ({deny: Scope, allow: Vec<Pattern>}), `cap_name(&str) -> Option<Cap>`. `deny`/`deny_all` only subtract —
  no public mutator widens the set. `Default` = all-granted.
- [ ] Green both configs; clippy. Review (confirm no `grant` path exists; `Copy`-snapshot ergonomics).
  Commit.

## Task 2 — `Interp.caps` + `require_cap` + the ONE central dispatch gate
**Files:** `src/interp.rs` (struct field + helper), `src/stdlib/mod.rs` (the gate). **Tests:**
`tests/cli.rs` (a `--deny`-style harness driving a script) + unit in `mod.rs`.
- [ ] Failing tests: with a `CapSet` denying `ffi`, a script call routed through `call_stdlib` for module
  `"ffi"` raises the **recoverable Tier-2 panic** `capability 'ffi' denied` (catchable by `recover`);
  default (all-granted) `Interp::new()` changes NOTHING (existing tests stay green — byte-identical path);
  `required_cap` mapping unit test: `"fs"`/`"io"`/`("os","<file>")`→`Fs`, `"net"`/`"net_tcp"`/`"net_http"`/
  `"net_udp"`/`"net_ws"`/`"http_server"`/`("os","networkInterfaces"|"localIp"|"hostname")`→`Net`,
  `"process"`→`Process`, `"ffi"`→`Ffi`, `"env"`→`Env`, and a non-resource module (e.g. `"math"`, `"json"`)
  → `None`.
- [ ] Add `caps: RefCell<CapSet>` to `struct Interp` (`interp.rs:437`); init **all-granted** in
  `Interp::new` (`interp.rs:788`/`:805` neighborhood). Add `fn require_cap(&self, cap, module, func, args,
  span) -> Result<(), Control>` that reads a `Copy` **snapshot** of `caps` (NEVER holds the borrow across
  `.await`) and returns the recoverable denial panic. Add the free fn `required_cap(module, func) ->
  Option<Cap>` (the **complete central enumeration**, §4.1/§4.3a; `os` is the one module that inspects
  `func`). Insert the gate in `call_stdlib` immediately **before** `match module { … }` (`mod.rs:364`):
  `if let Some(cap) = required_cap(module, func) { self.require_cap(cap, module, func, args, span)?; }`.
- [ ] **[SECURITY] DNS captured by construction:** add a test that the gate fires for `module == "net"`
  (the `net.lookup`/`lookupOne` path through `net_host`) — there is no per-connect-only path that can slip
  it (this is the [Gate 10] regression guard; full end-to-end version in Task 9).
- [ ] **Drift-guard test:** assert every resource-acquiring module string in `STD_MODULES` / the dispatch
  match has a `required_cap` entry (a `None` is an explicit decision, not an omission) — adding a new
  `std/*` module forces an entry here.
- [ ] Green both configs; clippy. Review (greps for any cap check living inside a free fn like `fs::call`;
  confirms the single funnel; confirms no borrow held across await). Commit.

## Task 3 — Granular fs-path / net-host carve-outs (Gate-12 short-circuit)
**Files:** `src/stdlib/caps.rs` (scope matching), the net connect/bind entries (`net_tcp::tcp_connect`/
`tcp_listen`, `net_http` request, `net_udp`/`net_ws` binds) + the fs path-resolving entries. **Tests:**
`tests/cli.rs` granular cases + unit.
- [ ] Failing tests: `net = { deny = "external", allow = ["127.0.0.1"] }` blocks a public address but
  permits loopback at connect time; `fs = { deny = "write", allow = ["./cache"] }` permits a read and a
  write under `./cache` but blocks a write elsewhere (resolved/canonicalized path prefix test);
  **[Gate 12]** when **no** carve-out is configured (`Option` is `None` — the default and the all-`--deny`/
  all-grant cases) the second-stage check **short-circuits to the bitset** with **no path canonicalization
  and no host comparison** (assert via a hook/counter that `canonicalize`/`to_addr` is NOT called on the
  hot path).
- [ ] Two-stage check: stage 1 is the dispatch-site bitset test (Task 2 — denied-outright panics now,
  granted-outright passes, *granular-configured* defers); stage 2 lives at the connect/bind + fs
  path-resolving entries and re-checks the resolved address/path **only when** `CapSet.net_scope`/`fs_scope`
  is `Some`. The `Option` IS the fast-path gate (no always-empty allow/deny list walk).
- [ ] Green both configs; clippy. Review (confirms the hot path never canonicalizes; the carve-out is the
  only place granularity exists). Commit.

## Task 4 — `std/caps` module (has/list/drop/dropAll) + pooled-drop refusal
**Files:** new `src/stdlib/caps.rs` `exports()`/`call`, register in `src/stdlib/mod.rs` (`std_module_exports`
`:109` arm + the `call` match `:364` arm + `STD_MODULES` `:211`, all **unconditional** — core), `std_arity.rs`.
**Tests:** unit in `caps.rs` + `tests/check.rs` arity.
- [ ] Failing tests: `caps.has("net")` → bool; `caps.list()` → array of currently-granted names;
  `caps.drop("process")` then `caps.has("process")` is false **and stays false** (no re-grant API);
  `caps.dropAll()` clears all five; an unknown cap name to `drop`/`has` is a Tier-2 panic; **`caps.drop`
  inside a pooled `worker fn` is REFUSED** (no-op-with-warning / recoverable panic — see §4.5a; the
  Interp carries a flag set at pooled-request install time, Task 8); on the top-level program isolate the
  drop mutates `Interp.caps` and is irreversible.
- [ ] `caps.call` routes `has`/`list`/`drop`/`dropAll` through `&self` (mutating `Interp.caps` for the two
  drops — `RefCell`, **no borrow across await**). `caps.drop` consults a per-`Interp` "drop-allowed" flag
  (default true; cleared on a pooled request, Task 8) and refuses when false. Register `("caps","has"|
  "list"|"drop"|"dropAll")` in `std_arity.rs` (drift-guard `:104`).
- [ ] Green both configs (incl. `--no-default-features` — caps is core). clippy. Review (monotonicity;
  no `grant`; the pooled-refusal path). Commit.

## Task 5 — CLI flags + `ascript.toml [capabilities]` manifest
**Files:** `src/main.rs` (the `Run` subcommand `:17`/`:213`), `src/pkg/manifest.rs` (a third owned table
`:21`). **Tests:** `tests/cli.rs` (flags), `tests/pkg.rs` (manifest parse).
- [ ] Failing tests: `ascript run app.as --deny ffi,process` denies exactly those; `--sandbox` denies all
  five; `--deny-net=external` / `--deny-fs=/etc` build the granular `Net`/`Fs` scope; an unknown cap name
  to `--deny` is a clean CLI error (not a panic); a `[capabilities] deny = ["ffi"]` table parses into a
  `CapSet`; manifest denials **union** with CLI denials (you cannot re-grant via CLI what the manifest
  denied — denial is monotone); a granular `net = { deny = "external", allow = ["api.internal"] }` /
  `fs = { deny = "write", allow = ["./cache"] }` table parses into the scope.
- [ ] Add `--deny <list>`, `--deny-net=<scope>`, `--deny-fs=<scope>`, `--sandbox` to `Command::Run`;
  parse into a `CapSet`; thread it into the `Interp` builder (set `Interp.caps` after construction, before
  running). Add a `[capabilities]` table to `struct Manifest` (parsed CLI-side — TOML/IO stays out of the
  core, per SP6); produce a `CapSet` unioned with the CLI's. (Optional `--sandbox` on `repl`.)
- [ ] Green both configs; clippy. Review (CLI/manifest union is monotone; no re-grant). Commit.

## Task 6 — FFI handles: `NativeKind` + `ResourceState` + GC-untraced proof
**Files:** `src/value.rs` (`NativeKind` `:366` + `type_name` `:483`), `src/interp.rs` (`ResourceState`
`:168` + `register_resource` `:1789`), `Cargo.toml` (`ffi` feature + `libloading`/`libffi` deps).
**Tests:** unit in `value.rs`/`gc` + `Cargo` both configs.
- [ ] Failing tests: `NativeKind::ForeignLib.type_name()` etc. return stable names;
  **[Gate 4 GC] `Value::Native(ForeignPtr)` contributes ZERO traced edges** (`Value::trace` `gc.rs:177`
  no-ops for `Native` via `_ => {}` — assert by tracing into a counter); the three resources reclaim on
  `Drop` (a `ForeignLib` drop `dlclose`s — assert via a drop-counting wrapper).
- [ ] Add `NativeKind::{ForeignLib, ForeignSymbol, ForeignPtr}` + their `type_name` arms (compiler flushes
  any other exhaustive `NativeKind` match). Add `ResourceState::ForeignLib(libloading::Library)` /
  `ForeignSymbol { addr: *mut c_void, cif/argtypes/rettype, _lib: <kept-alive Library ref> }` /
  `ForeignPtr(usize)`. **`ForeignSymbol` stores a raw `*mut c_void` + keeps the owning `Library` alive**
  (NOT a borrowed `Symbol<'lib>` — that is tied to `'lib` and cannot be `'static` in the `'static`
  resource table; the raw-ptr-plus-kept-alive-`Library` pairing gives both `'static` storage and lifetime
  correctness). Add `Cargo.toml` `libloading`/`libffi` as **optional** deps + `ffi = ["dep:libloading",
  "dep:libffi"]` added to the `default` set (`:107`); gate the new `ResourceState`/`NativeKind` FFI bodies
  with `#[cfg(feature = "ffi")]` where they reference the crates (the bare-`NativeKind` variant names can
  stay un-gated to keep matches simple, or gate consistently — pick one and keep both configs compiling).
- [ ] Green both configs; clippy BOTH. Review (the GC no-trace invariant is load-bearing — confirm no
  `Trace` edge is ever added; confirm `--no-default-features` builds without `libloading`/`libffi`). Commit.

## Task 7 — `std/ffi`: open/symbol/call + marshalling + struct/cstr/read_cstr
**Files:** new `src/stdlib/ffi.rs` (`exports()` = the `ffi.*` type descriptors as values + `open`; `call`
routing for `open`/`symbol`/`call`/`struct`/`cstr`/`read_cstr`; the libffi CIF build/invoke + in/out
marshalling), register in `src/stdlib/mod.rs` (both arms + `STD_MODULES`, **`#[cfg(feature="ffi")]`-aware**),
`std_arity.rs`. **Tests:** unit in `ffi.rs` (hermetic libc/libm).
- [ ] Failing tests (hermetic — `dlopen` the platform libc/libm via an OS-resolution helper):
  `cos`/`pow`/`sqrt` (`ffi.f64`), `abs(-5)→5` (`ffi.i32`), `strlen(ffi.cstr("hello"))→5` (`ffi.ptr`+
  `ffi.size`); marshalling: `300 → ffi.u8` is a **Tier-2 panic** `ffi: value 300 out of range for u8`
  (checked narrowing for signed + small-unsigned); **`ffi.u64`/`ffi.size` take the i64 bit pattern with NO
  sign range-check** (`-1` → `0xFFFF_FFFF_FFFF_FFFF`, round-trips bit-identical — resolves the prior
  contradiction); `f32` round-trips with precision loss; `ffi.cstr` is NUL-terminated `Bytes`;
  `ffi.struct([["x",ffi.i32],["y",ffi.f64]])` computes C size/alignment, `.alloc()` zeroes, `get`/`set`
  apply offsets; `ffi.read_cstr(ptr)` copies until NUL into a `Str`. Error tiers: `ffi.open("nope.so")` →
  **Tier-1 `[nil, err]`**; missing symbol → Tier-1; bad signature (argtypes not `ffi.*` descriptors) →
  Tier-2; wrong arity / wrong-shape arg at `call` → Tier-2.
- [ ] Implement: `ffi.*` descriptors as tagged Objects / small natives (NOT new `Value` kinds — like
  `std/schema`). `ffi.open` → `dlopen` → `ResourceState::ForeignLib`, Tier-1. `lib.symbol(name, argtypes,
  rettype)` → `dlsym` → resolve raw `*mut c_void` (via `Library::get` then `into_raw`/cast) + build the
  libffi CIF + keep the `Library` alive → `ForeignSymbol`, Tier-1 on missing symbol / Tier-2 on malformed
  signature. `symbol.call(args)` → marshal in per `argtypes` (checked narrowing §3.3), `unsafe` invoke via
  the trampoline (sound because the `Library` is held), marshal out per `rettype` → **synchronous** (returns
  the value directly, NOT a future — §3.5 stall caveat). `ffi.struct`/`cstr`/`read_cstr` over `Bytes`.
  Register `("ffi","open"|"symbol"|"call"|"struct"|"cstr"|"read_cstr")` in `std_arity.rs`.
- [ ] Green (default config; `--no-default-features` → `import "std/ffi"` is an `unresolved-import`-class
  error since `std_module_exports` returns `None` under the cfg — assert that too); clippy BOTH. Review
  (the `unsafe` deref soundness rests on the kept-alive `Library`; the u64 bit-pattern carve-out; Tier
  split). Commit.

## Task 8 — Worker spawn carries the `CapSet` — dedicated vs pooled (§4.5a keystone)
**Files:** `src/worker/{isolate,pool}.rs`, a new caller-side `run_in_worker` helper (the dispatch entry).
**Tests:** unit in `worker/` + the cap-leak regression (Task 9 has the end-to-end script versions).
- [ ] Failing tests: **[Gate 10 cap-leak]** dispatch two pooled `worker fn` requests to the **same** isolate
  — request A's `caps.drop("net")` is **refused** there (no-op-with-warning), and request B (same reused
  `Interp`) **still has `net`** (no forward leak; no re-grant); a dedicated `run_in_worker(plugin, input,
  {caps:{deny:["ffi"]}})` installs the reduced `CapSet` into the fresh isolate's brand-new `Interp`
  **before** running the plugin, the plugin's in-isolate `caps.drop("process")` is **durable** for that job,
  and the host isolate is **unaffected**; a `CapSet` is `Send` and rides the spawn path with no airlock
  change.
- [ ] **Dedicated path:** `run_in_worker(fn, input, opts)` — a **new** caller-side helper; when `opts.caps`
  is present it spawns a fresh **single-tenant** isolate via `spawn_isolate` (`isolate.rs:140`), capturing
  the `Send` `CapSet` **directly in the `Send + 'static` `make_loop` closure** (`isolate.rs:142`) — it never
  rides the `Vec<u8>` byte channel and never touches the structured-clone value serializer (it is not a
  `Value`). The closure installs `opts.caps` into the new `Interp` (`run_isolate_thread` `:230`) before
  running the entry, runs one job, tears down. **Pooled path:** when `opts.caps` is absent, fall through to
  the existing pooled dispatch (`pool.rs:68`); add a `caps: CapSet` field to `WorkerRequest`
  (`isolate.rs:37` — `Send`, fits the all-`Send` invariant) carrying the **caller's floor**; `isolate_loop`
  (`:240`) installs the request's `CapSet` **FRESH at the top of each request** (request B unaffected by A)
  AND sets the per-`Interp` "drop-refused" flag (Task 4) so a pooled `caps.drop` is refused.
- [ ] **[SECURITY] Sendability:** a `ForeignPtr`/`ForeignLib`/`ForeignSymbol` is a `Native` handle and is
  already on the structured-clone non-sendable list — returning one from a worker is the existing
  `value of kind Native cannot be sent to a worker` Tier-2 error (assert it; no new airlock work).
- [ ] Green both configs; clippy. Review (the monotone argument: pooled never drops → writing the caller's
  floor grants nothing it had-and-lost; dedicated is single-tenant → its drop is terminal; the two regimes
  are disjoint). Commit.

## Task 9 — Capability denial end-to-end + DNS/io/os audit tests (Gate 10)
**Files:** `tests/cli.rs`, `tests/vm_differential.rs`, examples. **Tests:** integration.
- [ ] **[Gate 10 — DNS egress IS gated (security regression)]:** a program that `caps.drop("net")` (or runs
  under `--deny net` / `--sandbox`) then calls `net.lookup("example.com")` raises `capability 'net' denied`,
  **NOT** a resolved address list — proving the dispatch-site gate covers `"net"` (DNS) with no per-connect
  bypass. Companions: `os.networkInterfaces()`/`os.localIp()`/`os.hostname()` blocked by `--deny net`;
  `io.readAll()` blocked by `--deny fs`; ambient `os.platform()`/`os.cpuCount()` **NOT** blocked (still
  succeed under `--sandbox`).
- [ ] Per-scope denial tests: `--deny ffi` makes `ffi.open` raise `capability 'ffi' denied` (recoverable via
  `recover`); the `[capabilities]` manifest table; `caps.drop`. Monotonicity (after `caps.drop("net")`,
  `caps.has("net")` is false forever; no re-grant API). Granular fs-path / net-host allow-within-deny.
- [ ] **The keystone sandboxed-worker test:** `run_in_worker(plugin, input, {caps:{deny:["ffi","process"]}})`
  (a **dedicated** isolate) where `plugin` attempts `ffi.open` → the worker observes the denial; the host
  isolate is **unaffected** (still has `ffi`); the denial crossed the boundary. Plus the §4.5a pooled
  cap-leak regression from Task 8 driven from a script.
- [ ] **Four-mode byte-identity:** every FFI/caps example runs identically on tree-walker, specialized VM,
  generic VM, and `.aso`-compiled, in both feature configs (`vm_differential.rs`) — the denials are pure
  control flow, the deterministic libc/libm calls (`cos(1.0)`/`abs(-5)`/`strlen`) are pure functions.
- [ ] Green both configs; clippy. Review (Gate 10 DNS guard is present and asserts the *value* path is
  blocked, not just a flag). Commit.

## Task 10 — SP9 determinism seam: record/replay FFI calls + out-param fidelity
**Files:** `src/det.rs` (the FFI-call boundary recording/refusal), `src/stdlib/ffi.rs` (the seam hook),
`src/check/rules/` (the `ffi-nondeterminism` lint) + `rules::ALL` (`rules/mod.rs:30`). **Tests:**
`tests/check.rs` (lint) + unit in `ffi.rs`/`det.rs`.
- [ ] Failing tests: **outside** a determinism context FFI runs normally, SP9 inert, nothing changes (the
  `None` branch is byte-identical). **Inside** Record/Replay: a value-returning call records its marshalled
  return bytes; on Replay the bytes are returned **without re-executing the C side** (signature-match
  assert). **[SECURITY — out-param fidelity]** a C call that writes a `ffi.ptr` `Bytes` out-param and
  returns a status `int`: Record snapshots the **post-call** buffer contents; on Replay the recorded
  **post-call bytes are written back into the live `Bytes`** before control returns (NOT stale pre-call
  bytes — §7A). **[SECURITY — loud refusal]** a call with a `ForeignPtr` out-param OR a `ForeignPtr`
  **return** inside a determinism context is a **Tier-2 panic** (`ffi: call with a foreign-pointer out-param
  is not replayable in a workflow` / `ffi: pointer-returning call is not replayable in a workflow`) — never
  a silent wrong replay (§7B). The `ffi-nondeterminism` lint (default Warning) flags FFI calls inside a
  `workflow`/`activity` body.
- [ ] Implement: reuse a `DetEvent`/`BoundaryOutcome` shape (`det.rs:50,94`) for the marshalled-return-bytes
  case **plus** the post-call snapshot of every `ffi.ptr`-typed `Bytes` arg; add the `ForeignPtr`
  out-param / pointer-return refusal panics; add the `ffi-nondeterminism` rule to `rules::ALL`. Never hold a
  borrow across the call's await-free body; the seam is entered only when `Interp.determinism` is `Some`.
- [ ] Green both configs; clippy. Review (no silent gap — gate 6: every non-recordable path is a loud
  refusal, the buffer write-back is faithful). Commit.

## Task 11 — Examples + docs + NAV + repositioning
**Files:** `examples/ffi_libm.as`, `examples/advanced/ffi_struct.as`, `examples/caps_sandbox.as`;
`docs/content/stdlib/{ffi,caps}.md` + the `NAV` array in `docs/assets/app.js`; a capability section in
`docs/content/language/` (or the workers page); `README.md`; `CLAUDE.md`; `roadmap.md`; `goal.md`.
**Tests:** conformance + differential + `target/release/ascript run`.
- [ ] New examples (verified with `target/release/ascript run`, exercised by conformance/differential):
  `ffi_libm.as` (call `cos`/`pow` from libm — hermetic), `advanced/ffi_struct.as` (a struct out-param
  round-trip, fully error-handled), `caps_sandbox.as` (`run_in_worker` with a denied-`ffi` plugin — the
  keystone demo, fully error-handled).
- [ ] `docs/content/stdlib/ffi.md` (marshalling table, the three handles, the §3.5 threading caveat, the
  Tier-1/Tier-2 split) and `docs/content/stdlib/caps.md` (opt-out model, three scopes, the sandbox-via-
  worker keystone). **Add both slugs to `NAV` in `docs/assets/app.js`** (sidebar + cmd-K derive from `NAV`
  — no entry ⇒ unreachable; per the docs-nav gotcha). A capability cross-link section in
  `docs/content/language/`. `README.md` stdlib table (+ `std/ffi`, `std/caps`). `CLAUDE.md` "Native resource
  handles" gains the three Foreign kinds + a capabilities note. `roadmap.md` entry; `goal.md` FFI status tick.
- [ ] Review (in-content links resolve relative to the page dir; both NAV slugs reachable). Commit.

## Done when
Every task checked behind an independent review. The CAPS gate is the **single** `call_stdlib` dispatch site
(no per-function bypass); **[Gate 10]** `net.lookup` (DNS) is blocked by `caps.drop("net")`/`--deny net`/
`--sandbox` and the pooled cap-leak regression passes; **[Gate 4]** the three FFI handles add zero traced
edges; **[SP9]** out-param `Bytes` buffers replay faithfully and `ForeignPtr` out-params/returns are loud
refusals; `caps.drop` is irreversible on top-level/dedicated isolates and **refused** in a pooled `worker fn`.
Four-mode byte-identity holds in both feature configs; FFI tests are hermetic (libc/libm); clippy + tests
green in BOTH configs (`std/caps` core, `std/ffi` default-on feature); the grammar/parsers/tree-sitter/
formatter are **VERIFIED unchanged**; `ASO_FORMAT_VERSION` is **NOT** bumped; docs + NAV updated. Merge
`--no-ff` to `main`. (Depends on NUM merged — rebase onto `Int`/`Float` first.)
