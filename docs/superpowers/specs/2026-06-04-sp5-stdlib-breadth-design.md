# SP5 — Stdlib breadth (conventional batteries) — Design

> **Status:** approved design, ready for implementation planning (superpowers:writing-plans).
> **Sub-project of** the post-cutover gap program (gap register in the session handoff; SP1–SP10).
> **Scope note:** `std/ai` and `std/telemetry` are SEPARATE sub-projects and are explicitly NOT in SP5.

**Goal:** Broaden the standard library toward the "Lua-simple language, Go/Deno-class stdlib" design
goal with conventional batteries that real programs reach for: collect-all schema validation, typed
HTTP query/param schemas, typed parse for CSV/TOML/YAML, binary serialization (MessagePack + CBOR),
richer compression (zstd + brotli + tar), first-class DB clients (Postgres + Redis), pure in-process
utilities (LRU cache, event-emitter, templating), and a locale-correct fix to `intl` long month/day
names.

**Architecture (why SP5 is low-risk):** Every `std/*` module is native Rust over the `Value` model.
BOTH engines (the bytecode VM and the `--tree-walker` reference engine) dispatch qualified stdlib
calls through the SAME native entry point — `Interp::call_stdlib(module, func, args, span)`
(`src/stdlib/mod.rs:232`), which routes to each module's `call(...)` / `self.call_*(...)`. A stdlib
addition is therefore **additive and engine-agnostic**: both engines call the identical native fn, so
there is **no VM/tree-walker differential risk** for new module surface (unlike SP1, which touched
compiler/VM lowering). The whole-corpus three-way differential is preserved automatically because no
opcode, no `Value` variant, and no eval path changes.

**The 3-point registration contract for a new module** (verified against `src/stdlib/mod.rs`): adding
`std/foo` means touching exactly:
1. `std_module_exports(path)` — the import match arm (`src/stdlib/mod.rs:92-167`), feature-gated.
2. `STD_MODULES` — the feature-INDEPENDENT canonical list (`src/stdlib/mod.rs:177-222`), un-gated, so
   the static checker's `unresolved-import` rule (`src/check/rules/unresolved_import.rs:47`) accepts
   the specifier even in a `--no-default-features` build.
3. `call_stdlib` routing — the dispatch match arm (`src/stdlib/mod.rs:311-390`), feature-gated.

Plus: declare `pub mod foo;` (feature-gated) at the top of `src/stdlib/mod.rs:7-77`, add the
example (`examples/*.as` + an `examples/advanced/*.as` where production-shaped), and a
`docs/content/stdlib/*.md` page.

**Tech stack:** Rust. Native modules expose `exports() -> Vec<(&'static str, Value)>` and either a
pure `call(func, args, span) -> Result<Value, Control>` (e.g. `json`, `csv`, `compress`) OR an async
`Interp::call_foo(&self, func, args, span)` method (e.g. `sqlite`, `net_http`) when they touch the
async runtime or native resources. Native functions are ordinary `function` values; argument-type
misuse is a Tier-2 panic (spec §11.3). Fallible runtime ops follow the Tier-1 `[value, err]`
convention.

---

## Conventions used throughout (verified against the codebase)

- **Tier-1 `[value, err]`** via `crate::interp::make_pair(val, err)` and
  `make_error(Value::Str(msg.into()))` — e.g. `compress.rs:51` `err_pair`, `csv.rs:43`.
- **Tier-2 panic** via `AsError::at(msg, span).into()` for arg-type misuse / programmer errors.
- **Arg helpers** in `src/stdlib/mod.rs`: `arg(args, i)`, `want_string`, `want_number`, `want_array`,
  `want_object`, `want_bytes`, `want_number`.
- **Bytes value:** `Value::Bytes(Rc<RefCell<Vec<u8>>>)` (note: `Bytes` stays on `Rc`, NOT `gcmodule::Cc`).
- **Object/Array construction:** `Value::Object(crate::value::ObjectCell::new(IndexMap))`,
  `Value::Array(gcmodule::Cc::new(RefCell::new(vec)))`.
- **Typed decode** reuses the SAME `validate_into` / `parse_value` core the existing
  `json.parse(text, Class|schema)` uses — see the §3 integration note.
- **Native resources** (DB connections) live in `Interp.resources` referenced by a `Value::Native`
  handle id; the resource discipline (`register_resource`/`take_resource`/`return_resource`,
  `src/interp.rs:697-740`, `ResourceState` enum `src/interp.rs:93`, `NativeKind` `src/value.rs:210`)
  is described in §6.

---

## §1 — Schema collect-all-errors mode (`std/schema`)

### Current behavior (verified)
`Interp::parse_value` (`src/stdlib/schema.rs:419`) is **fail-fast**: the `"object"`, `"array"`, and
`"map"` composite arms `return Err(ParseFail::Mismatch(errObj))` on the FIRST failing field/element
(e.g. `schema.rs:751` `self.parse_value(...).await?` inside the object field loop). `schema.parse`
(`schema.rs:1179`) returns a Tier-1 `[value, err]` where `err` is a single `{path, message}` Object
(`err_obj`, `schema.rs:137`) or `nil`.

### Target semantics
Add a **collect-all** mode that returns EVERY validation error instead of stopping at the first.

- New terminal: **`schema.parseAll(schema, value[, options])`** (free fn) and the fluent method
  `s.parseAll(value)` — add `"parseAll"` to `exports()` (`schema.rs:60`), to the dispatch match
  (`schema.rs:934`), and to `is_schema_method` (`schema.rs:196`) so the call-site fluent hook routes
  it (the hook in `interp.rs` already routes any `is_schema_method` name).
- **Return shape:** `[value, errors]` where on success `errors` is `nil` and `value` is the validated
  value (identical to `parse`); on failure `value` is `nil` and `errors` is an **Array** of
  `{path, message}` Objects (the SAME `err_obj` shape, so error consumers are uniform) — one entry per
  leaf failure, in deterministic document order (object fields in declared order, array elements by
  index, map entries in iteration order). An empty array is never returned for a failure (failure ⇒
  ≥1 error).
- **`parse` is unchanged** (fail-fast, single `{path, message}`). `parseAll` is purely additive.
- **Composites only differ in accumulation:** primitive arms (string/number/bool/nil/literal) still
  produce exactly one error each; the difference is the object/array/map/union arms KEEP GOING and
  gather child errors rather than short-circuiting.
- **`refine`, `default`, `coerce`, `strict`** behave identically; a failing `refine` contributes one
  error to the collection. An `InvalidSchema` (malformed schema) and a `Control` (a panic from a
  user `refine` fn) are STILL Tier-2 / re-raised immediately — collect-all only accumulates Tier-1
  `Mismatch`es, never swallows a programmer error.

### Implementation
- Introduce an internal **collecting variant** of the engine. Recommended shape: a private
  `parse_value_collect(&self, schema, value, path, coerce, span, errors: &mut Vec<Value>) ->
  Result<Value, ParseFail>` that mirrors `parse_value` but, in the object/array/map/union arms,
  pushes a `Mismatch`'s `err_obj` into `errors` and continues (substituting `Value::Nil` for the
  failed sub-value so traversal proceeds) instead of returning. `InvalidSchema`/`Control` still
  short-circuit via `?`. `parse_value` stays the fail-fast path (used by `parse`, `json.parse(...,
  schema)`, `resp.json(schema)`, the HTTP route body schema) — do NOT change its behavior. To avoid
  divergence between the two engines' error wording, factor the leaf checks (the exact
  `"expected X, got Y"` strings) so both paths emit byte-identical messages.
- `schema.parseAll` calls the collecting engine; if `errors` is empty → `[value, nil]`, else
  `[nil, Array(errors)]`.

### Open question for the owner
- **Name:** `parseAll` (chosen here) vs. `parse(s, v, {collectAll: true})` as an option on the
  existing terminal. `parseAll` keeps `parse`'s single-error return type stable (no caller has to
  branch on whether `err` is an Object or an Array); the option form is one fewer export but makes the
  `err` slot's type depend on an option. Recommendation: `parseAll` (typed-stable). **Owner: confirm.**

### Tests
Unit (`schema.rs` tests, drive `Interp::parse_value`-style): an object with 3 bad fields → 3 errors
with correct paths (`user.a`, `user.b`, `user.c`); nested object/array errors carry full dotted/indexed
paths; a fully-valid value → `[value, nil]`; `parse` still returns only the first error (regression);
a `refine` panic still escalates (not collected). E2E `.as`: `schema.parseAll` over a form with
multiple bad fields prints all messages.

---

## §2 — HTTP-framework query + path-param schemas (`std/http/server`)

### Current behavior (verified)
The route table stores `(method, path_pattern, schema?, handler)` (`http_server.rs:126-128`,
registered via `register_route` `:558`). A route with a 3rd schema arg validates the **JSON request
body** before the handler runs (`http_server.rs:979-1010`): JSON-decode the raw body, run
`self.parse_value(&schema, &decoded, "", false, span)`, and on `Mismatch` return a **400** with JSON
`{error, path, message}` (`schema_error_response`, `:366`). Path params (`:name`) are extracted by
`match_route` (`:310`) into `req.params` as **strings**; query string is parsed into `req.query` as an
object of **strings** (`query_params_are_parsed` test `:1462`). Neither `params` nor `query` is
schema-validated or type-coerced today — this is the documented future extension.

### Target semantics
Allow a route to declare **typed query and path-param schemas** that are validated (and coerced)
BEFORE the handler runs, mirroring the existing body-schema flow.

- **API:** extend the route registration to accept an **options object** carrying named schemas
  instead of (or in addition to) the bare body schema. Proposed verb signature:
  `s.get(path, {params: schema, query: schema, body: schema}, handler)` — the 3rd arg is either a
  bare schema (today's body-only behavior, kept for back-compat) OR an options object whose
  `params`/`query`/`body` fields are each a schema (any may be omitted). `route(method, path, opts,
  handler)` gains the same.
- **Validation, with coercion ON** (query/params arrive as strings; `{coerce:true}` lets a
  `schema.number()` accept `"42"`): for each present schema, run the collecting OR fail-fast engine
  against the corresponding request object (`req.params`, `req.query`, decoded `req.body`).
  - On success, the **coerced** values replace the raw strings in the request object the handler sees
    (so `req.query.page` is a `Number`, not `"42"`) — this reuses `parse_value`'s coerce path, which
    already returns the coerced value (`schema.rs:463-476`).
  - On failure, return **400** with the existing `schema_error_response` shape, extended with a
    `where` field (`"params"` | `"query"` | `"body"`) so the client knows which part failed; the
    handler is NOT called.
- Body-schema behavior is **unchanged** when the 3rd arg is a bare schema.

### Implementation
- Generalize `register_route` (`http_server.rs:558`) and the verb/route dispatch
  (`http_server.rs:599-638`) to detect: bare schema (`schema_kind(arg).is_some()`) → body schema (as
  today); Object WITHOUT `__kind` → options object → pull `params`/`query`/`body` schemas. Store a
  small struct in the route tuple (replace the `schema?` slot with `RouteSchemas { params, query,
  body }`, all `Option<Value>`).
- In the dispatch block (`http_server.rs:979`), after building `req_obj` (`:950-957`), validate
  `params`/`query` BEFORE body (params/query are always present objects; body may be absent). Use
  `coerce = true` for params/query (string-origin), `coerce = false` for body (JSON-origin, matches
  today). Replace the request fields with coerced results on success.
- Borrow discipline unchanged: clone schemas + the relevant request sub-objects out under a short
  borrow before the `parse_value` await (the body path already does this, `:974`).

### Open question for the owner
- **Coercion default for query/params:** always-on (chosen — strings must become numbers/bools to be
  useful) vs. caller-controlled. Recommendation: always-on for query/params since they are inherently
  string-typed in HTTP; body stays JSON-typed (no coerce). **Owner: confirm.**

### Tests
`http_server.rs` async tests (model on `schema_route_valid_body_*` `:1919-2002` and
`path_param_is_extracted` `:1294`): a `:id` param validated as `schema.number()` coerces `"7"`→`7`;
a bad param (`/users/abc` vs number) → 400 with `where:"params"`; a query schema coerces
`?page=2&active=true`; a missing/invalid query field → 400; a route with all three schemas; the
back-compat bare-body-schema route still works unchanged.

---

## §3 — Typed parse for CSV / TOML / YAML

### Current behavior (verified)
`csv.parse`, `toml.parse`, `yaml.parse` (`csv.rs:27`, `toml.rs:23`, `yaml.rs:18`) decode to plain
`Value` (objects/arrays/strings) and are pure `call(func, args, span)` functions — they are NOT on
`&self`, so they cannot call the async `validate_into`/`parse_value` directly. The established
precedent is **`json.parse(text, Class|schema)`**, which is special-cased in `call_stdlib`
(`src/stdlib/mod.rs:256-310`): `mod.rs` calls the module's plain 1-arg `parse` to get the decoded
`Value`, then runs `self.validate_into(class, &val, …)` (Class path) or `self.parse_value(&schema,
&val, …)` (schema path), fusing a decode failure and a shape mismatch into ONE Tier-1 `[value, err]`.
`resp.json(Class|schema)` does the same (`net_http.rs:1435-1453`).

### Target semantics
Extend typed decode to CSV / TOML / YAML, reusing the SAME `validate_into` (Class) and `parse_value`
(schema) core — no new validation logic.

- **`toml.parse(text, Class|schema)`** and **`yaml.parse(text, Class|schema)`**: identical to
  `json.parse(text, Class|schema)` — decode the document to a `Value`, then validate-into/parse the
  whole value against the class/schema, fusing decode + shape errors into one `[value, err]`.
- **`csv.parse(text, Class|schema[, options])`** is **row-oriented**: CSV decodes to an array of row
  objects (header mode) or arrays. Typed CSV validates **each row** against the class/schema and
  returns `[array<Instance>, err]`. The first failing row's error (with a `row[N]` path prefix) goes
  to the err channel (fail-fast across rows, matching `validate_into`'s field fail-fast); optionally
  expose a collect-all rows mode later (out of SP5 scope unless trivially free via §1). Header mode is
  required for class decode (a class needs named fields → use `{header:true}`); a positional row
  against a class with named fields is a Tier-1 error with a clear message.

### Implementation
- **Do NOT change the module `call` signatures** — replicate the `json`-in-`mod.rs` pattern. In
  `Interp::call_stdlib` (`src/stdlib/mod.rs`), add `#[cfg(feature = "data")]` pre-dispatch blocks
  mirroring the existing `if module == "json" && func == "parse"` block (`:257`):
  - `if module == "toml" && func == "parse"` and `if module == "yaml" && func == "parse"` — when
    `args.get(1)` is a `Value::Class` → call the module's 1-arg parse, then `validate_into`; when it
    is a tagged schema (`schema::schema_kind(second).is_some()`) → `parse_value`. Identical
    error-fusion to json.
  - `if module == "csv" && func == "parse"` — same detection, but iterate the decoded rows and
    `validate_into`/`parse_value` each row, accumulating into an output array; thread a `row[N]` path.
- **Refactor opportunity:** the json/toml/yaml whole-document typed path is byte-identical; factor a
  private `Interp::typed_decode(decoded: Value, type_arg: &Value, span) -> Result<Value, Control>`
  helper returning the `[value, err]` pair, and call it from the json/toml/yaml blocks (csv calls it
  per row). This keeps the three blocks from drifting.

### Open question for the owner
- **CSV typed rows error mode:** fail-fast on first bad row (chosen, matches `validate_into`) vs.
  collect-all rows (needs §1's collecting engine). Recommendation: fail-fast for SP5; collect-all
  rows is a clean follow-up once §1 lands. **Owner: confirm.**

### Tests
E2E `.as` for each: `toml.parse(text, Config)` where `Config` is a class → typed instance; a TOML
shape mismatch → `[nil, err]`; same for YAML; `csv.parse(text, Row, {header:true})` → array of typed
rows; a bad cell type → row-pathed err. Unit-level: a malformed document fuses the decode error into
the err channel (no panic). Verify the existing untyped `parse` (1-arg) is unchanged.

---

## §4 — MessagePack + CBOR binary serialization

### Current behavior (verified)
No binary-serialization module exists. `std/json` (`src/stdlib/json.rs`) converts `Value`↔JSON via
`from_ascript`/`to_json_lossy`; `std/encoding` handles base64/hex/utf8. Bytes are
`Value::Bytes(Rc<RefCell<Vec<u8>>>)`.

### Target semantics
Two new modules, **`std/msgpack`** and **`std/cbor`**, each with:
- `encode(value) -> bytes` — serialize a `Value` to the binary format (Tier-2 panic on a
  non-serializable value, e.g. a function/native handle, mirroring json's lossy-but-total stance:
  prefer the json approach — functions→`nil`/error per the chosen crate's `Value` mapping; decide a
  TOTAL mapping so encode never panics on data, only on genuinely unrepresentable handles).
- `decode(bytes) -> [value, err]` — Tier-1; malformed input → err channel.
- `decode(bytes, Class|schema) -> [value, err]` — typed decode via the SAME `validate_into`/
  `parse_value` core, special-cased in `call_stdlib` exactly like `json.parse(text, Class)` (§3).
  Because msgpack/cbor decode to bytes-in, the typed block lives in `call_stdlib` too.

`Value`↔format mapping (define explicitly): Number→int/float (msgpack/cbor distinguish; AScript has
one `Number(f64)` — encode integers that round-trip as ints, else float; decode int→`Number`), Str→
string, Bool→bool, Nil→nil, Array→array, Object/Map→map, Bytes→binary/byte-string. Document the
lossy edges (Object vs Map both → a format map; on decode a map → `Object` if all keys are strings,
else `Map`, matching json's decode convention — verify against `json.rs`).

### Rust crates + feature flags
- **MessagePack:** `rmpv` (the `rmpv::Value` dynamic model) is the cleanest fit — decode to
  `rmpv::Value`, convert to `Value` by hand (mirrors json's `serde_json::Value`↔`Value` bridge), no
  `serde` derive needed. Alternative `rmp-serde` requires a serde `Serialize`/`Deserialize` target;
  since AScript's `Value` is dynamic, `rmpv` is a better match. **Crate: `rmpv = "1"`.**
- **CBOR:** `ciborium` (actively maintained, `ciborium::value::Value` dynamic model) — same
  hand-bridge approach. (`serde_cbor` is unmaintained; do NOT use it.) **Crate: `ciborium = "0.2"`.**
- **Feature flag:** fold both into a new **`binary`** feature: `binary = ["dep:rmpv",
  "dep:ciborium"]`, added to `default`. Rationale: both are small, pure-Rust, dependency-light;
  grouping them keeps the feature list manageable and they are conceptually one "binary serialization"
  battery. (Alternative: gate under the existing `data` feature — but `data` already pulls
  serde_json/regex/csv/yaml; keeping `binary` separate lets a minimal `data`-only build skip msgpack/
  cbor.)

### Open question for the owner
- **Module naming + feature:** `std/msgpack` + `std/cbor` as two modules under one `binary` feature
  (chosen) vs. one `std/binary` module with `binary.msgpackEncode`/`binary.cborEncode` vs. folding
  into `std/encoding`. Recommendation: two modules (`std/msgpack`, `std/cbor`), one `binary` feature —
  discoverable names, shared dep group. **Owner: confirm.**

### Tests
Per module: round-trip every `Value` kind (number int + float, string, bool, nil, nested array/
object, bytes, map); a typed `decode(bytes, Class)` → instance; a shape mismatch → err; malformed
bytes → Tier-1 err (no panic); cross-check a known fixture (e.g. a small canonical msgpack/cbor byte
sequence decodes to the expected value) so the wire format is pinned. E2E `.as`: encode→decode round
-trip prints equality.

---

## §5 — Compression: zstd + brotli + tar (`std/compress`)

### Current behavior (verified)
`std/compress` (`src/stdlib/compress.rs`) exports `gzip`/`gunzip`/`deflate`/`inflate` (via `flate2`)
and `zipCreate`/`zipExtract` (via `zip`), under the **`compress`** feature
(`Cargo.toml:92` `compress = ["dep:flate2", "dep:zip"]`). Compressors accept string-or-bytes and
return bytes; decompressors are Tier-1; zip uses an array of `{name, data}` entry objects.

### Target semantics
Extend `std/compress` (same module, same conventions) with:
- **zstd:** `zstdCompress(src[, level]) -> bytes`, `zstdDecompress(bytes) -> [value, err]`. `src` is
  string-or-bytes (reuse `source_bytes`, `compress.rs:35`); optional `level` (default crate default).
- **brotli:** `brotliCompress(src[, quality]) -> bytes`, `brotliDecompress(bytes) -> [value, err]`.
- **tar:** `tarCreate(entries) -> [bytes, err]` and `tarExtract(bytes) -> [array<{name,data}>, err]`,
  matching the EXISTING zip entry-object shape (`{name, data}`, `compress.rs:96-212`) so tar and zip
  are symmetric. Tar create is Tier-1 (bad entry shape → Tier-2 type error like `build_zip`,
  `compress.rs:126`; I/O → Tier-1).

### Rust crates + feature flags
- **zstd:** `zstd = "0.13"` (pure bindings to libzstd; bundled C, no system dep — like rusqlite's
  `bundled`). reqwest already enables zstd internally (`Cargo.toml:59`) but that is transport-only;
  the stdlib codec needs the crate directly.
- **brotli:** `brotli = "7"` (pure-Rust, no C). reqwest enables brotli internally too; again,
  stdlib needs the crate.
- **tar:** `tar = "0.4"` (pure-Rust archive read/write over `Read`/`Write`, same I/O model as `zip`).
- **Feature flag:** EXTEND the existing `compress` feature:
  `compress = ["dep:flate2", "dep:zip", "dep:zstd", "dep:brotli", "dep:tar"]`. No new feature — these
  are all compression codecs, all default-on with `compress`.

### Open question for the owner
- None material. (zstd's `bundled`-C build is the only nuance; it is the standard, widely-used
  approach and matches rusqlite — note it for the implementer, not a design decision.)

### Tests
Round-trip (compress→decompress equals original) for zstd and brotli on both string and binary input;
"actually compresses" on repetitive input (model `gzip_actually_compresses_repetitive`,
`compress.rs:263`); garbage → Tier-1 err (`gunzip_garbage_is_tier1_err`, `:287`); tar create+extract
round-trip with text + binary entries (model `zip_create_extract_roundtrip`, `:301`); a string passed
to a `*Decompress` (bytes-only) → Tier-2 panic. E2E `.as` extends the advanced
`crypto_and_compress.as` example.

---

## §6 — DB clients: Postgres + Redis (`std/postgres`, `std/redis`)

### Current behavior (verified)
The only DB client is `std/sqlite` (`src/stdlib/sqlite.rs`, feature `sql = ["dep:rusqlite"]`). It is
the template for the native-resource pattern: `sqlite.open(path)` registers a
`ResourceState::SqliteConnection` and returns a `Value::Native` of `NativeKind::SqliteConnection`
(`sqlite.rs:61-88`); connection methods (`exec`/`run`/`all`/`close`) are `NativeMethod`s dispatched by
`Interp::call_sqlite_method` (`sqlite.rs:88-96`) using `take_resource`/`return_resource`. Resources
live in `Interp.resources` keyed by id; `close` does `take_resource` and drops the connection
deterministically. There is **no live-server integration-test infra** today — `std/net` tests bind
`127.0.0.1:0` in-process (no external dependency).

### Target semantics
Two new async modules following the sqlite native-resource model, but over a **network connection**:
- **`std/postgres`:**
  - `await postgres.connect(url) -> [conn, err]` (Tier-1; bad URL / unreachable server → err).
  - Connection (a `Value::Native`) methods, all async, all Tier-1:
    `await conn.query(sql, params?) -> [array<rowObject>, err]`,
    `await conn.exec(sql, params?) -> [affectedRows, err]`,
    `await conn.queryOne(sql, params?) -> [rowObject | nil, err]`,
    optional `await conn.begin()/commit()/rollback()` (transactions), and `conn.close()`.
    Rows are objects (column-name → value); Postgres types map to `Value` (int/float→Number,
    text→Str, bool→Bool, null→Nil, bytea→Bytes, json/jsonb→decoded Object/Array). Document the
    type-map and the lossy edges (e.g. numeric/decimal → Str to avoid f64 precision loss; arrays →
    Array).
  - Optional typed rows: `conn.query(sql, params, Class) -> [array<Instance>, err]` via `validate_into`
    per row (same as §3 CSV) — design it; gate the typed-row work behind owner confirmation as it adds
    surface.
- **`std/redis`:**
  - `await redis.connect(url) -> [conn, err]`.
  - Connection methods (async, Tier-1): a generic `await conn.command(name, ...args) -> [value, err]`
    plus typed conveniences `get`/`set`/`del`/`incr`/`expire`/`exists` and `await conn.close()`.
    Redis replies map to `Value` (bulk string→Str or Bytes, integer→Number, array→Array, nil→Nil,
    error→err channel).

### Rust crates + feature flags
- **Postgres:** `tokio-postgres = "0.7"` (pure-Rust, async, integrates with the current-thread tokio
  runtime; TLS optional — defer TLS to a documented follow-up, connect over plaintext/localhost for
  SP5, OR add `tokio-postgres-rustls` if the owner wants TLS in scope). Native-resource fit: a
  `tokio_postgres::Client` is the connection; `tokio_postgres::connect` returns `(Client, Connection)`
  where the `Connection` future must be **spawned** to drive the protocol — spawn it via
  `tokio::task::spawn_local` (the runtime is current-thread / `LocalSet`, `!Send`) and store the
  `Client` in `ResourceState::PostgresConnection { client, conn_task: AbortHandle }` so dropping/closing
  aborts the driver task (the cancel-on-drop discipline already used for futures, see `task.rs`).
- **Redis:** `redis = { version = "0.27", features = ["tokio-comp"] }` (async via `tokio-comp`;
  `aio::MultiplexedConnection` or a single `aio::Connection`). Store the async connection in
  `ResourceState::RedisConnection(...)`.
- **Feature flags:** add **`postgres = ["dep:tokio-postgres", ...]`** and **`redis = ["dep:redis"]`**.
  Keep them **default-ON** (consistent with the "batteries-included" goal and the other net/sql
  features) BUT see the build-weight note below. The prompt suggested extending `sql`; recommendation
  is SEPARATE features (`postgres`, `redis`) because (a) they pull heavy async deps independent of
  rusqlite, (b) a user wanting only sqlite should not compile the postgres/redis stacks, and (c)
  separate features make the integration-test gating (below) clean.

### `!Send` / runtime discipline (critical, verified against CLAUDE.md)
The interpreter is `!Send` (`Rc`/`RefCell`) on a current-thread tokio runtime inside a `LocalSet`.
Both crates support this: drive the postgres `Connection` and any redis background task via
`spawn_local` (NOT `tokio::spawn`, which needs `Send`). **Never hold a `resources` borrow or any
`RefCell` borrow across an `.await`** — use the take-out-across-await pattern (`take_resource` → await
on the owned client → `return_resource`), exactly as `sqlite` and the http server do. Connections are
`Rc`-based native resources with **deterministic Drop** (closing aborts the driver task and drops the
socket) — these handles STAY on `Rc` with a no-op GC `Trace` (per the value.rs native-resource
invariant in CLAUDE.md).

### Integration tests — how they run WITHOUT flaking
This is the heart of the DB-client risk. Design:
- **Unit-level (always run, no server):** connect to an obviously-dead address (e.g.
  `postgres://127.0.0.1:1/none`, redis `redis://127.0.0.1:1`) and assert a clean **Tier-1 err**
  (not a panic) — proves the error path, the resource registration/cleanup, and the arg handling
  WITHOUT any external dependency. Also unit-test the type-mapping helpers (`Row`→`Value`,
  reply→`Value`) directly in Rust with constructed inputs, no live connection.
- **Live integration (opt-in, skipped by default):** gate the real round-trip tests behind an
  **environment variable** holding the connection URL — `ASCRIPT_TEST_POSTGRES_URL` /
  `ASCRIPT_TEST_REDIS_URL`. Each live test does `let Ok(url) = std::env::var("ASCRIPT_TEST_POSTGRES_URL")
  else { return; }` at the top → **the test no-ops (passes) when the var is unset**, so `cargo test`
  on a machine/CI without a DB never fails and never flaks. When the var IS set (a developer or a CI
  job with a `services:` Postgres/Redis container), the full round-trip runs (connect → create temp
  table / set key → query → assert → cleanup → close). This mirrors the established "skip when the
  resource is unavailable" convention and the `vm_differential` skip discipline (every skip carries a
  documented reason). Document the env vars + a `docker run` one-liner in the module doc page and the
  plan.
- **No flakiness sources:** the default `cargo test` run touches NO socket beyond the localhost
  dead-port connect (which fails fast and deterministically). Live tests use unique temp table names /
  key prefixes (e.g. a UUID suffix) and clean up, so concurrent CI runs against a shared server don't
  collide.

### Open questions for the owner
- **Default-on vs. opt-in features:** `postgres`/`redis` default-ON (chosen, batteries-included) adds
  `tokio-postgres` + `redis` to every default build's compile time and binary size. If build weight is
  a concern, make them **opt-in** (NOT in `default`) — they would still be in `STD_MODULES` so the
  checker accepts the imports, and a default-build program importing them gets a clean
  "module requires feature" Tier-1/runtime error (verify the existing "unknown stdlib module" path,
  `mod.rs:389`, gives a usable message — may need a feature-specific message). **Owner: decide
  default-on vs opt-in.**
- **TLS for Postgres:** in scope (add `tokio-postgres-rustls`) or deferred (plaintext/localhost only,
  documented)? Recommendation: defer TLS to a follow-up; SP5 ships plaintext + a documented deferral.
  **Owner: confirm.**
- **Typed rows (`conn.query(sql, params, Class)`):** include in SP5 or defer? Recommendation: include
  for Postgres (reuses §3's per-row `validate_into`), it is the headline ergonomic. **Owner: confirm.**

---

## §7 — Utilities: LRU cache + event-emitter + templating

### Current behavior (verified)
No `std/lru`, `std/events`, or `std/template` exists. Pure-collection modules like `std/set`,
`std/map`, `std/object` are **core (NOT feature-gated)** — they appear ungated in `mod.rs`
(`set::exports()` `:106`, no `#[cfg]`). They are pure `Value`-over-`Value` with no external deps.

### Target semantics
Three pure in-process modules (NO external crates — all implementable over `Value` + `std`):

- **`std/lru`** — a bounded LRU map. `lru.new(capacity) -> lru` (a `Value::Native` resource, since it
  is stateful and mutable), with methods `get(key) -> value|nil`, `set(key, value)`, `has(key)`,
  `delete(key)`, `clear()`, `len()`, `keys()`. Eviction: setting beyond capacity evicts the
  least-recently-used entry; `get`/`set` mark an entry most-recently-used. Implement with an
  `IndexMap<MapKey, Value>` + recency bookkeeping in a `ResourceState::Lru { … }` (keys use the
  existing hashable `MapKey`, `value.rs`, so any hashable Value is a key — same as `std/map`).
  *Alternative (no resource):* model it as a tagged Object like schema does, but a cache is
  inherently mutable+stateful so a native resource is the honest fit (matches sqlite/tui handle style).
- **`std/events`** — an event-emitter / pub-sub. `events.new() -> emitter` (`Value::Native`
  resource), methods `on(event, fn)`, `once(event, fn)`, `off(event, fn?)`, `await emit(event,
  ...args)` (calls each listener via `call_value`; async because listeners may be `async fn` — await
  them in registration order; a listener panic propagates as Tier-2 unless documented otherwise),
  `listenerCount(event)`. Listeners stored per-event in the resource state.
- **`std/template`** — string templating. `template.render(tmpl, data) -> [string, err]` where
  `tmpl` is a string with `{{name}}` / `{{a.b.c}}` placeholders resolved against `data` (an
  Object/Instance/Map); missing key → Tier-1 err (or empty — see open question). Keep it SMALL: a
  single `{{path}}` substitution form (dotted paths), HTML-escaped OR raw (decide). No loops/
  conditionals in SP5 (that is a templating language, out of scope) — document the limitation.

### Feature flags
**Core, NOT feature-gated** — register ungated in `mod.rs` like `set`/`map`/`object`. Rationale: no
external dependency, pure `Value` logic, and they must build under `--no-default-features` (the bare
language). Add to the ungated section of `pub mod` (`mod.rs:7-77`), the ungated arms of
`std_module_exports` / `call_stdlib`, and `STD_MODULES`.

### Open questions for the owner
- **Template syntax:** `{{name}}` (chosen — familiar Mustache-ish, unambiguous, distinct from
  AScript's own `${…}` string interpolation so there's no confusion) vs. `${name}` vs. `%name%`.
  And: **missing-key behavior** — Tier-1 err (strict, chosen) vs. render empty string (lenient) vs. an
  option. And **escaping** — raw by default (chosen, since output is not necessarily HTML) with an
  optional `{escape:true}`. **Owner: confirm syntax + missing-key + escaping.**
- **LRU/events as Native resource vs. tagged Object:** resource (chosen — stateful/mutable, honest
  fit) vs. tagged Object (no resource-table entry, but then mutation semantics get awkward).
  Recommendation: Native resource. **Owner: confirm.**
- **events.emit async:** awaiting async listeners in order (chosen) vs. fire-and-forget. Recommendation:
  await in registration order so errors surface deterministically. **Owner: confirm.**

### Tests
LRU: capacity eviction order (set past capacity evicts LRU; get promotes), has/delete/clear/len; any
hashable key type. events: on/emit calls listeners in order with args; once fires exactly once; off
removes; async listeners are awaited; listenerCount. template: `{{name}}` and `{{a.b}}` substitution;
missing key → err (or empty per decision); literal `{{` handling. E2E `.as` for each; all run under
`--no-default-features` (core).

---

## §8 — `intl` long month/day names (locale-correct)

### Current behavior (verified)
`intl.formatDate` "long" style uses chrono's `%B` (`src/stdlib/intl.rs:112,114`) which ALWAYS renders
**English** month names regardless of locale — the documented limitation
(`intl.rs:27` "Month names for 'long' style are English (a documented limitation)"). Region/order is
locale-aware (`date_pattern`, `:90`) but the month/day NAMES are not. `intl` already depends on `icu`
1.5 with `compiled_data` (`Cargo.toml:43,87`).

### Target semantics
Render long-style month names (and weekday names where used) **locale-correctly** for the supported
locales, replacing the hard-coded English `%B`. `formatNumber`/`caseUpper`/`caseLower`/`compare`
(real ICU) and the region/order logic are unchanged — this is a SCOPED fix to the long-style name
rendering only.

### Implementation options (owner picks; recommendation given)
- **(A) icu4x datetime symbols (preferred):** use `icu`'s datetime symbols data to get the locale's
  long month/weekday names and substitute them into the rendered date, replacing the chrono `%B`
  format token with the ICU-sourced name (build the long-style string from the locale's
  month-of-year + day-of-week symbols rather than chrono's English ones). icu 1.5's stable date
  formatting plumbing is heavyweight (the module doc notes this, `intl.rs:21-26`) — the lighter route
  is to pull JUST the symbol tables. If the stable `icu::datetime` symbols API is awkward in 1.5,
  consider `icu_datetime`'s provider symbols, OR option B.
- **(B) curated data table (pragmatic, dependency-free):** ship a small static table of long
  month/weekday names for the locales `intl` already exercises in tests/examples (en, de, fr, ja, zh,
  ko, es, … — enumerate from the existing intl tests + `datetime_intl.as`). Look up by
  `loc.id.language` (+ region where it matters) and substitute. This is honest, testable, and
  matches the existing pragmatic-fallback stance of `formatCurrency`/`formatDate` (`intl.rs:18-26`).
- **Recommendation:** attempt (A) for correctness/coverage; fall back to (B) scoped to the supported
  locales if the 1.5 symbols API proves too heavyweight. Either way, REMOVE the "Month names are
  English" limitation note and document the actual coverage.

### Open question for the owner
- **(A) icu4x symbols vs. (B) curated table**, and **which locales** are "supported" (the set the fix
  must be correct for). Recommendation: (A) if feasible in icu 1.5 stable, else (B) over the locales
  already in the intl test/example corpus. **Owner: confirm approach + locale set.**

### Tests
`intl.formatDate(instant, "de-DE", "long")` renders a German month name (e.g. "März"), `"fr-FR"` a
French one, `"ja-JP"` keeps the `年月日` form (already correct), `"en-US"` unchanged. Add cases to the
existing intl tests and the `datetime_intl.as` advanced example. Assert the previously-English locales
now differ.

---

## Testing & quality bar (whole sub-project)

- **Both feature configs:** `cargo test` green (default) AND `cargo test --no-default-features` green
  (the core utilities §7 + schema §1 must pass un-gated; feature-gated modules cfg out cleanly).
- **Clippy clean** under `cargo clippy --all-targets` AND
  `cargo clippy --no-default-features --all-targets`; `await_holding_refcell_ref` stays denied + clean
  (critical for the DB clients' take-out-across-await pattern).
- **No VM/tree-walker differential risk:** new stdlib surface is dispatched through the shared native
  `call_stdlib`, so the whole-corpus three-way differential is preserved with no opcode/Value change.
  Where a NEW example `.as` is added, it must pass `treesitter_conformance` + `frontend_conformance`
  and be byte-identical across engines (the conformance/differential harness covers it automatically).
- **`STD_MODULES` sync:** every new module specifier added to ALL THREE of `std_module_exports`,
  `call_stdlib`, and the un-gated `STD_MODULES` list — the `unresolved-import` checker
  (`unresolved_import.rs:47`) and the module-resolution tests enforce this.
- **No `unsafe`, no `#[allow]`, no `#[ignore]`, no stubs** — DB live tests use the env-var early-return
  skip (NOT `#[ignore]`).
- **Per-task commit** with trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Docs:** each module-group adds/updates a `docs/content/stdlib/*.md` page and, where a `std/*` API
  changes (intl), updates the matching existing page; update the stdlib table in `README.md`.

## File-touch map (for the plan)

| Module-group | Files |
|---|---|
| §1 schema collect-all | `src/stdlib/schema.rs` (collecting engine + `parseAll`), `interp.rs` fluent hook is unchanged (routes any `is_schema_method`) |
| §2 HTTP query/param schemas | `src/stdlib/http_server.rs` (route schemas struct, dispatch validation) |
| §3 typed CSV/TOML/YAML | `src/stdlib/mod.rs` (pre-dispatch typed blocks + `typed_decode` helper); modules unchanged |
| §4 msgpack/cbor | new `src/stdlib/msgpack.rs` + `src/stdlib/cbor.rs`; `src/stdlib/mod.rs` (register + typed blocks); `Cargo.toml` (`binary` feature) |
| §5 compress zstd/brotli/tar | `src/stdlib/compress.rs`; `Cargo.toml` (extend `compress`) |
| §6 postgres/redis | new `src/stdlib/postgres.rs` + `src/stdlib/redis.rs`; `src/interp.rs` (`ResourceState` + `NativeKind` variants, dispatch); `src/value.rs` (`NativeKind` variants); `src/stdlib/mod.rs` (register); `Cargo.toml` (`postgres`/`redis` features) |
| §7 lru/events/template | new `src/stdlib/lru.rs` + `events.rs` + `template.rs`; `src/interp.rs`/`value.rs` (resource/native kinds for lru/events); `src/stdlib/mod.rs` (register, un-gated) |
| §8 intl months | `src/stdlib/intl.rs`; `Cargo.toml` (only if option A needs an icu sub-feature) |
| Common | `src/stdlib/mod.rs` (3-point registration each module), `STD_MODULES`, `examples/*.as` (+ `examples/advanced/*.as`), `docs/content/stdlib/*.md`, `README.md` |

## Self-review

- **Grounding:** every "current behavior" cites a verified file:line. The 3-point registration
  contract, the json typed-parse precedent (`mod.rs:256-310`), the resource pattern (`sqlite.rs`,
  `interp.rs:697-740`), the schema fail-fast (`schema.rs:751`), the HTTP body-schema flow
  (`http_server.rs:979`), the compress conventions, and the intl English-month limitation
  (`intl.rs:27,112`) are all confirmed against the code.
- **Engine-agnostic claim is real:** `call_stdlib` is the single dispatch both engines use; no §
  touches an opcode or a `Value` eval path, so the differential is preserved. The only `Value`-model
  edits are ADDITIVE `NativeKind`/`ResourceState` variants for §6/§7 resources (no change to existing
  variants; native handles stay on `Rc` with no-op `Trace`).
- **Typed-decode reuse:** §3/§4/§6 all reuse `validate_into`/`parse_value` via the established
  `mod.rs` special-case pattern — no new validation logic, no signature changes to the pure module
  `call`s.
- **DB-test flakiness:** addressed concretely — default `cargo test` touches no live server (dead-port
  Tier-1 + Rust-level type-map unit tests); live tests early-return when the env var is unset, use
  unique temp names, and clean up. No `#[ignore]`.
- **Open questions are genuine** (not deferred work): schema terminal name, query/param coerce default,
  CSV typed-row error mode, msgpack/cbor naming+feature, postgres/redis default-on vs opt-in + TLS +
  typed rows, template syntax/missing-key/escaping, lru/events resource-vs-object, intl approach+locales.
- **Placeholder scan:** no "TBD/handle edge cases"; crate versions, feature names, function signatures,
  and test strategies are concrete. The one deliberate deferral to the implementer is the exact Rust
  glue (the implementer reads the cited modules to match the arm style).
