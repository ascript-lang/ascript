# Record/Replay as a User-Facing Flagship — Design (REPLAY)

- **Status:** Draft for review
- **Date:** 2026-06-12
- **Code:** REPLAY (goal-perf.md, "Flagship & ecosystem track")
- **Depends on:** nothing unmerged. Builds entirely on shipped subsystems: SP9 determinism
  (`src/det.rs`, INERT by default), the FFI determinism seam (`src/stdlib/ffi.rs` §7), workflow
  event-sourced replay (`src/stdlib/workflow.rs`), the DBG DAP server (`src/dap/`), the worker
  airlock serializer (`src/worker/serialize.rs`), and the caps chokepoint discipline
  (`Interp::call_stdlib`, `src/stdlib/mod.rs:378`).
- **Coordinates with:** WARM Unit C (🏗️, unmerged) also refactors `det.rs`'s event-append sites
  into a `DeterminismContext::record_event` chokepoint. Whichever spec merges first physically
  introduces that single append chokepoint; the other rebases onto it (the goal.md
  "reconciliation 6" rule — never two sibling append paths).
- **Engines:** both. `det.rs` is engine-shared by construction — the VM holds an `Rc<Interp>`
  and routes every builtin through `Interp::call_builtin → call_stdlib`
  (`src/interp.rs:4563/6420`), and every seam accessor lives on `Interp`
  (`src/interp.rs:1330-1478`). Record/replay must be byte-identical across tree-walker,
  specialized VM, generic VM, and `.aso` — including **cross-engine** record-on-A/replay-on-B.
- **Breaking:** no. No grammar change, no `Value` variant, no opcode change, **no
  `ASO_FORMAT_VERSION` bump** (27 at time of writing, untouched). The trace file is a NEW,
  separately-versioned artifact. Zero-cost when off (the det `None` path is the shipped
  byte-identical default — `src/interp.rs:1382-1388`).

---

## 0. Read this first — code-vs-brief corrections (verified during drafting)

The roadmap entry and the campaign brief were grounded against the code. Five findings correct
or sharpen the brief; they are load-bearing for everything below:

1. **HTTP results are NOT plain data.** The brief's recommended v1 scope lists "http
   request/response" under "results are plain data". The code says otherwise:
   `http.get`/`request` return `[resp, nil]` where `resp` is a **`Value::Native(HttpResponse)`
   handle** (`src/stdlib/net_http.rs:8`), with the body read through accessor **methods**
   (`resp.text()`/`json()`/`bytes()`) and, in `{stream:true}` mode, a `Value::Native(HttpBody)`
   reader (`net_http.rs:56`). So recording http requires a **minimal handle virtualization**
   (§2.5) or scoping http out. We virtualize exactly two kinds (`HttpResponse` buffered +
   its plain-data accessor methods) and **refuse** streaming bodies/SSE/WS in v1 (§2.6).
2. **`--record` is a deterministic-mode run, not a passive observation.** Entering Record mode
   activates the existing seams: the RNG becomes the seeded PRNG, the wall clock becomes the
   `VirtualClock`, and `time.sleep` **does not sleep** — it advances the virtual clock
   instantly (`src/stdlib/mod.rs:771-794`; proven by
   `tests/determinism.rs::deterministic_sleep_advances_virtual_clock_without_real_delay`).
   A recorded run therefore differs from an unrecorded run in pacing and random values (both
   still valid). This is the shipped SP9 semantics and we keep it — it is also why **replay is
   fast** (§9). Documented prominently, never hidden.
3. **`ascript test` runs on the tree-walker by default.** `run_tests_serial`
   (`src/lib.rs:413-480`) loads files into one `Interp` via `load_module` — the tree-walker;
   only `--coverage` runs the VM (`run_tests_with_coverage`, `src/lib.rs:234`, "documented
   asymmetry"). Irrelevant to correctness (the seams live on `Interp`, shared by both engines)
   but it shapes the test-recording wiring (§4.3) and the cross-engine differential (§10.2).
4. **Replay's shipped fall-through-to-Record is wrong for a CLI replay.** `det.rs` Replay
   deliberately switches to Record at stream exhaustion (the workflow *crash-point* semantics,
   `src/det.rs:541-547`) and recovers best-effort on a kind mismatch
   (`replay_mismatch_recover`, `det.rs:526` — "the std/workflow detector is the authoritative
   guard"). A CLI `--replay` of a *complete* trace has no crash point: exhaustion and mismatch
   are both **divergence errors** and must be LOUD (the workflow `ctx_call_activity`
   non-determinism error is the model, `workflow.rs:615-627`). §7 adds a **strict** replay
   posture without touching workflow's semantics.
5. **Suspected pre-existing bug (Gate-14 — fix in-branch, failing test first):** bare
   `time.sleep` under a **Replay**-mode context unconditionally *appends* a `TimerSet`
   (`src/stdlib/mod.rs:779-789`) instead of *consuming* the recorded one at the cursor (only
   workflow's `ctx.sleep` consumes — `workflow.rs:562-576`). A workflow body that calls bare
   `time.sleep` between activities records a `TimerSet` that resume never consumes, so the next
   `ctx.call` finds a `TimerSet` at the cursor → a false "non-determinism" error. Verify with a
   failing record→resume test; the fix (consume-at-cursor in Replay, mirroring `ctx.sleep`) is
   prerequisite for strict CLI replay anyway (plan Task 1).

## 1. Summary & motivation

All the hard plumbing for record/replay is **shipped and INERT**:

- `src/det.rs` — `DeterminismContext { mode: Record|Replay, clock: VirtualClock, rng:
  SeededRng, cursor, events: Vec<DetEvent> }` per `Interp`
  (`determinism: RefCell<Option<…>>`, `src/interp.rs:549-558`), `None` by default — every seam
  takes its pre-SP9 path (`clock_now_ms`, `src/interp.rs:1382`: `None => real_now_ms()`).
- Seamed today: wall/monotonic clock (`time.now`/`time.monotonic`/`date.now` —
  `src/stdlib/mod.rs:520-527/755-770`), RNG (`math.random` — `math.rs:524`; `uuid.v4/v7`,
  `crypto.randomBytes` + password salts via `fill_seeded_bytes` — `interp.rs:1416`), durable
  sleep (`TimerSet`), **FFI** (`sym.call` records the marshalled return + post-call `Bytes`
  out-params, replays without re-invoking C; pointer returns/out-params are a LOUD refusal —
  `ffi.rs:635-735`, `DetEvent::FfiCall`), and the cross-isolate boundary events workflow uses
  (`ActorCall`, `GeneratorYield`).
- `std/workflow` proves the replay-mismatch UX: signature-pinned events
  (`signature_hash(name, args)`, `workflow.rs:120`), a loud divergence error with the event
  index and expected/got, and a persisted event log (newline-JSON, `events_to_log`/
  `log_to_events`).
- The DBG DAP server (`src/dap/`) gives us a debuggee thread that builds an instrumented VM
  and runs the program under editor-driven breakpoints, with clean re-launch teardown
  (`server.rs:373-385`) — exactly the machinery deterministic time-travel re-execution needs.

REPLAY turns this into the headline feature:

- **`ascript run --record t.trace prog.as`** — run deterministically, capture every
  non-deterministic input AND every effectful stdlib result into a portable trace file.
- **`ascript run --replay t.trace`** — re-execute byte-identically from the trace, performing
  **no real I/O**. A production failure or a flaky CI run becomes a deterministic local repro.
- **`ascript test --record`** — every FAILED test auto-saves a per-test trace under
  `.ascript-traces/`; **`ascript test --replay <trace>`** reruns that one test
  deterministically.
- **`ascript dap --replay t.trace` / `run --inspect --replay t.trace`** — replay-debugging:
  breakpoints and stepping over a deterministic re-execution, **plus `stepBack` and
  `reverseContinue`** (the rr model: time-travel by deterministic re-execution from the start —
  honest about the re-execution cost, §5).

The differentiating property vs. every mainstream scripting language: the determinism seams,
the airlock serializer, the structured-clone discipline, and the workflow mismatch detector
already exist and are battle-tested — REPLAY is integration + a file format + CLI/DAP surface,
not new runtime theory.

## 2. The core design question: recording effectful stdlib I/O

### 2.1 The verdict

**Extend `DetEvent` with a generic `StdlibCall` recorded at the `call_stdlib` chokepoint for a
curated, classified set of effectful functions** — the brief's recommended shape, confirmed
with two amendments forced by code evidence:

- (a) **Full fidelity for the existing seams** — clock, RNG, bytes, FFI, durable sleep —
  unchanged event kinds, now persisted to the trace. ✔ (as briefed)
- (b) **Record-at-the-RESULT-boundary** for the curated effectful set whose results are plain
  data: **fs (whole-file read/write/stat/readDir/walk/grep/exists/mkdir/remove/append), env
  (get/set/all), io (stdin reads), process.run (run-to-completion → plain
  `[{stdout,stderr,code}, err]`, `process.rs:375`), os.\* reads, net.lookup/lookupOne (DNS),
  time.sleep (skip-at-replay), and workflow.run/resume (its result is JSON-plain by
  construction — `is_serializable`, `workflow.rs:697`)** — PLUS **http with a minimal
  two-kind handle virtualization** (§2.5), because http results are handles, not plain data
  (§0.1). ✔ (amended)
- (c) **LOUD Tier-2 refusal on everything unseamed, in BOTH record and replay** — sockets
  (tcp/udp/ws), servers, SSE, streaming http bodies, `process.spawn`, sqlite/postgres/redis,
  tui/terminal, ai, telemetry-init, and **worker spawn** (§6). Refusing at record (not just
  replay) means a successfully-recorded trace is replayable **by construction** — the user
  never discovers non-replayability at replay time. The FFI pointer-refusal precedent
  (`ffi.rs` §7B) applies verbatim: never a silent wrong replay. Streaming/server/worker
  programs are v2, recorded in §11.

### 2.2 The chokepoint hook

`Interp::call_stdlib` (`src/stdlib/mod.rs:378`) is already the proven single dispatch root —
the FFI capability gate sits there precisely because "every OS-touching call is gated by
construction; there is no per-function path that can slip it" (`mod.rs:466-491`). The trace
hook is the same shape, immediately after the cap gate, before `match module`:

```rust
// REPLAY §2.2 — the trace hook. Gate-12: `trace_active` is a Cell<bool> snapshot
// (mirroring `caps_bits().all_granted()`, mod.rs:475) kept in sync by
// install/take/restore_determinism — ONE predictably-not-taken branch on the
// default path; no RefCell borrow when off.
if self.trace_active() {
    match replay_class(module, func) {
        ReplayClass::Recorded(shape) => {
            return self.trace_stdlib_call(module, func, args, shape, span).await;
        }
        ReplayClass::Refused => {
            return Err(AsError::at(
                format!(
                    "{}.{} is not supported under --record/--replay (no determinism \
                     seam; sockets/servers/streams/workers are v2) — see \
                     docs/tooling/record-replay",
                    module, func
                ),
                span,
            )
            .into());
        }
        ReplayClass::Seamed | ReplayClass::Harmless => {} // fall through to dispatch
    }
}
```

`replay_class(module, func) -> ReplayClass` is a **complete, central classification table**
exactly like `required_cap` (`mod.rs:325`), enforced by the same kind of completeness test
(`required_cap_complete_enumeration`, `mod.rs:991`): every entry of `STD_MODULES` must be
classified, and a new module fails the test until classified (§8). The four classes:

| Class | Meaning | Examples |
|---|---|---|
| `Seamed` | already routed through the det context; events flow without the hook | time, date.now, math.random, uuid, crypto random/salts, ffi.call |
| `Recorded` | result-boundary record/replay via `StdlibCall` (+ `NativeCall` for virtual handles) | fs, env, io, process.run, os, net.lookup, net_http requests, workflow.run/resume, time.sleep |
| `Refused` | loud Tier-2 in both modes | net_tcp/udp/ws, http_server, SSE/stream bodies, process.spawn, sqlite/postgres/redis, tui, ai, telemetry.init, caps.drop*, workers |
| `Harmless` | pure given inputs; runs for real in both modes | math (non-random), string, json, regex, schema, sync, lru, events, stream combinators, shared.freeze, … |

(*`caps.drop` under replay would diverge state the trace can't see; refused like the pooled
worker precedent, `caps.rs:894`. The audit task (§8) finalizes every per-function cell.)

`trace_stdlib_call` — Record: hash the call signature (`signature_hash(qualified_name, args)`,
the workflow precedent), dispatch the REAL call, encode the outcome, append
`DetEvent::StdlibCall`. Replay: consume the event at the cursor; verify
`(module, func, args_hash)`; mismatch/exhaustion → the strict divergence error (§7); decode
and return WITHOUT dispatching. A recorded `Err(Control::Panic)` replays as the same panic; a
recorded `Control::Propagate(v)` replays as the same propagation (the `ActorCall` panic-replay
precedent, `det.rs:87-91`).

### 2.3 The new event

```rust
/// REPLAY §2.3: one effectful stdlib call crossed the result boundary.
/// `args_hash` pins the call signature (the ActivityCompleted discipline) so a
/// code change that reorders effects is a detected divergence, never a silently
/// wrong value. `outcome` carries airlock structured-clone bytes (§2.4) — exact
/// Value fidelity, plain `Send` data, no Value/serde dep in det.rs (its standing
/// constraint: builds under --no-default-features).
StdlibCall {
    module: String,
    func: String,
    args_hash: u64,
    outcome: TraceOutcome,
},
/// One method call on a VIRTUALIZED native handle (§2.5).
NativeCall {
    /// The trace-scoped virtual handle id (assigned at birth, in event order).
    vid: u32,
    method: String,
    args_hash: u64,
    outcome: TraceOutcome,
},

pub enum TraceOutcome {
    /// Airlock-encoded result Value (the common case; plain data).
    Value(Vec<u8>),
    /// The canonical `[handle, err]` result whose handle is virtualized: the
    /// NativeKind tag, the assigned vid, and the handle's airlock-encoded plain
    /// `fields` map (status/headers/ok/url… — value.rs:487).
    Handle { kind_tag: u8, vid: u32, fields: Vec<u8> },
    /// The call raised a recoverable Tier-2 panic; replay re-raises it.
    Panic(String),
    /// The call returned `Control::Propagate(v)` (a `?` early return).
    Propagate(Vec<u8>),
}
```

### 2.4 Serialization: the worker airlock, NOT JSON (the verdict, with evidence)

Results are encoded with the **worker airlock serializer** (`worker::serialize::encode/decode`,
`src/worker/serialize.rs:423/608`), not `json::to_json_lossy`:

- **NUM fidelity.** JSON collapses the `Int`/`Float` subtype split — `5` and `5.0` round-trip
  identically through JSON but print differently and divide differently (CLAUDE.md NUM:
  "Float printing always shows a decimal"; `int/int` truncates). A JSON-coded replay would be
  observably WRONG on any program that branches on a recorded number. The airlock preserves
  the exact `Value` (Int/Float/Decimal/Bytes/Map/Set/EnumVariant payloads).
- **Precedent.** `DetEvent::ActorCall.result` is ALREADY airlock structured-clone bytes
  (`det.rs:81-91`) — the persisted workflow log embeds them as JSON number arrays. We follow
  that exact pattern; the trace file stores the bytes raw (it is binary, §3).
- **Free handle detection.** `encode` rejects non-sendable values with a field-path error
  (`check_sendable`, `serialize.rs:114-121`) — exactly the values that cannot be replayed
  (live native handles, closures, futures, generators). A `Recorded` call whose result fails
  to encode (and is not the §2.5 virtualized shape) is a LOUD record-time refusal naming the
  field path — never a lossy fallback.
- **Limit (recorded):** a result containing a `Value::Shared` frozen node encodes as
  `TAG_SHARED` + an `Arc` side-vector (`serialize.rs:107/423`) that cannot be persisted to a
  file. A `Shared`-carrying recorded result is a loud record-time refusal (rare by
  construction: `shared.freeze` is `Harmless`/pure, and no `Recorded` fn returns frozen data
  today). v2 may deep-materialize.
- `decode(bytes, &interp)` needs the `Interp` — available at the chokepoint. JSON keeps its
  one legitimate trace role: nothing (the workflow log keeps JSON for ITS file; the trace is
  binary).

`args_hash` reuses workflow's `signature_hash` (string-hash over `to_json_lossy` of each arg —
`workflow.rs:120-128`). Hashing may be lossy (it only PINS the signature; it never reproduces
values), so JSON-lossiness is acceptable there, and `Shared`/handle args hash fine
(`to_json_lossy` has SRV's `Shared` arms).

### 2.5 The handle problem, answered: minimal virtualization for HttpResponse

The hard case the brief names: a recorded call that returns a **resource id whose subsequent
reads must also be recorded**. The general answer (virtualize every `ResourceState`) is rr-scale
work — out of v1 scope. But http is the single most valuable effect for the flagship, and its
buffered shape is narrow:

- `http.get/post/put/delete/patch/head/request` (buffered mode, the default) return
  `[Native(HttpResponse), nil]` where the handle carries **plain readable `fields`**
  (`NativeObject.fields: IndexMap<String, Value>`, `src/value.rs:483-488` — status/ok/url/
  headers) and exposes **plain-data accessor methods** (`text()`, `json()`, `bytes()`,
  `json(Class)`) dispatched through `Interp::call_native_method` (`src/interp.rs:4883`).

v1 virtualization, scoped to exactly `NativeKind::HttpResponse`:

- **Record:** the chokepoint dispatches the real call; the result handle is assigned a
  trace-scoped `vid` (a `RefCell<HashMap<u64 /*resource id*/, u32 /*vid*/>>` on the trace
  state); the event stores `TraceOutcome::Handle { kind_tag, vid, fields }`. Each subsequent
  accessor-method call on that handle records a `NativeCall { vid, method, args_hash, outcome }`
  via a hook at the top of `call_native_method` — the **exact site and shape of the caps
  per-handle re-check** (`NativeKind::governing_cap` re-check, the FFI B3 fix), gated on the
  same `trace_active()` `Cell` flag.
- **Replay:** the chokepoint mints a fresh handle with the recorded `fields` and a new
  `ResourceState::ReplayVirtual { vid }` (one new `ResourceState` variant — the documented
  extension point, CLAUDE.md "Adding a stateful native API = a `ResourceState` variant").
  `call_native_method` on a `ReplayVirtual` handle consumes the next `NativeCall` event
  (vid+method+args_hash verified; divergence → §7 error) — the real http machinery is never
  touched. Member READS (`resp.status`) need no events: the fields were materialized at birth.
  `resp.close()` on a virtual handle is a no-op `Value::Nil` (recorded like any method).
- **Refused even within http:** `{stream:true}` (the `HttpBody` reader), `http.sse`,
  `http.cancelToken` raced against in-flight requests, and ws — streaming is v2 (§11). The
  refusal fires at the OPTION parse during record (loud, with the v2 pointer), so a recorded
  trace never contains a stream.

This is deliberately the **smallest possible virtualization** (one NativeKind, one
ResourceState variant, one extra event kind) that makes the flagship demo real: record a
program that calls a real API, replay it on a plane. The full handle-virtualization story
(files-as-handles, sockets, child processes) is recorded as the v2 design seed in §11 — v1's
`vid` + `NativeCall` machinery is forward-compatible with it.

### 2.6 GC + resources invariants (unchanged)

`ReplayVirtual` is inert state (no OS resource, no `Trace` — the native-resource rule,
`src/gc.rs`). Virtual handles reclaim by `Drop` like every `ResourceState`. No `resources`
borrow is held across an await in the hook (the take-out/return idiom is not even needed —
the replay path is synchronous once the event is consumed).

### 2.7 Interplay with workflow (and why it keeps working)

`workflow.run/resume` installs its OWN context and restores the previous one on finish
(`install_determinism` returns `prev` — `workflow.rs:467-478`, `interp.rs:1354`). Under
`--record`, the CLI trace context is the `prev`: the workflow's internal events go to the
WORKFLOW log (unchanged semantics, including its fall-through-to-record crash-point rules),
and the CLI trace records exactly ONE `StdlibCall` event — `workflow.run`'s plain-data
result at the boundary. Under `--replay`, `workflow.run` is consumed from the trace and never
executes (no workflow log writes during replay). The provenance flag (§7's `Origin`) is what
keeps the stdlib hook from firing inside a workflow-installed context. The workflow module is
otherwise **untouched** — its tests must pass unmodified (Gate guard).

## 3. The trace format

A new binary, versioned, length-prefixed, checksummed container — a sibling discipline to
`.aso` (`ASO_FORMAT_VERSION`, hostile-safe reader, fuzz target), NOT a change to it. New
module `src/trace.rs` (core — no serde; the det.rs constraint; builds under
`--no-default-features`).

```
magic            8  b"ASTRC\0\0\0"
version          u16 TRACE_FORMAT_VERSION = 1
header_len       u32
header           — seed u64 · start_ms f64 · kind u8 (0=run, 1=test)
                   · program path (len-prefixed utf8) · source sha256 [32]
                   · argv (count + len-prefixed strings)
                   · for kind=test: test name + the effective --filter string
                   · created_ms f64 · engine tag u8 (informational)
header_crc       u32 (crc32 over header bytes)
record*          — kind u8 · payload_len u32 · payload · crc32(kind‖len‖payload)
end              u8 0xFF · u32 event_count
```

- Every existing `DetEvent` variant gets a fixed binary encoding (the newline-JSON codec in
  `workflow.rs:133-365` stays as-is for the workflow log; the trace codec is separate and
  binary because `StdlibCall` outcomes are raw airlock bytes — base-N inflating them ~3× into
  JSON is exactly the `BytesRead` size note `det.rs:61-66` warns about).
- **Hostile-safe reader:** the trace is an attacker-writable file. Every length is
  bounds-checked against the remaining buffer (the P0 `.aso` clamp discipline); a bad crc, a
  truncated record, an unknown record kind, or an unknown version is a clean Tier-1 error
  ("trace file is corrupt/truncated at record N" / "trace version 2 is newer than this
  binary supports") — no panic, no unbounded allocation. **Fuzz target
  `fuzz/fuzz_targets/trace_roundtrip.rs`** follows the `aso_roundtrip` precedent verbatim
  (arbitrary bytes → `Ok|Err`, never panic/OOM/hang; seed corpus from real recorded traces
  over the corpus subset).
- **Write discipline:** events accumulate in memory and the trace is flushed at program end —
  **including the panic and `exit(n)` paths** (a failed run is precisely the one you want to
  replay; the workflow "always flush, even on error" rule, `workflow.rs:511-513`). Flush is
  atomic temp+rename (the `write_log` pattern, `workflow.rs:759`). Streaming/group append is
  the WARM-C coordination point (§0 header), not duplicated here.
- **Replay-side verification:** version check, header crc, then **source digest**: replaying
  a trace against a program whose sha256 differs is a clean error ("trace was recorded for a
  different program (source changed); re-record") — a changed program would otherwise consume
  a shifted stream and produce confusing mid-run divergence errors. `argv` is taken FROM the
  trace by default; explicitly-passed args that differ are the same clean error.

## 4. CLI surface

### 4.1 `ascript run`

```
ascript run --record <trace> [--seed N] [--tree-walker] file.as [args…]
ascript run --replay <trace> [--tree-walker] [file.as]
```

- `--record`: install `DeterminismContext::record(seed, real_now_ms())` with
  `Origin::CliTrace` on the run `Interp` before execution (the `enter_deterministic` shape,
  `interp.rs:1340`, which `lib.rs:678 run_source_deterministic` already models; start_ms =
  real time, the workflow record precedent, so recorded timestamps are real-looking). Seed
  defaults to OS entropy and is stored in the header; `--seed N` pins it (this also delivers
  the long-promised `--deterministic --seed N` seam noted in `det.rs:9` as a recorded
  side-benefit — `run --record /dev/null --seed N` is that mode; a dedicated alias is v2).
- `--replay`: read + verify the trace, install `DeterminismContext::replay(...)` with
  `Origin::CliTrace` (strict, §7). The program file argument is optional (the header carries
  the path); if given, the digest check still governs. **No real effectful I/O occurs**;
  `print` output streams live as normal (output is a derived value, not an effect event).
- Composition rules (all clean CLI errors, not silent precedence): `--record` + `--replay`
  together; `--replay` + extra script args that differ from the recorded argv; `--record/
  --replay` + `--profile` (untested matrix — v2); `--record/--replay` + `--coverage` n/a
  (run has no coverage). `--tree-walker` and `.aso` compose freely (engine-shared seams;
  the cross-engine differential is a Gate test, §10.2). `--inspect --replay` routes to the
  DAP server (§5); `--inspect --record` is refused v1 (record under a debugger pauses real
  I/O arbitrarily long — semantically fine but untested; v2).

### 4.2 Trace locations

Explicit paths for `run`. For `test --record`, auto-saved traces go under
**`.ascript-traces/`** in the CWD (project-local like `__snapshots__/`, attachable to a bug
report / CI artifact — NOT `$ASCRIPT_CACHE`, which is a content-addressed machine-local store
the user never browses; docs tell users to gitignore it). Name:
`<file_stem>__<test-name-slug>.trace` (slug = `[A-Za-z0-9_-]`, collisions suffixed `~2`).

### 4.3 `ascript test --record` / `--replay <trace>`

- `--record` (no path): each test FILE runs under one `Origin::CliTrace` Record context.
  Per-test traces are sliced from the file's event stream: the module-load prefix `P` is
  marked when `load_module` returns; each test marks its segment `S_k` (event indices noted
  before/after the test closure runs in `run_registered_tests_filtered`,
  `interp.rs:2724-2744`). **A trace is written ONLY for a failed test** (`P ⧺ S_k` + the
  test-kind header), keeping cost sane — a green run writes nothing. After the tally, the
  runner prints one line per saved trace:
  `  trace saved: .ascript-traces/orders__rejects_negative_total.trace (replay: ascript test --replay <path>)`.
- `--replay <trace>`: read the header (file, test name, recorded filter), run module load +
  exactly that test (an internal exact-name filter) under strict Replay. Pass/fail prints
  normally. The replay consumes `P` during load and `S_k` during the test.
- **Honest caveat (documented + detected):** a sliced per-test replay re-runs module load +
  one test. A test depending on a SIBLING test's seam effects diverges loudly (mismatch
  error); one depending on sibling in-memory state may simply behave differently — replay
  tells you the test is order-dependent, which is itself a finding. Documented in the docs
  page.
- Composition: `--record` with `--parallel` is a clean CLI error in v1 (per-isolate contexts
  are §6's refused territory); with `--coverage` allowed (coverage is observation-only and
  VM-side; the seams are engine-shared — covered by a test); with `--update-snapshots`
  allowed; `--watch --record` refused (unbounded trace accumulation; v2).
- **BATT coordination (reciprocal — BATT §10.3 carries the mirror note):** BATT's
  `test --seed/--frozen-time` installs per-test `DeterminismContext`s in the SAME
  `run_registered_tests_filtered` loop this section instruments, and the failure-line seed
  vocabulary is shared (`--seed N` means the same thing in both). Whichever spec lands second
  rebases onto the first's per-test install/marking seam.

## 5. Replay-debugging — the flagship (DAP time travel)

### 5.1 Threading replay into the debuggee

`ascript dap --replay <trace>` and `ascript run --inspect --replay <trace>` thread
`replay: Option<PathBuf>` through `dap::run_server` (`server.rs:277`) →
`launch::spawn_debuggee` (`launch.rs:34`) → `run_program` (`launch.rs:61`), which — after
building the `Interp` and before `vm.run` — reads/verifies the trace and installs the strict
Replay context (the same one-call install `run --replay` uses). Everything else about the
debuggee (instrumented `Vm`, break-on-entry patch, proto registration, capture sink,
`Terminated` shipping) is untouched. The debugged program performs **no real I/O** and all its
clock/RNG/effect values are pinned — so any number of re-executions reach byte-identical
states, which is the entire basis for time travel.

### 5.2 `stepBack` / `reverseContinue` = deterministic re-execution (the rr model)

DAP's `supportsStepBack` capability covers BOTH `stepBack` and `reverseContinue` (the DAP
spec ties them together) — we implement both, **advertised ONLY when a replay trace is
present** (the `initialize` response, `server.rs:315-335`, gains
`"supportsStepBack": replay.is_some()`; a non-replay session is bitwise-unchanged).

Mechanism — no checkpointing, no VM state capture, pure re-execution:

- The adapter keeps a session **navigation log**: the ordered list of commands that produce
  stops — the entry stop, then each resume command (`continue` / `next` / `stepIn` /
  `stepOut`) — interleaved with every `setBreakpoints` in arrival order. Stop index `k` is
  reached from a fresh launch by replaying the log's first `k` resume commands (breakpoint
  sets re-applied at their recorded positions).
- `stepBack` at stop `k`: tear down the current debuggee generation (the EXISTING re-launch
  teardown — resume, drop sender, join both threads, `teardown_session` + `reset_session`,
  `server.rs:373-385`), respawn the debuggee on the same trace, re-apply the navigation log
  through stop `k-1`, and report `stopped(reason:"step")`. The intermediate auto-driven stops
  are absorbed by the adapter (no `stopped` events emitted until the target stop).
- `reverseContinue` at stop `k`: same re-execution, target = the greatest stop index `< k`
  whose reason was a breakpoint hit (or the entry stop if none).
- **Honest cost, stated in docs and in the spec:** a backward step costs one re-execution of
  the program prefix — O(stops × prefix). Because replay does no I/O and sleeps are virtual,
  prefixes re-execute at full VM speed (§9); for the programs a debugger session handles this
  is interactive-fast, and rr's production experience validates the model. Checkpointing
  (periodic fork/snapshot to make backsteps O(1)) is the recorded v2 (§11).
- **v1 granularity:** stepBack lands at the previous STOP (breakpoint/step boundary), not the
  previous instruction — the brief's sanctioned scope. `evaluate` at any stop works unchanged
  (it reuses the tree-walker over the paused frame — DBG; the determinism context is live, so
  an `evaluate` that calls a Recorded fn would consume trace events and desync: `evaluate`
  expressions that hit the trace hook are REFUSED with a clean message in replay sessions —
  pure-value inspection, the overwhelmingly common case, is unaffected).
- **Output under time travel (documented):** the debuggee uses the Capture sink and ships
  output once at termination (DBG's documented v1 trade-off, `launch.rs:210-221`); a re-run
  generation that terminates ships its (byte-identical) output again. Editors render it as a
  fresh run's output; acceptable and documented.

### 5.3 What this buys

Record a failing production run (or let `test --record` capture it), then:
`ascript dap --replay crash.trace` → set a breakpoint at the suspicious line, run, **step
backwards** from the panic to the corruption point — with every http response, fs read, clock
value, and random draw exactly as they were. No mainstream scripting language ships this
out of the box; it is the campaign's "surprisingly capable" pillar made concrete.

## 6. Worker isolates: refuse in v1 (verdict + evidence)

Determinism contexts are **per-`Interp`** (`interp.rs:549-558`), and isolates build a FRESH
`Interp::new()` on their own thread (`worker/isolate.rs:301`) — a CLI trace context on the
main isolate does not, and cannot, extend across the airlock. The options were per-isolate
trace files vs. refusal; the evidence picks refusal:

- **Pooled `worker fn` isolates are REUSED across calls** (the documented reason `caps.drop`
  is refused in a pooled worker — `caps.rs:894`, FFI §4.5a). Per-isolate traces would
  interleave events from unrelated calls of the same program (or different call sites)
  nondeterministically — there is no stable per-isolate stream identity to replay against.
- **Dedicated isolates (actors/streams/`run_in_worker`)** run module top-level code and own
  real resources isolate-side; replaying "without spawning" requires the boundary events
  (`ActorCall`/`GeneratorYield` — which exist, built for workflow) **plus** refusing in-isolate
  effects — a half-recorded hybrid with real side effects at spawn. Not loud, not clean.
- Cheapness is therefore NOT shown; the brief's default holds. **v1: any isolate-creating
  operation under `Origin::CliTrace` (record OR replay) is a clean Tier-2 refusal** — pooled
  `worker fn` dispatch, `spawn()` of a `worker class` (`spawn_actor`, `interp.rs:2299`),
  `worker fn*` stream creation (`interp.rs:2548`), and `run_in_worker` (`interp.rs:6035`) —
  one shared guard helper, message modeled on the pooled-caps refusal. `test --record
  --parallel` is the CLI-level face of the same rule (§4.3).
- **Workflow is unaffected** (its own context, its own log, §2.7) — including workflow's
  recorded ActorCall/GeneratorYield interop, which is workflow-scoped and stays as shipped.
- v2 (recorded, §11): one trace per isolate (`t.trace`, `t.trace.w1`, …) with spawn events
  binding child traces — the design seed is the existing boundary events.

## 7. Strictness, provenance, and the divergence error

`DeterminismContext` gains two fields (plain data, no new deps):

```rust
pub enum Origin { Workflow, CliTrace }   // who installed the context
pub struct DeterminismContext {
    pub origin: Origin,        // default Workflow (every existing constructor) — the
    pub strict: bool,          // stdlib hook fires only for CliTrace; strict replay
    /* …existing fields… */    // disables fall-through-to-Record + best-effort recover
}
```

- **Workflow contexts are bit-for-bit unchanged** (`origin: Workflow, strict: false` — the
  existing constructors keep today's semantics; every existing det.rs/workflow test must pass
  unmodified).
- **Strict replay** (`CliTrace` + `Mode::Replay`): exhaustion at any seam and any kind/
  signature mismatch sets a `pending_divergence: Option<Divergence>` on the context instead of
  the best-effort `replay_mismatch_recover` / `switch_to_record_*` paths
  (`det.rs:526/541`). The infallible-signature seams (`clock_now_ms() -> f64` etc.) cannot
  return an error, so the divergence is RAISED at the nearest fallible chokepoint:
  `call_stdlib` checks `take_divergence()` after every Seamed/Recorded dispatch (one cheap
  check, only when `trace_active()`), and the trace hook itself raises directly for
  `StdlibCall`/`NativeCall`. Error format (the workflow model, with the brief's required
  index + expected/got):

  ```
  replay divergence at event 412 of 1093 (trace .ascript-traces/orders__x.trace):
    expected: fs.read("config.toml")  [recorded args#9f31c2]
    got:      fs.read("config.json")  [args#0b77ee]
  the program's effect order differs from the recording — re-record, or check for
  unpinned nondeterminism (task interleaving, sibling-test state)
  ```
- **The scheduling residual, stated honestly:** task interleaving is NOT pinned (tokio is not
  replaced — SP9 §3.6, the named 2b residual; deterministic task scheduling is an M17
  architectural non-goal). A single-task program replays exactly. A concurrent program whose
  seam-event ORDER depends on real I/O completion order may diverge at replay — and is
  **detected loudly by the signature check**, never silently wrong. Recorded calls complete
  inline at replay, so replay-side ordering follows program order deterministically; it is the
  RECORD side that can capture a race. v2 may tag events with task identity. The docs page
  carries this limitation verbatim.

## 8. The determinism audit — coverage table (plan Task 9 produces/verifies this)

Every stdlib module is classified in `replay_class`, with a completeness test forcing
classification of every `STD_MODULES` entry (the `required_cap_complete_enumeration` +
FFI-holistic "completeness sweep" precedents). The audit sweep verifies each cell against the
code (not assumption); known nondeterminism sources found during drafting:

| Source | Evidence | Class |
|---|---|---|
| wall/monotonic clock, `date.now` | seams shipped (`mod.rs:520/755`) | Seamed |
| `math.random`, uuid v4/v7, crypto random/salts | `fill_seeded_bytes`/`next_seeded_f64` | Seamed |
| `time.sleep` | virtual-clock advance (`mod.rs:771`) + §0.5 fix | Seamed (replay consumes + skips delay) |
| `ffi sym.call` | `ffi.rs:635` (ptr returns refused) | Seamed |
| fs read/write/stat/exists/grep | plain results (`fs.rs`) | Recorded |
| `fs.readDir`/`walk` **OS-order, unsorted** | `fs.rs:179` (no sort) | Recorded (replay faithful; order documented as platform-dependent outside replay) |
| env get/set/all, `env.args` | plain | Recorded (argv also pinned in header) |
| `process.run` | plain `{stdout,stderr,code}` (`process.rs:375`) | Recorded |
| `os.*` (cpuCount, memory, disks, hostname, pid, uptime, cpuUsage) | sysinfo reads | Recorded |
| `net.lookup/lookupOne` (DNS) | plain | Recorded |
| http buffered requests + response accessors | §2.5 virtualization | Recorded |
| `workflow.run/resume` | plain result (`is_serializable`) | Recorded |
| `num_cpus` (pool sizing), `$ASCRIPT_WORKERS` | only reachable via workers | Refused-by-construction (§6) |
| sockets/servers/ws/sse/stream-bodies, `process.spawn`, sqlite/postgres/redis, tui, ai, telemetry.init | live handles / streams | Refused |
| `intl`/locale | audit verifies: pure given args (bundled data) vs system-locale reads | Harmless expected; audit decides per-fn |
| Object/Map/Set iteration order | insertion-ordered `IndexMap` (`value.rs`, CLAUDE.md) | Harmless (deterministic) |
| hash randomization | SipHash keys are process-internal; no iteration order exposed beyond IndexMap | Harmless (audit confirms no exposed ordering) |
| task interleaving | SP9 §3.6 residual | NOT pinned — documented + divergence-detected (§7) |
| GC timing | no observable hooks | Harmless |

The table (finalized per-function by the audit task) is reproduced in
`docs/content/tooling/record-replay.md` as the user-facing "what replays / what's refused /
what's pure" reference.

## 9. Performance

- **Zero-cost when off (Gate 12/17).** The default path is the shipped INERT `None` —
  `clock_now_ms`'s `None => real_now_ms()` (`interp.rs:1382-1388`) and friends are untouched.
  The ONLY addition to the default path is the `trace_active()` `Cell<bool>` check in
  `call_stdlib`/`call_native_method` — the same single-`Copy`-flag pattern as the caps
  `all_granted()` short-circuit (`mod.rs:475`), sitting beside it. Proven by re-running the
  DBG zero-cost gate + the spec/tw ≥2× floor (`tests/vm_bench.rs`) and a same-session A/B in
  `bench/REPLAY_RESULTS.md` (Gates 16/17).
- **Record overhead: measured, reported, not promised.** Per Recorded call: one signature
  hash + one airlock encode + one in-memory event push (flush at exit). Expectation: small
  relative to the real I/O it brackets; the bench report measures record-vs-plain on an
  fs/process/http-mock workload + peak RSS (Gate 18 — traces buffer in memory; the report
  states the memory profile honestly; the `BytesRead` size-note discipline applies).
- **Replay is FAST — a selling point.** Replay performs no network/disk I/O, `time.sleep` is
  virtual (instant — §0.2), and recorded calls return decoded bytes inline. A recorded
  60-second integration run replays in however long the pure compute takes. CI re-runs and
  the §5 time-travel re-executions both bank on this; the bench report includes a
  record-vs-replay wall-time line to headline it.

## 10. Correctness gates (goal.md 1–14 + goal-perf 15–18 apply verbatim)

### 10.1 Tests (the load-bearing batteries)

- **Record→replay round-trip equality** over a corpus subset: programs exercising clock, RNG,
  uuid, crypto salts, fs, env, process.run, http (against a local `std/server` started by the
  TEST harness process, not the recorded program — the `tests/cli.rs` spawn precedent), and
  FFI (the libm fixture, the `ffi.rs` determinism-test precedent). Assert: recorded-run
  output == replayed-run output byte-for-byte, AND replay touched no real effect (e.g. delete
  the fs fixture / stop the http server before replaying — replay must still succeed).
- **Cross-engine differential (Gate 1 extension):** record on tree-walker → replay on VM
  (specialized AND generic) and vice versa; record `.as` → replay `.aso` — all byte-identical.
  This is the proof `det.rs` is engine-shared, asserted not assumed.
- **Mismatch detection:** replay against an edited program (digest check), a truncated trace,
  a reordered-effect program (same digest via a conditional on env — args-hash divergence),
  wrong-kind events — each a clean, indexed divergence error, exit non-zero, no partial wrong
  output beyond the divergence point.
- **Refusal battery:** every `Refused` row of §8 attempted under record AND replay → the
  Tier-2 message; worker spawn refusals at all four sites; `test --record --parallel` CLI
  error; `caps.drop` refusal.
- **Trace-format fuzz:** `fuzz/fuzz_targets/trace_roundtrip.rs` (the `aso_roundtrip` model)
  + the in-suite planted-bug guard (`tests/property.rs` precedent) so the "never panics on
  hostile bytes" claim has in-session evidence.
- **Workflow non-regression:** the entire workflow + determinism suites pass UNMODIFIED; a
  workflow inside a recorded run round-trips (§2.7); the §0.5 bare-sleep fix has a
  failing-test-first regression guard.
- **DAP replay:** `tests/dap.rs` extension — a scripted session over a recorded trace:
  breakpoint, continue, stepBack lands at the prior stop with identical frames/variables;
  `supportsStepBack` advertised only with `--replay`; evaluate-refusal on Recorded calls.

### 10.2 Standing gates

Four-mode byte-identity untouched (the feature is flag-gated; `vm_differential` unchanged);
clippy + tests green BOTH configs (`trace.rs`/`det.rs` changes are core — must build under
`--no-default-features`; the http virtualization is `net`-gated, fs recording `sys`-gated —
the classification table itself is feature-independent data like `required_cap`); no borrow
across await; zero corpus `type-*` diagnostics; docs gates (§ below); production-grade
mandate (any bug found — §0.5 is already one — fixed in-branch, failing test first).

## 11. Scope & rejected alternatives

**In scope (v1):** the `StdlibCall`/`NativeCall` events + airlock outcome encoding; the
`call_stdlib` + `call_native_method` hooks behind a `Cell` flag; the `replay_class` complete
table + completeness test; HttpResponse-only handle virtualization; strict replay +
divergence errors + `Origin` provenance; the `ASTRC` trace format + hostile-safe reader +
fuzz target; `run --record/--replay/--seed`; `test --record` (failure-only, per-test sliced,
`.ascript-traces/`) + `test --replay`; DAP replay with `stepBack`/`reverseContinue` by
re-execution; the worker-spawn refusal; the determinism audit + table; the bench report; the
§0.5 bug fix; docs page + NAV + cli/test docs.

**Out of scope / v2 (recorded, never silent):**
- **Streaming/server/worker recording** — `HttpBody`/SSE/WS/sockets/`process.spawn`/server
  accept loops, and per-isolate trace files with spawn-bound child traces (§6). The `vid` +
  `NativeCall` machinery and the shipped `ActorCall`/`GeneratorYield` events are the design
  seeds.
- **Replay checkpointing** (O(1) backsteps via periodic snapshots) — §5.2's re-execution
  model is the rr-validated v1; checkpointing is a pure optimization recorded for when trace
  lengths demand it.
- **Task-identity-tagged events** (pinning concurrent interleavings) — §7's residual.
- **`--deterministic --seed N` as a first-class alias**, `--inspect --record`,
  `--profile --record`, `--watch --record`, REPL recording — each refused cleanly in v1.
- **Deep-materializing `Shared` results** (§2.4 refusal).

**Rejected:**
- **JSON for outcome encoding** — collapses Int/Float (an observable NUM divergence at
  replay), inflates bytes ~3×, and re-implements what the airlock already guarantees (§2.4).
- **Recording at the OS-syscall layer (true rr)** — wrong altitude for a managed runtime:
  the stdlib result boundary is portable, value-faithful, and already chokepointed; syscall
  capture is platform-specific and breaks the worker/GC invariants for zero user benefit.
- **General handle virtualization in v1** — rr-scale scope; one kind ships the flagship,
  the rest is evidence-gated v2 (§2.5).
- **Lenient replay (fall through to live I/O on mismatch)** — the explicit anti-goal; the
  FFI §7B refusal precedent and pillar 1 forbid a silent wrong replay (§7).
- **Recording in `--parallel` test isolates / pooled workers v1** — no stable per-isolate
  stream identity (§6 evidence); refusal is the honest v1.
- **Reusing the workflow newline-JSON log as the trace format** — text JSON cannot carry
  airlock bytes efficiently and has no crc/versioning; the workflow log keeps its format,
  the trace gets a real container (§3).

## 12. Grounding (verified during drafting)

- `src/det.rs` — `DetEvent` (50-114, incl. `FfiCall`/`ActorCall` byte payload precedents),
  `DeterminismContext` (241+), Replay fall-through (`switch_to_record_clock`:541) and
  best-effort recover (:526) — the semantics §7 makes strict for `CliTrace` only.
- `src/interp.rs` — `determinism` field (549-558), seam accessors (1330-1478, short borrows,
  never across await), `enter_deterministic` (1340), `spawn_actor` (2299), worker-stream
  spawn (2548), `call_native_method` (4883), `call_builtin → call_stdlib` (4563/6153/6420 —
  the engine-shared route), `run_in_worker` (6035).
- `src/stdlib/mod.rs` — `call_stdlib` (378), the caps gate + `all_granted` short-circuit
  (466-491, the Gate-12 pattern §2.2 copies), `required_cap` (325) + completeness test (991),
  `date.now` seam (520-527), time/sleep seam (755-794, incl. the §0.5 suspect).
- `src/stdlib/workflow.rs` — context install/restore (467-478), signature hash (120),
  mismatch error (615-627), `is_serializable` (697), `write_log` atomicity (759), the
  JSON log codec (133-365).
- `src/stdlib/ffi.rs` — the INERT seam (635-638), pointer refusals (§7B tests ~1755+).
- `src/stdlib/net_http.rs` — response/body/SSE handle shapes (8/56/68, register sites
  1397-1416); `src/value.rs` — `NativeObject.fields` (483-488), `NativeKind` (492).
- `src/worker/serialize.rs` — `encode`/`decode` (423/608), non-sendable field-path rejection
  (114-121), `TAG_SHARED` side-vector (107) → the §2.4 `Shared` refusal.
- `src/worker/isolate.rs` — fresh per-isolate `Interp::new()` (301), caps install +
  drop-refusal flag (341-342); `src/stdlib/caps.rs:894` — the pooled-refusal message model.
- `src/dap/` — threading model (mod.rs doc), `spawn_debuggee`/`run_program`
  (launch.rs:34/61), `run_server` (server.rs:277), `initialize` capabilities (315-335),
  re-launch teardown (373-385), capture-sink output trade-off (launch.rs:210-221).
- `src/lib.rs` — `run_source_deterministic` (678), `run_tests_serial` tree-walker path
  (413-480), `run_tests_with_coverage` VM asymmetry (218-234), `run_tests_parallel` (525),
  run entry points (1679/2024); `src/interp.rs` `run_registered_tests_filtered` (2724).
- `src/main.rs` — `Run`/`Test`/`Dap` clap surfaces (155-261, 605-776, 1020-1204).
- `tests/determinism.rs` (the seed oracle + virtual-sleep proof), `tests/dap.rs`,
  `fuzz/fuzz_targets/aso_roundtrip.rs` (the fuzz model), `src/pkg/cache.rs` (`$ASCRIPT_CACHE`
  — considered and rejected for trace storage, §4.2).
- `docs/assets/app.js` NAV tooling section (54-57) — the new page slots there.
- External: rr (record once, replay deterministically, reverse-execute by re-running —
  the §5 model); Temporal/Restate (event-sourced replay + signature pinning — the shipped
  workflow model REPLAY generalizes); DAP specification (`supportsStepBack` covers both
  `stepBack` and `reverseContinue` — §5.2 implements both or advertises neither).
