:::eyebrow Standard library

# Standard library overview

AScript's standard library is deliberately rich — a **Go-class standard library** wrapped around a
focused language core. Each `std/*` module is implemented in native Rust over AScript's
[value model](../language/values-types), so the batteries are fast and dependency-light.

## Importing modules

Bring a module in by name (`std/...`). Use a namespace alias for the whole module, or destructure the
names you want:

```ascript
import * as json from "std/json"     // namespace: json.parse(...)
import { get, post } from "std/net/http"  // named imports
```

Names that collide with a builtin can be shadowed freely — `import` and `let` both bind into the
program's own scope.

## The calling convention

For the data-oriented modules, functions are called in **qualified form**, with the value passed as
the first argument:

```ascript
import * as array from "std/array"
array.map([1, 2, 3], (n) => n * 2)   // ✅  array.map(arr, fn)
```

There is no `value.method(...)` convention for these modules. **Method-style calls exist only for
native resource handles** — the objects returned by `std/process`, `std/sqlite`, `std/net/*`,
`std/http/server`, and `std/tui`. Those handles wrap a live OS resource (a socket, a child process, a
database connection) and expose methods directly:

```ascript
import { connect } from "std/net/tcp"
let [stream, err] = await connect("127.0.0.1", 8080)
await stream.write("ping\n")     // method on a live handle
stream.close()
```

## Error tiers

The library follows the same two-tier [error model](../language/errors) as the language:

> [!TIER1] **Recoverable failures are values.** Fallible functions return a `[value, err]` pair —
> `err` is `nil` on success. Destructure and check it: `let [data, err] = json.parse(s)`.

> [!TIER2] **Argument-type misuse panics.** Passing a string where a number is required is a *caller
> bug*, so it aborts rather than returning an error you'd have to handle everywhere.

A handful of functions sit deliberately on the Tier-1 side even for "bad input" because the input is
expected to be untrusted — for example `convert.parseNumber` returns a pair rather than panicking, so
you can validate user input safely.

## Async modules

Anything that touches I/O is `async` and must be `await`ed: `time.sleep`, all of `std/net/*` and
`std/http/server`, and `std/process` spawning/streaming. Synchronous modules (string, array, math,
json, crypto, …) need no `await`.

## The modules

| Page | Modules |
|---|---|
| [Core & collections](collections) | `string` · `array` · `object` · `map` · `set` · `math` · `decimal` · `convert` · `bytes` |
| [Data & serialization](data) | `json` · `csv` · `toml` · `yaml` · `msgpack` · `cbor` · `encoding` · `regex` · `uuid` · `url` · `xml` · `html` · `markdown` |
| [Validation & schema](schema) | `schema` (composable validators, constraints, refine, coerce, fromClass) |
| [System & files](system) | `fs` · `env` · `os` · `io` · `process` · `crypto` · `compress` · `jwt` · `oauth` · `archive` |
| [Databases](db) | `sqlite` · `postgres` · `redis` |
| [CLI & terminal](cli) | `cli` · `color` |
| [Time & locale](time) | `time` (+ `interval`/`debounce`/`throttle`) · `date` · `intl` · `cron` |
| [Networking & HTTP](net) | `net` (DNS) · `net/tcp` · `net/udp` · `net/http` · `http/server` · `net/ws` |
| [Email (SMTP)](email) | `email` (message builder + CRLF guard · SMTP client with STARTTLS) |
| [Object storage (S3)](blob) | `blob` (S3-compatible: put/get/list/presign/multipart · SigV4) |
| [Async & concurrency](async) | `task` (+ `retry`) · `sync` (channels · semaphore · rateLimiter) |
| [Lazy streams](stream) | `stream` (lazy pull engine: sources, combinators, terminals) |
| [Utilities](utilities) | `lru` · `events` · `template` · `semver` · `diff` |
| [Durable workflows](workflow) | `workflow` (event-sourced replay: `run` · `resume` · `activity` · `ctx`) |
| [AI](ai) | `ai` (multi-provider LLM: generate · stream · embed · tool) |
| [Telemetry](telemetry) | `telemetry` (OTLP traces/metrics · Sentry · PostHog) |
| [Logging](log) | `log` |
| [Terminal UI](tui) | `tui` |
| [Testing & assertions](assert) | `assert` (deep eq, comparisons, contains, approxEq, throws, snapshot) · `test` (generators · `prop()` property testing with shrinking) |
| [Benchmarking](bench) | `bench` (measure · compare) |

## Feature flags

Every module is behind a Cargo feature, all enabled by `default`. A build with
`--no-default-features` exposes only the gateless core (`string`, `array`, `object`, `map`, `math`,
`convert`, `time`, `cli`, `color`, `stream`, `bench`, `test` (generators + `prop`), and `assert` without
snapshot). See [Getting started](../getting-started#feature-flags) for the full mapping.
