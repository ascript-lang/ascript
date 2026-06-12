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

- 🔒 **LANE — Two-lane fiber engine + inline ready-future completion.** A synchronous dispatch
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

- 🔒 **CALL — Call-path allocation diet + higher-order callback trampoline.** Remove the ≥3
  allocations/call: empty-`cell_slots` fast path (no cells vector when nothing is captured
  by-reference), argument passing via the operand-stack window instead of `Vec` collection where
  the call shape allows, frame/fiber pooling. The trampoline: higher-order builtins
  (map/filter/reduce/sort-comparator/each) detect a non-async, non-generator callee and drive all
  elements through ONE reused fiber on the sync driver — no per-element `Vec`, no boxed future, no
  fresh fiber — with per-element escalation fallback to the async path. Depends on LANE (driver).
  - Spec: `superpowers/specs/2026-06-12-call-path-diet-design.md`
  - Plan: `superpowers/plans/2026-06-12-call-path-diet.md`

### Representation — where allocation & hashing go to die

- 🔒 **SHAPE — Shape-native object storage + interior hashing.** Shapes stop being an id beside
  an `IndexMap` and become the OWNER of the key→index layout; object/instance storage becomes a
  flat values slab. Object literals get compile-time-precomputed shape ids (zero hashing at
  construction); `resync_object_shape` loses its key-clone. Interior hash tables that never see
  user-controlled DoS surface (ShapeRegistry, IC maps, scope maps) move from SipHash to a fast
  hasher; `Map`/`Set` keep DoS-resistant hashing (documented decision). Megamorphic fallbacks
  preserve today's semantics exactly (insertion order, deletion, dynamic keys).
  - Spec: `superpowers/specs/2026-06-12-shape-storage-design.md`
  - Plan: `superpowers/plans/2026-06-12-shape-storage.md`

- 🔒 **NANB — 8-byte NaN-boxed `Value`.** The representation endgame VAL §3.2 sanctioned but
  parked: `Value` becomes a single 8-byte NaN-boxed word (inline `float`; tagged inline `int`
  within payload range with overflow escape; tagged `Cc`/`Rc` pointers for heap kinds; immediate
  nil/bool). Clears the JIT spec's ≤16-byte precondition. The prior 16-byte thin-`Str` attempt was
  REJECTED as a measured regression (`bench/COMPACT_VALUE_RESULTS.md`) — NANB must re-run that
  evaluation gate and ship only on a measured win (Gate-12 style A/B, same session). Depends on
  SHAPE (object internals stabilize first; avoids double-churn).
  - Spec: `superpowers/specs/2026-06-12-nan-boxing-design.md`
  - Plan: `superpowers/plans/2026-06-12-nan-boxing.md`

### Dispatch — decode once, fuse what the data says, inline what guards allow

- 🔒 **DECODE — Pre-decoded instruction stream + data-driven superinstructions + speculative
  inlining.** A per-`FnProto`, lazily-built side representation (fixed-width op records, operands
  widened, jump targets pre-resolved) following the `arith_cache` side-table precedent —
  `Chunk.code` stays byte-identical (disassembler/goldens/differential untouched); `Op::Break`
  byte-patching INVALIDATES the decoded cache (the same invalidation story a future JIT needs —
  built and tested here first). Superinstruction selection is empirical: fusion pairs chosen from
  shipped coverage/profiler data over the bench corpus, never guessed. Small hot global fns are
  speculatively inlined at bytecode level behind the EXISTING global-version guard (`struct_gen`),
  deopting to the generic call on guard failure. Depends on LANE (the sync driver is the primary
  consumer).
  - Spec: `superpowers/specs/2026-06-12-decoded-dispatch-design.md`
  - Plan: `superpowers/plans/2026-06-12-decoded-dispatch.md`

### Types that pay you back

- 🔒 **ELIDE — Contract elision via static proof.** When the TYPE checker statically PROVES a
  call site's arguments satisfy the callee's annotations (or a field assignment its schema), the
  compiler emits an unchecked call/store; checks remain at every unproven (gradual) boundary —
  sound gradual typing where annotations BUY performance (the loop TypeScript/Sorbet structurally
  cannot close; Static Python/Cinder precedent). Strictly compiler+checker work: the tree-walker
  keeps full checks, and elision must be OBSERVABLY invisible (a program that passes checks
  behaves identically; one that would fail them is, by proof, unreachable — the differential
  corpus + fuzzer enforce this). `--no-elide` kill switch mirrors `--no-specialize`.
  - Spec: `superpowers/specs/2026-06-12-contract-elision-design.md`
  - Plan: `superpowers/plans/2026-06-12-contract-elision.md`

### Multi-core — the ×cores lever (from shipped pieces)

- 🔒 **PAR — Data-parallel primitives over the frozen shared heap.** `task.pmap(arr, f)` /
  `task.preduce(arr, f, init)` (std-lib, no syntax): chunk a `shared.freeze`-frozen array across
  the worker pool (zero-copy reads via the `TAG_SHARED` airlock path), run the callback in
  isolates, merge results. Unfrozen inputs take a freeze-or-copy documented path; non-sendable
  callbacks are the existing field-path panic. Builds entirely on `src/worker/` + `std/shared` +
  the pool-side archive cache. Rayon-class throughput for batch work — a ×cores lever no
  baseline JIT can match.
  - Spec: `superpowers/specs/2026-06-12-data-parallel-design.md`
  - Plan: `superpowers/plans/2026-06-12-data-parallel.md`

### Deployment & I/O throughput

- 🔒 **WARM — Warm starts & durable-log throughput.** (a) A content-addressed compile cache for
  `ascript run` (key: source digest + `ASO_FORMAT_VERSION` + flags; store under `$ASCRIPT_CACHE`)
  — large projects re-run instantly. (b) A PGO feedback section in the module-archive manifest
  (BNDL): `build --pgo <training-run>` records warmed shapes/IC layouts/arith kinds; the loader
  pre-seeds the side tables — `--native --pgo` ships a warm-starting, sandboxed, tree-shaken
  single binary. (c) Workflow-log group commit: batched/coalesced fsync with an explicit
  durability mode knob (`workflow` stays default-durable; the 96%-fsync workload becomes a
  policy choice, never a silent relaxation).
  - Spec: `superpowers/specs/2026-06-12-warm-starts-design.md`
  - Plan: `superpowers/plans/2026-06-12-warm-starts.md`

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

- 🔒 **REGION — Task-scoped region allocation.** Per-spawned-task / per-request bump arenas with
  promote-on-escape (returns, captured-env writes, globals, channel/event sends, the airlock),
  bulk-freed at task end — sound because isolation already draws the region boundary. **Gate: a
  spike on `json_roundtrip` + the server workload proving ≥20% allocation-time win without
  promotion-cost blowback.** v1 may narrow to compiler-PROVEN non-escaping allocations (Go-style
  escape analysis on `bcanalysis` facts) if the dynamic promotion spike fails its gate. Depends
  on NANB (value representation must be final first).
  - Spec: `superpowers/specs/2026-06-12-task-regions-design.md`
  - Plan: `superpowers/plans/2026-06-12-task-regions.md`

- 🔒 **JIT — Baseline Cranelift JIT (existing spec, still deferred).** The design stands at
  `superpowers/specs/2026-06-08-baseline-jit-design.md`. This campaign UPDATES its preconditions:
  (1) NUM ✅; (2) the ≤16-byte value precondition is satisfied by **NANB**; (3) profiling must be
  re-run AFTER LANE+CALL+SHAPE+DECODE — only if dispatch then dominates does the JIT proceed.
  New addendum requirements discovered since the spec was written: `Op::Break`/coverage
  byte-patching must invalidate compiled code (DECODE builds and proves the invalidation
  machinery); the sync lane defines the compilable subset and the lane-escalation seam is the
  natural native↔interpreter boundary; the cargo-fuzz infrastructure (shipped) takes the "JIT
  joins the fuzzer" cost to near-zero. Remains the LAST lever, by evidence.

### Developer-experience track (owner-sequenced relative to the engine waves)

- 🔒 **DOCS — documentation reconciliation + permanent drift tripwires.** The 2026-06-12
  docs-vs-reality audit (re-verified during spec drafting) found `docs/content/cli.md` missing
  **27 CLI flags, the `ascript dap` subcommand, and all 7 `pkg` subcommands** (e.g. `build
  --native` is documented only on `language/bundles.md`, never on the CLI reference page), all
  7 user-facing `ASCRIPT_*` env vars undocumented there (`ASCRIPT_NO_SPECIALIZE` documented
  nowhere), one stdlib member gap (`task.pipe` absent from `stdlib/async.md`), and a
  CLAUDE.md meta-drift ("stdlib pages mirror the source modules" — they are domain-grouped).
  Unit A is the one-time reconciliation sweep; Unit B is the durable value: six in-tree drift
  TRIPWIRES (clap-introspected CLI-surface ⊆ cli.md; env-var coverage; a validated
  module→page claiming table; NAV ⇄ files bijection; in-content link checker; editor-pin
  manual checklist) written failing-first against today's gaps, then kept green in CI —
  proposed as gate 19. Boundary with SIG: SIG owns per-function stdlib *signature*
  consistency; DOCS owns existence/claiming/CLI/env/NAV/links. Independent of all engine
  specs; mutually independent of SIG.
  - Spec: `superpowers/specs/2026-06-12-docs-reconciliation-design.md`
  - Plan: `superpowers/plans/2026-06-12-docs-reconciliation.md`


- 🔒 **SIG — stdlib signature table + LSP signature/completion/hover enrichment + audit
  hardening.** The 2026-06-12 LSP audit established that signature help resolves ONLY a unique
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

- 🔒 **RT — runtime-only native stubs, v2 upfront (tier matrix + import-driven pruning).**
  `build --native` today appends the archive to a copy of the FULL 42 MB toolchain binary
  (LSP, DAP, fmt, REPL, pkg, checker, three parsers). RT ships a runtime-only `ascript-rt`
  bin target and, from day one, a prebuilt per-target **stub tier matrix** (curated feature
  supersets, fetched fail-closed with pinned checksums into the `$ASCRIPT_CACHE` store) with
  **nearest-superset selection** driven by the tree-shaker's import graph via a drift-tested
  module→Cargo-feature table, plus `--exact` local-cargo builds. Includes: `--target` cross
  builds (platform-independent payload onto platform stubs), **`--oci`** (emit a loadable OCI
  image tarball without Docker), `--compress` (zstd payload), reproducible outputs, and the
  tier-selection build report. Foundation for CNTR's images and WASM-adjacent distribution.
  - Spec: `superpowers/specs/2026-06-12-native-runtime-stubs-design.md` · Plan: `superpowers/plans/2026-06-12-native-runtime-stubs.md`

- 🔒 **CNTR — container-native runtime + `std/docker`.** Unix-domain sockets in `std/net` +
  `std/http` (`{socketPath}`) as the missing foundation; `std/docker` as a typed wrapper over
  the Engine API (containers/images/exec, `logs`/`events` as `for await` streams) gated on
  **net AND process** caps (dual-cap chokepoint extension — the docker socket is
  host-root-equivalent); inbound signal handling (`process.on("SIGTERM", …)`),
  `server.serve({onShutdown, drainTimeout})` graceful drain, cgroup-aware worker sizing
  (`cpu.max`), `os.inContainer()`, official base images built from RT stubs, and
  `ascript init --template server` scaffolding (Dockerfile + healthcheck + shutdown +
  resilience wired). Depends on RT (images) and RESIL (template policies).
  - Spec: `superpowers/specs/2026-06-12-containers-docker-design.md` · Plan: `superpowers/plans/2026-06-12-containers-docker.md`

- 🔒 **RESIL — `std/resilience` for backend hosting.** Composable per-isolate policies:
  circuit breaker, keyed token-bucket rate limiter, bulkhead + load shedding, retry v2
  (backoff + jitter + budgets), fallback, policy composition; **singleflight** +
  stampede-protected memoization (composing `std/lru`); **deadline propagation** via the
  spec's ONE runtime seam — task-local storage (zero-cost when unused; also unlocks
  request-id/trace propagation); Prometheus text `/metrics` + telemetry counters;
  health/readiness helpers. Per-isolate state is documented honestly (actor pattern for
  global state). Parked with sketches: hedged requests, AIMD adaptive concurrency, `std/k8s`.
  - Spec: `superpowers/specs/2026-06-12-resilience-stdlib-design.md` · Plan: `superpowers/plans/2026-06-12-resilience-stdlib.md`

- 🔒 **EMBED — embedding API (Rust crate + C API).** A stable, versioned host API: create
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

- 🔒 **REPLAY — record/replay as a user-facing flagship.** The plumbing is shipped and INERT
  (`src/det.rs` Record/Replay, virtual clock, seeded RNG, FFI replay, workflow replay); REPLAY
  makes it a headline feature: `ascript run --record/--replay`, `ascript test --record` (failed
  tests auto-save a trace; any failure replays deterministically), and replay-debugging through
  the shipped DAP server (time-travel via deterministic re-execution, the rr model). The core
  design question it must answer honestly: extending `DetEvent` recording to effectful stdlib
  I/O at the `call_stdlib` chokepoint (http/fs/process results) vs documenting the seamed
  subset (clock/RNG/FFI) as v1. Zero-cost-when-off inherited from det's INERT default.
  - Spec: `superpowers/specs/2026-06-12-record-replay-design.md` · Plan: `superpowers/plans/2026-06-12-record-replay.md`

- 🔒 **BATT — backend batteries (T1+T2).** One multi-unit stdlib spec, phased like the
  batteries campaign: **T1** — TLS for `std/server`/`std/tcp` (rustls); `std/jwt` + auth
  (JWKS, OAuth2/OIDC client, signed cookies/sessions); `std/archive` (tar+zip, streaming —
  also RT's `--oci` tar substrate); `std/xml` (+ HTML sanitizer); `std/email` (SMTP + message
  builder); `std/blob` (S3-compatible client: sigv4, presign, MinIO/R2); deterministic-testing
  batteries (frozen clock / seeded RNG in `ascript test` via the det seams + user-facing
  property testing `test.prop` with shrinking, surfacing the FUZZ generator philosophy).
  **T2** — `std/cron`, `std/semver`, `std/markdown`, `std/diff`. Each unit: feature flag, caps
  mapping, docs page + NAV (DOCS tripwires apply), intro + advanced examples, four-mode tests.
  - Spec: `superpowers/specs/2026-06-12-backend-batteries-design.md` · Plan: `superpowers/plans/2026-06-12-backend-batteries.md`

- 🔒 **LSPEC — language specification + stability policy.** A versioned normative spec
  (grammar derived from the tree-sitter grammar with a drift check; semantics chapters; the
  examples corpus formally adopted as the conformance suite), a stability-tier policy
  (stable/experimental surface), the pre-1.0 → 1.0 breaking-change criteria checklist, and an
  RFC-lite process doc. Documentation-and-governance work; no code surface.
  - Spec: `superpowers/specs/2026-06-12-language-spec-stability-design.md` · Plan: `superpowers/plans/2026-06-12-language-spec-stability.md`

### Language surface track (the campaign's ONE grammar change)

- 🔒 **DEFER — `defer` statement for scoped cleanup.** Go-shaped: function-scoped, LIFO,
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
19. **Docs drift tripwires stay green in CI** once DOCS lands (the gate the DOCS spec proposes;
    reserved here so the number is stable).
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
