::: eyebrow Resources

# Examples

The repository ships a library of runnable programs under `examples/`. The introductory set lives in
`examples/`; the larger, production-shaped programs live in `examples/advanced/`. Run any of them
with:

```text
ascript run examples/advanced/data_pipeline.as
```

> Want to run these without installing anything? Many of the introductory programs run in the
> browser-based [**Playground**](../tooling/playground) (the **Playground** link in the top
> navigation) — anything that stays within its [stdlib subset](../tooling/playground) (no
> filesystem, network, processes, or workers).

## Introductory examples

Short, single-concept programs in `examples/`:

| File | Shows |
|---|---|
| `hello.as` | the smallest program |
| `functions.as` | functions, closures, recursion |
| `factorial.as` | iterative factorial with a `for…in` range |
| `core_types.as` | `Set`, `Decimal`, and other core value kinds |
| `numbers.as` | numeric literals (hex, binary, `_` separators), numeric builtins |
| `strings.as` | string literals, escape sequences, template interpolation |
| `data.as` | arrays and objects — basics of `map`/`filter` and iteration |
| `ranges.as` | `..` / `..=` ranges, direction inference, and signed `step` |
| `map_literals.as` | `#{ expr: expr }` map literals with arbitrary evaluated keys |
| `spread.as` | `...` spread in arrays, objects, and call arguments |
| `rest.as` | rest parameters (`...name`) collecting trailing args into an array |
| `object_destructuring.as` | `let {a, b as local, "k" as v}` and rest collectors |
| `pattern_matching.as` | `match` with range patterns, guards, and multi-pattern arms |
| `optional_types.as` | nullable suffix `T?` in every type position |
| `typed.as` | gradual type contracts on parameters and return types |
| `typed_fields.as` | required, optional (`T?`), and defaulted typed class fields |
| `typed_parse.as` | `json.parse(text, Class)` — fused parse + shape validation |
| `typed_config.as` | typed parse for TOML / YAML / CSV with a Class argument |
| `shape_validation.as` | `ClassName.from(obj)` — validate a raw object into a class instance |
| `validation.as` | `std/schema` fluent validators and `parseAll` collect-all-errors mode |
| `schema_collect.as` | `schema.parseAll` returning every validation error in document order |
| `instanceof.as` | `instanceof` runtime type test against a class hierarchy |
| `records.as` | auto-derived positional `init` for classes with no explicit constructor |
| `static_methods.as` | `static fn` / `static async fn` class-level members and factory patterns |
| `default_params.as` | per-parameter defaults evaluated left-to-right at call time |
| `frozen.as` | `object.freeze` / `object.isFrozen` — shallow one-way immutability |
| `force_unwrap.as` | postfix `!` force-unwrap and its interaction with `await`/`recover` |
| `oop.as` | classes, inheritance, enums, and `match` on enum variants |
| `result.as` | the `[value, err]` convention and the `?` propagation operator |
| `async.as` | `async fn`, `await`, and the cooperative task model |
| `generators.as` | `fn*` generator functions, `yield`, and lazy sequences |
| `generators_test.as` | `async fn*` generators, `for await`, and coroutine composition |
| `concurrency.as` | `task.gather`, `task.race`, `task.timeout`, and `task.spawn` |
| `structured_concurrency.as` | structured concurrency: `gather`/`race`/cancel-on-drop guarantees |
| `concurrency_toolkit.as` | channels (`std/sync`), `task.spawn`, producer/consumer pipelines |
| `streams_and_testing.as` | lazy streams (`std/stream`), `std/assert`, and `std/bench` |
| `serialization.as` | JSON / TOML / YAML / CSV round-trips and the Bytes kind |
| `binary_serialization.as` | MessagePack and CBOR encode/decode via `std/msgpack` + `std/cbor` |
| `regex.as` | compiled patterns — match, capture groups, replace |
| `datetime.as` | `std/time`, `std/date`, `std/intl` — timestamps, parsing, locale formatting |
| `system.as` | capstone: files, crypto, compression, SQLite, processes |
| `net.as` | a self-contained TCP loopback echo |
| `tui.as` | off-screen terminal UI buffer, styling, and event dispatch |
| `logging.as` | `std/log` — leveled structured logging with `debug`/`info`/`warn`/`error` |
| `events_emitter.as` | `std/events` — pub/sub event emitter |
| `lru_cache.as` | `std/lru` — bounded least-recently-used cache |
| `template_render.as` | `std/template` — `{{name}}` string templating with dotted-path lookup |
| `cli_toolkit.as` | `std/cli`, `std/url`, `std/color`, `std/env` — CLI argument parsing and URL handling |
| `host_info.as` | `std/os`, `std/net`, `std/net/udp` — platform and network introspection |
| `stdlib.as` | stdlib sampler: string, array, object, map, math, convert |
| `stdlib_completeness.as` | exercices a broad cross-section of stdlib APIs for regression coverage |
| `deep_recursion.as` | SP9 robust recursion — heap-backed stack growth under deep native re-entry |
| `ffi_libm.as` | `std/ffi` — call `pow`/`sqrt`/`cos` from the platform's libm across the C ABI |
| `caps_sandbox.as` | `std/caps` — sandbox a plugin in a capability-restricted `run_in_worker({caps})` isolate |
| `all_features.as` | deterministic showcase exercising most of the language in one file |

## Advanced examples

Robust, heavily-commented programs in `examples/advanced/` — each one handles every error path and is
verified to run end-to-end.

AScript runs on a **single-threaded cooperative event loop per isolate**: within one isolate all async
tasks share one OS thread, scheduled by the Tokio runtime. `std/task` provides `spawn` (fire-and-forget
detach), `gather` (await all), `race` (first-wins, losers cancelled), `timeout`, and `pipe` — enabling
structured concurrency patterns without locks. (For multi-core parallelism, shared-nothing
[workers](language/workers) run on separate isolates.) Because each isolate's event loop is
single-threaded, a full HTTP or WebSocket round-trip between a server and a client still requires
**two separate processes**; see the networking section below.

### Self-contained

These run on their own with no external services:

| File | Shows |
|---|---|
| `data_pipeline.as` | CSV → transform (`map`/`filter`/`sort`/`reduce`) → JSON + YAML, with regex and number parsing |
| `crypto_and_compress.as` | SHA-256, HMAC, Argon2 password hashing, base64/hex, gzip + zip round-trips |
| `sqlite_crud.as` | in-memory SQLite: DDL, positional & named params, a prepared statement, a transaction |
| `fs_toolkit.as` | path helpers, `mkdir`/`write`/`read`/`stat`/`walk`, and a `grep` over a directory tree |
| `process_streams.as` | `process.run` plus streaming stdin/stdout through a spawned `cat` |
| `datetime_intl.as` | monotonic timing, date parsing/formatting/arithmetic, locale-aware `intl` formatting |
| `tui_dashboard.as` | an off-screen double-buffered dashboard rendered with `tui.buffer(...).dump()` |
| `stream_pipeline.as` | async generator pipeline — `async fn*` token source composed with stream transforms |
| `typed_api.as` | schema-validated HTTP API server — `std/schema` request/response contracts |
| `typed_http.as` | `resp.json(Class)` — fused HTTP decode + shape validation in one Tier-1 pair |
| `workflow_signup.as` | durable execution via `std/workflow` — event-sourced replay, activities, `ctx.sleep` |
| `ffi_struct.as` | `std/ffi` — C structs over `Bytes` (layout/offset/alignment) + a real `memset` out-param round-trip |
| `telemetry.as` | `std/telemetry` — OTLP / Sentry / PostHog observability (opt-in `telemetry` feature) |
| `ai_chat.as` | `std/ai` multi-provider LLM chat (opt-in `ai` feature; exits cleanly without a key) |
| `ai_tools.as` | `std/ai` tool calling and structured output (opt-in `ai` feature) |
| `postgres_crud.as` | async PostgreSQL CRUD via `std/postgres` (skips gracefully without `ASCRIPT_TEST_POSTGRES_URL`) |
| `redis_cache.as` | async Redis set/get/incr/del via `std/redis` (skips gracefully without `ASCRIPT_TEST_REDIS_URL`) |

### Networking (run as two processes)

Start the server in one terminal, then run the client in another:

| Server | Client | Shows |
|---|---|---|
| `http_server.as` | `http_client.as` | a JSON API with middleware (logging + bearer auth), `:id` route params, query strings; the client exercises GET/POST, 404/401 handling, timeouts, and a retry policy |
| `ws_server.as` | `ws_client.as` | a WebSocket echo server (text upper-cased, binary verbatim) and a client that round-trips several messages |
| *(any SSE endpoint)* | `sse_client.as` | a Server-Sent Events consumer with `.next()` event parsing; defaults to a public Wikimedia stream, override with `SSE_URL=…` |

```text
# terminal 1
ascript run examples/advanced/http_server.as
# terminal 2
ascript run examples/advanced/http_client.as
```

> [!NOTE] Under the CLI `run` command, `print` output streams live to stdout (and is retained even
> if the program later panics). `serve({ maxRequests: N })` still lets a `serve()` loop finish
> gracefully after N requests, but it's no longer needed just to *see* a server's log lines.
