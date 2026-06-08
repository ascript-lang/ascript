# SRV — Multi-isolate Server Tier & Shared Read-only Heap — Implementation Plan

> REQUIRED SUB-SKILL: superpowers:subagent-driven-development (fresh implementer + independent reviewer
> per task; reviewer runs the commands and probes edges). Steps use `- [ ]`. Follows the NUM plan format
> (`superpowers/plans/2026-06-08-numeric-model.md`).

**Spec:** `superpowers/specs/2026-06-08-server-tier-shared-heap-design.md`. **Branch:** `feat/server-tier`
off `main`. **Depends on:** NUM merged (`Value::Int(i64)` + `Value::Float(f64)`, NUM renamed
`Value::Number→Float`), ADT merged (`EnumVariant` gains a payload), and the shipped worker subsystem
(`src/worker/` — isolates, the structured-clone airlock, `spawn_isolate`, the demand-grown pool).
**Not breaking** — `server.serve(opts)` gains a `workers` key (absent ⇒ today's single-isolate path,
unchanged); `std/shared` + `Value::Shared` are purely additive.

**Architecture:** TWO parts. **Part A — multi-isolate HTTP.** `server.serve({ port, workers: N, setup,
args })` runs the accept loop on N isolates that each bind the same port via `SO_REUSEPORT`
(kernel-balanced). The single-`&self` accept loop (`http_server.rs:832`, listener pulled from per-isolate
`self.resources` via `self.http_server_mut(id)`) is refactored into a free helper `accept_loop(&self,
listener: TcpListener, id, max_body, timeout_ms, max_concurrent, budget: Arc<AtomicUsize>, stop, span)`
that takes the listener **by value** and resolves the handler by a per-isolate `id`. `socket2` becomes a
**direct** dep (gated to `net`). Windows has no `SO_REUSEPORT` → single-isolate fallback + one-time warn.
Global `maxRequests` is a shared `Arc<AtomicUsize>` budget + a coordinated stop (`Notify`/`watch`); only the
total is asserted, never the per-isolate split. **Part B — `std/shared`.** `shared.freeze(v)` deep-converts
a value into an immutable `Arc`-backed `Value::Shared(Arc<SharedNode>)` — AScript's **first `Send`** value
(`Value` as a whole stays `!Send`; guarded by `assert_not_impl_any!(Value: Send)`). Reads dispatch through
`index_get`/`read_member` + a call-position `call_shared` hook (mirrors the `std/schema` hook); mutation →
the shipped `frozen_kind` `cannot mutate a frozen {kind}` panic; a frozen-instance user-method call → a
**distinct** diagnostic. `SharedNode` is rebased onto NUM (`Int`/`Float`) + ADT (payload `EnumVariant`). The
graph is an acyclic `Arc` DAG (freeze rejects cycles, reuses diamonds via two identity tables); GC traces it
as a no-op. Crosses the airlock as an `Arc` bump (path-a: closure capture into `make_loop`; path-b: a
`TAG_SHARED` wire tag + a `Vec<Arc<SharedNode>>` side-vector). No grammar change. No `.aso` bump.

**Tech stack:** Rust; `src/value.rs`, `src/gc.rs`, `src/interp.rs`, `src/stdlib/{shared,http_server,mod}.rs`,
`src/worker/{serialize,isolate,mod}.rs`, `src/check/std_arity.rs`; `socket2` + `static_assertions`;
`bench/`; docs.

---

## Shared API Contract (pinned to current code, post-NUM/ADT)
**Existing (verified):** `Value::Number(f64)` `value.rs:626` (→ `Int`/`Float` after NUM); `enum Value`
`value.rs:623`; `Value::Native(Rc<NativeObject>)` `value.rs:656`; `Value::EnumVariant(Rc<EnumVariant>)`
`value.rs:660` (gains a payload after ADT); `Cc<…>` container variants `value.rs:639-662`; `frozen_kind`
`value.rs:236`; `freeze_value` `value.rs:249`; `is_frozen_value` `value.rs:263`; `type_name` `value.rs:483`;
`is_truthy` `value.rs:687`; `PartialEq` identity arms `value.rs:708-723`; `impl Trace for Value` `gc.rs:176`
(native/scalar no-op `_ => {}` at `gc.rs:204`); `cc_addr` `gc.rs:135`. **interp:** `read_member`
`interp.rs:3567`; the `std/schema` call-site hook `interp.rs:3437-3461` (`is_schema_value`→`call_schema`);
`call_value` `interp.rs:3695`; `check_not_frozen` `interp.rs:4881` (reads `frozen_kind`); `index_get`
`interp.rs:5265`; the VM `SET_INDEX`/`SET_PROPERTY` frozen guard `interp.rs:5319`; `type_name` free fn
`interp.rs:5392`. **http_server:** `http_server_serve(&self, id, args, span)` `http_server.rs:832`; listener
from `self.http_server_mut(id)` `http_server.rs:871`; accept loop body `http_server.rs:900-940`; the
`maxConcurrent` `Arc<Semaphore>` `http_server.rs:891`; `served` counter + `maxRequests` break
`http_server.rs:899-904`; `self.rc()` + `handle_connection` per-task `http_server.rs:919-928`; request object
build `http_server.rs:1070-1077`; `value_to_response` `http_server.rs:508`. **worker airlock:** wire tags
`TAG_NIL..TAG_REF` `serialize.rs:80-94` (`TAG_REF = 13` is the last; NUM adds an `Int` tag — `TAG_SHARED` is
the next free tag AFTER whatever NUM left); `check_sendable` `serialize.rs:103`; `unsendable_kind`
`serialize.rs:109` (`Native`→Some, `Instance`/`Regex`→None); `check_inner` `serialize.rs:154`; `struct Writer`
`serialize.rs:269`; `encode(v) -> Result<Vec<u8>, SendError>` `serialize.rs:360`; `decode(bytes, interp)`
`serialize.rs:517`; `SendError` + path machinery `serialize.rs:34-78`. **isolate/mod:** `WorkerRequest`
`isolate.rs:37` (`args: Vec<u8>` `:50`); `spawn_isolate<F>(make_loop)` where `F: FnOnce(Rc<Vm>,
mpsc::UnboundedReceiver<Vec<u8>>) -> Fut + Send + 'static` `isolate.rs:140-142`; `dispatch_worker`
`mod.rs:83`; `run_slice_inline` `mod.rs:232`. **stdlib routing:** `std_module_exports` `mod.rs:109` +
`call`-routing `"schema" => self.call_schema(...)` `mod.rs:377`; module-name list `mod.rs:223`; `pub mod
schema` `mod.rs:72`. **arity:** `required_args` match `std_arity.rs:41`. **Cargo:** `[dependencies]`
`Cargo.toml:18`; `[dev-dependencies]` `:102`; `[features]` `:106`; `net = [...]` `:149`. **docs:** `NAV`
`docs/assets/app.js:11` (`['stdlib/async', …]` `:37`, `['language/workers', …]` `:26`); workers page
`docs/content/language/workers.md`.

**New names (do not rename):** `Value::Shared(Arc<SharedNode>)`; `pub enum SharedNode` + `type SharedValue =
Arc<SharedNode>` + `SharedMap`/`SharedSet`; `serialize::TAG_SHARED`; `Writer.shared: Vec<Arc<SharedNode>>`;
`WorkerRequest.shared: Vec<Arc<SharedNode>>`; `Interp::call_shared` / `call_shared_method` / `freeze_value_to_shared`
(freeze walker); `http_server::accept_loop`; the `shared` Cargo feature (folded into `default`).

## Conventions (every task)
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- `cargo test` AND `cargo test --no-default-features` green; clippy clean BOTH configs
  (`--all-targets` and `--no-default-features --all-targets`).
- Both engines byte-identical for the data half (`vm_differential.rs`, both feature configs) — fix the
  engine, never the assertion. No grammar/parser/formatter/tree-sitter change (Task 9 verifies this).
- No `await` across a `RefCell`/resource borrow; the frozen `Arc` graph holds no `Cc`/`Rc`/`Native`.
- No `.aso` / `ASO_FORMAT_VERSION` bump (the `TAG_SHARED` tag is worker-wire only; freeze is a runtime call).

---

## Task 1 — Core value layer: `SharedNode`, `Value::Shared`, exhaustive arms, Send-safety guards
**Files:** `src/value.rs` (+ every exhaustive `match Value`, compiler-flushed), `Cargo.toml`
(`static_assertions` dev-dep). **Tests:** `value.rs`. **Core, NO feature gate** (the variant + `SharedNode`
must build under `--no-default-features`, §6).
- [ ] Failing tests: `assert_send_sync::<SharedNode>()` compiles (hand-rolled `const _: fn() = || { fn
  is_send_sync<T: Send + Sync>(){} is_send_sync::<SharedNode>(); };`); the negative
  `static_assertions::assert_not_impl_any!(Value: Send)` compiles (proves `Value` stays `!Send` with the new
  `Send` member); `frozen_kind(&Value::Shared(obj_node))==Some("object")`, `Some("array")` for an array node
  (§3.8); `is_frozen_value(&Shared)==true`; `freeze_value(&Shared)` is a no-op; `type_name(Shared(Object))=="object"`;
  `Shared(_)` is truthy; two clones of one `Arc` `==` (Arc identity), distinct `Arc`s `!=`, a `Shared`
  never equals a non-frozen container.
- [ ] Define `pub enum SharedNode { Nil, Bool(bool), Int(i64), Float(f64), Decimal(Decimal), Str(Arc<str>),
  Bytes(Arc<[u8]>), Array(Arc<[SharedValue]>), Object(Arc<SharedMap>), Map(Arc<SharedMap>), Set(Arc<SharedSet>),
  EnumVariant { enum_name: Arc<str>, name: Arc<str>, value: SharedValue }, Regex { source: Arc<str> },
  Instance { class_name: Arc<str>, fields: Arc<SharedMap> } }` + `pub type SharedValue = Arc<SharedNode>` +
  `SharedMap` (ordered `Vec<(MapKey/Arc<str>, SharedValue)>` or `IndexMap`) + `SharedSet` (§3.3). Both numeric
  arms `Copy+Send`; the ADT payload `value: SharedValue` keeps the graph `Send+Sync`. Add
  `Value::Shared(Arc<SharedNode>)` AFTER the `Rc`/`Cc` variants.
- [ ] Add the `Shared` arm to every exhaustive `Value` match the compiler flushes: `PartialEq` (Arc
  identity, mirroring `value.rs:708-723`), `Debug`/`Display` (render like the underlying kind), `is_truthy`
  (truthy), `type_name` (underlying kind name), `frozen_kind` (returns underlying container kind),
  `is_frozen_value` (true), `freeze_value` (no-op). Add `static_assertions = "1"` to `[dev-dependencies]`
  (compile-time only, §6).
- [ ] Green both configs; clippy. Independent review: greps for any non-`Send` field smuggled into
  `SharedNode`; confirms `assert_not_impl_any!(Value: Send)` is load-bearing (temporarily add `unsafe impl
  Send for Value` → build must fail). Commit.

## Task 2 — GC: no-op trace arm for `Shared` (Arc, not Cc)
**Files:** `src/gc.rs`. **Tests:** `gc.rs`. **Core, NO feature gate.**
- [ ] Failing tests: a `Cc` object holding a `Value::Shared` field is GC-collected normally (the `Shared`
  is a traced-skipped leaf, §3.6); a frozen graph stored, dropped, and re-collected leaks nothing; the GC
  adds **zero** edges through a `Shared` (it holds no `Cc`/`Value` cells).
- [ ] Add `Value::Shared(_) => {}` to `impl Trace for Value` (`gc.rs:176`, alongside the native/scalar no-op
  at `gc.rs:204`) — the GC must NEVER trace into the `Arc` graph (a different ownership domain; acyclic by
  construction so refcounting reclaims it). One-liner; mirror the native-handle invariant comment.
- [ ] Green both configs; clippy. Review: confirms no `Cc`/`Rc` reachable from a `SharedNode` (so no
  cross-domain `Arc→Cc→Arc` cycle possible). Commit.

## Task 3 — `shared.freeze`: the walker, two identity tables, `std/shared` module
**Files:** `src/stdlib/shared.rs` (new), `src/stdlib/mod.rs` (both match arms + `pub mod` + module-name list),
`src/check/std_arity.rs`, `Cargo.toml` (new `shared` feature, folded into `default`). **Tests:** `shared.rs`,
`tests/check.rs`. **`shared.*` fns feature-gated; the freeze logic lives behind the feature.**
- [ ] Failing tests: `freeze` of every sendable kind (incl. NUM `Int`/`Float` and ADT payload `EnumVariant`)
  produces the right `SharedNode`; idempotence — `freeze(freeze(x))` returns the SAME `Arc`
  (`freeze(Shared)==identity`); **diamond** — one container reachable by two paths freezes to ONE `Arc` (via
  `completed`); **cycle** — `a.push(a)` → recoverable Tier-2 panic `shared.freeze does not support cyclic
  values at <path>` (via `in_progress`); a dedicated test feeds BOTH a diamond and a cycle in one value and
  asserts the diamond reuses one `Arc` while the cycle panics; non-sendable input (function/native/future/
  generator) → recoverable panic naming the field path (`value of kind function cannot be frozen at
  routes.handler`); `shared.isShared(v)` true only for a `Shared`.
- [ ] Create `src/stdlib/shared.rs` with `exports()` (`freeze`, `isShared`) + `call(func, args, span)` for
  `"shared.freeze"`/`"shared.isShared"`. The freeze walker reuses `check_sendable`'s path-building
  (`serialize.rs:103`, the `SendError` path machinery) and uses the TWO identity tables (§3.3), both keyed by
  `gc::cc_addr` (`Cc` containers) / `Rc::as_ptr` (`Bytes`): `in_progress: HashSet<usize>` (insert BEFORE
  recursing, remove AFTER the child `Arc` is built → an in-`in_progress` hit is a cycle → reject) checked
  FIRST, then `completed: HashMap<usize, Arc<SharedNode>>` (a finished node → reuse the `Arc`). Map keys
  canonicalize per NUM's post-split `MapKey` rule. Register in BOTH `mod.rs` match arms (`std_module_exports`
  `:109` + the `call` router `:377`), add `"shared"` to the module-name list (`:223`), declare `pub mod
  shared` gated on the new `shared` feature. Add `("std/shared", "freeze") => 1` and `("std/shared",
  "isShared") => 1` to `required_args` (`std_arity.rs:41`).
- [ ] Add a new `shared = [...]` feature folded into `default` (mirror how `log`/`net` are listed,
  `Cargo.toml:106-149`). Green both configs; clippy. Review: probes a deeply-nested diamond + a self-cycle
  through a `Map` value; confirms the `completed`-hit is NOT misreported as a cycle. Commit.

## Task 4 — Read dispatch: `index_get` / `read_member` / `call_shared` (+ VM deopt)
**Files:** `src/interp.rs` (+ VM read fast paths). **Tests:** `interp.rs`, `vm_differential.rs`. **Core
read-only dispatch is NO feature gate** (the bare language can read a `Shared`, §6).
- [ ] Failing tests: `index_get` on `Shared(Array)` by int → child as `Shared`/scalar, OOB → `nil`;
  `Shared(Object|Map)` by key → child, missing → `nil`; `read_member` on `Shared(Object|Instance)` → field
  as `Shared`/scalar, missing → `nil`; descending stays zero-copy (a sub-object reads as a `Shared` view, not
  a deep copy); read-only methods `.has(k)`/`.keys()`/`.values()`/`.len()`/`length`/`.contains(x)`/`.get(k,
  default)` + `for ... of` iteration yield `Shared` children/scalars; a `Shared(Regex)` exposes `.source` and
  `.test`/`.match` recompile per-isolate from source.
- [ ] Add `Value::Shared` arms to `index_get` (`interp.rs:5265`) and `read_member` (`interp.rs:3567`)
  returning child-as-`Shared` or a materialized scalar (`Nil`/`Bool`/`Int`/`Float`/`Decimal`/`Str` from an
  `Arc<str>` clone). Add `call_shared`/`call_shared_method` routed from the `Call` evaluator when the receiver
  is `Value::Shared` — mirror the `std/schema` call-site hook exactly (`interp.rs:3437-3461`: when the callee
  is `Member{object,name}` and `is <shared receiver>`, route to `call_shared`; else fall through). The
  read-only method set lives here; mutating-method names + frozen-instance user-methods are deferred to Task 5.
  Equality is Arc identity (Task 1). The VM `index_get`/member-read inline-cache fast paths get a `Shared`
  receiver **deopt to the generic `Shared` reader** (no shape id / no `ObjectCell`), so specialized == generic
  (Gate 1).
- [ ] Green both configs; clippy. Three-way differential (`tree-walker == specialized == generic`) over a
  shared-read corpus program. Review: confirms iteration over a frozen array is zero-copy and the VM deopt is
  correct. Commit.

## Task 5 — Mutation panic (reuse `frozen_kind`) + the distinct frozen-instance-method diagnostic
**Files:** `src/interp.rs`. **Tests:** `interp.rs`, `vm_differential.rs`.
- [ ] Failing tests: index-assign `shared[k]=v`, member-assign `shared.f=v`, and every mutating method
  (`push`/`set`/`insert`/`delete`/`clear`/`sort`/...) on a `Shared` → the SHIPPED `cannot mutate a frozen
  {kind}` panic (`{kind}` = the underlying container kind), catchable by `recover`, byte-identical on both
  engines; a frozen-INSTANCE user-method call `frozen.someMethod()` → the DISTINCT recoverable panic `method
  '<name>' is not available on a frozen instance (methods are not shared across isolates; freeze exposes
  fields only)` — asserted to be a DIFFERENT message than the mutation panic; the read-only structural method
  set STILL works on a frozen instance's fields.
- [ ] Because Task 1 added the `Shared` arm to `frozen_kind`, the assignment write arms (`check_not_frozen`
  at the `ExprKind::Index`/`Member` write sites) and the VM `SET_INDEX`/`SET_PROPERTY` guard
  (`interp.rs:5319`, both read `frozen_kind` via `check_not_frozen` `interp.rs:4881`) reject a `Shared` write
  **with NO new code** — verify this and add a test, do NOT invent a divergent string. In
  `call_shared_method`, a mutating method name raises the same `frozen_kind` mutation message; a
  frozen-instance user-method call (no method on the decoded shape, NOT a write) emits the distinct
  diagnostic above (§3.8).
- [ ] Green both configs; clippy. Three-way differential over a freeze+mutation-panic corpus program (the
  panic is order-deterministic). Review: confirms zero bespoke "(shared)" wording and that the
  instance-method message is genuinely distinct. Commit.

## Task 6 — Airlock path (b): `TAG_SHARED` + `Writer.shared` side-vector + `WorkerRequest.shared`
**Files:** `src/worker/serialize.rs`, `src/worker/isolate.rs`, `src/worker/mod.rs`. **Tests:** `serialize.rs`,
`tests/modules.rs`. **NO `.aso` / `ASO_FORMAT_VERSION` bump** (worker-wire tag only, §3.7).
- [ ] Failing tests: `unsendable_kind(&Shared)` → `None` (sendable); `check_inner` treats a `Shared` as a
  sendable LEAF (no recursion into the graph); `encode∘decode` of a `Shared` round-trips by `Arc` bump (no
  deep copy) — on the same-thread inline path the OUTGOING `Arc` pointer is PRESERVED (pointer-equality
  asserted); a `Shared` nested inside a normal sendable object crosses correctly; cross-thread the structural
  equality holds.
- [ ] `unsendable_kind` (`serialize.rs:109`): `Value::Shared(_) => None`. `check_inner` (`serialize.rs:154`):
  `Shared` is a sendable leaf, no recurse. Add `TAG_SHARED` = the next free tag after NUM's last (today
  `TAG_REF = 13` `serialize.rs:94`; NUM inserts an `Int` tag, so read the post-NUM max — do NOT hardcode 14).
  `encode` gains `Writer.shared: Vec<Arc<SharedNode>>` and emits `TAG_SHARED` + a u32 index =
  `shared.push(arc.clone())` (an atomic bump, no graph walk); change `encode` to return `(Vec<u8>,
  Vec<Arc<SharedNode>>)` and thread the second member through callers. `decode` on `TAG_SHARED` reads the
  index and reconstructs `Value::Shared(shared_table[i].clone())`. `WorkerRequest` (`isolate.rs:37`) gains
  `shared: Vec<Arc<SharedNode>>` (it is `Send`); `dispatch_worker` (`mod.rs:83`) threads it;
  `run_slice_inline` (`mod.rs:232`) passes it straight through.
- [ ] Green both configs; clippy. Review: confirms the side-vector index is bounds-checked on decode and the
  inline path preserves `Arc` identity. Commit.

## Task 7 — Part A refactor: `accept_loop(listener, id, …)` extraction (single-isolate parity)
**Files:** `src/stdlib/http_server.rs`. **Tests:** `tests/modules.rs` (single-isolate serve unchanged).
- [ ] Failing tests: today's single-isolate `serve` behavior is byte-for-byte unchanged after the refactor
  (existing http_server integration tests stay green); `maxRequests`/`maxConcurrent`/`maxBodySize`/
  `requestTimeout`/drain-on-stop all preserved.
- [ ] Factor the loop body (`http_server.rs:900-940`: the `served`/`maxRequests` break, the
  `sem.acquire_owned` permit, the per-connection `spawn_local` of `self.rc().handle_connection(id, …)`, the
  inflight-drain) into a free async helper `accept_loop(&self, listener: TcpListener, id: u64, max_body,
  timeout_ms, max_concurrent, budget: Arc<AtomicUsize>, stop, span)` that takes the listener BY VALUE (the
  `&self` stays only for `self.rc()`/`handle_connection`, already per-isolate) and resolves the handler by
  the per-isolate `id`. Single-isolate `serve` (`http_server_serve` `:832`) builds the listener from its
  resource as today (via `self.http_server_mut(id)` `:871`) and calls `accept_loop`. Introduce the
  `budget: Arc<AtomicUsize>` (single-isolate seeds it from `maxRequests` or `usize::MAX`) + a `stop` signal
  param now so the multi-isolate path (Task 8) reuses the same body; the single-isolate `fetch_sub`-and-check
  on `budget` must reproduce today's `served`/`maxRequests` semantics exactly.
- [ ] Green both configs; clippy. Review: diffs the refactor against the original loop to confirm zero
  behavior change on the single-isolate path. Commit.

## Task 8 — Part A multi-isolate: `socket2` REUSEPORT, N isolates, shared `maxRequests` + stop, Windows fallback
**Files:** `Cargo.toml` (direct `socket2` dep), `src/stdlib/http_server.rs`, `src/worker/isolate.rs`/`mod.rs`
(closure-capture boot, path a). **Tests:** `tests/modules.rs` (integration, spawns the built binary), a
`#[cfg(windows)]` test. **NO `.aso` bump.**
- [ ] Failing tests: `server.serve({ port, workers: N, setup, args })` on a REUSEPORT platform binds N
  isolates, fires M concurrent requests, all M get correct responses, and ≥2 distinct isolate ids appear
  (each isolate tags responses with an id set in `setup` → real parallelism, not the fallback); with
  `maxRequests: K` across N isolates, EXACTLY K connections are served IN TOTAL and all isolates then halt —
  the per-isolate split is NEVER asserted (OS scheduling, §4.1/§5); a `#[cfg(windows)]` test asserts the
  single-isolate fallback + the one-time warn; a `setup` opening a per-isolate (mock) resource proves the
  resource never crosses the airlock.
- [ ] Add `socket2 = { version = "0.5", optional = true }` to `[dependencies]` (`Cargo.toml:18`) and fold
  `"dep:socket2"` into `net = [...]` (`:149`). In `serve`, parse `workers`/`setup`/`args`/`port`. Platform
  gate via `cfg!(any(target_os="linux", target_os="macos", target_os="freebsd", ...))` + a runtime probe (a
  failed `set_reuse_port` also degrades). **Multi-isolate path** (`workers > 1`, or `0` = `num_cpus`, on a
  REUSEPORT platform): spawn N dedicated isolates via `spawn_isolate` (`isolate.rs:140`), **capturing** the
  `setup` code slice + the sendable `args` directly in the `Send` `make_loop` closure (§3.7a, R1) — the
  frozen `Value::Shared` args decomposed to their raw `Arc<SharedNode>` (a `Send` value) and moved into the
  closure; the non-`Shared` args `encode`d to `Vec<u8>` and captured; the accept-loop isolate's inbound
  `Vec<u8>` channel is unused and NO `WorkerRequest` is built (path a does NOT touch the Task-6 side-vector).
  Inside each isolate `make_loop` re-wraps the `Arc`s as fresh `Value::Shared`, `decode`s the bytes, runs
  `setup(...args)` → a per-isolate handle `id`, builds the listener via `socket2::Socket` +
  `set_reuse_port(true)` (the call `#[cfg(unix)]`-gated) + bind + listen + `TcpListener::from_std`, and runs
  `accept_loop(listener, id, …)` with the SHARED `Arc<AtomicUsize>` budget + a shared stop
  (`tokio::sync::Notify`/`watch`, cloned into every isolate). Each accepted connection `fetch_sub`-and-checks
  the budget; hitting 0 fires the stop so the others halt. `serve` awaits all N. **Windows / non-REUSEPORT:**
  single-isolate fallback + a one-time `warn`-level `std/log` diagnostic (§2.2).
- [ ] Green both configs; clippy (both, incl. that the `set_reuse_port` call never compiles on the non-unix
  build). Review: probes the budget race (assert ONLY the total, never the split) and confirms a per-isolate
  `Native` resource never crosses. Commit.

## Task 9 — Negative-space verification: no grammar / parser / formatter / tree-sitter / `.aso` change
**Files:** (verification only — no edits expected). **Tests:** `tests/treesitter_conformance.rs`,
`tests/frontend_conformance.rs`, fmt idempotence, `aso.rs`.
- [ ] Confirm SRV added NO syntax: `serve`/`freeze` are ordinary calls, `worker`/`fn` already exist. No
  `grammar.js` change, no `parser.c` regen, no `sync-grammar.sh`, no editor-pin bump (`editors/zed`,
  `editors/nvim`). Run `git diff --stat` over `tree-sitter-ascript/`, `src/parser.rs`, `src/syntax/`,
  `src/fmt.rs`, `src/lexer.rs`, `src/token.rs`, `src/ast.rs` and assert EMPTY. Confirm `ASO_FORMAT_VERSION`
  (`aso.rs`) is UNCHANGED (the `TAG_SHARED` tag is worker-wire only) and `verify.rs` is untouched. Run the
  conformance + fmt-idempotence suites green.
- [ ] This is the Gate-9 "Touching syntax is genuinely N/A" checkpoint. Review independently re-runs the
  diff-stat assertions. Commit (if any doc/comment touch-up needed; otherwise note as a no-op verification).

## Task 10 — Checker / LSP (conservative; gradual gate)
**Files:** `src/check/infer/*` (SP10), `src/lsp/*`. **Tests:** `tests/check.rs`, `tests/lsp.rs`.
- [ ] Failing tests: `shared.freeze(x)` synthesizes a frozen/opaque type that reads like its argument (or
  `Unknown`) — NEVER a false `type-*`; reading a `Shared` field synthesizes the field type where known else
  `Unknown`; hover on `shared.freeze` shows it returns a frozen shareable value; completion offers `shared`
  as an importable module and `freeze`/`isShared` as its functions; no semantic-token change (no keyword).
- [ ] Wire the conservative inference + LSP hover/completion. **Gate 5:** `examples/**` emits ZERO `type-*`
  in BOTH feature configs (CI tripwire) — default to `Unknown` for any uncertain frozen read.
- [ ] Green both configs; clippy. Review confirms zero `type-*` on the corpus. Commit.

## Task 11 — Example corpus + four-mode differential
**Files:** `examples/shared_config.as`, `examples/advanced/shared_routing_table.as`,
`examples/advanced/server_multicore.as`. **Tests:** conformance + `vm_differential.rs` (both configs) + fmt
idempotence.
- [ ] `examples/shared_config.as` — `freeze` a config object, read it (scalar/descend/index/method/iterate),
  show the mutation panic caught by `recover`. `examples/advanced/shared_routing_table.as` — a big frozen
  lookup table read by a worker fan-out (the zero-copy-share pattern), fully error-handled.
  `examples/advanced/server_multicore.as` — `server.serve({ port, workers: 0, setup })` with a per-isolate
  setup, a frozen shared lookup table, and a per-isolate (mock or real) connection, production-shaped.
- [ ] Every `shared.*` example runs byte-identically on tree-walker, specialized VM, generic VM, and
  `.aso`-compiled (§7.2). The server example asserts order-deterministic response sequences across modes
  (§5), wired into the differential harness like the Spec A worker examples. **Gate 9 caveat (documented):**
  these examples demonstrate the REUSEPORT path but CANNOT exercise the Windows single-isolate fallback (an
  `.as` example runs identically on every platform — the Windows branch is covered ONLY by the
  `#[cfg(windows)]` Rust test in Task 8). Green both configs; review; commit.

## Task 12 — Benchmark (Gate 12 + the headline Part B number)
**Files:** `bench/` (new harness reusing `src/stdlib/bench.rs`), a markdown report sibling to
`bench/PROFILING_RESULTS.md`. **Tests:** the bench compiles/runs.
- [ ] **Gate 12 — the NORMAL index/member path is unchanged.** Run the existing index/member microbench (or
  an IC-heavy differential program) BEFORE vs AFTER the `Shared` arm in BOTH `--specialize` (default) and
  `--no-specialize` (generic) modes; assert no measurable steady-state delta for a non-`Shared` receiver (the
  added arm is a single predictably-not-taken tag check after the existing fast paths).
- [ ] Report: **req/s across worker counts** (CPU-bound handler at `workers` = 1/2/4/8 + the single-isolate
  baseline) with speedup + parallel efficiency (expectation, not a hard CI gate since core counts vary: ≳3×
  on 4 cores). **Shared-heap vs deep-clone per-request cost** (table 10k→1M entries: the `Arc`-bump path flat
  O(1) vs the deep-clone path linear — quantifying the ~1.3 ms/10k-floats clone the shared heap eliminates).
  **Freeze cost** vs value size (the one-time amortized cost).
- [ ] Review confirms the Gate-12 before/after shows no regression in either mode. Commit.

## Task 13 — Docs (+NAV)
**Files:** `docs/content/stdlib/shared.md` (new) + `docs/assets/app.js` (`NAV`),
`docs/content/language/workers.md`, `docs/content/stdlib/async.md`, `README.md`, `CLAUDE.md`,
`superpowers/roadmap.md`, the design spec status.
- [ ] New `docs/content/stdlib/shared.md` (freeze/isShared, read-only/Send semantics, the mutation panic,
  idempotence/diamond, the cross-isolate `Arc`-bump story) AND add its slug to the `NAV` array
  (`docs/assets/app.js:11` — sidebar + cmd-K derive from `NAV`; no entry ⇒ unreachable, the documented orphan
  gotcha). Add a **"Multi-core servers & the shared heap"** section to `docs/content/language/workers.md`
  (REUSEPORT model, per-isolate `setup`, the Windows caveat); cross-link from `docs/content/stdlib/async.md`
  (`NAV` `['stdlib/async', …]` `:37`) and the http/server page. Update `README.md` (concurrency/stdlib table:
  multi-core HTTP + `std/shared`), `CLAUDE.md` (an SRV "Larger subsystems" entry: the first `Send` value, the
  REUSEPORT server tier, `Value::Shared`/`SharedNode`, the two-table freeze, path-a/path-b airlock), and
  `roadmap.md`.
- [ ] Serve the docs locally (`cd docs && python3 -m http.server`), confirm the new page is reachable from
  the sidebar and cmd-K. Review; commit.

## Done when
Every task checked behind an independent review; the data half (`shared.freeze`/reads/mutation-panic) is
four-mode byte-identical in both feature configs (`vm_differential.rs`); the multi-isolate server asserts
order-deterministic response sequences across modes and EXACTLY-`maxRequests`-total (never the per-isolate
split); `cargo build --no-default-features` compiles (variant + `SharedNode` + read-only dispatch core, only
`shared.*` fns gated); both `assert_send_sync::<SharedNode>` and `assert_not_impl_any!(Value: Send)` hold;
Gate 5 zero `type-*` on `examples/**`; Gate 9 confirms NO grammar/parser/formatter/tree-sitter/`.aso` change
(no `ASO_FORMAT_VERSION` bump); the Gate-12 normal-path bench shows no regression in either specialize mode;
clippy + tests green both configs; docs + NAV updated. Merge `--no-ff` to `main`.
