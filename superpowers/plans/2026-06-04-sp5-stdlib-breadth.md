# SP5 — Stdlib breadth (conventional batteries) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended)
> or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`)
> syntax for tracking.

**Goal:** Add conventional stdlib batteries — schema collect-all, HTTP query/param schemas, typed
CSV/TOML/YAML parse, MessagePack + CBOR, zstd/brotli/tar compression, Postgres + Redis clients, LRU/
events/template utilities, and an intl long-month-name fix — each ADDITIVE and engine-agnostic
(dispatched through the shared native `call_stdlib`, so no VM/tree-walker differential risk).

**Architecture:** Eight phases (1–8), one per module-group, plus a closing docs/holistic phase (9).
Each phase is TDD, ends green on BOTH feature configs + clippy in both, and gets an independent review
before the next. Adding a module touches the **3-point registration contract** in `src/stdlib/mod.rs`
(`std_module_exports`, `STD_MODULES`, `call_stdlib`) + the `pub mod` declaration + example + docs page.

**Tech Stack:** Rust. Native `std/*` modules over the `Value` model. Pure `call(func, args, span)` for
stateless modules; async `Interp::call_*(&self, ...)` for runtime/resource modules. Tier-1 `[value,
err]` via `make_pair`/`make_error`; Tier-2 `AsError::at(...).into()`. Native resources via
`register_resource`/`take_resource`/`return_resource` + `ResourceState`/`NativeKind`.

**Spec:** `docs/superpowers/specs/2026-06-04-sp5-stdlib-breadth-design.md`.

**Branch:** `feat/sp1-engine-parity` (current; SP5 work lands on its own branch off the post-cutover
base per the milestone workflow — create `feat/sp5-stdlib-breadth` before Phase 1).

---

## Conventions for every task

- **Build/test gate after each phase (paste tails):**
  `cargo build 2>&1 | tail`;
  `cargo test 2>&1 | tail` (0 failures all binaries, default features);
  `cargo test --no-default-features 2>&1 | tail` (0 failures);
  `cargo clippy --all-targets 2>&1 | tail` AND `cargo clippy --no-default-features --all-targets 2>&1 | tail` (clean);
  `grep await_holding_refcell_ref Cargo.toml` (still `deny`).
- **Run an `.as` program both engines** (sanity, where relevant):
  `cargo run -- run X.as` (VM) vs `cargo run -- run --tree-walker X.as` — must be byte-identical.
- **Run a `.as` file's `test(name, fn)` registrations:** `cargo run -- test X.as`.
- **3-point registration (every new module):** add the `#[cfg]`-gated `pub mod` at `src/stdlib/mod.rs`
  top; add the arm to `std_module_exports` (gated); add the un-gated entry to `STD_MODULES`; add the
  arm to `call_stdlib` (gated). The module-resolution tests + `unresolved-import` checker enforce sync.
- **Per-module:** an `examples/<name>.as` (introductory, exercised by conformance) and, where
  production-shaped (DB, msgpack/cbor pipeline), an `examples/advanced/<name>.as` fully error-handled;
  plus a `docs/content/stdlib/*.md` page; plus the `README.md` stdlib table row.
- **Commit trailer:** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **No `unsafe`, no `#[allow]`, no `#[ignore]`, no stubs.** Read the cited neighbor module before
  writing each new one to match the arm/error/test style.

> **Owner sign-off needed before starting** (from the spec's open questions): schema terminal name
> (`parseAll`); query/param coerce default; CSV typed-row error mode; msgpack/cbor naming + `binary`
> feature; postgres/redis default-on vs opt-in + TLS scope + typed rows; template syntax/missing-key/
> escaping; lru/events resource-vs-object; intl approach (icu4x vs table) + locale set. The tasks below
> assume the spec's RECOMMENDED answers; adjust if the owner decides otherwise.

---

## Phase 1 — Schema collect-all-errors (`std/schema`)

**Files:** `src/stdlib/schema.rs`. (The fluent call-site hook in `interp.rs` is unchanged — it routes
any `is_schema_method` name, so adding `parseAll` there is enough.)

### Task 1.1: failing tests for collect-all

- [ ] **Step 1 — Read** `src/stdlib/schema.rs`: `parse_value` (`:419`), the object/array/map arms
  (`:692`/`:640`/`:766`) and how they `?`-short-circuit; `err_obj` (`:137`); `schema.parse` (`:1179`);
  `exports()` (`:60`); `is_schema_method` (`:196`).
- [ ] **Step 2 — Write failing tests.** Add Rust unit tests (drive `Interp` like the existing
  `parse_*_ok` tests, `:1362`+) asserting a collecting engine returns ALL errors. And an E2E `.as`:

```as
import { object, string, number, parseAll } from "std/schema"
let s = object({ a: string(), b: number(), c: string() })
let [val, errs] = parseAll(s, { a: 1, b: "x", c: 2 })
print(val)
for (e in errs) { print(e.path + ": " + e.message) }
```
Expect three error lines (paths `a`, `b`, `c`), `val` nil. Also assert `parse` (fail-fast) still
returns ONLY the first error (regression).

- [ ] **Step 3 — Run, verify fail:** `cargo test schema 2>&1 | tail` (no `parseAll` export yet).

### Task 1.2: implement the collecting engine + `parseAll`

- [ ] **Step 4 — Implement** a private `parse_value_collect(&self, schema, value, path, coerce, span,
  errors: &mut Vec<Value>) -> Result<Value, ParseFail>` mirroring `parse_value` but, in the
  object/array/map/union arms, pushing the `Mismatch` `err_obj` into `errors` and continuing
  (substitute `Value::Nil` for the failed child) instead of `return Err`. `InvalidSchema`/`Control`
  still short-circuit. Factor the leaf "expected X, got Y" messages so collect and fail-fast emit
  identical wording. Add `"parseAll"` to `exports()`, to the `call_schema` match (`:934`) — calls the
  collecting engine, returns `[value, nil]` when `errors` empty else `[nil, Array(errors)]` — and to
  `is_schema_method` (`:196`).
- [ ] **Step 5 — Run:** `cargo test schema 2>&1 | tail` → green; the E2E prints three errors.
- [ ] **Step 6 — Phase-1 gate** (full set) + `docs/content/stdlib/schema.md` updated +
  `examples/schema_collect.as` runnable both engines.
- [ ] **Step 7 — Commit:** `feat(schema): parseAll collect-all-errors validation mode`.

---

## Phase 2 — HTTP query + path-param schemas (`std/http/server`)

**Files:** `src/stdlib/http_server.rs`.

### Task 2.1: failing tests

- [ ] **Step 1 — Read** `http_server.rs`: route tuple `(method, path, schema?, handler)` (`:126`),
  `register_route` (`:558`), verb/route dispatch (`:599-638`), the body-schema validation block
  (`:979-1010`), `schema_error_response` (`:366`), `match_route` params (`:310`), `req_obj` build
  (`:950-957`), and the async test helpers (`schema_route_valid_body_*` `:1919`, `path_param_*` `:1294`,
  `query_params_are_parsed` `:1462`).
- [ ] **Step 2 — Write failing async tests** (model the neighbors): a `:id` param schema
  `number()` coerces `"7"`→`7` (handler echoes `req.params.id + 1` → `8`); a bad param → 400 with
  body containing `"params"`; a query schema coerces `?page=2`; a route with `{params, query, body}`
  all set; and the back-compat bare-body-schema route still works. Drive via the existing bind→serve
  test harness (`:2077`+).
- [ ] **Step 3 — Run, verify fail:** `cargo test --test cli 2>&1 | tail` / the http_server unit tests.

### Task 2.2: implement route schemas struct + validation

- [ ] **Step 4 — Implement** a `RouteSchemas { params: Option<Value>, query: Option<Value>, body:
  Option<Value> }`; replace the route tuple's `schema?` slot with it. In dispatch (`:599-638`): a 3rd
  arg that is a bare schema (`schema_kind(arg).is_some()`) → `body` only (today's behavior); an Object
  WITHOUT `__kind` → read `params`/`query`/`body` schemas from it. In the dispatch validation block
  (`:979`), validate `params` then `query` (with `coerce=true`) then `body` (`coerce=false`), replacing
  the request fields with coerced results on success; on `Mismatch` return 400 via
  `schema_error_response` extended with a `where` field. Keep borrow discipline (clone out before await).
- [ ] **Step 5 — Run** the Task-2.1 tests → green.
- [ ] **Step 6 — Phase-2 gate** + update `docs/content/stdlib/net.md` (route schemas) + extend the
  `examples/advanced/typed_http.as` example with a typed query/param route.
- [ ] **Step 7 — Commit:** `feat(http/server): typed query + path-param route schemas (coerced)`.

---

## Phase 3 — Typed parse for CSV / TOML / YAML

**Files:** `src/stdlib/mod.rs` (pre-dispatch typed blocks + a shared `typed_decode` helper). The
`csv`/`toml`/`yaml` module `call`s are UNCHANGED.

### Task 3.1: failing tests

- [ ] **Step 1 — Read** the json typed-parse precedent in `Interp::call_stdlib`
  (`src/stdlib/mod.rs:256-310`) and `validate_into` (`src/interp.rs:2481`); the module parse fns
  (`csv.rs:27`, `toml.rs:23`, `yaml.rs:18`); the `resp.json(Class|schema)` path (`net_http.rs:1435`).
- [ ] **Step 2 — Write failing E2E `.as` tests** (add to `tests/modules.rs` or a new `.as` exercised by
  conformance):

```as
import { parse } from "std/toml"
class Config { host: string  port: number }
let [cfg, err] = parse("host = \"localhost\"\nport = 8080", Config)
print(err)
print(cfg.host + ":" + cfg.port)
```
plus a YAML analogue, plus a CSV row→class:
```as
import { parse } from "std/csv"
class Row { name: string  age: number }
let [rows, err] = parse("name,age\nAda,36\nGrace,37", Row, { header: true })
print(err)
for (r in rows) { print(r.name + " " + r.age) }
```
and a shape-mismatch case asserting `[nil, err]` (no panic).

- [ ] **Step 3 — Run, verify fail** (the 2nd `Class` arg is currently ignored by toml/yaml/csv).

### Task 3.2: implement typed blocks + shared helper

- [ ] **Step 4 — Implement** in `call_stdlib`: a private
  `async fn typed_decode(&self, decoded: Value, type_arg: &Value, span) -> Result<Value, Control>`
  returning the `[value, err]` pair (Class → `validate_into`; tagged schema → `parse_value`; fuse
  errors) — extracted from the existing json block so json/toml/yaml share it. Add
  `#[cfg(feature="data")]` pre-dispatch blocks for `toml.parse`/`yaml.parse` (whole-document
  `typed_decode`) and `csv.parse` (call the 1-arg module parse, then `validate_into`/`parse_value` per
  row with a `row[N]` path, fail-fast on the first bad row). Refactor the json block to use
  `typed_decode` (behavior unchanged — keep its existing tests green).
- [ ] **Step 5 — Run:** the Task-3.1 tests → green; existing `json.parse(text, Class)` tests still green.
- [ ] **Step 6 — Phase-3 gate** + update `docs/content/stdlib/data.md` (typed parse for csv/toml/yaml) +
  extend `examples/advanced/typed_api.as` (or add `examples/typed_config.as`).
- [ ] **Step 7 — Commit:** `feat(stdlib): typed parse (Class/schema) for csv/toml/yaml via shared validate_into`.

---

## Phase 4 — MessagePack + CBOR (`std/msgpack`, `std/cbor`)

**Files:** new `src/stdlib/msgpack.rs` + `src/stdlib/cbor.rs`; `src/stdlib/mod.rs` (register +
typed-decode blocks); `Cargo.toml` (`binary` feature).

### Task 4.1: Cargo feature + module skeletons (failing tests)

- [ ] **Step 1 — Cargo.toml:** add `rmpv = { version = "1", optional = true }` and
  `ciborium = { version = "0.2", optional = true }`; add `binary = ["dep:rmpv", "dep:ciborium"]`;
  add `"binary"` to `default`.
- [ ] **Step 2 — Write failing tests.** Create `src/stdlib/msgpack.rs` + `cbor.rs` with `exports()`
  (`encode`/`decode`) and a `call` stub returning a not-implemented error; add Rust round-trip unit
  tests per module (every `Value` kind: int+float Number, Str, Bool, Nil, nested Array/Object, Bytes,
  Map) and a fixture test (a known byte sequence decodes to the expected value). E2E `.as`:
```as
import { encode, decode } from "std/msgpack"
let bytes = encode({ name: "Ada", nums: [1, 2, 3], ok: true })
let [val, err] = decode(bytes)
print(err)
print(val.name + " " + val.nums[1])
```
- [ ] **Step 3 — Register** both modules (3-point + `pub mod`, gated on `binary`). Run, verify the
  tests fail (stub).

### Task 4.2: implement encode/decode + typed decode

- [ ] **Step 4 — Implement** the `Value`↔`rmpv::Value` and `Value`↔`ciborium::value::Value` bridges
  (mirror json's `from_ascript`/decode mapping in `src/stdlib/json.rs`): Number→int when it round-trips
  as an integer else float; Str→string; Bool/Nil; Array→array; Object/Map→map; Bytes→binary. Decode:
  map → `Object` if all keys are strings else `Map` (verify json's convention). `encode` is a total
  data mapping (Tier-2 only on a genuinely unrepresentable handle like a function/native); `decode` is
  Tier-1 (malformed → err).
- [ ] **Step 5 — Typed decode:** add `#[cfg(feature="binary")]` pre-dispatch blocks in `call_stdlib`
  for `msgpack.decode`/`cbor.decode` with a 2nd `Class|schema` arg, reusing the Phase-3 `typed_decode`
  helper.
- [ ] **Step 6 — Run** all Phase-4 tests → green (and `--no-default-features` cfg's the modules out
  cleanly: confirm `STD_MODULES` still lists them un-gated so the checker accepts the import).
- [ ] **Step 7 — Phase-4 gate** + `docs/content/stdlib/data.md` (or a new `binary.md`) +
  `examples/binary_serialization.as`.
- [ ] **Step 8 — Commit:** `feat(stdlib): std/msgpack + std/cbor binary serialization (binary feature)`.

---

## Phase 5 — Compression: zstd + brotli + tar (`std/compress`)

**Files:** `src/stdlib/compress.rs`; `Cargo.toml` (extend `compress`).

### Task 5.1: Cargo deps + failing tests

- [ ] **Step 1 — Cargo.toml:** extend `compress = ["dep:flate2", "dep:zip", "dep:zstd", "dep:brotli",
  "dep:tar"]`; add `zstd = { version = "0.13", optional = true }`,
  `brotli = { version = "7", optional = true }`, `tar = { version = "0.4", optional = true }`.
- [ ] **Step 2 — Read** `src/stdlib/compress.rs` end-to-end (the gzip/zip arms, `source_bytes` `:35`,
  `err_pair` `:51`, `build_zip`/`extract_zip` `:126`/`:192`, and the round-trip/garbage/Tier-2 tests).
- [ ] **Step 3 — Write failing tests** (model the existing ones): zstd round-trip (string + binary),
  brotli round-trip, "actually compresses" repetitive input, garbage→Tier-1 err, string→`*Decompress`
  is Tier-2 panic; tar create+extract round-trip with a text + a binary `{name, data}` entry.
- [ ] **Step 4 — Add** the six exports (`zstdCompress`/`zstdDecompress`/`brotliCompress`/
  `brotliDecompress`/`tarCreate`/`tarExtract`) to `exports()` and stub their `call` arms. Run, verify fail.

### Task 5.2: implement codecs + tar

- [ ] **Step 5 — Implement** the arms: zstd/brotli compress accept string-or-bytes (reuse
  `source_bytes`), return bytes; decompress is Tier-1; `tarCreate`/`tarExtract` reuse the EXACT
  `{name, data}` entry-object shape as zip (factor the entry build/extract with `build_zip`/`extract_zip`
  where it helps). Optional `level`/`quality` 2nd arg (default crate default).
- [ ] **Step 6 — Run** all compress tests → green.
- [ ] **Step 7 — Phase-5 gate** + `docs/content/stdlib/system.md` (or wherever compress is documented)
  + extend `examples/advanced/crypto_and_compress.as`.
- [ ] **Step 8 — Commit:** `feat(compress): zstd + brotli codecs + tar archives`.

---

## Phase 6 — DB clients: Postgres + Redis (`std/postgres`, `std/redis`)

**Files:** new `src/stdlib/postgres.rs` + `src/stdlib/redis.rs`; `src/interp.rs` (`ResourceState` +
`NativeKind` variants + dispatch methods); `src/value.rs` (`NativeKind` variants); `src/stdlib/mod.rs`
(register); `Cargo.toml` (`postgres`/`redis` features). Read `src/stdlib/sqlite.rs` + the resource API
(`src/interp.rs:93` `ResourceState`, `:697-740` register/take/return) FIRST — it is the exact template.

### Task 6.1: Cargo features + resource plumbing

- [ ] **Step 1 — Cargo.toml:** add `tokio-postgres = { version = "0.7", optional = true }` and
  `redis = { version = "0.27", features = ["tokio-comp"], optional = true }`; add
  `postgres = ["dep:tokio-postgres"]` and `redis = ["dep:redis"]` (default-on per the spec's
  recommendation — flip to opt-in if the owner decides). Add the `#[cfg]`-gated `pub mod` + 3-point
  registration (stubs). Add `STD_MODULES` entries `"std/postgres"`, `"std/redis"` (un-gated).
- [ ] **Step 2 — Resource variants:** add `ResourceState::PostgresConnection { client, conn_task:
  AbortHandle }` and `ResourceState::RedisConnection(...)` (`src/interp.rs:93`), and matching
  `NativeKind` variants (`src/value.rs:210`). Confirm the GC `Trace` for these handles is no-op (native
  resources stay on `Rc`, per the value.rs invariant).

### Task 6.2: Postgres — connect, query, exec, close (TDD)

- [ ] **Step 3 — Write unit tests (always run, no server):** `await postgres.connect(
  "postgres://127.0.0.1:1/none")` → clean Tier-1 err (NOT a panic); a constructed-`Row`→`Value`
  type-map helper test (Rust-level, no live connection). And a LIVE integration test gated by env var:
```rust
#[tokio::test(flavor = "current_thread")]
async fn pg_roundtrip_live() {
    let Ok(url) = std::env::var("ASCRIPT_TEST_POSTGRES_URL") else { return; }; // skip when unset
    // connect -> CREATE TEMP TABLE <uuid> -> insert -> query -> assert -> close
}
```
(Run inside a `LocalSet` like the other interp tests; use a UUID-suffixed temp table; clean up.)
- [ ] **Step 4 — Implement** `Interp::call_postgres_open` (async `connect`): `tokio_postgres::connect`
  → `spawn_local` the driver `Connection` future, store the `Client` + the task `AbortHandle` in
  `ResourceState::PostgresConnection`, return a `Value::Native`. Implement `query`/`exec`/`queryOne`/
  `begin`/`commit`/`rollback`/`close` as `NativeMethod`s dispatched via an async
  `call_postgres_method` — using the take-out-across-await pattern (`take_resource` → await on the owned
  client → `return_resource`); NEVER hold a `resources`/`RefCell` borrow across `.await`. Row→`Value`
  per the spec type-map (numeric/decimal→Str to avoid precision loss; bytea→Bytes; json/jsonb→decoded).
  `close` aborts the driver task and drops the client.
- [ ] **Step 5 — Run:** `cargo test postgres 2>&1 | tail` (unit + dead-port pass; live no-ops when unset).
  With a server: `ASCRIPT_TEST_POSTGRES_URL=postgres://localhost/postgres cargo test pg_roundtrip_live`.
- [ ] **Step 6 — Commit:** `feat(stdlib): std/postgres async client (tokio-postgres, native resource)`.

### Task 6.3: Redis — connect, command, conveniences, close (TDD)

- [ ] **Step 7 — Write tests** mirroring 6.2: dead-port connect → Tier-1 err; reply→`Value` map unit
  test; env-gated (`ASCRIPT_TEST_REDIS_URL`) live round-trip (set/get/del with a UUID key prefix).
- [ ] **Step 8 — Implement** `connect` (store the async `redis::aio` connection in
  `ResourceState::RedisConnection`) and methods `command`/`get`/`set`/`del`/`incr`/`expire`/`exists`/
  `close`, async, Tier-1, take-out-across-await. Reply→`Value` per the spec map.
- [ ] **Step 9 — Run** redis tests → green (live no-ops when unset).
- [ ] **Step 10 — Phase-6 gate** + `docs/content/stdlib/db.md` (postgres + redis, with the env-var +
  `docker run` instructions for live tests) + `examples/advanced/postgres_crud.as` +
  `examples/advanced/redis_cache.as` (both fully error-handled, model on `sqlite_crud.as`).
- [ ] **Step 11 — Commit:** `feat(stdlib): std/redis async client (redis tokio-comp, native resource)`.

---

## Phase 7 — Utilities: LRU + events + template (core, un-gated)

**Files:** new `src/stdlib/lru.rs` + `events.rs` + `template.rs`; `src/interp.rs`/`src/value.rs`
(`ResourceState`/`NativeKind` for lru + events); `src/stdlib/mod.rs` (register UN-GATED, like `set`).

### Task 7.1: LRU (TDD)

- [ ] **Step 1 — Read** an un-gated pure module (`src/stdlib/set.rs`, `map.rs`) for the registration
  + arm style, and the resource API (sqlite/interp) for the Native handle.
- [ ] **Step 2 — Write failing tests + E2E `.as`:**
```as
import { new } from "std/lru"
let cache = new(2)
cache.set("a", 1)
cache.set("b", 2)
cache.get("a")        // promotes "a"
cache.set("c", 3)     // evicts LRU ("b")
print(cache.has("a")) // true
print(cache.has("b")) // false
print(cache.len())    // 2
```
- [ ] **Step 3 — Implement** `lru.new(capacity)` → `Value::Native` (`ResourceState::Lru` holding an
  `IndexMap<MapKey, Value>` + capacity + recency); methods `get`/`set`/`has`/`delete`/`clear`/`len`/
  `keys` via a `call_lru_method` dispatch. Eviction = least-recently-used; `get`/`set` mark MRU.
  Register UN-GATED (3-point + `pub mod`, no `#[cfg]`). Run → green.
- [ ] **Step 4 — Commit:** `feat(stdlib): std/lru bounded LRU cache (core)`.

### Task 7.2: events (TDD)

- [ ] **Step 5 — Write tests + E2E:** `on`/`emit` calls listeners in order with args; `once` fires
  once; `off` removes; async listeners awaited; `listenerCount`.
- [ ] **Step 6 — Implement** `events.new()` → `Value::Native` (`ResourceState::Events` holding
  per-event listener lists); `on`/`once`/`off`/`listenerCount` sync, `await emit(event, ...args)` async
  (call each listener via `call_value`, awaiting in registration order; take listeners out before
  await per borrow discipline). Register un-gated. Run → green.
- [ ] **Step 7 — Commit:** `feat(stdlib): std/events event-emitter (core)`.

### Task 7.3: template (TDD)

- [ ] **Step 8 — Write tests + E2E:** `template.render("Hi {{name}}, {{a.b}}", {name:"Ada", a:{b:1}})`
  → `"Hi Ada, 1"`; missing key → Tier-1 err (per spec recommendation — adjust if owner picks lenient);
  literal text with no placeholders passes through.
- [ ] **Step 9 — Implement** a pure `template.render(tmpl, data) -> [string, err]`: scan for
  `{{path}}`, resolve dotted paths against an Object/Instance/Map, substitute; missing key → err. No
  loops/conditionals (documented limitation). Pure `call` (no resource). Register un-gated. Run → green.
- [ ] **Step 10 — Phase-7 gate** (MUST pass `--no-default-features` — these are core) +
  `docs/content/stdlib/utilities.md` (lru + events + template) + `examples/` for each.
- [ ] **Step 11 — Commit:** `feat(stdlib): std/template string templating (core)`.

---

## Phase 8 — intl long month/day names (locale-correct)

**Files:** `src/stdlib/intl.rs` (+ `Cargo.toml` only if option A needs an icu sub-feature).

### Task 8.1: failing tests

- [ ] **Step 1 — Read** `intl.rs`: the limitation note (`:27`), `date_pattern` long-style `%B`
  (`:112,114`), `formatDate` (the arm in `call`), and the existing intl tests + the
  `examples/advanced/datetime_intl.as` example to enumerate the SUPPORTED locale set.
- [ ] **Step 2 — Write failing tests:** `intl.formatDate(instant, "de-DE", "long")` contains a German
  month name (e.g. `"März"`), `"fr-FR"` a French one, `"ja-JP"` keeps `年月日`, `"en-US"` unchanged.
  These fail today (English `%B` everywhere).

### Task 8.2: implement locale-correct names

- [ ] **Step 3 — Implement** the owner-chosen approach: (A) source long month/weekday names from icu4x
  symbols data and substitute for the `%B` token; or (B) a curated static table keyed by
  `loc.id.language` (+ region) over the supported locales, substituted into the rendered string. Remove
  the "Month names are English" limitation note; document the actual coverage in the module doc.
- [ ] **Step 4 — Run:** intl tests → green; `datetime_intl.as` updated to show non-English months.
- [ ] **Step 5 — Phase-8 gate** + update `docs/content/stdlib/` intl/datetime page.
- [ ] **Step 6 — Commit:** `fix(intl): locale-correct long month/day names (replaces English %B)`.

---

## Phase 9 — Docs, README, holistic review

**Files:** `docs/content/stdlib/*.md`, `README.md`, `docs/superpowers/roadmap.md`.

### Task 9.1: docs + README

- [ ] **Step 1 — Verify** every new module has a `docs/content/stdlib/*.md` page and the `README.md`
  stdlib table lists it (with the feature flag where gated). Verify each documented snippet against the
  built binary (`cargo run -- run` the doc examples).
- [ ] **Step 2 — Record** the milestone in `docs/superpowers/roadmap.md`.
- [ ] **Step 3 — Commit:** `docs: SP5 stdlib breadth — schema/http/typed-parse/binary/compress/db/utils/intl`.

### Task 9.2: holistic gate + review

- [ ] **Step 4 — Full gate set** BOTH feature configs + clippy BOTH + confirm `treesitter_conformance`
  + `frontend_conformance` green (new examples parse on all parsers) + the whole-corpus three-way
  differential unchanged (no opcode/Value eval change, so it should be untouched — confirm).
- [ ] **Step 5 — Independent review:** re-read the spec; re-run gates; adversarial probe —
  schema collect-all error ordering/paths, HTTP coercion edge cases, typed-parse error fusion (decode
  vs shape), msgpack/cbor lossy edges (int vs float, Object vs Map), DB take-out-across-await borrow
  discipline (run clippy `await_holding_refcell_ref` — must be clean), connection Drop/abort on close,
  LRU eviction order, events async ordering, template missing-key, intl per-locale names. Fix any issue
  at the root.
- [ ] **Step 6 — Final commit** if the review surfaced fixes; otherwise SP5 is complete.

---

## Self-review (author)

**Spec coverage:** §1 schema collect-all → Phase 1; §2 HTTP query/param schemas → Phase 2; §3 typed
csv/toml/yaml → Phase 3; §4 msgpack/cbor → Phase 4; §5 zstd/brotli/tar → Phase 5; §6 postgres/redis →
Phase 6; §7 lru/events/template → Phase 7; §8 intl months → Phase 8; docs/holistic → Phase 9. All
covered.

**Engine-agnostic / differential:** every phase adds surface dispatched through the shared native
`call_stdlib` — no opcode, no `Value` eval-path change. The only `Value`-model edits are ADDITIVE
`NativeKind`/`ResourceState` variants (§6/§7), which are native-resource handles (stay on `Rc`, no-op
`Trace`) and never enter the differential corpus's compute paths. Phase 9 confirms the three-way
differential is untouched.

**Registration contract:** every new module phase adds the `pub mod`, the `std_module_exports` arm, the
un-gated `STD_MODULES` entry, AND the `call_stdlib` arm — enforced by the module-resolution tests and
the `unresolved-import` checker.

**DB-test flakiness:** Phase 6 default `cargo test` touches no live server (dead-port Tier-1 + Rust-
level type-map units); live tests early-return when the env var is unset (no `#[ignore]`), use
UUID-suffixed temp tables/keys, and clean up — no flakiness, no required infra for green CI.

**Typed-decode reuse:** Phases 3/4/6 all reuse the SAME `validate_into`/`parse_value` via the
`typed_decode` helper extracted from the existing json block in Phase 3 — no new validation logic, no
module-`call` signature changes.

**Placeholder scan:** no "TBD/handle edge cases". Crate versions (`rmpv 1`, `ciborium 0.2`, `zstd 0.13`,
`brotli 7`, `tar 0.4`, `tokio-postgres 0.7`, `redis 0.27`), feature names (`binary`, extended
`compress`, `postgres`, `redis`), and the AScript test programs are concrete. The one deliberate
deferral to the implementer is the exact Rust glue per module (read the cited neighbor to match style)
and the owner sign-off items listed at the top.

**Consistency:** feature `binary` defined once (Phase 4) and added to `default`; `compress` extended
once (Phase 5); `postgres`/`redis` features once (Phase 6); §7 modules un-gated (core, must build
`--no-default-features`); the `typed_decode` helper named consistently across Phases 3/4/6.
