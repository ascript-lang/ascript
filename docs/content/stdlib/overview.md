:::eyebrow Standard library

# Standard library overview

AScript's standard library is deliberately rich — the philosophy is *"Lua-simple language, Go/Deno-class
standard library."* Each `std/*` module is implemented in native Rust over AScript's
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
| [Core & collections](collections) | `string` · `array` · `object` · `map` · `math` · `convert` · `bytes` |
| [Data & serialization](data) | `json` · `csv` · `toml` · `yaml` · `encoding` · `regex` · `uuid` |
| [System & files](system) | `fs` · `env` · `process` · `crypto` · `compress` · `sqlite` |
| [Time & locale](time) | `time` · `date` · `intl` |
| [Networking & HTTP](net) | `net/tcp` · `net/http` · `http/server` · `net/ws` |
| [Terminal UI](tui) | `tui` |

## Feature flags

Every module is behind a Cargo feature, all enabled by `default`. A build with
`--no-default-features` exposes only the gateless core (`string`, `array`, `object`, `map`, `math`,
`convert`, `time`). See [Getting started](../getting-started#feature-flags) for the full mapping.
