:::eyebrow Standard library

# Databases — Postgres & Redis

Beyond the embedded `std/sqlite`, AScript ships async network clients for two of the
most common backing stores: PostgreSQL (`std/postgres`) and Redis (`std/redis`).
Both follow the **native-resource** pattern — `connect` returns an opaque handle and
every operation is a method on it — and run on AScript's single-threaded async
runtime, so every network operation is `await`ed.

> [!TIER1] Connection and command methods return `[value, err]`. A bad URL,
> unreachable server, or query error lands in the `err` channel — no panics for
> expected runtime failures.

Both modules are default-on (Cargo features `postgres` / `redis`). A connection is
a deterministic native resource: `close()` (and dropping the last handle) tears
down the socket — for Postgres it also aborts the background driver task.

## std/postgres

```ascript
import * as postgres from "std/postgres"

let [conn, err] = await postgres.connect("postgres://user:pw@localhost/mydb")
if (err != nil) { print(`connect failed: ${err.message}`); return }

await conn.exec("CREATE TEMP TABLE t (id int, name text)")
await conn.exec("INSERT INTO t VALUES ($1, $2)", [1, "Ada"])

let [rows, qerr] = await conn.query("SELECT id, name FROM t ORDER BY id")
for (r of rows) { print(`${r.id}: ${r.name}`) }

conn.close()
```

**Methods** (all async, all Tier-1 unless noted):

| Method | Returns | Notes |
|---|---|---|
| `connect(url)` | `[conn, err]` | plaintext / localhost; TLS is a documented follow-up |
| `conn.query(sql, params?)` | `[array<rowObject>, err]` | rows are objects keyed by column name |
| `conn.query(sql, params, Class\|schema)` | `[array<instance>, err]` | typed rows (per-row `validate_into`) |
| `conn.queryOne(sql, params?)` | `[rowObject \| nil, err]` | first row, or nil if none |
| `conn.exec(sql, params?)` | `[affectedRows, err]` | for INSERT/UPDATE/DELETE/DDL |
| `conn.begin()` / `commit()` / `rollback()` | `[nil, err]` | transactions |
| `conn.close()` | `nil` | aborts the driver task, drops the client |

Bind params are a positional array; `$1`, `$2`, … placeholders. **Type map**
(Postgres → AScript): bool→bool; int2/int4/int8/float4/float8→number;
**numeric→string** (avoids f64 precision loss); text/varchar→string; bytea→bytes;
json/jsonb→decoded value; uuid/timestamp/date→string; null→nil.

## std/redis

```ascript
import * as redis from "std/redis"

let [conn, err] = await redis.connect("redis://localhost")
if (err != nil) { print(`connect failed: ${err.message}`); return }

await conn.set("greeting", "hello")
let [v, _] = await conn.get("greeting")          // v == "hello"
let [n, _] = await conn.incr("counter")          // atomic increment
let [out, _] = await conn.command("LPUSH", "q", "a", "b")  // any command

conn.close()
```

**Methods:** `connect(url)`, then `command(name, ...args)` (generic), plus the
conveniences `get(key)`, `set(key, value)`, `del(key)`, `incr(key)`,
`expire(key, secs)`, `exists(key)`, and `close()`. **Reply map** (Redis → AScript):
nil→nil; integer→number; bulk string → string (UTF-8) or bytes (binary); status
string → string; array → array; a Redis error reply → the `err` channel.

## Running the live tests / examples

There is no bundled server, so the runnable examples
(`examples/advanced/postgres_crud.as`, `examples/advanced/redis_cache.as`) and the
Rust integration tests **no-op gracefully** when no server URL is provided. To
exercise them against a real server:

```sh
# Postgres
docker run -e POSTGRES_PASSWORD=pw -p 5432:5432 -d postgres
ASCRIPT_TEST_POSTGRES_URL=postgres://postgres:pw@localhost/postgres \
  ascript run examples/advanced/postgres_crud.as

# Redis
docker run -p 6379:6379 -d redis
ASCRIPT_TEST_REDIS_URL=redis://localhost ascript run examples/advanced/redis_cache.as
```

The Rust tests read the same env vars (`ASCRIPT_TEST_POSTGRES_URL` /
`ASCRIPT_TEST_REDIS_URL`); when unset they pass as no-ops, so `cargo test` is green
without any database infrastructure.
