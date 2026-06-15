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
