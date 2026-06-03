<div align="center">

# AScript

**A Lua-simple language with a Go/Deno-class standard library.**

AScript is a small, dynamically-typed scripting language with JavaScript-flavored syntax, optional
runtime-checked type contracts, and a batteries-included standard library — all in a single Rust
binary.

</div>

```ascript
import { get } from "std/net/http"
import * as json from "std/json"

async fn weather(city: string): Result<object> {
  let [resp, err] = await get(`https://api.example.com/weather?q=${city}`)
  if (err != nil) { return Err(err.message) }
  return await resp.json()
}

let [report, err] = await weather("Lisbon")
print(err == nil ? `${report.tempC}°C in Lisbon` : "could not load weather")
```

## Why AScript

The guiding model is **"Lua-simple language, Go/Deno-class standard library."** The core stays tiny —
a tree-walking interpreter, ~10 value kinds, gradual contracts, no hidden control flow. The library
does the heavy lifting, because Rust's crate ecosystem makes high-quality batteries cheap.

Design priorities, in strict order: **simplicity → safety → familiarity → performance**.

- **Familiar syntax** — braces, `fn`, arrows, template strings, `for…of`. If you read JavaScript, you read AScript.
- **Gradual type contracts** — optional annotations, checked at runtime as contracts, never erased. Includes the nullable suffix `T?` (≡ `T | nil`) and typed class fields (required, optional, defaulted) checked on assignment.
- **Errors as values** — no exceptions; fallible calls return `[value, err]`; the `?` operator propagates and the `!` force-unwrap asserts success (panicking, recoverably, with the original message). Bugs panic, loudly.
- **Shape validation** — turn untrusted data into checked instances: `ClassName.from(obj)` validates a raw object (recursing into nested classes, `array<Class>`, and `map<K, Class>`), and the typed parse `json.parse(text, Class)` / `resp.json(Class)` fuses decode + validation into one result — `let user = await resp.json(User)?`.
- **Composable schema validation** — `std/schema` lets you build and compose schemas independently of any class: `schema.object({name: schema.minLength(schema.string(), 1), age: schema.min(schema.number(), 0)})`, with `min`/`max`, `minLength`/`maxLength`, `pattern`, `refine` (custom async predicates), `default`, `optional`, `union`, `oneOf`, and coercion (`{coerce: true}`). Refiners and `parse` also chain as **fluent methods** — `schema.string().minLength(3).maxLength(12).pattern("^[a-z0-9_]+$").parse(input)` — equivalent to and interoperable with the free-function form. `schema.fromClass(Class)` derives a schema from class field declarations. Pass a schema to `json.parse(text, schema)` to fuse JSON decoding and validation into one Tier-1 pair.
- **Pattern matching** — `match` is an expression with structural patterns: wildcard `_`, ranges (`1..=9`, `0..10`), array destructuring (`[a, b, ...rest]`), object destructuring (`{key, role: "admin", ...rest}`), `|` alternatives, and `if` guards. Bare identifiers use **Option C**: a name already defined in scope is compared (`==`); an undefined name binds the subject. The `[value, err]` idiom (`[v, nil] => …`) and enum variants work naturally. See [the docs](docs/content/language/classes-enums.md).
- **Destructuring** — pull fields out of an object or instance by key with `let {a, b as local, "k" as v} = obj`; missing keys bind `nil`.
- **Spread** — expand a collection inline with `...` in array literals, object literals, and call arguments (`[0, ...xs]`, `{...defaults, k: v}`, `f(...args)`); strict about container kind, object-spread is later-value-wins.
- **Rest** — collect what's left over with a trailing `...name`: a rest parameter gathers extra arguments into an array (`fn sum(...nums: array<number>)`, per-element typed), and rest destructuring takes the tail/leftover keys (`let [head, ...tail] = xs`, `let {id, ...meta} = obj`).
- **Single-threaded async & concurrency** — `await` any I/O on a cooperative event loop; `future<T>` and `std/task` (`spawn`/`gather`/`race`/`timeout`/`retry`); structured concurrency with cancel-on-drop; `std/sync` for channels, semaphores, and rate limiters; `std/time` `interval`/`debounce`/`throttle` timer utilities. No data races.
- **Generators & coroutines** — `fn*`/`async fn*` with `yield`, bidirectional `gen.next(v)`, `gen.close()`, and `for await` over generators and native streams (composable async pipelines).
- **Batteries included** — JSON, regex, SQLite, crypto, compression, a modern HTTP client, WebSockets, a TUI, and now: `std/url` (RFC-3986 URL parsing/building/query helpers), `std/cli` (declarative arg parsing with flags/options/subcommands), `std/color` (ANSI colors + NO_COLOR), `std/io` (stdin reading), `std/set` (insertion-ordered hash set with union/intersection/difference), `std/decimal` (exact 96-bit decimal arithmetic — `0.1 + 0.2 == 0.3`), `env.args()` (script arguments), `std/os` (host facts + live system metrics via the `sysinfo` feature), DNS resolution (`std/net`), UDP datagram sockets (`std/net/udp`), `std/stream` (lazy pull-based streams — sources, combinators, terminals — with short-circuiting; a 1M-range `filter+map+take(5)` touches only 9 source items), `std/assert` (rich test assertions: deep `eq`, `contains`, `approxEq`, `throws`), `std/bench` (micro-benchmarking: `measure` + `compare`), and the global `exit(code?)` builtin.
- **Real tooling** — a runner, REPL, formatter, test runner, and language server, all in one binary.

## Install

Build from source with a stable Rust toolchain:

```bash
cargo build --release      # → target/release/ascript
```

The default build includes the full standard library. Trim it with Cargo features (e.g.
`--no-default-features --features "data,sys,net"`) for a smaller binary.

## Usage

```bash
ascript run program.as     # run a program
ascript repl               # interactive REPL
ascript fmt file.as        # format in place
ascript check file.as      # static check (syntax + lints)
ascript test file.as       # run test(name, fn) cases
ascript lsp                # language server over stdio
```

### Checking

`ascript check` statically checks `.as` files (syntax errors + lints) and reports
**all** diagnostics with an exit code suited to CI: `0` clean, `1` on a lint failure,
`2` on a usage error (e.g. an unknown rule).

Per-rule severity is configurable via repeatable CLI flags and/or an `ascript.toml`:

```bash
ascript check src/*.as --deny unused-binding --allow shadowing --deny-warnings
```

```toml
# ascript.toml (discovered by walking up from the checked file)
[lint]
deny = ["unused-binding", "ignored-result"]
warn = ["unawaited-future"]
allow = ["shadowing"]
deny_warnings = true
```

Precedence is inline `// ascript-ignore[code]` > CLI flag > `ascript.toml` > rule
default; `syntax-error` is always an error. See the
[checker design spec](docs/superpowers/specs/2026-06-02-checker-design.md) for the
full rule-code list and details.

### Hello, world

```ascript
fn greet(name: string): string {
  return `Hello, ${name}!`
}
print(greet("world"))
```

```bash
ascript run hello.as
# Hello, world!
```

## The standard library

| Domain | Modules |
|---|---|
| Core & collections | `std/string` · `std/array` · `std/object` · `std/map` · `std/set` (insertion-ordered hash set) · `std/math` · `std/convert` · `std/bytes` |
| Data & serialization | `std/json` · `std/csv` · `std/toml` · `std/yaml` · `std/encoding` · `std/regex` · `std/uuid` · `std/url` · `std/decimal` (exact 96-bit decimal arithmetic) |
| Validation & schema | `std/schema` (composable validators: object/array/map/union/oneOf/optional, constraints, refine, coerce, fromClass) |
| System & files | `std/fs` · `std/env` · `std/io` · `std/process` · `std/crypto` · `std/compress` · `std/sqlite` |
| Host & OS | `std/os` (pid · platform · arch · cpuCount · hostname · tempDir; live metrics via `sysinfo` feature: memory · swap · cpuUsage · loadAvg · disks · uptime · networkInterfaces · localIp) |
| CLI & terminal | `std/cli` (declarative arg parser) · `std/color` (ANSI colors & styles, NO_COLOR-aware) |
| Time & locale | `std/time` (wall clock, sleep, `interval` · `debounce` · `throttle`) · `std/date` · `std/intl` |
| Networking | `std/net` (DNS: `lookup` · `lookupOne`) · `std/net/tcp` · `std/net/udp` (datagram sockets) · `std/net/http` · `std/http/server` (verb methods: `get`/`post`/`put`/`patch`/`delete`/`head`/`options`; schema-validated typed routes) · `std/net/ws` |
| Concurrency | `std/task` (`spawn` · `gather` · `race` · `timeout` · `retry` over `future<T>`) · `std/sync` (FIFO channels · counting semaphore · token-bucket rate limiter) |
| Logging | `std/log` (`debug` · `info` · `warn` · `error`; human/json, structured fields) |
| Terminal UI | `std/tui` |
| Lazy streams | `std/stream` (lazy pull engine: `range` · `from` sources; `map` · `filter` · `take` · `drop` · `flatMap` · `enumerate` · `zip` combinators; `collect` · `reduce` · `count` · `find` · `first` · `forEach` terminals) |
| Test assertions | `std/assert` (deep `eq`/`ne`, `isTrue`/`isFalse`/`isNil`/`notNil`, `gt`/`gte`/`lt`/`lte`, `contains`, `approxEq`, `throws`, `snapshot`) |
| Benchmarking | `std/bench` (`measure` · `compare`) |

## Documentation

Full documentation — language guide and a complete standard-library reference — lives in [`docs/`](docs/)
as a small static site. It uses `fetch` to load Markdown content, so serve the folder rather than
opening the files directly:

```bash
cd docs
python3 -m http.server 8000
# then open http://localhost:8000/
```

- **`docs/index.html`** — landing page.
- **`docs/reader.html`** — the documentation reader (language guide + stdlib reference, with search).
- **`docs/content/`** — every page as plain Markdown, readable straight from the repo if you prefer.

## Examples

Runnable programs live in [`examples/`](examples/) (introductory) and
[`examples/advanced/`](examples/advanced/) (production-shaped, fully error-handled). Highlights:

```bash
ascript run examples/pattern_matching.as           # pattern matching: ranges, arrays, objects, Option C
ascript run examples/streams_and_testing.as        # lazy streams + std/assert + std/bench
ascript test examples/streams_and_testing.as       # run the test() blocks in the same file
ascript run examples/advanced/data_pipeline.as     # CSV → JSON/YAML pipeline
ascript run examples/advanced/sqlite_crud.as       # SQLite with prepared statements & a transaction
ascript run examples/advanced/crypto_and_compress.as

# A JSON API + client (two terminals — see examples/advanced/):
ascript run examples/advanced/http_server.as       # terminal 1
ascript run examples/advanced/http_client.as       # terminal 2

# Phase-7 HTTP framework: verb methods + schema-validated typed routes (self-contained):
ascript run examples/advanced/typed_api.as
```

See the [Examples page](docs/content/examples.md) for the full catalog.

## Development

```bash
cargo test                         # full suite (~540 tests, all features)
cargo test --no-default-features   # core language only (~245 tests)
cargo clippy --all-targets         # lint — kept clean in both feature configs
```

Architecture and contributor guidance live in [CLAUDE.md](CLAUDE.md); the full design spec and
milestone history are under [`docs/superpowers/`](docs/superpowers/).

## License

See the repository for license details.
