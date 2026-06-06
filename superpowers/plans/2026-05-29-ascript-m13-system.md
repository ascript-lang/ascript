# AScript Milestone 13 — System Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Implement spec §11.2 "System & data stores" + §11.3 (`fs.grep`) + §11.4 (`std/process`): `std/env`, `std/fs` (incl. recursive `grep`), `std/crypto`, `std/compress`, `std/sqlite`, `std/process`. Introduces a **native resource-handle mechanism** (`Value::Native` + `Value::NativeMethod`) for OS resources with methods — used by `std/sqlite` (connections/statements) and `std/process` (child handle + async stdout/stderr readers + stdin writer), and **reused by M14** (HTTP streaming bodies, SSE, sockets).

**Architecture:** Same stdlib pattern. The new piece: a `Value::Native(Rc<NativeObject>)` handle carrying an integer `id` + a `NativeKind` tag + plain `fields`; the actual non-`Clone` OS resource (rusqlite `Connection`, tokio `Child`, a reader) lives in an interp-side `resources: HashMap<u64, ResourceState>` table, so the `Value` stays cheaply clonable. `read_member` on a `Native` returns a plain field or a `Value::NativeMethod` (receiver + method name); `call_value` dispatches `NativeMethod` to an async `call_native_method`. The two new `Value` kinds + their match arms are ALWAYS compiled (not feature-gated); only the modules that CONSTRUCT them (`sqlite`/`process`) are feature-gated. `std/process` is async (`tokio::process`, second async area after `time.sleep`).

**Tech Stack:** Rust 2021. New crates (feature-gated): `walkdir`+`ignore`+`grep`(reuse `regex`) under `fs`; RustCrypto (`sha2`,`md-5`,`hmac`,`getrandom`/`rand`, `argon2`,`bcrypt`) under `crypto`; `flate2`+`zip` under `compress`; `rusqlite` (bundled) under `sql`; `dotenvy` under `sys`/`env`; `tokio` `process` feature for `std/process`. Features: `sys` (env+fs+process), `crypto`, `compress`, `sql` — all default-on.

**Starting state (end of M12, on `main`):** 294 tests default (244 `--no-default`), clippy clean. Stdlib: core + string/array/object/map/math/convert + bytes/json/encoding/regex/uuid/csv/toml/yaml + time/date/intl. Value kinds incl. `Bytes`/`Regex`. `call_stdlib` async-dispatch precedent (`call_time`/`call_array`). `read_member` at interp.rs:686, `call_value` at :733, `type_name` at :1052. Resources/handles: none yet.

**Conventions:** single-threaded `Rc`/`RefCell` (never `Arc`); `Control` Panic(Tier-2)/Propagate(`?`); Tier-1 `[value,err]` via `make_pair`/`make_error`; Tier-2 panic for arg-type misuse via `want_*`; per-module `ctx`; cfg-gated registration; `run`/`run_err` test helpers; dual-config builds. **CRITICAL spec §11.4 rule:** a non-zero process exit is NOT an error (it's a normal result with `success==false`); spawn FAILURE (binary not found/permission/timeout) IS the `err`.

## Semantics decided

- **Resource handles (`Value::Native`):** identity equality, truthy, `type`→a per-kind name (`"connection"`/`"statement"`/`"childProcess"`/`"reader"`/`"writer"`), Display `<native {kind} #{id}>`. Methods via `read_member`→`NativeMethod`→async `call_native_method`. Closing a resource (`conn.close()`, `child.kill()`, EOF) removes it from the table; using a closed handle → Tier-2 panic ("use after close").
- **`std/env` (`sys`):** `get(name)`→string|nil; `set(name, value)`; `unset(name)`; `vars()`→object of all env vars; `loadDotenv(path?)`→`[count, err]` (dotenvy; default `.env`).
- **`std/fs` (`sys`):** `read(path)`→`[string, err]`; `readBytes(path)`→`[bytes, err]`; `write(path, data)`→`[nil, err]` (data string or bytes); `append(path, data)`→`[nil, err]`; `exists(path)`→bool; `stat(path)`→`[{size, isFile, isDir, modifiedMs}, err]`; `mkdir(path, recursive?)`→`[nil, err]`; `remove(path, recursive?)`→`[nil, err]`; `readDir(path)`→`[array of names, err]`; `walk(path)`→`[array of paths, err]` (recursive, `walkdir`); path helpers `join(...parts)`→string, `dirname(p)`, `basename(p)`, `extname(p)`, `isAbsolute(p)`→bool; `grep(pattern, dir, opts?)`→`[matches, err]` (§11.3, see Task 3).
- **`std/crypto` (`crypto`):** `sha256(data)`/`sha512(data)`/`md5(data)`→hex string (data string or bytes); `hmacSha256(key, data)`→hex string; `randomBytes(n)`→bytes; `hashPassword(pw)`→`[string, err]` (argon2 PHC string); `verifyPassword(pw, hash)`→bool; (also `bcryptHash`/`bcryptVerify` per spec "argon2/bcrypt"). NOT Tier-1 except where genuinely fallible (hashPassword).
- **`std/compress` (`compress`):** `gzip(bytes)`→bytes; `gunzip(bytes)`→`[bytes, err]`; `deflate(bytes)`→bytes; `inflate(bytes)`→`[bytes, err]`; `zipCreate(entries)`→`[bytes, err]` (entries = array of `{name, data}`); `zipExtract(bytes)`→`[array of {name, data}, err]`.
- **`std/sqlite` (`sql`):** `open(path)`→`[connection, err]` (`:memory:` for in-memory); connection methods: `conn.exec(sql, params?)`→`[changes, err]`; `conn.query(sql, params?)`→`[array of row-objects, err]`; `conn.prepare(sql)`→`[statement, err]`; `conn.close()`; `stmt.run(params?)`→`[changes, err]`; `stmt.all(params?)`→`[rows, err]`; transactions: `conn.transaction(fn)` runs `fn` and commits, or rolls back on a returned err/panic — OR explicit `conn.begin()/commit()/rollback()`. Params = array (positional `?`) or object (named `:name`). Row = object keyed by column name; SQLite types → number/string/bytes/nil.
- **`std/process` (`sys`, §11.4):** `run(cmd, args, opts?)`→`[result, err]` (async one-shot capture); `spawn(cmd, args, opts?)`→`[child, err]` (async streaming handle). See §11.4 for the exact `result`/`opts`/child-handle shape (capture string/bytes/inherit/null; cwd/env/clearEnv/stdin/shell/timeout/check; result `{stdout, stderr, stderrText, code, signal, success}`; child `{pid, stdin (writer), stdout/stderr (async readers), wait(), kill(sig?)}`). Reader methods: `read(n?)`/`readLine()`/`readToEnd()`→chunk or nil at EOF (string or bytes per capture). **Non-zero exit ≠ err; spawn failure = err; `check:true` flips non-zero into err.**

## Cargo features (add to `[features]`, all in default)

```
default = ["data", "datetime", "intl", "sys", "crypto", "compress", "sql"]
sys = ["dep:walkdir", "dep:ignore", "dep:dotenvy", "tokio/process", "tokio/io-util", "tokio/fs"]
crypto = ["dep:sha2", "dep:md-5", "dep:hmac", "dep:argon2", "dep:bcrypt", "dep:rand"]
compress = ["dep:flate2", "dep:zip"]
sql = ["dep:rusqlite"]
```
(`regex` for grep comes from the existing `data` feature; if `fs.grep` must work without `data`, move `regex` to a shared dep or have `sys` also pull `dep:regex` — DECIDE in Task 3.)

## Scope & Justified Deferrals

| Deferred | Why | Owner |
|---|---|---|
| `std/net/*`, `std/http/*`, `std/tui` | Async I/O + UI | **M14/M15** |
| LSP | Tooling | **M16** |
| The native-reader mechanism's HTTP/socket reuse | Built here; consumed there | **M14** |

Nothing in M13's own scope is deferred. (If `std/sqlite` transactions-via-callback prove to need re-entrant interp calls that are awkward, explicit begin/commit/rollback is the fallback — document.)

---

## Task 1: The native resource-handle mechanism (`Value::Native` + `Value::NativeMethod`)

**Files:** modify `src/value.rs`, `src/interp.rs`.

This is foundational infra with NO user-facing module yet — it's exercised by Tasks 6–7 (sqlite/process) and M14. Build + unit-test the mechanism in isolation with a tiny throwaway "demo" native kind, OR (preferred) build it and let sqlite (Task 6) be its first real test. Keep this task to the type machinery + dispatch + a minimal in-Rust unit test.

- [ ] **Step 1: `src/value.rs`** — add the types + variants. After the existing structs:
```rust
/// A native resource handle (sqlite connection/statement, process child/reader/writer,
/// and — in M14 — http bodies/sse/sockets). The non-Clone OS resource lives in the
/// interp's `resources` table keyed by `id`; this value is a cheap clonable handle.
pub struct NativeObject {
    pub id: u64,
    pub kind: NativeKind,
    /// Plain readable fields (e.g. a child's `pid`); methods are resolved separately.
    pub fields: indexmap::IndexMap<String, Value>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NativeKind {
    SqliteConnection,
    SqliteStatement,
    ChildProcess,
    Reader,
    Writer,
    // M14 adds: HttpBody, SseStream, TcpStream, ...
}

impl NativeKind {
    pub fn type_name(self) -> &'static str {
        match self {
            NativeKind::SqliteConnection => "connection",
            NativeKind::SqliteStatement => "statement",
            NativeKind::ChildProcess => "childProcess",
            NativeKind::Reader => "reader",
            NativeKind::Writer => "writer",
        }
    }
}

/// A method bound to a native handle (e.g. `child.wait`), dispatched async.
pub struct NativeMethod {
    pub receiver: std::rc::Rc<NativeObject>,
    pub method: String,
}
```
Add to `enum Value` (after `Regex`, NOT cfg-gated):
```rust
    Native(Rc<NativeObject>),
    NativeMethod(Rc<NativeMethod>),
```
Add match arms (compiler flags each):
- PartialEq: `(Value::Native(a), Value::Native(b)) => Rc::ptr_eq(a, b),` and `(Value::NativeMethod(a), Value::NativeMethod(b)) => Rc::ptr_eq(a, b),`
- Debug: `Value::Native(n) => write!(f, "Native({} #{})", n.kind.type_name(), n.id),` ; `Value::NativeMethod(m) => write!(f, "NativeMethod({}.{})", m.receiver.kind.type_name(), m.method),`
- write_display: `Value::Native(n) => write!(f, "<native {} #{}>", n.kind.type_name(), n.id),` ; `Value::NativeMethod(m) => write!(f, "<native method {}>", m.method),`
- is_truthy: no change (auto-truthy).
(If clippy flags an unconstructed variant of `NativeKind` under `--no-default-features` — because sqlite/process are gated — add `#[allow(dead_code)]` on the `NativeKind` enum; the variants ARE referenced by `type_name`, so likely no warning, but verify both configs.)

- [ ] **Step 2: `src/interp.rs`** — extend `Interp` with the resource table. Add a field `resources: HashMap<u64, ResourceState>` and `next_resource_id: u64`. Define `ResourceState` as an enum holding the actual OS resources (feature-gated variants):
```rust
pub(crate) enum ResourceState {
    #[cfg(feature = "sql")]
    SqliteConnection(rusqlite::Connection),
    #[cfg(feature = "sql")]
    SqliteStatement { /* see Task 6 — likely the prepared SQL + conn id, since rusqlite Statement borrows the Connection */ },
    #[cfg(feature = "sys")]
    ChildProcess(tokio::process::Child),
    #[cfg(feature = "sys")]
    Reader(/* a boxed async reader — see Task 7 */),
    #[cfg(feature = "sys")]
    Writer(/* a child stdin writer */),
    /// Placeholder so the enum is non-empty in `--no-default-features` (no real resources).
    #[allow(dead_code)]
    Closed,
}
```
Add helper methods on `Interp`:
```rust
    pub(crate) fn register_resource(&mut self, kind: NativeKind, fields: indexmap::IndexMap<String, Value>, state: ResourceState) -> Value {
        let id = self.next_resource_id;
        self.next_resource_id += 1;
        self.resources.insert(id, state);
        Value::Native(std::rc::Rc::new(crate::value::NativeObject { id, kind, fields }))
    }
    pub(crate) fn take_resource(&mut self, id: u64) -> Option<ResourceState> { self.resources.remove(&id) }
    // (sqlite/process add typed accessors that match on ResourceState.)
```
Initialize `resources: HashMap::new()`, `next_resource_id: 0` in `Interp::new()`.

- [ ] **Step 3: `src/interp.rs`** — `read_member`: add a `Value::Native` arm. Return a plain field if present, else a bound method:
```rust
            Value::Native(n) => {
                if let Some(v) = n.fields.get(name) {
                    return Ok(v.clone());
                }
                Ok(Value::NativeMethod(std::rc::Rc::new(crate::value::NativeMethod {
                    receiver: n.clone(),
                    method: name.to_string(),
                })))
            }
```
`type_name`: `Value::Native(n) => n.kind.type_name(),` and `Value::NativeMethod(_) => "function",`.
`call_value`: add `Value::NativeMethod(m) => self.call_native_method(m, args, span).await,`.

- [ ] **Step 4: `src/interp.rs`** — add the async dispatcher stub (real arms land in Tasks 6–7):
```rust
    #[async_recursion(?Send)]
    pub(crate) async fn call_native_method(&mut self, m: std::rc::Rc<crate::value::NativeMethod>, args: Vec<Value>, span: Span) -> Result<Value, Control> {
        use crate::value::NativeKind::*;
        match m.receiver.kind {
            #[cfg(feature = "sql")]
            SqliteConnection | SqliteStatement => self.call_sqlite_method(&m, args, span).await,
            #[cfg(feature = "sys")]
            ChildProcess | Reader | Writer => self.call_process_method(&m, args, span).await,
            #[allow(unreachable_patterns)]
            _ => Err(AsError::at(format!("native handle has no method '{}'", m.method), span).into()),
        }
    }
```
(The `call_sqlite_method`/`call_process_method` are added by Tasks 6/7 in their files via `impl Interp`. For THIS task, make `call_native_method` compile by having the match's only always-present arm be the `_ =>` error; the cfg arms are added now but their handler methods don't exist until Tasks 6/7 — so either stub the handlers here returning the error, or gate the arms so this compiles standalone. SIMPLEST: in this task, make `call_native_method` just return the `_ =>` error for all kinds, and Tasks 6/7 ADD the cfg arms + handlers. Do that — keep Task 1 self-contained.)

- [ ] **Step 5: unit test** — in `src/interp.rs` tests, a minimal Rust-level test that `register_resource` returns a `Value::Native`, `read_member` on it yields a field and a `NativeMethod`, and Display/type_name work. (No AScript-level test yet — no module constructs a Native until Task 6.) Example:
```rust
    #[tokio::test]
    async fn native_handle_fields_and_methods() {
        let mut interp = Interp::new();
        let mut fields = indexmap::IndexMap::new();
        fields.insert("pid".to_string(), Value::Number(42.0));
        let h = interp.register_resource(crate::value::NativeKind::ChildProcess, fields, ResourceState::Closed);
        assert_eq!(type_name(&h), "childProcess");
        assert_eq!(interp.read_member(&h, "pid", Span::new(0,0)).unwrap(), Value::Number(42.0));
        let m = interp.read_member(&h, "wait", Span::new(0,0)).unwrap();
        assert!(matches!(m, Value::NativeMethod(_)));
        assert_eq!(h.to_string(), format!("<native childProcess #{}>", 0));
    }
```
(Note `read_member` is `&self`; calling it on `interp` is fine. `register_resource` is `&mut self`.)

- [ ] **Step 6:** FULL `cargo test` + `cargo test --no-default-features` + `cargo clippy --all-targets` (both configs) + `cargo build --no-default-features`. Green/clean/compile (the Native kinds are always-present; ResourceState's real variants are feature-gated with a `Closed` always-present). Commit `feat: native resource-handle mechanism (Value::Native + NativeMethod + resource table)`.

---

## Task 2: `std/env` (feature `sys`)

**Files:** `Cargo.toml` (`sys` feature + `dotenvy`); create `src/stdlib/env.rs`; register cfg-gated.

API: `get(name)→string|nil`, `set(name, value)`, `unset(name)`, `vars()→object`, `loadDotenv(path?)→[count, err]`.
- [ ] Implement with `std::env` (get/set/vars) + `dotenvy` (loadDotenv). `set`/`unset` mutate the process env (document the global side effect). `vars()` returns an object of all current env vars. `loadDotenv` loads a `.env` file (default `.env`) into the process env, returning the count loaded (Tier-1 err on read/parse failure). Arg-type misuse → Tier-2 panic. Unit tests (set→get→unset round-trip; vars contains a just-set key) + interp e2e. Register cfg-gated `sys`. `cargo test` (both configs) + clippy + commit `feat: std/env module`.

(Full code: follow the established module pattern — `exports()`/`call()`, `want_string`, `make_pair`/`make_error`. `std::env::set_var`/`var`/`remove_var`/`vars`. Note: tests that mutate process env can interfere if run in parallel — use unique key names per test to avoid flakiness.)

---

## Task 3: `std/fs` (feature `sys`) incl. recursive `grep` (§11.3)

**Files:** `Cargo.toml` (`walkdir`, `ignore`; ensure `regex` available under `sys` — `grep` reuses `std/regex`'s engine); create `src/stdlib/fs.rs`; register cfg-gated.

- [ ] **File ops + path helpers** (sync, `std::fs` + `std::path`): read/readBytes/write/append/exists/stat/mkdir/remove/readDir/walk + join/dirname/basename/extname/isAbsolute (per "Semantics decided"). Fallible I/O → Tier-1 `[value, err]`; arg-type misuse → Tier-2. `stat` returns `{size, isFile, isDir, modifiedMs}`. `walk` uses `walkdir` (recursive). Paths are strings.
- [ ] **`grep(pattern, dir, opts?)`→`[matches, err]`** (§11.3): each match `{path, line, column, text}`. `pattern` is a `std/regex` pattern (compile via the `regex` crate — REUSE the engine, don't add a new one). `opts`: `glob` (filename filter), `ignoreCase`, `maxResults`, `respectGitignore` (default true, via the `ignore` crate's walker). Walk `dir` with the `ignore` crate (honors `.gitignore` when `respectGitignore`), read each text file, run the regex per line, collect matches up to `maxResults`. Skip binary/non-UTF8 files gracefully.
  - DECIDE the `regex` availability: `grep` needs the `regex` crate. It's currently under the `data` feature. Either (a) add `dep:regex` to the `sys` feature too (cargo dedups — fine), or (b) gate `grep` behind `all(feature="sys", feature="data")`. PREFER (a): add `dep:regex` to `sys` so `fs.grep` works whenever `fs` does.
- [ ] Tests: file round-trips in a temp dir (use `std::env::temp_dir()` + unique subdir per test); stat; mkdir/remove recursive; walk finds nested files; path helpers; **grep** finds a pattern across files in a temp tree with `{path,line,column,text}`, respects `maxResults`, `ignoreCase`, and a `glob` filter. interp e2e. Register cfg-gated. `cargo test` (both configs) + clippy + commit `feat: std/fs module with recursive grep`.

---

## Task 4: `std/crypto` (feature `crypto`)

**Files:** `Cargo.toml` (`sha2`, `md-5`, `hmac`, `argon2`, `bcrypt`, `rand`); create `src/stdlib/crypto.rs`; register cfg-gated.

- [ ] API: `sha256/sha512/md5(data)→hex string` (data string→utf8 bytes, or bytes); `hmacSha256(key, data)→hex`; `randomBytes(n)→bytes` (CSPRNG via `rand`/`getrandom`); `hashPassword(pw)→[phc string, err]` (argon2); `verifyPassword(pw, phc)→bool`; `bcryptHash(pw, cost?)→[string, err]`; `bcryptVerify(pw, hash)→bool`. Hashes are deterministic → plain string returns; password hashing is fallible → Tier-1. Arg misuse → Tier-2. Tests with KNOWN vectors (e.g. `sha256("")` = `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`; `sha256("abc")` = `ba7816bf...`; `md5("")` = `d41d8cd98f00b204e9800998ecf8427e`); hashPassword→verifyPassword round-trip true, wrong pw false; randomBytes(16) has len 16 and two calls differ. Register cfg-gated. `cargo test` (both configs) + clippy + commit `feat: std/crypto module`.

---

## Task 5: `std/compress` (feature `compress`)

**Files:** `Cargo.toml` (`flate2`, `zip`); create `src/stdlib/compress.rs`; register cfg-gated.

- [ ] API: `gzip(bytes)→bytes`, `gunzip(bytes)→[bytes, err]`, `deflate(bytes)→bytes`, `inflate(bytes)→[bytes, err]`, `zipCreate(entries)→[bytes, err]` (entries = array of `{name, data}` where data is string/bytes), `zipExtract(bytes)→[array of {name, data(bytes)}, err]`. Compress accepts bytes (or string→utf8). Decompress fallible → Tier-1. Tests: gzip→gunzip round-trip preserves bytes (incl. binary); deflate→inflate; zipCreate→zipExtract round-trips multiple named entries; gunzip of garbage → Tier-1 err. Register cfg-gated. `cargo test` (both configs) + clippy + commit `feat: std/compress module`.

---

## Task 6: `std/sqlite` (feature `sql`) — first real consumer of the resource mechanism

**Files:** `Cargo.toml` (`rusqlite` with `bundled`); create `src/stdlib/sqlite.rs`; modify `src/interp.rs` (add the `call_sqlite_method` arm + ResourceState handling); register cfg-gated.

- [ ] **`open(path)→[connection, err]`** (`:memory:` for in-memory): registers a `ResourceState::SqliteConnection(rusqlite::Connection)` and returns a `Value::Native(kind=SqliteConnection)`. 
- [ ] **Connection methods** (via `call_sqlite_method`, looked up by `NativeMethod.method` + the receiver's `id` into the resource table): `exec(sql, params?)→[changesCount, err]`; `query(sql, params?)→[rows, err]` (rows = array of objects keyed by column name; map SQLite value → Number/Str/Bytes/Nil); `prepare(sql)→[statement, err]`; `begin()`/`commit()`/`rollback()`→[nil, err] (explicit transactions — simpler than callback-style given rusqlite's borrow model; document); `close()`→removes the connection from the table (subsequent use → "use after close" panic). Params: a positional array (`?1`/`?`) or a named object (`:name`).
- [ ] **Statement** (`ResourceState::SqliteStatement`): rusqlite `Statement` borrows the `Connection`, which fights the resource-table model. PRAGMATIC approach: store the prepared SQL string + the owning connection id in the statement resource; `stmt.run(params?)`/`stmt.all(params?)` re-prepare-and-execute against the connection (rusqlite caches prepared statements internally via `prepare_cached`). Document this. (This sidesteps the self-referential borrow cleanly.)
- [ ] **`call_sqlite_method`** (`impl Interp` in sqlite.rs, async-signature for uniformity even though sqlite is sync): match `m.receiver.kind` + `m.method`; fetch the connection from `self.resources` by `m.receiver.id`; run via rusqlite; return Tier-1 Results. Add the `#[cfg(feature="sql")]` arm in `call_native_method` (interp.rs) routing SqliteConnection/SqliteStatement here.
- [ ] Tests (in-memory DB): create table, insert (exec → changes), query (rows as objects), positional + named params, a prepared statement run/all, a transaction (begin/insert/commit; begin/insert/rollback leaves no rows), close then use → panic, a SQL error → Tier-1 err. interp e2e. Register cfg-gated. `cargo test` (both configs — sqlite cfg's out under no-default) + clippy + commit `feat: std/sqlite module (resource-handle connections + statements + transactions)`.

---

## Task 7: `std/process` (feature `sys`, §11.4) — async one-shot + streaming with native readers

**Files:** `Cargo.toml` (tokio `process`/`io-util`); create `src/stdlib/process.rs`; modify `src/interp.rs` (add `call_process_method` + ResourceState Reader/Writer/ChildProcess); register cfg-gated.

Read spec §11.4 in full. Implement:
- [ ] **`run(cmd, args, opts?)→[result, err]`** (async, `tokio::process::Command`): one-shot, await completion, capture per `opts.capture` (`"string"` lossy-utf8 default / `"bytes"` / `"inherit"` / `"null"`). `result = {stdout, stderr, stderrText, code, signal, success}`. opts: cwd, env (object merged; null a key to unset), clearEnv, stdin (string/bytes written then closed), shell (bool — `/bin/sh -c` unix / `cmd.exe /C` windows), timeout (ms; on expiry kill + a `timeout` err), check (non-zero → err). **Non-zero exit → normal result with `success=false`; spawn failure (not found/permission/timeout) → the `err`.**
- [ ] **`spawn(cmd, args, opts?)→[child, err]`** (async): returns a `Value::Native(kind=ChildProcess)` with `fields={pid}` and methods `stdin` (returns a Writer native), `stdout`/`stderr` (return Reader natives — created lazily or at spawn), `wait()→{code,signal,success}`, `kill(sig?)`. The child + its piped stdout/stderr/stdin live in `ResourceState` entries. **The reader idiom:** `await r.read(n?)`→next chunk or nil at EOF; `await r.readLine()`; `await r.readToEnd()`; chunk type string|bytes per `capture`. Writer: `await w.write(data)`, `w.close()`. Use `tokio::io::{AsyncReadExt, AsyncBufReadExt, AsyncWriteExt, BufReader}`. The Reader ResourceState holds a `BufReader<ChildStdout/ChildStderr>` (boxed/enum to unify stdout+stderr); Writer holds the `ChildStdin`.
- [ ] **`call_process_method`** (`impl Interp` in process.rs, `#[async_recursion(?Send)]`): dispatch ChildProcess (`wait`/`kill`/`stdin`/`stdout`/`stderr`), Reader (`read`/`readLine`/`readToEnd`), Writer (`write`/`close`) by `m.method`, operating on `self.resources` by id. Add the `#[cfg(feature="sys")]` arm in `call_native_method`. Cross-platform per §11.4 (signals: `kill()`/`"KILL"` forceful everywhere; `"TERM"`/`"INT"`/`"HUP"` POSIX on unix, map to forceful on Windows with a doc caveat; `.bat`/`.cmd` via shell on Windows).
- [ ] Tests (use portable commands available on the CI/dev host — e.g. `echo`, `cat`, `sh -c`, `true`/`false`; gate Windows-specifics): `run("echo", ["hi"])` → stdout "hi\n", success true, code 0; non-zero exit (`sh -c "exit 3"`) → result success=false code=3 (NOT an err); `check:true` → that becomes an err; spawn-not-found (`run("definitely-not-a-binary", [])`) → err; stdin piped to `cat` → echoed stdout; capture "bytes" → bytes; timeout kills a `sleep`; **spawn** `cat` and write a line to stdin + readLine from stdout → round-trip; `child.wait()` → status; `child.kill()` terminates. interp e2e exercising spawn + reader + wait. Register cfg-gated. `cargo test` (both configs) + clippy + commit `feat: std/process module (run + spawn with async readers, §11.4)`.

(NOTE: process tests run real subprocesses — keep them to ubiquitous unix tools; if the host is Windows-only the implementer must adapt. Mark any platform-specific test `#[cfg(unix)]`.)

---

## Task 8: End-to-end example + integration test + holistic

**Files:** create `examples/system.as`; modify `tests/cli.rs`.

- [ ] **`examples/system.as`** — a cohesive showcase: write a temp file with `std/fs`, read it back, `grep` it; hash a string with `std/crypto`; gzip→gunzip round-trip with `std/compress`; an in-memory `std/sqlite` create/insert/query; run a subprocess with `std/process` (`echo`) and print its stdout; read an env var with `std/env`. RUN it; capture exact output; verify each line. (Use deterministic operations; avoid asserting random/uuid/time values.)
- [ ] Integration test `runs_system_example` in `tests/cli.rs`, gated `#[cfg(all(feature="sys", feature="crypto", feature="compress", feature="sql"))]`, asserting stable substrings (the sha256 hex, the queried row value, the grep match, the echo output).
- [ ] Conformance: `cargo test` parses the example under both parsers (only existing syntax + the new native-method call syntax `child.stdout.readLine()` which is just member+call — already grammatical). Confirm.
- [ ] FINAL: `cargo test` (default) + `cargo test --no-default-features` + `cargo clippy --all-targets` (both configs) + `cargo build --no-default-features`. All green/clean/compile. Commit `test: system modules end-to-end example + integration test`.

---

## Definition of Done

- `cargo test` (default) passes all suites; `cargo clippy --all-targets` clean; `cargo test --no-default-features` passes + `cargo build --no-default-features` compiles (sys/crypto/compress/sql cfg out; the `Value::Native`/`NativeMethod` kinds are always-present and harmless when unconstructed).
- Implemented per spec §11.2 System + §11.3 grep + §11.4 process: `std/env`, `std/fs` (+grep), `std/crypto`, `std/compress`, `std/sqlite`, `std/process`.
- The native resource-handle mechanism (`Value::Native`/`NativeMethod` + interp resource table) works; sqlite + process are its first consumers; method-style handle access (`conn.query(...)`, `child.stdout.readLine()`) works.
- §11.4 rules honored (non-zero exit ≠ err; spawn failure = err; check flips it). Tier-1/Tier-2 conventions uniform.
- Nothing in M13 scope deferred.

## Hand-off to Milestone 14 ("Async I/O")

M14 builds `std/net/tcp`, `std/net/http` (the MODERN client — spec §11.5: SSE first-class via `http.sse`, streaming bodies via the SAME reader idiom built here, HTTP/1.1+2+(3 feature-gated), retries/redirects/cookies/tls/proxy/multipart, backed by `reqwest`), `std/http/server` (`hyper`), `std/net/ws` (`tokio-tungstenite`). **M14 introduces a real future/awaitable `Value` kind** so `await` suspends on it (today `ExprKind::Await` is identity; sockets/http need real suspension on a value). The native reader/writer mechanism from M13 Task 1/7 is REUSED for http streaming bodies + sockets + the SSE stream's `next()`. The §11.4 reader idiom (`read/readLine/readToEnd`) IS the §11.5 streaming-body idiom — keep them identical. `reqwest` + `hyper` + `tokio-tungstenite` under a `net` feature; HTTP/3 behind a nested `http3` feature (default-off, per §11.5 deferrals).
