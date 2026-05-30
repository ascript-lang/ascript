:::eyebrow Resources

# Examples

The repository ships a library of runnable programs under `examples/`. The introductory set lives in
`examples/`; the larger, production-shaped programs live in `examples/advanced/`. Run any of them
with:

```text
ascript run examples/advanced/data_pipeline.as
```

## Introductory examples

Short, single-concept programs in `examples/`:

| File | Shows |
|---|---|
| `hello.as` | the smallest program |
| `functions.as` | functions, closures, recursion |
| `ranges.as` | `for…in` ranges and `for…of` iteration |
| `result.as` | the `[value, err]` convention and `?` |
| `typed.as` | gradual type contracts |
| `oop.as` | classes, inheritance, enums, `match` |
| `async.as` | `async fn` and `await` |
| `strings.as` · `numbers.as` · `data.as` | core values and collections |
| `serialization.as` | JSON / TOML / YAML / CSV round-trips |
| `regex.as` | compiled patterns |
| `datetime.as` | time, dates, locale formatting |
| `system.as` | files, crypto, compression, sqlite, processes |
| `net.as` | a self-contained TCP loopback echo |
| `tui.as` | the terminal UI |

## Advanced examples

Robust, heavily-commented programs in `examples/advanced/` — each one handles every error path and is
verified to run end-to-end.

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

### Networking (run as two processes)

AScript is single-threaded with no task-spawn primitive, so a full HTTP or WebSocket round-trip needs
the server and client in **separate processes**. Start the server, then run the client in another
terminal:

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

> [!NOTE] AScript buffers a program's `print` output and flushes it when the program ends. A server
> started with the forever-looping `serve()` therefore won't stream its log lines live — pass
> `serve({ maxRequests: N })` if you want it to finish (and flush) after N requests.
