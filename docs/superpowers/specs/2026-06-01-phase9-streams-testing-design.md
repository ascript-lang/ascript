# Phase 9 — Streams & Test-Runner Depth Design

- **Date:** 2026-06-01
- **Status:** Design — proceeding under the standing multi-phase goal (final phase).
- **Roadmap:** Phase 9 of `2026-05-31-batteries-completeness-roadmap.md`.
- **Owner:** Mahmoud Kayyali

## Goal

Two ergonomics tracks that round out the language:
1. **`std/stream`** — a composable, lazy stream/pipe abstraction (map/filter/take/… over arrays
   and generators), so users get reusable combinators instead of hand-writing `async fn*`
   pipelines (the `stream_pipeline.as` example shows the manual form).
2. **Test-runner depth** — `std/assert` (rich assertions), `assert.snapshot` (snapshot testing),
   and `std/bench` (micro-benchmark timing) — on top of the existing `test(name, fn)` runner.

Both are additive stdlib (no grammar change). Streams build on the M17 generator engine
(consumer-driven, `for await`); benchmarks use `time.monotonic`; snapshots use `fs`.

## Sub-phases & feature placement
- **9a — `std/stream`** (lazy combinators) — core (no feature gate; depends on the generator
  engine which is core).
- **9b — `std/assert`** (assertions) — core.
- **9c — `assert.snapshot` + `std/bench`** — snapshot needs `fs` → `sys`-gated; `std/bench`
  core (uses `time.monotonic`).
- **9d — integration** (example, docs, README, merge).

Conventions: native modules; lazy stream is a native resource (pull engine); assertions panic
Tier-2 on failure with clear messages; clippy clean both configs; RUN both test configs;
docs+README+example; no `RefCell` borrow across `.await` (stream stages + generator resume are
async).

---

## 9a — `std/stream` (lazy streams)

A **lazy, pull-based** stream: a source + a chain of transform stages; nothing runs until a
terminal pulls. Represented as a native resource `ResourceState::Stream { source, stages }`
(NOT a `Value::Generator` — avoids constructing native generator bodies). Async (stages invoke
user fns; a generator source is driven via `resume`).

- **Sources:** `stream.from(x)` — `x` is an array (index-pull) OR a generator (`Value::Generator`,
  pulled via the consumer-driven `resume`). `stream.range(start, end, step?)` — a numeric stream.
  `stream.repeat(value, n)` / `stream.once(value)` (small conveniences; include if cheap).
- **Lazy combinators** (each returns a NEW stream with a stage appended; no work yet):
  `map(s, fn)`, `filter(s, fn)`, `take(s, n)`, `drop(s, n)`, `flatMap(s, fn)` (fn returns an
  array/stream, flattened one level), `enumerate(s)` (→ stream of `[index, value]`), `zip(s, t)`
  (pairs until either ends).
- **Terminals** (drive the pull): `collect(s) -> array`, `forEach(s, fn)`, `reduce(s, fn, init)
  -> value`, `count(s) -> number`, `find(s, fn) -> value|nil`, `first(s) -> value|nil`.
- **Pull engine:** a terminal repeatedly pulls the next source item and threads it through the
  stages: `filter` skips (loop), `map` transforms, `take` stops after N, `drop` skips first N,
  `flatMap` buffers the expanded items. Laziness means `take(map(hugeRange, f), 3)` only
  evaluates `f` 3 times. Single-consumption (a stream is consumed by one terminal; document —
  re-collecting a consumed stream yields empty / or is a Tier-2 error; pick & document).
- Borrow discipline: the pull loop drives async stages/generator-resume without holding a
  `resources`/`RefCell` borrow across `.await` (take the stream state out / clone stage fns).

### Tests (9a)
`stream.from([1,2,3]) |> map(*2) |> collect` → `[2,4,6]`; laziness: `stream.range(0, 1000000) |>
map(sideEffectCounter) |> take(3) |> collect` evaluates the map only 3 times (assert counter==3);
filter; flatMap (`[1,2] -> [[1],[2,2]]` flattened); enumerate; reduce sum; from a generator
source (`stream.from(genFn())`) collects its yields; zip; find/first/count. Example.

(Calling convention: module-qualified `stream.map(s, fn)`; if the codebase supports method
dispatch on the stream native handle, `s.map(fn)` too — mirror how other native handles do it;
streams chain naturally either way.)

---

## 9b — `std/assert` (rich assertions)

A module of assertions for use in `test(name, fn)` bodies. Each FAILS via a **Tier-2 panic**
with a clear, value-showing message (a failing assertion fails the test). Complements the
existing global `assert(cond)`.
- `assert.eq(a, b, msg?)` / `assert.ne(a, b, msg?)` — deep structural equality (reuse
  `object.deep_equal` so arrays/objects/maps compare by value, not identity). Message shows both
  values.
- `assert.true(x)` / `assert.false(x)` — truthiness.
- `assert.nil(x)` / `assert.notNil(x)`.
- `assert.gt/gte/lt/lte(a, b)` — numeric/decimal ordering.
- `assert.contains(haystack, needle)` — substring (string) / membership (array) / key (object).
- `assert.approxEq(a, b, epsilon?)` — float tolerance (default small epsilon).
- `assert.throws(fn) -> err` — calls `fn`; passes iff it panics; returns the caught error (so the
  test can assert on the message). Uses the `recover` machinery.
- Decision: `assert.eq` is DEEP (structural) — the common test need (comparing arrays/objects).
  Document that it differs from `==` (identity for containers).

### Tests (9b)
each assertion passes on the true case and panics (Tier-2) on the false case (observe via
recover); `assert.eq` deep-compares arrays/objects; `assert.throws` catches + returns the error;
messages include the values.

---

## 9c — `assert.snapshot` (snapshot testing) + `std/bench`

### `assert.snapshot(name, value)` (in `std/assert`, sys-gated for fs)
- Serializes `value` (via `json.stringify` pretty / a stable repr) and compares against a stored
  snapshot file at `__snapshots__/<name>.snap` (relative to cwd; document the location).
- First run (no file) → writes the snapshot and passes. Subsequent runs → compares; mismatch →
  Tier-2 panic showing a diff/both values. An env var `ASCRIPT_UPDATE_SNAPSHOTS=1` → overwrite
  (update mode). Document.
- sys-gated (writes files). If `sys` is off, `assert.snapshot` is unavailable (the `assert`
  module's core assertions stay available; only `snapshot` is gated — structure so the rest of
  `std/assert` is core and `snapshot` is the sys-gated arm).

### `std/bench` (micro-benchmark timing) — core
- `bench.measure(fn, iterations?) -> {iterations, totalMs, avgMs, opsPerSec}` — runs `fn`
  `iterations` times (default a sensible N, or auto-calibrate to ~a target duration), timing via
  `time.monotonic`; returns stats. async (fn may be async — await it).
- `bench.compare({name: fn, ...}, iterations?) -> array<{name, avgMs, opsPerSec}>` — convenience
  to benchmark several fns (optional; include if cheap).
- Decision: `std/bench` is a measurement helper (returns stats), NOT wired into `ascript test`
  (keep it a library; a script prints/asserts on the stats). Document.

### Tests (9c)
snapshot: first call writes + passes (use a temp cwd / unique name + clean up); second call with
same value passes; with a different value panics; update-mode env var overwrites. bench:
`bench.measure(fn, 100)` returns stats with iterations==100, avgMs>=0, opsPerSec>0; an async fn
is awaited.

---

## 9d — integration

- `examples/streams_and_testing.as`: a lazy stream pipeline (range → filter → map → take →
  collect, showing laziness), and a `test(...)` block using `std/assert` (eq/throws), plus a
  `bench.measure` printing stats. Bounded, terminates, prints success. (Snapshot demo optional —
  if shown, write to a temp/example snapshot and note it.)
- Docs: `std/stream` page (sources/combinators/terminals/laziness/single-consumption), `std/assert`
  page (assertions + snapshot + the deep-eq note), `std/bench` page; README stdlib table; a note
  in the testing docs (`docs/content/...` testing/CLI page) about `std/assert` + `ascript test`.
- Full gates (both test configs, clippy both, fmt, conformance, idempotence); holistic review;
  merge `--no-ff`.

## Decisions (made; flagged)
1. `std/stream` is a native lazy pull engine (source=array|generator, staged combinators), NOT a
   Value::Generator — avoids native-generator-body construction. Single-consumption. **Settled.**
2. `assert.eq` is DEEP structural (reuses object.deep_equal). **Settled.**
3. `assert.throws(fn)` returns the caught error for further assertions. **Settled.**
4. `assert.snapshot` is file-based (`__snapshots__/<name>.snap`), sys-gated, with
   `ASCRIPT_UPDATE_SNAPSHOTS` update mode; the rest of `std/assert` is core. **Settled.**
5. `std/bench` is a timing-stats library (not wired into the test runner). **Settled.**

## Open implementation choices (decide during impl, document)
- Stream re-consumption after a terminal: empty vs Tier-2 error — pick one, document.
- `stream.from(generator)`: confirm the generator `resume` API is callable from the stream pull
  engine (it's consumer-driven; the pull loop resumes it). If driving a `Value::Generator` from
  native stream code is awkward, document array-source as primary + generator-source support level.
- bench default iterations / auto-calibration — keep simple (fixed default + explicit override).
- snapshot serialization format + diff verbosity — JSON pretty; show both values on mismatch.
