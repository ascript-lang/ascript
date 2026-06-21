# Goal — Performance & Memory Campaign (PERF: the pay-for-what-you-use engine)

Take AScript from a proven-correct interpreter to **performance leadership among dynamic
languages** — an engine where programs pay only for the effects they actually use: the async
machinery only when code suspends, refcount/GC traffic only for values that escape, contract
checks only at unproven boundaries, dispatch overhead shrunk by representation and pre-decoding,
and multi-core throughput delivered through the shipped isolate + frozen-heap model. The end
state is a language that is **genuinely great: performant, surprisingly capable, and a beautiful
developer experience** — without ever trading away the correctness discipline that got us here.

**This is a multi-spec campaign**, not one feature. Each item below is a standalone design spec +
implementation plan, executed in dependency order, each merged on its own feature branch off
`main` behind an independent review gate — exactly the cadence of the Serious Language campaign
(`goal.md`, 12/13 merged; this campaign is its successor and inherits its rules wholesale).
Backward compatibility is not a constraint (pre-1.0); observable *behavior* identity across all
engine modes **is** — byte-for-byte, always.

## Evidence base (read this before any spec — optimization is justified by measurement)

The campaign order is dictated by `bench/PROFILING_RESULTS.md` (Phase-0 profiling) plus
code-confirmed constant factors. The load-bearing facts:

| Workload | Dominant cost | VM dispatch share |
|---|---|---|
| `async_inline` (400k trivial async calls) | **async runtime 78%** (kevent/reactor park 55%, tokio abort+ref_dec+notify+SharedFuture ~12%) | 9% |
| `async_concurrent` (200k gathers ×4) | **async runtime 71%** | 5% |
| `json_roundtrip` | **allocation 38%**, hashing 11% (SipHash), gc/refcount 6% | 12% |
| `object_churn` (tight loop) | **dispatch/VM 49%** (run_loop 18%, Fiber::frame 9%, push/pop 6%), alloc 22%, hashing 13% | 49% |
| `workflow_loop` | **fsync 96%** (`F_FULLFSYNC` 82%) | <1% |

Code-confirmed constant factors (verified 2026-06-12, all still present):

- **≥3 heap allocations per call**, even for a function that captures nothing: the cells vector
  (`alloc_cells`, `src/vm/fiber.rs:56` — `vec![None; slot_count]` on EVERY frame), a fresh
  `Cc<RefCell>` per captured slot, and the `Vec<Value>` argument collection.
- **A full fiber + boxed async future per CALLBACK ELEMENT** in higher-order builtins:
  `arr.map(f)` runs `f.clone()` + `vec![item]` + `call_value(..).await` → `check_call_args` →
  new `Fiber` → `grow_future(self.run(&mut fiber)).await` for **every element**
  (`src/stdlib/array.rs:58`, `src/vm/run.rs:3704`).
- **Variable-width operand decode per instruction** through `Op::operand_width` matches
  (`src/vm/opcode.rs:788`); no pre-decoded representation exists.
- **Objects hash on construction** despite shapes: storage is `IndexMap` (SipHash), literal keys
  are hashed at runtime although they are statically known, and `resync_object_shape` clones every
  key into a fresh `Vec<String>` (`src/vm/run.rs:4526`).
- **Every call pays `check_call_args` contract validation** (`src/vm/run.rs:3656`), even when the
  static checker could prove the site safe.
- `Value` is 24 bytes (`src/value.rs` size assertion); scalars are inline; heap kinds are `Rc`/`Cc`.

**Measurement mandate (Phase 0 of this campaign, executed as LANE plan Task 0):** the profiling
corpus has a blind spot — no functional-idiom (map/filter/reduce pipelines), call-heavy, or
server-request workloads, which is exactly where the confirmed constant factors live. Before any
engine change merges, `bench/` gains those workloads and a same-session A/B harness; every spec's
headline number is measured with the **shipped profiler** (`ascript run --profile cpu`) and
recorded in a `bench/*.md` report. A suspected cost gets a corpus workload BEFORE its fix, so
every change has a before/after number. No speedup is ever promised in a spec — expectations are
stated, results are measured.

## The four pillars (inherited verbatim from `goal.md` — non-negotiable)

1. **No bugs.** The tree-walker stays the permanent byte-identical differential oracle; every new
   engine configuration joins `tests/vm_differential.rs` as a mode AND the differential fuzzer as
   an axis. Fix the engine, never relax the assertion.
2. **Developer experience.** Tooling (LSP, fmt, REPL, doc, DAP, profiler) tracks every change;
   diagnostics stay excellent; docs staleness is a campaign-blocking defect.
3. **Language capabilities.** Nothing in this campaign changes surface syntax or semantics —
   except where a spec explicitly says so (none do). Performance is never bought with semantics.
4. **Performance.** Evidence-ordered: measured bottlenecks first, constant factors second,
   speculative compilation last. Zero-cost-when-off for every new counter/cache/seam, proven by
   benchmark (the Gate-12 discipline DBG established at geomean ≈1.0×).

## The specs (dependency-ordered; codes are stable references)

> **Status legend:** ⬜ spec not written · 📝 spec drafted · 🔒 spec locked (reviewed) · 🏗️ plan
> written · 🟡 in progress · ✅ merged. Update this table as the single source of truth.

### Foundation — the async & call tax (the measured #1 and the largest constant factors)

- ✅ **LANE — Two-lane fiber engine + inline ready-future completion.** A synchronous dispatch
  driver (`run_loop_sync`, a plain non-async fn) executes the suspension-free opcode subset over
  the SAME `Fiber` state; the existing async `run_loop` becomes the orchestrator that bursts into
  the sync lane and takes over only at genuine suspension points (`Await` on a pending future,
  async-fn scheduling, generator resume, `Import`, async-native stdlib, `Op::Break`, and
  `maybe_yield_for_inflight` when in-flight tasks exist). `await` on an already-completed future
  takes the value inline with no reactor round-trip. Because the `Fiber` externalizes ALL
  execution state (frames/ip/stack, `src/vm/fiber.rs:71`), lane-switching is just choosing which
  driver polls — no OSR, no metadata. Includes Phase-0 bench-corpus extension (Task 0).
  - Spec: `superpowers/specs/2026-06-12-two-lane-engine-design.md`
  - Plan: `superpowers/plans/2026-06-12-two-lane-engine.md`

- ✅ **CALL — Call-path allocation diet + higher-order callback trampoline.** Remove the ≥3
  allocations/call: empty-`cell_slots` fast path (no cells vector when nothing is captured
  by-reference), argument passing via the operand-stack window instead of `Vec` collection where
  the call shape allows, frame/fiber pooling. The trampoline: higher-order builtins
  (map/filter/reduce/sort-comparator/each) detect a non-async, non-generator callee and drive all
  elements through ONE reused fiber on the sync driver — no per-element `Vec`, no boxed future, no
  fresh fiber — with per-element escalation fallback to the async path. Depends on LANE (driver).
  - Spec: `superpowers/specs/2026-06-12-call-path-diet-design.md`
  - Plan: `superpowers/plans/2026-06-12-call-path-diet.md`

### Representation — where allocation & hashing go to die

- ✅ **SHAPE — Shape-native object storage + interior hashing.** Shapes stop being an id beside
  an `IndexMap` and become the OWNER of the key→index layout; object/instance storage becomes a
  flat values slab. Object literals get compile-time-precomputed shape ids (zero hashing at
  construction); `resync_object_shape` loses its key-clone. Interior hash tables that never see
  user-controlled DoS surface (ShapeRegistry, IC maps, scope maps) move from SipHash to a fast
  hasher; `Map`/`Set` keep DoS-resistant hashing (documented decision). Megamorphic fallbacks
  preserve today's semantics exactly (insertion order, deletion, dynamic keys).
  - Spec: `superpowers/specs/2026-06-12-shape-storage-design.md`
  - Plan: `superpowers/plans/2026-06-12-shape-storage.md`
  - **MERGED to `main` (`--no-ff`).** See EXECUTION LOG. NANB is now unblocked.

- ⚖️ **NANB — 16-byte two-word `Value` — EVIDENCE-REJECTED (Phase 1 seam SHIPPED).** The
  representation endgame VAL §3.2 sanctioned but parked. **Outcome:** Phase 1 (the sealed `pub
  struct Value(ValueRepr)` + `ValueKind`/`OwnedKind` view seam) is **MERGED to `main`** — proven
  zero-cost (geomean spec/tw 4.07× == pre-NANB baseline 4.00×), size unchanged at 24 B,
  `ASO_FORMAT_VERSION` 28. The 16-byte `value16` repr (Phases 2–3: `ThinStr` + `cfg(value16)`) was
  built, proven behavior-invisible (cross-binary 110/110 byte-identical, four-mode 444/0 ×2 configs,
  300k-case deep fuzz 0 divergence, Miri-clean) and measured same-session — then **evidence-REJECTED
  by the reviewer-of-record against the fixed §8.1 SHIP criteria:** time geomean **1.005× spec** (bar
  ≥1.02× — rides noise, FAIL), peak RSS **1.001× / flat** (bar ≥5% improvement, FAIL), STRING-subset
  geomean not isolated (unconfirmable). Mirrors the prior thin-`Str` reject
  (`bench/COMPACT_VALUE_RESULTS.md`). The `value16` repr stays frozen+flagged on `feat/value16` as
  the cheap re-run path. The repr-independent decimal-overflow fix found by the fuzz campaign landed
  separately on `main`. Verdict + numbers: `bench/NANB_RESULTS.md` "Phase 4". (8-byte NaN-box —
  inline `float`, tagged inline `int`, tagged pointers — remains the double-gated future endgame,
  unattempted.) Depended on SHAPE ✅.
  - Spec: `superpowers/specs/2026-06-12-nan-boxing-design.md` (§8.1 verdict appended)
  - Plan: `superpowers/plans/2026-06-12-nan-boxing.md`

### Dispatch — decode once, fuse what the data says, inline what guards allow

- ✅ **DECODE — Pre-decoded instruction stream + data-driven superinstructions (Units A+B);
  speculative inlining + TOS cache evidence-dropped.** **MERGED to `main` (`--no-ff`, `9a4cd76`).** A
  per-`FnProto`, lazily-built side representation (fixed-width op records, operands widened, jump
  targets pre-resolved) following the `arith_cache` side-table precedent — `Chunk.code` stays
  byte-identical (disassembler/goldens/differential untouched); `Op::Break` byte-patching
  INVALIDATES the decoded cache via the `patch_epoch` chokepoint (the same invalidation story a
  future JIT needs — built and tested here first; THE primary recorded purpose). Superinstruction
  selection is empirical: fusion pairs chosen from the committed op-pair census over the bench
  corpus, never guessed. **Unit C (speculative global-fn inlining) and Unit D (TOS register cache)
  were EVIDENCE-DROPPED** at the Task-11 gate (inline +0.45% < 2%; TOS −1.6%, object_churn −3.2%) —
  reverted, not shipped. The owner SHIPPED Units A+B default-on accepting a ~2.3% whole-program
  regression (the invalidation contract is the value, `ASCRIPT_NO_DECODE` is the escape hatch).
  Depends on LANE (the sync driver is the primary consumer). See EXECUTION LOG.
  - Spec: `superpowers/specs/2026-06-12-decoded-dispatch-design.md`
  - Plan: `superpowers/plans/2026-06-12-decoded-dispatch.md`

### Types that pay you back

- ✅ **ELIDE — Contract elision via static proof. MERGED to `main` (`--no-ff`). See EXECUTION LOG.**
  When the TYPE checker statically PROVES a
  call site's arguments satisfy the callee's annotations (or a field assignment its schema), the
  compiler emits an unchecked call/store; checks remain at every unproven (gradual) boundary —
  sound gradual typing where annotations BUY performance (the loop TypeScript/Sorbet structurally
  cannot close; Static Python/Cinder precedent). Both engines elide identically — the VM via
  `Op::CallElided` + skipped `Op::CheckLocal` + `proto.ret=None`; the tree-walker via a
  per-module AST marking pass (`Call.elide_args` / `Stmt::Fn.ret=None`) — so elision is
  OBSERVABLY invisible by construction (a program that passes checks behaves identically; one
  that would fail them is, by proof, unreachable — the elide-on vs elide-off differential axis
  + paranoid mode enforce this). `--no-elide` / `ASCRIPT_NO_ELIDE=1` kill switch; default-OFF
  opt-in via `--elide` / `ASCRIPT_ELIDE=1`. `--no-elide` kill switch mirrors `--no-specialize`.
  - Spec: `superpowers/specs/2026-06-12-contract-elision-design.md`
  - Plan: `superpowers/plans/2026-06-12-contract-elision.md`

### Multi-core — the ×cores lever (from shipped pieces)

- ✅ **PAR — Data-parallel primitives over the worker pool. MERGED to `main` (`--no-ff`). See EXECUTION LOG.**
  `task.pmap(arr, f)` /
  `task.preduce(arr, f, init)` (std-lib, no syntax): chunk an array across the worker pool, run
  the callback in isolates, merge results. **Input: frozen array → `Arc`-bump zero-copy (TAG_SHARED
  airlock); plain array → per-chunk structured-clone copy. No auto-freeze** — freeze-or-copy is
  explicit and the shipped decision. Non-sendable callbacks are the existing field-path panic. Builds
  entirely on `src/worker/` + `std/shared` + the pool-side archive cache. Rayon-class throughput for
  batch work — a ×cores lever no baseline JIT can match.
  - Spec: `superpowers/specs/2026-06-12-data-parallel-design.md`
  - Plan: `superpowers/plans/2026-06-12-data-parallel.md`

### Deployment & I/O throughput

- ✅ **WARM — Warm starts & durable-log throughput.** MERGED `02cf14c` (2026-06-17). Three
  behaviour-invisible units; no `ASO_FORMAT_VERSION`/`ARCHIVE_VERSION` bump; tree-walker untouched.
  (a) Content-addressed compile cache for `ascript run` (key: source + transitive module graph +
  flags + lockfile; `$ASCRIPT_CACHE/compiled/`) — **fail-open + verify-on-hit**; `--no-cache` /
  `ascript cache clean|dir`. **8.0× warm @ N=500, +60ms cold tax.** (b) PGO trailing `ASPGO` section
  riding OUTSIDE the archive codec; `build --pgo` harvests warmed IC/arith/shape state, `seed_chunk`
  re-installs behind every specialization guard — byte-INVISIBLE (seeded-PGO is the **445/0**
  `vm_differential` axis); `ASCRIPT_NO_PGO`. Steady-state seeded/unseeded **1.007×** (no load-path
  tax). (c) Workflow `Durability::{Fsync (default), Group, Buffered}` via one `record_event`
  chokepoint; crc-framed group appender + torn-tail prefix repair; at-least-once. **Group ≈98.85×
  faster than fsync** on per-commit shapes; default `"fsync"` ≈ baseline.
  - Spec: `superpowers/specs/2026-06-12-warm-starts-design.md`
  - Plan: `superpowers/plans/2026-06-12-warm-starts.md`
  - Follow-ups (recorded, none silent): cache auto-GC; PGO profile merging; method-IC seeding;
    group-mode background flusher.

### Evidence-gated (designed now, executed only when their gate opens — the JIT discipline)

- 🔒 **EXEC — Bespoke single-thread executor.** Replace tokio `current_thread`+`LocalSet` as the
  VM's task driver with a purpose-built `!Send` executor (intrusive run queue, no per-spawn
  `JoinHandle`/`AbortHandle` allocations, same-thread wakes that never touch the reactor, tokio
  retained solely as the I/O/timer driver). **Gate: a post-LANE re-profile showing the residual
  async tax still material (≥15% on the async corpus).** Cancel-on-drop and structured-concurrency
  semantics must survive byte-identically — this is the riskiest spec in the campaign and runs
  last among engine specs.
  - Spec: `superpowers/specs/2026-06-12-vm-executor-design.md`
  - Plan: `superpowers/plans/2026-06-12-vm-executor.md`

- ⚖️ **REGION — Task-scoped region allocation — EVIDENCE-REJECTED (NO-GO). See EXECUTION LOG.**
  The spike was executed honestly (probe → narrow prototype → A/B). The narrow refcount recycler
  (proven-dead `ObjectCell` reuse, `strong_count()==1` proof, `region-spike` feature) is SOUND and
  effective on its design shape (`region_escape` bench: 1,999,960 recycles, byte-identical,
  `vm_differential` 444/0 region-on) — but the §5.5 **G1 gate FAILS decisively:** recycled=0 on
  BOTH gate workloads. `json_roundtrip` builds all containers native-side in serde (0% VM-literal
  eligibility, Phase-0 probe); `server_request`'s `resp` is module-scope + passed to
  `json.stringify` (a Call-arg sink statically disqualified per §3.1/§4). 0% allocation-time
  reduction, wall not improved. G2/G3/G4/G5 pass; G6 moot. The ~45% alloc/gc CPU headroom lives in
  native-serde + Call-escaping allocations a bytecode-literal recycler provably cannot touch —
  confirming the lock-record prediction (promote-on-escape killed on identity grounds). The spike
  is frozen on `feat/task-regions` (unmerged); the vendored gcmodule `strong_count` fork (G6) was
  spike-local and does not ship. Evidence: `bench/REGION_RESULTS.md` + `bench/REGION_PROBE.md`.
  - Spec: `superpowers/specs/2026-06-12-task-regions-design.md` (Status → evidence-rejected)
  - Spec: `superpowers/specs/2026-06-12-task-regions-design.md`
  - Plan: `superpowers/plans/2026-06-12-task-regions.md`

- 🔒 **JIT — Baseline Cranelift JIT (existing spec, still deferred).** The design stands at
  `superpowers/specs/2026-06-08-baseline-jit-design.md`. This campaign UPDATES its preconditions:
  (1) NUM ✅; (2) the ≤16-byte value precondition is **UNMET — NANB evidence-REJECTED the 16-byte
  repr** (`value16` showed no measured win, see NANB row); `Value` is **final at 24 B**, so the
  JIT's ≤16-byte precondition does NOT hold and the JIT stays deferred unless its own re-profile
  (precondition 3) overrides on dispatch-dominance grounds alone; (3) profiling must be
  re-run AFTER LANE+CALL+SHAPE+DECODE — only if dispatch then dominates does the JIT proceed.
  New addendum requirements discovered since the spec was written: `Op::Break`/coverage
  byte-patching must invalidate compiled code (DECODE builds and proves the invalidation
  machinery); the sync lane defines the compilable subset and the lane-escalation seam is the
  natural native↔interpreter boundary; the cargo-fuzz infrastructure (shipped) takes the "JIT
  joins the fuzzer" cost to near-zero. Remains the LAST lever, by evidence.

### Developer-experience track (owner-sequenced relative to the engine waves)

- ✅ **DOCS — documentation reconciliation + permanent drift tripwires.** The 2026-06-12
  docs-vs-reality audit (re-verified during spec drafting) found `docs/content/cli.md` missing
  **27 CLI flags, the `ascript dap` subcommand, and all 7 `pkg` subcommands** (e.g. `build
  --native` is documented only on `language/bundles.md`, never on the CLI reference page), all
  9 user-facing `ASCRIPT_*` env vars undocumented there (`ASCRIPT_NO_SPECIALIZE`,
  `ASCRIPT_NO_SYNC_LANE`, and `ASCRIPT_NO_CALL_FAST` — the three kill switches — documented
  nowhere before DOCS), one stdlib member gap (`task.pipe` absent from `stdlib/async.md`), and
  a CLAUDE.md meta-drift ("stdlib pages mirror the source modules" — they are domain-grouped).
  Unit A is the one-time reconciliation sweep; Unit B is the durable value: six in-tree drift
  TRIPWIRES (clap-introspected CLI-surface ⊆ cli.md; env-var coverage; a validated
  module→page claiming table; NAV ⇄ files bijection; in-content link checker; editor-pin
  manual checklist) written failing-first against today's gaps, then kept green in CI — gate 19.
  Boundary with SIG: SIG owns per-function stdlib *signature* consistency; DOCS owns
  existence/claiming/CLI/env/NAV/links. Independent of all engine specs; mutually independent
  of SIG. **MERGED to `main` (`--no-ff`).**
  - Spec: `superpowers/specs/2026-06-12-docs-reconciliation-design.md`
  - Plan: `superpowers/plans/2026-06-12-docs-reconciliation.md`


- ✅ **SIG — stdlib signature table + LSP signature/completion/hover enrichment + audit
  hardening. MERGED to `main` (`--no-ff`, `11cdb6a`). See EXECUTION LOG.** The 2026-06-12 LSP audit established that signature help resolves ONLY a unique
  same-file `fn` (`src/lsp/providers/signature.rs` — a `MemberExpr` callee like `array.map(`
  returns `None` by construction, so the ENTIRE stdlib, all methods, all builtins, and all
  cross-file imports show no signatures), and that native stdlib functions have NO
  machine-readable signatures anywhere (only prose in `docs/content/stdlib/*.md` and the
  ~80-entry min-arity table `src/check/std_arity.rs`). SIG builds the missing data asset — a
  drift-tested `(module, fn) → {params, optionals/variadic, return, one-line doc}` table for
  all std modules, generated/validated from the stdlib reference pages — and wires it into
  THREE consumers: signature help (member callees: stdlib via namespace-import detection,
  methods via the infer `Table`'s `FnSig`s, imported user fns via the workspace `ParamList`
  walk), completion (real kind/detail/docs for member items + resolve), and hover on stdlib
  members. Also absorbs the audit's remaining hardening items (partial-identifier member
  completion, `workspace_diagnostic` yielding, model-cached inference for hover/completion,
  workspace-folder unindexing, fs-canonicalized index keys, auto-import dedup/sort_text,
  snippet-capability gating, string/comment completion suppression). Technically independent
  of every engine spec (LSP-only; no engine/VM/`.aso` surface) — sequenced after the engine
  waves by owner decision, executable any time the sequencing allows.
  - Spec: `superpowers/specs/2026-06-12-lsp-stdlib-signatures-design.md`
  - Plan: `superpowers/plans/2026-06-12-lsp-stdlib-signatures.md`

### Deployment & reach track (independent of the engine waves; RT is the track's foundation)

- ✅ **RT — runtime-only native stubs.** MERGED `349f4ce` (2026-06-18). CLI/link-level — **no engine
  change** (`ASO_FORMAT_VERSION` 29 + `ARCHIVE_VERSION` 1 unchanged; `vm_differential` 445/0 both configs
  with the cfg additions present). A runtime-only **`ascript-rt`** bin compiles the front-end (parsers,
  compiler, checker, LSP/DAP/fmt/REPL/pkg, tree-sitter) OUT via a **build-time cfg `ascript_rt`** (NOT a
  Cargo feature); §2.3 audit + an `nm` tripwire (0 `compile_source`/tree-sitter symbols) prove the stub
  ships no compiler. **4-tier matrix** (rt-core **5.75 MB = 13%** of the 43 MB toolchain .. rt-full
  32.6 MB) selected by the archive's import facts through a drift-tested module→feature table. **Fail-closed
  distribution:** ed25519-signed version-locked manifest (compiled-in pubkey, no insecure env knob; signing
  on a default-OFF `rt-release` feature, never in a stub), a content-addressed cache that re-hashes on load,
  a 5-rung ladder where **integrity aborts and only availability falls through**. Footer flags + `--compress`
  (zstd, bounded; `flags=0` byte-identical), `--target` cross (platform-independent payload, macOS
  sign-before-append), `--exact` (local-cargo precise stub), `--oci` (deterministic Docker-less OCI tarball,
  two-digest rule, musl-only), reproducible outputs, `--report-json` (schema-locked). Foundation for CNTR's
  images. Whole-effort holistic review APPROVE; FINAL gates all green; musl spike validated-at-first-CI
  (narrow-fallback recorded).
  - Spec: `superpowers/specs/2026-06-12-native-runtime-stubs-design.md` · Plan: `superpowers/plans/2026-06-12-native-runtime-stubs.md`

- ✅ **CNTR — container-native runtime + `std/docker`. MERGED to `main` (`--no-ff`). See EXECUTION LOG.** Unix-domain sockets in `std/net` +
  `std/http` (`{socketPath}`) as the missing foundation; `std/docker` as a typed wrapper over
  the Engine API (containers/images/exec, `logs`/`events` as `for await` streams) gated on
  **net AND process** caps (dual-cap chokepoint extension — the docker socket is
  host-root-equivalent); inbound signal handling (`process.on("SIGTERM", …)`),
  `server.serve({onShutdown, drainTimeout})` graceful drain, cgroup-aware worker sizing
  (`cpu.max`), `os.inContainer()`, official base images built from RT stubs, and
  `ascript init --template server` scaffolding (Dockerfile + healthcheck + shutdown +
  resilience wired). Depends on RT (images) and RESIL (template policies).
  - Spec: `superpowers/specs/2026-06-12-containers-docker-design.md` · Plan: `superpowers/plans/2026-06-12-containers-docker.md`

- ✅ **RESIL — `std/resilience` for backend hosting. MERGED to `main` (`--no-ff`). See EXECUTION LOG.** Composable per-isolate policies:
  circuit breaker, keyed token-bucket rate limiter, bulkhead + load shedding, retry v2
  (backoff + jitter + budgets), fallback, policy composition; **singleflight** +
  stampede-protected memoization (composing `std/lru`); **deadline propagation** via the
  spec's ONE runtime seam — task-local storage (zero-cost when unused; also unlocks
  request-id/trace propagation); Prometheus text `/metrics` + telemetry counters;
  health/readiness helpers. Per-isolate state is documented honestly (actor pattern for
  global state). Parked with sketches: hedged requests, AIMD adaptive concurrency, `std/k8s`.
  - Spec: `superpowers/specs/2026-06-12-resilience-stdlib-design.md` · Plan: `superpowers/plans/2026-06-12-resilience-stdlib.md`

- ✅ **EMBED — embedding API (Rust crate + C API). MERGED to `main` (`--no-ff`, `aa512616`). See EXECUTION LOG.**
  A stable, versioned host API: create
  isolates, eval/load archives, call script functions, register host functions/modules,
  value conversion, host-controlled caps, async integration — the `!Send`-isolate model is
  ideal for embedding (one isolate per host thread, no global VM lock). C API as a `cdylib`
  feature with a handle-based, panic-safe `ascript.h`. Lua's niche: game scripting, plugins,
  edge hosts.
  - Spec: `superpowers/specs/2026-06-12-embedding-api-design.md` · Plan: `superpowers/plans/2026-06-12-embedding-api.md`

- 🔒 **WASM — wasm32 target + browser playground (spike-gated).** v1 = compile front-end +
  VM to wasm for an in-browser playground on the docs site (compile+run, captured output,
  caps default-deny, wasm-compatible stdlib subset); WASI/edge runtimes recorded as the
  evidence-gated follow-up. Phase 0 is a build-matrix feasibility spike (tokio-on-wasm,
  stacker, tree-sitter C linkage) with GO/NO-GO recorded.
  - Spec: `superpowers/specs/2026-06-12-wasm-target-design.md` · Plan: `superpowers/plans/2026-06-12-wasm-target.md`

### Flagship & ecosystem track

- ✅ **REPLAY — record/replay as a
    user-facing flagship. MERGED to `main` (`--no-ff`, `e6229e68`).** SP9's INERT determinism plumbing made a
    headline: `run --record/--replay/--seed`, `test --record/--replay` (failed tests auto-save a trace under
    `.ascript-traces/`), and DAP time-travel (`stepBack`/`reverseContinue` by deterministic re-execution, the
    rr model). Extended `DetEvent` recording to effectful stdlib at the `call_stdlib` chokepoint behind a
    `trace_active()` `Cell` (zero-cost-when-off), the answer to the core design question: a complete
    `replay_class` table (Seamed/Recorded/Refused/Harmless) with a completeness test, airlock outcome encoding
    (NOT JSON — NUM fidelity), HttpResponse-only handle virtualization, strict CliTrace replay, worker refusal,
    the hostile-safe `ASTRC` trace format + fuzz target. No grammar/`Value`/opcode change, `ASO_FORMAT_VERSION`
    29 unchanged, `vm_differential` flag-gated-untouched. Headline: plain→record `sleep_heavy` **56.0×** (real
    sleeps become virtual), replay skips all real OS effects; spec/tw geomean 3.78× ≥ 2×; `dbg_zero_cost_gate`
    0.969×. Spec: `superpowers/specs/2026-06-12-record-replay-design.md` · Plan:
    `superpowers/plans/2026-06-12-record-replay.md`. **Deltas from spec (recorded):** (a) `run --replay <trace>
    <prog>` REQUIRES the program-file arg in v1 (the spec §4.1 draft called it optional) — documented in
    `docs/content/cli.md` + `--help`, the source digest still governs; (b) `test --replay` of a CHANGED test
    file proceeds with a printed WARNING (not the hard `run` error — §4.3 sharpening). v2 follow-ups (streaming/
    SSE/WS + general handle virtualization, per-isolate worker traces, replay checkpointing for O(1) backsteps,
    task-identity event tags, `--deterministic` alias, `--profile`/`--inspect`×record matrix) recorded in
    `superpowers/roadmap.md`.

- ✅ **BATT — backend batteries (T1+T2). MERGED to `main` in 4 phases (`--no-ff`): A auth `fc21c1f`,
  B data `bf13fb3`, C testing `a1c92cf`, D toolbelt `72d5977`. See EXECUTION LOG.** One multi-unit stdlib spec, phased like the
  batteries campaign: **T1** — TLS for `std/server`/`std/tcp` (rustls); `std/jwt` + auth
  (JWKS, OAuth2/OIDC client, signed cookies/sessions); `std/archive` (tar+zip, streaming —
  also RT's `--oci` tar substrate); `std/xml` (+ HTML sanitizer); `std/email` (SMTP + message
  builder); `std/blob` (S3-compatible client: sigv4, presign, MinIO/R2); deterministic-testing
  batteries (frozen clock / seeded RNG in `ascript test` via the det seams + user-facing
  property testing `test.prop` with shrinking, surfacing the FUZZ generator philosophy).
  **T2** — `std/cron`, `std/semver`, `std/markdown`, `std/diff`. Each unit: feature flag, caps
  mapping, docs page + NAV (DOCS tripwires apply), intro + advanced examples, four-mode tests.
  - Spec: `superpowers/specs/2026-06-12-backend-batteries-design.md` · Plan: `superpowers/plans/2026-06-12-backend-batteries.md`

- ✅ **LSPEC — language specification + stability policy. MERGED to `main` (`--no-ff`, `0064be93`). See EXECUTION LOG.**
  A versioned normative spec
  (grammar derived from the tree-sitter grammar with a drift check; semantics chapters; the
  examples corpus formally adopted as the conformance suite), a stability-tier policy
  (stable/experimental surface), the pre-1.0 → 1.0 breaking-change criteria checklist, and an
  RFC-lite process doc. Documentation-and-governance work; no code surface.
  - Spec: `superpowers/specs/2026-06-12-language-spec-stability-design.md` · Plan: `superpowers/plans/2026-06-12-language-spec-stability.md`

### Language surface track (the campaign's ONE grammar change)

- ✅ **DEFER — `defer` statement for scoped cleanup.** Go-shaped: function-scoped, LIFO,
  arguments evaluated at `defer` time, deferred calls run on EVERY body exit — normal return,
  `?`-propagation, and panic unwind to a `recover` boundary. Closes the recurring gap where
  `?` early-exits skip manual `close()` calls. Pays the full grammar tax (both parsers,
  tree-sitter regen + editor pins, formatter canonicalization, both engines byte-identical,
  `.aso` bump + verifier, exhaustive AST matches, LSP/REPL/checker). The hard design
  questions the spec must settle honestly: defer in async fns under cancel-on-drop (do
  defers run on task abort?), defer in generators (`gen.close()`/last-drop), sync-only
  execution of deferred calls (a deferred async fn's future is not awaited), defer-in-loop
  accumulation semantics (+ a lint). **Sequencing constraint:** touches the same frame
  return/unwind paths LANE/CALL/DECODE rework — land it before LANE starts or after the
  engine waves merge (owner call), never concurrently.
  - Spec: `superpowers/specs/2026-06-12-defer-statement-design.md` · Plan: `superpowers/plans/2026-06-12-defer-statement.md`

### Removed / parked (recorded so they aren't re-litigated)

- **`using` blocks** — rejected in favor of `defer` (see the DEFER spec: needs a closeable
  protocol, composes worse across mixed resource lifetimes; recorded there).
- *(Top-of-stack register caching was promoted into DECODE as its evidence-gated Unit D.)*
- **Package registry (REG)** — owner-deferred for now; the pkg manager's bare-version source
  stays the reserved error.

- **Register-based bytecode** — rejected: rewrites compiler/VM/verifier/`.aso`/disasm and
  re-proves the whole differential while LANE+DECODE capture most of the win incrementally.
- **Deferred refcounting / immortal values** — parked with the sanctioned future GC rework; the
  `Cc` cycle-collector's invariants make it a separate campaign.
- **Tail-call threaded dispatch** — blocked on Rust `become` stabilization; zero cost to wait.
- **Small-string optimization** — demoted to opportunistic (no profiling evidence); NANB may
  revisit inline short strings ONLY behind its measured-win gate.

## EXECUTION LOG (live)

- **REPLAY** — ✅ MERGED to `main` (`--no-ff`, `e6229e68`). Record/replay as a user-facing flagship: 13 commits
  (Tasks 0-11), each subagent-implemented + independently opus-reviewed + controller-verified (vm_differential
  446/0 BOTH configs after every task; the final holistic ran the FULL suite both configs **5230/0** + a 10-min
  fuzz campaign **5.3M runs / 0 crashes**). **The mechanism:** SP9's shipped-but-INERT determinism plumbing
  surfaced behind a `trace_active()` `Cell` at the `call_stdlib` chokepoint (the answer to the spec's core design
  question — record effectful stdlib I/O at the result boundary, NOT just the seamed clock/RNG subset). A complete
  `replay_class` table (Seamed/Recorded/Refused/Harmless) with a **sabotage-proven completeness test** (a new
  unclassified module trips it loudly — no silently-Harmless effect); **airlock outcome encoding, NOT JSON** (NUM
  Int/Float fidelity — a JSON-coded replay would branch wrong on a recorded number); HttpResponse-only handle
  virtualization (vid birth + `NativeCall` record/replay); the hostile-safe `ASTRC` trace format + fuzz target.
  **Surfaces:** `run --record/--replay/--seed` (cross-engine: a trace records on any engine and replays
  byte-identically on tw↔VM↔generic↔.aso — the Gate-1 extension); `test --record/--replay` (per-test failure
  traces under `.ascript-traces/`, sliced as O(1) index-ranges, failure-only save); DAP **time-travel**
  (`stepBack`/`reverseContinue` by deterministic re-execution, the rr model — `supportsStepBack` replay-only).
  **Bugs fixed in-branch (failing-test-first):** the §0.5 bare-`time.sleep` replay desync (a pre-existing
  workflow cursor bug); a DAP `teardown_session` deadlock (ClearBreakpoints before the final Continue — surfaced
  by multi-breakpoint re-execution); the Task-7 module-load-failure trace replaying just-the-load (was rejected as
  "corrupt"). **Determinism audit (§8):** every std module classified vs SOURCE; `time.interval/debounce/throttle`
  reclassified Seamed→Refused (real tokio timers bypass the clock seam) + `archive` disk funcs Harmless→Recorded.
  **Invariants held:** NO grammar/`Value`/opcode change; `ASO_FORMAT_VERSION` **29 unchanged** (0 diff vs main
  `src/vm/aso.rs`); `vm_differential` flag-gated-untouched (REPLAY adds no engine mode); `Origin::Workflow`
  det/workflow semantics additions-only; no `unwrap`/`panic!` reachable from hostile trace bytes. **Process note:**
  Task 6 was implemented INLINE by the controller through a sustained API 529-overload that blocked subagent
  dispatch, then fully independently reviewed once the API recovered. **Headline:** plain→record `sleep_heavy`
  **56.0×** (real sleeps become virtual under the SP9 clock); replay skips all real OS effects (effect_heavy
  12×, proc_heavy ∞); spec/tw geomean **3.78×** ≥ 2× (the `call_stdlib` neighbor untaxed); `dbg_zero_cost_gate`
  **0.969×**; zero-cost-off cross-binary A/B **0.982×** (branch faster — the `Cell` is free). **Deltas from spec
  (recorded):** `run --replay` requires the program-file arg in v1 (spec §4.1 draft called it optional —
  consistently documented in `cli.md`/`--help`); `test --replay` of a changed test file warns (not the hard `run`
  error). v2 follow-ups recorded in `superpowers/roadmap.md`. Now **19/21** (WASM spike-gated + EXEC evidence-gated
  remain; JIT a recorded deferral).

- **LSPEC** — ✅ MERGED to `main` (`--no-ff`, `0064be93`). The AScript normative language specification + its
  stability/governance policy, plus the owner-approved grammar feature that resolved the recover-contract triage.
  **The owner-triage (plan-marked):** the `recover(fn(){…})` carry-forward — the owner chose to **ADD anonymous
  fn-expressions** (over documenting a restriction). Implemented as `fn(params){body}` value expressions DESUGARED to
  `ExprKind::Arrow` (parsers-only: legacy + CST + tree-sitter regen; **NO new `ExprKind` variant, NO opcode/`.aso`
  change, `ASO_FORMAT_VERSION` 29 unchanged**; `vm_differential` 446/0 four-mode BOTH configs). So `recover(fn(){…})`
  now works (alongside arrow + named-fn); `fn(x):T{}` is REJECTED (Arrow has no ret_type field; enforce-both would
  break fmt idempotence — arrows carry no return contract). **The independent review caught THREE four-mode divergences
  the 445-corpus + both conformance suites missed** — all in speculative-parse *expression-starter predicates* the
  corpus never exercised with these token combos: `return fn(){}` (CST `can_start_expr`/`token_can_start_expr` +
  `ternary_ahead` + legacy `starts_expression` omitted the `fn`-keyword), `fn(x):T{}` (return-annotation enforced on VM,
  dropped on tree-walker), and `yield ~x` (legacy `starts_expression` omitted `Tilde` — a pre-existing gap, fixed +
  regression-tested). The controller's full-suite run ALSO caught a stale `check::rules::workflow_determinism` test
  (it asserted the fn-form was a syntax error; updated to assert the rule now *walks* the fn-form — strictly better).
  **THE SPEC:** 16 normative chapters under `docs/content/spec/` (intro · lexical · grammar [all **108** tree-sitter
  rules covered] · values · expressions · statements · classes · patterns · errors · modules · concurrency ·
  capabilities · types · stdlib · conformance · stability), each **Gate-14-grounded** (the `## Conformance` section
  cites real examples/tests that were RUN to confirm every normative claim — the spec describes the IMPLEMENTED
  language, not aspiration; the final holistic re-probed 6 cross-chapter claims against the live binary, all matched).
  `tests/spec_drift.rs` is the permanent drift guardrail (grammar-rule coverage anchored to `grammar.js` + chapter
  existence + citation resolution + a mutation self-test) — **fully GREEN** both configs. **Governance:**
  `spec/stability.md` (version = crate 0.6; STABLE [chapters 2–13 + stdlib surface] / EXPERIMENTAL [`http3`, DAP
  stepping, record/replay surface, lint inventory, …] / INTERNAL [`.aso` by `ASO_FORMAT_VERSION` name, opcodes, worker
  wire tags, shape/IC; `ascript::embed` is the carved-out STABLE Rust host surface post-EMBED] tiers, each
  owner-editable; the road-to-1.0 checklist) + `superpowers/rfcs/{0000-template,README}.md` (the RFC process) +
  `CONTRIBUTING.md` "Language changes & stability" + a pointer in the 2026-05-29 design doc (now the historical
  record). Reality-corrections written into the chapters (not papered over): there is **no decimal literal** (decimal
  is `std/decimal.parse` only); redeclaration is a runtime error for module-scope globals only (block-local `let` may
  shadow). The governance loop demonstrably CLOSES (a fake grammar rule → `spec_drift` RED with a CLAUDE.md-citing
  message; the three governance views are one consistent process). Docs-only + parsers-only branch — zero engine/
  compiler/interp diff. CLAUDE.md gained the spec-staleness "Touching syntax" bullet; README + NAV name
  `docs/content/spec/` as the normative set. EDITOR-PIN follow-up (the tree-sitter regen): mirror sync + Zed/Nvim rev
  bump is a publish step CI handles.

- **EMBED** — ✅ MERGED to `main` (`--no-ff`, `aa512616`). A host-embedding API in **6 units** (subagent-driven; fresh
  implementer + independent opus reviewer per unit; the host-module security phase + the C-FFI safety phase + a final
  cross-subsystem holistic). **NO engine/`.aso`/opcode/`Value` change** — `ASO_FORMAT_VERSION` 29 + the `Op` count 121
  pinned by `tests/embed_negative_space.rs`; `vm_differential` 445/0 four-mode UNCHANGED both configs; Gate-12 A/B
  geomean **1.014×** (≈baseline), RSS identical (`bench/EMBED_RESULTS.md`) — the host-side facade adds nothing to the
  hot path (the `host:` dispatch sits on the previously-error fall-through arm). **Unit A** (`src/embed/`, `embed`
  feature): `Isolate` (`!Send+!Sync`, owns a current-thread `tokio::runtime::Runtime`; parallelism = more isolates),
  blocking `eval` lifting the REPL substrate (`compile_source`→Fiber→`vm.run`; a compile error leaves the session
  UNmutated) + a nested-runtime guard (ambient runtime → `EmbedError::NestedRuntime`, not a `block_on` panic),
  `call`/globals/`load_archive` (+`*_async`), `EmbedError`, and **DENY-ALL caps by default** (the loud inverse of the
  CLI's all-granted — a host that forgets to grant gets the safe default). **Unit B:** `AsValue(Value)` (`!Send`) —
  container handles are LIVE ALIASES (same `Rc`/`Cc` cell, bidirectional host↔script visibility); mutators route
  through the engine paths (`index_set`/`ObjectCell::insert`+`frozen_kind`) so type/frozen checks aren't bypassed
  (Map/Set read-only host-side; SHAPE slab-safe `.get()`); the 25-kind table crosses every `Value` variant (opaque
  kinds prove `Rc`/`Cc` pointer identity); `to_json`/`json_parse` deep bridge (cyclic → error, never hangs). **Unit C
  (the only core touches, both behaviour-invisible):** `classify_specifier`→`SpecifierKind::Host` (after the `std/`
  hot-path check) + `load_host_module` mirroring `load_std_module` on BOTH engines + the dispatch arm
  `m if m.starts_with("host:")` riding the PREVIOUSLY-ERROR fall-through in `call_stdlib` (engine-parity test
  byte-identical). FFI-mirrored tiering (`func` Recoverable→Tier-2, `fallible_func` Recoverable→`[nil,err]`,
  `Panic`→Tier-2). **HOST FNS BYPASS CAPS** (native Rust the host wrote — LOUD docs). The `host_modules` registry is a
  CORE `Interp` field (empty under `--no-default-features`). **Worker isolation (the #1 security threat, defended in
  depth):** a `host:` import in a worker consults THAT isolate's registry → clean miss panic unless a
  `host_module_factory` (a `Send` factory riding the SAME side-channel as `caps`, installed FRESH per pooled request →
  no cross-isolate leak) is passed; a `Value::Builtin("host:…")` is non-sendable (can't cross the airlock).
  `StdlibFilter` = availability knob, NOT a security boundary; checker `host:`-skip. **Unit D:** the `ascript-capi` C
  crate (`capi/`, own empty `[workspace]`, NEVER in the root build graph) — every `unsafe extern "C"` fn does
  NULL→thread(`AS_ERR_WRONG_THREAD`, the Isolate is thread-affine)→poison→`catch_unwind`; a wrong-thread `as_value_free`
  LEAKS (documented contract — an off-thread `Rc` decrement is a data race); late host-fn registration errors if the
  module was already imported; a hand-written `ascript.h` + a source-scan drift test + a compiled-C smoke test
  (`cc::Build` links the cdylib, runs it, exit 0). The Unit-D review compiled a HOSTILE C misuse program (every misuse
  → clean status code, no segfault), ran `leaks` (0 leaks), and `nm`-verified 23==23 exported symbols == header.
  **Units E/F:** `examples/embed/{rust-host,c-host}` (CI-executed sentinel-checked, NOT corpus — discovery is
  non-recursive over `examples/`); `docs/content/embedding.md` (+NAV `embedding`) + README + the §9 rustdoc stability
  contract (`ascript::embed` is the only semver-promised Rust surface). The final cross-subsystem holistic verified the
  seams per-unit reviews can't see (filter↔archive import filtering, Isolate-drop is leak-free via `leaks`,
  poison↔output, host-fn re-entry can't double-borrow, async-`call` auto-await completes). Carry-forward: a pre-existing
  jitter-sensitive DECODE microbench gate (`decode-on/off 1.056×`) is NOT EMBED (decode diff empty) — documented in
  `EMBED_RESULTS.md`.

- **BATT** — ✅ MERGED to `main` in **4 phases** (`--no-ff`): **A auth `fc21c1f`** (std/jwt typed-keys +
  std/oauth PKCE), **B data `bf13fb3`** (std/{archive,xml,html,email,blob}), **C testing `a1c92cf`**
  (det-test seam + std/test + prop()), **D toolbelt `72d5977`** (std/{cron,semver,markdown,diff}). One
  multi-unit stdlib spec, subagent-driven (fresh implementer + independent opus reviewer per task; per-phase
  + whole-effort holistic; the four security-sensitive units each got a dedicated adversarial security
  review). **Pure stdlib + cap-gate generalizations — NO engine/`.aso`/opcode/`Value`-size change across the
  whole campaign** (`ASO_FORMAT_VERSION` 29 unchanged, `Value` 24; `tests/batt_negative_space.rs` pins both;
  `vm_differential` four-mode unchanged at 445/0 both feature configs). **Real defects caught + fixed in-branch
  by the review gates** (production-grade mandate, each failing-test-first): (1) **std/blob SigV4
  double-encode** — keys + list/multipart queries were double-percent-encoded in the signature vs the wire URL;
  ASCII-idempotent so all happy-path tests passed against a non-verifying mock, but real S3 would reject every
  non-ASCII key / base64 continuation token with `SignatureDoesNotMatch`. Caught only because the review
  upgraded the test stub to **RECOMPUTE the signature from the wire bytes** (the durable guard); fixed by
  single-encoding (raw path/pairs → one `canonical_uri`/`canonical_query_pairs`, wire URL built from the same
  strings). (2) **std/cron slab-`borrow()` VM panic** — `ObjectCell::borrow()` on a source-literal opts object
  hard-aborts the VM (slab storage) while the tree-walker (Dict) succeeds — a four-mode divergence + uncatchable
  crash that unit tests built from `IndexMap` (Dict) structurally could not catch; fixed with the slab-safe
  `ObjectCell::get` accessor + a VM-path regression test. This **SHAPE footgun is now a documented DX rule** (any
  stdlib fn reading user-Object fields uses `.get()`, never `.borrow()`). (3) **A1 missed a std_sigs row**, (4)
  **Phase-A/B rt_select tiering rows**, (5) **B6 swapped gate-label counts** — drift caught at merge boundaries.
  (6) **Carry-forward: a pre-existing tree-sitter gap** (the `template_string` rule had no escape alternative, so
  `` `${x}\n${y}` `` produced editor ERROR nodes) surfaced by Phase-B's `blob_basics.as` and fixed root-cause
  (`template_escape` rule + regen `parser.c --abi 14` + regression test) — the lesson: tree-sitter is a THIRD
  parser that four-mode + `check` don't exercise. **Security batteries** (each adversarially reviewed + RUN):
  zip-slip (lexical-normalize + canonical-root containment + symlink-nofollow), XXE (no entity table by
  construction), XSS (fail-closed emit-from-parse sanitizer; reused as the single source of truth by markdown's
  sanitize-by-default pipeline — 25+ vectors inert), SMTP STARTTLS-strip → Tier-1 / plaintext-auth → Tier-2 +
  wire-layer CRLF re-validation, SigV4 (cross-checked vs the AWS test-suite vectors; semver cross-checked vs node
  `semver`; diff byte-matched vs GNU `diff -u`; cron next-time cross-checked vs python `croniter`). **Caps:** the
  `required_cap` chokepoint generalized to a `CapReq` conjunction (CNTR precedent) where needed; whole-module
  `Net` for email/blob (incl. `blob.presign`), PER-FUNC `Fs` for archive, `KNOWN_UNGATED` for the pure modules;
  `cap_audit` (Gate-10) + the classification-completeness test enforce the partition. **Determinism:** the C1
  seam (`set_determinism`, `ascript test --seed/--frozen-time`) is INERT by default (no-flag run byte-identical),
  installed per-test; `prop()` shrinking converges to exact boundaries (`x<=99`→`100`, `!contains("ab")`→`"ab"`).
  10 new Cargo features (tls/auth/archive/xml/email/blob/cron/semver/markdown/diff), all default-on, all
  `#[cfg]`-gated so `--no-default-features` still builds. CLAUDE.md gained a condensed BATT subsection + the 10
  features; stdlib overview + README rows complete; per-module docs on their owning pages with NAV intact.

- **DEFER** — ✅ MERGED to `main` (`--no-ff`). The campaign's one grammar change: `defer [await]
  <call>`, reserved keyword, call-only, args-evaluated-at-statement, per-activation LIFO, drained
  on every frame exit (normal/return/`?`-propagate/panic-unwind; NOT on `exit()`/cancellation/
  `gen.close()`), §3.6 panic-merge rules, first-class `defer await`. Four-mode byte-identical
  (tree-walker == specialized == generic == `.aso`); full grammar tax paid (both hand parsers +
  tree-sitter regen `--abi 14` + editor-pin bump to split SHA `3c2bb8b`; CI mirrors the grammar on
  origin push); ASO_FORMAT_VERSION 27→28 (two opcodes `DeferPush`/`DeferPushMethod` + verifier
  negative-space + disasm + bcanalysis). 6 phases, subagent-driven (fresh implementer + independent
  spec & quality reviewers per task; per-phase holistic; whole-effort holistic). **Six real defects
  caught + fixed in-branch by the review/fuzz gates** (production-grade mandate, each with a
  failing-test-first regression guard): (1) CST nested-named-arg false-positive (`defer
  f(g(x:1))` wrongly rejected); (2) **concurrency unsoundness** — an Interp-level defer stack let
  concurrent async activations clobber each other's lists → reworked to the spec's per-activation
  env-scope (`Scope.defers`); (3) module-import top-level defers silently dropped (`load_module` →
  `exec_program`); (4) a vacuous cancellation test + missing `task.timeout`/`async fn*` coverage;
  (5) **VM async-closure inline-drain** returning `Nil` instead of a `Future` (the bare-future §3.4
  panic never fired on the VM — a four-mode divergence) — found by mandating four-mode coverage of
  §8.1; (6) **verifier `StackJoinMismatch`** — `verify_stack_balance` treated `DeferPush`/
  `DeferPushMethod` as stack-neutral, so a `defer` inside an `if`/`else` branch failed `.aso`
  round-trip — **found by the Gate-15 differential fuzzer** (no hand-written test had a defer in a
  conditional), fixed + a deterministic Gate-0 regression test + a corpus seed. Plus a holistic-
  found flaky example (shared `/tmp` path raced the concurrent four-mode corpus) → per-execution
  unique temp dir, 10/10 vm_differential green. Gates: vm_differential 409/0 both feature configs;
  full suite + clippy clean both configs (+ `--features fuzzgen`); Gate-5 0 on `examples/**` both
  configs; perf (`bench/DEFER_RESULTS.md`) defer-free geomean +0.6%, spec/tw geomean 2.94× ≥ 2×,
  dbg_zero_cost 0.998×, RSS noise-level; two lints (`defer-in-loop`, `defer-async-call`); fmt/LSP/
  REPL parity; examples (intro + advanced, four-mode + fmt-idempotent); docs (errors/syntax/
  modules-async + CLAUDE.md + roadmap + LSPEC note, NAV intact). Spec correction recorded in-branch:
  §2.2.5/§8.4 — tree-sitter recovers a reserved keyword as an identifier name (true of every
  reserved word; the hand parsers are the reservation SoT) — a tooling-reality correction, no change
  to recorded language semantics.

- **LANE** — ✅ MERGED to `main` (pending; on `feat/two-lane-engine`, holistic review complete). Two
  drivers over the existing `Fiber`: `run_loop_sync` (plain non-async, tight-loop, suspension-free subset)
  and `run_loop` demoted to an orchestrator that bursts into the sync driver and takes over only at genuine
  suspension points. Per-op runtime escalation (`NeedsAsync` at un-advanced ip); `Op::Await` on an
  already-resolved future taken inline via `SharedFuture::try_get`; `Op::DeferPush`/`Op::DeferPushMethod`
  in-subset but frame-exit-with-non-empty-defers escalates. Kill switch: `Vm.sync_lane` +
  `ASCRIPT_NO_SYNC_LANE=1`. **No grammar change, no semantics change, no `.aso` change.**
  - **Gates:** four-way differential (tree-walker == specialized-lane-on == specialized-lane-off ==
    generic-lane-on) + fuzz axis (`fuzz/fuzz_targets/differential.rs`) + corpus coverage assertion
    (`lane_corpus_coverage_check`); `vm_differential` 423/0 BOTH feature configs; full suite + clippy clean
    both configs; Gate-5 0 on `examples/**` both configs.
  - **Performance (`bench/LANE_RESULTS.md`, same-session A/B, Gate 16):** A/B geomean **1.045×** (4.5%
    faster); dispatch-bound workloads: `object_churn` +15%, `call_heavy` +21%. Async-scheduler-dominated
    workloads within noise (kevent/park bottleneck unchanged). RSS: no regression (Gate 18). DBG zero-cost
    gate: **1.006×** (≤1.05× threshold). Spec/tw geomean: **3.59×** (≥2× Gate 12/17 floor).
  - **Post-LANE re-profile + EXEC gate verdict:** Residual async share on `async_inline` ≥70%, on
    `async_concurrent` ≥60% — both well above the ≥15% EXEC gate threshold. The sync lane moved only
    the VM-dispatch fraction (~9% of async_inline wall time); the scheduler round-trip on every pending
    `await` (kevent/park/notify/SharedFuture) is untouched. **EXEC gate: OPEN.** EXEC stays #1 priority
    (inline-first dispatch; §4 zero-overhead trivial-async). After EXEC: allocation (#2 — json_roundtrip
    ~38% alloc; CALL/SHAPE/NANB); hashing (#3 — SipHash in object_churn 13%). JIT remains the LAST lever
    (only dispatch-dominated tight loops, and LANE+specialization already deliver 3–6× there).

- **CALL** — ✅ MERGED to `main` (`--no-ff`). The campaign's call-path allocation diet: three allocation
  units (A1/A2/A3) over `src/vm/{fiber,run}.rs` + a callback trampoline (Unit B) over the higher-order
  stdlib builtins. **No grammar change, no semantics change, no `.aso` change** (`ASO_FORMAT_VERSION` 28
  unchanged), no tree-walker change. VM-only throughout.
  - **A1 (empty-cells fast path):** `alloc_cells` returns `Vec::new()` when `cell_slots` is empty —
    capture-free frames allocate no cells vector. Always-on (not gated on `call_fast` — behavior-invisible).
    Saves ~1 heap alloc per capture-free call. Alloc slope: pre-A1 ~3.0/call → post-A1 ~2.0/call.
  - **A2 (in-place arg binding):** the qualifying `Op::Call` plain-Closure arm (`call_fast=true`,
    `!has_rest`) runs `check_call_args_in_place` (borrows the operand-stack window, no `Vec`) then
    `fiber.stack.remove(callee_idx)` + `resize` for defaults — eliminates the `vec![Value::Nil; argc]`
    and `BoundArgs.values` Vec. Combined with A1: **0 allocs/qualifying call** (the per-call allocation
    floor is reached). Shared arity + contract logic extracted into `check_call_arity`/`check_param_contract`
    cores consumed by both paths — wording byte-identical by construction.
  - **A3 (fiber pooling):** `fiber_pool: RefCell<Vec<Fiber>>` (cap `FIBER_POOL_MAX = 8`) on `Vm`;
    take=exclusive-ownership (`take_pooled_fiber` pops + resets — fresh cells per element, capture
    identity preserved); return-only-on-`RunOutcome::Done` (`return_pooled_fiber`); on `Err` the fiber
    is dropped. Three re-entrant call funnels pooled: `call_value` plain-Closure arm,
    `invoke_compiled_method`, `invoke_compiled_static`. Generator fibers, the module fiber, and the
    program root are never pooled. Saves 2 Vec allocs per re-entry amortised; A3 alloc slope: 31→15
    allocs/element (both within budget; `on ≤ off + 2`).
  - **Unit B (trampoline):** `array.{map,filter,reduce,sort,find,findIndex,some,every,flatMap,groupBy,
    partition}`, `object.mapValues`, and stream pipeline + terminals detect a `Value::Closure` callee
    and drive all elements through ONE reused fiber on LANE's sync lane; per-element escalation to the
    async driver when a callback suspends — never re-executing the element. Arming seam:
    `CallbackTrampoline::arm` returns `Some` iff callee is `Value::Closure` (VM-only); `Value::Function`
    (tree-walker) takes the unchanged generic path.
  - **Kill switch:** `Vm.call_fast` (`bool`, default true; env `ASCRIPT_NO_CALL_FAST=1`);
    `Vm::new_generic` disables it — generic path is the complete semantic floor. Cost-free when off
    (kill-switch-off parity ≤1.006×).
  - **Fifth differential mode:** `vm_run_source_no_call_fast` joins `vm_differential.rs` (both feature
    configs). Alloc-count slope harness: `tests/alloc_count.rs`. Coverage assertions:
    `trampoline_calls`, `inplace_binds`, `trampoline_escalations` > 0.
  - **Gates:** `vm_differential` **424/0** both feature configs; spec/tw geomean **4.05×** (≥2×);
    `dbg_zero_cost_gate` **1.005×** (≤1.05×); A/B geomean **1.000×** (func_pipeline +1.1%, call_heavy
    +1.6% — modest on a fast-allocator machine; the alloc/memory win is the headline); A1+A2 alloc
    slope **0.000/call** (< 1.0 budget); A3 alloc slope **15 vs 31/element** (on ≤ off+2; both < 50);
    kill-switch-off parity ≤1.006×; RSS no regression; full suite + clippy clean both feature configs;
    Gate-5 0 on `examples/**` both configs.
  - **Spec deltas (recorded):** (1) stream-stage trampoline is per-element, not cross-element — `Stage`
    must be `Clone` but `CallbackTrampoline` is not; deferred to DECODE; (2) `Op::CallMethod` in-place
    binding deferred to DECODE (§7 follow-up; method-IC fast path unchanged); (3) smallvec alternative
    not needed (in-place binding reached 0 allocs/call without it).
  - **Post-CALL re-profile + remaining lever re-rank (mandatory campaign checkpoint):** Post-CALL
    profiling of `func_pipeline` shows the bottleneck is NOT call-path allocation (driven to ~0 by
    A1+A2+A3) but dispatch/arithmetic in callback bodies (already addressed by LANE) and **object
    hashing/storage** — SipHash on IndexMap key insertion in the filter/map pass is the dominant
    remaining cost. Re-ranked remaining levers: (1) **EXEC** (async scheduler tax — gate OPEN from
    LANE, residual async share ≥70%/#1 unchanged); (2) **SHAPE** (object hashing/storage — the new
    `func_pipeline` ceiling post-CALL); (3) **NANB** (value representation — enables SHAPE's flat
    storage and is the JIT precondition); (4) **DECODE** (pre-decoded stream — CALL bought the
    call-allocation lever, DECODE targets dispatch decode overhead). CALL's primary deliverable is the
    **memory/alloc win** (Gate 18): 0 allocs/qualifying call + halved re-entrant allocs. The +1.1%
    wall-clock headline reflects that a fast system allocator's amortised cost is already low on this
    hardware; the structural allocation elimination matters more at scale and under memory pressure.

- **SHAPE** — ✅ MERGED to `main` (`--no-ff`). Shape-native object/instance storage: `ObjectCell` and
  `Instance.fields` now hold an `ObjectStorage::{Slab{keys: Rc<[Rc<str>]>, values: Vec<Value>} | Dict(IndexMap)}`
  behind SEALED accessors (the legacy `borrow()` shim panics on a slab). The VM builds slabs; the
  tree-walker builds Dict (shape 0) — the oracle is unchanged, which the four/five-mode differential proves.
  - **Phases:** 0 (the live `object.delete` stale-shape IC bug, fixed first on the old representation);
    1 (mechanical accessor-API migration + sealing `map` private — ~48 files); 2 (`ShapeRegistry` v2 with
    canonical key-lists + Fx borrowed probes + caps `SLAB_MAX_KEYS=64`/`SHAPE_FANOUT_MAX=128`, the
    `ObjectStorage` slab/dict dual mode, GC two-arm trace + slab-cycle reclamation); 3 (VM wiring —
    slab-native `Op::NewObject`, the per-site `lit_shapes` cache, IC read/write over the slab, instance
    fields on the slab via `vm_instance_insert`, fuzzgen-gated mode counters; `resync_object_shape` +
    `resync_instance_shape` + `class_base_shape`/`object_shape_for` all DELETED in favor of precise per-key
    transitions); 4 (FxHash on the bounded VM interior tables — `class_methods`/`class_static_methods`/
    `class_defaults`/`user_globals` + registry — with `Map`/`Set`/dict-mode objects/decode paths KEEPING
    SipHash, §6.2 hash-flooding-DoS decision; `tests/shape_security.rs` 100k-hostile-key bound + Map-SipHash
    type proof); 5 (order-stress examples intro+advanced, fuzzer axis spread/delete/rest/wide-object +
    coverage assertion slab>0∧dict>0∧demote>0, negative-space `.aso`-unchanged guard); 6 (A/B + docs + merge).
  - **Field-type contract** for instances hoisted to the single shared `Interp::check_instance_field_contract`
    (byte-identical message/span on both engines).
  - **Performance (`bench/SHAPE_RESULTS.md`, same-session A/B, Gate 16):** **per-object alloc 13.0 → 2.0
    (6.5×, Gate 18)** — the mechanical core; `object_churn` **1.77×**; A/B geomean **1.089×**; peak RSS no
    regression; profiler object_churn hashing **14% → 0%**, alloc 17.6% → 5.7%. `json_roundtrip` **flat by
    design** (decode-born objects stay Dict/SipHash, spec §9 — recorded honestly, not hidden). Cap sweep
    (9 combos) showed zero sensitivity → kept defaults 64/128. Gate-12 spec/tw **4.2–4.3×** (≥2×);
    `dbg_zero_cost_gate` **0.994×** (≤1.05× — the dispatch loop's `NewObject`/prop arms changed).
  - **No grammar change, no `.aso`/opcode change** (`ASO_FORMAT_VERSION` stays **28**; guarded by
    `tests/shape_negative_space.rs` — version pin + `from_u8`-count Op-variant pin + round-trip; the
    `git diff main` audit shows only a +1 non-serializing `debug_assert` in `aso.rs`). No new `Value`
    variant; no tree-walker behavior change; demotion is one-way (no dict→slab re-promotion).
  - **Four/five-mode byte-identical** (tree-walker == specialized == generic == no-sync-lane == no-call-fast
    == `.aso`) over the full corpus + goldens, BOTH feature configs (443/0). Whole-effort holistic: GO.
  - **Bugs fixed in-branch failing-test-first:** Phase-0 `object.delete` stale-shape IC (four-way regression);
    3 production slab-panic stdlib sites (compress `entry_name_data`/`build_zip`, `ffi.alloc`) + 1 more found
    in review (`ai/json_schema`) + `interp.rs TestSummary::from_value`; 2 vacuous IC tests caught + fixed;
    the Op-count append blind-spot in the negative-space guard. NANB is now unblocked (SHAPE+CALL done).

- **DOCS** — ✅ MERGED to `main` (`--no-ff`). Documentation reconciliation + permanent drift tripwires.
  **Unit B (6 permanent drift tripwires in `tests/docs_drift.rs`):** (1) CLI-surface⊆cli.md
  (clap-introspected; 4 were RED-at-birth, turned green by Unit A); (2) env-var coverage (9 `ASCRIPT_*`
  vars — spec had 7; Phase-0 re-verify caught LANE's `ASCRIPT_NO_SYNC_LANE` and CALL's
  `ASCRIPT_NO_CALL_FAST` as drift, both added by Unit A); (3) module→page mapping (`MODULE_PAGES` table,
  validated both directions); (4) NAV⇄files bijection (no orphan pages, no missing NAV entries); (5)
  in-content link checker; (6) editor-pin consistency (zed/nvim tree-sitter SHA manual checklist) — 4
  tripwires green-at-birth with self-test mutation guards, 2 were RED (CLI-surface + env-var) and turned
  green by Unit A. **Unit A (one-time reconciliation):** `docs/content/cli.md` brought to full CLI parity
  — 27 previously undocumented flags, `ascript dap` subcommand, all 7 `pkg` subcommands; env-var section
  covering all 9 `ASCRIPT_*` vars incl. the 3 kill switches (`ASCRIPT_NO_SPECIALIZE` /
  `ASCRIPT_NO_SYNC_LANE` / `ASCRIPT_NO_CALL_FAST`) that were documented nowhere before DOCS;
  `task.pipe` added to `stdlib/async.md`; CLAUDE.md meta-drift fix ("stdlib pages mirror the source
  modules" → corrected to domain-grouped). **Seam:** clap CLI surface extracted to `src/cli_surface.rs`
  (behavior-identical move — the introspection seam for tripwire 1; vm_differential proves engines
  untouched). Gate 19 added. No engine change, no `.aso` change, `ASO_FORMAT_VERSION` unchanged.

- **DECODE** — ✅ **MERGED to `main` (`--no-ff`, `9a4cd76`)** from `feat/decoded-dispatch`; **Task-11 evidence gate
  executed — DOUBLE DROP by measurement; owner SHIPPED Units A+B default-on (recorded trade).**
  Pre-decoded instruction stream (Unit A) + data-driven superinstruction fusion (Unit B) ship for their
  **invalidation contract** (the byte-patch→drop-decoded-code `patch_epoch`/deps-epoch machinery — the
  JIT prerequisite, the spec's PRIMARY recorded purpose), NOT for a measured end-to-end speedup. The two
  speculative units BOTH failed their evidence gate and were reverted on their own same-session A/B data
  (`bench/DECODE_RESULTS.md`, Apple M4, env-toggle A/B on ONE binary, 7 runs/median, 8-workload profiling
  corpus). **No grammar change, no semantics change, `ASO_FORMAT_VERSION` unchanged at 28.**
  - **OWNER DECISION (2026-06-15, recorded verbatim):** **SHIP DECODE default-ON, accepting the ~2.3%
    whole-program regression** (decode-on geomean 0.977× vs decode-off; worst `func_pipeline` 0.933×).
    DECODE's value is the **invalidation contract — the JIT prerequisite** (the spec's primary recorded
    purpose), exercised on the REAL execution path; the `ASCRIPT_NO_DECODE` kill switch is the escape
    hatch. This is a **CONSCIOUSLY-ACCEPTED, recorded trade against the "zero perf regression" gate**
    (owner-noted per AskUserQuestion, 2026-06-15). Units C+D dropped by evidence (inline +0.45% < 2%;
    TOS −1.6%, object_churn −3.2%). The kill switch sits beside `--no-specialize` /
    `ASCRIPT_NO_SYNC_LANE` / `ASCRIPT_NO_CALL_FAST` as the complete byte-path floor.
  - **Units A+B (kept) — `ASCRIPT_NO_DECODE=1` vs default, isolated:** geomean **0.977×** (decode-on is
    ~2.3% SLOWER on the realistic corpus; worst `func_pipeline` −6.7%, `server_request` −5.0%). The
    pre-decode warm-up + frame-entry validity-check cost is not repaid by the flatter record stream at
    whole-program scale here. RSS flat (12–14 MB, no Gate-18 regression). Kept anyway: the deps-epoch
    invalidation contract + byte-patch battery (`tests/vm_decode.rs`) are the JIT precondition and are
    proven; the dispatch *speedup* a JIT would build on did not materialize from interpretation-level
    pre-decode.
  - **UNIT-C VERDICT (§6.7) — DROP.** Isolated speculative-inline win (`ASCRIPT_NO_DECODE_INLINE=1` vs
    default) = **+0.45% geomean on the call-heavy corpus** (`func_pipeline` +0.1%, `call_heavy` +0.8%;
    `object_churn` −2.7%) — **< 2% ship gate ⇒ DROPPED.** Reverted Task-9 feature commit `bd95cd7`
    (revert `6fa54d3`); KEPT the deps-epoch machinery + battery (Unit A §4's, verified present). Clean
    revert, zero conflicts.
  - **UNIT-D VERDICT (§7.5) — RECORD-REJECT.** Isolated TOS-cache win (`ASCRIPT_NO_DECODE_TOS=1` vs
    default) on the dispatch-bound trio = **−1.6% geomean** (object_churn **−3.2%, regresses past the
    0.97 bound**, func_pipeline −1.8%, call_heavy +0.1%) — fails BOTH ship conditions (≥2% AND no
    regression) ⇒ **RECORD-REJECT.** Reverted Task-10 feature commit `4611291` (revert `2065217`); the
    `stack_ops`/`tos_ops` census counters stay as evidence. The Task-8 residual `stack/decoded` of >1.2
    (object_churn) / ~1.5 (func_pipeline) was a real but non-sufficient signal — eliminating the residual
    push/pop did not pay against the per-edge flush bookkeeping + accessor indirection on this M4.
  - **Threshold A/B (§2.3):** thresholds 0/8/32 all within noise (0→8 = 1.001×, 32→8 = 0.999×) — **kept
    `DECODE_THRESHOLD = 8`** (no winner, placeholder stands).
  - **Gates (Task-12 final, branch green):** spec/tw geomean **4.02×** (≥2× Gate 12/17, 7/9 compute
    benches ≥2×, 2 alloc-bound exempt); `dbg_zero_cost_gate` **1.003×** (≤1.05×); `decode_on_off`
    microbench 1.014× REPORTED (owner-accepted; authoritative realistic A/B 0.977× in
    `bench/DECODE_RESULTS.md`); `vm_differential` **444/0** BOTH feature configs (7-way: tw == spec ==
    generic == lane-off == no-call-fast == decoded-forced == no-decode); `vm_decode` 12/0 (kept
    battery — invalidation + fusion; the flush-edge battery was reverted with Unit D, no dangling
    reference); `property` 27/0 BOTH configs + stress 2000 seeds 0 divergences; full suite + clippy
    clean BOTH configs; `ASO_FORMAT_VERSION` 28 unchanged; no grammar/disasm/verify/`.aso`/LSP/fmt
    change. New corpus example `examples/advanced/decode_hot_loop.as` (decoded+fused happy path),
    7-way + golden recorded.
  - **JIT-gate verdict (mandatory re-rank):** the Phase-0 ranking holds — `async_*` reactor/park-bound
    (~70%+), `json_roundtrip` alloc/hash-bound, `workflow_loop` fsync-bound (96%), the dispatch-bound trio
    already within a small constant of generic. Dispatch does NOT dominate whole-program time on the
    realistic corpus, and interpretation-level pre-decode did not move it. The JIT precondition DECODE
    delivers is the *invalidation contract* (shipped + proven), not a dispatch speedup; the JIT decision
    remains evidence-gated downstream.

- **NANB** — ⚖️ **EVIDENCE-REJECTED (the 16-byte repr); Phase 1 seam ✅ MERGED to `main`.** Two
  outcomes were first-class; PATH B (RECORD-REJECT) was executed because the measured A/B missed the
  fixed §8.1 SHIP bar.
  - **Phase 1 (the API seam) — SHIPPED on `main` (`7f4c862`, `--no-ff`).** `Value` became a sealed
    `pub struct Value(ValueRepr)` (enum module-private) reached only through total constructors + the
    `ValueKind`/`OwnedKind` borrowing/owning view (≈675 interp sites + 9 compile/repl/stdlib/worker
    files migrated off enum-matching). Proven **zero-cost** (geomean spec/tw 4.07× == pre-NANB
    baseline 4.00×; `dbg_zero_cost` 1.005×); size UNCHANGED at 24 B; `ASO_FORMAT_VERSION` 28; 444/0
    four-mode both configs. This is the permanent hygiene win and the cheap re-run path.
  - **Phases 2–3 (the `value16` 16-byte two-word repr: `ThinStr` single-alloc thin string + the
    `cfg(value16)` `AStr` payload) — built, fully proven CORRECT, then evidence-REJECTED.**
    Correctness GREEN: cross-BINARY 110/110 byte-identical (24 B vs 16 B, whole corpus,
    stdout/stderr/exit diffed); four-mode `vm_differential` 444/0 ×2 feature configs under `value16`;
    **300k-case deep fuzz × 8 engine modes, 0 divergences**; `ThinStr` Miri-clean; Gate-12 spec/tw
    4.03× and DBG 0.996× under `value16`.
  - **VERDICT (independent reviewer-of-record, against §8.1 fixed-before-measurement): STOP.**
    Criterion 1 FAIL (time geomean **1.005× spec** < ≥1.02× — rides noise); criterion 3 FAIL (peak
    RSS **1.001× / flat** < ≥5% improvement — the 24→16 B cell shrink is swamped by the ~12–14 MB
    runtime image + native buffers on the corpus); criterion 2 unconfirmable (STRING-subset geomean
    not isolated on the §8.2 string corpus); criteria 4 (tw 1.000×) + 5 (all correctness) PASS.
    No measured win ⇒ reject, mirroring the prior thin-`Str` reject (`COMPACT_VALUE_RESULTS.md`).
  - **Disposition:** `value16` repr NOT merged — frozen+flagged on `feat/value16` (pin commit). The
    repr-INDEPENDENT decimal-overflow fix the fuzz campaign surfaced (`apply_binop`/VM `decimal_fast`
    bare ops → checked ops raising recoverable Tier-2 `decimal <op> overflowed`; failing-test-first)
    landed on `main` separately. JIT precondition 2 (≤16 B) annotated **UNMET at 24 B**; REGION
    unblocked (representation final at 24 B). Numbers: `bench/NANB_RESULTS.md` "Phase 4"; spec §8.1
    GATE-VERDICT appended. **Gates (PATH-B `main`-applicable subset):** `vm_differential` 444/0 both
    configs; full suite + clippy clean both configs; `ASO_FORMAT_VERSION` 28 unchanged; no
    grammar/`.aso`/LSP/fmt change.

- **ELIDE** — ✅ **MERGED to `main` (`--no-ff`).** Contract elision via static proof: when the TYPE
  checker PROVES (the strict **(E)∧(Y)∧(A)** predicate — elide-safe destination ∧ `assignable==Yes` ∧
  argument *anchored*, deliberately stronger than raw `Yes`) that a call's args / an annotated let's
  initializer / a fn's returns satisfy their contracts, the runtime check is elided **identically on
  both engines** — VM: `Op::CallElided` + skipped `Op::CheckLocal` + `proto.ret=None`; tree-walker: a
  per-module AST marking pass (`Call.elide_args` / stripped `Stmt::Let.ty`/`Stmt::Fn.ret`) computed
  from the same source-derived `ElisionSet`. 6 phases, subagent-driven (fresh implementer + independent
  opus reviewer per task; per-phase holistic; whole-effort holistic). **DECISION (measured, §5.1.1):
  DEFAULT-OFF, opt-in `--elide` / `ASCRIPT_ELIDE=1`** — `ascript run` doesn't type-check, so enabling
  the collector on every run exceeded the §5.1 budget (corpus geomean **+6.99%** > 2%; collector 1.42 ms
  at 266 lines > 1 ms); honest recorded outcome (spec option b). Kill switch `--no-elide` /
  `ASCRIPT_NO_ELIDE=1` (force-off wins); paranoid audit mode `ASCRIPT_ELIDE_PARANOID=1` (runs elide-OFF,
  escalates a violated proof to `ELIDE proof violated (checker soundness bug):` on both engines, zero
  hot-path cost). **Headline opt-in win:** typed call-heavy **−6.0%** (`--elide` vs `--no-elide`), 66.7%
  elision rate; default path unchanged (Gate-12/17 spec/tw **3.92×** ≥ 2×, DBG zero-cost 1.004×, startup
  unchanged). `ASO_FORMAT_VERSION` **28→29** (`Op::CallElided` + verify/disasm/decode/bcanalysis arms).
  **Four real bugs caught + fixed in-branch (each failing-test-first, Gate 14):** (1) a rule-6
  `Class/ClassApp → Object` checker **unsoundness** (checker said `Yes`, runtime rejects instances —
  would have elided an enforced check) → `Yes`→`Unknown`; (2) a resolver `mutated`-flag gap (reassigned
  module **globals** read `mutated==false`, unsound anchoring) → `mutated_globals` set, behavior-neutral;
  (3) `mark_program` skipped `LetDestructure` RHS calls (compiler-consumed but tree-walker-unmarked —
  a cross-front-end divergence) — **found by the count-parity gate**; (4) `Op::CallElided` missing from
  LANE's `sync_lane_op()` + DECODE's block-terminator set (+19% untyped regression) — **found by the A/B**.
  **Gates:** the elide axis + **cross-axis (elide-on == elide-off)** joins the whole-corpus
  `vm_differential` (444/0 both configs) + the fuzz differential (8th config, 3843 programs / 301 s / 0
  divergences) + non-vacuous coverage/count-parity (56 typed files, 245 proven sites, collector ==
  compiler == marker); diagnostic-neutral collector (§6.5, byte-identical diagnostics over `examples/**`);
  paranoid corpus **zero escalations** both engines; a ~35-program adversarial predicate attack produced
  **no** elide-on/off divergence. Full suite + clippy clean both configs; Gate-5 0 `type-*` on
  `examples/**` both configs (incl. 2 new typed examples); fmt/LSP/REPL parity; **no grammar / tree-sitter
  change** (internal AST field only). Docs: `type-contracts.md` "Annotations and performance",
  `bench/ELIDE_RESULTS.md` (baseline/envelope/decision/headline), CLAUDE.md + roadmap + spec §5.1.1.
  REPL / worker isolates / DAP / `--parallel` keep FULL checks (never elide). "Raw `Yes` is not a proof"
  recorded as a warning for future checker work.

- **PAR** — ✅ **MERGED to `main` (`--no-ff`).** Data-parallel primitives over the shipped worker pool:
  `task.pmap(data, f, opts?) -> future<array>` + `task.preduce(data, f, init, opts?) -> future<T>`,
  ordinary `std/task` functions. **STDLIB-ONLY — no syntax, no opcode, no `.aso` change (`ASO_FORMAT_VERSION`
  stays 29), no new worker-wire tag** (`tests/par_negative_space.rs` pins all three). `ChunkJob` rides
  `WorkerRequest` as plain `Send` fields; a native `run_chunk_job` driver in the isolate loop maps/reduces a
  chunk's slice; `dispatch_worker`'s public signature is unchanged (delegates to `dispatch_worker_job(.., None)`).
  **Input (the §3.1 freeze-or-copy decision, NOT auto-freeze):** a frozen array → `Arc`-bump zero-copy
  (TAG_SHARED, O(1)/chunk, 2.01× faster at 1M); a plain array → per-chunk structured-clone copy. Callback = a
  **named TOP-LEVEL `worker fn`** (a `static worker fn` is rejected at the `worker_fn_dispatch_name` value gate
  with the §2.2 message — fixed in-branch, byte-identical both engines, also fixing `run_in_worker`'s latent
  leak). Input-order merge; first-by-input-order errors; venue-invariant inline nesting (an isolate runs the
  SAME chunk decomposition, never blocks on its own pool); cancel-on-drop. **`preduce`:** each chunk folds
  seeded by its own first element; `init` participates EXACTLY once (the single final combine); `f` must be
  associative to equal sequential reduce (the §3.8 contract, byte-identical reproducibility under pinned
  `{chunks}`). 5 phases, subagent-driven (fresh implementer + independent opus reviewer per task; per-phase +
  whole-effort holistic). **Two spec PROSE corrections recorded in-branch** (both match shipped reality, no
  recorded-semantics change): a worker body `?` yields the `[nil, err]` PAIR (run_body converts Propagate→Ok;
  the isolate Propagate→nil arm is dead, kept to mirror), and plain instances cross the airlock FIELD-ONLY
  (methods not shipped — a Spec A limitation, not PAR). **Headline (`bench/DATA_PARALLEL_RESULTS.md`):** scaling
  **4.28× @ 8 workers** (1.94×/3.16×/4.28× at 2/4/8), ≈ the hand-rolled `gather(map)`; break-even ~1000
  LCG-iters/element (below it sequential wins — the honest non-goal); frozen-vs-plain 2.01× at 1M; **Gate-12
  spec/tw geomean 3.87× ≥ 2×** (PAR touched no engine path — proof, not assumption). **Gates:** the new
  examples join the whole-corpus differential four-mode byte-identical; `vm_differential` 444/0 both feature
  configs; full suite + clippy clean both configs; Gate-5 0 `type-*` on `examples/**` both configs (incl. 2
  new examples); `par_negative_space` (ASO 29 / wire-tag / opcode-count pins) green; the §4 failure-mode table
  re-probed row-for-row VM==tree-walker. Docs: `async.md` "Data parallelism" (verbatim §3.8 contract + chunk
  formula), `workers.md`, README, CLAUDE.md + roadmap. **Recorded pre-existing (NOT a PAR blocker, route to a
  future worker-hardening task):** two DIFFERENT `worker fn`s sharing a top-level helper hit a `DefineGlobal`
  redeclaration panic on a WARM pool isolate — reproduces with plain pooled `worker fn` calls (zero PAR
  involvement); the worker code-shipping slice re-defines an already-defined top-level helper.

- **REGION** — ⚖️ **EVIDENCE-REJECTED (NO-GO); spike frozen on `feat/task-regions` (unmerged).** Task-scoped
  region allocation, executed as the spec's PROBE → NARROW-PROTOTYPE → GO/NO-GO spike. **Phase 0:** identity
  audit (semantics unchanged post-NANB, NO persistent container-address-keyed table — the spike's premise
  holds) + a `region-probe` cfg-gated allocation-lifetime instrument; the §5.3 checkpoint **passed** (server
  workload 40% literal-in-task ≥ 25%; json_roundtrip 0% — all serde-native), independently verified. **Phase
  1:** the narrow recycler — vendored gcmodule `strong_count()` (path-patch, G6 production decision deferred),
  `region_candidates` kill-site analysis + lazy `region_kills` bitmap (runtime-only, ASO 29 unchanged),
  `RegionPool` reusing proven-dead `ObjectCell`s at `strong_count()==1` kill sites behind `region-spike` +
  `ASCRIPT_NO_REGIONS`. Proven SOUND + byte-invisible: region-ON `vm_differential` **444/0** over the whole
  corpus (region-on == tree-walker oracle == generic), an adversarial review found no shape-staleness /
  refcount-guard / frozen-leak divergence, and `region_escape` recycles 1,999,960 cells byte-identically (the
  mechanism WORKS). **Phase 2 A/B → NO-GO:** §5.5 **G1 FAILS decisively** — recycled=0 on BOTH gate workloads
  (independently reproduced): `json_roundtrip` is 100% native-serde construction (no VM literal fires);
  `server_request`'s `resp` is module-scope + a `json.stringify(resp)` Call-arg sink (statically disqualified
  per §3.1/§4). 0% allocation-time reduction, wall not improved (json +0.00%, server +0.60%). G2 (escape <5%,
  object_churn ok), G3 (regions-off ≈1.00× geomean), G4 (identity battery + differential green), G5 (RSS not
  regressed, overflow=0) all PASS; G6 moot on a NO-GO. **The ~45% alloc/gc CPU headroom lives in native-serde
  + Call-escaping allocations a bytecode-literal recycler provably cannot touch** — confirming the lock-record
  prediction (promote-on-escape killed on identity grounds). Branch frozen+pushed (`origin/feat/task-regions`)
  as the cheap re-run path; the vendored gcmodule fork was spike-local (does not ship). A first-class honored
  evidence outcome (the VAL/NANB precedent). Evidence: `bench/REGION_RESULTS.md` + `bench/REGION_PROBE.md`;
  spec Status → evidence-rejected.

- **WARM** — ✅ **MERGED to `main` (`--no-ff`, `02cf14c`).** Warm starts & durable-log throughput — three
  independent, behaviour-invisible units; **no `ASO_FORMAT_VERSION` bump (stays 29), no `ARCHIVE_VERSION`
  bump (stays 1), tree-walker behaviorally untouched** (`git diff main -- src/interp.rs` empty). **Unit A —
  compile cache** (CLI-side, `src/cache/`): content-addressed cache under `$ASCRIPT_CACHE/compiled/`, keyed
  on source + the transitive module graph (`collect_module_graph`, a parallel re-derivation of
  `compile_path_module_set` kept ≡ by the §2.5 walk-drift tripwire) + flags + lockfile; `CompileCacheKey`
  (`ck1-`). **Fail-open + verify-on-hit** — a corrupt/hostile entry degrades to a normal compile; applies to
  the plain `.as`-on-VM path only. `--no-cache` / `ASCRIPT_NO_COMPILE_CACHE`; `ascript cache clean|dir`.
  CLI-only → `vm_differential` untouched. **8.0× warm @ N=500, +60ms cold tax.** **Unit B — PGO**
  (`src/vm/{pgo,run,shape}.rs`): `build --pgo` runs the program as a real training workload, harvests warmed
  IC/adaptive-arith state from live `FnProto`s, appends a self-described `ASPGO` section riding **OUTSIDE**
  the `ModuleArchive` codec (count-bomb / hostile-byte safe); `seed_chunk` re-installs behind every
  specialization guard (DERIVED field index, digest-checked) — **byte-INVISIBLE** (a build without `--pgo`
  is byte-identical, a seeded run byte-identical to unseeded across all engines). Seeded-PGO joins
  `vm_differential` as the **445/0** axis (both configs); `ASCRIPT_NO_PGO`. Steady-state seeded/unseeded
  **1.007×** (no archive-load-path tax — Gate 17). **Unit C — workflow durability** (`workflow`-gated;
  `src/stdlib/workflow.rs` + CORE `src/det.rs`): `Durability::{Fsync (default, unchanged), Group, Buffered}`
  via ONE `record_event` chokepoint; crc-framed group appender with torn-tail **prefix repair**; at-least-once
  activity contract; `det.rs` chokepoint compiles under `--no-default-features`. **Group ≈98.85× faster than
  fsync** on per-commit shapes; default `"fsync"` ≈ baseline; kill-9 battery green (×5 stable). **Whole-effort
  holistic review (independent opus, ran the suites + reproduced edges): PASS on all 6 focus areas with ONE
  blocker found + fixed in-branch failing-test-first** — `parse_hex32` panicked on a hostile manifest with a
  multibyte char at an even byte offset (byte-length check but byte-slice); fixed with a `!s.is_ascii()` guard
  + 3 regression tests (§2.9 "corrupt manifest → MISS, never crash"). **Two incidental pre-existing fixes**
  (campaign rule #1): the `worker_serialize` fuzz target's NANB-era Value-API drift (swapped to the public
  lowercase constructors — the isolated fuzz workspace now builds) and a missing `corpus/pgo_section/*`
  gitignore stanza. **Gates:** clippy clean both configs; full suite both configs (47 binaries, 0 fail);
  `vm_differential` **445/0** both configs; compile_cache 20/0, pgo 8/0, workflow_durability 21/0;
  `pgo_section` fuzz 858K runs / 60s, 0 findings; spec/tw geomean **4.13×** (≥2×), DBG zero-cost **1.002×**;
  WARM_RESULTS.md complete (3 unit tables + RSS + same-session methodology). **Spec-prose deltas recorded** (no
  recorded-semantics change): `collect_module_graph` is a parallel re-derivation (not a literal extraction);
  the §3 "ASO v27 unchanged" references were stale at authoring time (the constant was already 29 via ELIDE) —
  the binding invariant "WARM introduces no constant change vs `main`" holds. Follow-ups recorded in the
  roadmap (none silent): cache auto-GC, PGO profile merging, method-IC seeding, group-mode background flusher.

- **RT** — ✅ **MERGED to `main` (`--no-ff`, `349f4ce`).** Runtime-only native stubs — CLI/link-level, **no
  engine change** (`ASO_FORMAT_VERSION` 29 + `ARCHIVE_VERSION` 1 unchanged; `vm_differential` 445/0 BOTH feature
  configs with the cfg additions present — the `ascript_rt` cfg is never set under `cargo test`). **The
  architectural keystone:** the front-end (parsers, compiler, checker, LSP/DAP/fmt/REPL/pkg, tree-sitter) is
  compiled OUT of the `ascript-rt` bin via a **build-time cfg `ascript_rt`** (NOT a Cargo feature — features are
  additive and `--no-default-features` must keep building the parsers; emitted by `build.rs` from `ASCRIPT_RT=1`,
  the `fuzzing`-cfg precedent). The §2.3 audit enumerated every compiler-reaching runtime path and cfg-gated each
  to a loud refusal; the **holistic review found a third path (`Interp::load_module`) the spec folded into row
  (g) — also correctly gated (the impl gates MORE than the two promised)**; an `nm` tripwire proves a stub has 0
  `compile_source`/tree-sitter symbols (full toolchain = 4). **12 tasks, subagent-driven** (fresh implementer +
  independent opus reviewer per task; per-task SABOTAGE of every fail-closed path; whole-effort holistic).
  **4-tier matrix** (rt-core **5.75 MB = 13.3%** of 43.3 MB .. rt-local 32% .. rt-net 47% .. rt-full 75.3%)
  selected by the archive's own import facts through a **drift-tested module→feature table** (3 gates,
  sabotage-proven; `closure_drift` made bidirectional in review). **Fail-closed supply chain:** an
  ed25519-signed, version-locked release manifest (compiled-in pubkey, **no insecure env knob**; the signing
  half rides a default-OFF `rt-release` feature — `nm`-proven absent from a stub), a content-addressed cache
  that **re-hashes on load** (never trusts by path), a **5-rung resolution ladder** (`--stub` → cache → fetch →
  dev-sibling → `current_exe`) where **integrity failures ABORT and only availability failures fall through** (a
  tampered stub never recovers to a weaker rung). Reviewers SABOTAGE-proved every integrity gate (disable
  signature/version-lock/re-hash → the matching refusal test fails). **Footer flags** (`reserved`→`flags`,
  `FLAG_ZSTD`; `BUNDLE_FOOTER_VERSION` 1→2 only for compressed; `flags=0` byte-identical to pre-RT) power
  **`--compress`** (zstd, bounded 512 MB decompress, exact-length). Plus un-rejected **`--target`** cross builds
  (platform-independent payload — same bytes onto any stub; macOS sign-before-append means prebuilt darwin
  stubs append cleanly with no host signing), **`--exact`** (local-cargo precise-feature stub, sign-before-cache
  + content-addressed reuse), **`--oci`** (a deterministic, Docker-less OCI image tarball — hand-rolled USTAR +
  the two-digest rule; musl-only/scratch; the reviewer validated a produced tarball with **skopeo + docker
  load**), reproducible outputs (cross-flag double-build battery — every form bit-identical, sabotage-proven
  non-vacuous), and **`--report-json`** (a schema-locked build report). **Recovered TWO crashed/cut-off
  implementer agents** (T6 1st-attempt API crash → 2nd completed; T7 ended mid-verify → I verified+committed).
  **Worktree hygiene:** the subagent-driven skill self-creates git worktrees; reclaimed 23 GB of stale ones,
  then instructed implementers to commit directly. **Cross-task interactions verified compose** (`--compress
  --oci --stub` byte-identical rebuild; `--stub`-onto-a-bundle overlay-stripped; the WARM compile cache
  verified to never cache an RT `--native`/`--oci` artifact). **Spec-prose deltas (no recorded-semantics
  change):** the §2.3(a) disk-recompile scenario is architecturally unreachable via `build --native` (the
  archive embeds every static import; `load_file_module` resolves archive-first) — the gate is a defensive
  backstop, string-proven in the stub, unchanged; the §13 grounding's "ASO v27" is stale (real 29, code +
  `--rt-info` report 29). **FINAL gates all green:** clippy clean default/`--no-default-features`/`--features
  rt-release`; full suite both configs; `native` 37/0 (rewritten `--target` pin); the five `rt_*` suites green
  against a real stub; `vm_bench` spec/tw geomean ≥2× (RT touches no engine); `RT_SIZE_RESULTS.md` complete;
  `docs_drift` green (bundles.md + cli.md, no new NAV — Gate 13). **Musl-feasibility spike** failed locally as
  RT §12 predicted (no musl cross-linker on a macOS host) → validated at the first CI release run; narrow-
  fallback recorded in the spec header + roadmap (never a silent absent artifact). **Recorded futures:** SBOM
  for `--oci`, registry-push (`--push`), tree-walker-eval carve-out if Phase-0-material, musl-matrix narrowing.
- **RESIL** — ✅ **MERGED to `main` (`--no-ff`, `b3fec2d`)** from `feat/resilience-stdlib`. `std/resilience` —
  composable per-isolate backend policies (`resilience` feature, default-on). **NO `.aso`/opcode/grammar change**
  (`ASO_FORMAT_VERSION` 29 unchanged); `vm_differential` **445/0 BOTH feature configs** every step. Six policy kinds
  (breaker/limiter/keyedLimiter/bulkhead/retry/memoize) as **tagged Objects** routed through a **call-position-only
  hook** mirroring `std/schema` (hook ladder: schema FIRST then resilience — disjoint tags+method sets, pinned);
  module fns fallback/singleflight/deadline/withTrace/metricsHandler/health/handler. Substrates reused not rebuilt
  (breaker ring + `sync.semaphore` + `std/lru` + `SharedFuture`). **THE engine seam = `TASK_LOCALS`** (CORE
  `tokio::task_local!`, NOT feature-gated): copy-on-spawn at the **5 user-code async spawn sites** + `ambient_root_
  scope` (renamed from `telemetry_root_scope`) at EVERY entry point. Zero-cost when unused — every consult is one
  TLS `try_with` → `None` fast; the §5.4 deadline-aware I/O sites + limiter/bulkhead parks all take the `None`
  branch unchanged. Time via det-routed `clock_monotonic_ms` → Record/Replay verdicts replay byte-identically (§8
  probe `tests/resil_determinism_probe.rs`); the enforcement sleep is timing-only. Per-isolate honesty (§7): N
  workers = N copies, `__local` marker = loud field-path panic on a worker boundary, actor pattern for global state.
  Always-on per-isolate metrics registry + `#[cfg(telemetry)]` mirror + `#[cfg(log)]` breaker breadcrumb;
  `metricsHandler`/`health`/`handler` are `NativeKind::Resilience` `NativeMethod`s (Prometheus 0.0.4; 429/503/504
  map; `required_cap`=`None` → serve under `--sandbox`). **6 phases, subagent-driven** (fresh implementer +
  independent opus reviewer per phase + a final holistic). **Reviews caught & fixed 2 real four-mode/integration
  gaps:** the CLI tree-walker module load (`lib.rs`) lacked `ambient_root_scope` → `deadline` silently no-op'd vs the
  VM (CRITICAL, fixed + `tests/cli.rs` regression); each http connection task lacked the scope → `handler({deadlineMs})`
  no-op'd in-server (fixed + `/slow`+`/slowasync` 504 e2e proofs). **Two Gate-14 carry-forward fixes landed:** the VM
  async-spawn sites previously lacked the `telemetry_scope` wrap the tree-walker had (span lineage now matches;
  regression in `tests/telemetry.rs`), and a stale telemetry doc-comment. **Corrected a misdiagnosis:** the alleged
  "module-call-in-native-async-closure" gap was just `task.sleep` not existing (the sleep fn is `time.sleep`) — the
  async deadline RACE works fully on both engines, proven in-server (`/slowasync` deadlineMs:50 over an 800 ms body
  → 504). `task.retry` gained v2 keys (additive, v1 bit-identical, Phase-0 pins green). **Zero-cost gate**
  (`bench/RESILIENCE_RESULTS.md`, same-session cross-binary vs the `11a5d7d` merge-base): worst-case 1M-empty-spawn
  microbench **1.024× wall** (within the 1.05× DBG bound; ~80 ns/spawn user CPU, unmeasurable on real work), RSS
  flat 1.011×; in-process compute-floor spec/tw geomean **5.32× ≥ 2×**; DBG/LANE/DECODE gates unperturbed.
  **FINAL gates all green:** clippy clean default/`--no-default-features`/`--features resilience`; full suite both
  configs (4527/0); `vm_differential` 445/0 both; examples `examples/resilience.as` + `examples/advanced/resilient_
  gateway.as` four-mode byte-identical, fmt-idempotent, check 0-diag (in-corpus, goldens); `resil_negative_space`
  (ASO-29 + no-opcode + hook-order + OptMember + retry-v1 pins); `docs_drift` green (resilience.md + NAV). Examples
  honesty: the bulkhead-SHED demo is concurrency-driven (`spawn(async () => bh.run())` hangs under the live CLI —
  M17 spawn-driving; shed is server-tested in `resil_handler_server`). **Recorded follow-up:** redis deadline-mid-op
  abandon may reuse a connection that ideally should be discarded (honestly documented at `src/stdlib/redis.rs`;
  low-risk, narrow window). Parked per spec: hedged requests, AIMD adaptive concurrency, `std/k8s`.
- **CNTR** — ✅ **MERGED to `main` (`--no-ff`, `6e22800`)** from `feat/containers-docker`. Container-native runtime +
  `std/docker` (`docker` feature = `["net","data"]`, default-on). **NO `.aso`/opcode/grammar change**
  (`ASO_FORMAT_VERSION` 29; `vm_differential` **445/0 BOTH feature configs** every step; `src/vm/run.rs` untouched).
  **The cap chokepoint is now a CONJUNCTION:** `required_cap`/`NativeKind::governing_caps` return a `CapReq` (a
  `Copy(u8)` bitset newtype with `NONE`/`one`/`and`/`is_empty`/`iter`-in-`Cap::ALL`-order) instead of `Option<Cap>`,
  so `docker` requires **net ∧ process** (the docker socket is host-root-equivalent); the gate at `call_stdlib` + the
  per-handle re-check at `call_native_method` iterate `…iter()` behind the **unchanged `!all_granted()`
  short-circuit** → the all-granted default is byte-identical (single-cap = one iteration; `cap_audit` 100% green =
  verdict-preservation). **`std/net/unix`** (UDS connect/listen, the `net_tcp.rs` structural mirror, Drop-unlinks the
  bound path) + a stage-2 `check_unix_path` `unix:<canonical>` carve-out. **`src/stdlib/http1.rs`** — a small HARDENED
  HTTP/1.1 client codec (generic over the transport; bounds 64KiB/256/16MiB; hostile→clean Tier-1, never
  panic/hang/OOM; `read_to_end` never pre-`reserve`s an attacker length; `101`→`Upgraded{transport,leftover}`); HTTP
  over UDS, NOT reqwest. `std/net/http` routes `{socketPath}` through it (`call_http_send_uds`, surface-identical incl.
  `errorOnStatus`/stream/json). **`std/docker`** (`src/stdlib/docker.rs`): connect (socket resolution
  opts→`$DOCKER_HOST`→`/var/run/docker.sock`; `tcp://`→Tier-1) + version negotiation (clamp `[1.24,1.43]`) + the
  unary table + `logs`/`events`/`pull`/exec streams over the **8-byte multiplexed demux** (Multiplexed/Tty
  auto-detect, oversize-no-alloc + partial-frame reassembly + truncation→Tier-1; `NativeKind::DockerStream` +
  `native_stream_method=>Some("next")` makes `for await` work on BOTH engines); exec via the `Upgraded` hijack. Return
  shapes per §4.2 (`ping`→`[true]`, `wait`→`[StatusCode int]`). `DockerClient`/`DockerStream` `governing_caps` =
  net∧process; all four new handles GC-untraced + non-sendable. Hermetic **recorded-fixture mock Engine daemon**
  (`tests/docker.rs`) → the whole module is testable with NO Docker; live tests env-gated on `ASCRIPT_DOCKER_LIVE=1`.
  **Inbound signals + graceful drain** (§6–§7): `process.on`/`off` (`tokio::signal`, MAIN-ISOLATE only; `off` →
  emulated `exit(128+signo)`; the listeners are daemon tasks ABORTED at program end via `abort_signal_listeners()`
  before the `local.await` drain — **a review-caught Critical: a registered handler otherwise hung the process at
  exit**, fixed `0811668`). `srv.shutdown()` + `serve({onShutdown,drainTimeout})` graceful drain: accept_loop
  predicate generalized `budget==0` → `budget==0 || shutdown.is_armed()` with the **lost-wakeup
  register→`enable()`→recheck sequence PRESERVED** (the existing server battery byte-identical = the proof);
  `onShutdown` once (`AtomicBool::swap`); drain awaits in-flight raced vs `drainTimeout`. **cgroup-aware sizing** (§8):
  `effective_parallelism()` = `$ASCRIPT_WORKERS || min(num_cpus, cgroup_quota)` (cgroup v2 `cpu.max` / v1
  `cfs_quota`, Linux-only → `None` elsewhere = non-Linux byte-identical to `main`) swapped into the 4 pool/worker
  sizing sites; `os.inContainer()`. `ascript init --template server`. **8 phases, subagent-driven** (fresh implementer
  + independent opus reviewer per phase + a final holistic). **Reviews caught & FIXED real bugs:** the exit-hang
  (above); an `errorOnStatus`-ignored silent divergence on the UDS path; the `rt_select` module→feature drift RED
  (CNTR added `std/net/unix`+`std/docker` to `STD_MODULES` without the RT stub table). The final holistic tried hard
  to **skip the net∧process gate** (handle method, stream `next`, `for await`, `net.connect("unix:")`,
  `http.request({socketPath})`) and **could not** — every UDS-open site is gated. **Two spec-prose corrections** (no
  recorded-semantics change): the plan's `ASO == 27` is stale (real 29, pinned 29); the docker examples test via the
  built binary (the in-process VM entry points don't abort signal listeners — a harness property, not an engine
  divergence). **Perf** (`bench/CNTR_RESULTS.md`, same-session cross-binary vs `5bdb24b`): the cap-gate **mechanism is
  zero-cost** (bisected: `ff65c5b` = 1.00×); pure-compute flat; whole-program + **real-program A/B flat** (`json_adt`
  startup-dominated, no measurable delta); RSS flat; in-process `vm_bench` gate PASS (spec/tw ≥2× + DBG armed/none
  ≤1.05×). **OWNER-VISIBLE finding:** a synthetic 100%-stdlib-call-spam microbench carries a **~5–11% code-layout
  tax** — bisection-confirmed to be the `net`-essential http1/UDS **code volume** shifting the large `call_stdlib`
  function's layout (NOT the cap gate; NOT `docker`-feature-gate-avoidable since http1 is `net`-essential), invisible
  in every real/whole-program workload. Accepted as the **DECODE-precedent class** (DECODE shipped a 2.3×
  *whole-program* layout tax with an owner note; CNTR's whole-program is flat). Examples
  `examples/{docker_info,advanced/docker_supervisor}.as`; docs `docs/content/{deploying,stdlib/docker}.md`.
  **Recorded ENABLED follow-ups** (RT+RESIL merged): rt-stub/`--oci` scratch image base for the Dockerfiles; the
  template `/proxy` upgrade to `std/resilience`; a `docker.md` note pointing at `task.timeout` for an unresponsive
  daemon (the docker calls have no built-in per-call read timeout — non-blocking, the daemon is trusted/local).

- **SIG** — ✅ **MERGED to `main` (`--no-ff`, `11cdb6a`)** from `feat/lsp-stdlib-signatures`. Stdlib signature table +
  LSP signature/completion/hover enrichment + audit hardening. **LSP/checker-static-only — ZERO engine/grammar/fmt/
  `.aso` surface** (`git diff main -- src/vm src/interp.rs src/compile src/syntax src/value.rs src/gc.rs` EMPTY;
  `ASO_FORMAT_VERSION` 29 unchanged; `vm_differential` trivially preserved — never touched). **Unit A — the data
  asset:** `src/check/std_sigs.rs`, a curated `&'static` signature table (params + optionality/variadic + return +
  one-line doc) for **all ~60 `STD_MODULES` exports + 10 global builtins + handle-method rows** (ffi/docker). Authored
  with const-fn `StdParam` constructors (`req`/`req_untyped`/`opt`/`with_default`/`variadic`) + a `validate_param_order`
  ordering guard — NOT a macro (spec granted latitude). The MACHINE source of truth; the docs pages stay the SOCIAL
  source; **two drift-test families bind them** — the in-module bidirectional completeness pair (every export ⇄ a
  kind-consistent row, both feature configs) + `tests/std_sigs_docs.rs` (a tolerant Style-1/Style-2 docs parser, 295
  facts/283 matched, a comparator self-test, a Style-1 full-coverage guard). `std_arity.rs` **subsumed**: `std_fn_arity`
  is now a thin derivation (`min` = leading non-optional non-variadic param count, `max=None`) over the table — one
  source of truth, pinned by a 61-entry no-behavior-change test (the campaign's CNTR/PAR/RESIL additions made the plan's
  published ~36-entry list stale; the full current 61 were pinned). **Unit B — three consumers read the table:**
  `signatureHelp` gained a resolution ladder over a `MemberExpr`-extended `enclosing_call` (`Callee::{Named|Member}`):
  same-file `FnDecl` → `builtin_sig` → cross-file `exported_fn_signature_by_import` (param names + annotations from the
  workspace `ParamList` walk) → namespace-import + `std_sig` → typed-receiver method; active-param advances on `,` +
  clamps to a variadic `...rest`; UTF-16 `LabelOffsets`; one-line docs. `completion` member items carry real
  `FUNCTION`/`CONSTANT` kind + signature detail + lazy-resolved docs (from `module_members` — works under a core build
  where the runtime export is compiled out), auto-import is a cached `OnceLock` list deprioritized via `sort_text="zz…"`,
  partial-identifier member completion (`math.sq` offers `sqrt`), string/comment suppression. `hover` shows stdlib-member
  signatures + docs. **One shared `std_sigs::render_param`** across all three surfaces (char-identical optional-`?`/
  variadic-`...` rendering — holistic-verified). **Unit C — audit hardening C1–C8:** per-model `OnceLock<InferArtifacts>`
  inference cache (factored `hover_type_at` → `build_artifacts`+`hover_type_in`; `Table` is `!Send` so only the rendered
  hover spans are cached — `SemanticModel` stays `Send+Sync`) + hover size-class gate; `workspace_diagnostic` yields +
  reuses open models; folder-removal unindex; fs-canonical index keys (symlink-correct); typeHierarchy decision +
  index poison-log (`AtomicBool` once); `snippetSupport` gating (capability-less clients get plain bodies — a deliberate
  behavior change, existing snippet assertions relocated to a snippet-enabled test). **6 phases (0–4 + holistics),
  subagent-driven** (fresh implementer + independent **opus** reviewer per task; per-phase + whole-effort holistics).
  **Reviews caught & FIXED real defects failing-test-first:** (1) signature **nested-call selection** — the `+2` slop
  for unterminated calls let a *completed* inner call win past its own `)` (`pow(abs(x), 2)` showed `abs` over pow's
  second arg) → bound terminated arg-lists by the `)` position; (2) **three table↔docs reconciliation regressions** — the
  docs-drift implementer "fixed" docs to match a WRONG table (`bytes` endian made required, `set.from`/`date.parse`/
  `date.format` optionals) → reverted to true behavior in BOTH (these would have become call-arity false-positives once
  `std_arity` derived min-arity — the drift test only checks docs==table, NOT table==source-optionality, so a reviewer
  must audit optionality vs `src/stdlib/<mod>.rs` `call()`); (3) signature-help vs completion **render divergence**
  (`end: number` vs `end?: number`) → extracted the shared `render_param`; (4) a **narrowest-span coverage gap** (the
  parity test couldn't catch a `min→max` bug in the shared `hover_type_in`) → a synthetic overlapping-spans guard. **An
  important campaign-methodology finding recorded:** the self-dev-dep `ascript = {path=".", features=["fuzzgen"]}` lacks
  `default-features=false`, so `cargo test --no-default-features` / `cargo clippy --no-default-features --all-targets`
  RE-UNIFY default features (dev-deps load) and do NOT exercise the core-only config — the TRUE core check is
  `cargo build/clippy --no-default-features --lib` (confirmed: the core-only lib compiles clean). SIG is unaffected
  (static, 0 `cfg` gates), but the "both configs" gate has had this hole campaign-wide. **Gates:** full suite + clippy
  clean BOTH feature configs; core-only `--lib` compiles; **Gate-5 zero `examples/**` diagnostics** (the std_arity
  widening from ~36 to every curated fn introduced no FP — the broad table-optionality tripwire); tree-sitter + frontend
  conformance green (untouched); the table↔docs drift tripwire **scratch-probe-confirmed live** (a fake export with no
  row fails CI). Docs: `docs/content/tooling/lsp-capabilities.md` + CLAUDE.md + roadmap (no NAV change). **v1 narrowings
  (documented, not silent):** cross-file sig help needs the calling file to parse cleanly (the import edge is recorded
  from a clean parse; in-file/stdlib rungs work on unterminated calls); handle-method/complex-receiver sig help deferred
  (the curated handle-method rows exist so v2 costs no data work).

## Execution order

```
DEFER (first — owner decision: unwind semantics are paid ONCE, pre-two-lane; ASO → 28)
  ║  (SHAPE may run in a PARALLEL branch — disjoint surfaces, no unwind paths)
  ▼
Phase 0 (bench corpus, in LANE Task 0)
LANE ──> CALL ──┬──> NANB ──> REGION (spike-gated)
                ├──> DECODE ──> (re-profile) ──> EXEC? ──> JIT?
SHAPE ──────────┘                                  (each gate: evidence)
ELIDE, PAR, WARM — independent; schedule alongside any wave.
SIG, DOCS (DX track) — independent of ALL engine specs and of each other; owner-sequenced
(SIG after the engine waves; DOCS any time — its tripwires guard every later spec's docs gate).
Deployment & reach: RT first (track foundation), CNTR's RT/RESIL-dependent tasks after those
merge; RESIL/EMBED/WASM(spike)/REPLAY/BATT/LSPEC independent of the engine waves.
```

**Lock record (2026-06-12).** All 21 specs + plans were cross-reviewed for seam consistency
(dependency graph, shared vocabulary, ownership boundaries, env/feature/module namespaces,
ASO-version arithmetic, sequencing) and the named amendments applied; all specs are LOCKED.
Owner decisions recorded at lock: **DEFER lands first** (before LANE; SHAPE parallel-allowed);
**RT owns its own minimal `--oci` tar writer** (no BATT edge; unification optional later);
env-var convention: kill switches are `ASCRIPT_NO_*`, value-selectors are value-style.
**Where an entry summary below disagrees with its spec, THE SPEC IS AUTHORITATIVE.** Known
summary-level corrections (entries predate the specs): ELIDE — BOTH engines elide identically
(`Op::CallElided`, ASO bump); NANB — 16-byte two-word Candidate B ships first, the 8-byte
NaN-box is a double-gated follow-up, and the JIT value-precondition is satisfied only if NANB's
gate passes; REGION — promote-on-escape was KILLED on identity grounds (the spike is proven-dead
`Cc`-cell recycling); WARM — today's workflow log is one snapshot+fsync per run (group mode
*introduces* appends and strictly improves mid-run durability); DECODE — carries Unit D (TOS
caching, evidence-gated); REPLAY — record mode is a deterministic-mode run and the I/O-recording
scope is answered in-spec; LANE — the inflight-yield framing is corrected in its §3; SIG — the
min-arity table is ~36 entries, not ~80.

LANE and SHAPE may proceed in parallel branches; CALL rebases on LANE; NANB starts only after
SHAPE merges. Re-profile checkpoints (after CALL, after DECODE) are mandatory campaign events:
each produces a `bench/PROFILING_RESULTS` update that re-ranks the remaining specs — **the order
above is a hypothesis the measurements are allowed to overturn.**

## How to work (per spec — inherited unchanged from `goal.md` / the workers cadence)

- **Spec → review → lock → plan → subagent-driven-development → independent review → holistic
  review → merge `--no-ff`.** Fresh implementer per task; an *independent* reviewer that runs
  commands and probes edges; don't skip the gate; check off plan checkboxes.
- **TDD, DRY, YAGNI, frequent commits** (house trailer per `CLAUDE.md`).
- **One feature branch per spec, off `main`.** Merge when that spec's whole plan is green.

## Gates (non-negotiable — fix the code, never the assertion)

**Gates 1–14 of `goal.md` apply verbatim to every spec in this campaign** (four-mode
byte-identity; clippy clean both configs; tests green both configs; no borrow across await;
zero `type-*` corpus false positives; no placeholders/silent deferrals; corpus migrated never
deleted; continuous infra green; examples happy+edge; unit tests happy+edge; tooling parity
confirmed-working; zero perf regression with zero-cost-when-off instrumentation; docs updated;
production-grade & zero lingering bugs — including the rule that ANY bug found while working,
ours or pre-existing, is fixed in-branch with a failing-test-first regression guard).

Campaign-specific additions:

15. **Every new engine configuration is a differential mode AND a fuzz axis.** Sync-lane
    forced/disabled, elision on/off, decoded-stream on/off, NaN-box (during bring-up), pooled
    frames on/off — each joins `vm_differential.rs` (both feature configs) and the differential
    fuzzer the same PR that introduces it, with a coverage assertion proving the new path actually
    ran (the JIT spec's anti-false-green rule, applied campaign-wide). Kill switches mirror
    `--no-specialize` and are permanent, not bring-up scaffolding.
16. **Same-session A/B for every headline number.** Baseline and candidate measured in one
    session on one machine (the SRV MINOR-2 lesson); results recorded in `bench/<spec>.md`;
    the shipped profiler is the instrument wherever possible (dogfooding is part of the gate).
17. **The Gate-12 floor never moves:** spec/tw bench geomean ≥2× holds at every merge, and the
    DBG zero-cost gate (instrument==None ≈ armed-idle) is re-run by any spec touching the
    dispatch loop or call path.
18. **Memory is measured, not assumed.** Every spec reports peak RSS on the corpus alongside
    time; an allocation-discipline spec (CALL, SHAPE, NANB, REGION) additionally reports
    allocation counts (e.g. via the existing bench harness + `/usr/bin/time -l` or an allocation
    counter), and a memory regression is a bug to fix, never a tradeoff to accept silently.
19. **Docs drift tripwires (`tests/docs_drift.rs`) stay green in CI.** Doc changes ship in the same
    PR as the surface they describe; allowlist additions are owner-justified. (DOCS campaign gate — tripwires
    cover CLI-surface⊆cli.md, env-var coverage, module→page mapping, NAV⇄files bijection, in-content links,
    and editor-pin consistency.)
20. **Tree-sitter / LSP / formatter parity is explicit per spec, never assumed.** Three tiers,
    each enforced by something that FAILS, not by convention: (a) a spec that touches grammar
    (this campaign: DEFER only) pays the FULL syntax checklist from `CLAUDE.md` — both parsers,
    tree-sitter `grammar.js` + regen `--abi 14` + `sync-grammar.sh` publish + zed/nvim editor-pin
    bumps + highlights, formatter arms + idempotence, LSP keyword/semantic-token/completion
    providers, REPL — verified by the treesitter/frontend conformance suites and LSP provider
    tests (Gate 11: "confirmed working", not "edited"). (b) A spec that adds stdlib surface
    (BATT, RESIL, CNTR, PAR, WARM…) inherits structural enforcement: registering in
    `STD_MODULES` feeds LSP import/auto-import completion automatically (the list is derived,
    not copied), SIG's export⇄table drift test fails on uncovered functions once SIG lands, and
    DOCS's module→page + CLI/env tripwires fail on undocumented surface — a new battery CANNOT
    ship tooling-invisible; whichever of the stdlib spec or SIG/DOCS lands second absorbs the
    delta as part of going green. (c) An engine-internal spec (LANE, CALL, SHAPE, DECODE, NANB,
    EXEC, REGION) asserts as part of its gates that it adds NO tooling surface (the LSP is
    static-only and never instantiates the runtime — cite it, then prove it by the suites
    staying green untouched).

## Done when

- LANE, CALL, SHAPE, DECODE, ELIDE, PAR, WARM, NANB are merged green under all gates; EXEC,
  REGION, and JIT are merged OR closed with a recorded evidence-based justification (their gates
  measured and found not met — that is a legitimate, documented outcome).
- The re-profile after DECODE shows: the async corpus within striking distance of the
  tree-walker×10 class (async tax no longer dominant), functional-idiom and object workloads
  dominated by useful work rather than allocation/hashing/dispatch bookkeeping, and a recorded
  decision on the JIT with numbers attached.
- `bench/` tells the whole story: every spec has a same-session A/B report; the profiling
  results doc has post-LANE, post-CALL, post-DECODE snapshots; peak-RSS tracked throughout.
- All cross-cutting subsystems updated per each spec's checklist (both engines, `.aso` + verify
  where touched, determinism seams, fuzzers, LSP/fmt/REPL where surface changed, docs + NAV,
  `CLAUDE.md`, `roadmap.md`) — and the four-mode differential + fuzz suites are green in CI on
  every merge.
- Production quality, fully tested. Nothing deferred unless evidence-gated, justified, recorded.

---

*Successor to `goal.md` (Serious Language Campaign, 12/13 merged — JIT carried forward here).
The correctness infrastructure that campaign built (differential oracle, four-mode identity,
cargo-fuzz CI, instrument seam, bcanalysis, archive manifests) is the substrate this campaign
spends.*
