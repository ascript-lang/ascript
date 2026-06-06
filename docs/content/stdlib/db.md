:::eyebrow Standard library

# Databases — SQLite, Postgres & Redis

AScript ships three database clients: the **embedded `std/sqlite`** (a bundled SQLite —
no system library required), and async network clients for two of the most common backing
stores — **PostgreSQL** (`std/postgres`) and **Redis** (`std/redis`). The network clients
follow the **native-resource** pattern — `connect` returns an opaque handle and every
operation is a method on it — and run on AScript's single-threaded async runtime, so every
network operation is `await`ed. SQLite is embedded and synchronous: its calls are *not* awaited.

> [!TIER1] Connection and command methods return `[value, err]`. A bad URL,
> unreachable server, or query error lands in the `err` channel — no panics for
> expected runtime failures.

All three are default-on (Cargo features `sql` / `postgres` / `redis`). A network connection
is a deterministic native resource: `close()` (and dropping the last handle) tears down the
socket — for Postgres it also aborts the background driver task.

## std/sqlite

Embedded SQLite access, backed by a bundled SQLite (no system library required). `open` is the only module-level function; everything else is a method on a connection or statement handle.

Values map between AScript and SQLite as follows: `Number` → integer (if integral) or real, `Str` → text, `Bool` → integer `0`/`1`, `Nil` → null, `Bytes` → blob. Reading back: integer/real → `Number`, text → `Str`, blob → `Bytes`, null → `Nil`.

**Parameter binding.** Pass parameters as the optional second argument:

- A **positional array** binds `?` placeholders in order: `conn.exec("INSERT INTO t VALUES (?, ?)", [1, "alice"])`.
- A **named object** binds `:name` placeholders by key (the leading `:` in the key is optional): `conn.exec("INSERT INTO t VALUES (:id, :name)", { id: 1, name: "alice" })`.

> [!TIER2]
> Using a connection or statement handle after `close()` (or after its connection is closed) is a use-after-close Tier-2 panic.

### sqlite.open

Opens (or creates) a database file and returns a connection handle.

- **path** `string` — the database file path. Use `":memory:"` for an in-memory database.
- **Returns** `[connection, err]` — a connection handle.

```ascript
import { open } from "std/sqlite"
let [conn, err] = open(":memory:")
print(err)
print(type(conn))
```

### Connection methods (SQLite)

- **conn.exec(sql, params?)** — execute a statement that returns no rows. Returns `[changes, err]`, where `changes` is the number of rows affected.
- **conn.query(sql, params?)** — run a query. Returns `[rows, err]`, where `rows` is an array of objects keyed by column name.
- **conn.prepare(sql)** — prepare a statement for repeated execution. Returns `[statement, err]`; the SQL is validated immediately.
- **conn.begin()** / **conn.commit()** / **conn.rollback()** — explicit transaction control (plain `BEGIN`/`COMMIT`/`ROLLBACK`). Each returns `[nil, err]`.
- **conn.close()** — close the connection and release its resources. Returns `nil`.

### Statement methods (SQLite)

A prepared statement re-resolves its owning connection on each call, so it stays valid until the connection is closed.

- **stmt.run(params?)** — execute the prepared statement. Returns `[changes, err]`.
- **stmt.all(params?)** — run the prepared query. Returns `[rows, err]` (array of objects keyed by column).

A complete create-table → insert → query flow:

```ascript
import { open } from "std/sqlite"
let [conn, _] = open(":memory:")

conn.exec("CREATE TABLE users (id INTEGER, name TEXT)")

let [ins, perr] = conn.prepare("INSERT INTO users VALUES (?, ?)")
ins.run([1, "alice"])
ins.run([2, "bob"])

let [rows, err] = conn.query("SELECT id, name FROM users WHERE id = :id", { id: 2 })
print(err)
print(rows[0].name)

conn.close()
```

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

### postgres.connect

```ascript
let [conn, err] = await postgres.connect(url)
```

- `url` (string) — a `postgres://user:pw@host:port/dbname` connection string. TLS is a documented follow-up; the current implementation uses `NoTls` (plaintext / localhost / stunnel).
- Returns `[conn, err]` — the connection handle on success, or `[nil, err]` if the server is unreachable or the URL is malformed.

The driver `Connection` future is spawned as a separate local task. Dropping `conn` without calling `close()` leaks that task until the program exits; prefer an explicit `conn.close()`.

### conn.query

```ascript
let [rows, err] = await conn.query(sql, params?)
let [rows, err] = await conn.query(sql, params, Class|schema)
```

Execute a `SELECT` (or any statement that returns rows). `params` is a positional array; placeholders are `$1`, `$2`, …

- Without a third argument, each row is an **object** keyed by column name.
- With a `Class` or `std/schema` value as the third argument, each row is validated per-row via `validate_into` (the same logic as `Class.from`). A shape mismatch fuses into the `err` channel.
- Returns `[array<rowObject|instance>, err]`.

```ascript
let [rows, err] = await conn.query("SELECT id, name FROM users WHERE active = $1", [true])
for (r of rows) { print(`${r.id}: ${r.name}`) }

// Typed rows:
class User { id: number  name: string }
let [users, e] = await conn.query("SELECT id, name FROM users", [], User)
// users is array<User instance>
```

### conn.queryOne

```ascript
let [row, err] = await conn.queryOne(sql, params?)
```

Like `query`, but returns only the first row (an object or `nil` if no rows matched). Use this for `SELECT ... WHERE id = $1` style lookups.

- Returns `[rowObject | nil, err]`.

```ascript
let [one, err] = await conn.queryOne("SELECT name FROM users WHERE id = $1", [42])
if (one == nil) { print("not found") } else { print(one.name) }
```

### conn.exec

```ascript
let [affected, err] = await conn.exec(sql, params?)
```

Execute a statement that does **not** return rows — `INSERT`, `UPDATE`, `DELETE`, `CREATE`, `DROP`, etc.

- Returns `[number, err]` — the number of rows affected (always `0` for DDL).

```ascript
let [n, err] = await conn.exec(
  "INSERT INTO events (user_id, action) VALUES ($1, $2)",
  [userId, "login"]
)
print(`inserted ${n} row(s)`)
```

### conn.begin / conn.commit / conn.rollback

```ascript
let [_, err] = await conn.begin()
// ... conn.exec / conn.query calls ...
let [_, cerr] = await conn.commit()
// or: await conn.rollback()
```

Wrap work in a transaction. Each method sends a bare `BEGIN` / `COMMIT` / `ROLLBACK` statement and returns `[nil, err]`. There is no automatic rollback on error — you must call `rollback()` in your error handler.

```ascript
let [_, e1] = await conn.begin()
let [_, e2] = await conn.exec("UPDATE accounts SET balance = balance - $1 WHERE id = $2", [50, src])
let [_, e3] = await conn.exec("UPDATE accounts SET balance = balance + $1 WHERE id = $2", [50, dst])
if (e2 != nil or e3 != nil) {
  await conn.rollback()
} else {
  await conn.commit()
}
```

### conn.close

```ascript
conn.close()
```

Aborts the background driver task and drops the client, closing the socket. Always call `close()` when done — this is the only way to guarantee the driver task is torn down. Returns `nil`.

**Type map (Postgres → AScript)**

| Postgres type | AScript value |
|---|---|
| `bool` | `bool` |
| `int2`, `int4`, `int8`, `float4`, `float8` | `number` |
| `numeric` / `decimal` | `string` (avoids f64 precision loss — parse with `decimal.from` if needed) |
| `text`, `varchar`, `name`, `char` | `string` |
| `bytea` | `bytes` |
| `json`, `jsonb` | decoded AScript value |
| `uuid`, `timestamp`, `timestamptz`, `date`, `time` | `string` (ISO-8601 text) |
| `null` | `nil` |
| unknown | `string` (text representation) or `nil` |

**Bind params (AScript → Postgres):** `nil`→`null`, `bool`→`BOOL`, `number`→`FLOAT8`, `string`→`TEXT`, `bytes`→`BYTEA`. Passing any other type (array, object, etc.) is a Tier-2 panic.

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

### redis.connect

```ascript
let [conn, err] = await redis.connect(url)
```

- `url` (string) — a `redis://host:port` or `redis://:password@host:port/db` connection string.
- Returns `[conn, err]`.

### conn.get

```ascript
let [value, err] = await conn.get(key)
```

Get the value of a key. Returns `[nil, nil]` (not an error) when the key does not exist.

```ascript
let [v, err] = await conn.get("session:abc")
if (v == nil) { print("cache miss") }
```

### conn.set

```ascript
let [ok, err] = await conn.set(key, value)
```

Set a key to a string or number value. Returns `[true, nil]` on success.

```ascript
await conn.set("user:1:name", "Ada")
await conn.set("counter", 0)
```

### conn.del

```ascript
let [count, err] = await conn.del(key)
```

Delete a key. Returns `[number, err]` — the count of keys deleted (0 or 1).

```ascript
await conn.del("session:abc")
```

### conn.incr

```ascript
let [newValue, err] = await conn.incr(key)
```

Atomically increment the integer stored at `key` by 1. Creates the key with value `1` if it does not exist.

```ascript
let [n, _] = await conn.incr("page_views")
print(`page views: ${n}`)
```

### conn.expire

```ascript
let [ok, err] = await conn.expire(key, secs)
```

Set a TTL on `key` (in seconds). Returns `[1, nil]` if the timeout was set, `[0, nil]` if the key does not exist.

```ascript
await conn.set("session:abc", token)
await conn.expire("session:abc", 3600)   // expire in 1 hour
```

### conn.exists

```ascript
let [count, err] = await conn.exists(key)
```

Return `[1, nil]` if the key exists, `[0, nil]` otherwise.

```ascript
let [exists, _] = await conn.exists("feature:dark_mode")
if (exists == 1) { /* ... */ }
```

### conn.command

```ascript
let [reply, err] = await conn.command(name, ...args)
```

Send any Redis command by name. Arguments are passed as additional positional values. Use this for commands that do not have a convenience wrapper (`LPUSH`, `LRANGE`, `HSET`, `ZADD`, …).

```ascript
let [_, _] = await conn.command("LPUSH", "queue", "task-1", "task-2")
let [items, _] = await conn.command("LRANGE", "queue", 0, -1)
// items is an array of strings
```

### conn.close

```ascript
conn.close()
```

Close the connection. Returns `nil`.

**Reply map (Redis → AScript)**

| Redis reply type | AScript value |
|---|---|
| Nil (null bulk) | `nil` |
| Integer | `number` |
| Bulk string (UTF-8) | `string` |
| Bulk string (binary) | `bytes` |
| Status string | `string` |
| Array | `array` |
| Error reply | `err` channel (`[nil, err]`) |

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
