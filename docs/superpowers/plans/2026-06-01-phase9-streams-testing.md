# Phase 9 — Streams & Test-Runner Depth Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** `std/stream` (lazy combinators), `std/assert` (rich assertions + snapshot), `std/bench` (timing). Full spec: `docs/superpowers/specs/2026-06-01-phase9-streams-testing-design.md`.

**Conventions:** native modules registered in BOTH mod.rs arms; stream is a native lazy pull resource (async stages, no RefCell borrow across await — take state out / clone fns); assertions Tier-2 panic on failure with value-showing messages; reuse `object::deep_equal` (pub(crate)) for assert.eq; clippy clean both configs; RUN both test configs; docs+README+example. No grammar change.

Sub-phases: 9a stream → 9b assert → 9c snapshot+bench → 9d integration.

---

## Sub-phase 9a: `std/stream` (lazy pull engine)

**Files:** `src/stdlib/stream.rs` (new), `src/stdlib/mod.rs` (register `"std/stream"` + `"stream"` dispatch, core/no gate), `src/interp.rs` (`ResourceState::Stream`), tests.

- [ ] **Step 1 — failing tests** (lex→parse→exec; stream ops are async → await):
  - `stream.collect(stream.map(stream.from([1,2,3]), (x) => x*2))` → `[2,4,6]`.
  - LAZINESS: a counter incremented in a map fn over `stream.range(0, 1000000)` then `take(_,3)` then collect → counter == 3 (map called only 3×). (Use a 1-elem array counter.)
  - filter; drop; flatMap (`stream.from([1,2])` flatMap `(x)=>[x,x]` → `[1,1,2,2]`); enumerate (`[10,20]` → `[[0,10],[1,20]]`); zip; reduce (sum→6); count; find; first.
  - generator source: `async fn* g(){ yield 1; yield 2 }` then `stream.collect(stream.from(g()))` → `[1,2]`.
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:**
  - `ResourceState::Stream` holding `{ source: StreamSource, stages: Vec<Stage> }` where StreamSource = Array(Rc<RefCell<Vec<Value>>>, cursor) | Range{cur,end,step} | Generator(Value) ; Stage = Map(Value)|Filter(Value)|Take(usize,taken)|Drop(usize)|FlatMap(Value, buffer)|Enumerate(idx)|Zip(other-stream).
  - `stream.from`/`range`/`map`/`filter`/`take`/`drop`/`flatMap`/`enumerate`/`zip` construct/append (lazy — return a new stream handle with the stage list; `map` etc. clone the prior stages + push). Terminals `collect`/`forEach`/`reduce`/`count`/`find`/`first` drive a `pull_next` loop.
  - `pull_next(&self, stream_id) -> Result<Option<Value>, Control>` (async): take the stream state OUT (or borrow-free), pull one from source (array cursor++/range step/`generator.resume`), thread through stages (filter→loop, map→transform via call_value, take→stop, drop→skip, flatMap→buffer, enumerate→wrap, zip→pull other), return Some(value)/None at end; return state. NO RefCell borrow across the `.await` (clone the stage fns + take the source out). Generator source: drive via the consumer-driven resume (study coro.rs `GeneratorHandle::resume` / how `gen.next` / `for await` resumes — call it from the pull loop).
  - Register `pub mod stream` + both mod.rs arms (core). Single-consumption: a consumed stream's source is exhausted; document (re-collect → empty).
- [ ] **Step 4 — verify:** both `cargo test` configs + both clippy.
- [ ] **Step 5 — commit:** `feat(stream): std/stream lazy combinators (map/filter/take/flatMap/.../collect)`

---

## Sub-phase 9b: `std/assert` (rich assertions)

**Files:** `src/stdlib/assert_mod.rs` (new; name to avoid clashing with the `assert` builtin — module routes as `"assert"`), `src/stdlib/mod.rs` (register), tests.

- [ ] **Step 1 — failing tests:** each assertion passes on true case, Tier-2 panics on false (observe via recover); `assert.eq([1,2],[1,2])` passes (DEEP — arrays compare by value not identity), `assert.eq([1],[2])` panics; `assert.throws((){ panic("x") })` returns the error (assert its message contains "x"); `assert.throws((){ 1 })` (no panic) → panics itself; `assert.contains("hello","ell")`, `assert.contains([1,2,3],2)`, `assert.contains({a:1},"a")`; `assert.approxEq(0.1+0.2, 0.3)`; gt/lt/nil/notNil.
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:** module `assert_mod` routed as `"assert"`. NOTE: there's a GLOBAL `assert(cond)` builtin — `assert.eq` is a MODULE function (`import * as assert from "std/assert"; assert.eq(...)`), distinct from the global. Dispatch: assert.eq/ne use `crate::stdlib::object::deep_equal` (pub(crate)); each failure → `Err(Control::Panic(AsError::at(msg_with_values, span)))`. `assert.throws` calls the fn via `self.call_value` and inspects for a panic (like `recover` does) → returns the caught error Value, or panics if the fn did NOT throw. (assert.throws needs the interpreter → `impl Interp` async dispatch; the pure comparisons can be sync OR also routed through the async dispatch for uniformity.) Register both mod.rs arms (core).
- [ ] **Step 4 — verify:** both configs + clippy.
- [ ] **Step 5 — commit:** `feat(assert): std/assert (eq/ne/true/false/nil/cmp/contains/approxEq/throws, deep eq)`

---

## Sub-phase 9c: `assert.snapshot` + `std/bench`

**Files:** `src/stdlib/assert_mod.rs` (snapshot arm, sys-gated), `src/stdlib/bench.rs` (new), `src/stdlib/mod.rs`, tests.

- [ ] **Step 1 — failing tests:**
  - snapshot (sys-gated test): in a temp cwd / with a unique name, `assert.snapshot("t1", value)` first call writes `__snapshots__/t1.snap` + passes; second call same value passes; different value → Tier-2 panic; with `ASCRIPT_UPDATE_SNAPSHOTS=1` overwrites. (Manage cwd/cleanup carefully; env-race-safe — prefer a unique snapshot dir per test or an internal helper.)
  - bench: `bench.measure(fn, 50)` returns `{iterations:50, totalMs>=0, avgMs>=0, opsPerSec>0}`; an async fn is awaited; `bench.measure((){ })` works.
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:**
  - `assert.snapshot(name, value)` (`#[cfg(feature="sys")]` arm in assert_mod): serialize value via `json` pretty; path `__snapshots__/<name>.snap` (sanitize name); if absent → write + pass; else read + compare → mismatch panics with both values; `std::env::var("ASCRIPT_UPDATE_SNAPSHOTS")` set → overwrite. Use fs.
  - `std/bench` (`src/stdlib/bench.rs`, core): `bench.measure(fn, iterations?)` async — default iterations (e.g. 100); loop calling `self.call_value(fn, [])` (await if it returns a future — drive like task.retry does), time via `time.monotonic` (or std Instant — match how std/time monotonic works), return `{iterations, totalMs, avgMs, opsPerSec}` Object. Optional `bench.compare`. Register both mod.rs arms (core).
- [ ] **Step 4 — verify:** `cargo test` (sys on) + `cargo test --no-default-features` (snapshot excluded under no-sys; bench core present) + both clippy.
- [ ] **Step 5 — commit:** `feat(assert,bench): assert.snapshot (sys) + std/bench timing`

---

## Sub-phase 9d: integration

- [ ] `examples/streams_and_testing.as`: a lazy stream pipeline (`stream.range` → filter → map → take → collect, with a comment on laziness), a `test("...", () => { ... assert.eq(...); assert.throws(...) })` block, and a `bench.measure` printing stats. Bounded, terminates, prints success. Run it; conformance (treesitter+frontend) + fmt idempotence.
- [ ] Docs: `std/stream` page (sources/combinators/terminals/laziness/single-consumption), `std/assert` page (assertions + deep-eq note + snapshot + ASCRIPT_UPDATE_SNAPSHOTS), `std/bench` page; README stdlib table; the testing/CLI doc page notes `std/assert` usage with `ascript test`.
- [ ] FULL gates: both `cargo test` configs, both clippy `--all-targets`, `fmt --check`, the example, both conformance tests.
- [ ] Holistic review (focus: stream laziness + borrow-safety + single-consumption; generator-source pull correct; assert deep-eq + throws + messages; snapshot file behavior + sys-gating; bench timing; no regression to test()/global assert; no TODOs). Merge `--no-ff`.

## Self-review notes
- Riskiest: 9a stream pull engine (laziness, generator-source resume from native code, borrow-across-await, flatMap buffering, single-consumption) and 9c snapshot (file IO + env-race-safe tests + sys-gating).
- assert.eq DEEP (reuse object::deep_equal) — don't reimplement.
- No grammar change → conformance unchanged.
